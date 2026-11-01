use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use sqlx::{PgConnection, PgPool, postgres::PgListener};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};

#[derive(Clone, Debug)]
pub struct Notice {
    pub scope: String,
    pub kind: String,
    pub scope_seq: i64,
}

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<Notice>>,
    listener: Option<Arc<Mutex<Option<JoinHandle<()>>>>>,
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
}

static BUS: OnceLock<EventBus> = OnceLock::new();
static MULTI_DATABASES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

fn databases() -> &'static Mutex<HashMap<String, usize>> {
    MULTI_DATABASES.get_or_init(Default::default)
}

impl EventBus {
    pub fn local() -> Self {
        BUS.get_or_init(|| Self {
            sender: broadcast::channel(256).0,
            listener: None,
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
        })
        .clone()
    }

    pub fn spawn(pool: PgPool, multi_process: bool, shutdown: watch::Receiver<bool>) -> Self {
        let mut bus = Self::local();
        let database = pool
            .connect_options()
            .get_database()
            .unwrap_or("postgres")
            .to_owned();
        if multi_process {
            *databases()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(database.clone())
                .or_default() += 1;
            let notices = bus.clone();
            bus.listener = Some(Arc::new(Mutex::new(Some(tokio::spawn(async move {
                listen(pool, notices, shutdown).await;
                let mut modes = databases().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(count) = modes.get_mut(&database) {
                    *count -= 1;
                    if *count == 0 {
                        modes.remove(&database);
                    }
                }
            })))));
        }
        bus
    }

    pub fn publish(&self, scope: &str, kind: &str, scope_seq: i64) {
        #[cfg(feature = "fault-injection")]
        if self
            .faults
            .take_drop(crate::fault_injection::Point::DropBusNotice, scope)
        {
            return;
        }
        let _ = self.sender.send(Arc::new(Notice {
            scope: scope.into(),
            kind: kind.into(),
            scope_seq,
        }));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Notice>> {
        self.sender.subscribe()
    }

    pub async fn join(&self) {
        let task = self
            .listener
            .as_ref()
            .and_then(|listener| listener.lock().unwrap_or_else(|e| e.into_inner()).take());
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    #[cfg(feature = "fault-injection")]
    pub fn drop_next_notice(&self, scope: &str) {
        self.faults
            .drop_next_at(crate::fault_injection::Point::DropBusNotice, scope);
    }
}

pub(crate) async fn appended(
    conn: &mut PgConnection,
    scope: &str,
    kind: &str,
    seq: i64,
) -> Result<(), sqlx::Error> {
    if let Some(bus) = BUS.get() {
        bus.publish(scope, kind, seq);
    }
    let any_multi = !databases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty();
    if any_multi {
        let database: String = sqlx::query_scalar("select current_database()")
            .fetch_one(&mut *conn)
            .await?;
        let enabled = databases()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&database);
        if enabled {
            sqlx::query("select pg_notify('canterlot_events', $1)")
                .bind(format!("{scope}|{kind}"))
                .execute(conn)
                .await?;
        }
    }
    Ok(())
}

/// Notice будит читателя до commit; курсор проверяется повторно с ограниченным ожиданием.
pub async fn wait_visible(
    pool: &PgPool,
    notice: &Notice,
    poll_interval: Duration,
) -> Result<bool, sqlx::Error> {
    if notice.scope_seq <= 0 {
        return Ok(true);
    }
    let deadline = tokio::time::Instant::now() + poll_interval;
    let mut delay = Duration::from_millis(5);
    loop {
        let visible: bool = sqlx::query_scalar(
            "select coalesce((select last_seq >= $2 from scope_counters where scope=$1), false)",
        )
        .bind(&notice.scope)
        .bind(notice.scope_seq)
        .fetch_one(pool)
        .await?;
        if visible {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + delay)).await;
        delay = (delay * 2).min(Duration::from_millis(100));
    }
}

async fn listen(pool: PgPool, bus: EventBus, mut shutdown: watch::Receiver<bool>) {
    let mut backoff = Duration::from_millis(100);
    loop {
        if *shutdown.borrow() {
            return;
        }
        let connection = tokio::select! {
            biased;
            _ = crate::dispatcher::wait_for_shutdown(&mut shutdown) => return,
            result = async { let mut listener = PgListener::connect_with(&crate::db::listener_pool(&pool)).await?; listener.eager_reconnect(false); listener.listen("canterlot_events").await?; Ok::<_,sqlx::Error>(listener) } => result,
        };
        if let Ok(mut listener) = connection {
            bus.publish("*", "*", 0);
            backoff = Duration::from_millis(100);
            loop {
                let result = tokio::select! {
                    biased;
                    _ = crate::dispatcher::wait_for_shutdown(&mut shutdown) => return,
                    result = listener.try_recv() => result,
                };
                match result {
                    Ok(Some(notification)) => {
                        if let Some((scope, kind)) = notification.payload().split_once('|') {
                            bus.publish(scope, kind, 0);
                        }
                    }
                    _ => break,
                }
            }
        }
        tokio::select! {
            biased;
            _ = crate::dispatcher::wait_for_shutdown(&mut shutdown) => return,
            _ = tokio::time::sleep(backoff) => {},
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
