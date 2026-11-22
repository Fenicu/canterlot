//! Команды хаба носителю: `StageSource`, `CollectOutputs`, `CleanupSource`, `OwnerJobFinished`.
//!
//! Команды долговечные: воркер сохраняет их до исполнения, а после рестарта доигрывает
//! непогашенные, поэтому каждая идемпотентна (`Stager`). Ответ — факт журнала, который
//! погашает команду вместе с записью; пока факт не записан, команда остаётся непогашенной.
//! Долгие команды идут отдельной задачей, чтобы не держать очередь маршрутизации, а повторная
//! доставка той же команды, пока она идёт, второй раз не запускается.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    AttachRunner, Command, OutputFile, OutputsCollectFailed, OutputsCollected, SourceCleaned,
    SourceStageFailed, SourceStaged, command::Kind, record_body::Body,
};
use tokio::sync::watch;
use uuid::Uuid;

use super::{
    cloud::{AttachError, Attachment, CloudState},
    collect::{CollectRequest, Outputs},
    stage::{CleanupRequest, Failure, StageRequest, Stager},
};
use crate::owner_jobs::{Finished, Store};

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("runner commands are not negotiated on this connection")]
    NotNegotiated,
    #[error("not a carrier command")]
    Invalid,
    #[error("carrier command state I/O failed")]
    Io,
}

/// Куда уходят факты; реализацию даёт воркер (журнал управления).
#[async_trait]
pub trait FactSink: Send + Sync {
    /// Факты одной записью и гашение команд, которые они закрывают.
    async fn facts(&self, facts: Vec<Body>, terminal: Vec<(String, u32)>) -> Result<(), ()>;
}

pub struct CarrierCommands {
    stager: Stager,
    store: Arc<Store>,
    sink: Arc<dyn FactSink>,
    cloud: Arc<CloudState>,
    enabled: AtomicBool,
    /// `(job_id, вид команды)` исполняемых сейчас: повторная доставка не запускает вторую копию.
    running: Mutex<HashSet<(String, &'static str)>>,
    shutdown: watch::Receiver<bool>,
}

/// Убирает запись `running`, когда задача команды кончилась любым способом.
struct Running(Arc<CarrierCommands>, (String, &'static str));

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.lock().unwrap().remove(&self.1);
    }
}

/// Строка причины в факте: короткий код и одна строка пояснения.
fn reason(failure: &Failure) -> String {
    format!("{}: {}", failure.reason, failure.message)
}

impl CarrierCommands {
    pub fn new(
        stager: Stager,
        store: Arc<Store>,
        sink: Arc<dyn FactSink>,
        cloud: Arc<CloudState>,
        shutdown: watch::Receiver<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            stager,
            store,
            sink,
            cloud,
            enabled: AtomicBool::new(false),
            running: Mutex::default(),
            shutdown,
        })
    }

    /// `runners_v1` согласована на текущем соединении.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Принимает команду. Долгие идут фоном и возвращаются сразу; `Err(Io)` оставляет команду
    /// непогашенной, чтобы её доиграл следующий сеанс.
    pub async fn handle(self: &Arc<Self>, command: &Command) -> Result<(), CommandError> {
        if !self.enabled() {
            return Err(CommandError::NotNegotiated);
        }
        let terminal = vec![(command.command_id.clone(), command.dispatch_attempt)];
        match &command.kind {
            Some(Kind::StageSource(stage)) => {
                let request = StageRequest {
                    job_id: stage.job_id.clone(),
                    repo: PathBuf::from(&stage.repo),
                    rev: stage.rev.clone(),
                    domain: stage.domain.clone(),
                    target_machine_id: stage.target_machine_id.clone(),
                    target_root_dir: stage.target_root_dir.clone(),
                    workdir: stage.workdir.clone(),
                    repo_key: stage.repo_key.clone(),
                };
                let job = stage.job_id.clone();
                {
                    let me = self.clone();
                    self.spawn("stage", job.clone(), terminal, async move {
                    Some(match me.stager.stage(&request).await {
                        Ok(()) => Body::SourceStaged(SourceStaged { job_id: job }),
                        Err(failure) => {
                            tracing::warn!(kind = "stage_failed", reason = %failure.reason, "source staging failed");
                            Body::SourceStageFailed(SourceStageFailed {
                                job_id: job,
                                reason: reason(&failure),
                            })
                        }
                    })
                })
                };
                Ok(())
            }
            Some(Kind::CollectOutputs(collect)) => {
                let request = CollectRequest {
                    job_id: collect.job_id.clone(),
                    target_machine_id: collect.target_machine_id.clone(),
                    target_root_dir: collect.target_root_dir.clone(),
                    workdir: collect.workdir.clone(),
                    paths: collect.paths.clone(),
                    cap_bytes: collect.cap_bytes,
                };
                let job = collect.job_id.clone();
                {
                    let me = self.clone();
                    self.spawn("collect", job.clone(), terminal, async move {
                    Some(match me.stager.collect(&request).await {
                        Ok(outputs) => {
                            write_notes(&outputs);
                            Body::OutputsCollected(collected(job, &outputs))
                        }
                        Err(failure) => {
                            tracing::warn!(kind = "collect_failed", reason = %failure.reason, "output collection failed");
                            Body::OutputsCollectFailed(OutputsCollectFailed {
                                job_id: job,
                                reason: reason(&failure),
                            })
                        }
                    })
                })
                };
                Ok(())
            }
            Some(Kind::CleanupSource(cleanup)) => {
                let request = CleanupRequest {
                    job_id: cleanup.job_id.clone(),
                    target_machine_id: cleanup.target_machine_id.clone(),
                    target_root_dir: cleanup.target_root_dir.clone(),
                    workdir: cleanup.workdir.clone(),
                    repo_key: cleanup.repo_key.clone(),
                    domain: cleanup.domain.clone(),
                };
                let job = cleanup.job_id.clone();
                {
                    let me = self.clone();
                    self.spawn("cleanup", job.clone(), terminal, async move {
                    match me.stager.cleanup(&request).await {
                        Ok(()) => Some(Body::SourceCleaned(SourceCleaned { job_id: job })),
                        Err(failure) => {
                            // Хаб повторит уборку до предела; факта «убрано» не будет.
                            tracing::warn!(kind = "cleanup_failed", reason = %failure.reason, "source cleanup failed");
                            None
                        }
                    }
                })
                };
                Ok(())
            }
            Some(Kind::OwnerJobFinished(done)) => {
                let finished = Finished {
                    job_id: done.job_id.clone(),
                    state: done.state.clone(),
                    exit_code: done.exit_code,
                    tail: String::from_utf8_lossy(&done.tail).into_owned(),
                    outputs_dir: done.outputs_dir.clone(),
                    outputs_note: done.outputs_note.clone(),
                };
                let store = self.store.clone();
                let written = tokio::task::spawn_blocking(move || store.record(&finished))
                    .await
                    .map_err(|_| CommandError::Io)?;
                match written {
                    Ok(()) => {}
                    // Негодный идентификатор не станет годным от повтора: команда гасится.
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                        tracing::warn!(
                            kind = "owner_job_finished_invalid",
                            "owner job result dropped"
                        );
                    }
                    Err(_) => return Err(CommandError::Io),
                }
                self.sink
                    .facts(vec![], terminal)
                    .await
                    .map_err(|()| CommandError::Io)
            }
            Some(Kind::AttachRunner(attach)) => {
                let command = attach.clone();
                self.attach(&command)?;
                // Хаб считает окно готовым, когда воркер раннера подключился; отдельного факта
                // протокол не несёт, поэтому команда гасится обычным подтверждением.
                self.sink
                    .facts(vec![], terminal)
                    .await
                    .map_err(|()| CommandError::Io)
            }
            _ => Err(CommandError::Invalid),
        }
    }

    /// Принимает подключение облачного раннера. Команда долговечна и идемпотентна по
    /// `window_id`: носитель запоминает её до ответа, а реальное подключение ведёт задача
    /// раннера, поэтому повтор не поднимает второй воркер или туннель.
    fn attach(&self, attach: &AttachRunner) -> Result<(), CommandError> {
        let Ok(machine_id) = Uuid::parse_str(&attach.machine_id) else {
            return Err(CommandError::Invalid);
        };
        let Ok(ssh_port) = u16::try_from(attach.ssh_port) else {
            return Err(CommandError::Invalid);
        };
        let attachment = Attachment {
            window_id: attach.window_id.clone(),
            machine_id,
            host: attach.host.clone(),
            ssh_port,
            host_key_public: attach.host_key_public.clone(),
            root_dir: PathBuf::from(&attach.root_dir),
            max_running: attach.max_running,
            lease_slots: attach.lease_slots,
        };
        match self.cloud.apply(attachment) {
            Ok(()) => Ok(()),
            Err(AttachError::Invalid) => Err(CommandError::Invalid),
            Err(AttachError::Io) => Err(CommandError::Io),
        }
    }

    /// Исполняет команду фоном и записывает факт вместе с гашением. Повтор той же команды,
    /// пока она идёт, пропускается.
    fn spawn(
        self: &Arc<Self>,
        kind: &'static str,
        job: String,
        terminal: Vec<(String, u32)>,
        work: impl std::future::Future<Output = Option<Body>> + Send + 'static,
    ) {
        let key = (job, kind);
        if !self.running.lock().unwrap().insert(key.clone()) {
            return;
        }
        let me = self.clone();
        let mut shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            let _running = Running(me.clone(), key);
            let fact = tokio::select! {
                fact = work => fact,
                _ = shutdown.wait_for(|stop| *stop) => return,
            };
            let facts = fact.into_iter().collect();
            if me.sink.facts(facts, terminal).await.is_err() {
                tracing::warn!(
                    kind = "carrier_fact_not_written",
                    "the command stays pending and replays"
                );
            }
        });
    }
}

fn collected(job_id: String, outputs: &Outputs) -> OutputsCollected {
    OutputsCollected {
        job_id,
        local_dir: outputs.local_dir.display().to_string(),
        files: outputs
            .files
            .iter()
            .map(|file| OutputFile {
                path: file.path.clone(),
                bytes: file.bytes,
            })
            .collect(),
        truncated: outputs.truncated,
    }
}

/// Факт не несёт списков отсутствующих и пропущенных путей: они остаются рядом с результатами,
/// чтобы владелец видел, почему чего-то нет.
fn write_notes(outputs: &Outputs) {
    if outputs.missing.is_empty() && outputs.skipped.is_empty() && !outputs.truncated {
        return;
    }
    let mut text = String::new();
    if outputs.truncated {
        text.push_str("truncated: the size cap was reached\n");
    }
    for path in &outputs.missing {
        text.push_str(&format!("missing: {path}\n"));
    }
    for path in &outputs.skipped {
        text.push_str(&format!("skipped (not a regular file): {path}\n"));
    }
    let _ = std::fs::write(outputs.local_dir.join("COLLECT-NOTES.txt"), text);
}
