use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, SystemTime},
};

use canterlot_proto::{
    limits::{IPC_CAP_COMPACT, IPC_CAP_STEER, IPC_PROTOCOL_VERSION},
    worker::{
        ipc::{
            CliStarted, CompactRequest, ControlToSupervisor, Deliver, DeliverAck, InterruptRequest,
            LocalFile, MediaResult, MediaUpload, Ownership, OwnershipPhase, ShipFrom, SjAck,
            SjBatch, StartCli, StopRequest, SupervisorToControl, control_to_supervisor as down,
            supervisor_to_control as up,
        },
        v1::{
            AttachmentFailed, AttachmentReady, AttachmentRef, CancelInput, Command,
            CommandRejected, CompactionState, CompactionStatus, ContentBlock, DecisionNotApplied,
            ExecutionState, InputCancelLost, InputCancelWon, InputDispatchDeferred, InputWithdrawn,
            InterruptState, InterruptStatus, MediaHeader, RemoveWorktree, StartExecution,
            WithdrawInput, WorktreeRemoved, command::Kind, content_block, record_body::Body,
        },
    },
};
use sha2::{Digest, Sha256};
use tokio::{
    net::unix::{OwnedReadHalf, OwnedWriteHalf},
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{ExecError, Shared, SjRelay, lost, maintenance, open, procfs, spawn};
use crate::{
    adapter,
    attachments::{AttachmentStore, FetchError, RetryPolicy},
    media::{self, PutError},
    session::WorkerError,
    supervisor::ipc::{read_message, write_message},
};

const ORPHANED: &str = "owner_lost";
const REPORTED: &str = "owner_lost_reported";
const RECONNECT_EVERY: Duration = Duration::from_millis(200);

#[allow(clippy::large_enum_variant)]
pub(crate) enum Msg {
    Command(Command),
    SjAck(u64),
    Replay(u64),
    AttachmentProgress {
        key: (Uuid, u32),
        attachment_id: String,
        result: Result<(), FetchError>,
        retryable: bool,
    },
    FetchFinished {
        key: (Uuid, u32),
        files: Result<Vec<LocalFile>, FetchError>,
    },
    MediaFinished {
        media_id: Uuid,
        result: Result<(), PutError>,
    },
    ShareFinished {
        share_id: Uuid,
        result: Result<(), PutError>,
    },
    AgentCallDone {
        call_id: Uuid,
        result: crate::adapter::AgentCallResult,
    },
}

/// Загрузка картинки агента на хаб; прерывается вместе с актором.
struct MediaTask(tokio::task::JoinHandle<()>);
impl Drop for MediaTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct FetchTask {
    command: Command,
    references: Vec<AttachmentRef>,
    cancel: CancellationToken,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for FetchTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) struct SpawnRequest {
    pub command: Command,
    pub start: StartExecution,
    pub workdir: PathBuf,
    /// Проверенный исходный каталог, если рабочую копию надо подготовить перед запуском.
    pub source: Option<PathBuf>,
    pub conversation_id: Uuid,
}

pub(super) enum Start {
    Spawn(Box<SpawnRequest>),
    Reattach(Box<(spawn::Linked, Ownership)>),
}

// Размер варианта растёт с аддитивными полями IPC: форму не меняем.
#[allow(clippy::large_enum_variant)]
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

pub(super) fn is_reported(record: &Ownership) -> bool {
    record.released_reason == REPORTED
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
    let record = shared
        .store
        .call(move |store| {
            let store = open(store)?;
            let Some(mut record) = store.ownership().get(id).cloned() else {
                return Ok(None);
            };
            change(&mut record);
            store.ownership_mut()?.append(record.clone())?;
            Ok(Some(record))
        })
        .await?;
    if let Some(record) = record {
        shared.remember_ownership(&record);
    }
    Ok(())
}

/// Что из отправленных записей журнала касается команд, лежащих у управляющей службы.
#[derive(Default)]
struct Relayed {
    terminal: Vec<(String, u32)>,
    transmitted: Vec<String>,
    decisions: Vec<(String, String)>,
}

impl Relayed {
    fn note(&mut self, body: &Body) {
        match body {
            Body::DecisionApplied(f) => self
                .decisions
                .push((f.request_id.clone(), f.decision_id.clone())),
            Body::DecisionNotApplied(f) => self
                .decisions
                .push((f.request_id.clone(), f.decision_id.clone())),
            Body::DecisionUnconfirmed(f) => self
                .decisions
                .push((f.request_id.clone(), f.decision_id.clone())),
            Body::InputAccepted(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::InputRejected(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::AcceptanceUnconfirmed(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::CancelLost(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::CancelWon(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::InputWithdrawn(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::DispatchDeferred(f) => self
                .terminal
                .push((f.command_id.clone(), f.dispatch_attempt)),
            Body::TransmissionStarted(f) => self.transmitted.push(f.command_id.clone()),
            _ => {}
        }
    }

    async fn store(self, shared: &Shared, execution: Uuid) -> Result<(), ExecError> {
        let Relayed {
            mut terminal,
            transmitted,
            decisions,
        } = self;
        if terminal.is_empty() && transmitted.is_empty() && decisions.is_empty() {
            return Ok(());
        }
        let execution = execution.to_string();
        shared
            .store
            .call(move |store| {
                let store = open(store)?;
                if !decisions.is_empty() {
                    for command in store.stored_commands()? {
                        if command.execution_id == execution
                            && let Some(Kind::DeliverDecision(decision)) = &command.kind
                            && decisions.iter().any(|(request, id)| {
                                request == &decision.request_id && id == &decision.decision_id
                            })
                        {
                            terminal.push((command.command_id.clone(), command.dispatch_attempt));
                        }
                    }
                }
                for command_id in transmitted {
                    store.note_transmitted(&command_id)?;
                }
                for (command_id, attempt) in terminal {
                    store.mark_terminal(&command_id, attempt)?;
                }
                Ok(())
            })
            .await?;
        Ok(())
    }
}

/// Записи умершего сопровождения обновляют команды так же, как живые.
pub(super) async fn store_relayed(
    shared: &Shared,
    execution: Uuid,
    records: &[canterlot_proto::worker::v1::JournalRecord],
) -> Result<(), ExecError> {
    let mut relayed = Relayed::default();
    for body in records
        .iter()
        .filter_map(|record| record.body.as_ref().and_then(|body| body.body.as_ref()))
    {
        relayed.note(body);
    }
    relayed.store(shared, execution).await
}

/// Регистрирует актор исполнения; новый отправитель заменяет прежний.
pub(super) fn spawn_actor(shared: &Arc<Shared>, id: Uuid, start: Start) {
    let generation = shared
        .infos
        .lock()
        .expect("infos lock")
        .get(&id)
        .map_or(0, |info| info.generation);
    let (sender, receiver) = mpsc::unbounded_channel();
    shared
        .actors
        .lock()
        .expect("actors lock")
        .insert(id, sender);
    let actor = Actor::new(shared.clone(), id, generation, receiver);
    let task = tokio::spawn(actor.run(start));
    shared.tasks.lock().expect("tasks lock").push(task);
}

/// Сокеты и каталоги прерванных запусков: только то, на что нет записи принадлежности.
pub(super) async fn sweep_runtime(shared: &Shared) -> Result<(), ExecError> {
    let data_dir = shared.data_dir().await?.clone();
    let runtime_dir = shared.runtime_dir.clone();
    let spawn_timeout = shared.config.spawn_timeout;
    maintenance::ensure_test_runtime_allowed(runtime_dir.as_deref())?;
    let executions_dir = data_dir.join("executions");
    let now = SystemTime::now();
    let (executions, sockets) = tokio::task::spawn_blocking({
        let data_dir = data_dir.clone();
        let runtime_dir = runtime_dir.clone();
        move || {
            Ok::<_, io::Error>((
                maintenance::stale_executions(
                    &data_dir,
                    runtime_dir.as_deref(),
                    spawn_timeout,
                    now,
                )?,
                maintenance::stale_sockets(&data_dir, runtime_dir.as_deref())?,
            ))
        }
    })
    .await
    .map_err(|_| ExecError::Shutdown)??;
    for execution in executions {
        before_recheck(&execution.id.to_string()).await;
        shared
            .store
            .call(move |slot| {
                if open(slot)?.ownership().get(execution.id).is_some() {
                    return Ok(());
                }
                maintenance::remove_stale_execution(&execution, spawn_timeout, SystemTime::now())?;
                Ok(())
            })
            .await?;
    }
    for (id, path) in sockets {
        before_recheck(&id.to_string()).await;
        let executions_dir = executions_dir.clone();
        shared
            .store
            .call(move |slot| {
                let store = open(slot)?;
                let unreleased = store
                    .ownership()
                    .unreleased()
                    .iter()
                    .any(|record| Path::new(&record.ipc_socket) == path.as_path());
                if unreleased {
                    return Ok(());
                }
                maintenance::remove_stale_socket(id, &path, &executions_dir)?;
                Ok(())
            })
            .await?;
    }
    Ok(())
}

/// Тестовый барьер между перечислением и удалением: проверяет, что решение перечитывается.
#[cfg(test)]
pub(super) type BeforeRecheck = std::sync::Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

#[cfg(test)]
pub(super) static BEFORE_RECHECK: std::sync::Mutex<Option<BeforeRecheck>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
async fn before_recheck(key: &str) {
    let hook = BEFORE_RECHECK
        .lock()
        .expect("maintenance hook lock")
        .clone();
    if let Some(hook) = hook {
        hook(key.to_owned()).await;
    }
}

#[cfg(not(test))]
async fn before_recheck(_: &str) {}

/// Сборка в рабочей копии без живого исполнения: `target` и кэш узлов старше шести часов.
pub(super) async fn gc_worktrees(shared: &Shared) -> Result<(), ExecError> {
    let data_dir = shared.data_dir().await?.clone();
    let idle_worktrees = tokio::task::spawn_blocking(move || {
        maintenance::idle_worktrees(&data_dir, SystemTime::now(), Duration::from_secs(6 * 3600))
    })
    .await
    .map_err(|_| ExecError::Shutdown)??;
    for worktree in idle_worktrees {
        before_recheck(&worktree.conversation_id).await;
        let conversation_id = worktree.conversation_id.clone();
        let path = worktree.path.clone();
        // Проверка и удаление в потоке хранилища: запись принадлежности не вклинится между ними.
        shared
            .store
            .call(move |slot| {
                let store = open(slot)?;
                let live = store
                    .ownership()
                    .unreleased()
                    .iter()
                    .any(|record| record.conversation_id == conversation_id);
                if live {
                    return Ok(());
                }
                maintenance::remove_build(&path)?;
                Ok(())
            })
            .await?;
    }
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
                .and_then(|text| toml::from_str::<crate::supervisor::PersistedSpec>(&text).ok());
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
        if !is_reported(record) {
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

/// `verify` снова доказал то же живое сопровождение: хаб получает факт раньше записей `SJ`,
/// потому что `CJ` доигрывается до сверки. Сбой между записями только повторит доказательство.
pub(super) async fn reclaim_owner(
    shared: &Shared,
    id: Uuid,
    generation: u64,
) -> Result<u64, ExecError> {
    let (seq, record) = shared
        .store
        .call(move |store| {
            let store = open(store)?;
            let seq = store.append_cj_fact(Body::ExecutionState(ExecutionState {
                execution_id: id.to_string(),
                generation,
                state: "running".into(),
                outcome: canterlot_proto::OWNER_RECLAIMED.into(),
                native_conversation_id: String::new(),
            }))?;
            let mut record = store
                .ownership()
                .get(id)
                .cloned()
                .ok_or(WorkerError::Execution)?;
            record.released_reason.clear();
            record.reclaimed_cj_seq = seq;
            store.ownership_mut()?.append(record.clone())?;
            Ok((seq, record))
        })
        .await?;
    shared.remember_ownership(&record);
    Ok(seq)
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
    let spec: crate::supervisor::PersistedSpec =
        toml::from_str(&fs::read_to_string(dir.join("spec.toml"))?)
            .map_err(|_| ExecError::Verify("spec"))?;
    if spec.execution_id != id || spec.generation != record.generation {
        return Err(ExecError::Verify("spec"));
    }
    if Path::new(&record.ipc_socket) != spec.ipc_socket.unwrap_or_else(|| dir.join("ipc.sock")) {
        return Err(ExecError::Verify("ipc_socket"));
    }
    if sha256(&secret) != record.secret_sha256 {
        return Err(ExecError::Verify("secret"));
    }
    let deadline = Instant::now() + shared.config.supervisor.attach_wait;
    let linked = loop {
        let attempt = spawn::connect_attach(
            Path::new(&record.ipc_socket),
            &secret,
            id,
            record.generation,
            Some(record.supervisor_pid),
            None,
        );
        let result = tokio::time::timeout_at(deadline, attempt)
            .await
            .unwrap_or(Err(ExecError::AttachTimeout));
        match result {
            Ok(linked) => break linked,
            Err(error) => {
                if Instant::now() >= deadline
                    || !procfs::is_same_process(
                        record.supervisor_pid,
                        record.supervisor_start_ticks,
                    )
                {
                    return Err(error);
                }
                sleep_until(deadline.min(Instant::now() + RECONNECT_EVERY)).await;
            }
        }
    };
    let attached = &linked.attached;
    if attached.pid != record.supervisor_pid
        || attached.start_ticks != record.supervisor_start_ticks
        || attached.sj_journal_id != record.sj_journal_id
    {
        return Err(ExecError::Verify("attached"));
    }
    Ok(linked)
}

/// Каталоги проверенного запуска: `source` задан только для изолированной рабочей копии.
pub(super) struct Validated {
    pub workdir: PathBuf,
    pub source: Option<PathBuf>,
}

fn under_roots(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let path = fs::canonicalize(path).ok()?;
    roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .any(|root| path.starts_with(root))
        .then_some(path)
}

pub(super) async fn validate(
    shared: &Shared,
    command: &Command,
    start: &StartExecution,
    conversation_id: Uuid,
) -> Result<Result<Validated, &'static str>, ExecError> {
    if !shared.enabled.load(Ordering::Relaxed) {
        return Ok(Err("hub_capability_missing"));
    }
    if !adapter::known_harness(&start.harness)
        || (start.harness == "claude_code"
            && shared
                .adapters
                .lock()
                .expect("adapter config lock")
                .claude_code
                .is_none())
        || (start.harness == "codex"
            && shared
                .adapters
                .lock()
                .expect("adapter config lock")
                .codex
                .is_none())
        || (start.harness == "opencode"
            && shared
                .adapters
                .lock()
                .expect("adapter config lock")
                .opencode
                .is_none())
        || (start.harness == "antigravity"
            && shared
                .adapters
                .lock()
                .expect("adapter config lock")
                .antigravity
                .is_none())
    {
        return Ok(Err("harness_unavailable"));
    }
    let workdir = start.workdir.clone();
    let roots = shared.config.allowed_workdir_roots.clone();
    let source = start
        .worktree
        .as_ref()
        .map(|spec| spec.source_workdir.clone());
    let data_dir = match source {
        Some(_) => Some(shared.data_dir().await?.clone()),
        None => None,
    };
    let canonical = tokio::task::spawn_blocking(move || match (source, data_dir) {
        (Some(source), Some(data_dir)) => {
            let source = under_roots(&roots, Path::new(&source))?;
            let root = PathBuf::from(crate::machine::worktree_root(&data_dir));
            let target = crate::worktree::resolve_target(&root, Path::new(&workdir))?;
            Some(Validated {
                workdir: target,
                source: Some(source),
            })
        }
        _ => Some(Validated {
            workdir: under_roots(&roots, Path::new(&workdir))?,
            source: None,
        }),
    })
    .await
    .map_err(|_| ExecError::Shutdown)?;
    let Some(validated) = canonical else {
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
    Ok(Ok(validated))
}

/// Уборка рабочей копии не привязана к исполнению: итог идёт в CJ, команда гасится вместе с ним.
pub(super) async fn remove_worktree(
    shared: &Shared,
    command: &Command,
    request: &RemoveWorktree,
) -> Result<(), ExecError> {
    let data_dir = shared.data_dir().await?.clone();
    let roots = shared.config.allowed_workdir_roots.clone();
    let source = request.source_workdir.clone();
    let (root, source) = tokio::task::spawn_blocking(move || {
        (
            PathBuf::from(crate::machine::worktree_root(&data_dir)),
            under_roots(&roots, Path::new(&source)),
        )
    })
    .await
    .map_err(|_| ExecError::Shutdown)?;
    let done = match source {
        Some(source) => crate::worktree::remove(&root, &source, request).await,
        None => crate::worktree::Removed::failed("source_not_allowed"),
    };
    shared
        .cj(
            vec![Body::WorktreeRemoved(WorktreeRemoved {
                command_id: command.command_id.clone(),
                dispatch_attempt: command.dispatch_attempt,
                outcome: done.outcome.into(),
                detail: done.detail.into(),
                branch_kept: done.branch_kept,
            })],
            terminal_self(command),
        )
        .await
}

/// Отказ запуска виден в журнале воркера: `supervisor.log` удаляется вместе с каталогом
/// исполнения, а `start_failed` без причины владелец не разберёт.
fn log_start_failure(execution_id: Uuid, reason: &str) {
    tracing::warn!(
        execution_id = %execution_id,
        kind = "start_failed",
        reason,
        "execution start failed"
    );
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
        log_start_failure(id, reason);
        shared.with_info(id, |info| {
            info.state = "failed".into();
            info.outcome = "start_rejected".into();
            info.cj_terminal_seq = Some(seq);
            info.retired = seq <= shared.cj_acked.load(Ordering::Relaxed);
        });
    }
    Ok(())
}

/// Сопровождение этого исполнения не объявило нужную возможность IPC.
const CAPABILITY_MISSING: &str = "capability_missing";

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

fn compaction_status(command: &Command, state: CompactionState) -> Body {
    Body::CompactionStatus(CompactionStatus {
        command_id: command.command_id.clone(),
        dispatch_attempt: command.dispatch_attempt,
        state: state.into(),
    })
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
        Some(
            Kind::DeliverInput(_)
                | Kind::SteerInput(_)
                | Kind::CancelInput(_)
                | Kind::WithdrawInput(_)
        )
    ) {
        let owned = match id {
            Some(id) => {
                shared
                    .store
                    .call(move |store| {
                        Ok(open(store)?
                            .ownership()
                            .get(id)
                            .is_some_and(|record| record.phase != OwnershipPhase::Released as i32))
                    })
                    .await?
            }
            None => false,
        };
        if owned {
            return Ok(());
        }
    }
    match &command.kind {
        Some(Kind::DeliverDecision(decision)) => {
            decision_rejected(
                shared,
                command,
                decision,
                if known.is_some_and(|generation| generation != command.execution_generation)
                    || decision.generation != command.execution_generation
                {
                    "stale_generation"
                } else {
                    "request_closed"
                },
            )
            .await
        }
        Some(Kind::DeliverInput(_)) | Some(Kind::SteerInput(_)) => {
            defer_unhanded(shared, command).await
        }
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
        Some(Kind::CompactSession(_)) => {
            let state = match known {
                Some(generation) if generation != command.execution_generation => {
                    CompactionState::RejectedStale
                }
                _ => CompactionState::Superseded,
            };
            shared
                .cj(
                    vec![compaction_status(command, state)],
                    terminal_self(command),
                )
                .await
        }
        // Актор успел смениться наблюдателем между выбором адресата и отправкой.
        Some(Kind::ResolveOwnerLost(_)) => {
            let watcher =
                id.and_then(|id| shared.actors.lock().expect("actors lock").get(&id).cloned());
            if let Some(sender) = watcher {
                let _ = sender.send(Msg::Command(command.clone()));
                return Ok(());
            }
            shared.cj(vec![], terminal_self(command)).await
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

async fn decision_rejected(
    shared: &Shared,
    command: &Command,
    decision: &canterlot_proto::worker::v1::DeliverDecision,
    reason: &str,
) -> Result<(), ExecError> {
    shared
        .cj(
            vec![Body::DecisionNotApplied(DecisionNotApplied {
                request_id: decision.request_id.clone(),
                decision_id: decision.decision_id.clone(),
                reason: reason.into(),
            })],
            terminal_self(command),
        )
        .await
}

pub(super) struct Actor {
    shared: Arc<Shared>,
    id: Uuid,
    generation: u64,
    rx: mpsc::UnboundedReceiver<Msg>,
    pending: VecDeque<Msg>,
    waiting: VecDeque<Command>,
    link: Option<Link>,
    /// Возможности IPC сопровождения: из последнего `Attached`, у 1.0.0 и старше — по выпуску.
    ipc_caps: HashSet<String>,
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
    fetching: HashMap<(Uuid, u32), FetchTask>,
    attachments: Option<Arc<AttachmentStore>>,
    uploads: HashMap<Uuid, MediaTask>,
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
            ipc_caps: HashSet::new(),
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
            fetching: HashMap::new(),
            attachments: None,
            uploads: HashMap::new(),
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
        for (_, mut fetch) in self.fetching.drain() {
            fetch.shutdown.cancel();
            let _ = (&mut fetch.task).await;
        }
        if let Err(error) = &result {
            tracing::warn!(execution_id = %self.id, error_kind = ?std::mem::discriminant(error), "execution actor stopped");
        }
        self.shared
            .actors
            .lock()
            .expect("actors lock")
            .remove(&self.id);
        let id = self.id;
        if self.orphaned && !*self.shutdown.borrow() {
            let record = self
                .shared
                .store
                .call(move |store| Ok(open(store)?.ownership().get(id).cloned()))
                .await;
            if let Ok(Some(record)) = record {
                lost::watch(&self.shared, record, None);
            }
        }
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
                let watcher = self
                    .shared
                    .actors
                    .lock()
                    .expect("actors lock")
                    .get(&id)
                    .cloned();
                match watcher {
                    Some(sender) => {
                        let _ = sender.send(Msg::Command(command));
                    }
                    None => {
                        let _ = answer_without_execution(&self.shared, &command).await;
                    }
                }
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
                        tracing::debug!(execution_id = %self.id, error = %error, "supervisor start failed");
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
        #[allow(unused_mut)]
        let mut attached = linked.attached;
        #[cfg(feature = "fault-injection")]
        if *self.shared.strip_caps.lock().expect("caps fault lock") {
            attached.ipc_capabilities.clear();
        }
        self.ipc_caps = if attached.ipc_capabilities.is_empty() {
            // Сборка без списка: возможности по выпуску, из которого запущено сопровождение.
            let caps = super::ipc_caps::supervisor_exe(attached.pid, attached.start_ticks)
                .map(|exe| super::ipc_caps::of_release_exe(&exe))
                .unwrap_or_default();
            tracing::info!(
                execution_id = %self.id,
                kind = "supervisor_ipc_legacy",
                capabilities = caps.len(),
                "supervisor does not advertise ipc capabilities"
            );
            caps
        } else {
            attached.ipc_capabilities.iter().cloned().collect()
        };
        self.journal_id = Uuid::parse_str(&attached.sj_journal_id).ok();
        self.last_seq = attached.last_sj_seq;
        self.acked_seq = attached.acked_sj_seq;
        for input in &attached.received {
            if let Ok(id) = Uuid::parse_str(&input.command_id) {
                let key = (id, input.dispatch_attempt);
                if matches!(input.state.as_str(), "cancelled" | "withdrawn") {
                    self.handed.remove(&key);
                    self.cancelled_local.insert(key);
                } else {
                    self.handed.insert(key);
                }
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
            source,
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
        if let (Some(spec), Some(source)) = (&start.worktree, &source) {
            // Подготовка идёт в акторе: медленная распаковка не задерживает остальные команды.
            if let Err(reason) = crate::worktree::prepare(&workdir, source, spec).await {
                return self.spawn_failed(&command, false, reason).await;
            }
            self.drain_mailbox();
            if *self.shutdown.borrow() {
                return Ok(false);
            }
            if self.stop_pending() {
                self.cancel_start().await?;
                return Ok(false);
            }
        }
        let id = self.id;
        let timers = self.shared.config.supervisor.clone();
        let data_dir = self.shared.data_dir().await?.clone();
        let generation = self.generation;
        let runtime_dir = self.shared.runtime_dir.clone();
        let adapters = self
            .shared
            .adapters
            .lock()
            .expect("adapter config lock")
            .clone();
        let prepared = tokio::task::spawn_blocking(move || {
            let prepared = spawn::prepare(
                &data_dir,
                id,
                generation,
                conversation_id,
                &timers,
                runtime_dir.as_deref(),
            )?;
            let spec_path = prepared.dir.join("spec.toml");
            let mut spec: toml::Value = toml::from_str(&fs::read_to_string(&spec_path)?)
                .map_err(|_| ExecError::Verify("spec"))?;
            let table = spec.as_table_mut().ok_or(ExecError::Verify("spec"))?;
            if let Some(cfg) = adapters.claude_code {
                let cfg =
                    toml::Value::try_from(cfg).map_err(|_| ExecError::Verify("claude_code"))?;
                table.insert("claude_code".into(), cfg);
            }
            if let Some(cfg) = adapters.codex {
                let cfg = toml::Value::try_from(cfg).map_err(|_| ExecError::Verify("codex"))?;
                table.insert("codex".into(), cfg);
            }
            if let Some(cfg) = adapters.opencode {
                let cfg = toml::Value::try_from(cfg).map_err(|_| ExecError::Verify("opencode"))?;
                table.insert("opencode".into(), cfg);
            }
            if let Some(mut cfg) = adapters.antigravity {
                cfg.data_dir = Some(data_dir.clone());
                let cfg =
                    toml::Value::try_from(cfg).map_err(|_| ExecError::Verify("antigravity"))?;
                table.insert("antigravity".into(), cfg);
            }
            fs::write(
                &spec_path,
                toml::to_string(&spec).map_err(|_| ExecError::Verify("spec"))?,
            )?;
            fs::File::open(&spec_path)?.sync_all()?;
            Ok::<_, ExecError>(prepared)
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
        let mut child = match spawn::spawn_supervisor(&exe, &dir, self.shared.config.build_jobs) {
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
                model: start.model.clone(),
                effort: start.effort.clone(),
                permission_mode: start.permission_mode.clone(),
                instructions: start.instructions.clone(),
                pilot_v1: start.capabilities.iter().any(|c| c == "pilot_v1"),
                todo_v1: start.capabilities.iter().any(|c| c == "todo_v1"),
                compact_v1: start.capabilities.iter().any(|c| c == "compact_v1"),
                background_v1: start.capabilities.iter().any(|c| c == "background_v1"),
                session_tools_v1: start.capabilities.iter().any(|c| c == "session_tools_v1"),
                isolate_mcp: start.isolate_mcp,
                send_now_v1: start.capabilities.iter().any(|c| c == "send_now_v1"),
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
        // Причина отказа — короткий код; строку журнала пишет `reject` вместе с CJ.
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
                    | down::Msg::Compact(_)
                    | down::Msg::Stop(_)
                    | down::Msg::DeliverDecision(_)
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
            map.get(&journal).copied().unwrap_or(self.acked_seq)
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
        // Каталог исполнения удаляется ниже: незавершённые загрузки возвращают ввод хабу.
        self.cancel_fetches().await?;
        self.uploads.clear();
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
            Msg::AttachmentProgress {
                key,
                attachment_id,
                result,
                retryable,
            } => {
                if self.discard_closed_fetch(key).await? {
                    return Ok(());
                }
                if let Some(fetch) = self.fetching.get(&key) {
                    let command_id = fetch.command.command_id.clone();
                    let dispatch_attempt = fetch.command.dispatch_attempt;
                    let fact = match result {
                        Ok(()) => Body::AttachmentReady(AttachmentReady {
                            command_id,
                            dispatch_attempt,
                            attachment_id,
                        }),
                        Err(error) => Body::AttachmentFailed(AttachmentFailed {
                            command_id,
                            dispatch_attempt,
                            attachment_id,
                            reason: error.reason().into(),
                            retryable,
                        }),
                    };
                    self.shared.cj(vec![fact], vec![]).await?;
                }
                Ok(())
            }
            Msg::MediaFinished { media_id, result } => {
                self.uploads.remove(&media_id);
                if let Err(error) = result {
                    tracing::warn!(kind = "media_upload_failed", execution_id = %self.id, media_id = %media_id, reason = error.reason(), "media upload failed");
                }
                // Без связи сопровождение повторит запрос после нового Attach.
                self.send(down::Msg::MediaResult(MediaResult {
                    media_id: media_id.to_string(),
                    stored: result.is_ok(),
                    reason: result
                        .err()
                        .map(PutError::reason)
                        .unwrap_or_default()
                        .into(),
                }))
                .await;
                Ok(())
            }
            Msg::ShareFinished { share_id, result } => {
                self.uploads.remove(&share_id);
                if let Err(error) = result {
                    tracing::warn!(kind = "share_upload_failed", execution_id = %self.id, share_id = %share_id, reason = error.reason(), "share upload failed");
                }
                self.send(down::Msg::ShareResult(
                    canterlot_proto::worker::ipc::ShareResult {
                        share_id: share_id.to_string(),
                        stored: result.is_ok(),
                        reason: result
                            .err()
                            .map(PutError::reason)
                            .unwrap_or_default()
                            .into(),
                    },
                ))
                .await;
                Ok(())
            }
            Msg::FetchFinished { key, files } => {
                if self.discard_closed_fetch(key).await? {
                    return Ok(());
                }
                let Some(fetch) = self.fetching.remove(&key) else {
                    return Ok(());
                };
                match files {
                    Ok(files) => self.deliver(fetch.command.clone(), files).await,
                    Err(_) => reject(&self.shared, &fetch.command, "attachment_unavailable").await,
                }
            }
            Msg::AgentCallDone { call_id, result } => {
                let (result_json, error_code, error_message) = match result {
                    crate::adapter::AgentCallResult::Ok(value) => (
                        serde_json::to_string(&value).unwrap_or_default(),
                        String::new(),
                        String::new(),
                    ),
                    crate::adapter::AgentCallResult::Err { code, message } => {
                        (String::new(), code, message)
                    }
                };
                self.send(down::Msg::AgentCallResult(
                    canterlot_proto::worker::ipc::AgentCallResult {
                        call_id: call_id.to_string(),
                        result_json,
                        error_code,
                        error_message,
                    },
                ))
                .await;
                Ok(())
            }
        }
    }

    async fn on_up(&mut self, message: up::Msg) -> Result<(), ExecError> {
        match message {
            // После JournalClosed отказ пишется в CJ; остальные исходы приходят из SJ.
            up::Msg::DecisionAck(ack) => {
                if ack.result == "execution_closed" {
                    let execution = self.id.to_string();
                    let commands = self
                        .shared
                        .store
                        .call(move |slot| {
                            Ok(open(slot)?
                                .stored_commands()?
                                .into_iter()
                                .filter(|cmd| cmd.execution_id == execution)
                                .collect::<Vec<_>>())
                        })
                        .await?;
                    for command in commands {
                        if let Some(Kind::DeliverDecision(decision)) = &command.kind
                            && decision.request_id == ack.request_id
                            && decision.decision_id == ack.decision_id
                        {
                            decision_rejected(&self.shared, &command, decision, "request_closed")
                                .await?;
                        }
                    }
                }
            }
            up::Msg::DecisionOutcome(_) => {}
            up::Msg::Attached(_) => {}
            up::Msg::CliStarted(started) => self.on_cli_started(started).await?,
            up::Msg::AgentCall(call) => {
                let shared = self.shared.clone();
                let (id, generation) = (self.id, self.generation);
                if let Ok(call_id) = Uuid::parse_str(&call.call_id) {
                    let tool = call.tool.clone();
                    let arguments: serde_json::Value = serde_json::from_str(&call.arguments_json)
                        .unwrap_or(serde_json::Value::Null);
                    tokio::spawn(async move {
                        let caller = shared.agent_caller.borrow().clone();
                        let result =
                            super::agent_call(caller, id, generation, call_id, tool, arguments)
                                .await;
                        let sender = shared.actors.lock().expect("actors lock").get(&id).cloned();
                        if let Some(sender) = sender {
                            let _ = sender.send(Msg::AgentCallDone { call_id, result });
                        }
                    });
                }
            }
            up::Msg::DeliverAck(ack) => self.on_deliver_ack(ack).await?,
            up::Msg::SjBatch(batch) => self.on_batch(batch).await?,
            up::Msg::MediaUpload(request) => self.start_upload(request),
            up::Msg::ShareUpload(request) => self.start_share(request),
            up::Msg::ShareCancel(cancel) => {
                if let Ok(share_id) = Uuid::parse_str(&cancel.share_id) {
                    self.uploads.remove(&share_id);
                }
            }
            up::Msg::Exited(exited) => {
                self.cancel_fetches().await?;
                self.uploads.clear();
                self.finished = true;
                self.reconnect_at = None;
                let outcome = exited.reason;
                // Итог сопровождения виден в журнале воркера: отказ запуска не должен
                // оставаться только в `supervisor.log`, который удаляется с каталогом.
                if matches!(
                    outcome.as_str(),
                    "start_failed" | "crashed" | "protocol_incompatible"
                ) {
                    tracing::warn!(
                        execution_id = %self.id,
                        kind = "execution_exited",
                        outcome = %outcome,
                        "execution failed"
                    );
                } else {
                    tracing::info!(
                        execution_id = %self.id,
                        kind = "execution_exited",
                        outcome = %outcome,
                        "execution exited"
                    );
                }
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
            "generation_mismatch" | "execution_stopped" | "execution_closed" => {
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
            "cancel_won" => Body::CancelWon(InputCancelWon {
                command_id: ack.command_id,
                dispatch_attempt: ack.dispatch_attempt,
            }),
            "input_withdrawn" => Body::InputWithdrawn(InputWithdrawn {
                command_id: ack.command_id,
                dispatch_attempt: ack.dispatch_attempt,
            }),
            "not_applicable" => Body::CancelLost(InputCancelLost {
                command_id: ack.command_id,
                dispatch_attempt: ack.dispatch_attempt,
                reason: "transmission_started".into(),
            }),
            _ => return Ok(()),
        };
        self.shared.cj(vec![fact], terminal).await
    }

    async fn on_batch(&mut self, batch: SjBatch) -> Result<(), ExecError> {
        if Uuid::parse_str(&batch.journal_id).ok() != self.journal_id || batch.records.is_empty() {
            return Ok(());
        }
        let mut relayed = Relayed::default();
        for record in &batch.records {
            self.last_seq = self.last_seq.max(record.seq);
            let Some(body) = record.body.as_ref().and_then(|body| body.body.as_ref()) else {
                continue;
            };
            relayed.note(body);
            match body {
                Body::CancelWon(fact) => {
                    if let Ok(id) = fact.command_id.parse() {
                        self.handed.remove(&(id, fact.dispatch_attempt));
                    }
                }
                Body::InputWithdrawn(fact) => {
                    if let Ok(id) = fact.command_id.parse() {
                        self.handed.remove(&(id, fact.dispatch_attempt));
                    }
                }
                Body::ExecutionState(fact) => {
                    let (state, outcome) = (fact.state.clone(), fact.outcome.clone());
                    self.shared.with_info(self.id, |info| {
                        info.state = state;
                        info.outcome = outcome;
                    });
                }
                Body::JournalClosed(_) => self.closed_seq = Some(record.seq),
                // Причина отказа запуска приходит из журнала сопровождения с коротким кодом.
                Body::AdapterDiagnostic(fact) if fact.kind == "start_failed" => {
                    log_start_failure(self.id, &fact.event_type);
                }
                _ => {}
            }
        }
        let last = self.last_seq;
        self.shared.with_info(self.id, |info| info.last_seq = last);
        relayed.store(&self.shared, self.id).await?;
        if let Some(journal_id) = self.journal_id {
            self.outbox.push_back(SjRelay {
                journal_id,
                records: batch.records,
            });
        }
        Ok(())
    }

    async fn on_command(&mut self, command: Command) -> Result<(), ExecError> {
        if self.link.is_none()
            && !self.finished
            && !matches!(
                command.kind,
                Some(Kind::CancelInput(_) | Kind::WithdrawInput(_) | Kind::StopExecution(_))
            )
        {
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
            Some(Kind::DeliverInput(input)) => {
                self.deliver_input_like(command, input.content, live).await
            }
            Some(Kind::SteerInput(steer)) => {
                if live && !self.ipc_caps.contains(IPC_CAP_STEER) {
                    return reject(&self.shared, &command, CAPABILITY_MISSING).await;
                }
                self.deliver_input_like(command, steer.content, live).await
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
            Some(Kind::RemoveWorktree(_)) => reject_invalid(&self.shared, &command).await,
            // Хаб считает исполнение потерянным, а связь с ним цела: это и есть доказательство.
            Some(Kind::ResolveOwnerLost(_)) => {
                if live && command.execution_generation == self.generation {
                    reclaim_owner(&self.shared, self.id, self.generation).await?;
                }
                self.shared.cj(vec![], self_terminal).await
            }
            Some(Kind::CompactSession(_)) => {
                if live && command.execution_generation == self.generation {
                    // Сборка без сжатия порвала бы связь на незнакомом сообщении.
                    if !self.ipc_caps.contains(IPC_CAP_COMPACT) {
                        return reject(&self.shared, &command, CAPABILITY_MISSING).await;
                    }
                    if !self
                        .send(down::Msg::Compact(CompactRequest {
                            command_id: cid_text,
                            dispatch_attempt: attempt,
                        }))
                        .await
                    {
                        self.waiting.push_back(command);
                        return Ok(());
                    }
                    self.shared.cj(vec![], self_terminal).await
                } else {
                    let state = if command.execution_generation == self.generation {
                        CompactionState::Superseded
                    } else {
                        CompactionState::RejectedStale
                    };
                    self.shared
                        .cj(
                            vec![compaction_status(&command, state)],
                            terminal_self(&command),
                        )
                        .await
                }
            }
            Some(Kind::StopExecution(stop)) => {
                self.cancel_fetches().await?;
                if !live && !self.finished {
                    self.waiting.push_back(command);
                    return Ok(());
                }
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
            Some(Kind::DeliverDecision(decision)) => {
                if command.execution_generation != self.generation
                    || decision.generation != self.generation
                {
                    return decision_rejected(
                        &self.shared,
                        &command,
                        &decision,
                        "stale_generation",
                    )
                    .await;
                }
                if !live {
                    return decision_rejected(&self.shared, &command, &decision, "request_closed")
                        .await;
                }
                if !self.send(down::Msg::DeliverDecision(decision)).await {
                    self.waiting.push_back(command);
                }
                Ok(())
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
        if self.fetching.contains_key(&(target_id, target_attempt)) {
            self.cancel_fetch((target_id, target_attempt)).await;
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

    /// Общий путь ввода и steer: вложения, пилот, доставка сопровождению.
    /// Вид команды (`deliver_input`/`steer_input`) едет дальше как есть:
    /// сопровождение решает, открывать ход или вливать в идущий.
    async fn deliver_input_like(
        &mut self,
        command: Command,
        content: Vec<ContentBlock>,
        live: bool,
    ) -> Result<(), ExecError> {
        let (cid_text, attempt) = (command.command_id.clone(), command.dispatch_attempt);
        let self_terminal = vec![(cid_text.clone(), attempt)];
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
        if self.fetching.contains_key(&(cid, attempt)) {
            return Ok(());
        }
        let references: Vec<_> = content
            .into_iter()
            .filter_map(|b| match b.block {
                Some(content_block::Block::Attachment(r)) => Some(r),
                _ => None,
            })
            .collect();
        if references.is_empty() {
            return self.deliver(command, vec![]).await;
        }
        if self.discard_closed_fetch((cid, attempt)).await? {
            return Ok(());
        }
        let obsolete: Vec<_> = self
            .fetching
            .keys()
            .copied()
            .filter(|(id, old_attempt)| *id == cid && *old_attempt != attempt)
            .collect();
        for key in obsolete {
            self.cancel_fetch(key).await;
        }
        let command_id = cid_text;
        let pilot = self
            .shared
            .store
            .call(move |slot| {
                Ok(open(slot)?
                    .command_state(&command_id)?
                    .is_some_and(|s| s.pilot_v1))
            })
            .await?;
        if !pilot {
            return reject(&self.shared, &command, "attachment_unavailable").await;
        }
        if self.attachments.is_none() {
            let dir = self
                .exec_dir
                .as_ref()
                .ok_or(ExecError::Verify("exec_dir"))?;
            self.attachments = Some(Arc::new(AttachmentStore::with_slots(
                dir,
                self.shared.fetch_slots.clone(),
            )?));
        }
        self.start_fetch(command, references);
        Ok(())
    }

    async fn deliver(&mut self, command: Command, files: Vec<LocalFile>) -> Result<(), ExecError> {
        let id = Uuid::parse_str(&command.command_id).map_err(|_| ExecError::InvalidCommand)?;
        self.handed.insert((id, command.dispatch_attempt));
        if !self
            .send(down::Msg::Deliver(Deliver {
                command: Some(command.clone()),
                files,
            }))
            .await
        {
            self.waiting.push_back(command);
        }
        Ok(())
    }

    /// Одна загрузка на картинку; повторный запрос той же картинки ждёт уже идущую.
    fn start_upload(&mut self, request: MediaUpload) {
        let (Ok(media_id), Some(exec_dir)) =
            (Uuid::parse_str(&request.media_id), self.exec_dir.clone())
        else {
            return;
        };
        if self.uploads.contains_key(&media_id) {
            return;
        }
        let sender = self
            .shared
            .actors
            .lock()
            .expect("actors lock")
            .get(&self.id)
            .expect("actor sender")
            .clone();
        let sink = self.shared.media_sink.subscribe();
        let backoff_cap = self.shared.attachment_config.fetch_backoff_cap;
        let header = MediaHeader {
            media_id: request.media_id,
            execution_id: self.id.to_string(),
            block_id: request.block_id,
            mime: request.mime,
            size: request.size,
            sha256: request.sha256,
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let path = media::media_path(&exec_dir, media_id);
            let result = media::upload(sink, &path, header, backoff_cap).await;
            let _ = sender.send(Msg::MediaFinished { media_id, result });
        });
        self.uploads.insert(media_id, MediaTask(task));
    }

    fn start_share(&mut self, request: canterlot_proto::worker::ipc::ShareUpload) {
        let (Ok(share_id), Some(exec_dir)) =
            (Uuid::parse_str(&request.share_id), self.exec_dir.clone())
        else {
            return;
        };
        if self.uploads.contains_key(&share_id) {
            return;
        }
        let sender = self
            .shared
            .actors
            .lock()
            .expect("actors lock")
            .get(&self.id)
            .expect("actor sender")
            .clone();
        let sink = self.shared.media_sink.subscribe();
        let backoff_cap = self.shared.attachment_config.fetch_backoff_cap;
        // Тот же блок, что в записи BlockFile: без него хаб молча не свяжет файл,
        // поэтому угадывать нельзя — отказ возвращается агенту.
        let block_id = match Uuid::parse_str(&request.block_id) {
            Ok(id) if !id.is_nil() => id,
            _ => {
                let _ = sender.send(Msg::ShareFinished {
                    share_id,
                    result: Err(PutError::NoBlock),
                });
                return;
            }
        };
        let header = MediaHeader {
            media_id: request.share_id,
            execution_id: self.id.to_string(),
            block_id: block_id.to_string(),
            mime: request.mime,
            size: request.size,
            sha256: request.sha256,
            name: request.name,
            title: request.title,
            note: request.note,
            share: true,
            width: request.width,
            height: request.height,
        };
        let task = tokio::spawn(async move {
            let path = media::media_path(&exec_dir, share_id);
            let result = media::upload(sink, &path, header, backoff_cap).await;
            let _ = sender.send(Msg::ShareFinished { share_id, result });
        });
        self.uploads.insert(share_id, MediaTask(task));
    }

    fn start_fetch(&mut self, command: Command, references: Vec<AttachmentRef>) {
        let key = (
            Uuid::parse_str(&command.command_id).expect("validated command"),
            command.dispatch_attempt,
        );
        let store = self.attachments.as_ref().expect("attachment store").clone();
        let source = self.shared.attachment_source.subscribe();
        let config = &self.shared.attachment_config;
        let policy = RetryPolicy {
            content_attempts: config.fetch_attempts,
            retry: config.fetch_retry,
            backoff_cap: config.fetch_backoff_cap,
        };
        let sender = self
            .shared
            .actors
            .lock()
            .expect("actors lock")
            .get(&self.id)
            .expect("actor sender")
            .clone();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let shutdown = CancellationToken::new();
        let shutdown_rx = shutdown.clone();
        let refs = references.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for (position, reference) in refs.into_iter().enumerate() {
                let (store, source, policy, cancel, sender) = (
                    store.clone(),
                    source.clone(),
                    policy.clone(),
                    token.clone(),
                    sender.clone(),
                );
                tasks.spawn(async move {
                    let attachment_id = reference.attachment_id.clone();
                    let report = |error, retryable| {
                        let _ = sender.send(Msg::AttachmentProgress {
                            key,
                            attachment_id: attachment_id.clone(),
                            result: Err(error),
                            retryable,
                        });
                    };
                    let result = store
                        .ensure_reporting(source, &reference, &policy, cancel.clone(), report)
                        .await;
                    if cancel.is_cancelled() {
                        return Err(FetchError::Transport);
                    }
                    let message = Msg::AttachmentProgress {
                        key,
                        attachment_id,
                        result: result.as_ref().map(|_| ()).map_err(|e| *e),
                        retryable: false,
                    };
                    let _ = sender.send(message);
                    result.map(|p| {
                        (
                            position,
                            LocalFile {
                                path: p.path.to_string_lossy().into_owned(),
                                name: p.name,
                                mime: p.mime,
                                size: p.size,
                                image: p.image,
                            },
                        )
                    })
                });
            }
            let mut files = Vec::new();
            let mut failure = None;
            loop {
                let result = tokio::select! {
                    biased;
                    _=shutdown_rx.cancelled()=>{tasks.shutdown().await;return;}
                    result=tasks.join_next()=>result,
                };
                let Some(result) = result else {
                    break;
                };
                match result.unwrap_or(Err(FetchError::Io)) {
                    Ok(file) => files.push(file),
                    Err(error) => {
                        if failure.is_none() {
                            failure = Some(error);
                        }
                        token.cancel();
                    }
                }
            }
            files.sort_by_key(|(position, _)| *position);
            let result = failure.map_or_else(
                || Ok(files.into_iter().map(|(_, file)| file).collect()),
                Err,
            );
            let _ = sender.send(Msg::FetchFinished { key, files: result });
        });
        self.fetching.insert(
            key,
            FetchTask {
                command,
                references,
                cancel,
                shutdown,
                task,
            },
        );
    }

    async fn cancel_fetch(&mut self, key: (Uuid, u32)) {
        if let Some(mut fetch) = self.fetching.remove(&key) {
            fetch.cancel.cancel();
            let _ = (&mut fetch.task).await;
            if let Some(store) = &self.attachments {
                for r in &fetch.references {
                    if let Ok(id) = r.attachment_id.parse() {
                        store.remove_temp(id);
                    }
                }
            }
        }
    }

    async fn discard_closed_fetch(&mut self, key: (Uuid, u32)) -> Result<bool, ExecError> {
        let closed = self
            .shared
            .store
            .call(move |slot| {
                let store = open(slot)?;
                let id = key.0.to_string();
                let closed = store.delivery_is_closed(&id, key.1)?;
                if closed {
                    store.mark_terminal(&id, key.1)?;
                }
                Ok(closed)
            })
            .await?;
        if closed {
            self.cancel_fetch(key).await;
        }
        Ok(closed)
    }
    async fn cancel_fetches(&mut self) -> Result<(), ExecError> {
        let keys: Vec<_> = self.fetching.keys().copied().collect();
        for key in keys {
            let command = self.fetching.get(&key).map(|fetch| fetch.command.clone());
            self.cancel_fetch(key).await;
            if let Some(command) = command {
                defer_unhanded(&self.shared, &command).await?;
            }
        }
        Ok(())
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
    use canterlot_proto::worker::v1::{
        AdapterDiagnostic, DeliverInput, InputCancelLost, JournalRecord, RecordBody,
    };

    #[test]
    fn a_start_failure_reason_reaches_the_worker_log() {
        #[derive(Clone, Default)]
        struct Output(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let output = Output::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || writer.clone())
            .finish();
        let id = Uuid::now_v7();
        tracing::subscriber::with_default(subscriber, || log_start_failure(id, "unknown_profile"));
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("start_failed"), "{log}");
        assert!(log.contains("unknown_profile"), "{log}");
        assert!(log.contains(&id.to_string()), "{log}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_start_failed_diagnostic_is_logged_with_its_reason() {
        #[derive(Clone, Default)]
        struct Output(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("store")).unwrap();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let id = Uuid::now_v7();
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        let journal = Uuid::now_v7();
        actor.journal_id = Some(journal);
        let output = Output::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || writer.clone())
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        actor
            .on_batch(SjBatch {
                journal_id: journal.to_string(),
                records: vec![JournalRecord {
                    seq: 1,
                    body: Some(RecordBody {
                        body: Some(Body::AdapterDiagnostic(AdapterDiagnostic {
                            execution_id: id.to_string(),
                            kind: "start_failed".into(),
                            event_type: "session_create_failed".into(),
                            ..Default::default()
                        })),
                    }),
                }],
            })
            .await
            .unwrap();
        drop(guard);
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("start_failed"), "{log}");
        assert!(log.contains("session_create_failed"), "{log}");
        assert!(log.contains(&id.to_string()), "{log}");
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn closed_journal_decision_ack_is_persisted_in_cj() {
        let root = tempfile::tempdir().unwrap();
        let mut store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let command_id = Uuid::now_v7().to_string();
        let decision = canterlot_proto::worker::v1::DeliverDecision {
            request_id: Uuid::now_v7().to_string(),
            decision_id: Uuid::now_v7().to_string(),
            generation: 1,
            kind: "permission".into(),
            decision_payload_json: br#"{"kind":"permission","behavior":"allow"}"#.to_vec(),
        };
        store
            .accept_command(&Command {
                command_id: command_id.clone(),
                dispatch_attempt: 1,
                execution_id: id.to_string(),
                execution_generation: 1,
                kind: Some(Kind::DeliverDecision(decision.clone())),
            })
            .unwrap();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        actor
            .on_up(up::Msg::DecisionAck(
                canterlot_proto::worker::ipc::DecisionAck {
                    request_id: decision.request_id,
                    decision_id: decision.decision_id,
                    result: "execution_closed".into(),
                },
            ))
            .await
            .unwrap();
        thread.client().call(move |slot| {
            let store = open(slot)?;
            assert!(store.command_state(&command_id)?.unwrap().terminal);
            assert!(store.cj_records_from(1, usize::MAX)?.iter().any(|record|
                matches!(record.body.as_ref().and_then(|b| b.body.as_ref()), Some(Body::DecisionNotApplied(f)) if f.reason == "request_closed")));
            Ok(())
        }).await.unwrap();
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn cancel_and_withdraw_of_a_redispatched_fetch_are_local() {
        for cancel_input in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let mut store = WorkerStore::open(&root.path().join("store")).unwrap();
            let id = Uuid::now_v7();
            let cid = Uuid::now_v7();
            let input = Command {
                command_id: cid.to_string(),
                dispatch_attempt: 2,
                execution_id: id.to_string(),
                execution_generation: 1,
                kind: Some(Kind::DeliverInput(DeliverInput::default())),
            };
            let kind = if cancel_input {
                Kind::CancelInput(CancelInput {
                    target_command_id: cid.to_string(),
                    target_dispatch_attempt: 2,
                })
            } else {
                Kind::WithdrawInput(WithdrawInput {
                    target_command_id: cid.to_string(),
                    target_dispatch_attempt: 2,
                })
            };
            let command = Command {
                command_id: Uuid::now_v7().to_string(),
                dispatch_attempt: 1,
                execution_id: id.to_string(),
                execution_generation: 1,
                kind: Some(kind),
            };
            store.accept_command(&input).unwrap();
            store.accept_command(&command).unwrap();
            let thread = StoreThread::new(store);
            let (relay, _rx) = mpsc::channel(1);
            let manager = ExecutionManager::new_with_runtime_dir(
                ExecutionConfig::default(),
                thread.client(),
                relay,
                Some(root.path().join("runtime")),
            );
            let (_tx, rx) = mpsc::unbounded_channel();
            let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
            actor.handed.insert((cid, 1));
            let journal = Uuid::now_v7();
            actor.journal_id = Some(journal);
            actor
                .on_batch(SjBatch {
                    journal_id: journal.to_string(),
                    records: vec![JournalRecord {
                        seq: 1,
                        body: Some(RecordBody {
                            body: Some(Body::InputWithdrawn(InputWithdrawn {
                                command_id: cid.to_string(),
                                dispatch_attempt: 1,
                            })),
                        }),
                    }],
                })
                .await
                .unwrap();
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            actor.fetching.insert(
                (cid, 2),
                FetchTask {
                    command: input,
                    references: vec![],
                    cancel,
                    shutdown: CancellationToken::new(),
                    task: tokio::spawn(async move {
                        token.cancelled().await;
                    }),
                },
            );
            actor.on_command(command).await.unwrap();
            assert!(actor.fetching.is_empty());
            assert!(actor.waiting.is_empty());
            thread
                .client()
                .call(move |slot| {
                    let store = open(slot)?;
                    assert!(store.command_state(&cid.to_string())?.unwrap().terminal);
                    assert!(store.cj_records_from(1, usize::MAX)?.iter().any(|r| {
                        match r.body.as_ref().and_then(|b| b.body.as_ref()) {
                            Some(Body::CancelWon(f)) => cancel_input && f.dispatch_attempt == 2,
                            Some(Body::InputWithdrawn(f)) => {
                                !cancel_input && f.dispatch_attempt == 2
                            }
                            _ => false,
                        }
                    }));
                    Ok(())
                })
                .await
                .unwrap();
            drop(actor);
            manager.shutdown().await;
            thread.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn stop_during_ipc_reconnect_cancels_fetch_but_waits_for_delivery() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("store");
        let mut store = WorkerStore::open(&data).unwrap();
        let id = Uuid::now_v7();
        let cid = Uuid::now_v7();
        let input = Command {
            command_id: cid.to_string(),
            dispatch_attempt: 1,
            execution_id: id.to_string(),
            execution_generation: 1,
            kind: Some(Kind::DeliverInput(DeliverInput::default())),
        };
        let stop = Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            execution_id: id.to_string(),
            execution_generation: 1,
            kind: Some(Kind::StopExecution(Default::default())),
        };
        store.accept_command(&input).unwrap();
        store.accept_command(&stop).unwrap();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        let attachment = Uuid::now_v7();
        let files = Arc::new(AttachmentStore::new(root.path(), 2).unwrap());
        let temporary = root
            .path()
            .join("attachments/.tmp")
            .join(format!("{attachment}.part"));
        fs::write(&temporary, b"partial").unwrap();
        actor.attachments = Some(files);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        actor.fetching.insert(
            (cid, 1),
            FetchTask {
                command: input,
                references: vec![AttachmentRef {
                    attachment_id: attachment.to_string(),
                    ..Default::default()
                }],
                cancel,
                shutdown: CancellationToken::new(),
                task: tokio::spawn(async move {
                    token.cancelled().await;
                }),
            },
        );
        actor.on_command(stop.clone()).await.unwrap();
        assert!(actor.fetching.is_empty());
        assert!(!temporary.exists());
        assert_eq!(actor.waiting.len(), 1);
        let stop_id = stop.command_id;
        thread
            .client()
            .call(move |slot| {
                assert!(!open(slot)?.command_state(&stop_id)?.unwrap().terminal);
                Ok(())
            })
            .await
            .unwrap();
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn not_found_after_durable_cancel_receipt_is_silent() {
        let root = tempfile::tempdir().unwrap();
        let mut store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let cid = Uuid::now_v7();
        let command = Command {
            command_id: cid.to_string(),
            dispatch_attempt: 1,
            execution_id: id.to_string(),
            execution_generation: 1,
            kind: Some(Kind::DeliverInput(DeliverInput::default())),
        };
        store.accept_command_with_pilot(&command, true).unwrap();
        store
            .append_cj_fact(Body::CancelWon(InputCancelWon {
                command_id: cid.to_string(),
                dispatch_attempt: 1,
            }))
            .unwrap();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        actor.fetching.insert(
            (cid, 1),
            FetchTask {
                command,
                references: vec![],
                cancel: CancellationToken::new(),
                shutdown: CancellationToken::new(),
                task: tokio::spawn(async {}),
            },
        );
        actor
            .on_msg(Msg::FetchFinished {
                key: (cid, 1),
                files: Err(FetchError::NotFound),
            })
            .await
            .unwrap();
        thread
            .client()
            .call(move |slot| {
                let store = open(slot)?;
                assert!(store.command_state(&cid.to_string())?.unwrap().terminal);
                assert!(
                    !store
                        .cj_records_from(1, usize::MAX)?
                        .iter()
                        .any(|r| matches!(
                            r.body.as_ref().and_then(|b| b.body.as_ref()),
                            Some(Body::CommandRejected(_))
                        ))
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn release_defers_inputs_still_fetching() {
        let root = tempfile::tempdir().unwrap();
        let mut store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let cid = Uuid::now_v7();
        let input = Command {
            command_id: cid.to_string(),
            dispatch_attempt: 1,
            execution_id: id.to_string(),
            execution_generation: 1,
            kind: Some(Kind::DeliverInput(DeliverInput::default())),
        };
        store.accept_command_with_pilot(&input, true).unwrap();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        actor.fetching.insert(
            (cid, 1),
            FetchTask {
                command: input,
                references: vec![],
                cancel,
                shutdown: CancellationToken::new(),
                task: tokio::spawn(async move {
                    token.cancelled().await;
                }),
            },
        );
        actor.release().await.unwrap();
        assert!(actor.fetching.is_empty());
        thread
            .client()
            .call(move |slot| {
                let store = open(slot)?;
                assert!(store.command_state(&cid.to_string())?.unwrap().terminal);
                assert!(
                    store
                        .cj_records_from(1, usize::MAX)?
                        .iter()
                        .any(|r| matches!(
                            r.body.as_ref().and_then(|b| b.body.as_ref()),
                            Some(Body::DispatchDeferred(f)) if f.command_id == cid.to_string()
                        ))
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

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
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
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

    fn age_tree(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(8 * 3600);
        fs::File::open(path).unwrap().set_modified(old).unwrap();
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                age_tree(&entry.path());
            }
        }
    }

    #[tokio::test]
    async fn gc_keeps_build_when_ownership_appears_after_listing() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("store");
        let store = WorkerStore::open(&data).unwrap();
        let conversation = Uuid::now_v7();
        let worktree = data.join("worktrees").join(conversation.to_string());
        fs::create_dir_all(worktree.join("target/debug")).unwrap();
        fs::write(worktree.join("target/debug/old"), "old").unwrap();
        age_tree(&worktree.join("target"));

        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let client = thread.client();
        *BEFORE_RECHECK.lock().unwrap() = Some(std::sync::Arc::new(move |_| {
            let client = client.clone();
            Box::pin(async move {
                client
                    .call(move |slot| {
                        open(slot)?.ownership_mut()?.append(Ownership {
                            execution_id: Uuid::now_v7().to_string(),
                            conversation_id: conversation.to_string(),
                            phase: OwnershipPhase::Attached as i32,
                            ..Default::default()
                        })?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            })
        }));
        super::gc_worktrees(&manager.shared).await.unwrap();
        *BEFORE_RECHECK.lock().unwrap() = None;
        assert!(worktree.join("target/debug/old").exists());
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn sweep_keeps_socket_when_ownership_appears_after_listing() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("store");
        let store = WorkerStore::open(&data).unwrap();
        let id = Uuid::now_v7();
        let runtime = root.path().join("runtime");
        let sockets = runtime.join("canterlot");
        fs::create_dir_all(&sockets).unwrap();
        let socket = sockets.join(format!("{id}.sock"));
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(runtime.clone()),
        );
        let client = thread.client();
        let hook_socket = socket.clone();
        *BEFORE_RECHECK.lock().unwrap() = Some(std::sync::Arc::new(move |_| {
            let client = client.clone();
            let socket = hook_socket.clone();
            Box::pin(async move {
                client
                    .call(move |slot| {
                        open(slot)?.ownership_mut()?.append(Ownership {
                            execution_id: id.to_string(),
                            ipc_socket: socket.to_string_lossy().into_owned(),
                            phase: OwnershipPhase::Attached as i32,
                            ..Default::default()
                        })?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            })
        }));
        super::sweep_runtime(&manager.shared).await.unwrap();
        *BEFORE_RECHECK.lock().unwrap() = None;
        assert!(socket.exists());
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn sweep_keeps_execution_when_ownership_appears_after_listing() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("store");
        let store = WorkerStore::open(&data).unwrap();
        let id = Uuid::now_v7();
        let exec_dir = data.join("executions").join(id.to_string());
        fs::create_dir_all(&exec_dir).unwrap();
        age_tree(&exec_dir);

        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let client = thread.client();
        *BEFORE_RECHECK.lock().unwrap() = Some(std::sync::Arc::new(move |_| {
            let client = client.clone();
            Box::pin(async move {
                client
                    .call(move |slot| {
                        open(slot)?.ownership_mut()?.append(Ownership {
                            execution_id: id.to_string(),
                            phase: OwnershipPhase::Attached as i32,
                            ..Default::default()
                        })?;
                        Ok(())
                    })
                    .await
                    .unwrap();
            })
        }));
        super::sweep_runtime(&manager.shared).await.unwrap();
        *BEFORE_RECHECK.lock().unwrap() = None;
        assert!(exec_dir.exists());
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn share_without_block_id_is_refused_to_the_agent() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        actor.exec_dir = Some(root.path().to_owned());
        let (self_tx, mut self_rx) = mpsc::unbounded_channel();
        manager.shared.actors.lock().unwrap().insert(id, self_tx);
        for block_id in [String::new(), Uuid::nil().to_string()] {
            let share_id = Uuid::now_v7();
            actor
                .on_up(up::Msg::ShareUpload(
                    canterlot_proto::worker::ipc::ShareUpload {
                        share_id: share_id.to_string(),
                        call_id: Uuid::now_v7().to_string(),
                        name: "doc.txt".into(),
                        title: String::new(),
                        note: String::new(),
                        mime: "text/plain".into(),
                        size: 6,
                        sha256: "e".repeat(64),
                        width: 0,
                        height: 0,
                        block_id,
                    },
                ))
                .await
                .unwrap();
            assert!(actor.uploads.is_empty(), "nothing is uploaded");
            match self_rx.try_recv().expect("refusal") {
                Msg::ShareFinished {
                    share_id: refused,
                    result: Err(error),
                } => {
                    assert_eq!(refused, share_id);
                    assert_eq!(error.reason(), "share_block_missing");
                }
                _ => panic!("a share without a block id must be refused"),
            }
        }
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }

    #[tokio::test]
    async fn share_cancel_aborts_the_upload_task() {
        let root = tempfile::tempdir().unwrap();
        let store = WorkerStore::open(&root.path().join("store")).unwrap();
        let id = Uuid::now_v7();
        let thread = StoreThread::new(store);
        let (relay, _rx) = mpsc::channel(1);
        let manager = ExecutionManager::new_with_runtime_dir(
            ExecutionConfig::default(),
            thread.client(),
            relay,
            Some(root.path().join("runtime")),
        );
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut actor = Actor::new(manager.shared.clone(), id, 1, rx);
        let share_id = Uuid::now_v7();
        actor.uploads.insert(
            share_id,
            MediaTask(tokio::spawn(std::future::pending::<()>())),
        );
        actor
            .on_up(up::Msg::ShareCancel(
                canterlot_proto::worker::ipc::ShareCancel {
                    share_id: share_id.to_string(),
                },
            ))
            .await
            .unwrap();
        assert!(
            actor.uploads.is_empty(),
            "the worker upload task is cancelled"
        );
        drop(actor);
        manager.shutdown().await;
        thread.close().await.unwrap();
    }
}
