pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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
