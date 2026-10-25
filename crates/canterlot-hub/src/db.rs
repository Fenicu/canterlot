pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::PgPool::connect(url).await
}
