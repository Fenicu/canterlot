use sqlx::PgPool;
use tokio::{sync::mpsc, sync::watch, task::JoinHandle};
use uuid::Uuid;

use crate::{config::HubTimers, dispatcher, machine_info, outbox, session::Registry};

pub struct BackgroundDeps {
    pub pool: PgPool,
    pub registry: Registry,
    pub timers: HubTimers,
    pub dispatch_rx: mpsc::Receiver<Uuid>,
    pub scheduler: crate::scheduler::Scheduler,
    pub activity: crate::config::ActivityConfig,
    pub machines: crate::config::MachinesConfig,
    pub multi_process: bool,
    /// Сборщик вложений; `None` у служебных стендов без хранилища.
    pub attachments: Option<(
        std::sync::Arc<dyn crate::attachments::BlobStore>,
        crate::config::AttachmentsConfig,
    )>,
}

/// Фоновая служба, завершившаяся паникой.
#[derive(Debug, PartialEq, Eq)]
pub struct ServiceFailed(pub &'static str);

pub struct Background {
    signal: watch::Sender<bool>,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
    bus: crate::bus::EventBus,
}

impl Background {
    pub fn start(deps: BackgroundDeps) -> Self {
        let (signal, stopped) = watch::channel(false);
        let BackgroundDeps {
            pool,
            registry,
            timers,
            dispatch_rx,
            scheduler,
            activity,
            machines,
            multi_process,
            attachments,
        } = deps;
        let bus = crate::bus::EventBus::spawn(pool.clone(), multi_process, stopped.clone());
        let mut tasks = vec![
            (
                "activity_task_error",
                tokio::spawn(crate::activity::run(
                    pool.clone(),
                    bus.clone(),
                    activity,
                    machines,
                    stopped.clone(),
                )),
            ),
            (
                "scheduler_task_error",
                tokio::spawn(scheduler.run(stopped.clone())),
            ),
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
                    pool.clone(),
                    registry,
                    dispatch_rx,
                    timers,
                    stopped.clone(),
                )),
            ),
        ];
        if let Some((store, config)) = attachments {
            tasks.push((
                "attachment_gc_task_error",
                tokio::spawn(crate::attachments::gc::run(pool, store, config, stopped)),
            ));
        }
        Self { signal, tasks, bus }
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
        self.bus.join().await;
        failed.map_or(Ok(()), Err)
    }
}
