pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

use std::time::Duration;

use futures_util::future::BoxFuture;
use sqlx::{PgConnection, PgPool, Postgres, SqlSafeStr as _, Transaction, migrate::MigrateError};
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
        self.transient()
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

/// Начинает транзакцию так, чтобы отмена вызывающей задачи не оставила
/// соединение в открытой транзакции.
///
/// `sqlx` увеличивает глубину транзакции только после ответа сервера на
/// `BEGIN`. Если future отменить между отправкой `BEGIN` и ответом, соединение
/// вернётся в пул с открытой серверной транзакцией, которую клиент не считает
/// открытой. Следующий `BEGIN ISOLATION LEVEL ...` или `SET TRANSACTION ...`
/// на таком соединении падает с «SET TRANSACTION ISOLATION LEVEL must be
/// called before any query». Поэтому `BEGIN` идёт в отдельной задаче: отмена
/// вызывающего лишь отцепляет её, а транзакция либо вернётся, либо откатится
/// при собственном `Drop`. Брошенных транзакций не возникает, если все начала
/// идут через `begin`/`begin_with` (это проверяет тест ниже).
async fn start(
    pool: &PgPool,
    statement: Option<&'static str>,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    tokio::spawn(async move {
        let conn = pool.acquire().await?;
        let statement = statement.map(|statement| statement.into_sql_str());
        Transaction::begin(conn, statement).await
    })
    .await
    .map_err(|error| sqlx::Error::Protocol(format!("transaction task failed: {error}")))?
}

/// Открывает обычную транзакцию, переживая отмену вызывающей задачи.
pub async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    start(pool, None).await
}

/// Открывает транзакцию с собственным `BEGIN`, переживая отмену задачи.
///
/// Нужен для `BEGIN ISOLATION LEVEL ...`: изоляция задаётся до первого запроса
/// в одной команде, а не отдельным `SET TRANSACTION` после `BEGIN`.
pub async fn begin_with(
    pool: &PgPool,
    statement: &'static str,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    start(pool, Some(statement)).await
}

/// Выполняет `op` в одной транзакции: `Ok` фиксируется, `Err` синхронно
/// откатывается до возврата, не оставляя блокировок строк висеть в пуле.
pub async fn in_tx<T, E, F>(pool: &PgPool, op: F) -> Result<T, E>
where
    F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T, E>>,
    E: From<sqlx::Error>,
{
    let mut tx = begin(pool).await?;
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
    use sqlx::{Connection as _, postgres::PgListener};

    /// Пул из одного соединения: так брошенная транзакция достанется следующему
    /// началу, а не затеряется среди свободных соединений.
    fn single_connection(pool: &PgPool) -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with((*pool.connect_options()).clone())
    }

    #[sqlx::test(migrations = false)]
    async fn begin_with_prevents_an_abandoned_transaction(pool: PgPool) {
        let pool = single_connection(&pool);
        sqlx::query("select 1").execute(&pool).await.unwrap();
        // Отменяем начало после того, как пул выдал соединение и BEGIN ушёл на сервер.
        {
            let mut begin = Box::pin(begin_with(
                &pool,
                "begin isolation level repeatable read read only",
            ));
            for _ in 0..3 {
                if futures_util::poll!(begin.as_mut()).is_ready() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        // Следующее начало должно открыться чисто, без 25001.
        let mut tx = begin_with(&pool, "begin isolation level repeatable read read only")
            .await
            .expect("cancelled begin must not poison the pool");
        let one: i32 = sqlx::query_scalar("select 1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(one, 1);
        tx.rollback().await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn in_tx_prevents_an_abandoned_transaction(pool: PgPool) {
        let pool = single_connection(&pool);
        sqlx::query("select 1").execute(&pool).await.unwrap();
        // Отменяем начало после того, как пул выдал соединение и BEGIN ушёл на сервер.
        {
            let mut cancelled = Box::pin(in_tx(&pool, |conn| {
                Box::pin(async move {
                    sqlx::query("select 1").execute(&mut *conn).await?;
                    Ok::<_, sqlx::Error>(())
                })
            }));
            for _ in 0..3 {
                if futures_util::poll!(cancelled.as_mut()).is_ready() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        let one: i32 = in_tx(&pool, |conn| {
            Box::pin(async move {
                sqlx::query("set transaction isolation level repeatable read read only")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query_scalar("select 1").fetch_one(&mut *conn).await
            })
        })
        .await
        .unwrap();
        assert_eq!(one, 1);
    }

    /// Документирует, что именно предотвращает `db::begin`: отменённый сырой
    /// `conn.begin()` оставляет серверную транзакцию, и `BEGIN ... ISOLATION`
    /// на этом соединении падает. Тест не должен пережить исправление в sqlx.
    #[sqlx::test(migrations = false)]
    async fn raw_cancelled_begin_still_deep_poisons_a_connection(pool: PgPool) {
        let pool = single_connection(&pool);
        let mut conn = pool.acquire().await.unwrap();
        {
            let mut begin = Box::pin(conn.begin());
            assert!(futures_util::poll!(begin.as_mut()).is_pending());
        }
        // Запрос попадает в брошенную транзакцию.
        sqlx::query("select 1").execute(&mut *conn).await.unwrap();
        let error = match conn
            .begin_with("begin isolation level repeatable read")
            .await
        {
            Ok(_) => panic!("raw cancelled BEGIN must reproduce the PostgreSQL hazard"),
            Err(error) => error,
        };
        assert!(
            matches!(&error, sqlx::Error::Database(db) if db.code().as_deref() == Some("25001")),
            "unexpected error: {error}"
        );
        sqlx::query("rollback").execute(&mut *conn).await.unwrap();
    }

    /// Начала транзакций в исходниках хаба идут только через `db::begin`;
    /// иначе отмена задачи снова начнёт оставлять открытые транзакции в пуле.
    #[test]
    fn transaction_starts_bypassing_db_begin_are_rejected() {
        const ALLOW: &str = "db::begin: savepoint";
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let own_file = root.join("db.rs");
        let mut pending = vec![root];
        let mut offenders = Vec::new();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    pending.push(entry.unwrap().path());
                }
                continue;
            }
            if path == own_file || path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for (index, line) in source.lines().enumerate() {
                // Пометка разрешает вложенную точку сохранения на открытой транзакции.
                if line.contains(ALLOW) {
                    continue;
                }
                let code = line.split("//").next().unwrap_or("");
                let trimmed = code.trim_start();
                if trimmed.starts_with('*') || trimmed.starts_with("/*") {
                    continue;
                }
                if code.contains(".begin(") || code.contains("Transaction::begin(") {
                    offenders.push(format!("{}:{}: {}", path.display(), index + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "начинайте транзакции хаба через db::begin/db::begin_with;\n\
             исключение — вложенная точка сохранения с пометкой «{ALLOW}»:\n{}",
            offenders.join("\n")
        );
    }

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
