use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use canterlot_proto::{
    limits::{BLOCK_FINAL_INLINE_MAX_BYTES, IPC_PROTOCOL_VERSION},
    requests::DecisionPayload,
    worker::{
        ipc::{
            Attached, CliStarted, ControlToSupervisor, DecisionAck, DeliverAck, Exited,
            MediaResult, MediaUpload, ReceivedInput, SupervisorToControl,
            control_to_supervisor as down, supervisor_to_control as up,
        },
        v1::{
            AdapterDiagnostic, BackgroundTask as ProtoBackgroundTask, BackgroundTaskKind,
            BackgroundTaskState, BackgroundWorkChanged, BlockDelta, BlockFinal, BlockIncomplete,
            BlockMedia, BlockStarted, CompactionEvent, CompactionState, CompactionStatus,
            ContextProgress, DecisionApplied, DecisionNotApplied, DecisionTransmissionStarted,
            DecisionUnconfirmed, DeliverDecision, InputAcceptanceUnconfirmed, InputAccepted,
            InputCancelLost, InputCancelWon, InputDispatchDeferred, InputRejected,
            InputTransmissionStarted, InputWithdrawn, InterruptState, InterruptStatus,
            JournalClosed, JournalOpened, PhaseChanged, RequestClosed, RequestOpened, RunFinished,
            SessionStuck, TodoItem as ProtoTodoItem, TodoStatus as ProtoTodoStatus, TodoUpdated,
            content_block, record_body::Body,
        },
    },
};
use sha2::{Digest, Sha256};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use uuid::Uuid;

use super::{
    SupervisorError, SupervisorFaults, SupervisorSpec,
    blocks::{OpenBlock, delta_chunks},
    bounds::{
        MAX_REQUEST_ID_BYTES, bounded_background, bounded_json, bounded_record, bounded_todo,
    },
    git_facts,
    ipc::{IpcConnection, IpcServer, constant_time_eq, read_message, write_message},
    sj::Sj,
};
use crate::{
    adapter::{
        Adapter, AdapterContext, AdapterEvent, AgentCallResult, BackgroundKind, BackgroundState,
        BackgroundTask, CloseMode, CompactOutcome, DecisionOutcome, InstructionSpec,
        InterruptOutcome, LimitFact, LocalFile, MediaSource, Phase, SettingsChange, StartSpec,
        SteerOutcome, TodoStatus, Transmission, TransmitOutcome, frame_names, registry_with,
    },
    containment::{Containment, ContainmentError, Member, Termination, TreeContainment},
    machine::share::{self, Prepared as PreparedShare, ShareError},
    media::{self, Prepared, Skip},
};

/// Картинки в подготовке и ожидании загрузки на одно исполнение; сверх этого событие пропускается.
const MAX_PENDING_MEDIA: usize = 32;
/// Период уборки зомби усыновлённых процессов: они занимают лимит процессов владельца.
const ORPHAN_REAP_PERIOD: Duration = Duration::from_secs(30);
/// Период снимка участников: он находит потомков, потерявших метку группы.
const GROUP_RECORD_PERIOD: Duration = Duration::from_secs(5);
/// Подготовка файла и постановка в очередь: загрузки ещё нет, срок короткий.
const SHARE_PREPARE_TIMEOUT: Duration = Duration::from_secs(20);
/// Передача файла: базовый срок и время на размер, крупный файл не обрывается.
const SHARE_TRANSFER_BASE: Duration = Duration::from_secs(60);
const SHARE_TRANSFER_PER_MIB: Duration = Duration::from_secs(2);
const SHARE_TRANSFER_MAX: Duration = Duration::from_secs(15 * 60);

fn share_transfer_timeout(size: u64) -> Duration {
    let mib = u32::try_from(size / (1024 * 1024)).unwrap_or(u32::MAX);
    SHARE_TRANSFER_BASE
        .saturating_add(SHARE_TRANSFER_PER_MIB.saturating_mul(mib))
        .min(SHARE_TRANSFER_MAX)
}

struct MediaJob {
    media_id: Uuid,
    block_id: Uuid,
    source: MediaSource,
}

/// Очередь картинок: загружается одна за раз через управляющую службу; ссылка `BlockMedia`
/// пишется в SJ только после ответа хаба о надёжной публикации.
#[derive(Default)]
struct MediaQueue {
    preparing: usize,
    ready: VecDeque<Prepared>,
    inflight: Option<Uuid>,
}

struct ShareJob {
    share_id: Uuid,
    call_id: Uuid,
    block_id: Uuid,
    path: PathBuf,
    /// Рабочий каталог сессии: вне него отдаются только файлы из временных каталогов.
    workdir: Option<PathBuf>,
    title: String,
    note: String,
    /// Ход, в котором модель вызвала `share_file`: блок файла встаёт в него же.
    turn: Option<Uuid>,
}

struct PendingShare {
    call_id: Uuid,
    block_id: Uuid,
    turn: Option<Uuid>,
    prepared: PreparedShare,
}

/// Очередь файлов владельцу: читается и загружается по одному, блок и ссылка
/// появляются только после ответа хаба о надёжной публикации.
#[derive(Default)]
struct ShareQueue {
    preparing: usize,
    ready: VecDeque<PendingShare>,
    inflight: Option<Uuid>,
}

#[allow(clippy::large_enum_variant)]
enum Control {
    Message(u64, ControlToSupervisor),
    Closed(u64),
}

struct Conn {
    tx: mpsc::Sender<SupervisorToControl>,
    attached: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputState {
    Queued,
    Transmitting,
    Done,
    Cancelled,
    Withdrawn,
}

struct Input {
    state: InputState,
    text: String,
    accepted: bool,
    files: Vec<LocalFile>,
    settings: Option<SettingsChange>,
}

#[derive(Clone)]
enum StopCause {
    User {
        force: bool,
    },
    StartCancelled,
    StartFailed,
    /// Codex оставил в своей базе незавершённый перенос sqlite: запуск без
    /// вмешательства владельца больше не пройдёт.
    CodexStateBackfillStuck,
    InvalidInstructions,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    ProtocolIncompatible,
    AuthRequired,
    CliIncompatible,
    ContextExhausted,
}

struct StopState {
    cause: StopCause,
    force: bool,
    closed: bool,
    next_round: Instant,
    survivors_reported: usize,
}

enum Lifecycle {
    Starting,
    Running,
    Stopping(StopState),
    Draining { outcome: String },
}

struct Supervisor {
    spec: SupervisorSpec,
    exec_dir: PathBuf,
    secret: Vec<u8>,
    sj: Sj,
    containment: Arc<dyn Containment>,
    faults: SupervisorFaults,
    pid: u32,
    start_ticks: u64,
    adapter: Option<Box<dyn Adapter>>,
    adapter_exited: bool,
    start_attempted: bool,
    started: Option<CliStarted>,
    ready: bool,
    lifecycle: Lifecycle,
    /// Адаптер на время `close`: цикл продолжает обслуживать поток и команды.
    closing: Option<JoinHandle<Box<dyn Adapter>>>,
    terminate: Option<JoinHandle<Result<Termination, ContainmentError>>>,
    inputs: HashMap<(Uuid, u32), Input>,
    queue: VecDeque<(Uuid, u32)>,
    transmitted: HashSet<Uuid>,
    cancel_marks: HashMap<Uuid, u32>,
    reported: HashSet<(Uuid, u32, &'static str)>,
    active_turn: Option<Uuid>,
    /// Команда, которой приписан идущий запуск без ввода.
    adopted_turn: Option<Uuid>,
    turn_started: Option<Instant>,
    pilot_v1: bool,
    todo_v1: bool,
    compact_v1: bool,
    send_now_v1: bool,
    background_v1: bool,
    session_tools_v1: bool,
    /// HEAD рабочего каталога на конец прошлого хода (или на старт CLI).
    git_head: Option<String>,
    background_drop_reported: bool,
    /// Потерю списка задач без возможности хаба показываем один раз, а не на каждое обновление.
    todo_drop_reported: bool,
    provider: String,
    open_blocks: BTreeMap<Uuid, OpenBlock>,
    known_interrupts: HashSet<(String, u32)>,
    pending_interrupts: Vec<(String, u32)>,
    known_compactions: HashSet<(String, u32)>,
    /// Сжатие принято адаптером, итог придёт событием.
    pending_compaction: Option<(String, u32)>,
    /// Открытый эпизод сжатия для durable ленты: `id` делят старт и финиш, повторный
    /// старт эпизода не дублирует, повторный финиш после закрытия ignored.
    compaction_episode: Option<(Uuid, bool)>,
    interrupted: bool,
    aborting_turn: bool,
    /// Вызовы инструментов агента в ожидании ответа хаба: срок ответа.
    agent_calls: HashMap<Uuid, Instant>,
    conns: HashMap<u64, Conn>,
    attached: Option<u64>,
    /// Неизвестное сообщение IPC уже названо в журнале: дальше молча.
    unknown_ipc_logged: bool,
    next_conn: u64,
    attach_deadline: Option<Instant>,
    workdir: Option<PathBuf>,
    media: MediaQueue,
    media_jobs: mpsc::UnboundedSender<MediaJob>,
    shares: ShareQueue,
    share_jobs: mpsc::UnboundedSender<ShareJob>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn checksum(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn snake<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn diagnostic_fact(
    execution_id: String,
    kind: &str,
    event_type: &str,
    required: bool,
    field_names: &[String],
) -> AdapterDiagnostic {
    AdapterDiagnostic {
        execution_id,
        kind: if required {
            "unknown_required_event"
        } else {
            "unknown_optional_event"
        }
        .into(),
        event_type: diagnostic_event_type(kind, event_type),
        field_names: frame_names::field_names(field_names.iter().map(String::as_str)),
    }
}

/// Настоящее имя кадра хранится только там, где адаптер кладёт его в `event_type`.
fn diagnostic_event_type(kind: &str, event_type: &str) -> String {
    safe_event_type(
        if matches!(
            kind,
            "unknown_event"
                | "provider_failure"
                | "unknown_notification"
                | "unsupported_server_request"
                | "unsupported_elicitation"
                | "unparsed_event"
        ) && !event_type.is_empty()
        {
            event_type
        } else {
            kind
        },
    )
}

fn safe_event_type(raw: &str) -> String {
    frame_names::safe_name(raw)
}

fn input_text(command: &canterlot_proto::worker::v1::Command) -> Option<String> {
    let content = match &command.kind {
        Some(canterlot_proto::worker::v1::command::Kind::DeliverInput(input)) => &input.content,
        Some(canterlot_proto::worker::v1::command::Kind::SteerInput(steer)) => &steer.content,
        _ => return None,
    };
    Some(
        content
            .iter()
            .filter_map(|block| {
                block.block.as_ref().and_then(|block| match block {
                    content_block::Block::Text(text) => Some(text.text.as_str()),
                    content_block::Block::Attachment(_) => None,
                })
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn input_settings(
    command: &canterlot_proto::worker::v1::Command,
) -> Result<Option<SettingsChange>, crate::adapter::InstructionError> {
    let settings = match &command.kind {
        Some(canterlot_proto::worker::v1::command::Kind::DeliverInput(input)) => {
            input.settings.as_ref()
        }
        Some(canterlot_proto::worker::v1::command::Kind::SteerInput(steer)) => {
            steer.settings.as_ref()
        }
        _ => None,
    };
    settings
        .map(|settings| {
            Ok::<_, crate::adapter::InstructionError>(SettingsChange {
                model: (!settings.model.is_empty()).then(|| settings.model.clone()),
                effort: (!settings.effort.is_empty()).then(|| settings.effort.clone()),
                permission_mode: (!settings.permission_mode.is_empty())
                    .then(|| settings.permission_mode.clone()),
                instructions: settings
                    .instructions
                    .as_ref()
                    .map(|preset| InstructionSpec::from_proto(Some(preset)))
                    .transpose()?,
            })
        })
        .transpose()
}

pub(super) async fn run(exec_dir: PathBuf) -> Result<ExitCode, SupervisorError> {
    let (spec, secret) = super::load(&exec_dir)?;
    match rustix::process::setsid() {
        Ok(_) | Err(rustix::io::Errno::PERM) => {}
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let containment: Arc<dyn Containment> =
        Arc::new(TreeContainment::marked(spec.execution_id.to_string())?);
    crate::containment::write_group_file(&exec_dir, &[])?;
    let sj = Sj::create(&exec_dir)?;
    let recovered = sj.last_seq() > 0;
    let lifecycle = sj
        .closed_reason()
        .map_or(Lifecycle::Starting, |outcome| Lifecycle::Draining {
            outcome,
        });
    let socket = spec
        .ipc_socket
        .clone()
        .unwrap_or_else(|| exec_dir.join("ipc.sock"));
    let server =
        IpcServer::bind_socket(&socket, secret.clone(), rustix::process::getuid().as_raw()).await?;
    let pid = std::process::id();
    let start_ticks = crate::containment::start_ticks(pid)?;
    let (media_jobs, prepared) = media_preparer(exec_dir.clone());
    let (share_jobs, share_prepared) = share_preparer(exec_dir.clone());
    let mut supervisor = Supervisor {
        attach_deadline: Some(Instant::now() + spec.timers.attach_wait),
        spec,
        exec_dir,
        secret,
        sj,
        containment: containment.clone(),
        faults: SupervisorFaults::from_env(),
        pid,
        start_ticks,
        adapter: None,
        adapter_exited: false,
        start_attempted: recovered,
        started: None,
        ready: false,
        lifecycle,
        closing: None,
        terminate: None,
        inputs: HashMap::new(),
        queue: VecDeque::new(),
        transmitted: HashSet::new(),
        cancel_marks: HashMap::new(),
        reported: HashSet::new(),
        active_turn: None,
        adopted_turn: None,
        turn_started: None,
        pilot_v1: false,
        todo_v1: false,
        compact_v1: false,
        send_now_v1: false,
        background_v1: false,
        session_tools_v1: false,
        git_head: None,
        background_drop_reported: false,
        todo_drop_reported: false,
        provider: String::new(),
        open_blocks: BTreeMap::new(),
        known_interrupts: HashSet::new(),
        pending_interrupts: Vec::new(),
        known_compactions: HashSet::new(),
        pending_compaction: None,
        compaction_episode: None,
        interrupted: false,
        aborting_turn: false,
        agent_calls: HashMap::new(),
        conns: HashMap::new(),
        attached: None,
        unknown_ipc_logged: false,
        next_conn: 0,
        workdir: None,
        media: MediaQueue::default(),
        media_jobs,
        shares: ShareQueue::default(),
        share_jobs,
    };
    if !recovered {
        supervisor.record(Body::JournalOpened(JournalOpened {
            execution_id: supervisor.spec.execution_id.to_string(),
            generation: supervisor.spec.generation,
            supervisor_protocol_version: IPC_PROTOCOL_VERSION,
        }))?;
        supervisor.state_record("starting", "", "")?;
    }
    while supervisor.faults.attach_held() {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tracing::info!(execution_id = %supervisor.spec.execution_id, journal_id = %supervisor.sj.journal_id(), "supervisor started");
    match supervisor
        .event_loop(server, prepared, share_prepared)
        .await
    {
        Ok(code) => Ok(code),
        Err(error) => {
            tracing::error!(error_kind = ?std::mem::discriminant(&error), "supervisor failed");
            let _ = containment.terminate(Duration::from_millis(200)).await;
            Err(error)
        }
    }
}

/// Подготовка картинок по одной в порядке событий: чтение источника сразу, копия, SHA-256.
fn media_preparer(
    exec_dir: PathBuf,
) -> (
    mpsc::UnboundedSender<MediaJob>,
    mpsc::UnboundedReceiver<Result<Prepared, Skip>>,
) {
    let (jobs, mut queue) = mpsc::unbounded_channel::<MediaJob>();
    let (done, prepared) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(job) = queue.recv().await {
            let dir = exec_dir.clone();
            let result = tokio::task::spawn_blocking(move || {
                media::prepare(&dir, job.media_id, job.block_id, job.source)
            })
            .await
            .unwrap_or(Err(Skip::Unreadable));
            if done.send(result).is_err() {
                break;
            }
        }
    });
    (jobs, prepared)
}

type SharePrepared = Result<(ShareJob, PreparedShare), (ShareJob, ShareError)>;

fn share_preparer(
    exec_dir: PathBuf,
) -> (
    mpsc::UnboundedSender<ShareJob>,
    mpsc::UnboundedReceiver<SharePrepared>,
) {
    let (jobs, mut queue) = mpsc::unbounded_channel::<ShareJob>();
    let (done, prepared) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(job) = queue.recv().await {
            let dir = exec_dir.clone();
            let (path, title, note) = (job.path.clone(), job.title.clone(), job.note.clone());
            let workdir = job.workdir.clone();
            let share_id = job.share_id;
            let result = tokio::task::spawn_blocking(move || {
                share::prepare(&dir, share_id, &path, title, note, workdir.as_deref())
            })
            .await
            .unwrap_or(Err(ShareError::Unreadable));
            let sent = match result {
                Ok(prepared) => done.send(Ok((job, prepared))),
                Err(error) => done.send(Err((job, error))),
            };
            if sent.is_err() {
                break;
            }
        }
    });
    (jobs, prepared)
}

/// Просмотр /proc — вне цикла супервизора.
async fn reap_orphans(containment: Arc<dyn Containment>) {
    let mut tick =
        tokio::time::interval_at(Instant::now() + ORPHAN_REAP_PERIOD, ORPHAN_REAP_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let containment = containment.clone();
        let _ = tokio::task::spawn_blocking(move || containment.reap_orphans()).await;
    }
}

async fn record_group(containment: Arc<dyn Containment>, exec_dir: PathBuf) {
    let mut tick =
        tokio::time::interval_at(Instant::now() + GROUP_RECORD_PERIOD, GROUP_RECORD_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let (containment, dir) = (containment.clone(), exec_dir.clone());
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(members) = containment.observed() {
                let _ = crate::containment::write_group_file(&dir, &members);
            }
        })
        .await;
    }
}

async fn adapter_event(adapter: &mut Option<Box<dyn Adapter>>, gone: bool) -> AdapterEvent {
    match adapter {
        Some(adapter) if !gone => adapter.next_event().await,
        _ => std::future::pending().await,
    }
}

async fn ready_adapter_event(adapter: &mut Option<Box<dyn Adapter>>) -> Option<AdapterEvent> {
    let adapter = adapter.as_mut()?;
    let mut event = adapter.next_event();
    std::future::poll_fn(|cx| {
        std::task::Poll::Ready(match event.as_mut().poll(cx) {
            std::task::Poll::Ready(event) => Some(event),
            std::task::Poll::Pending => None,
        })
    })
    .await
}

async fn close_result(
    handle: &mut Option<JoinHandle<Box<dyn Adapter>>>,
) -> Result<Box<dyn Adapter>, tokio::task::JoinError> {
    match handle {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

async fn terminate_result(
    handle: &mut Option<JoinHandle<Result<Termination, ContainmentError>>>,
) -> Result<Result<Termination, ContainmentError>, tokio::task::JoinError> {
    match handle {
        Some(handle) => handle.await,
        None => std::future::pending().await,
    }
}

impl Supervisor {
    fn record(&mut self, body: Body) -> Result<u64, SupervisorError> {
        self.sj.append(bounded_record(self.spec.execution_id, body))
    }

    /// Отказ запуска до адаптера: строка журнала процесса и диагностика хабу, чтобы
    /// отказ видел владелец, а не только удалённый `supervisor.log` исполнения.
    /// Причина — короткий код, без секретов и путей.
    fn fail_start(&mut self, reason: &'static str) -> Result<(), SupervisorError> {
        tracing::warn!(kind = "start_failed", reason, "adapter start failed");
        self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
            execution_id: self.spec.execution_id.to_string(),
            kind: "start_failed".into(),
            event_type: safe_event_type(reason),
            ..Default::default()
        }))?;
        Ok(())
    }

    fn state_record(
        &mut self,
        state: &str,
        outcome: &str,
        native: &str,
    ) -> Result<(), SupervisorError> {
        self.record(Body::ExecutionState(
            canterlot_proto::worker::v1::ExecutionState {
                execution_id: self.spec.execution_id.to_string(),
                generation: self.spec.generation,
                state: state.into(),
                outcome: outcome.into(),
                native_conversation_id: native.into(),
            },
        ))?;
        Ok(())
    }

    async fn event_loop(
        &mut self,
        mut server: IpcServer,
        mut prepared: mpsc::UnboundedReceiver<Result<Prepared, Skip>>,
        mut share_prepared: mpsc::UnboundedReceiver<
            Result<(ShareJob, PreparedShare), (ShareJob, ShareError)>,
        >,
    ) -> Result<ExitCode, SupervisorError> {
        let (accepted_tx, mut accepted) = mpsc::channel::<IpcConnection>(8);
        let accept_task = tokio::spawn(async move {
            let period = Duration::from_secs(5);
            let mut socket_check = tokio::time::interval_at(Instant::now() + period, period);
            socket_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    connection = server.accept() => {
                        let Ok(connection) = connection else { break };
                        tracing::debug!("control connection accepted");
                        if accepted_tx.send(connection).await.is_err() { break; }
                    }
                    _ = socket_check.tick() => {
                        if server.restore_socket().await.is_err() {
                            tracing::warn!(kind = "ipc_restore_failed", "ipc socket restore failed");
                        }
                    }
                }
            }
        });
        let reaper = tokio::spawn(reap_orphans(self.containment.clone()));
        let recorder = tokio::spawn(record_group(
            self.containment.clone(),
            self.exec_dir.clone(),
        ));
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Control>();
        loop {
            if let Some(code) = self.after_event().await? {
                accept_task.abort();
                reaper.abort();
                recorder.abort();
                return Ok(code);
            }
            let wake = self.next_wake();
            tokio::select! {
                Some(connection) = accepted.recv() => self.on_connection(connection, &control_tx),
                Some(control) = control_rx.recv() => self.on_control(control).await?,
                Some(result) = prepared.recv() => self.on_media_prepared(result),
                Some(result) = share_prepared.recv() => self.on_share_prepared(result).await?,
                event = adapter_event(&mut self.adapter, self.adapter_exited || matches!(self.lifecycle, Lifecycle::Draining { .. })) => {
                    self.on_adapter_event(event).await?;
                    for _ in 0..63 {
                        if self.adapter_exited || matches!(self.lifecycle, Lifecycle::Draining { .. }) { break; }
                        let Some(event) = ready_adapter_event(&mut self.adapter).await else { break; };
                        self.on_adapter_event(event).await?;
                    }
                },
                result = close_result(&mut self.closing) => {
                    self.closing = None;
                    self.adapter = result.ok();
                }
                result = terminate_result(&mut self.terminate) => self.on_terminate(result).await?,
                () = async { match wake { Some(at) => sleep_until(at).await, None => std::future::pending().await } } => {
                    self.attach_deadline = None;
                }
            }
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        let mut consider = |at: Instant| wake = Some(wake.map_or(at, |current| current.min(at)));
        match &self.lifecycle {
            Lifecycle::Stopping(state) if self.terminate.is_none() && self.closing.is_none() => {
                consider(state.next_round)
            }
            _ => {}
        }
        if !self.queue.is_empty() && self.faults.gate_held(&self.exec_dir) {
            consider(Instant::now() + Duration::from_millis(25));
        }
        if let Some(at) = self.attach_deadline.filter(|_| self.attached.is_none()) {
            consider(at);
        }
        if let Some(earliest) = self.agent_calls.values().min() {
            consider(*earliest);
        }
        wake
    }

    async fn after_event(&mut self) -> Result<Option<ExitCode>, SupervisorError> {
        self.sj.flush()?;
        self.drive_stop().await?;
        self.try_transmit().await?;
        self.expire_agent_calls().await;
        self.sj.flush()?;
        self.pump_shipping();
        if let Lifecycle::Draining { outcome } = &self.lifecycle
            && self.sj.fully_acked()
        {
            let reason = outcome.clone();
            self.send(up::Msg::Exited(Exited { reason }));
            self.conns.clear();
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Ok(Some(ExitCode::SUCCESS));
        }
        Ok(None)
    }

    fn on_connection(
        &mut self,
        connection: IpcConnection,
        control: &mpsc::UnboundedSender<Control>,
    ) {
        let id = self.next_conn;
        self.next_conn += 1;
        let (mut read, mut write) = connection.into_split();
        let (tx, mut rx) = mpsc::channel::<SupervisorToControl>(64);
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                if write_message(&mut write, &message).await.is_err() {
                    break;
                }
            }
        });
        let control = control.clone();
        tokio::spawn(async move {
            loop {
                match read_message::<ControlToSupervisor, _>(&mut read).await {
                    Ok(message) => {
                        if control.send(Control::Message(id, message)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = control.send(Control::Closed(id));
                        break;
                    }
                }
            }
        });
        self.conns.insert(
            id,
            Conn {
                tx,
                attached: false,
            },
        );
    }

    fn send(&mut self, message: up::Msg) {
        let Some(id) = self.attached else { return };
        let delivered = self.conns.get(&id).is_some_and(|conn| {
            conn.tx
                .try_send(SupervisorToControl { msg: Some(message) })
                .is_ok()
        });
        if !delivered {
            self.drop_connection(id);
        }
    }

    fn drop_connection(&mut self, id: u64) {
        self.conns.remove(&id);
        if self.attached == Some(id) {
            self.attached = None;
            self.media.inflight = None;
            self.shares.inflight = None;
            self.sj.disable_ship();
        }
    }

    fn pump_shipping(&mut self) {
        while self.attached.is_some() {
            match self.sj.next_batch() {
                Ok(Some(batch)) => self.send(up::Msg::SjBatch(batch)),
                Ok(None) => break,
                Err(_) => break,
            }
        }
    }

    async fn on_control(&mut self, control: Control) -> Result<(), SupervisorError> {
        let (id, message) = match control {
            Control::Closed(id) => {
                self.drop_connection(id);
                return Ok(());
            }
            Control::Message(id, message) => (id, message),
        };
        let Some(conn) = self.conns.get(&id) else {
            return Ok(());
        };
        let Some(message) = message.msg else {
            // Новая управляющая служба может прислать то, чего эта сборка не знает:
            // связь остаётся, иначе она рвалась бы на каждом таком сообщении.
            if conn.attached {
                if !std::mem::replace(&mut self.unknown_ipc_logged, true) {
                    tracing::warn!(
                        kind = "ipc_unknown_message",
                        "unknown control message ignored"
                    );
                }
                return Ok(());
            }
            self.drop_connection(id);
            return Ok(());
        };
        if !conn.attached {
            let down::Msg::Attach(attach) = message else {
                self.drop_connection(id);
                return Ok(());
            };
            let valid = attach.protocol_version == IPC_PROTOCOL_VERSION
                && constant_time_eq(&self.secret, &attach.secret)
                && attach.execution_id == self.spec.execution_id.to_string()
                && attach.generation == self.spec.generation;
            if !valid {
                tracing::warn!(kind = "attach_rejected", "attach rejected");
                self.drop_connection(id);
                return Ok(());
            }
            if let Some(previous) = self.attached.take() {
                self.conns.remove(&previous);
            }
            self.sj.disable_ship();
            if let Some(conn) = self.conns.get_mut(&id) {
                conn.attached = true;
            }
            self.attached = Some(id);
            if !matches!(self.lifecycle, Lifecycle::Draining { .. }) {
                for request in self.sj.open_requests() {
                    self.record(Body::RequestOpened(request))?;
                }
                self.sj.flush()?;
            }
            let received = self.received_table();
            self.send(up::Msg::Attached(Attached {
                protocol_version: IPC_PROTOCOL_VERSION,
                pid: self.pid,
                start_ticks: self.start_ticks,
                sj_journal_id: self.sj.journal_id().to_string(),
                last_sj_seq: self.sj.last_seq(),
                acked_sj_seq: self.sj.acked_seq(),
                received,
                ipc_capabilities: canterlot_proto::limits::IPC_CAPABILITIES
                    .iter()
                    .map(|capability| (*capability).to_owned())
                    .collect(),
            }));
            tracing::info!(kind = "attached", "control attached");
            // Новая управляющая служба получает незавершённую загрузку заново.
            self.media.inflight = None;
            self.pump_media();
            self.shares.inflight = None;
            self.pump_share();
            return Ok(());
        }
        if matches!(self.lifecycle, Lifecycle::Draining { .. }) {
            match message {
                down::Msg::Cancel(cancel) => {
                    self.closed_input_reply(
                        &cancel.target_command_id,
                        cancel.target_dispatch_attempt,
                        "cancel_won",
                    );
                    return Ok(());
                }
                down::Msg::Withdraw(withdraw) => {
                    self.closed_input_reply(
                        &withdraw.target_command_id,
                        withdraw.target_dispatch_attempt,
                        "input_withdrawn",
                    );
                    return Ok(());
                }
                down::Msg::Deliver(_)
                | down::Msg::ShipFrom(_)
                | down::Msg::SjAck(_)
                | down::Msg::DeliverDecision(_)
                | down::Msg::Attach(_) => {}
                _ => return Ok(()),
            }
        }
        match message {
            down::Msg::Attach(_) => self.drop_connection(id),
            down::Msg::StartCli(start) => self.start_cli(start).await?,
            down::Msg::Deliver(deliver) => {
                if matches!(
                    &deliver.command,
                    Some(command)
                        if matches!(
                            &command.kind,
                            Some(canterlot_proto::worker::v1::command::Kind::SteerInput(_))
                        )
                ) {
                    self.on_steer(deliver).await?
                } else {
                    self.on_deliver(deliver)
                }
            }
            down::Msg::Cancel(cancel) => {
                self.on_cancel(&cancel.target_command_id, cancel.target_dispatch_attempt)?
            }
            down::Msg::Withdraw(withdraw) => self.on_withdraw(
                &withdraw.target_command_id,
                withdraw.target_dispatch_attempt,
            )?,
            down::Msg::Interrupt(interrupt) => {
                self.on_interrupt(interrupt.command_id, interrupt.dispatch_attempt)
                    .await?
            }
            down::Msg::Compact(compact) => {
                self.on_compact(compact.command_id, compact.dispatch_attempt)
                    .await?
            }
            down::Msg::Stop(stop) => self.on_stop(stop.force).await?,
            down::Msg::ShipFrom(ship) => self.sj.enable_ship(ship.committed_seq),
            down::Msg::SjAck(ack) => self.sj.ack(ack.up_to_seq)?,
            down::Msg::MediaResult(result) => self.on_media_result(result)?,
            down::Msg::ShareResult(result) => self.on_share_result(result).await?,
            down::Msg::DeliverDecision(decision) => self.on_decision(decision).await?,
            down::Msg::AgentCallResult(result) => self.on_agent_call_result(result).await,
        }
        Ok(())
    }

    async fn on_agent_call_result(
        &mut self,
        result: canterlot_proto::worker::ipc::AgentCallResult,
    ) {
        let Ok(call_id) = Uuid::parse_str(&result.call_id) else {
            return;
        };
        if self.agent_calls.remove(&call_id).is_none() {
            return;
        }
        let Some(adapter) = self.adapter.as_mut() else {
            return;
        };
        let outcome = if result.error_code.is_empty() {
            match serde_json::from_str(&result.result_json) {
                Ok(value) => AgentCallResult::Ok(value),
                Err(_) => AgentCallResult::Err {
                    code: "hub_error".into(),
                    message: "некорректный ответ хаба".into(),
                },
            }
        } else {
            AgentCallResult::Err {
                code: result.error_code,
                message: result.error_message,
            }
        };
        adapter.agent_call_result(call_id, outcome).await;
    }

    /// Просроченные вызовы получают `hub_timeout`; вызывается после каждого события.
    async fn expire_agent_calls(&mut self) {
        let now = Instant::now();
        let expired: Vec<Uuid> = self
            .agent_calls
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| *id)
            .collect();
        for call_id in expired {
            self.agent_calls.remove(&call_id);
            // Просроченная отправка файла: убрать из очереди и стереть копию.
            if let Some(position) = self
                .shares
                .ready
                .iter()
                .position(|pending| pending.call_id == call_id)
            {
                let pending = self.shares.ready.remove(position).expect("found share");
                if self.shares.inflight == Some(pending.prepared.id) {
                    self.shares.inflight = None;
                    // Загрузка на управляющей службе больше не нужна: её задача снимается.
                    self.send(up::Msg::ShareCancel(
                        canterlot_proto::worker::ipc::ShareCancel {
                            share_id: pending.prepared.id.to_string(),
                        },
                    ));
                }
                let _ =
                    std::fs::remove_file(media::media_path(&self.exec_dir, pending.prepared.id));
                self.pump_share();
            }
            if let Some(adapter) = self.adapter.as_mut() {
                tracing::info!(
                    kind = "agent_call",
                    call_id = %call_id,
                    outcome = "hub_timeout",
                    "agent call timed out"
                );
                adapter
                    .agent_call_result(
                        call_id,
                        AgentCallResult::Err {
                            code: "hub_timeout".into(),
                            message: "хаб не ответил вовремя".into(),
                        },
                    )
                    .await;
            }
        }
    }

    /// При остановке все ожидающие вызовы получают `supervisor_stopping`.
    async fn fail_agent_calls(&mut self) {
        for call_id in std::mem::take(&mut self.agent_calls).keys() {
            if let Some(adapter) = self.adapter.as_mut() {
                adapter
                    .agent_call_result(
                        *call_id,
                        AgentCallResult::Err {
                            code: "supervisor_stopping".into(),
                            message: "сопровождение останавливается".into(),
                        },
                    )
                    .await;
            }
        }
    }

    fn decision_result(&mut self, body: Body) -> Result<(), SupervisorError> {
        use canterlot_proto::worker::ipc::{
            DecisionOutcome as IpcOutcome, decision_outcome::Outcome,
        };
        let outcome = match &body {
            Body::DecisionApplied(f) => Outcome::Applied(f.clone()),
            Body::DecisionNotApplied(f) => Outcome::NotApplied(f.clone()),
            Body::DecisionUnconfirmed(f) => Outcome::Unconfirmed(f.clone()),
            _ => return Ok(()),
        };
        if !matches!(self.lifecycle, Lifecycle::Draining { .. }) {
            self.record(body)?;
            self.sj.flush()?;
        }
        self.send(up::Msg::DecisionOutcome(IpcOutcome {
            outcome: Some(outcome),
        }));
        Ok(())
    }

    async fn on_decision(&mut self, cmd: DeliverDecision) -> Result<(), SupervisorError> {
        let rejected = |reason: &str| {
            Body::DecisionNotApplied(DecisionNotApplied {
                request_id: cmd.request_id.clone(),
                decision_id: cmd.decision_id.clone(),
                reason: reason.into(),
            })
        };
        if cmd.generation != self.spec.generation {
            return self.decision_result(rejected("stale_generation"));
        }
        if matches!(self.lifecycle, Lifecycle::Draining { .. }) {
            self.send(up::Msg::DecisionAck(DecisionAck {
                request_id: cmd.request_id.clone(),
                decision_id: cmd.decision_id.clone(),
                result: "execution_closed".into(),
            }));
            return self.decision_result(Body::DecisionNotApplied(DecisionNotApplied {
                request_id: cmd.request_id,
                decision_id: cmd.decision_id,
                reason: "request_closed".into(),
            }));
        }
        let Some(request) = self.sj.request(&cmd.request_id) else {
            return self.decision_result(rejected("request_closed"));
        };
        if request.opened.generation != self.spec.generation
            || request.opened.execution_id != self.spec.execution_id.to_string()
        {
            return self.decision_result(rejected("stale_generation"));
        }
        if request.transmission.is_some() {
            return self.decision_result(rejected("already_transmitted"));
        }
        if request.terminal
            || self.aborting_turn
            || !matches!(self.lifecycle, Lifecycle::Running)
            || self.adapter_exited
            || self.active_turn.is_none()
        {
            return self.decision_result(rejected("request_closed"));
        }
        let decision = serde_json::from_slice::<DecisionPayload>(&cmd.decision_payload_json);
        let payload = serde_json::from_slice::<serde_json::Value>(&request.opened.payload_json);
        let (Ok(decision), Ok(payload)) = (decision, payload) else {
            return self.decision_result(rejected("invalid_decision"));
        };
        if cmd.kind != request.opened.kind
            || cmd.decision_id.is_empty()
            || !crate::adapter::valid_decision(&cmd.kind, &payload, &decision)
        {
            return self.decision_result(rejected("invalid_decision"));
        }
        self.sj.intent(Body::DecisionTransmissionStarted(
            DecisionTransmissionStarted {
                request_id: cmd.request_id.clone(),
                decision_id: cmd.decision_id.clone(),
                machine_time_ms: now_ms(),
            },
        ))?;
        self.faults.abort_at("after_decision_intention");
        while self.faults.decision_held() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.send(up::Msg::DecisionAck(DecisionAck {
            request_id: cmd.request_id.clone(),
            decision_id: cmd.decision_id.clone(),
            result: "transmitting".into(),
        }));
        let outcome = match self.adapter.as_mut() {
            Some(adapter) => adapter.deliver_decision(&cmd.request_id, &decision).await,
            None => DecisionOutcome::Undetermined,
        };
        self.faults.abort_at("after_decision_adapter_before_record");
        let body = match outcome {
            DecisionOutcome::Applied(proof) => Body::DecisionApplied(DecisionApplied {
                request_id: cmd.request_id,
                decision_id: cmd.decision_id,
                proof_kind: proof.proof_kind,
                proof_payload_json: serde_json::to_vec(&proof.payload)
                    .map_err(|_| SupervisorError::Spec)?,
            }),
            DecisionOutcome::NotApplied(reason) => Body::DecisionNotApplied(DecisionNotApplied {
                request_id: cmd.request_id,
                decision_id: cmd.decision_id,
                reason,
            }),
            DecisionOutcome::Undetermined => Body::DecisionUnconfirmed(DecisionUnconfirmed {
                request_id: cmd.request_id,
                decision_id: cmd.decision_id,
                reason: "undetermined".into(),
            }),
        };
        self.decision_result(body)
    }

    fn close_requests(&mut self, reason: &str) -> Result<(), SupervisorError> {
        for request in self.sj.open_requests() {
            self.record(Body::RequestClosed(RequestClosed {
                request_id: request.request_id,
                reason: reason.into(),
            }))?;
        }
        Ok(())
    }

    fn on_todo(
        &mut self,
        lane: Option<Uuid>,
        items: Vec<crate::adapter::TodoItem>,
    ) -> Result<(), SupervisorError> {
        if !self.todo_v1 {
            if !std::mem::replace(&mut self.todo_drop_reported, true) {
                self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                    execution_id: self.spec.execution_id.to_string(),
                    kind: "capability_missing".into(),
                    event_type: "todo_dropped".into(),
                    ..Default::default()
                }))?;
            }
            return Ok(());
        }
        let items = items
            .into_iter()
            .map(|item| ProtoTodoItem {
                text: item.text,
                status: match item.status {
                    TodoStatus::Pending => ProtoTodoStatus::Pending,
                    TodoStatus::InProgress => ProtoTodoStatus::InProgress,
                    TodoStatus::Completed => ProtoTodoStatus::Completed,
                    TodoStatus::Cancelled => ProtoTodoStatus::Cancelled,
                } as i32,
            })
            .collect();
        let (items, omitted) = bounded_todo(items);
        self.record(Body::TodoUpdated(TodoUpdated {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            revision: self.sj.next_seq(),
            lane_block_id: lane.map(|id| id.to_string()).unwrap_or_default(),
            items,
            omitted,
        }))?;
        Ok(())
    }

    fn on_background(
        &mut self,
        tasks: Vec<BackgroundTask>,
        wake_turn: bool,
    ) -> Result<(), SupervisorError> {
        if !self.background_v1 {
            if !std::mem::replace(&mut self.background_drop_reported, true) {
                self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                    execution_id: self.spec.execution_id.to_string(),
                    kind: "capability_missing".into(),
                    event_type: "background_dropped".into(),
                    ..Default::default()
                }))?;
            }
            return Ok(());
        }
        let tasks = tasks
            .into_iter()
            .map(|task| ProtoBackgroundTask {
                task_id: task.task_id,
                kind: match task.kind {
                    BackgroundKind::Agent => BackgroundTaskKind::Agent,
                    BackgroundKind::Bash => BackgroundTaskKind::Bash,
                } as i32,
                block_id: task.block_id.map(|id| id.to_string()).unwrap_or_default(),
                parent_task_id: task.parent_task_id.unwrap_or_default(),
                description: task.description,
                state: match task.state {
                    BackgroundState::Running => BackgroundTaskState::Running,
                    BackgroundState::Completed => BackgroundTaskState::Completed,
                    BackgroundState::Failed => BackgroundTaskState::Failed,
                    BackgroundState::Stopped => BackgroundTaskState::Stopped,
                } as i32,
            })
            .collect();
        let (tasks, omitted) = bounded_background(tasks);
        self.record(Body::BackgroundWorkChanged(BackgroundWorkChanged {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            revision: self.sj.next_seq(),
            machine_time_ms: now_ms(),
            tasks,
            omitted,
            wake_turn,
        }))?;
        Ok(())
    }

    fn on_media(&mut self, block_id: Uuid, source: MediaSource) -> Result<(), SupervisorError> {
        if !self.pilot_v1 {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "capability_missing".into(),
                event_type: "media_dropped".into(),
                ..Default::default()
            }))?;
            return Ok(());
        }
        if self.media.preparing + self.media.ready.len() >= MAX_PENDING_MEDIA {
            tracing::warn!(kind = "media_skipped", reason = "backlog", "media skipped");
            return Ok(());
        }
        let source = match source {
            MediaSource::Path(path) if path.is_relative() => match &self.workdir {
                Some(workdir) => MediaSource::Path(workdir.join(path)),
                None => return Ok(()),
            },
            other => other,
        };
        let job = MediaJob {
            media_id: Uuid::now_v7(),
            block_id,
            source,
        };
        if self.media_jobs.send(job).is_ok() {
            self.media.preparing += 1;
        }
        Ok(())
    }

    fn on_media_prepared(&mut self, result: Result<Prepared, Skip>) {
        self.media.preparing = self.media.preparing.saturating_sub(1);
        match result {
            Ok(prepared) if !matches!(self.lifecycle, Lifecycle::Draining { .. }) => {
                self.media.ready.push_back(prepared);
            }
            Ok(prepared) => {
                let _ = std::fs::remove_file(media::media_path(&self.exec_dir, prepared.media_id));
            }
            Err(skip) => {
                tracing::info!(
                    kind = "media_skipped",
                    reason = skip.reason(),
                    "media skipped"
                );
            }
        }
        self.pump_media();
    }

    fn pump_media(&mut self) {
        if self.media.inflight.is_some()
            || self.attached.is_none()
            || matches!(self.lifecycle, Lifecycle::Draining { .. })
        {
            return;
        }
        let Some(next) = self.media.ready.front() else {
            return;
        };
        let request = MediaUpload {
            media_id: next.media_id.to_string(),
            block_id: next.block_id.to_string(),
            mime: next.mime.into(),
            size: next.size,
            sha256: next.sha256.clone(),
        };
        self.media.inflight = Some(next.media_id);
        self.send(up::Msg::MediaUpload(request));
    }

    fn on_media_result(&mut self, result: MediaResult) -> Result<(), SupervisorError> {
        let Ok(id) = Uuid::parse_str(&result.media_id) else {
            return Ok(());
        };
        if self.media.inflight != Some(id)
            || self.media.ready.front().map(|p| p.media_id) != Some(id)
        {
            return Ok(());
        }
        self.media.inflight = None;
        let prepared = self.media.ready.pop_front().expect("front media");
        let _ = std::fs::remove_file(media::media_path(&self.exec_dir, id));
        if result.stored {
            self.record(Body::BlockMedia(BlockMedia {
                block_id: prepared.block_id.to_string(),
                media_id: id.to_string(),
                mime: prepared.mime.into(),
                size: prepared.size,
                sha256: prepared.sha256,
                width: prepared.width,
                height: prepared.height,
            }))?;
        } else {
            tracing::warn!(kind = "media_unavailable", reason = %safe_event_type(&result.reason), "media upload failed");
        }
        self.pump_media();
        Ok(())
    }

    async fn on_share(
        &mut self,
        call_id: Uuid,
        arguments: &serde_json::Value,
    ) -> Result<(), SupervisorError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            path: String,
            title: Option<String>,
            note: Option<String>,
        }
        let args: Args = match serde_json::from_value(arguments.clone()) {
            Ok(args) => args,
            Err(_) => {
                self.answer_share(
                    call_id,
                    crate::adapter::AgentCallResult::Err {
                        code: "invalid_arguments".into(),
                        message: "invalid share_file arguments".into(),
                    },
                )
                .await;
                return Ok(());
            }
        };
        if args.path.is_empty() || args.path.len() > 2048 || args.path.contains('\0') {
            self.answer_share(
                call_id,
                crate::adapter::AgentCallResult::Err {
                    code: "invalid_arguments".into(),
                    message: "invalid path".into(),
                },
            )
            .await;
            return Ok(());
        }
        let path = PathBuf::from(&args.path);
        let path = if path.is_absolute() {
            path
        } else {
            match self.workdir.clone() {
                Some(workdir) => workdir.join(path),
                None => {
                    self.answer_share(
                        call_id,
                        crate::adapter::AgentCallResult::Err {
                            code: "share_unreadable".into(),
                            message: "the workdir is not known".into(),
                        },
                    )
                    .await;
                    return Ok(());
                }
            }
        };
        if !self.pilot_v1 {
            self.answer_share(
                call_id,
                crate::adapter::AgentCallResult::Err {
                    code: "capability_missing".into(),
                    message: "the hub does not support files yet".into(),
                },
            )
            .await;
            return Ok(());
        }
        if self.shares.preparing + self.shares.ready.len() >= 4 {
            self.answer_share(
                call_id,
                crate::adapter::AgentCallResult::Err {
                    code: "share_busy".into(),
                    message: "too many files are being shared, try again".into(),
                },
            )
            .await;
            return Ok(());
        }
        let job = ShareJob {
            share_id: Uuid::now_v7(),
            call_id,
            block_id: Uuid::now_v7(),
            path,
            workdir: self.workdir.clone(),
            title: args.title.unwrap_or_default(),
            note: args.note.unwrap_or_default(),
            turn: self.active_turn,
        };
        // Подготовка и постановка в очередь ограничены коротко; передача получит
        // свой срок от размера, когда файл действительно пойдёт на хаб.
        self.agent_calls
            .insert(call_id, Instant::now() + SHARE_PREPARE_TIMEOUT);
        if self.share_jobs.send(job).is_ok() {
            self.shares.preparing += 1;
        }
        Ok(())
    }

    async fn on_share_prepared(&mut self, result: SharePrepared) -> Result<(), SupervisorError> {
        self.shares.preparing = self.shares.preparing.saturating_sub(1);
        match result {
            Ok((job, prepared)) if !matches!(self.lifecycle, Lifecycle::Draining { .. }) => {
                if self.agent_calls.contains_key(&job.call_id) {
                    self.shares.ready.push_back(PendingShare {
                        call_id: job.call_id,
                        block_id: job.block_id,
                        turn: job.turn,
                        prepared,
                    });
                } else {
                    // Срок вызова вышел раньше подготовки: ответ уже ушёл, копия не нужна.
                    let _ = std::fs::remove_file(media::media_path(&self.exec_dir, prepared.id));
                }
            }
            Ok((job, prepared)) => {
                let _ = std::fs::remove_file(media::media_path(&self.exec_dir, prepared.id));
                self.answer_share(
                    job.call_id,
                    crate::adapter::AgentCallResult::Err {
                        code: "share_unavailable".into(),
                        message: "the session is stopping".into(),
                    },
                )
                .await;
            }
            Err((job, error)) => {
                self.answer_share(
                    job.call_id,
                    crate::adapter::AgentCallResult::Err {
                        code: error.reason().into(),
                        message: error.message().into(),
                    },
                )
                .await;
            }
        }
        self.pump_share();
        Ok(())
    }

    fn pump_share(&mut self) {
        if self.shares.inflight.is_some()
            || self.attached.is_none()
            || matches!(self.lifecycle, Lifecycle::Draining { .. })
        {
            return;
        }
        let Some(next) = self.shares.ready.front() else {
            return;
        };
        let (share_id, call_id, size) = (next.prepared.id, next.call_id, next.prepared.size);
        let request = canterlot_proto::worker::ipc::ShareUpload {
            share_id: share_id.to_string(),
            call_id: call_id.to_string(),
            block_id: next.block_id.to_string(),
            name: next.prepared.name.clone(),
            title: next.prepared.title.clone(),
            note: next.prepared.note.clone(),
            mime: next.prepared.mime.clone(),
            size: next.prepared.size,
            sha256: next.prepared.sha256.clone(),
            width: next.prepared.width.unwrap_or_default(),
            height: next.prepared.height.unwrap_or_default(),
        };
        self.shares.inflight = Some(share_id);
        // Передача крупного файла получает срок от его размера, а не короткий срок подготовки.
        if let Some(deadline) = self.agent_calls.get_mut(&call_id) {
            *deadline = Instant::now() + share_transfer_timeout(size);
        }
        self.send(up::Msg::ShareUpload(request));
    }

    async fn on_share_result(
        &mut self,
        result: canterlot_proto::worker::ipc::ShareResult,
    ) -> Result<(), SupervisorError> {
        let Ok(id) = Uuid::parse_str(&result.share_id) else {
            return Ok(());
        };
        if self.shares.inflight != Some(id)
            || self.shares.ready.front().map(|p| p.prepared.id) != Some(id)
        {
            return Ok(());
        }
        self.shares.inflight = None;
        let pending = self.shares.ready.pop_front().expect("front share");
        let _ = std::fs::remove_file(media::media_path(&self.exec_dir, id));
        if result.stored {
            // Блок-носитель вложения стоит в том же ходе, что и вызов модели (ход
            // запомнен при вызове: загрузка может закончиться и после хода): лента
            // держит его в карточке хода, а не открывает новую. Метка агента — та же,
            // что у основного агента в адаптерах.
            self.record(Body::BlockStarted(BlockStarted {
                block_id: pending.block_id.to_string(),
                rev: 1,
                kind: "tool_call".into(),
                input_command_id: pending.turn.map(|id| id.to_string()).unwrap_or_default(),
                tool_name: "share_file".into(),
                summary: if pending.prepared.title.is_empty() {
                    pending.prepared.name.clone()
                } else {
                    pending.prepared.title.clone()
                },
                model: String::new(),
                agent: "orchestrator".into(),
                ..Default::default()
            }))?;
            self.record(Body::BlockFinal(BlockFinal {
                block_id: pending.block_id.to_string(),
                rev: 2,
                text: pending.prepared.note.clone(),
                ..Default::default()
            }))?;
            self.record(Body::BlockFile(canterlot_proto::worker::v1::BlockFile {
                block_id: pending.block_id.to_string(),
                attachment_id: id.to_string(),
                name: pending.prepared.name.clone(),
                mime: pending.prepared.mime.clone(),
                size: pending.prepared.size,
                sha256: pending.prepared.sha256.clone(),
                title: pending.prepared.title.clone(),
                note: pending.prepared.note.clone(),
                width: pending.prepared.width.unwrap_or_default(),
                height: pending.prepared.height.unwrap_or_default(),
            }))?;
            let kind = if pending.prepared.mime.starts_with("image/") {
                "image"
            } else if pending.prepared.mime.starts_with("video/") {
                "video"
            } else {
                "file"
            };
            self.answer_share(
                pending.call_id,
                crate::adapter::AgentCallResult::Ok(serde_json::json!({
                    "status": "shared",
                    "attachment_id": id,
                    "name": pending.prepared.name,
                    "mime": pending.prepared.mime,
                    "size": pending.prepared.size,
                    "kind": kind,
                    "title": pending.prepared.title,
                    "note": pending.prepared.note,
                    "hint": "The owner sees this file in this turn: images and videos open on tap, other files download.",
                })),
            )
            .await;
        } else {
            let (code, message) = share_refusal(&result.reason);
            self.answer_share(
                pending.call_id,
                crate::adapter::AgentCallResult::Err {
                    code: code.into(),
                    message,
                },
            )
            .await;
        }
        self.pump_share();
        Ok(())
    }

    async fn answer_share(&mut self, call_id: Uuid, result: crate::adapter::AgentCallResult) {
        self.agent_calls.remove(&call_id);
        if let Some(adapter) = self.adapter.as_mut() {
            adapter.agent_call_result(call_id, result).await;
        }
    }

    fn received_table(&self) -> Vec<ReceivedInput> {
        let mut table: Vec<_> = self
            .inputs
            .iter()
            .map(|((command, attempt), input)| ReceivedInput {
                command_id: command.to_string(),
                dispatch_attempt: *attempt,
                state: match input.state {
                    InputState::Queued => "received",
                    InputState::Transmitting => "transmitting",
                    InputState::Done => "done",
                    InputState::Cancelled => "cancelled",
                    InputState::Withdrawn => "withdrawn",
                }
                .into(),
            })
            .collect();
        table.sort_by(|a, b| {
            (&a.command_id, a.dispatch_attempt).cmp(&(&b.command_id, b.dispatch_attempt))
        });
        table
    }

    async fn start_cli(
        &mut self,
        start: canterlot_proto::worker::ipc::StartCli,
    ) -> Result<(), SupervisorError> {
        if self.start_attempted {
            if let Some(started) = self.started.clone() {
                self.send(up::Msg::CliStarted(started));
            }
            return Ok(());
        }
        if !matches!(self.lifecycle, Lifecycle::Starting) {
            return Ok(());
        }
        self.start_attempted = true;
        let timers = self.spec.timers.clone();
        let context = AdapterContext {
            transmit_deadline: timers.transmit_deadline,
            ready_deadline: timers.ready_deadline,
            exec_dir: self.exec_dir.clone(),
            claude_code: self.spec.claude_code.clone(),
            codex: self.spec.codex.clone(),
            opencode: self.spec.opencode.clone(),
            antigravity: self.spec.antigravity.clone(),
        };
        let Some(mut adapter) = registry_with(&start.profile, &context) else {
            self.fail_start("unknown_profile")?;
            return self.begin_stop(StopCause::StartFailed, false);
        };
        if start.isolate_mcp && !adapter.isolates_mcp() {
            self.fail_start("mcp_isolation_unsupported")?;
            return self.begin_stop(StopCause::StartFailed, false);
        }
        adapter.set_stop_grace(timers.stop_grace);
        self.pilot_v1 = start.pilot_v1;
        self.todo_v1 = start.todo_v1;
        self.compact_v1 = start.compact_v1;
        self.send_now_v1 = start.send_now_v1;
        self.background_v1 = start.background_v1;
        self.session_tools_v1 = start.session_tools_v1;
        self.workdir = Some(PathBuf::from(&start.workdir));
        if self.session_tools_v1 {
            self.git_head = git_facts::head(Path::new(&start.workdir)).await;
        }
        self.provider = canterlot_proto::harness::provider_of(&start.profile)
            .unwrap_or_default()
            .into();
        let Some(instructions) = start.instructions.as_ref() else {
            // Хаб всегда присылает инструкции (план 2c, задача 3): их отсутствие —
            // старый хаб, молчаливый откат на конфигурацию владельца запрещён.
            self.fail_start("instructions_missing")?;
            return self.begin_stop(StopCause::InvalidInstructions, false);
        };
        let instructions = match InstructionSpec::from_proto(Some(instructions)) {
            Ok(instructions) => instructions,
            Err(_) => {
                self.fail_start("invalid_instructions")?;
                return self.begin_stop(StopCause::InvalidInstructions, false);
            }
        };
        let spec = StartSpec {
            execution_id: self.spec.execution_id,
            generation: self.spec.generation,
            workdir: PathBuf::from(&start.workdir),
            native_conversation_id: (!start.native_conversation_id.is_empty())
                .then(|| start.native_conversation_id.clone()),
            profile: start.profile.clone(),
            model: start.model,
            effort: start.effort,
            permission_mode: start.permission_mode,
            instructions,
            isolate_mcp: start.isolate_mcp,
        };
        let outcome = timeout(
            timers.ready_deadline,
            adapter.start(&spec, &*self.containment),
        )
        .await;
        let started = match outcome {
            Ok(Ok(started)) => started,
            other => {
                let detail = match &other {
                    Ok(Err(error)) => error.to_string(),
                    _ => "deadline".to_owned(),
                };
                tracing::warn!(
                    kind = "start_failed",
                    reason = "adapter_start",
                    detail,
                    "adapter start failed"
                );
                // Шаг запуска без текста разговора: иначе владелец увидит только «не запустилось».
                // Отказ адаптера, его прочая ошибка и срок дают разные коды причины.
                let event_type = match &other {
                    Ok(Err(crate::adapter::AdapterError::StartFailed(reason))) => {
                        safe_event_type(reason)
                    }
                    Ok(Err(_)) => "adapter_error".into(),
                    _ => "adapter_start_timeout".into(),
                };
                self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                    execution_id: self.spec.execution_id.to_string(),
                    kind: "start_failed".into(),
                    event_type,
                    ..Default::default()
                }))?;
                let cause = match &other {
                    Ok(Err(crate::adapter::AdapterError::StartFailed(reason))) => {
                        start_failure_cause(Some(*reason))
                    }
                    _ => StopCause::StartFailed,
                };
                return self.begin_stop(cause, false);
            }
        };
        let info = CliStarted {
            pid: started.cli_pid,
            start_ticks: started.cli_start_ticks,
            native_conversation_id: started.native_conversation_id.clone(),
        };
        self.adapter = Some(adapter);
        self.started = Some(info.clone());
        self.lifecycle = Lifecycle::Running;
        self.send(up::Msg::CliStarted(info));
        self.state_record("running", "", &started.native_conversation_id)?;
        self.phase_record(Phase::AwaitingInput)?;
        self.ready = true;
        Ok(())
    }

    fn phase_record(&mut self, phase: Phase) -> Result<(), SupervisorError> {
        self.record(Body::PhaseChanged(PhaseChanged {
            execution_id: self.spec.execution_id.to_string(),
            phase: snake(&phase),
            ..Default::default()
        }))?;
        Ok(())
    }

    fn on_deliver(&mut self, deliver: canterlot_proto::worker::ipc::Deliver) {
        let command = deliver.command;
        let ack = |command_id: String, attempt: u32, result: &str| {
            up::Msg::DeliverAck(DeliverAck {
                command_id,
                dispatch_attempt: attempt,
                result: result.into(),
            })
        };
        let Some(command) = command else { return };
        let parsed = Uuid::parse_str(&command.command_id).ok();
        let (Some(id), Some(text)) = (parsed, input_text(&command)) else {
            return self.send(ack(command.command_id, command.dispatch_attempt, "invalid"));
        };
        let Ok(settings) = input_settings(&command) else {
            return self.send(ack(command.command_id, command.dispatch_attempt, "invalid"));
        };
        let attempt = command.dispatch_attempt;
        let key = (id, attempt);
        let result = if command.execution_id != self.spec.execution_id.to_string()
            || command.execution_generation != self.spec.generation
        {
            "generation_mismatch"
        } else if self.inputs.contains_key(&key) || self.transmitted.contains(&id) {
            "duplicate"
        } else if matches!(self.lifecycle, Lifecycle::Draining { .. }) {
            "execution_closed"
        } else if matches!(self.lifecycle, Lifecycle::Stopping(_)) {
            "execution_stopped"
        } else if self
            .cancel_marks
            .get(&id)
            .is_some_and(|from| attempt >= *from)
        {
            self.inputs.insert(
                key,
                Input {
                    state: InputState::Cancelled,
                    text: String::new(),
                    accepted: false,
                    files: Vec::new(),
                    settings: None,
                },
            );
            "duplicate"
        } else {
            self.inputs.insert(
                key,
                Input {
                    state: InputState::Queued,
                    text,
                    accepted: false,
                    files: deliver
                        .files
                        .into_iter()
                        .map(|f| LocalFile {
                            path: f.path.into(),
                            name: f.name,
                            mime: f.mime,
                            size: f.size,
                            image: f.image,
                        })
                        .collect(),
                    settings,
                },
            );
            self.queue.push_back(key);
            "accepted_for_gate"
        };
        self.send(ack(command.command_id, attempt, result));
    }

    /// Влить ожидающий ввод в идущий ход («Отправить сейчас»). Новый ход не
    /// открывается: доказательство приписывает ввод чужому ходу, слот хода и
    /// очередь не трогаются. Любой отказ оставляет ввод в очереди хаба —
    /// следующим ходом он уйдёт сам, дубля нет.
    async fn on_steer(
        &mut self,
        deliver: canterlot_proto::worker::ipc::Deliver,
    ) -> Result<(), SupervisorError> {
        let Some(command) = deliver.command else {
            return Ok(());
        };
        let ack = |command_id: String, attempt: u32, result: &str| {
            up::Msg::DeliverAck(DeliverAck {
                command_id,
                dispatch_attempt: attempt,
                result: result.into(),
            })
        };
        // Подтверждения steer каналу управления не нужны: итог едет записью
        // журнала, хаб маршрутизирует её по виду команды. Строки здесь ни с чем
        // не пересекаются, управляющая служба их игнорирует.
        let Some(canterlot_proto::worker::v1::command::Kind::SteerInput(steer)) =
            command.kind.clone()
        else {
            return Ok(());
        };
        let (Ok(sid), Ok(_input_id), Some(text)) = (
            Uuid::parse_str(&command.command_id),
            Uuid::parse_str(&steer.input_id),
            input_text(&command),
        ) else {
            return Ok(());
        };
        let attempt = command.dispatch_attempt;
        let key = (sid, attempt);
        let expected: Option<Uuid> = Uuid::parse_str(&steer.expected_running_command_id).ok();
        let deferred = |supervisor: &mut Self, reason: &str| {
            supervisor.record(Body::DispatchDeferred(InputDispatchDeferred {
                command_id: command.command_id.clone(),
                dispatch_attempt: attempt,
                reason: reason.into(),
            }))?;
            supervisor.send(ack(command.command_id.clone(), attempt, "steer_deferred"));
            Ok(())
        };
        // Проверка в порядке дешевизны: поколение, дубли, затем состояние хода.
        if command.execution_id != self.spec.execution_id.to_string()
            || command.execution_generation != self.spec.generation
            || matches!(self.lifecycle, Lifecycle::Draining { .. })
        {
            return deferred(self, "generation_closed");
        }
        if !matches!(self.lifecycle, Lifecycle::Running) {
            return deferred(self, "generation_closed");
        }
        if self.inputs.contains_key(&key) {
            // Повторная выдача той же команды: итог уже записан или пишется.
            self.send(ack(command.command_id.clone(), attempt, "steer_duplicate"));
            return Ok(());
        }
        if !self.send_now_v1 {
            return deferred(self, "steer_unsupported");
        }
        let running = match self.active_turn {
            Some(turn) if expected.is_none_or(|want| want == turn) => turn,
            _ => return deferred(self, "steer_no_turn"),
        };
        let _ = running;
        if self.aborting_turn {
            return deferred(self, "steer_aborting");
        }
        if self.sj.has_open_requests() {
            return deferred(self, "steer_request_open");
        }
        if self.pending_compaction.is_some() || self.compaction_episode.is_some() {
            return deferred(self, "steer_compacting");
        }
        let Some(adapter) = self.adapter.as_mut() else {
            return deferred(self, "steer_no_turn");
        };
        let Ok(settings) = input_settings(&command) else {
            self.record(Body::InputRejected(InputRejected {
                command_id: command.command_id.clone(),
                dispatch_attempt: attempt,
                reason: "invalid".into(),
            }))?;
            self.send(ack(command.command_id.clone(), attempt, "steer_rejected"));
            return Ok(());
        };
        self.inputs.insert(
            key,
            Input {
                state: InputState::Queued,
                text,
                accepted: false,
                files: deliver
                    .files
                    .into_iter()
                    .map(|f| LocalFile {
                        path: f.path.into(),
                        name: f.name,
                        mime: f.mime,
                        size: f.size,
                        image: f.image,
                    })
                    .collect(),
                settings,
            },
        );
        let transmission = Transmission {
            command_id: sid,
            attempt,
            text: std::mem::take(&mut self.inputs.get_mut(&key).unwrap().text),
            files: std::mem::take(&mut self.inputs.get_mut(&key).unwrap().files),
            settings: self.inputs.get_mut(&key).unwrap().settings.take(),
        };
        self.inputs.get_mut(&key).unwrap().state = InputState::Transmitting;
        let outcome = adapter.steer(&transmission).await;
        match outcome {
            SteerOutcome::Accepted(_) => {
                if let Some(input) = self.inputs.get_mut(&key) {
                    input.accepted = true;
                    input.state = InputState::Done;
                }
                self.record(Body::InputAccepted(InputAccepted {
                    command_id: command.command_id.clone(),
                    dispatch_attempt: attempt,
                    machine_time_ms: now_ms(),
                }))?;
                self.send(ack(command.command_id.clone(), attempt, "steer_accepted"));
            }
            SteerOutcome::Rejected(reason) => {
                let reason: String = reason.chars().take(128).collect();
                self.inputs.remove(&key);
                self.record(Body::InputRejected(InputRejected {
                    command_id: command.command_id.clone(),
                    dispatch_attempt: attempt,
                    reason,
                }))?;
                self.send(ack(command.command_id.clone(), attempt, "steer_rejected"));
            }
            SteerOutcome::NotApplicable(reason) => {
                self.inputs.remove(&key);
                self.record(Body::DispatchDeferred(InputDispatchDeferred {
                    command_id: command.command_id.clone(),
                    dispatch_attempt: attempt,
                    reason,
                }))?;
                self.send(ack(command.command_id.clone(), attempt, "steer_deferred"));
            }
            SteerOutcome::Undetermined => {
                self.record(Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                    command_id: command.command_id.clone(),
                    dispatch_attempt: attempt,
                    reason: "undetermined".into(),
                }))?;
                self.reported.insert((sid, attempt, "unconfirmed"));
                self.send(ack(
                    command.command_id.clone(),
                    attempt,
                    "steer_unconfirmed",
                ));
            }
        }
        Ok(())
    }

    fn closed_input_reply(&mut self, target: &str, attempt: u32, result: &str) {
        let result = if Uuid::parse_str(target).is_ok_and(|id| self.transmitted.contains(&id)) {
            "not_applicable"
        } else {
            result
        };
        self.send(up::Msg::DeliverAck(DeliverAck {
            command_id: target.into(),
            dispatch_attempt: attempt,
            result: result.into(),
        }));
    }

    fn on_cancel(&mut self, target: &str, attempt: u32) -> Result<(), SupervisorError> {
        let Ok(id) = Uuid::parse_str(target) else {
            return Ok(());
        };
        self.cancel_marks
            .entry(id)
            .and_modify(|from| *from = (*from).min(attempt))
            .or_insert(attempt);
        let mut won = false;
        let mut keys: Vec<_> = self
            .inputs
            .iter()
            .filter(|((command, n), input)| {
                *command == id && *n >= attempt && input.state == InputState::Queued
            })
            .map(|(key, _)| *key)
            .collect();
        keys.sort();
        for key in keys {
            self.finish_queued(key, InputState::Cancelled);
            self.record(Body::CancelWon(InputCancelWon {
                command_id: id.to_string(),
                dispatch_attempt: key.1,
            }))?;
            won = true;
        }
        let transmitted = self.transmitted.contains(&id);
        if transmitted {
            if self.reported.insert((id, attempt, "cancel_lost")) {
                self.record(Body::CancelLost(InputCancelLost {
                    command_id: id.to_string(),
                    dispatch_attempt: attempt,
                    reason: "transmission_started".into(),
                }))?;
            }
        } else if !won && !self.inputs.contains_key(&(id, attempt)) {
            self.inputs.insert(
                (id, attempt),
                Input {
                    state: InputState::Cancelled,
                    text: String::new(),
                    accepted: false,
                    files: Vec::new(),
                    settings: None,
                },
            );
            self.record(Body::CancelWon(InputCancelWon {
                command_id: id.to_string(),
                dispatch_attempt: attempt,
            }))?;
        }
        Ok(())
    }

    fn on_withdraw(&mut self, target: &str, attempt: u32) -> Result<(), SupervisorError> {
        let Ok(id) = Uuid::parse_str(target) else {
            return Ok(());
        };
        let key = (id, attempt);
        match self.inputs.get(&key).map(|input| input.state) {
            Some(InputState::Queued) => {
                self.finish_queued(key, InputState::Withdrawn);
                self.record(Body::InputWithdrawn(InputWithdrawn {
                    command_id: id.to_string(),
                    dispatch_attempt: attempt,
                }))?;
            }
            Some(InputState::Cancelled | InputState::Withdrawn) => {}
            Some(InputState::Transmitting | InputState::Done) => {
                self.refuse_withdraw(id, attempt)?
            }
            None if self.transmitted.contains(&id) => self.refuse_withdraw(id, attempt)?,
            None => {
                self.inputs.insert(
                    key,
                    Input {
                        state: InputState::Withdrawn,
                        text: String::new(),
                        accepted: false,
                        files: Vec::new(),
                        settings: None,
                    },
                );
                self.record(Body::InputWithdrawn(InputWithdrawn {
                    command_id: id.to_string(),
                    dispatch_attempt: attempt,
                }))?;
            }
        }
        Ok(())
    }

    fn refuse_withdraw(&mut self, id: Uuid, attempt: u32) -> Result<(), SupervisorError> {
        if self.reported.insert((id, attempt, "withdraw_refused")) {
            self.record(Body::WithdrawRefused(
                canterlot_proto::worker::v1::WithdrawRefused {
                    command_id: id.to_string(),
                    dispatch_attempt: attempt,
                    reason: "too_late".into(),
                },
            ))?;
        }
        Ok(())
    }

    fn finish_queued(&mut self, key: (Uuid, u32), state: InputState) {
        if let Some(input) = self.inputs.get_mut(&key) {
            input.state = state;
            input.text = String::new();
        }
        self.queue.retain(|queued| *queued != key);
    }

    /// Hold — решение адаптера: серверное сжатие очереди ввода не держит.
    /// Без адаптера держим: передать всё равно некому, а потерять нельзя.
    fn compaction_holds_input(&self) -> bool {
        self.pending_compaction.is_some()
            && self
                .adapter
                .as_ref()
                .is_none_or(|adapter| adapter.holds_input_during_compaction())
    }

    async fn try_transmit(&mut self) -> Result<(), SupervisorError> {
        if !matches!(self.lifecycle, Lifecycle::Running)
            || self.adapter_exited
            || !self.ready
            || self.active_turn.is_some()
            || self.compaction_holds_input()
            || self.faults.gate_held(&self.exec_dir)
        {
            return Ok(());
        }
        while let Some(key) = self.queue.pop_front() {
            let Some(input) = self.inputs.get_mut(&key) else {
                continue;
            };
            if input.state != InputState::Queued
                || self
                    .cancel_marks
                    .get(&key.0)
                    .is_some_and(|from| key.1 >= *from)
            {
                continue;
            }
            if self.transmitted.contains(&key.0) {
                input.state = InputState::Done;
                input.text = String::new();
                self.record(Body::DispatchDeferred(InputDispatchDeferred {
                    command_id: key.0.to_string(),
                    dispatch_attempt: key.1,
                    reason: "already_transmitted".into(),
                }))?;
                continue;
            }
            let text = std::mem::take(&mut input.text);
            let transmission = Transmission {
                command_id: key.0,
                attempt: key.1,
                text,
                files: std::mem::take(&mut input.files),
                settings: input.settings.take(),
            };
            if let Some(adapter) = self.adapter.as_ref()
                && let Err(reason) = adapter.validate_transmission(&transmission)
            {
                input.state = InputState::Done;
                self.record(Body::InputRejected(InputRejected {
                    command_id: key.0.to_string(),
                    dispatch_attempt: key.1,
                    reason: reason.into(),
                }))?;
                continue;
            }
            input.state = InputState::Transmitting;
            self.transmitted.insert(key.0);
            self.turn_started = Some(Instant::now());
            self.sj
                .intent(Body::TransmissionStarted(InputTransmissionStarted {
                    command_id: key.0.to_string(),
                    dispatch_attempt: key.1,
                    machine_time_ms: now_ms(),
                }))?;
            // Передача может ждать доказательства от CLI секундами: хаб узнаёт о ней сразу.
            self.pump_shipping();
            self.faults.abort_at("before_first_byte");
            self.ready = false;
            self.aborting_turn = false;
            // Новый ход: выход процесса после него — уже не остановка пользователем.
            self.interrupted = false;
            self.active_turn = Some(key.0);
            let Some(adapter) = self.adapter.as_mut() else {
                break;
            };
            let outcome = adapter.transmit(&transmission, &*self.containment).await;
            match outcome {
                TransmitOutcome::Accepted(_) => {
                    self.faults.abort_at("after_accepted_before_record");
                    if let Some(input) = self.inputs.get_mut(&key) {
                        input.accepted = true;
                    }
                    self.record(Body::InputAccepted(InputAccepted {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        machine_time_ms: now_ms(),
                    }))?;
                }
                TransmitOutcome::Rejected(reason) => {
                    let reason: String = reason.chars().take(128).collect();
                    self.record(Body::InputRejected(InputRejected {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        reason,
                    }))?;
                    if let Some(input) = self.inputs.get_mut(&key) {
                        input.state = InputState::Done;
                    }
                    self.active_turn = None;
                    self.ready = true;
                }
                TransmitOutcome::Undetermined => {
                    self.record(Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        reason: "undetermined".into(),
                    }))?;
                    self.reported.insert((key.0, key.1, "unconfirmed"));
                }
            }
            break;
        }
        Ok(())
    }

    async fn on_adapter_event(&mut self, event: AdapterEvent) -> Result<(), SupervisorError> {
        match event {
            AdapterEvent::RequestOpened {
                request_id,
                kind,
                payload,
            } => {
                if self.sj.request(&request_id).is_some() {
                    return Ok(());
                }
                if self.active_turn.is_none() || !matches!(self.lifecycle, Lifecycle::Running) {
                    // Решения принимаются только в ходе: фоновому субагенту между ходами
                    // ответить некому, а CLI ждал бы вечно.
                    return self
                        .refuse(&request_id, "request_outside_turn", "No active turn")
                        .await;
                }
                let payload_json = (request_id.len() <= MAX_REQUEST_ID_BYTES)
                    .then(|| bounded_json(&kind, payload))
                    .flatten();
                let Some(payload_json) = payload_json else {
                    return self
                        .refuse(
                            &request_id,
                            "request_too_large",
                            "Request is too large to show to the owner",
                        )
                        .await;
                };
                self.record(Body::RequestOpened(RequestOpened {
                    request_id: request_id.clone(),
                    execution_id: self.spec.execution_id.to_string(),
                    generation: self.spec.generation,
                    kind,
                    payload_json,
                }))?;
                if self.aborting_turn {
                    // Карточка закрыта сразу: ответить на неё нельзя, CLI нужен отказ.
                    self.close_requests("interrupted")?;
                    if let Some(adapter) = self.adapter.as_mut() {
                        adapter
                            .refuse_request(&request_id, "The turn is being interrupted")
                            .await;
                    }
                }
            }
            AdapterEvent::RequestClosed { request_id, reason } => {
                self.record(Body::RequestClosed(RequestClosed { request_id, reason }))?;
            }
            AdapterEvent::Phase {
                phase,
                tool_name,
                summary,
            } => {
                // Без открытого запроса фаза ожидания держала бы очередь разговора.
                if matches!(phase, Phase::AwaitingPermission | Phase::AwaitingAnswer)
                    && !self.sj.has_open_requests()
                {
                    return Ok(());
                }
                let ready = matches!(phase, Phase::AwaitingInput);
                self.record(Body::PhaseChanged(PhaseChanged {
                    execution_id: self.spec.execution_id.to_string(),
                    phase: snake(&phase),
                    tool_name: tool_name.unwrap_or_default(),
                    summary: summary.unwrap_or_default(),
                }))?;
                if ready {
                    self.ready = true;
                }
            }
            AdapterEvent::BlockStarted {
                block_id,
                kind,
                input_command_id,
                parent,
                agent,
                model,
                tool_name,
                summary,
                approved_by,
            } => {
                self.open_blocks.insert(block_id, OpenBlock::new());
                self.record(Body::BlockStarted(BlockStarted {
                    block_id: block_id.to_string(),
                    rev: 1,
                    kind: snake(&kind),
                    input_command_id: input_command_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    parent_block_id: parent.map(|id| id.to_string()).unwrap_or_default(),
                    agent,
                    model,
                    tool_name: tool_name.unwrap_or_default(),
                    summary: summary.unwrap_or_default(),
                    approved_by: approved_by.unwrap_or_default(),
                }))?;
            }
            AdapterEvent::BlockDelta { block_id, append } => {
                let Some(mut block) = self.open_blocks.remove(&block_id) else {
                    return Ok(());
                };
                let shipped = self.ship_deltas(block_id, &mut block, &append);
                self.open_blocks.insert(block_id, block);
                shipped?;
            }
            AdapterEvent::BlockFinal {
                block_id,
                text,
                summary,
            } => {
                let Some(mut block) = self.open_blocks.remove(&block_id) else {
                    return Ok(());
                };
                let omitted = text.len() > BLOCK_FINAL_INLINE_MAX_BYTES;
                if omitted {
                    let tail = block.missing_tail(&text);
                    self.ship_deltas(block_id, &mut block, tail)?;
                }
                self.record(Body::BlockFinal(BlockFinal {
                    block_id: block_id.to_string(),
                    rev: block.rev + 1,
                    checksum: checksum(&text),
                    text: if omitted { String::new() } else { text },
                    text_omitted: omitted,
                    summary: summary.unwrap_or_default(),
                }))?;
            }
            AdapterEvent::RunAdopted { input_command_id } => {
                self.adopted_turn = Some(input_command_id);
                if self.active_turn.is_none() && matches!(self.lifecycle, Lifecycle::Running) {
                    self.active_turn = Some(input_command_id);
                    self.turn_started = Some(Instant::now());
                    self.ready = false;
                    self.aborting_turn = false;
                }
            }
            AdapterEvent::RunFinished {
                input_command_id,
                usage,
                context_tokens,
                context_window_tokens,
            } => {
                let adopted = self.adopted_turn == Some(input_command_id);
                if adopted {
                    self.adopted_turn = None;
                }
                // Конец приписанного запуска мог прийти, когда новый ввод уже передан.
                let ours = !adopted || self.active_turn == Some(input_command_id);
                let reason = if self.pending_interrupts.is_empty() {
                    "turn_ended"
                } else {
                    "interrupted"
                };
                let git = self.turn_git_facts().await;
                let open = self.sj.open_requests();
                self.close_blocks(reason)?;
                self.close_requests(reason)?;
                let started = if ours { self.turn_started.take() } else { None };
                let duration_ms = started.map_or(0, |at| {
                    u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
                });
                self.record(Body::RunFinished(RunFinished {
                    adopted,
                    execution_id: self.spec.execution_id.to_string(),
                    input_command_id: input_command_id.to_string(),
                    usage: usage
                        .into_iter()
                        .map(|u| canterlot_proto::worker::v1::ModelUsage {
                            model: u.model,
                            agent: u.agent,
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            cache_read_tokens: u.cache_read_tokens,
                            cache_write_tokens: u.cache_write_tokens,
                        })
                        .collect(),
                    git,
                    context_tokens,
                    context_window_tokens,
                    duration_ms,
                    machine_time_ms: now_ms(),
                }))?;
                if ours {
                    self.active_turn = None;
                    // Прерванный ход закончился, процесс жив: сессия снова idle,
                    // следующий ввод уйдёт в тот же CLI. Без сброса решения
                    // следующего хода отклонялись бы как «во время прерывания».
                    self.aborting_turn = false;
                }
                for ((command, _), input) in self.inputs.iter_mut() {
                    if *command == input_command_id && input.state == InputState::Transmitting {
                        input.state = InputState::Done;
                    }
                }
                self.complete_interrupts()?;
                // Карточка закрыта, ответить на неё уже нельзя: фоновому субагенту нужен отказ.
                if let Some(adapter) = self.adapter.as_mut() {
                    for request in open {
                        adapter
                            .refuse_request(&request.request_id, "The turn has ended")
                            .await;
                    }
                }
            }
            AdapterEvent::Limit(fact) => {
                if !self.pilot_v1 {
                    self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                        execution_id: self.spec.execution_id.to_string(),
                        kind: "capability_missing".into(),
                        event_type: "limit_fact_dropped".into(),
                        ..Default::default()
                    }))?;
                } else {
                    let body = match fact {
                        LimitFact::Observed {
                            window: w,
                            used_permille,
                            resets_at_ms,
                        } => Body::LimitObserved(canterlot_proto::worker::v1::LimitObservation {
                            provider: self.provider.clone(),
                            window: w.into(),
                            used_permille: u32::from(used_permille),
                            resets_at_ms: resets_at_ms.unwrap_or(0),
                            observed_machine_ms: now_ms(),
                            ..Default::default()
                        }),
                        LimitFact::Reached {
                            window: w,
                            resets_at_ms,
                        } => Body::LimitReached(canterlot_proto::worker::v1::LimitReached {
                            execution_id: self.spec.execution_id.to_string(),
                            provider: self.provider.clone(),
                            window: w.into(),
                            resets_at_ms: resets_at_ms.unwrap_or(0),
                            input_command_id: self
                                .active_turn
                                .map(|id| id.to_string())
                                .unwrap_or_default(),
                        }),
                    };
                    self.record(body)?;
                }
            }
            AdapterEvent::Media { block_id, source } => self.on_media(block_id, source)?,
            AdapterEvent::TodoUpdated { lane, items } => self.on_todo(lane, items)?,
            AdapterEvent::BackgroundWork { tasks, wake_turn } => {
                self.on_background(tasks, wake_turn)?
            }
            AdapterEvent::DeferredRestartDue => {
                if matches!(self.lifecycle, Lifecycle::Running)
                    && !self.adapter_exited
                    && self.active_turn.is_none()
                    && let Some(adapter) = self.adapter.as_mut()
                {
                    adapter.apply_deferred(&*self.containment).await;
                }
            }
            AdapterEvent::SessionStuck => self.on_stuck()?,
            AdapterEvent::Compacted { ok } => {
                if let Some((id, attempt)) = self.pending_compaction.take() {
                    let state = if ok {
                        CompactionState::Completed
                    } else {
                        CompactionState::Failed
                    };
                    self.compaction_record(&id, attempt, state)?;
                }
            }
            AdapterEvent::CompactionStarted { manual } => self.on_compaction_started(manual)?,
            AdapterEvent::CompactionFinished {
                manual,
                ok,
                pre_tokens,
                post_tokens,
            } => self.on_compaction_finished(manual, ok, pre_tokens, post_tokens)?,
            AdapterEvent::ContextProgress {
                context_tokens,
                context_window_tokens,
            } => self.on_context_progress(context_tokens, context_window_tokens)?,
            AdapterEvent::ContextExhausted => {
                self.begin_stop(StopCause::ContextExhausted, false)?;
            }
            AdapterEvent::Diagnostic {
                kind,
                event_type,
                required,
                field_names,
            } => {
                self.record(Body::AdapterDiagnostic(diagnostic_fact(
                    self.spec.execution_id.to_string(),
                    kind,
                    &event_type,
                    required,
                    &field_names,
                )))?;
                if required {
                    self.begin_stop(required_diagnostic_cause(kind), false)?;
                }
            }
            AdapterEvent::ProcessExited { code, signal } => {
                self.adapter_exited = true;
                self.active_turn = None;
                self.begin_stop(StopCause::Exited { code, signal }, false)?;
            }
            AdapterEvent::InputAccepted { command_id } => {
                // Позднее доказательство: ввод ещё передаётся и о нём уже сказано
                // «не подтверждён»; хаб снимает паузу unconfirmed_delivery.
                let key = self
                    .inputs
                    .iter()
                    .find(|(key, input)| {
                        key.0 == command_id
                            && input.state == InputState::Transmitting
                            && !input.accepted
                    })
                    .map(|(key, _)| *key)
                    .filter(|key| self.reported.contains(&(key.0, key.1, "unconfirmed")));
                if let Some(key) = key {
                    if let Some(input) = self.inputs.get_mut(&key) {
                        input.accepted = true;
                    }
                    self.record(Body::InputAccepted(InputAccepted {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        machine_time_ms: now_ms(),
                    }))?;
                }
            }
            AdapterEvent::CliRestarted {
                cli_pid,
                cli_start_ticks,
            } => {
                // Перезапуск CLI между ходами: нативный ID тот же, владелец следит за новым pid.
                if let Some(started) = self.started.as_mut() {
                    started.pid = cli_pid;
                    started.start_ticks = cli_start_ticks;
                    let info = started.clone();
                    self.send(up::Msg::CliStarted(info));
                }
            }
            AdapterEvent::ShareFile { call_id, arguments } => {
                self.on_share(call_id, &arguments).await?;
            }
            AdapterEvent::Limits { call_id } => {
                let tool = "limits.report".to_owned();
                let arguments = serde_json::json!({"provider": self.provider});
                let deadline = crate::exec::deadline_for(&tool, &arguments).supervisor;
                self.agent_calls.insert(call_id, Instant::now() + deadline);
                self.send(up::Msg::AgentCall(
                    canterlot_proto::worker::ipc::AgentCall {
                        call_id: call_id.to_string(),
                        tool,
                        arguments_json: arguments.to_string(),
                    },
                ));
            }
            AdapterEvent::AgentCall {
                call_id,
                tool,
                arguments,
            } => {
                use canterlot_proto::limits::MAX_AGENT_CALL_BYTES;
                let arguments_json = serde_json::to_string(&arguments).unwrap_or_default();
                if arguments_json.len() > MAX_AGENT_CALL_BYTES {
                    if let Some(adapter) = self.adapter.as_mut() {
                        adapter
                            .agent_call_result(
                                call_id,
                                AgentCallResult::Err {
                                    code: "too_large".into(),
                                    message: "аргументы больше лимита".into(),
                                },
                            )
                            .await;
                    }
                    return Ok(());
                }
                // Ответа хаба ждём столько, сколько допускает инструмент: ожидание сессии длиннее прочих.
                let deadline = crate::exec::deadline_for(&tool, &arguments).supervisor;
                self.agent_calls.insert(call_id, Instant::now() + deadline);
                self.send(up::Msg::AgentCall(
                    canterlot_proto::worker::ipc::AgentCall {
                        call_id: call_id.to_string(),
                        tool,
                        arguments_json,
                    },
                ));
            }
        }
        Ok(())
    }

    /// Запрос, который нельзя показать владельцу, получает явный отказ: иначе
    /// CLI ждал бы ответа вечно. В журнале остаётся только отметка.
    async fn refuse(
        &mut self,
        request_id: &str,
        event_type: &str,
        message: &str,
    ) -> Result<(), SupervisorError> {
        self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
            execution_id: self.spec.execution_id.to_string(),
            kind: "unknown_optional_event".into(),
            event_type: event_type.into(),
            ..Default::default()
        }))?;
        if let Some(adapter) = self.adapter.as_mut() {
            adapter.refuse_request(request_id, message).await;
        }
        Ok(())
    }

    /// Текст дельтами не крупнее лимита, ревизии подряд от последней ушедшей.
    fn ship_deltas(
        &mut self,
        block_id: Uuid,
        block: &mut OpenBlock,
        text: &str,
    ) -> Result<(), SupervisorError> {
        for chunk in delta_chunks(text) {
            self.record(Body::BlockDelta(BlockDelta {
                block_id: block_id.to_string(),
                base_rev: block.rev,
                rev: block.rev + 1,
                append: chunk.to_owned(),
            }))?;
            block.sent(chunk);
        }
        Ok(())
    }

    /// Факты git на конец хода; без возможности хаба их нет вовсе. Недоступное чтение
    /// оставляет прежний HEAD, и следующий ход покроет оба.
    async fn turn_git_facts(&mut self) -> Option<canterlot_proto::worker::v1::GitFacts> {
        if !self.session_tools_v1 {
            return None;
        }
        let workdir = self.workdir.clone()?;
        let facts = git_facts::collect(&workdir, self.git_head.as_deref()).await;
        if facts.available {
            self.git_head = Some(facts.head_after.clone());
        }
        Some(facts)
    }

    fn close_blocks(&mut self, reason: &str) -> Result<(), SupervisorError> {
        for (block_id, block) in std::mem::take(&mut self.open_blocks) {
            self.record(Body::BlockIncomplete(BlockIncomplete {
                block_id: block_id.to_string(),
                rev: block.rev + 1,
                reason: reason.into(),
            }))?;
        }
        Ok(())
    }

    fn complete_interrupts(&mut self) -> Result<(), SupervisorError> {
        for (command_id, attempt) in std::mem::take(&mut self.pending_interrupts) {
            self.record(Body::InterruptStatus(InterruptStatus {
                command_id,
                dispatch_attempt: attempt,
                state: InterruptState::Completed as i32,
            }))?;
        }
        Ok(())
    }

    fn interrupt_record(
        &mut self,
        id: &str,
        attempt: u32,
        state: InterruptState,
    ) -> Result<(), SupervisorError> {
        self.record(Body::InterruptStatus(InterruptStatus {
            command_id: id.to_owned(),
            dispatch_attempt: attempt,
            state: state as i32,
        }))?;
        Ok(())
    }

    async fn on_interrupt(&mut self, id: String, attempt: u32) -> Result<(), SupervisorError> {
        if !self.known_interrupts.insert((id.clone(), attempt)) {
            return Ok(());
        }
        match &self.lifecycle {
            Lifecycle::Stopping(_) | Lifecycle::Draining { .. } => {
                return self.interrupt_record(&id, attempt, InterruptState::Superseded);
            }
            _ => {}
        }
        if self.active_turn.is_none() || self.adapter.is_none() {
            return self.interrupt_record(&id, attempt, InterruptState::NotApplicable);
        }
        self.interrupt_record(&id, attempt, InterruptState::SentToAgent)?;
        self.aborting_turn = true;
        self.sj.flush()?;
        let Some(adapter) = self.adapter.as_mut() else {
            return Ok(());
        };
        match adapter.interrupt().await {
            InterruptOutcome::Acknowledged => {
                self.interrupted = true;
                self.close_requests("interrupted")?;
                self.interrupt_record(&id, attempt, InterruptState::Acknowledged)?;
                self.pending_interrupts.push((id, attempt));
            }
            InterruptOutcome::Undetermined => {
                self.interrupted = true;
                self.close_requests("interrupted")?;
                self.pending_interrupts.push((id, attempt));
            }
            InterruptOutcome::NotApplicable => {
                // Прерывать было нечего: ход и его запросы остаются как были.
                self.aborting_turn = false;
                self.interrupt_record(&id, attempt, InterruptState::NotApplicable)?;
            }
        }
        Ok(())
    }

    fn compaction_record(
        &mut self,
        id: &str,
        attempt: u32,
        state: CompactionState,
    ) -> Result<(), SupervisorError> {
        self.record(Body::CompactionStatus(CompactionStatus {
            command_id: id.to_owned(),
            dispatch_attempt: attempt,
            state: state.into(),
        }))?;
        Ok(())
    }

    fn on_stuck(&mut self) -> Result<(), SupervisorError> {
        if !self.compact_v1 {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "capability_missing".into(),
                event_type: "session_stuck_dropped".into(),
                ..Default::default()
            }))?;
            return Ok(());
        }
        self.record(Body::SessionStuck(SessionStuck {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
        }))?;
        Ok(())
    }

    /// Начало эпизода сжатия для durable ленты. Повторный старт того же эпизода
    /// (CLI дублирует статус) запись не дублирует; без возможности хаба эпизод
    /// отбрасывается, как `SessionStuck`.
    fn on_compaction_started(&mut self, manual: bool) -> Result<(), SupervisorError> {
        if !self.compact_v1 {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "capability_missing".into(),
                event_type: "compaction_event_dropped".into(),
                ..Default::default()
            }))?;
            return Ok(());
        }
        if self.compaction_episode.is_some() {
            return Ok(());
        }
        let id = Uuid::now_v7();
        self.compaction_episode = Some((id, manual));
        self.record(Body::CompactionEvent(CompactionEvent {
            id: id.to_string(),
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            started: true,
            manual,
            ok: false,
            pre_tokens: 0,
        }))?;
        Ok(())
    }

    /// Конец эпизода сжатия. Финиш без старта (переподключение в середине) открывает
    /// и закрывает эпизод сразу, чтобы тихая строка всё равно пережила перезагрузку.
    /// Удачное сжатие обнуляет живой контекст: индикатор ждёт новый размер, а не
    /// показывает досжаточные проценты.
    fn on_compaction_finished(
        &mut self,
        manual: bool,
        ok: bool,
        pre_tokens: u64,
        post_tokens: u64,
    ) -> Result<(), SupervisorError> {
        if !self.compact_v1 {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "capability_missing".into(),
                event_type: "compaction_event_dropped".into(),
                ..Default::default()
            }))?;
            return Ok(());
        }
        let (id, _) = match self.compaction_episode.take() {
            Some(episode) => episode,
            None => (Uuid::now_v7(), manual),
        };
        self.record(Body::CompactionEvent(CompactionEvent {
            id: id.to_string(),
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            started: false,
            manual,
            ok,
            pre_tokens,
        }))?;
        if ok {
            // Новый размер известен сразу: индикатор показывает его, а не прячется.
            // Без него расход гасится, окно при этом не трогаем.
            self.record(Body::ContextProgress(ContextProgress {
                execution_id: self.spec.execution_id.to_string(),
                generation: self.spec.generation,
                context_tokens: post_tokens,
                context_window_tokens: 0,
            }))?;
        }
        Ok(())
    }

    /// Живой расход контекста в ходу. Без возможности хаба молча отбрасывается:
    /// поток частый, диагностика на каждое обновление залила бы журнал.
    fn on_context_progress(
        &mut self,
        context_tokens: u64,
        context_window_tokens: u64,
    ) -> Result<(), SupervisorError> {
        if !self.compact_v1 {
            return Ok(());
        }
        self.record(Body::ContextProgress(ContextProgress {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            context_tokens,
            context_window_tokens,
        }))?;
        Ok(())
    }

    // Сжатие запускает отдельный период работы: во время хода его не начать.
    async fn on_compact(&mut self, id: String, attempt: u32) -> Result<(), SupervisorError> {
        if !self.known_compactions.insert((id.clone(), attempt)) {
            return Ok(());
        }
        if matches!(
            self.lifecycle,
            Lifecycle::Stopping(_) | Lifecycle::Draining { .. }
        ) {
            return self.compaction_record(&id, attempt, CompactionState::Superseded);
        }
        // Без compact_v1 хаб не примет ни эпизод, ни сброс контекста: сжатие прошло бы
        // без строки в ленте.
        if !self.compact_v1 {
            return self.compaction_record(&id, attempt, CompactionState::NotApplicable);
        }
        let (Some(command), None, Some(adapter)) = (
            Uuid::parse_str(&id).ok(),
            self.active_turn,
            self.adapter.as_mut(),
        ) else {
            return self.compaction_record(&id, attempt, CompactionState::NotApplicable);
        };
        match adapter.compact(command).await {
            CompactOutcome::Accepted => {
                self.compaction_record(&id, attempt, CompactionState::SentToAgent)?;
                self.pending_compaction = Some((id, attempt));
                Ok(())
            }
            CompactOutcome::NotApplicable => {
                self.compaction_record(&id, attempt, CompactionState::NotApplicable)
            }
            CompactOutcome::Failed => self.compaction_record(&id, attempt, CompactionState::Failed),
        }
    }

    async fn on_stop(&mut self, force: bool) -> Result<(), SupervisorError> {
        match &mut self.lifecycle {
            Lifecycle::Stopping(state) => {
                if force && !state.force {
                    state.force = true;
                    if matches!(state.cause, StopCause::User { .. }) {
                        state.cause = StopCause::User { force: true };
                    }
                    state.next_round = Instant::now();
                    // Адаптер уходит вместе с задачей, а CLI убивает kill_on_drop.
                    if let Some(closing) = &self.closing {
                        closing.abort();
                    }
                }
                Ok(())
            }
            Lifecycle::Draining { .. } => Ok(()),
            Lifecycle::Starting => {
                self.start_attempted = true;
                self.begin_stop(StopCause::StartCancelled, force)
            }
            Lifecycle::Running => self.begin_stop(StopCause::User { force }, force),
        }
    }

    fn begin_stop(&mut self, cause: StopCause, force: bool) -> Result<(), SupervisorError> {
        match &mut self.lifecycle {
            Lifecycle::Stopping(_) | Lifecycle::Draining { .. } => return Ok(()),
            _ => {}
        }
        if matches!(cause, StopCause::Exited { .. }) {
            self.complete_interrupts()?;
        }
        self.close_requests(
            if matches!(cause, StopCause::User { .. } | StopCause::StartCancelled) {
                "interrupted"
            } else {
                "execution_terminal"
            },
        )?;
        self.state_record("stopping", "", "")?;
        self.lifecycle = Lifecycle::Stopping(StopState {
            cause,
            force,
            closed: false,
            next_round: Instant::now(),
            survivors_reported: 0,
        });
        Ok(())
    }

    async fn drive_stop(&mut self) -> Result<(), SupervisorError> {
        let (force, closed) = match &self.lifecycle {
            Lifecycle::Stopping(state)
                if self.terminate.is_none()
                    && self.closing.is_none()
                    && Instant::now() >= state.next_round =>
            {
                (state.force, state.closed)
            }
            _ => return Ok(()),
        };
        if !closed {
            if let Lifecycle::Stopping(state) = &mut self.lifecycle {
                state.closed = true;
            }
            self.fail_agent_calls().await;
            if let Some(mut adapter) = self.adapter.take() {
                let mode = if force {
                    CloseMode::Force
                } else {
                    CloseMode::Graceful
                };
                let containment = self.containment.clone();
                self.closing = Some(tokio::spawn(async move {
                    let _ = adapter.close(mode, &*containment).await;
                    adapter
                }));
                return Ok(());
            }
        }
        let grace = if force {
            Duration::from_millis(200)
        } else {
            self.spec.timers.stop_grace
        };
        let fake = self.faults.take_survivor();
        let error = self.faults.take_error();
        let containment = self.containment.clone();
        self.terminate = Some(tokio::spawn(async move {
            if error {
                Err(ContainmentError::Process(rustix::io::Errno::PERM))
            } else if fake {
                Ok(Termination::Survivors(vec![Member {
                    pid: 0,
                    start_ticks: 0,
                }]))
            } else {
                containment.terminate(grace).await
            }
        }));
        Ok(())
    }

    async fn on_terminate(
        &mut self,
        result: Result<Result<Termination, ContainmentError>, tokio::task::JoinError>,
    ) -> Result<(), SupervisorError> {
        self.terminate = None;
        let empty = matches!(result, Ok(Ok(Termination::Empty)))
            && matches!(self.containment.populated(), Ok(false));
        if empty {
            return self.finish_stop();
        }
        let survivors = match &result {
            Ok(Ok(Termination::Survivors(members))) => members.len(),
            _ => usize::MAX,
        };
        let report = match &mut self.lifecycle {
            Lifecycle::Stopping(state) => {
                state.next_round = Instant::now() + self.spec.timers.stop_retry;
                let report = survivors > 0 && state.survivors_reported != survivors;
                if report {
                    state.survivors_reported = survivors;
                }
                report
            }
            _ => false,
        };
        if report {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "containment_survivors".into(),
                event_type: String::new(),
                ..Default::default()
            }))?;
        }
        Ok(())
    }

    fn finish_stop(&mut self) -> Result<(), SupervisorError> {
        let Lifecycle::Stopping(state) = &self.lifecycle else {
            return Ok(());
        };
        let cause = state.cause.clone();
        let cli_started = self.started.is_some();
        let (terminal, outcome, reason) = match cause {
            StopCause::User { force } if cli_started => (
                "stopped",
                if force { "force_stopped" } else { "user_stop" },
                "stopped",
            ),
            StopCause::User { .. } | StopCause::StartCancelled => {
                ("stopped", "start_cancelled", "stopped")
            }
            StopCause::StartFailed => ("failed", "start_failed", "start_failed"),
            StopCause::CodexStateBackfillStuck => (
                "failed",
                "codex_state_backfill_stuck",
                "codex_state_backfill_stuck",
            ),
            StopCause::InvalidInstructions => {
                ("failed", "invalid_instructions", "invalid_instructions")
            }
            StopCause::AuthRequired => ("failed", "auth_required", "auth_required"),
            StopCause::CliIncompatible => ("failed", "cli_incompatible", "cli_incompatible"),
            StopCause::ContextExhausted => ("failed", "context_exhausted", "context_exhausted"),
            StopCause::ProtocolIncompatible => {
                ("failed", "protocol_incompatible", "protocol_incompatible")
            }
            StopCause::Exited { code, signal } => {
                let (terminal, outcome) = super::exit_outcome(code, signal, self.interrupted);
                (terminal, outcome, "process_exited")
            }
        };
        self.close_blocks(reason)?;
        self.complete_interrupts()?;
        if let Some((id, attempt)) = self.pending_compaction.take() {
            self.compaction_record(&id, attempt, CompactionState::Superseded)?;
        }
        let mut keys: Vec<_> = self.inputs.keys().copied().collect();
        keys.sort();
        for key in keys {
            let state = self.inputs[&key].state;
            match state {
                InputState::Queued => {
                    self.finish_queued(key, InputState::Done);
                    self.record(Body::DispatchDeferred(InputDispatchDeferred {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        reason: "execution_stopped".into(),
                    }))?;
                }
                InputState::Transmitting
                    if !self.inputs[&key].accepted
                        && !self.reported.contains(&(key.0, key.1, "unconfirmed")) =>
                {
                    self.record(Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                        command_id: key.0.to_string(),
                        dispatch_attempt: key.1,
                        reason: "execution_terminal".into(),
                    }))?;
                }
                _ => {}
            }
        }
        self.state_record(terminal, outcome, "")?;
        if self.faults.will_abort("after_stopped_before_closed") {
            self.sj.flush()?;
        }
        self.faults.abort_at("after_stopped_before_closed");
        self.record(Body::JournalClosed(JournalClosed {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            reason: outcome.into(),
        }))?;
        // Картинки, не опубликованные к закрытию журнала, отбрасываются вместе с каталогом исполнения.
        self.media.ready.clear();
        self.media.inflight = None;
        self.lifecycle = Lifecycle::Draining {
            outcome: outcome.into(),
        };
        tracing::info!(kind = "execution_closed", outcome, "execution closed");
        Ok(())
    }
}

/// Причина остановки по отказу запуска: застрявший перенос базы Codex — свой исход.
fn start_failure_cause(reason: Option<&str>) -> StopCause {
    match reason {
        Some("cli_too_old") => StopCause::CliIncompatible,
        Some("codex_state_backfill_stuck") => StopCause::CodexStateBackfillStuck,
        _ => StopCause::StartFailed,
    }
}

/// Причина остановки по обязательной диагностике адаптера; неизвестная трактуется как несовместимость.
fn required_diagnostic_cause(kind: &str) -> StopCause {
    match kind {
        "auth_required" => StopCause::AuthRequired,
        "cli_too_old" | "session_id_mismatch" => StopCause::CliIncompatible,
        // CLI не прислал init в срок запуска: протокол не нарушен, процесс не поднялся.
        "mcp_unavailable" | "mcp_isolation_failed" | "restart_failed" | "cli_init_missing" => {
            StopCause::StartFailed
        }
        _ => StopCause::ProtocolIncompatible,
    }
}

/// Отказ хаба по файлу `share_file` (причина `PutError::reason`) → код и текст для модели.
/// `share_unavailable` остаётся только за хабом без поддержки файлов и недоступным хабом;
/// названная хабом причина становится своим кодом, остальное — `share_rejected` с причиной.
fn share_refusal(reason: &str) -> (&'static str, String) {
    match reason {
        "too_large" => (
            "share_too_large",
            "the hub refused the file: it is larger than the hub limit of 50 MiB".into(),
        ),
        "invalid_name" => (
            "share_invalid_name",
            "the hub refused the file name; rename the file (at most 255 bytes, no control characters or path separators) and share it again".into(),
        ),
        "invalid_type" => (
            "share_invalid_type",
            "the hub refused the file type: the content does not match it (an image needs its signature; an empty file cannot be an image or a video)".into(),
        ),
        "invalid_text" => (
            "invalid_arguments",
            "the hub refused the title or the note: too long or contains control characters".into(),
        ),
        "rejected" => (
            "share_unavailable",
            "the hub does not accept shared files (rejected); the hub may need an update".into(),
        ),
        "hub_unreachable" => ("share_unavailable", "the hub is unreachable".into()),
        "io" => (
            "share_unreadable",
            "the prepared copy of the file could not be read; share it again".into(),
        ),
        "checksum_mismatch" => (
            "share_rejected",
            "the hub did not accept the file: the content did not match its checksum on the way; share it again".into(),
        ),
        other => (
            "share_rejected",
            format!("the hub did not accept the file ({other})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_refusals_of_a_shared_file_keep_their_reason() {
        use crate::media::PutError;
        let code =
            |message: &str| share_refusal(PutError::from_invalid_argument(message).reason()).0;
        assert_eq!(code("media_too_large"), "share_too_large");
        assert_eq!(code("media_invalid_name"), "share_invalid_name");
        assert_eq!(code("media_invalid_type"), "share_invalid_type");
        assert_eq!(code("media_invalid_text"), "invalid_arguments");
        // Хаб без поддержки share отвечает общим `media_invalid`.
        assert_eq!(code("media_invalid"), "share_unavailable");
        assert_eq!(
            share_refusal(PutError::Transport.reason()).0,
            "share_unavailable"
        );
        assert_eq!(
            share_refusal(PutError::Corrupt.reason()).0,
            "share_rejected"
        );
        let (code, message) = share_refusal(PutError::Conflict.reason());
        assert_eq!(code, "share_rejected");
        assert!(message.contains("conflict"), "{message}");
        assert_eq!(share_refusal(PutError::Io.reason()).0, "share_unreadable");
        let (_, message) = share_refusal("invalid_type");
        assert!(message.contains("empty file"), "{message}");
    }

    #[test]
    fn missing_init_stops_as_start_failure_not_protocol_incompatibility() {
        assert!(matches!(
            required_diagnostic_cause("cli_init_missing"),
            StopCause::StartFailed
        ));
        assert!(matches!(
            required_diagnostic_cause("unknown_required_event"),
            StopCause::ProtocolIncompatible
        ));
        assert!(matches!(
            required_diagnostic_cause("mcp_isolation_failed"),
            StopCause::StartFailed
        ));
    }

    #[test]
    fn stuck_codex_state_backfill_keeps_its_reason() {
        assert!(matches!(
            start_failure_cause(Some("codex_state_backfill_stuck")),
            StopCause::CodexStateBackfillStuck
        ));
        assert!(matches!(
            start_failure_cause(Some("app_server_failed")),
            StopCause::StartFailed
        ));
        assert!(matches!(
            start_failure_cause(Some("cli_too_old")),
            StopCause::CliIncompatible
        ));
    }

    #[test]
    fn real_name_survives_for_kinds_that_carry_one() {
        for (kind, name, stored) in [
            (
                "unknown_notification",
                "thread/futureThing",
                "thread_futureThing",
            ),
            ("unsupported_server_request", "item/new/ask", "item_new_ask"),
            ("unsupported_elicitation", "openai/form", "openai_form"),
            ("unparsed_event", "permission.asked", "permission_asked"),
            ("unknown_event", "session.new", "session_new"),
            ("provider_failure", "rate_limit", "rate_limit"),
        ] {
            assert_eq!(diagnostic_event_type(kind, name), stored, "{kind}");
        }
    }

    #[test]
    fn journal_fact_carries_only_bounded_names() {
        let raw: Vec<String> = (0..50)
            .map(|n| format!("key{n:02}"))
            .chain(["bad key!".to_owned(), "x".repeat(200)])
            .collect();
        let fact = diagnostic_fact("e".into(), "unknown_event", "future", false, &raw);
        assert_eq!(fact.kind, "unknown_optional_event");
        assert_eq!(fact.event_type, "future");
        assert_eq!(fact.field_names.len(), 32);
        assert!(fact.field_names.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            fact.field_names
                .iter()
                .all(|name| name.chars().count() <= 64)
        );
        assert!(
            fact.field_names
                .iter()
                .all(|name| { name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') })
        );
        assert!(
            diagnostic_fact("e".into(), "k", "k", true, &[])
                .field_names
                .is_empty()
        );
    }

    #[test]
    fn kinds_without_a_name_keep_their_own_label() {
        for kind in ["oversized_frame", "todo_lane_unknown", "unparsed_line"] {
            assert_eq!(diagnostic_event_type(kind, ""), kind);
        }
        assert_eq!(
            diagnostic_event_type("codex_warning", "codex_warning"),
            "codex_warning"
        );
    }

    #[derive(Default)]
    struct StubState {
        transmit_calls: Vec<Uuid>,
        agent_call_errors: Vec<(Uuid, String)>,
        ack_interrupt: bool,
        steer_accept: bool,
        steer_undetermined: bool,
        refused: Vec<String>,
        compact_calls: usize,
    }

    struct StubAdapter {
        state: std::sync::Arc<std::sync::Mutex<StubState>>,
        hold_compaction: bool,
    }

    #[async_trait::async_trait]
    impl Adapter for StubAdapter {
        fn holds_input_during_compaction(&self) -> bool {
            self.hold_compaction
        }

        fn capabilities(&self) -> crate::adapter::Capabilities {
            crate::adapter::Capabilities {
                streaming: crate::adapter::Support::Supported,
                resume: crate::adapter::Support::Supported,
                interrupt: crate::adapter::Support::Supported,
                approvals: crate::adapter::Support::Supported,
                questions: crate::adapter::Support::Supported,
                background_work: crate::adapter::Support::Supported,
                interrupt_keeps_process: false,
            }
        }

        async fn start(
            &mut self,
            _spec: &StartSpec,
            _group: &dyn Containment,
        ) -> Result<crate::adapter::Started, crate::adapter::AdapterError> {
            Ok(crate::adapter::Started {
                native_conversation_id: "stub".into(),
                cli_pid: 0,
                cli_start_ticks: 0,
            })
        }

        async fn transmit(
            &mut self,
            input: &Transmission,
            _group: &dyn Containment,
        ) -> TransmitOutcome {
            self.state
                .lock()
                .unwrap()
                .transmit_calls
                .push(input.command_id);
            TransmitOutcome::Accepted(crate::adapter::Proof {
                command_id: input.command_id,
                kind: "stub_proof",
            })
        }

        async fn deliver_decision(
            &mut self,
            _request_id: &str,
            _decision: &DecisionPayload,
        ) -> DecisionOutcome {
            DecisionOutcome::NotApplied("stub".into())
        }

        async fn interrupt(&mut self) -> InterruptOutcome {
            if self.state.lock().unwrap().ack_interrupt {
                InterruptOutcome::Acknowledged
            } else {
                InterruptOutcome::NotApplicable
            }
        }

        async fn compact(&mut self, _command: Uuid) -> crate::adapter::CompactOutcome {
            self.state.lock().unwrap().compact_calls += 1;
            crate::adapter::CompactOutcome::Accepted
        }

        async fn refuse_request(&mut self, request_id: &str, _message: &str) {
            self.state
                .lock()
                .unwrap()
                .refused
                .push(request_id.to_owned());
        }

        async fn steer(&mut self, input: &Transmission) -> SteerOutcome {
            let state = self.state.lock().unwrap();
            if state.steer_accept {
                SteerOutcome::Accepted(crate::adapter::Proof {
                    command_id: input.command_id,
                    kind: "stub_steer",
                })
            } else if state.steer_undetermined {
                SteerOutcome::Undetermined
            } else {
                SteerOutcome::NotApplicable("unsupported".into())
            }
        }

        async fn next_event(&mut self) -> AdapterEvent {
            std::future::pending().await
        }

        async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
            let code = match &result {
                AgentCallResult::Ok(_) => "ok".to_owned(),
                AgentCallResult::Err { code, .. } => code.clone(),
            };
            self.state
                .lock()
                .unwrap()
                .agent_call_errors
                .push((call_id, code));
        }

        async fn close(
            &mut self,
            _mode: CloseMode,
            _group: &dyn Containment,
        ) -> Result<(), crate::adapter::AdapterError> {
            Ok(())
        }
    }

    fn test_supervisor() -> (
        tempfile::TempDir,
        Supervisor,
        std::sync::Arc<std::sync::Mutex<StubState>>,
    ) {
        test_supervisor_with_hold(true)
    }

    fn test_supervisor_with_hold(
        hold_compaction: bool,
    ) -> (
        tempfile::TempDir,
        Supervisor,
        std::sync::Arc<std::sync::Mutex<StubState>>,
    ) {
        let dir = tempfile::TempDir::new().unwrap();
        let sj = Sj::create(dir.path()).unwrap();
        let (media_jobs, _prepared) = mpsc::unbounded_channel();
        let (share_jobs, _share_prepared) = mpsc::unbounded_channel();
        let state = std::sync::Arc::new(std::sync::Mutex::new(StubState::default()));
        let adapter: Option<Box<dyn Adapter>> = Some(Box::new(StubAdapter {
            state: state.clone(),
            hold_compaction,
        }));
        let supervisor = Supervisor {
            attach_deadline: None,
            spec: SupervisorSpec {
                execution_id: Uuid::now_v7(),
                generation: 1,
                conversation_id: Uuid::now_v7(),
                protocol_version: canterlot_proto::limits::IPC_PROTOCOL_VERSION,
                ipc_socket: None,
                timers: Default::default(),
                claude_code: None,
                codex: None,
                opencode: None,
                antigravity: None,
            },
            exec_dir: dir.path().to_owned(),
            secret: Vec::new(),
            sj,
            containment: Arc::new(TreeContainment::new().unwrap()),
            faults: SupervisorFaults::from_env(),
            pid: 0,
            start_ticks: 0,
            adapter,
            adapter_exited: false,
            start_attempted: false,
            started: None,
            ready: true,
            lifecycle: Lifecycle::Running,
            closing: None,
            terminate: None,
            inputs: HashMap::new(),
            queue: VecDeque::new(),
            transmitted: HashSet::new(),
            cancel_marks: HashMap::new(),
            reported: HashSet::new(),
            active_turn: None,
            adopted_turn: None,
            turn_started: None,
            pilot_v1: false,
            todo_v1: false,
            compact_v1: true,
            send_now_v1: true,
            background_v1: false,
            session_tools_v1: false,
            git_head: None,
            background_drop_reported: false,
            todo_drop_reported: false,
            provider: "claude_code".into(),
            open_blocks: BTreeMap::new(),
            known_interrupts: HashSet::new(),
            pending_interrupts: Vec::new(),
            known_compactions: HashSet::new(),
            pending_compaction: None,
            compaction_episode: None,
            interrupted: false,
            aborting_turn: false,
            agent_calls: HashMap::new(),
            conns: HashMap::new(),
            attached: None,
            unknown_ipc_logged: false,
            next_conn: 0,
            workdir: None,
            media: MediaQueue::default(),
            media_jobs,
            shares: ShareQueue::default(),
            share_jobs,
        };
        (dir, supervisor, state)
    }

    fn queue_owner(supervisor: &mut Supervisor) -> (Uuid, u32) {
        let id = Uuid::now_v7();
        supervisor.inputs.insert(
            (id, 1),
            Input {
                state: InputState::Queued,
                text: "held input".into(),
                accepted: false,
                files: Vec::new(),
                settings: None,
            },
        );
        supervisor.queue.push_back((id, 1));
        (id, 1)
    }

    fn shipped_bodies(supervisor: &mut Supervisor) -> Vec<Body> {
        // Цикл сбрасывает журнал после событий, тест — явно.
        supervisor.sj.flush().unwrap();
        let sj = &mut supervisor.sj;
        sj.enable_ship(0);
        let mut out = Vec::new();
        while let Some(batch) = sj.next_batch().unwrap() {
            let last = batch.records.last().map(|record| record.seq).unwrap_or(0);
            for record in batch.records {
                if let Some(body) = record.body.and_then(|record| record.body) {
                    out.push(body);
                }
            }
            sj.ack(last).unwrap();
        }
        out
    }

    fn assert_delivered_once_with_proof(
        supervisor: &mut Supervisor,
        state: &std::sync::Arc<std::sync::Mutex<StubState>>,
        key: (Uuid, u32),
    ) {
        assert_eq!(
            state.lock().unwrap().transmit_calls,
            vec![key.0],
            "held input is delivered exactly once"
        );
        assert!(supervisor.inputs[&key].accepted, "delivery proof recorded");
        let bodies = shipped_bodies(supervisor);
        assert!(
            bodies
                .iter()
                .any(|body| matches!(body, Body::InputAccepted(_))),
            "delivery proof shipped: {bodies:?}"
        );
        assert!(
            bodies
                .iter()
                .all(|body| !matches!(body, Body::AcceptanceUnconfirmed(_))),
            "no undetermined state: {bodies:?}"
        );
    }

    #[tokio::test]
    async fn owner_input_held_while_compaction_in_flight() {
        let (_dir, mut supervisor, state) = test_supervisor();
        // Ввод пришёл до границы сжатия: сжатие принято, итога ещё нет.
        supervisor.pending_compaction = Some(("compact-1".into(), 1));
        let key = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert!(
            state.lock().unwrap().transmit_calls.is_empty(),
            "no transmission while compaction is in flight"
        );
        assert!(matches!(supervisor.inputs[&key].state, InputState::Queued));
        assert!(supervisor.active_turn.is_none());
        // Итог сжатия снял hold: ввод уходит сам, по порядку.
        supervisor
            .on_adapter_event(AdapterEvent::Compacted { ok: true })
            .await
            .unwrap();
        supervisor.try_transmit().await.unwrap();
        assert_delivered_once_with_proof(&mut supervisor, &state, key);
    }

    #[tokio::test]
    async fn owner_input_held_after_compact_boundary_until_result() {
        let (_dir, mut supervisor, state) = test_supervisor();
        // Ввод пришёл между границей и служебным итогом: эпизод закрыт,
        // команда сжатия открыта, hold держится до Compacted.
        supervisor.pending_compaction = Some(("compact-1".into(), 1));
        supervisor
            .on_adapter_event(AdapterEvent::CompactionStarted { manual: true })
            .await
            .unwrap();
        supervisor
            .on_adapter_event(AdapterEvent::CompactionFinished {
                manual: true,
                ok: true,
                pre_tokens: 41_000,
                post_tokens: 15_000,
            })
            .await
            .unwrap();
        assert!(
            supervisor.pending_compaction.is_some(),
            "boundary must not complete the compact command"
        );
        let key = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert!(
            state.lock().unwrap().transmit_calls.is_empty(),
            "no transmission before the service result is consumed"
        );
        assert!(matches!(supervisor.inputs[&key].state, InputState::Queued));
        supervisor
            .on_adapter_event(AdapterEvent::Compacted { ok: true })
            .await
            .unwrap();
        supervisor.try_transmit().await.unwrap();
        assert_delivered_once_with_proof(&mut supervisor, &state, key);
    }

    #[tokio::test]
    async fn server_side_compaction_does_not_hold_owner_input() {
        let (_dir, mut supervisor, state) = test_supervisor_with_hold(false);
        // Сжатие серверное (delivery "queue", как у OpenCode): hold не нужен,
        // ввод уходит сразу, как до фикса.
        supervisor.pending_compaction = Some(("compact-1".into(), 1));
        let key = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert_delivered_once_with_proof(&mut supervisor, &state, key);
    }

    fn steer_deliver(
        supervisor: &Supervisor,
        running_command: Uuid,
        text: &str,
    ) -> canterlot_proto::worker::ipc::Deliver {
        use canterlot_proto::worker::v1::{
            Command, ContentBlock, SteerInput, TextBlock, command::Kind,
        };
        let steer_id = Uuid::now_v7();
        canterlot_proto::worker::ipc::Deliver {
            command: Some(Command {
                command_id: steer_id.to_string(),
                dispatch_attempt: 0,
                execution_id: supervisor.spec.execution_id.to_string(),
                execution_generation: supervisor.spec.generation,
                kind: Some(Kind::SteerInput(SteerInput {
                    input_id: Uuid::now_v7().to_string(),
                    conversation_id: supervisor.spec.conversation_id.to_string(),
                    content: vec![ContentBlock {
                        block: Some(canterlot_proto::worker::v1::content_block::Block::Text(
                            TextBlock {
                                text: text.to_owned(),
                            },
                        )),
                    }],
                    settings: None,
                    expected_running_command_id: running_command.to_string(),
                })),
            }),
            files: Vec::new(),
        }
    }

    fn steer_key(deliver: &canterlot_proto::worker::ipc::Deliver) -> (Uuid, u32) {
        let command = deliver.command.as_ref().unwrap();
        (
            Uuid::parse_str(&command.command_id).unwrap(),
            command.dispatch_attempt,
        )
    }

    #[tokio::test]
    async fn deliver_stores_hub_text_verbatim_for_later_transmission() {
        // Сопровождение не держит текст очереди заранее: что прислал хаб в
        // `deliver_input`, то и уйдёт агенту. Правка живёт только на хабе.
        use canterlot_proto::worker::v1::{
            Command, ContentBlock, DeliverInput, TextBlock, command::Kind,
        };
        let (_dir, mut supervisor, _state) = test_supervisor();
        let deliver_id = Uuid::now_v7();
        supervisor.on_deliver(canterlot_proto::worker::ipc::Deliver {
            command: Some(Command {
                command_id: deliver_id.to_string(),
                dispatch_attempt: 1,
                execution_id: supervisor.spec.execution_id.to_string(),
                execution_generation: supervisor.spec.generation,
                kind: Some(Kind::DeliverInput(DeliverInput {
                    input_id: Uuid::now_v7().to_string(),
                    conversation_id: supervisor.spec.conversation_id.to_string(),
                    content: vec![ContentBlock {
                        block: Some(canterlot_proto::worker::v1::content_block::Block::Text(
                            TextBlock {
                                text: "отредактировано на хабе".into(),
                            },
                        )),
                    }],
                    settings: None,
                })),
            }),
            files: Vec::new(),
        });
        let stored = supervisor
            .inputs
            .get(&(deliver_id, 1))
            .expect("delivered input is stored");
        assert_eq!(stored.text, "отредактировано на хабе");
        assert!(matches!(stored.state, InputState::Queued));
    }

    #[tokio::test]
    async fn steer_into_running_turn_is_accepted_without_second_turn() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().steer_accept = true;
        // Ход идёт: ввод передан, ход активен.
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert_eq!(supervisor.active_turn, Some(first.0));
        // Вливание второго ввода в тот же ход.
        let deliver = steer_deliver(&supervisor, first.0, "send now");
        let key = steer_key(&deliver);
        supervisor.on_steer(deliver).await.unwrap();
        // Доказательство записано, ход тот же, сессия бежит.
        assert!(supervisor.inputs[&key].accepted);
        assert!(matches!(supervisor.inputs[&key].state, InputState::Done));
        assert_eq!(supervisor.active_turn, Some(first.0));
        assert!(matches!(supervisor.lifecycle, Lifecycle::Running));
        let bodies = shipped_bodies(&mut supervisor);
        assert!(
            bodies.iter().any(|body| matches!(
                body,
                Body::InputAccepted(accepted) if accepted.command_id == key.0.to_string()
            )),
            "steer proof shipped: {bodies:?}"
        );
        assert!(
            !bodies.iter().any(|body| matches!(
                body,
                Body::TransmissionStarted(started) if started.command_id == key.0.to_string()
            )) && !bodies
                .iter()
                .any(|body| matches!(body, Body::DispatchDeferred(_) | Body::InputRejected(_))),
            "no second turn, no refusal: {bodies:?}"
        );
        // Конец хода закрывает его как обычно, влитый ввод историю не ломает.
        supervisor
            .on_adapter_event(AdapterEvent::RunFinished {
                input_command_id: first.0,
                usage: Vec::new(),
                context_tokens: 0,
                context_window_tokens: 0,
            })
            .await
            .unwrap();
        assert!(supervisor.active_turn.is_none());
    }

    #[tokio::test]
    async fn steer_without_turn_stays_queued_for_next_turn() {
        let (_dir, mut supervisor, _state) = test_supervisor();
        let running = Uuid::now_v7();
        let deliver = steer_deliver(&supervisor, running, "too late");
        let key = steer_key(&deliver);
        supervisor.on_steer(deliver).await.unwrap();
        // Хода нет: ввод не записан, хаб вернёт его в очередь следующим ходом.
        assert!(!supervisor.inputs.contains_key(&key));
        let bodies = shipped_bodies(&mut supervisor);
        assert!(
            bodies.iter().any(|body| matches!(
                body,
                Body::DispatchDeferred(deferred)
                    if deferred.command_id == key.0.to_string() && deferred.reason == "steer_no_turn"
            )),
            "steer defers without taking the input: {bodies:?}"
        );
    }

    #[tokio::test]
    async fn steer_into_wrong_turn_stays_queued() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().steer_accept = true;
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        // Ход другой команды: вливать некуда.
        let deliver = steer_deliver(&supervisor, Uuid::now_v7(), "stale");
        let key = steer_key(&deliver);
        supervisor.on_steer(deliver).await.unwrap();
        assert!(!supervisor.inputs.contains_key(&key));
        assert_eq!(supervisor.active_turn, Some(first.0));
        let bodies = shipped_bodies(&mut supervisor);
        assert!(bodies.iter().any(|body| matches!(
            body,
            Body::DispatchDeferred(deferred)
                if deferred.command_id == key.0.to_string() && deferred.reason == "steer_no_turn"
        )));
        assert_eq!(state.lock().unwrap().transmit_calls, vec![first.0]);
    }

    #[tokio::test]
    async fn steer_during_open_request_or_compaction_defers() {
        let (_dir, mut supervisor, _state) = test_supervisor();
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        // Открытый запрос: вливать некуда, итог хода решит владелец.
        supervisor
            .on_adapter_event(AdapterEvent::RequestOpened {
                request_id: "req-1".into(),
                kind: "permission".into(),
                payload: serde_json::json!({"tool": "Bash"}),
            })
            .await
            .unwrap();
        let deliver = steer_deliver(&supervisor, first.0, "during prompt");
        supervisor.on_steer(deliver).await.unwrap();
        let bodies = shipped_bodies(&mut supervisor);
        assert!(bodies.iter().any(|body| matches!(
            body,
            Body::DispatchDeferred(deferred) if deferred.reason == "steer_request_open"
        )));
        supervisor
            .on_adapter_event(AdapterEvent::RequestClosed {
                request_id: "req-1".into(),
                reason: "answered".into(),
            })
            .await
            .unwrap();
        // Сжатие: тот же отказ, ввод остаётся в очереди.
        supervisor.pending_compaction = Some(("compact-1".into(), 1));
        let deliver = steer_deliver(&supervisor, first.0, "during compact");
        supervisor.on_steer(deliver).await.unwrap();
        let bodies = shipped_bodies(&mut supervisor);
        assert!(bodies.iter().any(|body| matches!(
            body,
            Body::DispatchDeferred(deferred) if deferred.reason == "steer_compacting"
        )));
    }

    #[tokio::test]
    async fn steer_undetermined_resolves_by_late_proof() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().steer_undetermined = true;
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        let deliver = steer_deliver(&supervisor, first.0, "unconfirmed");
        let key = steer_key(&deliver);
        supervisor.on_steer(deliver).await.unwrap();
        assert!(matches!(
            supervisor.inputs[&key].state,
            InputState::Transmitting
        ));
        assert!(
            supervisor.reported.contains(&(key.0, key.1, "unconfirmed")),
            "late proof is awaited, not dropped"
        );
        // Ход при этом чужой и продолжается.
        assert_eq!(supervisor.active_turn, Some(first.0));
        // Позднее доказательство закрывает вливание, ход не трогает.
        supervisor
            .on_adapter_event(AdapterEvent::InputAccepted { command_id: key.0 })
            .await
            .unwrap();
        assert!(supervisor.inputs[&key].accepted);
        assert_eq!(supervisor.active_turn, Some(first.0));
        let bodies = shipped_bodies(&mut supervisor);
        assert!(bodies.iter().any(|body| matches!(
            body,
            Body::InputAccepted(accepted) if accepted.command_id == key.0.to_string()
        )));
    }

    fn queued_share() -> (Uuid, Uuid, PendingShare) {
        let share_id = Uuid::now_v7();
        let call_id = Uuid::now_v7();
        let pending = PendingShare {
            call_id,
            block_id: Uuid::now_v7(),
            turn: None,
            prepared: PreparedShare {
                id: share_id,
                name: "doc.txt".into(),
                title: String::new(),
                note: String::new(),
                mime: "text/plain".into(),
                size: 6,
                sha256: "e".repeat(64),
                width: None,
                height: None,
            },
        };
        (share_id, call_id, pending)
    }

    #[tokio::test]
    async fn reattach_restarts_share_upload_like_media() {
        let (_dir, mut supervisor, _state) = test_supervisor();
        // Прерванная загрузка: очередь и полёт, как при обрыве связи посередине.
        let (share_id, call_id, pending) = queued_share();
        let block_id = pending.block_id;
        supervisor.shares.ready.push_back(pending);
        supervisor.shares.inflight = Some(share_id);
        supervisor.media.inflight = Some(Uuid::now_v7());
        let (old_tx, _old_rx) = mpsc::channel(64);
        supervisor.conns.insert(
            0,
            Conn {
                tx: old_tx,
                attached: true,
            },
        );
        supervisor.attached = Some(0);
        supervisor.drop_connection(0);
        assert!(
            supervisor.shares.inflight.is_none(),
            "interrupted share upload leaves flight"
        );
        assert!(supervisor.media.inflight.is_none());
        assert_eq!(
            supervisor.shares.ready.len(),
            1,
            "queued share survives disconnect"
        );
        // Новая служба: загрузка уходит заново тем же блоком.
        let (tx, mut rx) = mpsc::channel(64);
        supervisor.conns.insert(
            1,
            Conn {
                tx,
                attached: false,
            },
        );
        let attach = canterlot_proto::worker::ipc::control_to_supervisor::Msg::Attach(
            canterlot_proto::worker::ipc::Attach {
                protocol_version: IPC_PROTOCOL_VERSION,
                secret: Vec::new(),
                execution_id: supervisor.spec.execution_id.to_string(),
                generation: supervisor.spec.generation,
            },
        );
        supervisor
            .on_control(Control::Message(
                1,
                ControlToSupervisor { msg: Some(attach) },
            ))
            .await
            .unwrap();
        assert_eq!(supervisor.shares.inflight, Some(share_id));
        let mut upload = None;
        for _ in 0..4 {
            match rx.recv().await.expect("share upload resent").msg {
                Some(up::Msg::ShareUpload(request)) => {
                    upload = Some(request);
                    break;
                }
                // Приложенный ответ о присоединении идёт первым.
                Some(up::Msg::Attached(_)) => {}
                other => panic!("expected ShareUpload, got {other:?}"),
            }
        }
        let upload = upload.expect("share upload resent");
        assert_eq!(upload.share_id, share_id.to_string());
        assert_eq!(upload.call_id, call_id.to_string());
        assert_eq!(upload.block_id, block_id.to_string());
    }

    #[test]
    fn share_transfer_deadline_scales_with_size_and_caps() {
        assert_eq!(share_transfer_timeout(0), Duration::from_secs(60));
        assert_eq!(
            share_transfer_timeout(10 * 1024 * 1024),
            Duration::from_secs(80)
        );
        assert_eq!(
            share_transfer_timeout(u64::MAX),
            Duration::from_secs(15 * 60)
        );
    }

    #[tokio::test]
    async fn large_share_upload_gets_a_scaled_deadline_and_success_is_honoured() {
        let (_dir, mut supervisor, state) = test_supervisor();
        let (share_id, call_id, mut pending) = queued_share();
        pending.prepared.size = 300 * 1024 * 1024;
        supervisor.shares.ready.push_back(pending);
        supervisor
            .agent_calls
            .insert(call_id, Instant::now() + SHARE_PREPARE_TIMEOUT);
        let (tx, mut rx) = mpsc::channel(64);
        supervisor.conns.insert(0, Conn { tx, attached: true });
        supervisor.attached = Some(0);
        supervisor.pump_share();
        assert!(
            supervisor.agent_calls[&call_id] > Instant::now() + SHARE_TRANSFER_BASE,
            "the transfer deadline scales with the file size"
        );
        match rx.recv().await.expect("share upload").msg {
            Some(up::Msg::ShareUpload(request)) => {
                assert_eq!(request.share_id, share_id.to_string());
            }
            other => panic!("expected ShareUpload, got {other:?}"),
        }
        // Успех до истечения срока всегда принимается.
        supervisor
            .on_share_result(canterlot_proto::worker::ipc::ShareResult {
                share_id: share_id.to_string(),
                stored: true,
                reason: String::new(),
            })
            .await
            .unwrap();
        assert!(supervisor.shares.ready.is_empty());
        assert!(!supervisor.agent_calls.contains_key(&call_id));
        assert!(
            state
                .lock()
                .unwrap()
                .agent_call_errors
                .iter()
                .any(|(id, code)| *id == call_id && code == "ok"),
            "the agent is told the file was shared"
        );
    }

    #[tokio::test]
    async fn shared_file_block_stays_in_the_active_turn_as_the_main_agent() {
        let (_dir, mut supervisor, _state) = test_supervisor();
        let turn = Uuid::now_v7();
        let (share_id, call_id, mut pending) = queued_share();
        // Ход запомнен при вызове; к концу загрузки он может уже закончиться.
        pending.turn = Some(turn);
        let block_id = pending.block_id;
        supervisor.shares.ready.push_back(pending);
        supervisor.shares.inflight = Some(share_id);
        supervisor
            .agent_calls
            .insert(call_id, Instant::now() + SHARE_PREPARE_TIMEOUT);
        supervisor
            .on_share_result(canterlot_proto::worker::ipc::ShareResult {
                share_id: share_id.to_string(),
                stored: true,
                reason: String::new(),
            })
            .await
            .unwrap();
        let bodies = shipped_bodies(&mut supervisor);
        let started = bodies
            .iter()
            .find_map(|body| match body {
                Body::BlockStarted(started) if started.block_id == block_id.to_string() => {
                    Some(started.clone())
                }
                _ => None,
            })
            .expect("carrier block recorded");
        assert_eq!(started.input_command_id, turn.to_string());
        assert_eq!(started.agent, "orchestrator");
        assert_eq!(started.tool_name, "share_file");
        assert!(bodies.iter().any(|body| matches!(
            body,
            Body::BlockFile(file) if file.block_id == block_id.to_string()
                && file.attachment_id == share_id.to_string()
        )));
    }

    #[tokio::test]
    async fn expired_share_upload_fails_the_call_and_deletes_the_copy() {
        let (_dir, mut supervisor, state) = test_supervisor();
        let (share_id, call_id, pending) = queued_share();
        supervisor.shares.ready.push_back(pending);
        supervisor.shares.inflight = Some(share_id);
        supervisor
            .agent_calls
            .insert(call_id, Instant::now() - Duration::from_secs(1));
        let (tx, mut rx) = mpsc::channel(8);
        supervisor.conns.insert(0, Conn { tx, attached: true });
        supervisor.attached = Some(0);
        let copy = media::media_path(&supervisor.exec_dir, share_id);
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
        std::fs::write(&copy, b"shared").unwrap();
        supervisor.expire_agent_calls().await;
        assert!(
            supervisor.shares.ready.is_empty(),
            "expired share leaves the queue"
        );
        assert!(
            supervisor.shares.inflight.is_none(),
            "expired share leaves flight"
        );
        assert!(!copy.exists(), "expired share copy is deleted");
        assert!(
            state
                .lock()
                .unwrap()
                .agent_call_errors
                .iter()
                .any(|(id, code)| *id == call_id && code == "hub_timeout"),
            "the agent gets a bounded error, not silence"
        );
        match rx.recv().await.expect("share cancellation").msg {
            Some(up::Msg::ShareCancel(cancel)) => {
                assert_eq!(cancel.share_id, share_id.to_string());
            }
            other => panic!("expected ShareCancel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acknowledged_interrupt_is_turn_aborted_not_session_stopped() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().ack_interrupt = true;
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert_eq!(supervisor.active_turn, Some(first.0));
        supervisor.on_interrupt("int-1".into(), 1).await.unwrap();
        assert!(supervisor.aborting_turn, "the turn is being aborted");
        assert!(
            matches!(supervisor.lifecycle, Lifecycle::Running),
            "no stop while the process survives"
        );
        // Прерванный ход закончился кадром итога: сессия idle, остановка не начата.
        supervisor
            .on_adapter_event(AdapterEvent::RunFinished {
                input_command_id: first.0,
                usage: Vec::new(),
                context_tokens: 0,
                context_window_tokens: 0,
            })
            .await
            .unwrap();
        assert!(supervisor.active_turn.is_none());
        assert!(!supervisor.aborting_turn, "the abort ended with its turn");
        assert!(
            matches!(supervisor.lifecycle, Lifecycle::Running),
            "the execution stays running for the next input"
        );
        assert!(!supervisor.adapter_exited);
        // CLI готов к следующему вводу той же фазой, что после обычного хода.
        supervisor
            .on_adapter_event(AdapterEvent::Phase {
                phase: crate::adapter::Phase::AwaitingInput,
                tool_name: None,
                summary: None,
            })
            .await
            .unwrap();
        // Следующий ввод уходит в тот же процесс без перезапуска.
        let second = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert_eq!(supervisor.active_turn, Some(second.0));
        assert_eq!(
            state.lock().unwrap().transmit_calls,
            vec![first.0, second.0]
        );
    }

    fn permission_opened(request_id: &str) -> AdapterEvent {
        AdapterEvent::RequestOpened {
            request_id: request_id.into(),
            kind: "permission".into(),
            payload: serde_json::json!({"tool_name": "Bash", "input": {"command": "ls"}}),
        }
    }

    #[tokio::test]
    async fn request_opened_during_an_interrupt_is_refused_to_the_cli() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().ack_interrupt = true;
        queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        supervisor.on_interrupt("int-1".into(), 1).await.unwrap();
        supervisor
            .on_adapter_event(permission_opened("req-late"))
            .await
            .unwrap();
        assert!(!supervisor.sj.has_open_requests());
        assert_eq!(state.lock().unwrap().refused, vec!["req-late".to_owned()]);
    }

    #[tokio::test]
    async fn interrupt_not_applicable_leaves_the_turn_answerable() {
        let (_dir, mut supervisor, state) = test_supervisor();
        queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        supervisor
            .on_adapter_event(permission_opened("req-1"))
            .await
            .unwrap();
        supervisor.on_interrupt("int-1".into(), 1).await.unwrap();
        assert!(!supervisor.aborting_turn, "nothing is being aborted");
        assert!(
            supervisor.sj.request("req-1").is_some() && supervisor.sj.has_open_requests(),
            "the request stays open for the owner"
        );
        assert!(state.lock().unwrap().refused.is_empty());
    }

    #[tokio::test]
    async fn interrupt_mark_does_not_outlive_the_next_turn() {
        let (_dir, mut supervisor, state) = test_supervisor();
        state.lock().unwrap().ack_interrupt = true;
        let first = queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        supervisor.on_interrupt("int-1".into(), 1).await.unwrap();
        assert!(supervisor.interrupted);
        supervisor
            .on_adapter_event(AdapterEvent::RunFinished {
                input_command_id: first.0,
                usage: Vec::new(),
                context_tokens: 0,
                context_window_tokens: 0,
            })
            .await
            .unwrap();
        assert!(
            supervisor.interrupted,
            "an exit right after it is still a user stop"
        );
        supervisor
            .on_adapter_event(AdapterEvent::Phase {
                phase: crate::adapter::Phase::AwaitingInput,
                tool_name: None,
                summary: None,
            })
            .await
            .unwrap();
        queue_owner(&mut supervisor);
        supervisor.try_transmit().await.unwrap();
        assert!(
            !supervisor.interrupted,
            "a later clean exit is a normal exit"
        );
    }

    #[tokio::test]
    async fn compaction_without_compact_v1_is_not_applicable() {
        let (_dir, mut supervisor, state) = test_supervisor();
        supervisor.compact_v1 = false;
        let id = Uuid::now_v7().to_string();
        supervisor.on_compact(id.clone(), 1).await.unwrap();
        assert_eq!(
            state.lock().unwrap().compact_calls,
            0,
            "never sent to the agent"
        );
        assert!(supervisor.pending_compaction.is_none());
        let bodies = shipped_bodies(&mut supervisor);
        assert!(
            bodies.iter().any(|body| matches!(
                body,
                Body::CompactionStatus(status)
                    if status.command_id == id
                        && status.state == i32::from(CompactionState::NotApplicable)
            )),
            "{bodies:?}"
        );
    }
}
