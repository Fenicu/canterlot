use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use canterlot_proto::worker::v1::{
    Command, ExecutionObservation, JournalCursor, JournalRecord, StartExecution, command::Kind,
    record_body::Body,
};
use tokio::{
    sync::{OnceCell, mpsc, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    attachments::AttachmentSource,
    config::{ExecutionConfig, WorkerAttachmentsConfig},
    media::MediaSink,
    session::WorkerError,
    session_store::StoreClient,
    store::{Accept, StoreError, WorkerStore},
    supervisor::ipc::IpcError,
};

mod manager;
mod procfs;
mod spawn;

use manager::{Actor, Msg, Start};

pub struct SjRelay {
    pub journal_id: Uuid,
    pub records: Vec<JournalRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Store(#[from] WorkerError),
    #[error("execution I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("supervisor ipc failed: {0}")]
    Ipc(#[from] IpcError),
    #[error("invalid command")]
    InvalidCommand,
    #[error("supervisor did not attach in time")]
    AttachTimeout,
    #[error("supervisor verification failed: {0}")]
    Verify(&'static str),
    #[error("execution manager is shut down")]
    Shutdown,
}

pub(crate) struct ExecInfo {
    pub conversation_id: Uuid,
    pub generation: u64,
    pub state: String,
    pub outcome: String,
    pub attached: bool,
    pub sj_journal_id: Option<Uuid>,
    pub last_seq: u64,
    pub acked_seq: u64,
    pub retired: bool,
    pub cj_terminal_seq: Option<u64>,
}

#[derive(Clone, Default)]
pub(crate) struct AdapterConfig {
    pub claude_code: Option<crate::config::ClaudeCodeConfig>,
    pub codex: Option<crate::config::CodexConfig>,
    pub profiles: Vec<crate::config::ProfileConfig>,
}

pub(crate) struct Shared {
    pub adapters: Mutex<AdapterConfig>,
    pub route_lock: tokio::sync::Mutex<()>,
    pub config: ExecutionConfig,
    pub attachment_config: WorkerAttachmentsConfig,
    pub attachment_source: watch::Sender<Option<Arc<dyn AttachmentSource>>>,
    pub media_sink: watch::Sender<Option<Arc<dyn MediaSink>>>,
    pub fetch_slots: Arc<tokio::sync::Semaphore>,
    pub store: StoreClient,
    pub relay: mpsc::Sender<SjRelay>,
    pub enabled: AtomicBool,
    pub cj_acked: AtomicU64,
    pub hub: watch::Sender<Option<Arc<HashMap<Uuid, u64>>>>,
    pub shutdown: watch::Receiver<bool>,
    pub infos: Mutex<HashMap<Uuid, ExecInfo>>,
    pub probes: Mutex<HashMap<Uuid, crate::machine::metrics::ExecutionProbe>>,
    pub actors: Mutex<HashMap<Uuid, mpsc::UnboundedSender<Msg>>>,
    pub tasks: Mutex<Vec<JoinHandle<()>>>,
    pub runtime_dir: Option<PathBuf>,
    pub data_dir: OnceCell<PathBuf>,
    pub boot_id: OnceCell<String>,
    #[cfg(feature = "fault-injection")]
    pub spawn_delay: Mutex<std::time::Duration>,
    #[cfg(feature = "fault-injection")]
    pub fail_sends: Mutex<usize>,
}

pub(crate) fn open(store: &mut Option<WorkerStore>) -> Result<&mut WorkerStore, WorkerError> {
    store
        .as_mut()
        .ok_or(WorkerError::Store(StoreError::NeedsReopen))
}

impl Shared {
    pub(crate) fn remember_ownership(&self, record: &canterlot_proto::worker::ipc::Ownership) {
        let Ok(id) = Uuid::parse_str(&record.execution_id) else {
            return;
        };
        let mut probes = self.probes.lock().expect("probes lock");
        if record.phase == canterlot_proto::worker::ipc::OwnershipPhase::Released as i32 {
            probes.remove(&id);
            return;
        }
        let members = [
            (record.supervisor_pid, record.supervisor_start_ticks),
            (record.cli_pid, record.cli_start_ticks),
        ]
        .into_iter()
        .filter(|(pid, ticks)| *pid > 0 && *ticks > 0)
        .collect();
        probes.insert(
            id,
            crate::machine::metrics::ExecutionProbe {
                execution_id: id,
                members,
            },
        );
    }

    pub(crate) async fn data_dir(&self) -> Result<&PathBuf, ExecError> {
        self.data_dir
            .get_or_try_init(|| async {
                self.store
                    .call(|store| Ok(open(store)?.data_dir().to_path_buf()))
                    .await
                    .map_err(ExecError::from)
            })
            .await
    }

    pub(crate) async fn boot_id(&self) -> Result<&String, ExecError> {
        self.boot_id
            .get_or_try_init(|| async {
                tokio::task::spawn_blocking(procfs::boot_id)
                    .await
                    .map_err(|_| ExecError::Shutdown)?
                    .map_err(ExecError::from)
            })
            .await
    }

    /// Факты управляющей службы пишутся в CJ; погашенные команды освобождают inbox.
    pub(crate) async fn cj(
        &self,
        facts: Vec<Body>,
        terminal: Vec<(String, u32)>,
    ) -> Result<(), ExecError> {
        let states = self
            .store
            .call(move |store| {
                let store = open(store)?;
                let mut states = Vec::new();
                for body in facts {
                    let state = match &body {
                        Body::ExecutionState(fact)
                            if matches!(
                                fact.state.as_str(),
                                "stopped" | "failed" | "owner_lost"
                            ) =>
                        {
                            Some(fact.clone())
                        }
                        _ => None,
                    };
                    let seq = store.append_cj_fact(body)?;
                    if let Some(state) = state {
                        states.push((seq, state));
                    }
                }
                for (command_id, attempt) in terminal {
                    store.mark_terminal(&command_id, attempt)?;
                }
                Ok(states)
            })
            .await?;
        self.remember_cj_states(states);
        Ok(())
    }

    fn remember_cj_states(&self, states: Vec<(u64, canterlot_proto::worker::v1::ExecutionState)>) {
        for (seq, state) in states {
            if let Ok(id) = Uuid::parse_str(&state.execution_id) {
                let mut infos = self.infos.lock().expect("infos lock");
                let info = infos.entry(id).or_insert_with(|| ExecInfo {
                    conversation_id: Uuid::nil(),
                    generation: state.generation,
                    state: state.state.clone(),
                    outcome: state.outcome.clone(),
                    attached: false,
                    sj_journal_id: None,
                    last_seq: 0,
                    acked_seq: 0,
                    retired: false,
                    cj_terminal_seq: None,
                });
                info.state = state.state;
                info.outcome = state.outcome;
                info.cj_terminal_seq = Some(seq);
                info.retired = seq <= self.cj_acked.load(Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn with_info(&self, id: Uuid, change: impl FnOnce(&mut ExecInfo)) {
        if let Some(info) = self.infos.lock().expect("infos lock").get_mut(&id) {
            change(info);
        }
    }
}

pub struct ExecutionManager {
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
}

impl Drop for ExecutionManager {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

impl ExecutionManager {
    pub fn new(
        config: ExecutionConfig,
        store: StoreClient,
        relay: mpsc::Sender<SjRelay>,
    ) -> ExecutionManager {
        Self::new_with_attachments(config, store, relay, WorkerAttachmentsConfig::default())
    }

    pub fn new_with_attachments(
        config: ExecutionConfig,
        store: StoreClient,
        relay: mpsc::Sender<SjRelay>,
        attachment_config: WorkerAttachmentsConfig,
    ) -> Self {
        let (shutdown, shutdown_rx) = watch::channel(false);
        ExecutionManager {
            shared: Arc::new(Shared {
                route_lock: tokio::sync::Mutex::new(()),
                config,
                adapters: Mutex::default(),
                fetch_slots: Arc::new(tokio::sync::Semaphore::new(
                    attachment_config.fetch_parallelism.clamp(1, 2),
                )),
                attachment_config,
                attachment_source: watch::channel(None).0,
                media_sink: watch::channel(None).0,
                store,
                relay,
                enabled: AtomicBool::new(true),
                cj_acked: AtomicU64::new(0),
                hub: watch::channel(None).0,
                shutdown: shutdown_rx,
                infos: Mutex::default(),
                probes: Mutex::default(),
                actors: Mutex::default(),
                tasks: Mutex::default(),
                runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
                data_dir: OnceCell::new(),
                boot_id: OnceCell::new(),
                #[cfg(feature = "fault-injection")]
                spawn_delay: Mutex::new(std::time::Duration::ZERO),
                #[cfg(feature = "fault-injection")]
                fail_sends: Mutex::new(0),
            }),
            shutdown,
        }
    }

    #[cfg(feature = "fault-injection")]
    pub fn set_spawn_delay(&self, delay: std::time::Duration) {
        *self.shared.spawn_delay.lock().expect("delay lock") = delay;
    }

    #[cfg(feature = "fault-injection")]
    pub fn fail_next_ipc_send(&self) {
        *self.shared.fail_sends.lock().expect("send fault lock") += 1;
    }

    pub fn configure_adapters(
        &self,
        claude_code: Option<crate::config::ClaudeCodeConfig>,
        codex: Option<crate::config::CodexConfig>,
        profiles: Vec<crate::config::ProfileConfig>,
    ) {
        *self.shared.adapters.lock().expect("adapter config lock") = AdapterConfig {
            claude_code,
            codex,
            profiles,
        };
    }

    pub fn set_attachment_source(&self, source: Option<Arc<dyn AttachmentSource>>) {
        self.shared.attachment_source.send_replace(source);
    }

    /// Канал загрузки картинок агента; `None` на время разрыва.
    pub fn set_media_sink(&self, sink: Option<Arc<dyn MediaSink>>) {
        self.shared.media_sink.send_replace(sink);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Останавливает только акторов; процессы исполнений не затрагиваются.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let tasks = std::mem::take(&mut *self.shared.tasks.lock().expect("tasks lock"));
        for task in tasks {
            let _ = task.await;
        }
    }

    pub fn probes(&self) -> Vec<crate::machine::metrics::ExecutionProbe> {
        let mut probes: Vec<_> = self
            .shared
            .probes
            .lock()
            .expect("probes lock")
            .values()
            .cloned()
            .collect();
        probes.sort_by_key(|p| p.execution_id);
        probes
    }

    pub(crate) async fn permitted_cursors(
        &self,
        pilot: bool,
    ) -> Result<Vec<JournalCursor>, WorkerError> {
        let cursors = self.journal_cursors();
        if pilot {
            return Ok(cursors);
        }
        self.shared
            .store
            .call(move |slot| {
                let store = open(slot)?;
                let mut permitted = Vec::new();
                for cursor in cursors {
                    let id =
                        Uuid::parse_str(&cursor.journal_id).map_err(|_| WorkerError::Protocol)?;
                    if !store.journal_next_requires_pilot(id)? {
                        permitted.push(cursor);
                    }
                }
                Ok(permitted)
            })
            .await
    }

    pub fn observations(&self) -> Vec<ExecutionObservation> {
        let infos = self.shared.infos.lock().expect("infos lock");
        let mut list: Vec<_> = infos
            .iter()
            .filter(|(_, info)| !info.retired)
            .map(|(id, info)| ExecutionObservation {
                execution_id: id.to_string(),
                generation: info.generation,
                state: info.state.clone(),
                outcome: info.outcome.clone(),
                supervisor_attached: info.attached,
                sj_journal_id: info
                    .sj_journal_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            })
            .collect();
        list.sort_by(|a, b| a.execution_id.cmp(&b.execution_id));
        list
    }

    pub fn journal_cursors(&self) -> Vec<JournalCursor> {
        let infos = self.shared.infos.lock().expect("infos lock");
        let mut list: Vec<_> = infos
            .values()
            .filter(|info| !info.retired && info.attached)
            .filter_map(|info| {
                info.sj_journal_id.map(|journal| JournalCursor {
                    journal_id: journal.to_string(),
                    first_stored_seq: info.acked_seq + 1,
                    last_seq: info.last_seq,
                    acked_seq_known_to_worker: info.acked_seq,
                })
            })
            .collect();
        list.sort_by(|a, b| a.journal_id.cmp(&b.journal_id));
        list
    }

    pub fn on_hub_session(&self, committed: HashMap<Uuid, u64>) {
        self.shared.hub.send_replace(Some(Arc::new(committed)));
    }

    pub fn on_cj_ack(&self, up_to: u64) {
        self.shared.cj_acked.fetch_max(up_to, Ordering::Relaxed);
        for info in self.shared.infos.lock().expect("infos lock").values_mut() {
            if info.cj_terminal_seq.is_some_and(|seq| seq <= up_to) {
                info.retired = true;
            }
        }
    }

    pub fn on_sj_ack(&self, journal_id: Uuid, up_to: u64) {
        self.send_journal(journal_id, Msg::SjAck(up_to));
    }

    pub(crate) fn replay_sj(&self, journal_id: Uuid, committed: u64) {
        self.send_journal(journal_id, Msg::Replay(committed));
    }

    fn send_journal(&self, journal_id: Uuid, message: Msg) {
        let execution = self
            .shared
            .infos
            .lock()
            .expect("infos lock")
            .iter()
            .find(|(_, info)| info.sj_journal_id == Some(journal_id))
            .map(|(id, _)| *id);
        if let Some(sender) = execution.and_then(|id| {
            self.shared
                .actors
                .lock()
                .expect("actors lock")
                .get(&id)
                .cloned()
        }) {
            let _ = sender.send(message);
        }
    }

    pub async fn startup_verify(&self) -> Result<Vec<Uuid>, ExecError> {
        let _route = self.shared.route_lock.lock().await;
        manager::cleanup_released(&self.shared).await?;
        let records = self
            .shared
            .store
            .call(|store| {
                Ok(open(store)?
                    .ownership()
                    .live()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>())
            })
            .await?;
        let states = self
            .shared
            .store
            .call(|store| {
                let store = open(store)?;
                let cursor = store.cj_cursor()?;
                Ok(store
                    .cj_records_from(cursor.acked_seq_known_to_worker + 1, usize::MAX)?
                    .into_iter()
                    .filter_map(|record| match record.body.and_then(|body| body.body) {
                        Some(Body::ExecutionState(state))
                            if matches!(
                                state.state.as_str(),
                                "stopped" | "failed" | "owner_lost"
                            ) =>
                        {
                            Some((record.seq, state))
                        }
                        _ => None,
                    })
                    .collect())
            })
            .await?;
        let mut attached = Vec::new();
        for record in records {
            self.shared.remember_ownership(&record);
            let Ok(id) = Uuid::parse_str(&record.execution_id) else {
                continue;
            };
            let conversation_id = Uuid::parse_str(&record.conversation_id).unwrap_or_default();
            self.shared.infos.lock().expect("infos lock").insert(
                id,
                ExecInfo {
                    conversation_id,
                    generation: record.generation,
                    state: "starting".into(),
                    outcome: String::new(),
                    attached: false,
                    sj_journal_id: Uuid::parse_str(&record.sj_journal_id).ok(),
                    last_seq: 0,
                    acked_seq: 0,
                    retired: false,
                    cj_terminal_seq: None,
                },
            );
            if manager::is_orphaned(&record) {
                self.shared.with_info(id, |info| {
                    info.state = "owner_lost".into();
                });
                continue;
            }
            match manager::verify(&self.shared, &record).await {
                Ok(linked) => {
                    let running = record.cli_pid != 0;
                    self.shared.with_info(id, |info| {
                        info.attached = true;
                        info.sj_journal_id = Uuid::parse_str(&linked.attached.sj_journal_id).ok();
                        info.last_seq = linked.attached.last_sj_seq;
                        info.acked_seq = linked.attached.acked_sj_seq;
                        if running {
                            info.state = "running".into();
                        }
                    });
                    self.spawn_actor(id, Start::Reattach(Box::new((linked, record))));
                    attached.push(id);
                }
                Err(error) => {
                    tracing::warn!(execution_id = %id, error_kind = ?std::mem::discriminant(&error), "ownership not verified");
                    manager::report_owner_lost(&self.shared, id, record.generation).await?;
                }
            }
        }
        let commands = self
            .shared
            .store
            .call(|store| Ok(open(store)?.stored_commands()?))
            .await?;
        for command in commands {
            let Ok(id) = Uuid::parse_str(&command.execution_id) else {
                continue;
            };
            let known = self
                .shared
                .infos
                .lock()
                .expect("infos lock")
                .contains_key(&id);
            // Непривязанные записи 1.2a остаются у сеанса до согласования exec_v1.
            if known
                || matches!(
                    command.kind,
                    Some(Kind::StartExecution(_) | Kind::StopExecution(_))
                )
            {
                self.route(command).await?;
            }
        }
        self.shared.remember_cj_states(states);
        Ok(attached)
    }

    pub async fn handle(&self, command: Command, accept: Accept) -> Result<(), ExecError> {
        let _route = self.shared.route_lock.lock().await;
        match accept {
            Accept::Stored { .. } | Accept::Redelivered { .. } => {}
            Accept::Duplicate => {
                let id = command.command_id.clone();
                let state = self
                    .shared
                    .store
                    .call(move |store| Ok(open(store)?.command_state(&id)?))
                    .await?;
                if state.is_none_or(|state| state.terminal) {
                    return Ok(());
                }
            }
            Accept::Stale | Accept::DroppedInvalidId | Accept::Rejected { .. } => return Ok(()),
        }
        self.route(command).await
    }

    async fn route(&self, command: Command) -> Result<(), ExecError> {
        let Some(kind) = &command.kind else {
            return Ok(());
        };
        if let Kind::StartExecution(start) = kind {
            let start = start.clone();
            return self.start(command, start).await;
        }
        let Ok(id) = Uuid::parse_str(&command.execution_id) else {
            return manager::reject_invalid(&self.shared, &command).await;
        };
        let sender = self
            .shared
            .actors
            .lock()
            .expect("actors lock")
            .get(&id)
            .cloned();
        if let Some(sender) = sender
            && sender.send(Msg::Command(command.clone())).is_ok()
        {
            return Ok(());
        }
        manager::answer_without_execution(&self.shared, &command).await
    }

    async fn start(&self, command: Command, start: StartExecution) -> Result<(), ExecError> {
        let shared = &self.shared;
        let (Ok(id), Ok(conversation_id)) = (
            Uuid::parse_str(&command.execution_id),
            Uuid::parse_str(&start.conversation_id),
        ) else {
            return manager::reject(shared, &command, "unsupported_command").await;
        };
        let existing = shared.actors.lock().expect("actors lock").get(&id).cloned();
        if let Some(sender) = existing {
            let _ = sender.send(Msg::Command(command));
            return Ok(());
        }
        let (known, cancelled) = shared
            .store
            .call(move |store| {
                let store = open(store)?;
                Ok((
                    store.ownership().get(id).is_some(),
                    store.start_cancelled(id),
                ))
            })
            .await?;
        if known || cancelled {
            return shared
                .cj(
                    vec![],
                    vec![(command.command_id.clone(), command.dispatch_attempt)],
                )
                .await;
        }
        let workdir = match manager::validate(shared, &command, &start, conversation_id).await? {
            Ok(workdir) => workdir,
            Err(reason) => return manager::reject(shared, &command, reason).await,
        };
        {
            let mut infos = shared.infos.lock().expect("infos lock");
            infos.insert(
                id,
                ExecInfo {
                    conversation_id,
                    generation: command.execution_generation,
                    state: "starting".into(),
                    outcome: String::new(),
                    attached: false,
                    sj_journal_id: None,
                    last_seq: 0,
                    acked_seq: 0,
                    retired: false,
                    cj_terminal_seq: None,
                },
            );
        }
        self.spawn_actor(
            id,
            Start::Spawn(Box::new(manager::SpawnRequest {
                command,
                start,
                workdir,
                conversation_id,
            })),
        );
        Ok(())
    }

    fn spawn_actor(&self, id: Uuid, start: Start) {
        let generation = self
            .shared
            .infos
            .lock()
            .expect("infos lock")
            .get(&id)
            .map_or(0, |info| info.generation);
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared
            .actors
            .lock()
            .expect("actors lock")
            .insert(id, sender);
        let actor = Actor::new(self.shared.clone(), id, generation, receiver);
        let task = tokio::spawn(actor.run(start));
        self.shared.tasks.lock().expect("tasks lock").push(task);
    }
}
