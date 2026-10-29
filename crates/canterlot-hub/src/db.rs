pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

use futures_util::future::BoxFuture;
use sqlx::{PgConnection, PgPool};

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
