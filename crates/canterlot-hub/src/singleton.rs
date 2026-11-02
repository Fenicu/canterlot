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

use sqlx::PgPool;
use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

type HealthMap = HashMap<String, BTreeMap<&'static str, Arc<AtomicBool>>>;
static HEALTH: OnceLock<Mutex<HealthMap>> = OnceLock::new();
fn pool_key(pool: &PgPool) -> String {
    let options = pool.connect_options();
    format!(
        "{}:{}:{}",
        options.get_host(),
        options.get_port(),
        options.get_database().unwrap_or("postgres")
    )
}
struct Health(Arc<AtomicBool>);
impl Drop for Health {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub fn down_services(pool: &PgPool) -> Vec<&'static str> {
    HEALTH
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pool_key(pool))
        .map(|services| {
            services
                .iter()
                .filter(|(_, healthy)| !healthy.load(Ordering::Acquire))
                .map(|(name, _)| *name)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) async fn supervise<F, Fut>(
    pool: &PgPool,
    service: &'static str,
    mut shutdown: watch::Receiver<bool>,
    mut work: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let health = Health(Arc::new(AtomicBool::new(false)));
    HEALTH
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(pool_key(pool))
        .or_default()
        .insert(service, health.0.clone());
    let options = pool.connect_options();
    let mut backoff = Duration::from_millis(250);
    loop {
        // Захват и остановка гоняют: завершившийся одновременно с ней захват
        // нельзя бросать — замок отпустит только закрытие соединения.
        let mut acquire = Box::pin(tokio::time::timeout(
            Duration::from_secs(10),
            Singleton::acquire(&options, service),
        ));
        let acquired = tokio::select! {biased;
            _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>{
                match futures_util::FutureExt::now_or_never(&mut acquire) {
                    Some(done) => done,
                    None => return,
                }
            }
            done=&mut acquire=>done,
        };
        match acquired {
            Ok(Ok(Some(mut singleton))) => {
                health.0.store(true, Ordering::Release);
                let acquired_at = tokio::time::Instant::now();
                let ended = {
                    let work = work();
                    tokio::pin!(work);
                    let mut check = tokio::time::interval(Duration::from_secs(1));
                    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {biased;
                            _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>break "shutdown",
                            _=check.tick()=>{
                                let held=tokio::select! {biased;
                                    _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>break "shutdown",
                                    held=singleton.still_held()=>held,
                                };
                                if !held {break "singleton_lost";}
                            },
                            _=&mut work=>break "singleton_work_ended",
                        }
                    }
                };
                let healthy = acquired_at.elapsed() >= Duration::from_secs(60);
                health.0.store(false, Ordering::Release);
                let _ = tokio::time::timeout(Duration::from_secs(1), singleton.release()).await;
                if ended == "shutdown" {
                    return;
                }
                if healthy {
                    backoff = Duration::from_millis(250);
                }
                tracing::warn!(kind = ended, service, "singleton work suspended");
            }
            Ok(Ok(None)) => health.0.store(true, Ordering::Release),
            Ok(Err(error)) => {
                health.0.store(false, Ordering::Release);
                tracing::warn!(kind="singleton_acquire_failed",service,error=%crate::dberr::describe(&error),"singleton unavailable");
            }
            Err(_) => {
                health.0.store(false, Ordering::Release);
                tracing::warn!(
                    kind = "singleton_acquire_timeout",
                    service,
                    "singleton unavailable"
                );
            }
        }
        tokio::select! {biased;
            _=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>return,
            _=tokio::time::sleep(backoff)=>{},
        }
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "./migrations")]
    async fn slow_lock_check_is_bounded_and_connection_can_be_replaced(pool: PgPool) {
        let options = pool.connect_options();
        let mut lock = Singleton::acquire(&options, "slow-check")
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                sqlx::query("select pg_sleep(3)").execute(&mut lock.connection)
            )
            .await
            .is_err()
        );
        let start = tokio::time::Instant::now();
        assert!(!lock.still_held().await);
        assert!(start.elapsed() < Duration::from_secs(3));
        lock.release().await;
        let mut replacement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(lock) = Singleton::acquire(&options, "slow-check").await.unwrap() {
                    break lock;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(replacement.still_held().await);
        replacement.release().await;
    }
}
