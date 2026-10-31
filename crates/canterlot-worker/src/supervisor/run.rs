use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
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
            AdapterDiagnostic, BlockDelta, BlockFinal, BlockIncomplete, BlockMedia, BlockStarted,
            DecisionApplied, DecisionNotApplied, DecisionTransmissionStarted, DecisionUnconfirmed,
            DeliverDecision, InputAcceptanceUnconfirmed, InputAccepted, InputCancelLost,
            InputCancelWon, InputDispatchDeferred, InputRejected, InputTransmissionStarted,
            InputWithdrawn, InterruptState, InterruptStatus, JournalClosed, JournalOpened,
            PhaseChanged, RequestClosed, RequestOpened, RunFinished, TodoItem as ProtoTodoItem,
            TodoStatus as ProtoTodoStatus, TodoUpdated, content_block, record_body::Body,
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
    bounds::{MAX_REQUEST_ID_BYTES, bounded_json, bounded_record, bounded_todo},
    ipc::{IpcConnection, IpcServer, constant_time_eq, read_message, write_message},
    sj::Sj,
};
use crate::{
    adapter::{
        Adapter, AdapterContext, AdapterEvent, AgentCallResult, CloseMode, DecisionOutcome,
        InstructionSpec, InterruptOutcome, LimitFact, LocalFile, MediaSource, Phase,
        SettingsChange, StartSpec, TodoStatus, Transmission, TransmitOutcome, registry_with,
    },
    containment::{Containment, ContainmentError, Member, Termination, TreeContainment},
    media::{self, Prepared, Skip},
};

/// Картинки в подготовке и ожидании загрузки на одно исполнение; сверх этого событие пропускается.
const MAX_PENDING_MEDIA: usize = 32;
/// Ожидание ответа хаба на вызов инструмента агента.
const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(20);
/// Период уборки зомби усыновлённых процессов: они занимают лимит процессов владельца.
const ORPHAN_REAP_PERIOD: Duration = Duration::from_secs(30);

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
    /// Потерю списка задач без возможности хаба показываем один раз, а не на каждое обновление.
    todo_drop_reported: bool,
    provider: String,
    open_blocks: BTreeMap<Uuid, OpenBlock>,
    known_interrupts: HashSet<(String, u32)>,
    pending_interrupts: Vec<(String, u32)>,
    interrupted: bool,
    aborting_turn: bool,
    /// Вызовы инструментов агента в ожидании ответа хаба: срок ответа.
    agent_calls: HashMap<Uuid, Instant>,
    conns: HashMap<u64, Conn>,
    attached: Option<u64>,
    next_conn: u64,
    attach_deadline: Option<Instant>,
    workdir: Option<PathBuf>,
    media: MediaQueue,
    media_jobs: mpsc::UnboundedSender<MediaJob>,
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

fn safe_event_type(raw: &str) -> String {
    raw.chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn input_text(command: &canterlot_proto::worker::v1::Command) -> Option<String> {
    let Some(canterlot_proto::worker::v1::command::Kind::DeliverInput(input)) = &command.kind
    else {
        return None;
    };
    Some(
        input
            .content
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

pub(super) async fn run(exec_dir: PathBuf) -> Result<ExitCode, SupervisorError> {
    let (spec, secret) = super::load(&exec_dir)?;
    match rustix::process::setsid() {
        Ok(_) | Err(rustix::io::Errno::PERM) => {}
        Err(error) => return Err(std::io::Error::from(error).into()),
    }
    let containment: Arc<dyn Containment> = Arc::new(TreeContainment::new()?);
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
        todo_drop_reported: false,
        provider: String::new(),
        open_blocks: BTreeMap::new(),
        known_interrupts: HashSet::new(),
        pending_interrupts: Vec::new(),
        interrupted: false,
        aborting_turn: false,
        agent_calls: HashMap::new(),
        conns: HashMap::new(),
        attached: None,
        next_conn: 0,
        workdir: None,
        media: MediaQueue::default(),
        media_jobs,
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
    match supervisor.event_loop(server, prepared).await {
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
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Control>();
        loop {
            if let Some(code) = self.after_event().await? {
                accept_task.abort();
                reaper.abort();
                return Ok(code);
            }
            let wake = self.next_wake();
            tokio::select! {
                Some(connection) = accepted.recv() => self.on_connection(connection, &control_tx),
                Some(control) = control_rx.recv() => self.on_control(control).await?,
                Some(result) = prepared.recv() => self.on_media_prepared(result),
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
            }));
            tracing::info!(kind = "attached", "control attached");
            // Новая управляющая служба получает незавершённую загрузку заново.
            self.media.inflight = None;
            self.pump_media();
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
            down::Msg::Deliver(deliver) => self.on_deliver(deliver),
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
            down::Msg::Stop(stop) => self.on_stop(stop.force).await?,
            down::Msg::ShipFrom(ship) => self.sj.enable_ship(ship.committed_seq),
            down::Msg::SjAck(ack) => self.sj.ack(ack.up_to_seq)?,
            down::Msg::MediaResult(result) => self.on_media_result(result)?,
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

    fn on_media(&mut self, block_id: Uuid, source: MediaSource) -> Result<(), SupervisorError> {
        if !self.pilot_v1 {
            self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: self.spec.execution_id.to_string(),
                kind: "capability_missing".into(),
                event_type: "media_dropped".into(),
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
            exec_dir: self.exec_dir.clone(),
            claude_code: self.spec.claude_code.clone(),
            codex: self.spec.codex.clone(),
            opencode: self.spec.opencode.clone(),
        };
        let Some(mut adapter) = registry_with(&start.profile, &context) else {
            tracing::warn!(
                kind = "start_failed",
                reason = "unknown_profile",
                "adapter start failed"
            );
            return self.begin_stop(StopCause::StartFailed, false);
        };
        adapter.set_stop_grace(timers.stop_grace);
        self.pilot_v1 = start.pilot_v1;
        self.todo_v1 = start.todo_v1;
        self.workdir = Some(PathBuf::from(&start.workdir));
        self.provider = canterlot_proto::harness::provider_of(&start.profile)
            .unwrap_or_default()
            .into();
        let Some(instructions) = start.instructions.as_ref() else {
            // Хаб всегда присылает инструкции (план 2c, задача 3): их отсутствие —
            // старый хаб, молчаливый откат на конфигурацию владельца запрещён.
            tracing::warn!(
                kind = "start_failed",
                reason = "instructions_missing",
                "adapter start failed"
            );
            return self.begin_stop(StopCause::InvalidInstructions, false);
        };
        let instructions = match InstructionSpec::from_proto(Some(instructions)) {
            Ok(instructions) => instructions,
            Err(_) => {
                tracing::warn!(
                    kind = "start_failed",
                    reason = "invalid_instructions",
                    "adapter start failed"
                );
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
                let cause = match other {
                    Ok(Err(crate::adapter::AdapterError::StartFailed("cli_too_old"))) => {
                        StopCause::CliIncompatible
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
        let settings = match &command.kind {
            Some(canterlot_proto::worker::v1::command::Kind::DeliverInput(input)) => input
                .settings
                .as_ref()
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
                .transpose(),
            _ => Ok(None),
        };
        let Ok(settings) = settings else {
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

    async fn try_transmit(&mut self) -> Result<(), SupervisorError> {
        if !matches!(self.lifecycle, Lifecycle::Running)
            || self.adapter_exited
            || !self.ready
            || self.active_turn.is_some()
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
            self.faults.abort_at("before_first_byte");
            self.ready = false;
            self.aborting_turn = false;
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
                    request_id,
                    execution_id: self.spec.execution_id.to_string(),
                    generation: self.spec.generation,
                    kind,
                    payload_json,
                }))?;
                if self.aborting_turn {
                    self.close_requests("interrupted")?;
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
                    context_tokens,
                    context_window_tokens,
                    duration_ms,
                    machine_time_ms: now_ms(),
                }))?;
                if ours {
                    self.active_turn = None;
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
            AdapterEvent::ContextExhausted => {
                self.begin_stop(StopCause::ContextExhausted, false)?;
            }
            AdapterEvent::Diagnostic {
                kind,
                event_type,
                required,
                ..
            } => {
                self.record(Body::AdapterDiagnostic(AdapterDiagnostic {
                    execution_id: self.spec.execution_id.to_string(),
                    kind: if required {
                        "unknown_required_event"
                    } else {
                        "unknown_optional_event"
                    }
                    .into(),
                    event_type: safe_event_type(
                        if matches!(kind, "unknown_event" | "provider_failure") {
                            &event_type
                        } else {
                            kind
                        },
                    ),
                }))?;
                if required {
                    self.begin_stop(
                        match kind {
                            "auth_required" => StopCause::AuthRequired,
                            "cli_too_old" | "session_id_mismatch" => StopCause::CliIncompatible,
                            "mcp_unavailable" | "restart_failed" => StopCause::StartFailed,
                            _ => StopCause::ProtocolIncompatible,
                        },
                        false,
                    )?;
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
                self.agent_calls
                    .insert(call_id, Instant::now() + AGENT_CALL_TIMEOUT);
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
        self.close_requests("interrupted")?;
        self.sj.flush()?;
        let Some(adapter) = self.adapter.as_mut() else {
            return Ok(());
        };
        match adapter.interrupt().await {
            InterruptOutcome::Acknowledged => {
                self.interrupted = true;
                self.interrupt_record(&id, attempt, InterruptState::Acknowledged)?;
                self.pending_interrupts.push((id, attempt));
            }
            InterruptOutcome::Undetermined => {
                self.interrupted = true;
                self.pending_interrupts.push((id, attempt));
            }
            InterruptOutcome::NotApplicable => {
                self.interrupt_record(&id, attempt, InterruptState::NotApplicable)?;
            }
        }
        Ok(())
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
