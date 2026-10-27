use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};

pub struct Singleton {
    connection: PgConnection,
    key: i64,
    pid: i32,
}
impl Singleton {
    pub async fn acquire(
        options: &PgConnectOptions,
        service: &str,
    ) -> Result<Option<Self>, sqlx::Error> {
        let mut connection = PgConnection::connect_with(options).await?;
        let key: i64 = sqlx::query_scalar("select hashtextextended($1,0)")
            .bind(format!("canterlot:{service}"))
            .fetch_one(&mut connection)
            .await?;
        let (held, pid): (bool, i32) =
            sqlx::query_as("select pg_try_advisory_lock($1),pg_backend_pid()")
                .bind(key)
                .fetch_one(&mut connection)
                .await?;
        if held {
            Ok(Some(Self {
                connection,
                key,
                pid,
            }))
        } else {
            connection.close().await?;
            Ok(None)
        }
    }
    pub async fn still_held(&mut self) -> bool {
        let check=sqlx::query_scalar::<_,bool>("select pg_backend_pid()=$1 and exists(select 1 from pg_locks where pid=pg_backend_pid() and locktype='advisory' and granted and classid::bigint=(($2::bigint >> 32) & 4294967295) and objid::bigint=($2::bigint & 4294967295) and objsubid=1)")
            .bind(self.pid).bind(self.key).fetch_one(&mut self.connection);
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), check).await,
            Ok(Ok(true))
        )
    }
    pub async fn release(self) {
        let _ = self.connection.close().await;
    }
}
