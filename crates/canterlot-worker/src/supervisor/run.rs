use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use canterlot_proto::{
    limits::{BLOCK_FINAL_INLINE_MAX_BYTES, IPC_PROTOCOL_VERSION, MAX_BLOCK_DELTA_BYTES},
    worker::{
        ipc::{
            Attached, CliStarted, ControlToSupervisor, DeliverAck, Exited, ReceivedInput,
            SupervisorToControl, control_to_supervisor as down, supervisor_to_control as up,
        },
        v1::{
            AdapterDiagnostic, BlockDelta, BlockFinal, BlockIncomplete, BlockStarted,
            InputAcceptanceUnconfirmed, InputAccepted, InputCancelLost, InputCancelWon,
            InputDispatchDeferred, InputRejected, InputTransmissionStarted, InputWithdrawn,
            InterruptState, InterruptStatus, JournalClosed, JournalOpened, PhaseChanged,
            RunFinished, content_block, record_body::Body,
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
    ipc::{IpcConnection, IpcServer, constant_time_eq, read_message, write_message},
    sj::Sj,
};
use crate::{
    adapter::{
        Adapter, AdapterEvent, CloseMode, InterruptOutcome, Phase, StartSpec, Transmission,
        TransmitOutcome, registry_with_deadline,
    },
    containment::{Containment, ContainmentError, Member, Termination, TreeContainment},
};

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
}

#[derive(Clone)]
enum StopCause {
    User {
        force: bool,
    },
    StartCancelled,
    StartFailed,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    ProtocolIncompatible,
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
    Draining { deadline: Instant, outcome: String },
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
    terminate: Option<JoinHandle<Result<Termination, ContainmentError>>>,
    inputs: HashMap<(Uuid, u32), Input>,
    queue: VecDeque<(Uuid, u32)>,
    transmitted: HashSet<Uuid>,
    cancel_marks: HashMap<Uuid, u32>,
    reported: HashSet<(Uuid, u32, &'static str)>,
    active_turn: Option<Uuid>,
    open_blocks: BTreeMap<Uuid, u64>,
    known_interrupts: HashSet<(String, u32)>,
    pending_interrupts: Vec<(String, u32)>,
    interrupted: bool,
    conns: HashMap<u64, Conn>,
    attached: Option<u64>,
    next_conn: u64,
    attach_deadline: Option<Instant>,
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
                block
                    .block
                    .as_ref()
                    .map(|content_block::Block::Text(text)| text.text.as_str())
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
    let server = IpcServer::bind(&exec_dir, secret.clone()).await?;
    let pid = std::process::id();
    let start_ticks = crate::containment::start_ticks(pid)?;
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
        start_attempted: false,
        started: None,
        ready: false,
        lifecycle: Lifecycle::Starting,
        terminate: None,
        inputs: HashMap::new(),
        queue: VecDeque::new(),
        transmitted: HashSet::new(),
        cancel_marks: HashMap::new(),
        reported: HashSet::new(),
        active_turn: None,
        open_blocks: BTreeMap::new(),
        known_interrupts: HashSet::new(),
        pending_interrupts: Vec::new(),
        interrupted: false,
        conns: HashMap::new(),
        attached: None,
        next_conn: 0,
    };
    supervisor.record(Body::JournalOpened(JournalOpened {
        execution_id: supervisor.spec.execution_id.to_string(),
        generation: supervisor.spec.generation,
        supervisor_protocol_version: IPC_PROTOCOL_VERSION,
    }))?;
    supervisor.state_record("starting", "", "")?;
    tracing::info!(execution_id = %supervisor.spec.execution_id, journal_id = %supervisor.sj.journal_id(), "supervisor started");
    match supervisor.event_loop(server).await {
        Ok(code) => Ok(code),
        Err(error) => {
            tracing::error!(error_kind = ?std::mem::discriminant(&error), "supervisor failed");
            let _ = containment.terminate(Duration::from_millis(200)).await;
            Err(error)
        }
    }
}

async fn adapter_event(adapter: &mut Option<Box<dyn Adapter>>, gone: bool) -> AdapterEvent {
    match adapter {
        Some(adapter) if !gone => adapter.next_event().await,
        _ => std::future::pending().await,
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
        self.sj.append(body)
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

    async fn event_loop(&mut self, server: IpcServer) -> Result<ExitCode, SupervisorError> {
        let (accepted_tx, mut accepted) = mpsc::channel::<IpcConnection>(8);
        tokio::spawn(async move {
            while let Ok(connection) = server.accept().await {
                if accepted_tx.send(connection).await.is_err() {
                    break;
                }
            }
        });
        let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Control>();
        loop {
            if let Some(code) = self.after_event().await? {
                return Ok(code);
            }
            let wake = self.next_wake();
            tokio::select! {
                Some(connection) = accepted.recv() => self.on_connection(connection, &control_tx),
                Some(control) = control_rx.recv() => self.on_control(control).await?,
                event = adapter_event(&mut self.adapter, self.adapter_exited) => self.on_adapter_event(event).await?,
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
            Lifecycle::Stopping(state) if self.terminate.is_none() => consider(state.next_round),
            Lifecycle::Draining { deadline, .. } => consider(*deadline),
            _ => {}
        }
        if !self.queue.is_empty() && self.faults.gate_held(&self.exec_dir) {
            consider(Instant::now() + Duration::from_millis(25));
        }
        if let Some(at) = self.attach_deadline.filter(|_| self.attached.is_none()) {
            consider(at);
        }
        wake
    }

    async fn after_event(&mut self) -> Result<Option<ExitCode>, SupervisorError> {
        self.drive_stop().await?;
        self.try_transmit().await?;
        self.pump_shipping();
        if let Lifecycle::Draining { deadline, outcome } = &self.lifecycle {
            let waiting = self.attached.is_some() && self.sj.shipping() && !self.sj.fully_acked();
            if !waiting || Instant::now() >= *deadline {
                let reason = outcome.clone();
                self.send(up::Msg::Exited(Exited { reason }));
                self.conns.clear();
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(Some(ExitCode::SUCCESS));
            }
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
            return Ok(());
        }
        match message {
            down::Msg::Attach(_) => self.drop_connection(id),
            down::Msg::StartCli(start) => self.start_cli(start).await?,
            down::Msg::Deliver(deliver) => self.on_deliver(deliver.command),
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
        }
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
        let Some(mut adapter) = registry_with_deadline(&start.profile, timers.transmit_deadline)
        else {
            tracing::warn!(
                kind = "start_failed",
                reason = "unknown_profile",
                "adapter start failed"
            );
            return self.begin_stop(StopCause::StartFailed, false);
        };
        let spec = StartSpec {
            execution_id: self.spec.execution_id,
            generation: self.spec.generation,
            workdir: PathBuf::from(&start.workdir),
            native_conversation_id: (!start.native_conversation_id.is_empty())
                .then(|| start.native_conversation_id.clone()),
            profile: start.profile.clone(),
        };
        let outcome = timeout(
            timers.ready_deadline,
            adapter.start(&spec, &*self.containment),
        )
        .await;
        let Ok(Ok(started)) = outcome else {
            tracing::warn!(
                kind = "start_failed",
                reason = "adapter_start",
                "adapter start failed"
            );
            return self.begin_stop(StopCause::StartFailed, false);
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
        }))?;
        Ok(())
    }

    fn on_deliver(&mut self, command: Option<canterlot_proto::worker::v1::Command>) {
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
        let attempt = command.dispatch_attempt;
        let key = (id, attempt);
        let result = if command.execution_id != self.spec.execution_id.to_string()
            || command.execution_generation != self.spec.generation
        {
            "generation_mismatch"
        } else if matches!(
            self.lifecycle,
            Lifecycle::Stopping(_) | Lifecycle::Draining { .. }
        ) {
            "execution_stopped"
        } else if self.inputs.contains_key(&key) || self.transmitted.contains(&id) {
            "duplicate"
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
                },
            );
            self.queue.push_back(key);
            "accepted_for_gate"
        };
        self.send(ack(command.command_id, attempt, result));
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
            let text = std::mem::take(&mut input.text);
            input.state = InputState::Transmitting;
            self.transmitted.insert(key.0);
            self.record(Body::TransmissionStarted(InputTransmissionStarted {
                command_id: key.0.to_string(),
                dispatch_attempt: key.1,
                machine_time_ms: now_ms(),
            }))?;
            self.faults.abort_at("before_first_byte");
            self.ready = false;
            self.active_turn = Some(key.0);
            let transmission = Transmission {
                command_id: key.0,
                attempt: key.1,
                text,
            };
            let Some(adapter) = self.adapter.as_mut() else {
                break;
            };
            let outcome = adapter.transmit(&transmission).await;
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
            AdapterEvent::Phase(phase) => {
                let ready = matches!(phase, Phase::AwaitingInput);
                self.phase_record(phase)?;
                if ready {
                    self.ready = true;
                }
            }
            AdapterEvent::BlockStarted {
                block_id,
                kind,
                input_command_id,
                parent,
            } => {
                self.open_blocks.insert(block_id, 1);
                self.record(Body::BlockStarted(BlockStarted {
                    block_id: block_id.to_string(),
                    rev: 1,
                    kind: snake(&kind),
                    input_command_id: input_command_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    parent_block_id: parent.map(|id| id.to_string()).unwrap_or_default(),
                }))?;
            }
            AdapterEvent::BlockDelta { block_id, append } => {
                let Some(rev) = self.open_blocks.get(&block_id).copied() else {
                    return Ok(());
                };
                let mut rev = rev;
                let mut rest = append.as_str();
                while !rest.is_empty() {
                    let mut end = rest.len().min(MAX_BLOCK_DELTA_BYTES);
                    while !rest.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.record(Body::BlockDelta(BlockDelta {
                        block_id: block_id.to_string(),
                        base_rev: rev,
                        rev: rev + 1,
                        append: rest[..end].to_owned(),
                    }))?;
                    rev += 1;
                    rest = &rest[end..];
                }
                self.open_blocks.insert(block_id, rev);
            }
            AdapterEvent::BlockFinal { block_id, text } => {
                let Some(rev) = self.open_blocks.remove(&block_id) else {
                    return Ok(());
                };
                let omitted = text.len() > BLOCK_FINAL_INLINE_MAX_BYTES;
                self.record(Body::BlockFinal(BlockFinal {
                    block_id: block_id.to_string(),
                    rev: rev + 1,
                    checksum: checksum(&text),
                    text: if omitted { String::new() } else { text },
                    text_omitted: omitted,
                }))?;
            }
            AdapterEvent::RunFinished { input_command_id } => {
                let reason = if self.pending_interrupts.is_empty() {
                    "turn_ended"
                } else {
                    "interrupted"
                };
                self.close_blocks(reason)?;
                self.record(Body::RunFinished(RunFinished {
                    execution_id: self.spec.execution_id.to_string(),
                    input_command_id: input_command_id.to_string(),
                }))?;
                self.active_turn = None;
                for ((command, _), input) in self.inputs.iter_mut() {
                    if *command == input_command_id && input.state == InputState::Transmitting {
                        input.state = InputState::Done;
                    }
                }
                self.complete_interrupts()?;
            }
            AdapterEvent::Diagnostic {
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
                    event_type: safe_event_type(&event_type),
                }))?;
                if required {
                    self.begin_stop(StopCause::ProtocolIncompatible, false)?;
                }
            }
            AdapterEvent::ProcessExited { code, signal } => {
                self.adapter_exited = true;
                self.active_turn = None;
                self.begin_stop(StopCause::Exited { code, signal }, false)?;
            }
        }
        Ok(())
    }

    fn close_blocks(&mut self, reason: &str) -> Result<(), SupervisorError> {
        for (block_id, rev) in std::mem::take(&mut self.open_blocks) {
            self.record(Body::BlockIncomplete(BlockIncomplete {
                block_id: block_id.to_string(),
                rev: rev + 1,
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
                if self.terminate.is_none() && Instant::now() >= state.next_round =>
            {
                (state.force, state.closed)
            }
            _ => return Ok(()),
        };
        if !closed {
            if let Lifecycle::Stopping(state) = &mut self.lifecycle {
                state.closed = true;
            }
            if let Some(adapter) = self.adapter.as_mut() {
                let mode = if force {
                    CloseMode::Force
                } else {
                    CloseMode::Graceful
                };
                let _ = adapter.close(mode).await;
            }
        }
        let grace = if force {
            Duration::from_millis(200)
        } else {
            self.spec.timers.stop_grace
        };
        let fake = self.faults.take_survivor();
        let containment = self.containment.clone();
        self.terminate = Some(tokio::spawn(async move {
            if fake {
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
            _ => 0,
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
            StopCause::ProtocolIncompatible => {
                ("failed", "protocol_incompatible", "protocol_incompatible")
            }
            StopCause::Exited {
                code: Some(0),
                signal: None,
            } => {
                if self.interrupted {
                    ("stopped", "user_stop", "process_exited")
                } else {
                    ("stopped", "normal_exit", "process_exited")
                }
            }
            StopCause::Exited { .. } => ("failed", "crashed", "process_exited"),
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
        self.faults.abort_at("after_stopped_before_closed");
        self.record(Body::JournalClosed(JournalClosed {
            execution_id: self.spec.execution_id.to_string(),
            generation: self.spec.generation,
            reason: outcome.into(),
        }))?;
        self.lifecycle = Lifecycle::Draining {
            deadline: Instant::now() + self.spec.timers.stop_grace,
            outcome: outcome.into(),
        };
        tracing::info!(kind = "execution_closed", outcome, "execution closed");
        Ok(())
    }
}
