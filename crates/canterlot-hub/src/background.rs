use sqlx::PgPool;
use tokio::{sync::mpsc, sync::watch, task::JoinHandle};
use uuid::Uuid;

use crate::{config::HubTimers, dispatcher, machine_info, outbox, session::Registry};

pub struct BackgroundDeps {
    pub pool: PgPool,
    pub registry: Registry,
    pub timers: HubTimers,
    pub dispatch_rx: mpsc::Receiver<Uuid>,
}

/// Фоновая служба, завершившаяся паникой.
#[derive(Debug, PartialEq, Eq)]
pub struct ServiceFailed(pub &'static str);

pub struct Background {
    signal: watch::Sender<bool>,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
}

impl Background {
    pub fn start(deps: BackgroundDeps) -> Self {
        let (signal, stopped) = watch::channel(false);
        let BackgroundDeps {
            pool,
            registry,
            timers,
            dispatch_rx,
        } = deps;
        let tasks = vec![
            (
                "outbox_task_error",
                tokio::spawn(outbox::run(
                    pool.clone(),
                    registry.clone(),
                    timers.clone(),
                    stopped.clone(),
                )),
            ),
            (
                "machine_status_task_error",
                tokio::spawn(machine_info::run_status_projection(
                    pool.clone(),
                    registry.clone(),
                    stopped.clone(),
                )),
            ),
            (
                "dispatcher_task_error",
                tokio::spawn(dispatcher::run(
                    pool,
                    registry,
                    dispatch_rx,
                    timers,
                    stopped,
                )),
            ),
        ];
        Self { signal, tasks }
    }

    /// Останавливает все службы и дожидается каждой; ошибка называет первую упавшую.
    pub async fn stop(self) -> Result<(), ServiceFailed> {
        let _ = self.signal.send(true);
        let mut failed = None;
        for (name, task) in self.tasks {
            if task.await.is_err() && failed.is_none() {
                failed = Some(ServiceFailed(name));
            }
        }
        failed.map_or(Ok(()), Err)
    }
}
