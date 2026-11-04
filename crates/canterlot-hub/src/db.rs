pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

use std::time::Duration;

use futures_util::future::BoxFuture;
use sqlx::{PgConnection, PgPool, migrate::MigrateError};
use uuid::Uuid;

/// Ошибки, которые PostgreSQL считает временными: взаимная блокировка и сбой
/// сериализации. Транзакция после них откатывается целиком и её можно повторить.
pub fn is_transient(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(db)
            if matches!(db.code().as_deref(), Some("40P01") | Some("40001"))
    )
}

/// Небольшая пауза перед повтором: попытки разводятся, чтобы не столкнуться снова.
pub fn retry_delay(seed: Uuid, attempt: u32) -> Duration {
    let bytes = seed.as_bytes();
    let jitter = u16::from_be_bytes([bytes[14], bytes[15]]) as u64 % 40;
    Duration::from_millis(20 + jitter + u64::from(attempt) * 40)
}

/// Доменные ошибки знают, пришла ли она из временного сбоя базы.
pub trait TransientError {
    fn is_transient(&self) -> bool;
}

impl TransientError for sqlx::Error {
    fn is_transient(&self) -> bool {
        is_transient(self)
    }
}

macro_rules! transient_from_db {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl TransientError for $ty {
                fn is_transient(&self) -> bool {
                    match self {
                        Self::Db(error) => is_transient(error),
                        _ => false,
                    }
                }
            }
        )+
    };
}

transient_from_db!(
    crate::attachments::AttachmentError,
    crate::conversations::CreateError,
    crate::conversations::SettingsError,
    crate::conversations::UpdateError,
    crate::crunch::CrunchError,
    crate::deletion::DeleteError,
    crate::executions::ExecutionError,
    crate::groups::GroupError,
    crate::presets::PresetError,
    crate::scheduler::ScheduleError,
    crate::session_tools::accept::AcceptError,
);

impl TransientError for crate::http::error::ApiError {
    fn is_transient(&self) -> bool {
        matches!(
            self.code,
            crate::http::error::ErrorCode::Internal | crate::http::error::ErrorCode::DbUnavailable
        )
    }
}

/// Код выхода при сбое миграции: вид и версия, без текста ошибки базы.
pub fn migration_error_code(error: &MigrateError) -> String {
    let (kind, version) = match error {
        MigrateError::Execute(_) => ("execute", None),
        MigrateError::ExecuteMigration(_, version) => ("execute_migration", Some(version)),
        MigrateError::Source(_) => ("source", None),
        MigrateError::VersionMissing(version) => ("version_missing", Some(version)),
        MigrateError::VersionMismatch(version) => ("version_mismatch", Some(version)),
        MigrateError::VersionNotPresent(version) => ("version_not_present", Some(version)),
        MigrateError::VersionTooOld(version, _) => ("version_too_old", Some(version)),
        MigrateError::VersionTooNew(version, _) => ("version_too_new", Some(version)),
        MigrateError::Dirty(version) => ("dirty", Some(version)),
        _ => ("other", None),
    };
    match version {
        Some(version) => format!("migration_error kind={kind} version={version}"),
        None => format!("migration_error kind={kind}"),
    }
}

/// Выполняет `op` в одной транзакции: `Ok` фиксируется, `Err` синхронно
/// откатывается до возврата, не оставляя блокировок строк висеть в пуле.
pub async fn in_tx<T, E, F>(pool: &PgPool, op: F) -> Result<T, E>
where
    F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T, E>>,
    E: From<sqlx::Error>,
{
    let mut tx = pool.begin().await?;
    match op(&mut tx).await {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// Отдельный пул из одного соединения под LISTEN: долгоживущее соединение не отнимает слот у запросов.
pub fn listener_pool(pool: &PgPool) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy_with((*pool.connect_options()).clone())
}

/// Пулы sqlx::test создаются отдельно и не используют этот after_connect.
pub async fn connect(url: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set lock_timeout = '5s'").execute(conn).await?;
                Ok(())
            })
        })
        .connect(url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgListener;

    #[sqlx::test(migrations = false)]
    async fn listeners_do_not_take_query_slots(pool: PgPool) {
        let mut listeners = Vec::new();
        for _ in 0..8 {
            let mut listener = PgListener::connect_with(&listener_pool(&pool))
                .await
                .unwrap();
            listener.listen("canterlot_events").await.unwrap();
            listeners.push(listener);
        }
        let mut held = Vec::new();
        for _ in 0..5 {
            held.push(
                tokio::time::timeout(std::time::Duration::from_secs(5), pool.acquire())
                    .await
                    .expect("query slot must stay free")
                    .unwrap(),
            );
        }
    }
}
