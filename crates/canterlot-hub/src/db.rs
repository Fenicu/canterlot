pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

use futures_util::future::BoxFuture;
use sqlx::{PgConnection, PgPool, Postgres, SqlSafeStr as _, Transaction, migrate::MigrateError};

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
/// при собственном `Drop`. Перед началом снимаем возможный чужой остаток:
/// `ROLLBACK` вне транзакции — предупреждение, а не ошибка.
async fn start(
    pool: &PgPool,
    statement: Option<&'static str>,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let pool = pool.clone();
    tokio::spawn(async move {
        let mut conn = pool.acquire().await?;
        sqlx::query("rollback").execute(&mut *conn).await?;
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

    /// Воспроизводит отмену `BEGIN` после того, как команда уже ушла на сервер:
    /// соединение остаётся с открытой транзакцией, а sqlx считает глубину нулевой.
    async fn abandon_transaction(pool: &PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        {
            let mut begin = Box::pin(conn.begin());
            assert!(futures_util::poll!(begin.as_mut()).is_pending());
        }
        // Этот запрос попадает в брошенную транзакцию и делает следующий
        // `BEGIN ISOLATION LEVEL ...` недопустимым.
        sqlx::query("select 1").execute(&mut *conn).await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn begin_with_ignores_an_abandoned_transaction(pool: PgPool) {
        let pool = single_connection(&pool);
        abandon_transaction(&pool).await;
        let mut tx = begin_with(&pool, "begin isolation level repeatable read read only")
            .await
            .expect("BEGIN ... ISOLATION must not see the abandoned transaction");
        let one: i32 = sqlx::query_scalar("select 1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(one, 1);
        tx.rollback().await.unwrap();
    }

    #[sqlx::test(migrations = false)]
    async fn in_tx_ignores_an_abandoned_transaction(pool: PgPool) {
        let pool = single_connection(&pool);
        abandon_transaction(&pool).await;
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
