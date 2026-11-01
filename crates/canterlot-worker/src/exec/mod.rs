use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use canterlot_proto::worker::v1::{
    AgentCallRequest, Command, ExecutionObservation, JournalCursor, JournalRecord, StartExecution,
    command::Kind, record_body::Body,
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

mod group;
mod lost;
mod manager;
mod procfs;
mod spawn;

use manager::{Msg, Start};

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

/// Вызов инструмента агента в хаб через канал данных; реализацию ставит сессия.
#[async_trait::async_trait]
pub trait AgentCaller: Send + Sync {
    async fn call(
        &self,
        request: AgentCallRequest,
    ) -> Result<canterlot_proto::worker::v1::AgentCallResponse, tonic::Status>;
}

/// Срок RPC вызова инструмента агента по каналу данных.
pub const AGENT_CALL_RPC_TIMEOUT: Duration = Duration::from_secs(15);
/// Срок управляющей службы: чуть дольше RPC, чтобы менеджер ответил первым.
pub const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(20);

const WAIT_TOOL: &str = "sessions.wait";
const WAIT_DEFAULT_SECONDS: u64 = 120;
const WAIT_MAX_SECONDS: u64 = 300;
/// Запас сверх ожидания: опрос хаба, сеть и разбор ответа.
const WAIT_RPC_SLACK: Duration = Duration::from_secs(10);
const WAIT_SUPERVISOR_SLACK: Duration = Duration::from_secs(15);

/// Сроки одного вызова: у каждого уровня свой, управляющая служба всегда позже менеджера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallDeadlines {
    pub supervisor: Duration,
    pub rpc: Duration,
}

/// Блокирующее ожидание сессии держится столько, сколько просила модель, плюс запас;
/// остальные инструменты отвечают быстро.
pub fn deadline_for(tool: &str, arguments: &serde_json::Value) -> CallDeadlines {
    if tool != WAIT_TOOL {
        return CallDeadlines {
            supervisor: AGENT_CALL_TIMEOUT,
            rpc: AGENT_CALL_RPC_TIMEOUT,
        };
    }
    let seconds = arguments
        .get("timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(WAIT_DEFAULT_SECONDS)
        .clamp(1, WAIT_MAX_SECONDS);
    let wait = Duration::from_secs(seconds);
    CallDeadlines {
        supervisor: wait + WAIT_SUPERVISOR_SLACK,
        rpc: wait + WAIT_RPC_SLACK,
    }
}

/// Выполняет вызов инструмента агента от имени исполнения: заполняет идентификатор
/// и поколение из состояния управляющей службы. Без сессии хаба — быстрый отказ;
/// `UNIMPLEMENTED` старшего хаба — отдельная ошибка. В журнал попадают только
/// инструмент, идентификатор вызова и код исхода, не аргументы и не результат.
pub async fn agent_call(
    caller: Option<Arc<dyn AgentCaller>>,
    execution_id: Uuid,
    generation: u64,
    call_id: Uuid,
    tool: String,
    arguments: serde_json::Value,
) -> crate::adapter::AgentCallResult {
    use crate::adapter::AgentCallResult;
    let outcome = |result: AgentCallResult| {
        let outcome = match &result {
            AgentCallResult::Ok(_) => "ok",
            AgentCallResult::Err { code, .. } => code.as_str(),
        };
        tracing::info!(
            kind = "agent_call",
            tool = %tool,
            call_id = %call_id,
            outcome = outcome,
            "agent call finished"
        );
        result
    };
    let Some(caller) = caller else {
        return outcome(AgentCallResult::Err {
            code: "hub_unreachable".into(),
            message: "нет связи с хабом".into(),
        });
    };
    let arguments_json = serde_json::to_string(&arguments).unwrap_or_default();
    let response = tokio::time::timeout(
        deadline_for(&tool, &arguments).rpc,
        caller.call(AgentCallRequest {
            execution_id: execution_id.to_string(),
            generation,
            call_id: call_id.to_string(),
            tool: tool.clone(),
            arguments_json,
        }),
    )
    .await;
    match response {
        Err(_) => outcome(AgentCallResult::Err {
            code: "hub_timeout".into(),
            message: "хаб не ответил вовремя".into(),
        }),
        Ok(Err(status)) => outcome(AgentCallResult::Err {
            code: if status.code() == tonic::Code::Unimplemented {
                "hub_unsupported"
            } else {
                "hub_error"
            }
            .into(),
            message: "хаб отклонил вызов".into(),
        }),
        Ok(Ok(response)) => {
            if response.error_code.is_empty() {
                match serde_json::from_str(&response.result_json) {
                    Ok(value) => outcome(AgentCallResult::Ok(value)),
                    Err(_) => outcome(AgentCallResult::Err {
                        code: "hub_error".into(),
                        message: "некорректный ответ хаба".into(),
                    }),
                }
            } else {
                outcome(AgentCallResult::Err {
                    code: response.error_code,
                    message: response.error_message,
                })
            }
        }
    }
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
    /// Журнал потерянного сопровождения: его точка продолжения нужна сессии, даже когда
    /// исполнение для наблюдений уже списано.
    pub lost_journal: bool,
}

#[cfg(test)]
pub(crate) struct TestActor(mpsc::UnboundedReceiver<Msg>);

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TestSignal {
    Ack(u64),
    Replay(u64),
}

#[cfg(test)]
impl TestActor {
    pub(crate) fn signals(&mut self) -> Vec<TestSignal> {
        let mut signals = Vec::new();
        while let Ok(message) = self.0.try_recv() {
            match message {
                Msg::SjAck(up_to) => signals.push(TestSignal::Ack(up_to)),
                Msg::Replay(committed) => signals.push(TestSignal::Replay(committed)),
                _ => panic!("unexpected actor message"),
            }
        }
        signals
    }
}

#[derive(Clone, Default)]
pub(crate) struct AdapterConfig {
    pub claude_code: Option<crate::config::ClaudeCodeConfig>,
    pub codex: Option<crate::config::CodexConfig>,
    pub opencode: Option<crate::config::OpenCodeConfig>,
}

pub(crate) struct Shared {
    pub adapters: Mutex<AdapterConfig>,
    pub route_lock: tokio::sync::Mutex<()>,
    pub config: ExecutionConfig,
    pub attachment_config: WorkerAttachmentsConfig,
    pub attachment_source: watch::Sender<Option<Arc<dyn AttachmentSource>>>,
    pub media_sink: watch::Sender<Option<Arc<dyn MediaSink>>>,
    /// Клиент канала данных для вызовов инструментов агента; `None` без сессии хаба.
    pub agent_caller: watch::Sender<Option<Arc<dyn AgentCaller>>>,
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
    /// Уборки рабочих копий в работе: повтор той же команды не запускает вторую.
    pub worktree_jobs: Mutex<HashSet<String>>,
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

    pub(crate) fn remember_cj_states(
        &self,
        states: Vec<(u64, canterlot_proto::worker::v1::ExecutionState)>,
    ) {
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
                    lost_journal: false,
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
                agent_caller: watch::channel(None).0,
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
                worktree_jobs: Mutex::default(),
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
        opencode: Option<crate::config::OpenCodeConfig>,
    ) {
        *self.shared.adapters.lock().expect("adapter config lock") = AdapterConfig {
            claude_code,
            codex,
            opencode,
        };
    }

    /// Клиент вызовов инструментов агента; `None` на время разрыва.
    pub fn set_agent_caller(&self, caller: Option<Arc<dyn AgentCaller>>) {
        self.shared.agent_caller.send_replace(caller);
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
            .filter(|info| (!info.retired && info.attached) || info.lost_journal)
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

    /// Сопровождение умерло, не сохранив подтверждение: наблюдатель перечитал с диска то, что
    /// хаб уже зафиксировал, и без ответа ждал бы вечно. Живому журналу повтор не нужен: его
    /// актор слышал ответ сам, а лишний дошёл бы до сопровождения.
    pub fn on_sj_resend_committed(&self, journal_id: Uuid, up_to: u64) {
        let lost = self
            .shared
            .infos
            .lock()
            .expect("infos lock")
            .values()
            .any(|info| info.lost_journal && info.sj_journal_id == Some(journal_id));
        if lost {
            self.send_journal(journal_id, Msg::SjAck(up_to));
        }
    }

    /// Тестовый адресат журнала: запоминает, что до него дошло.
    #[cfg(test)]
    pub(crate) fn test_actor(&self, journal: Uuid, lost: bool) -> TestActor {
        let id = Uuid::now_v7();
        let (sender, rx) = mpsc::unbounded_channel();
        self.shared.infos.lock().expect("infos lock").insert(
            id,
            ExecInfo {
                conversation_id: Uuid::nil(),
                generation: 1,
                state: "owner_lost".into(),
                outcome: String::new(),
                attached: !lost,
                sj_journal_id: Some(journal),
                last_seq: 0,
                acked_seq: 0,
                retired: false,
                cj_terminal_seq: None,
                lost_journal: lost,
            },
        );
        self.shared
            .actors
            .lock()
            .expect("actors lock")
            .insert(id, sender);
        TestActor(rx)
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
            .call(|store| Ok(open(store)?.ownership().unreleased()))
            .await?;
        let mut states: Vec<(u64, canterlot_proto::worker::v1::ExecutionState)> = self
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
        let mut reclaimed = HashMap::new();
        for record in records {
            let Ok(id) = Uuid::parse_str(&record.execution_id) else {
                continue;
            };
            let reported = manager::is_reported(&record);
            let lost = reported || manager::is_orphaned(&record);
            let rebooted =
                !record.boot_id.is_empty() && record.boot_id != *self.shared.boot_id().await?;
            let verified = if rebooted {
                Err(ExecError::Verify("boot_id"))
            } else {
                manager::verify(&self.shared, &record).await
            };
            self.shared.remember_ownership(&record);
            let conversation_id = Uuid::parse_str(&record.conversation_id).unwrap_or_default();
            self.shared.infos.lock().expect("infos lock").insert(
                id,
                ExecInfo {
                    conversation_id,
                    generation: record.generation,
                    state: if lost {
                        "owner_lost".into()
                    } else {
                        "starting".into()
                    },
                    outcome: String::new(),
                    attached: false,
                    sj_journal_id: Uuid::parse_str(&record.sj_journal_id).ok(),
                    last_seq: 0,
                    acked_seq: 0,
                    retired: reported && verified.is_err(),
                    cj_terminal_seq: None,
                    lost_journal: false,
                },
            );
            match verified {
                Ok(linked) => {
                    if lost {
                        let seq =
                            manager::reclaim_owner(&self.shared, id, record.generation).await?;
                        reclaimed.insert(id, seq);
                        tracing::info!(execution_id = %id, kind = "owner_reclaimed", "lost ownership reclaimed");
                    }
                    let running = record.cli_pid != 0;
                    self.shared.with_info(id, |info| {
                        info.attached = true;
                        info.sj_journal_id = Uuid::parse_str(&linked.attached.sj_journal_id).ok();
                        info.last_seq = linked.attached.last_sj_seq;
                        info.acked_seq = linked.attached.acked_sj_seq;
                        info.state = if running {
                            "running".into()
                        } else {
                            "starting".into()
                        };
                    });
                    self.spawn_actor(id, Start::Reattach(Box::new((linked, record))));
                    attached.push(id);
                }
                Err(_) if rebooted => {
                    tracing::info!(execution_id = %id, kind = "boot_changed", "ownership belongs to a previous boot");
                    lost::watch(&self.shared, record, Some(lost::MACHINE_REBOOTED));
                }
                Err(error) if lost => {
                    tracing::info!(execution_id = %id, reason = %error, "lost ownership not reclaimed");
                    lost::watch(&self.shared, record, None);
                }
                Err(error) => {
                    tracing::warn!(execution_id = %id, reason = %error, "ownership not verified");
                    manager::report_owner_lost(&self.shared, id, record.generation).await?;
                    lost::watch(&self.shared, record, None);
                }
            }
        }
        // Факт `owner_lost` раньше доказательства уже не описывает исполнение.
        states.retain(|(seq, state)| {
            Uuid::parse_str(&state.execution_id)
                .ok()
                .and_then(|id| reclaimed.get(&id))
                .is_none_or(|reclaimed| seq > reclaimed)
        });
        let commands = self
            .shared
            .store
            .call(|store| Ok(open(store)?.stored_commands()?))
            .await?;
        for command in commands {
            if matches!(command.kind, Some(Kind::RemoveWorktree(_))) {
                self.route(command).await?;
                continue;
            }
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
        if let Kind::RemoveWorktree(request) = kind {
            let request = request.clone();
            self.remove_worktree(command, request);
            return Ok(());
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
        let validated = match manager::validate(shared, &command, &start, conversation_id).await? {
            Ok(validated) => validated,
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
                    lost_journal: false,
                },
            );
        }
        self.spawn_actor(
            id,
            Start::Spawn(Box::new(manager::SpawnRequest {
                command,
                start,
                workdir: validated.workdir,
                source: validated.source,
                conversation_id,
            })),
        );
        Ok(())
    }

    /// Вне актора исполнения: у уборки его нет. Остановка менеджера обрывает её, команда
    /// остаётся непогашенной и доигрывается при следующем старте.
    fn remove_worktree(
        &self,
        command: Command,
        request: canterlot_proto::worker::v1::RemoveWorktree,
    ) {
        let key = command.command_id.clone();
        if !self
            .shared
            .worktree_jobs
            .lock()
            .expect("worktree jobs lock")
            .insert(key.clone())
        {
            return;
        }
        let shared = self.shared.clone();
        let mut shutdown = shared.shutdown.clone();
        let task = tokio::spawn(async move {
            let done = tokio::select! {
                result = manager::remove_worktree(&shared, &command, &request) => Some(result),
                _ = shutdown.wait_for(|stop| *stop) => None,
            };
            if let Some(Err(error)) = done {
                tracing::warn!(error_kind = ?std::mem::discriminant(&error), "worktree removal failed");
            }
            shared
                .worktree_jobs
                .lock()
                .expect("worktree jobs lock")
                .remove(&key);
        });
        self.shared.tasks.lock().expect("tasks lock").push(task);
    }

    fn spawn_actor(&self, id: Uuid, start: Start) {
        manager::spawn_actor(&self.shared, id, start);
    }
}
