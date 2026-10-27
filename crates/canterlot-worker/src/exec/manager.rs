use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use canterlot_proto::{
    limits::IPC_PROTOCOL_VERSION,
    worker::{
        ipc::{
            CliStarted, ControlToSupervisor, Deliver, DeliverAck, InterruptRequest, Ownership,
            OwnershipPhase, ShipFrom, SjAck, SjBatch, StartCli, StopRequest, SupervisorToControl,
            control_to_supervisor as down, supervisor_to_control as up,
        },
        v1::{
            CancelInput, Command, CommandRejected, ExecutionState, InputCancelWon,
            InputDispatchDeferred, InputWithdrawn, InterruptState, InterruptStatus, StartExecution,
            WithdrawInput, command::Kind, record_body::Body,
        },
    },
};
use sha2::{Digest, Sha256};
use tokio::{
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};
use uuid::Uuid;

use super::{ExecError, Shared, SjRelay, open, procfs, spawn};
use crate::{
    adapter,
    supervisor::ipc::{read_message, write_message},
};

const ORPHANED: &str = "owner_lost";
const RECONNECT_EVERY: Duration = Duration::from_millis(200);

pub(crate) enum Msg {
    Command(Command),
    SjAck(u64),
    Replay(u64),
}

pub(super) struct SpawnRequest {
    pub command: Command,
    pub start: StartExecution,
    pub workdir: PathBuf,
    pub conversation_id: Uuid,
}

pub(super) enum Start {
    Spawn(Box<SpawnRequest>),
    Reattach(Box<(spawn::Linked, Ownership)>),
}

enum Up {
    Msg(up::Msg),
    Eof,
}

struct Link {
    write: OwnedWriteHalf,
    up_rx: mpsc::UnboundedReceiver<Up>,
}

pub(super) fn is_orphaned(record: &Ownership) -> bool {
    record.released_reason == ORPHANED && record.phase != OwnershipPhase::Released as i32
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn start_reader(mut read: OwnedReadHalf) -> mpsc::UnboundedReceiver<Up> {
    let (sender, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = sender.closed() => break,
                message = read_message::<SupervisorToControl, _>(&mut read) => match message {
                    Ok(message) => {
                        if let Some(message) = message.msg
                            && sender.send(Up::Msg(message)).is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = sender.send(Up::Eof);
                        break;
                    }
                },
            }
        }
    });
    receiver
}

async fn ownership_update(
    shared: &Shared,
    id: Uuid,
    change: impl FnOnce(&mut Ownership) + Send + 'static,
) -> Result<(), ExecError> {
    shared
        .store
        .call(move |store| {
            let store = open(store)?;
            let Some(mut record) = store.ownership().get(id).cloned() else {
                return Ok(());
            };
            change(&mut record);
            store.ownership_mut()?.append(record)?;
            Ok(())
        })
        .await?;
    Ok(())
}

pub(super) async fn cleanup_released(shared: &Shared) -> Result<(), ExecError> {
    let records = shared
        .store
        .call(|store| Ok(open(store)?.ownership().released()))
        .await?;
    let root = shared.data_dir().await?.join("executions");
    for record in records {
        let Ok(id) = Uuid::parse_str(&record.execution_id) else {
            continue;
        };
        let dir = root.join(id.to_string());
        if Path::new(&record.exec_dir) != dir {
            continue;
        }
        if procfs::is_same_process(record.supervisor_pid, record.supervisor_start_ticks) {
            continue;
        }
        if dir.exists() {
            let spec = fs::read_to_string(dir.join("spec.toml"))
                .ok()
                .and_then(|text| toml::from_str::<crate::supervisor::SupervisorSpec>(&text).ok());
            if let Some(spec) = spec {
                let socket = spec.ipc_socket.unwrap_or_else(|| dir.join("ipc.sock"));
                if socket == Path::new(&record.ipc_socket) {
                    match fs::remove_file(socket) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            fs::remove_dir_all(dir)?;
        }
    }
    Ok(())
}

pub(super) async fn report_owner_lost(
    shared: &Shared,
    id: Uuid,
    generation: u64,
) -> Result<(), ExecError> {
    shared
        .cj(
            vec![Body::ExecutionState(ExecutionState {
                execution_id: id.to_string(),
                generation,
                state: "owner_lost".into(),
                outcome: String::new(),
                native_conversation_id: String::new(),
            })],
            vec![],
        )
        .await?;
    ownership_update(shared, id, |record| {
        if record.released_reason != "owner_lost_reported" {
            record.released_reason = ORPHANED.into();
        }
    })
    .await?;
    shared.with_info(id, |info| {
        info.state = "owner_lost".into();
        info.attached = false;
    });
    Ok(())
}

pub(super) async fn verify(
    shared: &Shared,
    record: &Ownership,
) -> Result<spawn::Linked, ExecError> {
    let id = Uuid::parse_str(&record.execution_id).map_err(|_| ExecError::Verify("id"))?;
    if record.phase != OwnershipPhase::Attached as i32 {
        return Err(ExecError::Verify("phase"));
    }
    if record.boot_id != *shared.boot_id().await? {
        return Err(ExecError::Verify("boot_id"));
    }
    let dir = shared
        .data_dir()
        .await?
        .join("executions")
        .join(id.to_string());
    if Path::new(&record.exec_dir) != dir {
        return Err(ExecError::Verify("exec_dir"));
    }
    if !procfs::is_same_process(record.supervisor_pid, record.supervisor_start_ticks) {
        return Err(ExecError::Verify("supervisor_process"));
    }
    let secret = fs::read(dir.join("secret"))?;
    let spec: crate::supervisor::SupervisorSpec =
        toml::from_str(&fs::read_to_string(dir.join("spec.toml"))?)
            .map_err(|_| ExecError::Verify("spec"))?;
    if Path::new(&record.ipc_socket) != spec.ipc_socket.unwrap_or_else(|| dir.join("ipc.sock")) {
        return Err(ExecError::Verify("ipc_socket"));
    }
    if sha256(&secret) != record.secret_sha256 {
        return Err(ExecError::Verify("secret"));
    }
    let linked = spawn::connect_attach(
        Path::new(&record.ipc_socket),
        &secret,
        id,
        record.generation,
        Some(record.supervisor_pid),
        None,
    )
    .await?;
    let attached = &linked.attached;
    if attached.pid != record.supervisor_pid
        || attached.start_ticks != record.supervisor_start_ticks
        || attached.sj_journal_id != record.sj_journal_id
    {
        return Err(ExecError::Verify("attached"));
    }
    Ok(linked)
}

pub(super) async fn validate(
    shared: &Shared,
    command: &Command,
    start: &StartExecution,
    conversation_id: Uuid,
) -> Result<Result<PathBuf, &'static str>, ExecError> {
    if !shared.enabled.load(Ordering::Relaxed) {
        return Ok(Err("hub_capability_missing"));
    }
    if adapter::registry(&start.harness).is_none() {
        return Ok(Err("harness_unavailable"));
    }
    let workdir = start.workdir.clone();
    let roots = shared.config.allowed_workdir_roots.clone();
    let canonical = tokio::task::spawn_blocking(move || {
        let workdir = fs::canonicalize(workdir).ok()?;
        roots
            .iter()
            .filter_map(|root| fs::canonicalize(root).ok())
            .any(|root| workdir.starts_with(root))
            .then_some(workdir)
    })
    .await
    .map_err(|_| ExecError::Shutdown)?;
    let Some(workdir) = canonical else {
        return Ok(Err("workdir_not_allowed"));
    };
    let conversation = conversation_id.to_string();
    let stored = shared
        .store
        .call(move |store| Ok(open(store)?.ownership().max_generation(&conversation)))
        .await?;
    let known = shared
        .infos
        .lock()
        .expect("infos lock")
        .values()
        .filter(|info| info.conversation_id == conversation_id)
        .map(|info| info.generation)
        .max()
        .unwrap_or(0);
    if command.execution_generation <= stored.max(known) {
        return Ok(Err("generation_mismatch"));
    }
    Ok(Ok(workdir))
}

pub(super) async fn reject(
    shared: &Shared,
    command: &Command,
    reason: &str,
) -> Result<(), ExecError> {
    let id = Uuid::parse_str(&command.execution_id).ok();
    let fact = Body::CommandRejected(CommandRejected {
        command_id: command.command_id.clone(),
        dispatch_attempt: command.dispatch_attempt,
        reason: reason.into(),
    });
    let (command_id, attempt) = (command.command_id.clone(), command.dispatch_attempt);
    let seq = shared
        .store
        .call(move |store| {
            let store = open(store)?;
            let seq = store.append_cj_fact(fact)?;
            store.mark_terminal(&command_id, attempt)?;
            Ok(seq)
        })
        .await?;
    if let Some(id) = id
        && matches!(command.kind, Some(Kind::StartExecution(_)))
    {
        shared.with_info(id, |info| {
            info.state = "failed".into();
            info.outcome = "start_rejected".into();
            info.cj_terminal_seq = Some(seq);
            info.retired = seq <= shared.cj_acked.load(Ordering::Relaxed);
        });
    }
    Ok(())
}

pub(super) async fn reject_invalid(shared: &Shared, command: &Command) -> Result<(), ExecError> {
    reject(shared, command, "unsupported_command").await
}

async fn known_generation(shared: &Shared, id: Uuid) -> Result<Option<u64>, ExecError> {
    if let Some(info) = shared.infos.lock().expect("infos lock").get(&id) {
        return Ok(Some(info.generation));
    }
    Ok(shared
        .store
        .call(move |store| {
            Ok(open(store)?
                .ownership()
                .get(id)
                .map(|record| record.generation))
        })
        .await?)
}

fn terminal_self(command: &Command) -> Vec<(String, u32)> {
    vec![(command.command_id.clone(), command.dispatch_attempt)]
}

async fn defer_unhanded(shared: &Shared, command: &Command) -> Result<(), ExecError> {
    shared
        .cj(
            vec![Body::DispatchDeferred(InputDispatchDeferred {
                command_id: command.command_id.clone(),
                dispatch_attempt: command.dispatch_attempt,
                reason: "generation_closed".into(),
            })],
            terminal_self(command),
        )
        .await
}

/// Исполнения нет, оно закрыто или потеряно: управляющая служба отвечает сама.
pub(super) async fn answer_without_execution(
    shared: &Shared,
    command: &Command,
) -> Result<(), ExecError> {
    let id = Uuid::parse_str(&command.execution_id).ok();
    let known = match id {
        Some(id) => known_generation(shared, id).await?,
        None => None,
    };
    let (cid, attempt) = (command.command_id.clone(), command.dispatch_attempt);
    if matches!(
        command.kind,
        Some(Kind::DeliverInput(_) | Kind::CancelInput(_) | Kind::WithdrawInput(_))
    ) {
        let owned = match id {
            Some(id) => {
                shared
                    .store
                    .call(move |store| Ok(open(store)?.ownership().get(id).is_some()))
                    .await?
            }
            None => false,
        };
        if owned {
            return Ok(());
        }
    }
    match &command.kind {
        Some(Kind::DeliverInput(_)) => defer_unhanded(shared, command).await,
        Some(Kind::CancelInput(CancelInput {
            target_command_id,
            target_dispatch_attempt,
        })) if known.is_none() => {
            shared
                .cj(
                    vec![Body::CancelWon(InputCancelWon {
                        command_id: target_command_id.clone(),
                        dispatch_attempt: *target_dispatch_attempt,
                    })],
                    vec![
                        (target_command_id.clone(), *target_dispatch_attempt),
                        (cid, attempt),
                    ],
                )
                .await
        }
        Some(Kind::WithdrawInput(WithdrawInput {
            target_command_id,
            target_dispatch_attempt,
        })) if known.is_none() => {
            shared
                .cj(
                    vec![Body::InputWithdrawn(InputWithdrawn {
                        command_id: target_command_id.clone(),
                        dispatch_attempt: *target_dispatch_attempt,
                    })],
                    vec![
                        (target_command_id.clone(), *target_dispatch_attempt),
                        (cid, attempt),
                    ],
                )
                .await
        }
        Some(Kind::Interrupt(_)) => {
            let state = match known {
                Some(generation) if generation != command.execution_generation => {
                    InterruptState::RejectedStale
                }
                _ => InterruptState::Superseded,
            };
            shared
                .cj(
                    vec![Body::InterruptStatus(InterruptStatus {
                        command_id: cid,
                        dispatch_attempt: attempt,
                        state: state as i32,
                    })],
                    terminal_self(command),
                )
                .await
        }
        Some(Kind::StopExecution(_)) if known.is_none() => {
            shared
                .cj(
                    vec![Body::ExecutionState(ExecutionState {
                        execution_id: command.execution_id.clone(),
                        generation: command.execution_generation,
                        state: "stopped".into(),
                        outcome: "start_cancelled".into(),
                        native_conversation_id: String::new(),
                    })],
                    terminal_self(command),
                )
                .await
        }
        _ => shared.cj(vec![], terminal_self(command)).await,
    }
}

pub(super) struct Actor {
    shared: Arc<Shared>,
    id: Uuid,
    generation: u64,
    rx: mpsc::UnboundedReceiver<Msg>,
    pending: VecDeque<Msg>,
    waiting: VecDeque<Command>,
    link: Option<Link>,
    exec_dir: Option<PathBuf>,
    ipc_socket: Option<PathBuf>,
    supervisor: Option<(u32, u64)>,
    journal_id: Option<Uuid>,
    start_command: Option<(String, u32)>,
    start: Option<StartExecution>,
    start_cli_sent: bool,
    cli_started: bool,
    handed: HashSet<(Uuid, u32)>,
    cancelled_local: HashSet<(Uuid, u32)>,
    outbox: VecDeque<SjRelay>,
    last_seq: u64,
    acked_seq: u64,
    closed_seq: Option<u64>,
    finished: bool,
    orphaned: bool,
    released: bool,
    hub_rx: watch::Receiver<Option<Arc<std::collections::HashMap<Uuid, u64>>>>,
    shutdown: watch::Receiver<bool>,
    reconnect_at: Option<Instant>,
}

impl Actor {
    pub(super) fn new(
        shared: Arc<Shared>,
        id: Uuid,
        generation: u64,
        rx: mpsc::UnboundedReceiver<Msg>,
    ) -> Actor {
        let hub_rx = shared.hub.subscribe();
        let shutdown = shared.shutdown.clone();
        Actor {
            shared,
            id,
            generation,
            rx,
            pending: VecDeque::new(),
            waiting: VecDeque::new(),
            link: None,
            exec_dir: None,
            ipc_socket: None,
            supervisor: None,
            journal_id: None,
            start_command: None,
            start: None,
            start_cli_sent: false,
            cli_started: false,
            handed: HashSet::new(),
            cancelled_local: HashSet::new(),
            outbox: VecDeque::new(),
            last_seq: 0,
            acked_seq: 0,
            closed_seq: None,
            finished: false,
            orphaned: false,
            released: false,
            hub_rx,
            shutdown,
            reconnect_at: None,
        }
    }

    pub(super) async fn run(mut self, start: Start) {
        let result = self.run_inner(start).await;
        if let Err(error) = &result {
            tracing::warn!(execution_id = %self.id, error_kind = ?std::mem::discriminant(error), "execution actor stopped");
        }
        self.shared
            .actors
            .lock()
            .expect("actors lock")
            .remove(&self.id);
        if self.orphaned || self.released {
            self.rx.close();
            let mut rest: Vec<Command> = self.waiting.drain(..).collect();
            for message in self.pending.drain(..) {
                if let Msg::Command(command) = message {
                    rest.push(command);
                }
            }
            while let Ok(message) = self.rx.try_recv() {
                if let Msg::Command(command) = message {
                    rest.push(command);
                }
            }
            for command in rest {
                let _ = answer_without_execution(&self.shared, &command).await;
            }
        }
    }

    async fn run_inner(&mut self, start: Start) -> Result<(), ExecError> {
        match start {
            Start::Spawn(request) => {
                let command = request.command.clone();
                match self.spawn_sequence(*request).await {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(error) if self.supervisor.is_none() => {
                        tracing::warn!(execution_id = %self.id, error_kind = ?std::mem::discriminant(&error), "supervisor start failed");
                        self.spawn_failed(&command, true, "supervisor_start_failed")
                            .await?;
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
            Start::Reattach(adopt) => {
                let (linked, record) = *adopt;
                self.exec_dir = Some(PathBuf::from(&record.exec_dir));
                self.ipc_socket = Some(PathBuf::from(&record.ipc_socket));
                self.supervisor = Some((record.supervisor_pid, record.supervisor_start_ticks));
                self.cli_started = record.cli_pid != 0;
                self.start_cli_sent = self.cli_started;
                self.relink(linked);
            }
        }
        self.apply_hub().await?;
        self.main_loop().await
    }

    fn relink(&mut self, linked: spawn::Linked) {
        let attached = linked.attached;
        self.journal_id = Uuid::parse_str(&attached.sj_journal_id).ok();
        self.last_seq = attached.last_sj_seq;
        self.acked_seq = attached.acked_sj_seq;
        for input in &attached.received {
            if let Ok(id) = Uuid::parse_str(&input.command_id) {
                self.handed.insert((id, input.dispatch_attempt));
            }
        }
        self.link = Some(Link {
            write: linked.write,
            up_rx: start_reader(linked.read),
        });
        self.reconnect_at = None;
        let (journal, last, acked, cli) = (
            self.journal_id,
            self.last_seq,
            self.acked_seq,
            self.cli_started,
        );
        self.shared.with_info(self.id, |info| {
            info.attached = true;
            info.sj_journal_id = journal;
            info.last_seq = last;
            info.acked_seq = acked;
            if cli && info.state == "starting" {
                info.state = "running".into();
            }
        });
    }

    async fn spawn_sequence(&mut self, request: SpawnRequest) -> Result<bool, ExecError> {
        let SpawnRequest {
            command,
            start,
            workdir,
            conversation_id,
        } = request;
        self.start_command = Some((command.command_id.clone(), command.dispatch_attempt));
        self.start = Some(start.clone());
        #[cfg(feature = "fault-injection")]
        {
            let delay = *self.shared.spawn_delay.lock().expect("delay lock");
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        self.drain_mailbox();
        if *self.shutdown.borrow() {
            return Ok(false);
        }
        if self.stop_pending() {
            self.cancel_start().await?;
            return Ok(false);
        }
        let id = self.id;
        let timers = self.shared.config.supervisor.clone();
        let data_dir = self.shared.data_dir().await?.clone();
        let generation = self.generation;
        let runtime_dir = self.shared.runtime_dir.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            spawn::prepare(
                &data_dir,
                id,
                generation,
                conversation_id,
                &timers,
                runtime_dir.as_deref(),
            )
        })
        .await
        .map_err(|_| ExecError::Shutdown)?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let reason = if matches!(
                    error,
                    ExecError::Ipc(crate::supervisor::ipc::IpcError::PathTooLong)
                ) {
                    "ipc_path_too_long"
                } else {
                    "supervisor_start_failed"
                };
                return self.spawn_failed(&command, false, reason).await;
            }
        };
        let dir = prepared.dir.clone();
        self.exec_dir = Some(dir.clone());
        self.ipc_socket = Some(prepared.socket.clone());
        let boot_id = self.shared.boot_id().await?.clone();
        let record = Ownership {
            execution_id: id.to_string(),
            generation,
            conversation_id: conversation_id.to_string(),
            boot_id,
            exec_dir: dir.to_string_lossy().into_owned(),
            ipc_socket: prepared.socket.to_string_lossy().into_owned(),
            secret_sha256: sha256(&prepared.secret),
            ipc_protocol_version: IPC_PROTOCOL_VERSION,
            phase: OwnershipPhase::Spawning as i32,
            ..Default::default()
        };
        self.shared
            .store
            .call(move |store| {
                open(store)?.ownership_mut()?.append(record)?;
                Ok(())
            })
            .await?;
        let exe = match &self.shared.config.supervisor_exe {
            Some(exe) => exe.clone(),
            None => std::env::current_exe()?,
        };
        if *self.shutdown.borrow() {
            return Ok(false);
        }
        self.drain_mailbox();
        if self.stop_pending() {
            self.cancel_start().await?;
            return Ok(false);
        }
        let mut child = match spawn::spawn_supervisor(&exe, &dir) {
            Ok(child) => child,
            Err(_) => {
                return self
                    .spawn_failed(&command, true, "supervisor_start_failed")
                    .await;
            }
        };
        let pid = child.id().ok_or(ExecError::Verify("child_pid"))?;
        let linked = match spawn::connect_attach(
            &prepared.socket,
            &prepared.secret,
            id,
            generation,
            Some(pid),
            Some(self.shared.config.spawn_timeout),
        )
        .await
        {
            Ok(linked) => linked,
            Err(_) => {
                child.kill().await?;
                return self
                    .spawn_failed(&command, true, "supervisor_start_failed")
                    .await;
            }
        };
        let (supervisor_pid, ticks, journal) = (
            linked.attached.pid,
            linked.attached.start_ticks,
            linked.attached.sj_journal_id.clone(),
        );
        ownership_update(&self.shared, id, move |record| {
            record.phase = OwnershipPhase::Attached as i32;
            record.supervisor_pid = supervisor_pid;
            record.supervisor_start_ticks = ticks;
            record.sj_journal_id = journal;
        })
        .await?;
        self.supervisor = Some((supervisor_pid, ticks));
        child.detach();
        self.relink(linked);
        self.drain_mailbox();
        if self.stop_pending() {
            self.mark_start_terminal().await?;
        } else {
            self.send_start_cli(&start, &workdir).await?;
        }
        Ok(true)
    }

    async fn send_start_cli(
        &mut self,
        start: &StartExecution,
        workdir: &Path,
    ) -> Result<(), ExecError> {
        self.start_cli_sent = self
            .send(down::Msg::StartCli(StartCli {
                profile: start.harness.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
                native_conversation_id: start.native_conversation_id.clone(),
            }))
            .await;
        Ok(())
    }

    async fn spawn_failed(
        &mut self,
        command: &Command,
        owned: bool,
        reason: &str,
    ) -> Result<bool, ExecError> {
        reject(&self.shared, command, reason).await?;
        if owned {
            ownership_update(&self.shared, self.id, |record| {
                record.phase = OwnershipPhase::Released as i32;
                record.released_reason = "spawn_failed".into();
            })
            .await?;
        }
        self.released = true;
        Ok(false)
    }

    fn drain_mailbox(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            self.pending.push_back(message);
        }
    }

    fn stop_pending(&self) -> bool {
        self.pending.iter().any(|message| {
            matches!(
                message,
                Msg::Command(Command {
                    kind: Some(Kind::StopExecution(_)),
                    ..
                })
            )
        })
    }

    async fn cancel_start(&mut self) -> Result<(), ExecError> {
        ownership_update(&self.shared, self.id, |record| {
            record.phase = OwnershipPhase::Released as i32;
            record.released_reason = "start_cancelled".into();
        })
        .await?;
        let mut terminal: Vec<(String, u32)> = self.start_command.iter().cloned().collect();
        let mut rest = Vec::new();
        for message in self.pending.drain(..) {
            if let Msg::Command(command) = message {
                if matches!(command.kind, Some(Kind::StopExecution(_))) {
                    terminal.push((command.command_id.clone(), command.dispatch_attempt));
                } else {
                    rest.push(command);
                }
            }
        }
        self.shared
            .cj(
                vec![Body::ExecutionState(ExecutionState {
                    execution_id: self.id.to_string(),
                    generation: self.generation,
                    state: "stopped".into(),
                    outcome: "start_cancelled".into(),
                    native_conversation_id: String::new(),
                })],
                terminal,
            )
            .await?;
        self.shared.with_info(self.id, |info| {
            info.state = "stopped".into();
            info.outcome = "start_cancelled".into();
        });
        self.released = true;
        self.pending.extend(rest.into_iter().map(Msg::Command));
        Ok(())
    }

    async fn finish_orphan(&mut self) -> Result<(), ExecError> {
        report_owner_lost(&self.shared, self.id, self.generation).await?;
        self.link = None;
        self.orphaned = true;
        Ok(())
    }

    async fn mark_start_terminal(&mut self) -> Result<(), ExecError> {
        if let Some(start) = self.start_command.take() {
            self.shared.cj(vec![], vec![start]).await?;
        }
        Ok(())
    }

    async fn main_loop(&mut self) -> Result<(), ExecError> {
        loop {
            if *self.shutdown.borrow() {
                return Ok(());
            }
            while let Some(message) = self.pending.pop_front() {
                self.on_msg(message).await?;
            }
            if self.orphaned || self.released {
                return Ok(());
            }
            let relay = self.shared.relay.clone();
            tokio::select! {
                biased;
                _ = self.shutdown.changed() => return Ok(()),
                _ = self.hub_rx.changed() => self.apply_hub().await?,
                message = self.rx.recv() => match message {
                    Some(message) => self.on_msg(message).await?,
                    None => return Ok(()),
                },
                up = recv_up(&mut self.link) => match up {
                    Some(Up::Msg(message)) => self.on_up(message).await?,
                    Some(Up::Eof) | None => self.link_lost(),
                },
                permit = relay.reserve(), if !self.outbox.is_empty() => {
                    let permit = permit.map_err(|_| ExecError::Shutdown)?;
                    if let Some(batch) = self.outbox.pop_front() {
                        permit.send(batch);
                    }
                }
                () = reconnect_timer(self.reconnect_at) => self.check_link().await?,
            }
        }
    }

    fn link_lost(&mut self) {
        self.link = None;
        if !self.finished {
            self.reconnect_at = Some(Instant::now());
        }
    }

    async fn check_link(&mut self) -> Result<(), ExecError> {
        self.reconnect_at = None;
        let Some((pid, ticks)) = self.supervisor else {
            return self.finish_orphan().await;
        };
        if !procfs::is_same_process(pid, ticks) {
            return self.finish_orphan().await;
        }
        let Some(dir) = self.exec_dir.clone() else {
            return self.finish_orphan().await;
        };
        let secret = fs::read(dir.join("secret")).unwrap_or_default();
        let socket = self
            .ipc_socket
            .clone()
            .unwrap_or_else(|| dir.join("ipc.sock"));
        match spawn::connect_attach(&socket, &secret, self.id, self.generation, Some(pid), None)
            .await
        {
            Ok(linked) => {
                self.relink(linked);
                self.apply_hub().await?;
                let id = self.id.to_string();
                let stored = self
                    .shared
                    .store
                    .call(move |store| {
                        Ok(open(store)?
                            .stored_commands()?
                            .into_iter()
                            .filter(|command| command.execution_id == id)
                            .collect::<Vec<_>>())
                    })
                    .await?;
                self.pending.extend(stored.into_iter().map(Msg::Command));
                let waiting: Vec<Command> = self.waiting.drain(..).collect();
                self.pending.extend(waiting.into_iter().map(Msg::Command));
            }
            Err(_) => self.reconnect_at = Some(Instant::now() + RECONNECT_EVERY),
        }
        Ok(())
    }

    async fn send(&mut self, message: down::Msg) -> bool {
        #[cfg(feature = "fault-injection")]
        {
            let fail = matches!(
                message,
                down::Msg::Deliver(_)
                    | down::Msg::Cancel(_)
                    | down::Msg::Withdraw(_)
                    | down::Msg::Interrupt(_)
                    | down::Msg::Stop(_)
            ) && {
                let mut count = self.shared.fail_sends.lock().expect("send fault lock");
                if *count > 0 {
                    *count -= 1;
                    true
                } else {
                    false
                }
            };
            if fail {
                self.link_lost();
                return false;
            }
        }
        let Some(link) = &mut self.link else {
            return false;
        };
        let message = ControlToSupervisor { msg: Some(message) };
        if write_message(&mut link.write, &message).await.is_ok() {
            return true;
        }
        self.link_lost();
        false
    }

    async fn apply_hub(&mut self) -> Result<(), ExecError> {
        let committed = {
            let Some(map) = self.hub_rx.borrow_and_update().clone() else {
                return Ok(());
            };
            let Some(journal) = self.journal_id else {
                return Ok(());
            };
            map.get(&journal).copied().unwrap_or(0)
        };
        self.outbox.clear();
        if self.link.is_some() {
            self.send(down::Msg::ShipFrom(ShipFrom {
                committed_seq: committed,
            }))
            .await;
        }
        if committed > 0 {
            self.on_ack(committed).await?;
        }
        Ok(())
    }

    async fn on_ack(&mut self, up_to: u64) -> Result<(), ExecError> {
        if up_to > self.acked_seq {
            self.acked_seq = up_to;
            let acked = self.acked_seq;
            self.shared
                .with_info(self.id, |info| info.acked_seq = acked);
        }
        let delivered = self
            .send(down::Msg::SjAck(SjAck { up_to_seq: up_to }))
            .await;
        if delivered
            && self
                .closed_seq
                .is_some_and(|closed| self.acked_seq >= closed)
        {
            self.release().await?;
        }
        Ok(())
    }

    async fn release(&mut self) -> Result<(), ExecError> {
        let outcome = self
            .shared
            .infos
            .lock()
            .expect("infos lock")
            .get(&self.id)
            .map(|info| info.outcome.clone())
            .unwrap_or_default();
        ownership_update(&self.shared, self.id, move |record| {
            record.phase = OwnershipPhase::Released as i32;
            record.released_reason = outcome;
        })
        .await?;
        if let Some((pid, ticks)) = self.supervisor {
            while procfs::is_same_process(pid, ticks) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if let Some(socket) = &self.ipc_socket {
            match fs::remove_file(socket) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(dir) = &self.exec_dir {
            match fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.shared.with_info(self.id, |info| {
            info.retired = true;
            info.attached = false;
        });
        self.released = true;
        Ok(())
    }

    async fn on_msg(&mut self, message: Msg) -> Result<(), ExecError> {
        match message {
            Msg::Command(command) => self.on_command(command).await,
            Msg::SjAck(up_to) => self.on_ack(up_to).await,
            Msg::Replay(committed) => {
                self.outbox.clear();
                self.send(down::Msg::ShipFrom(ShipFrom {
                    committed_seq: committed,
                }))
                .await;
                Ok(())
            }
        }
    }

    async fn on_up(&mut self, message: up::Msg) -> Result<(), ExecError> {
        match message {
            up::Msg::Attached(_) => {}
            up::Msg::CliStarted(started) => self.on_cli_started(started).await?,
            up::Msg::DeliverAck(ack) => self.on_deliver_ack(ack).await?,
            up::Msg::SjBatch(batch) => self.on_batch(batch).await?,
            up::Msg::Exited(exited) => {
                self.finished = true;
                self.reconnect_at = None;
                let outcome = exited.reason;
                self.shared.with_info(self.id, |info| {
                    if info.state != "stopped" && info.state != "failed" {
                        info.state = match outcome.as_str() {
                            "start_failed" | "crashed" | "protocol_incompatible" => "failed",
                            _ => "stopped",
                        }
                        .into();
                        info.outcome = outcome;
                    }
                });
                self.mark_start_terminal().await?;
                if self.closed_seq.is_none() {
                    self.closed_seq = Some(self.last_seq);
                }
                if self
                    .closed_seq
                    .is_some_and(|closed| self.acked_seq >= closed)
                {
                    self.release().await?;
                }
            }
        }
        Ok(())
    }

    async fn on_cli_started(&mut self, started: CliStarted) -> Result<(), ExecError> {
        self.cli_started = true;
        let (pid, ticks) = (started.pid, started.start_ticks);
        ownership_update(&self.shared, self.id, move |record| {
            record.cli_pid = pid;
            record.cli_start_ticks = ticks;
        })
        .await?;
        self.shared.with_info(self.id, |info| {
            if info.state == "starting" {
                info.state = "running".into();
            }
        });
        self.mark_start_terminal().await
    }

    async fn on_deliver_ack(&mut self, ack: DeliverAck) -> Result<(), ExecError> {
        let terminal = vec![(ack.command_id.clone(), ack.dispatch_attempt)];
        let fact = match ack.result.as_str() {
            "generation_mismatch" | "execution_stopped" => {
                Body::DispatchDeferred(InputDispatchDeferred {
                    command_id: ack.command_id,
                    dispatch_attempt: ack.dispatch_attempt,
                    reason: "generation_closed".into(),
                })
            }
            "invalid" => Body::CommandRejected(CommandRejected {
                command_id: ack.command_id,
                dispatch_attempt: ack.dispatch_attempt,
                reason: "unsupported_command".into(),
            }),
            _ => return Ok(()),
        };
        self.shared.cj(vec![fact], terminal).await
    }

    async fn on_batch(&mut self, batch: SjBatch) -> Result<(), ExecError> {
        if Uuid::parse_str(&batch.journal_id).ok() != self.journal_id || batch.records.is_empty() {
            return Ok(());
        }
        let mut terminal = Vec::new();
        let mut transmitted = Vec::new();
        for record in &batch.records {
            self.last_seq = self.last_seq.max(record.seq);
            let Some(body) = record.body.as_ref().and_then(|body| body.body.as_ref()) else {
                continue;
            };
            match body {
                Body::InputAccepted(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt));
                }
                Body::InputRejected(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt));
                }
                Body::AcceptanceUnconfirmed(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt))
                }
                Body::CancelLost(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt))
                }
                Body::CancelWon(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt));
                }
                Body::InputWithdrawn(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt));
                }
                Body::DispatchDeferred(fact) => {
                    terminal.push((fact.command_id.clone(), fact.dispatch_attempt));
                }
                Body::TransmissionStarted(fact) => transmitted.push(fact.command_id.clone()),
                Body::ExecutionState(fact) => {
                    let (state, outcome) = (fact.state.clone(), fact.outcome.clone());
                    self.shared.with_info(self.id, |info| {
                        info.state = state;
                        info.outcome = outcome;
                    });
                }
                Body::JournalClosed(_) => self.closed_seq = Some(record.seq),
                _ => {}
            }
        }
        let last = self.last_seq;
        self.shared.with_info(self.id, |info| info.last_seq = last);
        if !terminal.is_empty() || !transmitted.is_empty() {
            self.shared
                .store
                .call(move |store| {
                    let store = open(store)?;
                    for command_id in transmitted {
                        store.note_transmitted(&command_id)?;
                    }
                    for (command_id, attempt) in terminal {
                        store.mark_terminal(&command_id, attempt)?;
                    }
                    Ok(())
                })
                .await?;
        }
        if let Some(journal_id) = self.journal_id {
            self.outbox.push_back(SjRelay {
                journal_id,
                records: batch.records,
            });
        }
        Ok(())
    }

    async fn on_command(&mut self, command: Command) -> Result<(), ExecError> {
        if self.link.is_none() && !self.finished {
            self.waiting.push_back(command);
            return Ok(());
        }
        let live = self.link.is_some() && !self.finished;
        let (cid_text, attempt) = (command.command_id.clone(), command.dispatch_attempt);
        let self_terminal = vec![(cid_text.clone(), attempt)];
        match command.kind.clone() {
            Some(Kind::StartExecution(start)) => {
                if self.start_command.is_none() {
                    self.start_command = Some((cid_text, attempt));
                }
                if self.cli_started || self.finished {
                    return self.mark_start_terminal().await;
                }
                if live && !self.start_cli_sent {
                    let workdir = PathBuf::from(&start.workdir);
                    self.start = Some(start.clone());
                    self.send_start_cli(&start, &workdir).await?;
                }
                Ok(())
            }
            Some(Kind::DeliverInput(_)) => {
                let Ok(cid) = Uuid::parse_str(&cid_text) else {
                    return Ok(());
                };
                if self.cancelled_local.contains(&(cid, attempt)) {
                    return self.shared.cj(vec![], self_terminal).await;
                }
                if !live || command.execution_generation != self.generation {
                    if live && !self.handed.iter().any(|(id, _)| *id == cid) {
                        return defer_unhanded(&self.shared, &command).await;
                    }
                    return answer_without_execution(&self.shared, &command).await;
                }
                self.handed.insert((cid, attempt));
                if !self
                    .send(down::Msg::Deliver(Deliver {
                        command: Some(command.clone()),
                    }))
                    .await
                {
                    self.waiting.push_back(command);
                }
                Ok(())
            }
            Some(Kind::CancelInput(CancelInput {
                target_command_id,
                target_dispatch_attempt,
            })) => {
                self.cancel_or_withdraw(
                    &command,
                    &target_command_id,
                    target_dispatch_attempt,
                    live,
                    true,
                )
                .await
            }
            Some(Kind::WithdrawInput(WithdrawInput {
                target_command_id,
                target_dispatch_attempt,
            })) => {
                self.cancel_or_withdraw(
                    &command,
                    &target_command_id,
                    target_dispatch_attempt,
                    live,
                    false,
                )
                .await
            }
            Some(Kind::Interrupt(_)) => {
                if live && command.execution_generation == self.generation {
                    if !self
                        .send(down::Msg::Interrupt(InterruptRequest {
                            command_id: cid_text,
                            dispatch_attempt: attempt,
                        }))
                        .await
                    {
                        self.waiting.push_back(command);
                        return Ok(());
                    }
                    self.shared.cj(vec![], self_terminal).await
                } else if command.execution_generation != self.generation {
                    self.interrupt_status(&command, InterruptState::RejectedStale)
                        .await
                } else {
                    self.interrupt_status(&command, InterruptState::Superseded)
                        .await
                }
            }
            Some(Kind::StopExecution(stop)) => {
                if live
                    && !self
                        .send(down::Msg::Stop(StopRequest {
                            command_id: cid_text,
                            dispatch_attempt: attempt,
                            force: stop.force,
                        }))
                        .await
                {
                    self.waiting.push_back(command);
                    return Ok(());
                }
                self.shared.cj(vec![], self_terminal).await
            }
            None => Ok(()),
        }
    }

    async fn interrupt_status(
        &mut self,
        command: &Command,
        state: InterruptState,
    ) -> Result<(), ExecError> {
        self.shared
            .cj(
                vec![Body::InterruptStatus(InterruptStatus {
                    command_id: command.command_id.clone(),
                    dispatch_attempt: command.dispatch_attempt,
                    state: state as i32,
                })],
                terminal_self(command),
            )
            .await
    }

    async fn cancel_or_withdraw(
        &mut self,
        command: &Command,
        target: &str,
        target_attempt: u32,
        live: bool,
        cancel: bool,
    ) -> Result<(), ExecError> {
        let Ok(target_id) = Uuid::parse_str(target) else {
            return self.shared.cj(vec![], terminal_self(command)).await;
        };
        if self.cancelled_local.contains(&(target_id, target_attempt)) {
            return self.shared.cj(vec![], terminal_self(command)).await;
        }
        let handed = self.handed.iter().any(|(id, _)| *id == target_id);
        if handed {
            if live {
                let message = if cancel {
                    down::Msg::Cancel(CancelInput {
                        target_command_id: target.to_owned(),
                        target_dispatch_attempt: target_attempt,
                    })
                } else {
                    down::Msg::Withdraw(WithdrawInput {
                        target_command_id: target.to_owned(),
                        target_dispatch_attempt: target_attempt,
                    })
                };
                if self.send(message).await {
                    return self.shared.cj(vec![], terminal_self(command)).await;
                }
            }
            self.waiting.push_back(command.clone());
            return Ok(());
        }
        self.cancelled_local.insert((target_id, target_attempt));
        let fact = if cancel {
            Body::CancelWon(InputCancelWon {
                command_id: target.to_owned(),
                dispatch_attempt: target_attempt,
            })
        } else {
            Body::InputWithdrawn(InputWithdrawn {
                command_id: target.to_owned(),
                dispatch_attempt: target_attempt,
            })
        };
        self.shared
            .cj(
                vec![fact],
                vec![
                    (target.to_owned(), target_attempt),
                    (command.command_id.clone(), command.dispatch_attempt),
                ],
            )
            .await
    }
}

async fn recv_up(link: &mut Option<Link>) -> Option<Up> {
    match link {
        Some(link) => link.up_rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn reconnect_timer(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ExecutionConfig, exec::ExecutionManager, session_store::StoreThread,
        store::WorkerStore,
    };
    use canterlot_proto::worker::v1::{DeliverInput, InputCancelLost, JournalRecord, RecordBody};

    #[tokio::test]
    async fn cancel_lost_releases_pending_delivery() {
        let root = tempfile::tempdir().unwrap();
        let mut store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let cid = Uuid::now_v7().to_string();
        store
            .accept_command(&Command {
                command_id: cid.clone(),
                dispatch_attempt: 1,
                execution_id: id.to_string(),
                execution_generation: 1,
                kind: Some(Kind::DeliverInput(DeliverInput::default())),
            })
            .unwrap();
        assert!(!store.command_state(&cid).unwrap().unwrap().terminal);
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new(ExecutionConfig::default(), thread.client(), relay);
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        let journal = Uuid::now_v7();
        actor.journal_id = Some(journal);
        actor
            .on_batch(SjBatch {
                journal_id: journal.to_string(),
                records: vec![JournalRecord {
                    seq: 1,
                    body: Some(RecordBody {
                        body: Some(Body::CancelLost(InputCancelLost {
                            command_id: cid.clone(),
                            dispatch_attempt: 1,
                            reason: "transmission_started".into(),
                        })),
                    }),
                }],
            })
            .await
            .unwrap();
        assert!(
            thread
                .call(move |store| Ok(open(store)?.command_state(&cid)?.unwrap().terminal))
                .await
                .unwrap()
        );
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }
}
