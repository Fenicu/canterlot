#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use canterlot_proto::worker::{
    ipc::{Ownership, OwnershipPhase},
    v1::{
        CancelInput, Command, ContentBlock, DeliverInput, Interrupt, InterruptState,
        ResolveOwnerLost, StartExecution, StopExecution, TextBlock, WithdrawInput, command::Kind,
        content_block, record_body::Body,
    },
};
use canterlot_worker::{
    config::{ExecutionConfig, SupervisorTimers},
    containment::{GROUP_FILE, GROUP_MARKER_ENV},
    exec::{ExecutionManager, SjRelay},
    session_store::StoreThread,
    store::{Accept, StoreError, WorkerStore},
};
use prost::Message;
use tempfile::TempDir;
use tokio::{sync::mpsc, time::timeout};
use uuid::Uuid;

#[path = "support/process_guard.rs"]
mod process_guard;
#[path = "support/trace.rs"]
mod trace;
use process_guard::ProcessGuard;

const WAIT: Duration = Duration::from_secs(20);

fn script(steps: &str) -> String {
    format!(r#"{{"synthetic":[{steps}]}}"#)
}

fn processes() -> Vec<(u32, u32, String)> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(end) = stat.rfind(')') else { continue };
        let fields: Vec<_> = stat[end + 2..].split_whitespace().collect();
        if fields.first().is_some_and(|s| *s == "Z" || *s == "X") {
            continue;
        }
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
            .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
            .unwrap_or_default();
        found.push((pid, fields[1].parse().unwrap_or(0), cmdline));
    }
    found
}

fn ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 2..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// Строка `/proc/<pid>/stat` после имени: для отладочных шагов.
fn proc_state(pid: u32) -> String {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat.rfind(')').map_or(stat.clone(), |end| {
            stat[end + 2..]
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ")
        }),
        Err(error) => format!("absent({:?})", error.kind()),
    }
}

fn alive(pid: u32) -> bool {
    processes().iter().any(|(p, ..)| *p == pid)
}

fn supervisors_of(root: &Path) -> Vec<u32> {
    let needle = root.to_string_lossy().into_owned();
    processes()
        .into_iter()
        .filter(|(_, _, cmdline)| cmdline.contains("supervise") && cmdline.contains(&needle))
        .map(|(pid, ..)| pid)
        .collect()
}

fn cli_children(supervisor: u32) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, ppid, cmdline)| *ppid == supervisor && cmdline.contains("synthetic-cli"))
        .map(|(pid, ..)| pid)
        .collect()
}

fn command(id: Uuid, attempt: u32, execution: Uuid, generation: u64, kind: Kind) -> Command {
    Command {
        command_id: id.to_string(),
        dispatch_attempt: attempt,
        execution_id: execution.to_string(),
        execution_generation: generation,
        kind: Some(kind),
    }
}

fn start_command(
    execution: Uuid,
    generation: u64,
    conversation: Uuid,
    harness: &str,
    workdir: &Path,
) -> Command {
    command(
        Uuid::now_v7(),
        1,
        execution,
        generation,
        Kind::StartExecution(StartExecution {
            conversation_id: conversation.to_string(),
            harness: harness.into(),
            workdir: workdir.to_string_lossy().into_owned(),
            native_conversation_id: String::new(),
            instructions: Some(canterlot_proto::worker::v1::Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
}

fn deliver_command(
    id: Uuid,
    attempt: u32,
    execution: Uuid,
    generation: u64,
    conversation: Uuid,
    text: &str,
) -> Command {
    command(
        id,
        attempt,
        execution,
        generation,
        Kind::DeliverInput(DeliverInput {
            input_id: Uuid::now_v7().to_string(),
            conversation_id: conversation.to_string(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
            }],

            ..Default::default()
        }),
    )
}

struct Env {
    guard: ProcessGuard,
    root: TempDir,
    work: PathBuf,
    config: ExecutionConfig,
    thread: Option<StoreThread>,
    manager: Option<ExecutionManager>,
    runtime: TempDir,
    relay: mpsc::Receiver<SjRelay>,
    bodies: Vec<Body>,
    seen: HashSet<(Uuid, u64)>,
    committed: HashMap<Uuid, u64>,
    auto_ack: bool,
    execution: Uuid,
    conversation: Uuid,
}

impl Drop for Env {
    fn drop(&mut self) {
        drop(self.manager.take());
    }
}

fn open_store(root: &Path) -> StoreThread {
    StoreThread::new(WorkerStore::open(&root.join("store")).unwrap())
}

impl Env {
    async fn new() -> Env {
        Self::with_supervisor_exe(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))).await
    }

    async fn with_supervisor_exe(supervisor_exe: PathBuf) -> Env {
        let root = TempDir::new().unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let config = ExecutionConfig {
            allowed_workdir_roots: vec![work.clone()],
            supervisor_exe: Some(supervisor_exe.clone()),
            spawn_timeout: Duration::from_secs(10),
            build_jobs: 7,
            supervisor: SupervisorTimers {
                stop_grace: Duration::from_secs(1),
                stop_retry: Duration::from_millis(200),
                ..Default::default()
            },
        };
        let thread = open_store(root.path());
        let (sender, relay) = mpsc::channel(64);
        let runtime = tempfile::Builder::new()
            .prefix("cl-rt-")
            .tempdir_in("/tmp")
            .unwrap();
        let manager = ExecutionManager::new_with_runtime_dir(
            config.clone(),
            thread.client(),
            sender,
            Some(runtime.path().to_path_buf()),
        );
        Env {
            guard: ProcessGuard::for_executable(root.path(), &supervisor_exe),
            root,
            work,
            config,
            thread: Some(thread),
            manager: Some(manager),
            runtime,
            relay,
            bodies: Vec::new(),
            seen: HashSet::new(),
            committed: HashMap::new(),
            auto_ack: true,
            execution: Uuid::now_v7(),
            conversation: Uuid::now_v7(),
        }
    }

    fn manager(&self) -> &ExecutionManager {
        self.manager.as_ref().unwrap()
    }

    fn thread(&self) -> &StoreThread {
        self.thread.as_ref().unwrap()
    }

    async fn send(&self, command: Command) {
        let copy = command.clone();
        let accept = self
            .thread()
            .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
            .await
            .unwrap();
        assert!(
            matches!(
                accept,
                Accept::Stored { .. } | Accept::Redelivered { .. } | Accept::Duplicate
            ),
            "{accept:?}"
        );
        self.manager().handle(command, accept).await.unwrap();
    }

    async fn start(&self, generation: u64) {
        let command = start_command(
            self.execution,
            generation,
            self.conversation,
            "synthetic",
            &self.work,
        );
        self.send(command).await;
    }

    fn hub_session(&self) {
        self.manager().on_hub_session(self.committed.clone());
    }

    async fn pump(&mut self, within: Duration) -> bool {
        match timeout(within, self.relay.recv()).await {
            Ok(Some(batch)) => {
                let mut last = 0;
                for record in batch.records {
                    last = last.max(record.seq);
                    if self.seen.insert((batch.journal_id, record.seq)) {
                        self.bodies.push(record.body.unwrap().body.unwrap());
                    }
                }
                let entry = self.committed.entry(batch.journal_id).or_default();
                *entry = (*entry).max(last);
                if self.auto_ack {
                    self.manager().on_sj_ack(batch.journal_id, last);
                }
                true
            }
            _ => false,
        }
    }

    async fn wait(&mut self, what: &str, wanted: impl Fn(&[Body]) -> bool) {
        let outcome = timeout(WAIT, async {
            loop {
                if wanted(&self.bodies) {
                    return true;
                }
                if !self.pump(Duration::from_secs(5)).await {
                    return wanted(&self.bodies);
                }
            }
        })
        .await;
        assert!(
            outcome.unwrap_or(false),
            "timed out waiting for: {what}; bodies: {:?}; supervisor logs: {}",
            self.bodies.iter().map(describe).collect::<Vec<_>>(),
            self.logs()
        );
    }

    fn logs(&self) -> String {
        let mut text = String::new();
        if let Ok(entries) = fs::read_dir(self.root.path().join("store/executions")) {
            for entry in entries.flatten() {
                text +=
                    &fs::read_to_string(entry.path().join("supervisor.log")).unwrap_or_default();
            }
        }
        text
    }

    async fn wait_running(&mut self) {
        self.wait("running", |b| b.iter().any(|x| state(x, "running")))
            .await;
    }

    async fn cj(&self) -> Vec<Body> {
        self.thread()
            .call(|store| {
                let store = store.as_ref().unwrap();
                Ok(store
                    .cj_records_from(store.cj_cursor()?.first_stored_seq, usize::MAX)?
                    .into_iter()
                    .filter_map(|record| record.body.and_then(|body| body.body))
                    .collect())
            })
            .await
            .unwrap()
    }

    async fn wait_cj(&self, what: &str, wanted: impl Fn(&[Body]) -> bool) -> Vec<Body> {
        timeout(WAIT, async {
            loop {
                let cj = self.cj().await;
                if wanted(&cj) {
                    return cj;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for CJ: {what}; supervisor logs: {}",
                self.logs()
            )
        })
    }

    async fn ownership(&self, id: Uuid) -> Option<Ownership> {
        self.thread()
            .call(move |store| Ok(store.as_ref().unwrap().ownership().get(id).cloned()))
            .await
            .unwrap()
    }

    async fn stored_commands(&self) -> usize {
        self.thread()
            .call(|store| Ok(store.as_ref().unwrap().stored_commands()?.len()))
            .await
            .unwrap()
    }

    async fn terminal(&self, command: &Command) -> bool {
        let id = command.command_id.clone();
        self.thread()
            .call(move |store| Ok(store.as_ref().unwrap().command_state(&id)?))
            .await
            .unwrap()
            .is_some_and(|state| state.terminal)
    }

    async fn close_manager(&mut self) {
        self.manager.take().unwrap().shutdown().await;
        self.thread.take().unwrap().close().await.unwrap();
    }

    fn reopen(&mut self) {
        let thread = open_store(self.root.path());
        let (sender, relay) = mpsc::channel(64);
        self.manager = Some(ExecutionManager::new_with_runtime_dir(
            self.config.clone(),
            thread.client(),
            sender,
            Some(self.runtime.path().to_path_buf()),
        ));
        self.thread = Some(thread);
        self.relay = relay;
    }

    async fn stop(&mut self) {
        let stop = command(
            Uuid::now_v7(),
            1,
            self.execution,
            self.generation_of_execution(),
            Kind::StopExecution(StopExecution { force: true }),
        );
        self.send(stop).await;
        self.wait("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    }

    fn generation_of_execution(&self) -> u64 {
        self.manager()
            .observations()
            .iter()
            .find(|o| o.execution_id == self.execution.to_string())
            .map_or(1, |o| o.generation)
    }
}

fn kind(body: &Body) -> &'static str {
    match body {
        Body::JournalOpened(_) => "journal_opened",
        Body::ExecutionState(_) => "execution_state",
        Body::PhaseChanged(_) => "phase_changed",
        Body::TransmissionStarted(_) => "transmission_started",
        Body::InputAccepted(_) => "input_accepted",
        Body::CommandRejected(_) => "command_rejected",
        Body::CancelWon(_) => "cancel_won",
        Body::InputWithdrawn(_) => "input_withdrawn",
        Body::InterruptStatus(_) => "interrupt_status",
        Body::BlockStarted(_) => "block_started",
        Body::BlockDelta(_) => "block_delta",
        Body::BlockFinal(_) => "block_final",
        Body::RunFinished(_) => "run_finished",
        Body::DispatchDeferred(_) => "dispatch_deferred",
        Body::BackgroundWorkChanged(_) => "background_work_changed",
        Body::JournalClosed(_) => "journal_closed",
        Body::CommandStored(_) => "command_stored",
        _ => "other",
    }
}

fn describe(body: &Body) -> String {
    match body {
        Body::ExecutionState(f) => format!("execution_state({},{})", f.state, f.outcome),
        Body::AcceptanceUnconfirmed(f) => format!("acceptance_unconfirmed({})", f.reason),
        Body::AdapterDiagnostic(f) => format!("adapter_diagnostic({})", f.kind),
        other => kind(other).to_owned(),
    }
}

fn state(body: &Body, wanted: &str) -> bool {
    matches!(body, Body::ExecutionState(fact) if fact.state == wanted)
}

fn finished(bodies: &[Body], id: Uuid) -> bool {
    bodies
        .iter()
        .any(|b| matches!(b, Body::RunFinished(f) if f.input_command_id == id.to_string()))
}

fn rejected_reasons(cj: &[Body]) -> Vec<String> {
    cj.iter()
        .filter_map(|b| match b {
            Body::CommandRejected(f) => Some(f.reason.clone()),
            _ => None,
        })
        .collect()
}

fn owner_lost(cj: &[Body], id: Uuid) -> usize {
    cj.iter()
        .filter(|b| {
            matches!(b, Body::ExecutionState(f) if f.state == "owner_lost" && f.execution_id == id.to_string())
        })
        .count()
}

#[tokio::test]
async fn start_execution_spawns_supervisor_and_relays_running() {
    let mut env = Env::new().await;
    env.hub_session();
    let start = start_command(env.execution, 1, env.conversation, "synthetic", &env.work);
    env.send(start.clone()).await;
    env.wait_running().await;
    assert!(
        matches!(&env.bodies[0], Body::JournalOpened(f) if f.execution_id == env.execution.to_string())
    );
    let dir = env
        .root
        .path()
        .join("store/executions")
        .join(env.execution.to_string());
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in ["spec.toml", "secret"] {
        assert_eq!(
            fs::metadata(dir.join(file)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(fs::read(dir.join("secret")).unwrap().len(), 32);
    let observations = env.manager().observations();
    assert_eq!(observations.len(), 1);
    assert!(observations[0].supervisor_attached);
    assert_eq!(observations[0].generation, 1);
    assert!(!observations[0].sj_journal_id.is_empty());
    assert_eq!(env.manager().journal_cursors().len(), 1);
    assert!(
        env.terminal(&start).await,
        "the start command is settled once the CLI started"
    );
    let text = Uuid::now_v7();
    env.send(deliver_command(
        text,
        1,
        env.execution,
        1,
        env.conversation,
        "hello",
    ))
    .await;
    env.wait("answer", |b| finished(b, text)).await;
    env.stop().await;
}

#[tokio::test]
async fn start_with_configured_adapters_reaches_running() {
    let mut env = Env::new().await;
    env.manager().configure_adapters(
        Some(Default::default()),
        Some(Default::default()),
        Some(Default::default()),
        None,
    );
    env.hub_session();
    let start = start_command(env.execution, 1, env.conversation, "synthetic", &env.work);
    env.send(start).await;
    env.wait_running().await;
    let spec = fs::read_to_string(
        env.root
            .path()
            .join("store/executions")
            .join(env.execution.to_string())
            .join("spec.toml"),
    )
    .unwrap();
    assert!(spec.contains("[claude_code]"), "{spec}");
    assert!(spec.contains("[codex]"), "{spec}");
    env.stop().await;
}

#[tokio::test]
async fn execution_probes_carry_supervisor_and_cli_identities() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let probes = env.manager().probes();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].execution_id, env.execution);
    assert_eq!(probes[0].members.len(), 2);
    for (pid, start_ticks) in &probes[0].members {
        assert_eq!(ticks(*pid), Some(*start_ticks));
    }
    env.stop().await;
}

#[tokio::test]
async fn ownership_is_durable_before_cli_start() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    assert_eq!(record.phase, OwnershipPhase::Attached as i32);
    assert!(
        record.supervisor_pid > 0
            && ticks(record.supervisor_pid) == Some(record.supervisor_start_ticks)
    );
    let journal = env.manager().observations()[0].sj_journal_id.clone();
    assert_eq!(record.sj_journal_id, journal);
    assert_eq!(record.secret_sha256.len(), 32);
    assert!(!record.boot_id.is_empty());
    let expected = env
        .runtime
        .path()
        .join("canterlot")
        .join(format!("{}.sock", env.execution));
    assert_eq!(Path::new(&record.ipc_socket), expected);
    env.stop().await;
}

#[tokio::test]
async fn startup_sweeps_stale_runtime_state() {
    let mut env = Env::new().await;
    let sockets = env.runtime.path().join("canterlot");
    fs::create_dir_all(&sockets).unwrap();
    let id = Uuid::now_v7();
    let path = sockets.join(format!("{id}.sock"));
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let worktree = env
        .root
        .path()
        .join("store/worktrees")
        .join(Uuid::now_v7().to_string());
    fs::create_dir_all(worktree.join("target/debug/deps")).unwrap();
    fs::write(
        worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();
    fs::write(worktree.join("target/debug/deps/old"), "old").unwrap();
    let old = SystemTime::now() - Duration::from_secs(8 * 3600);
    for path in [
        worktree.join("target/debug/deps/old"),
        worktree.join("target/debug/deps"),
        worktree.join("target"),
        worktree.clone(),
    ] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }
    env.manager().startup_verify().await.unwrap();
    assert!(!path.exists());
    assert!(!worktree.join("target").exists());
    env.close_manager().await;
}

#[tokio::test]
async fn spawned_cli_gets_cargo_build_jobs() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let environ = fs::read(format!("/proc/{}/environ", record.cli_pid)).unwrap();
    let value = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"CARGO_BUILD_JOBS="))
        .map(|value| String::from_utf8_lossy(value).into_owned());
    let expected = std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "7".into());
    assert_eq!(value.as_deref(), Some(expected.as_str()));
    env.stop().await;
}

#[tokio::test]
async fn start_rejected_for_unknown_harness_outside_roots_and_old_generation() {
    let mut env = Env::new().await;
    let outside = TempDir::new().unwrap();
    let conversation = Uuid::now_v7();
    env.send(start_command(
        Uuid::now_v7(),
        1,
        conversation,
        "no-such-harness",
        &env.work,
    ))
    .await;
    env.send(start_command(
        Uuid::now_v7(),
        1,
        conversation,
        "synthetic",
        outside.path(),
    ))
    .await;
    let first = Uuid::now_v7();
    env.send(start_command(
        first,
        3,
        conversation,
        "synthetic",
        &env.work,
    ))
    .await;
    env.send(start_command(
        Uuid::now_v7(),
        3,
        conversation,
        "synthetic",
        &env.work,
    ))
    .await;
    let cj = env
        .wait_cj("three rejections", |cj| rejected_reasons(cj).len() >= 3)
        .await;
    let mut reasons = rejected_reasons(&cj);
    reasons.sort();
    assert_eq!(
        reasons,
        [
            "generation_mismatch",
            "harness_unavailable",
            "workdir_not_allowed"
        ]
    );
    env.manager().set_enabled(false);
    env.send(start_command(
        Uuid::now_v7(),
        9,
        conversation,
        "synthetic",
        &env.work,
    ))
    .await;
    let cj = env
        .wait_cj("capability rejection", |cj| rejected_reasons(cj).len() >= 4)
        .await;
    assert!(rejected_reasons(&cj).contains(&"hub_capability_missing".to_owned()));
    env.hub_session();
    env.execution = first;
    env.wait_running().await;
    for entry in fs::read_dir(env.root.path().join("store/executions")).unwrap() {
        assert_eq!(
            entry.unwrap().file_name().to_string_lossy(),
            first.to_string(),
            "a rejected start never creates an execution directory"
        );
    }
    env.stop().await;
}

#[tokio::test]
async fn control_restart_reattaches_live_supervisor() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let before = env.ownership(env.execution).await.unwrap();
    let answer = Uuid::now_v7();
    env.send(deliver_command(
        answer,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"delta":"a"},{"delay_ms":700},{"delta":"b"}"#),
    ))
    .await;
    env.wait("first delta", |b| {
        b.iter().any(|x| matches!(x, Body::BlockDelta(_)))
    })
    .await;
    env.close_manager().await;
    assert!(
        alive(before.supervisor_pid),
        "the supervisor survives the control service"
    );
    env.reopen();
    let attached = env.manager().startup_verify().await.unwrap();
    assert_eq!(attached, vec![env.execution]);
    let after = env.ownership(env.execution).await.unwrap();
    assert_eq!(
        (after.supervisor_pid, after.supervisor_start_ticks),
        (before.supervisor_pid, before.supervisor_start_ticks)
    );
    assert_eq!(
        ticks(after.supervisor_pid),
        Some(before.supervisor_start_ticks)
    );
    assert!(env.manager().observations()[0].supervisor_attached);
    let cursor = env.manager().journal_cursors().pop().unwrap();
    assert!(cursor.last_seq >= env.committed[&Uuid::parse_str(&cursor.journal_id).unwrap()]);
    assert!(cursor.acked_seq_known_to_worker > 0);
    env.hub_session();
    env.wait("answer completes", |b| finished(b, answer)).await;
    let text = env
        .bodies
        .iter()
        .rev()
        .find_map(|b| match b {
            Body::BlockFinal(f) => Some(f.text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(text, "ab");
    assert_eq!(
        cli_children(before.supervisor_pid).len(),
        1,
        "no second CLI"
    );
    assert_eq!(supervisors_of(env.root.path()), vec![before.supervisor_pid]);
    env.stop().await;
}

const SPEC_B644882: &str = include_str!("fixtures/spec/b644882ac458.toml");

// Подставляет в спеку живого исполнения таблицы, которые записал старый релиз.
fn age_spec(exec_dir: &Path) {
    let path = exec_dir.join("spec.toml");
    let mut spec: toml::Table = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let frozen: toml::Table = toml::from_str(SPEC_B644882).unwrap();
    for table in ["claude_code", "codex", "opencode", "timers"] {
        spec.insert(table.into(), frozen[table].clone());
    }
    fs::write(path, toml::to_string(&spec).unwrap()).unwrap();
}

#[tokio::test]
async fn spec_of_older_release_is_reattached() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    env.close_manager().await;
    age_spec(Path::new(&record.exec_dir));
    env.reopen();
    assert_eq!(
        env.manager().startup_verify().await.unwrap(),
        vec![env.execution]
    );
    assert_eq!(owner_lost(&env.cj().await, env.execution), 0);
    assert!(env.manager().observations()[0].supervisor_attached);
    env.hub_session();
    env.stop().await;
}

fn owner_reclaimed(cj: &[Body], id: Uuid) -> usize {
    cj.iter()
        .filter(|b| {
            matches!(b, Body::ExecutionState(f)
                if f.outcome == canterlot_proto::OWNER_RECLAIMED && f.execution_id == id.to_string())
        })
        .count()
}

impl Env {
    fn exec_dir(&self) -> PathBuf {
        self.root
            .path()
            .join("store/executions")
            .join(self.execution.to_string())
    }

    async fn ack_cj(&self) {
        let seq = self
            .thread()
            .call(|store| {
                let store = store.as_mut().unwrap();
                let seq = store.cj_cursor()?.last_seq;
                store.on_cj_ack(seq)?;
                Ok(seq)
            })
            .await
            .unwrap();
        self.manager().on_cj_ack(seq);
    }

    /// Повторяет сбой обновления: спека не читается, хаб подтвердил `owner_lost`.
    async fn lose_owner_to_a_bad_spec(&mut self) -> String {
        let spec = self.exec_dir().join("spec.toml");
        let text = fs::read_to_string(&spec).unwrap();
        self.close_manager().await;
        fs::write(&spec, "[broken").unwrap();
        self.reopen();
        assert!(self.manager().startup_verify().await.unwrap().is_empty());
        assert_eq!(owner_lost(&self.cj().await, self.execution), 1);
        text
    }

    async fn restart_with_spec(&mut self, spec: &str) -> Vec<Uuid> {
        self.close_manager().await;
        fs::write(self.exec_dir().join("spec.toml"), spec).unwrap();
        age_spec(&self.exec_dir());
        self.reopen();
        self.manager().startup_verify().await.unwrap()
    }
}

#[tokio::test]
async fn reported_owner_lost_is_reclaimed_from_the_same_supervisor() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let before = env.ownership(env.execution).await.unwrap();
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    assert_eq!(
        env.ownership(env.execution).await.unwrap().released_reason,
        "owner_lost_reported"
    );
    assert!(env.manager().observations().is_empty());
    let answer = Uuid::now_v7();
    env.send(deliver_command(
        answer,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"delta":"after loss"}"#),
    ))
    .await;
    assert_eq!(
        env.stored_commands().await,
        1,
        "input for a lost execution waits in the inbox"
    );

    assert_eq!(env.restart_with_spec(&spec).await, vec![env.execution]);
    let cj = env.cj().await;
    assert_eq!(owner_reclaimed(&cj, env.execution), 1);
    assert_eq!(owner_lost(&cj, env.execution), 0, "not reported again");
    let after = env.ownership(env.execution).await.unwrap();
    assert!(after.released_reason.is_empty());
    assert!(after.reclaimed_cj_seq > 0);
    assert_eq!(
        (after.supervisor_pid, after.supervisor_start_ticks),
        (before.supervisor_pid, before.supervisor_start_ticks)
    );
    let observations = env.manager().observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].state, "running");
    assert!(observations[0].supervisor_attached);
    assert_eq!(env.manager().journal_cursors().len(), 1);
    env.hub_session();
    env.wait("input after reclaim", |b| finished(b, answer))
        .await;
    assert_eq!(
        cli_children(before.supervisor_pid).len(),
        1,
        "no second CLI"
    );
    assert_eq!(supervisors_of(env.root.path()), vec![before.supervisor_pid]);

    // Подтверждённый ранее `owner_lost` ещё лежит в CJ: он не должен снова снять владение.
    env.ack_cj().await;
    assert!(
        env.ownership(env.execution)
            .await
            .unwrap()
            .released_reason
            .is_empty()
    );
    env.close_manager().await;
    env.reopen();
    assert!(
        env.ownership(env.execution)
            .await
            .unwrap()
            .released_reason
            .is_empty()
    );
    assert_eq!(
        env.manager().startup_verify().await.unwrap(),
        vec![env.execution]
    );
    assert_eq!(
        owner_reclaimed(&env.cj().await, env.execution),
        0,
        "nothing to prove after a normal restart"
    );
    env.hub_session();
    env.stop().await;
}

#[tokio::test]
async fn unacknowledged_owner_lost_is_reclaimed_and_stays_observed() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let spec = env.lose_owner_to_a_bad_spec().await;
    assert_eq!(env.manager().observations()[0].state, "owner_lost");
    assert_eq!(env.restart_with_spec(&spec).await, vec![env.execution]);
    assert_eq!(env.manager().observations()[0].state, "running");
    env.ack_cj().await;
    let observations = env.manager().observations();
    assert_eq!(
        observations.len(),
        1,
        "the reclaimed execution is not retired"
    );
    assert_eq!(observations[0].state, "running");
    assert!(
        env.ownership(env.execution)
            .await
            .unwrap()
            .released_reason
            .is_empty()
    );
    env.hub_session();
    env.stop().await;
}

#[tokio::test]
async fn crash_between_reclaim_fact_and_ownership_repeats_the_proof() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    let (id, generation) = (env.execution, 1);
    env.thread()
        .call(move |store| {
            store
                .as_mut()
                .unwrap()
                .append_cj_fact(Body::ExecutionState(
                    canterlot_proto::worker::v1::ExecutionState {
                        execution_id: id.to_string(),
                        generation,
                        state: "running".into(),
                        outcome: canterlot_proto::OWNER_RECLAIMED.into(),
                        native_conversation_id: String::new(),
                    },
                ))?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(env.restart_with_spec(&spec).await, vec![env.execution]);
    assert_eq!(owner_reclaimed(&env.cj().await, env.execution), 2);
    env.hub_session();
    env.stop().await;
}

#[tokio::test]
async fn second_loss_after_a_reclaim_is_reported_again() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    assert_eq!(env.restart_with_spec(&spec).await, vec![env.execution]);
    env.ack_cj().await;
    let record = env.ownership(env.execution).await.unwrap();
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    let id = env.execution;
    env.wait_cj("second loss", |cj| owner_lost(cj, id) == 1)
        .await;
    env.ack_cj().await;
    assert_eq!(
        env.ownership(env.execution).await.unwrap().released_reason,
        "owner_lost_reported"
    );
    assert!(env.manager().observations().is_empty());
}

#[tokio::test]
async fn reclaim_needs_the_same_supervisor_boot_secret_and_generation() {
    let tamper: [fn(&mut Ownership); 4] = [
        |record| record.supervisor_start_ticks += 1,
        |record| record.supervisor_pid = std::process::id(),
        |record| record.secret_sha256[0] ^= 0xff,
        |record| record.generation += 1,
    ];
    for change in tamper {
        let mut env = Env::new().await;
        env.config.supervisor.attach_wait = Duration::from_millis(250);
        env.hub_session();
        env.start(1).await;
        env.wait_running().await;
        let spec = env.lose_owner_to_a_bad_spec().await;
        env.ack_cj().await;
        let mut record = env.ownership(env.execution).await.unwrap();
        change(&mut record);
        env.thread()
            .call(move |store| {
                store.as_mut().unwrap().ownership_mut()?.append(record)?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(env.restart_with_spec(&spec).await.is_empty());
        let cj = env.cj().await;
        assert_eq!(owner_reclaimed(&cj, env.execution), 0);
        assert_eq!(owner_lost(&cj, env.execution), 0, "not reported again");
        assert!(env.manager().observations().is_empty());
        assert_eq!(
            env.ownership(env.execution).await.unwrap().released_reason,
            "owner_lost_reported"
        );
    }
}

#[tokio::test]
async fn dead_supervisor_is_never_reclaimed() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    while alive(record.supervisor_pid) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.restart_with_spec(&spec).await.is_empty());
    assert_eq!(owner_reclaimed(&env.cj().await, env.execution), 0);
    assert!(env.manager().observations().is_empty());
    env.hub_session();
    env.wait_exit("supervisor_died").await;
    env.assert_released("supervisor_died").await;
}

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn ownership_not_verified_logs_the_reason() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    env.close_manager().await;
    fs::write(Path::new(&record.exec_dir).join("spec.toml"), "[broken").unwrap();
    env.reopen();
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    drop(guard);
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("ownership not verified")
            && logs.contains("supervisor verification failed: spec"),
        "{logs}"
    );
}

#[tokio::test]
async fn unverifiable_ownership_is_reported_owner_lost_never_restarted() {
    let mut env = Env::new().await;
    env.hub_session();
    let mut executions = Vec::new();
    for _ in 0..3 {
        env.execution = Uuid::now_v7();
        env.conversation = Uuid::now_v7();
        env.start(1).await;
        let wanted = executions.len() + 1;
        env.wait("running", |b| {
            b.iter().filter(|x| state(x, "running")).count() == wanted
        })
        .await;
        executions.push(env.execution);
    }
    let mut unrelated = std::process::Command::new("sleep")
        .arg("10")
        .spawn()
        .unwrap();
    let sleeper = unrelated.id();
    let sleeper_ticks = ticks(sleeper).unwrap();
    let mut originals = supervisors_of(env.root.path());
    assert_eq!(originals.len(), 3);
    env.close_manager().await;
    env.config.supervisor.attach_wait = Duration::from_millis(250);
    env.reopen();
    for (index, id) in executions.iter().copied().enumerate() {
        let mut record = env.ownership(id).await.unwrap();
        match index {
            0 => {
                record.supervisor_pid = sleeper;
                record.supervisor_start_ticks = sleeper_ticks;
            }
            1 => record.supervisor_start_ticks += 1,
            _ => record.secret_sha256[0] ^= 0xff,
        }
        env.thread()
            .call(move |store| {
                store.as_mut().unwrap().ownership_mut()?.append(record)?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let verify_started = std::time::Instant::now();
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    assert!(verify_started.elapsed() >= env.config.supervisor.attach_wait);
    let cj = env.cj().await;
    for id in &executions {
        assert_eq!(
            owner_lost(&cj, *id),
            1,
            "exactly one owner_lost per execution"
        );
    }
    let observations = env.manager().observations();
    assert_eq!(observations.len(), 3);
    assert!(
        observations
            .iter()
            .all(|o| o.state == "owner_lost" && !o.supervisor_attached)
    );
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "an unrelated process is never signalled"
    );
    let mut now = supervisors_of(env.root.path());
    now.sort();
    originals.sort();
    assert_eq!(now, originals, "nothing was restarted");
    env.close_manager().await;
    env.reopen();
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    let cj = env.cj().await;
    for id in &executions {
        assert_eq!(owner_lost(&cj, *id), 1, "reported once across restarts");
        assert_eq!(
            env.ownership(*id).await.unwrap().phase,
            OwnershipPhase::Attached as i32,
            "ownership is kept"
        );
    }
    unrelated.wait().unwrap();
}

#[tokio::test]
async fn supervisor_death_reports_owner_lost() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let input = Uuid::now_v7();
    env.send(deliver_command(
        input,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"detach_child":true}"#),
    ))
    .await;
    env.wait("detached descendant", |b| finished(b, input))
        .await;
    let detached = processes()
        .into_iter()
        .find_map(|(pid, _, cmd)| {
            (cmd.contains("__detach_sleep")
                && fs::read_link(format!("/proc/{pid}/cwd"))
                    .is_ok_and(|cwd| cwd.starts_with(env.root.path())))
            .then_some(pid)
        })
        .expect("detached CLI descendant");
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    let id = env.execution;
    env.wait_cj("owner_lost", |cj| owner_lost(cj, id) == 1)
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(owner_lost(&env.cj().await, id), 1, "reported once");
    assert!(supervisors_of(env.root.path()).is_empty(), "no respawn");
    assert!(
        alive(detached),
        "CLI descendant is left alone after supervisor death"
    );
    assert_eq!(
        env.ownership(id).await.unwrap().phase,
        OwnershipPhase::Attached as i32,
        "nothing is released"
    );
    let observations = env.manager().observations();
    assert_eq!(observations[0].state, "owner_lost");
    assert!(!observations[0].supervisor_attached);
}

fn exit_of(cj: &[Body], id: Uuid) -> Option<(String, String)> {
    cj.iter().find_map(|b| match b {
        Body::ExecutionState(f)
            if f.execution_id == id.to_string()
                && matches!(f.state.as_str(), "stopped" | "failed") =>
        {
            Some((f.state.clone(), f.outcome.clone()))
        }
        _ => None,
    })
}

fn resolve_command(execution: Uuid, generation: u64) -> Command {
    command(
        Uuid::now_v7(),
        1,
        execution,
        generation,
        Kind::ResolveOwnerLost(ResolveOwnerLost {}),
    )
}

impl Env {
    /// Оставляет потомка вне сессии CLI: он переживает и CLI, и сопровождение.
    async fn detach_descendant(&mut self) -> u32 {
        let input = Uuid::now_v7();
        self.send(deliver_command(
            input,
            1,
            self.execution,
            1,
            self.conversation,
            &script(r#"{"detach_child":true}"#),
        ))
        .await;
        self.wait("detached descendant", |b| finished(b, input))
            .await;
        processes()
            .into_iter()
            .find_map(|(pid, _, cmd)| {
                (cmd.contains("__detach_sleep")
                    && fs::read_link(format!("/proc/{pid}/cwd"))
                        .is_ok_and(|cwd| cwd.starts_with(self.root.path())))
                .then_some(pid)
            })
            .expect("detached CLI descendant")
    }

    async fn kill_supervisor(&mut self) -> Ownership {
        let record = self.ownership(self.execution).await.unwrap();
        assert!(self.guard.signal(
            (record.supervisor_pid, record.supervisor_start_ticks),
            rustix::process::Signal::KILL
        ));
        while ticks(record.supervisor_pid) == Some(record.supervisor_start_ticks)
            && alive(record.supervisor_pid)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        record
    }

    async fn wait_exit(&mut self, outcome: &str) -> Vec<Body> {
        let id = self.execution;
        let found = timeout(WAIT, async {
            loop {
                let cj = self.cj().await;
                if exit_of(&cj, id).is_some_and(|(_, found)| found == outcome) {
                    return cj;
                }
                self.pump(Duration::from_millis(50)).await;
            }
        })
        .await;
        found.unwrap_or_else(|_| {
            panic!(
                "timed out waiting for exit {outcome}; supervisor logs: {}",
                self.logs()
            )
        })
    }

    async fn assert_released(&self, outcome: &str) {
        let record = self.ownership(self.execution).await.unwrap();
        assert_eq!(record.phase, OwnershipPhase::Released as i32);
        assert_eq!(record.released_reason, outcome);
        // Итог и освобождение пишутся одной записью, каталог удаляется уже после неё: итог в CJ
        // виден раньше, чем каталог исчезнет.
        let dir = self.exec_dir();
        timeout(WAIT, async {
            while dir.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("execution directory is removed");
    }
}

#[tokio::test]
async fn cli_carries_the_group_marker_and_the_supervisor_records_the_members() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let cli = cli_children(record.supervisor_pid)[0];
    let environ = fs::read(format!("/proc/{cli}/environ")).unwrap();
    let expected = format!("{GROUP_MARKER_ENV}={}", env.execution).into_bytes();
    assert!(
        environ
            .split(|byte| *byte == 0)
            .any(|entry| entry == expected)
    );
    // Сокет аренды и бинарник воркера идут рядом с меткой: адаптеры чистят окружение CLI.
    for expected in [
        format!(
            "CANTERLOT_LEASE_SOCKET={}",
            env.runtime.path().join("canterlot/lease.sock").display()
        ),
        format!(
            "CANTERLOT_WORKER_BIN={}",
            env.config.supervisor_exe.as_ref().unwrap().display()
        ),
    ] {
        assert!(
            environ
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected.as_bytes()),
            "{expected} missing"
        );
    }
    // Обёртка `scripts/lease.sh` запустит этот путь: он должен быть исполняемым файлом.
    let bin = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"CANTERLOT_WORKER_BIN="))
        .map(|value| std::path::PathBuf::from(String::from_utf8(value.to_vec()).unwrap()))
        .unwrap();
    let meta = fs::metadata(&bin).unwrap();
    assert!(
        meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        "{} is not executable",
        bin.display()
    );
    let path = env.exec_dir().join(GROUP_FILE);
    timeout(WAIT, async {
        loop {
            let pairs: Vec<(u32, u64)> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            if pairs.contains(&(cli, ticks(cli).unwrap())) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    env.stop().await;
}

#[tokio::test]
async fn killed_supervisor_with_an_empty_group_fails_and_frees_the_session() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    env.kill_supervisor().await;
    let cj = env.wait_exit("supervisor_died").await;
    assert_eq!(owner_lost(&cj, env.execution), 1, "owner_lost comes first");
    env.assert_released("supervisor_died").await;
    assert_eq!(
        exit_of(&cj, env.execution).unwrap(),
        ("failed".to_owned(), "supervisor_died".to_owned())
    );
    let first = env.execution;
    env.execution = Uuid::now_v7();
    env.start(2).await;
    env.wait("a new execution runs", |b| {
        b.iter().filter(|x| state(x, "running")).count() == 2
    })
    .await;
    assert_eq!(exit_of(&env.cj().await, env.execution), None);
    assert!(exit_of(&env.cj().await, first).is_some());
    env.stop().await;
}

#[tokio::test]
async fn orphan_descendant_keeps_owner_lost_until_the_group_empties() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let detached = env.detach_descendant().await;
    env.kill_supervisor().await;
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(exit_of(&env.cj().await, id), None, "descendant is alive");
    assert!(alive(detached), "nothing is killed without the owner");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(detached).unwrap()).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    env.wait_exit("supervisor_died").await;
    env.assert_released("supervisor_died").await;
}

#[tokio::test]
async fn worker_restart_exits_a_supervisor_that_died_while_it_was_down() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    env.close_manager().await;
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    while alive(record.supervisor_pid) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    env.reopen();
    env.hub_session();
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    let cj = env.wait_exit("supervisor_died").await;
    assert_eq!(owner_lost(&cj, env.execution), 1);
    env.assert_released("supervisor_died").await;
}

#[tokio::test]
async fn live_supervisor_is_never_failed_and_keeps_the_reclaim_path() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    env.hub_session();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let id = env.execution;
    assert_eq!(exit_of(&env.cj().await, id), None, "supervisor is alive");
    assert!(alive(record.supervisor_pid));
    assert!(alive(cli_children(record.supervisor_pid)[0]));
    assert_eq!(env.restart_with_spec(&spec).await, vec![id]);
    env.hub_session();
    assert_eq!(owner_reclaimed(&env.cj().await, id), 1);
    let input = Uuid::now_v7();
    env.send(deliver_command(
        input,
        1,
        id,
        1,
        env.conversation,
        &script(r#"{"delta":"alive"}"#),
    ))
    .await;
    env.wait("the reclaimed session answers", |b| finished(b, input))
        .await;
    assert_eq!(exit_of(&env.cj().await, id), None);
    env.stop().await;
}

#[tokio::test]
async fn resolve_reclaims_a_live_supervisor_and_kills_nothing() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let cli = cli_children(record.supervisor_pid);
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    env.hub_session();
    fs::write(env.exec_dir().join("spec.toml"), &spec).unwrap();
    let id = env.execution;
    let resolve = resolve_command(id, 1);
    env.send(resolve.clone()).await;
    env.wait_cj("reclaimed", |cj| owner_reclaimed(cj, id) == 1)
        .await;
    assert_eq!(exit_of(&env.cj().await, id), None);
    assert!(alive(record.supervisor_pid));
    assert_eq!(cli_children(record.supervisor_pid), cli, "CLI is untouched");
    let input = Uuid::now_v7();
    env.send(deliver_command(
        input,
        1,
        id,
        1,
        env.conversation,
        &script(r#"{"delta":"alive"}"#),
    ))
    .await;
    env.wait("the reclaimed session answers", |b| finished(b, input))
        .await;
    assert!(env.terminal(&resolve).await);
    env.stop().await;
}

#[tokio::test]
async fn resolve_of_a_linked_supervisor_proves_it_instead_of_stopping_it() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    let id = env.execution;
    env.send(resolve_command(id, 1)).await;
    env.wait_cj("reclaimed", |cj| owner_reclaimed(cj, id) == 1)
        .await;
    assert_eq!(exit_of(&env.cj().await, id), None);
    assert!(alive(record.supervisor_pid));
    env.stop().await;
}

#[tokio::test]
async fn late_input_for_a_released_execution_is_answered_not_kept() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    env.kill_supervisor().await;
    env.wait_exit("supervisor_died").await;
    let late = deliver_command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        env.conversation,
        "late",
    );
    env.send(late.clone()).await;
    assert!(env.terminal(&late).await);
    assert!(
        env.cj()
            .await
            .iter()
            .any(|b| matches!(b, Body::DispatchDeferred(f) if f.command_id == late.command_id))
    );
}

#[tokio::test]
async fn resolve_terminates_the_orphan_group_and_confirms_it_empty() {
    let mut steps = trace::Steps::new();
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    steps.step("running");
    let detached = env.detach_descendant().await;
    steps.step(&format!(
        "detached descendant {detached}: {}",
        proc_state(detached)
    ));
    let killed = env.kill_supervisor().await;
    steps.step(&format!(
        "supervisor {} killed: {}",
        killed.supervisor_pid,
        proc_state(killed.supervisor_pid)
    ));
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    steps.step("owner_lost in CJ");
    let resolve = resolve_command(id, 1);
    env.send(resolve.clone()).await;
    steps.step("resolve handled");
    env.wait_exit("supervisor_died").await;
    steps.step(&format!(
        "exit supervisor_died; descendant {detached}: {}",
        proc_state(detached)
    ));
    assert!(!alive(detached), "the rest of the group is terminated");
    env.assert_released("supervisor_died").await;
    assert!(env.terminal(&resolve).await);
}

/// Решение владельца сразу за `owner_lost`: актор уже ушёл, наблюдатель ещё не встал.
/// Решение не теряется и доходит до наблюдателя.
#[tokio::test]
async fn resolve_right_after_owner_lost_reaches_the_watcher() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let detached = env.detach_descendant().await;
    let id = env.execution;
    env.kill_supervisor().await;
    timeout(WAIT, async {
        while owner_lost(&env.cj().await, id) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owner_lost in CJ");
    let resolve = resolve_command(id, 1);
    env.send(resolve.clone()).await;
    env.wait_exit("supervisor_died").await;
    assert!(!alive(detached), "the rest of the group is terminated");
    assert!(env.terminal(&resolve).await);
}

#[tokio::test]
async fn group_of_an_old_supervisor_waits_for_the_owner_and_ends_admin_resolved() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    fs::remove_file(env.exec_dir().join(GROUP_FILE)).unwrap();
    env.kill_supervisor().await;
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(
        exit_of(&env.cj().await, id),
        None,
        "emptiness is not proven"
    );
    env.send(resolve_command(id, 1)).await;
    let cj = env.wait_exit("admin_resolved").await;
    assert_eq!(
        exit_of(&cj, id).unwrap(),
        ("failed".to_owned(), "admin_resolved".to_owned())
    );
    env.assert_released("admin_resolved").await;
}

#[tokio::test]
async fn resolve_for_another_generation_changes_nothing() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let detached = env.detach_descendant().await;
    env.kill_supervisor().await;
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    let stale = resolve_command(id, 7);
    env.send(stale.clone()).await;
    timeout(WAIT, async {
        while !env.terminal(&stale).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(alive(detached));
    assert_eq!(exit_of(&env.cj().await, id), None);
}

#[tokio::test]
async fn reboot_stops_the_execution_with_the_real_reason() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let mut record = env.ownership(env.execution).await.unwrap();
    env.close_manager().await;
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    while alive(record.supervisor_pid) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    env.reopen();
    record.boot_id = Uuid::now_v7().to_string();
    env.thread()
        .call(move |store| {
            store.as_mut().unwrap().ownership_mut()?.append(record)?;
            Ok(())
        })
        .await
        .unwrap();
    env.hub_session();
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    let cj = env.wait_exit("machine_rebooted").await;
    assert_eq!(
        exit_of(&cj, env.execution).unwrap(),
        ("stopped".to_owned(), "machine_rebooted".to_owned())
    );
    assert_eq!(owner_lost(&cj, env.execution), 0);
    env.assert_released("machine_rebooted").await;
}

#[tokio::test]
async fn reboot_also_exits_an_execution_already_reported_lost() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let spec = env.lose_owner_to_a_bad_spec().await;
    env.ack_cj().await;
    let mut record = env.ownership(env.execution).await.unwrap();
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    while alive(record.supervisor_pid) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    record.boot_id = Uuid::now_v7().to_string();
    env.thread()
        .call(move |store| {
            store.as_mut().unwrap().ownership_mut()?.append(record)?;
            Ok(())
        })
        .await
        .unwrap();
    env.restart_with_spec(&spec).await;
    env.hub_session();
    env.wait_exit("machine_rebooted").await;
    env.assert_released("machine_rebooted").await;
}

#[tokio::test]
async fn dead_journal_reaches_the_hub_before_the_exit_fact() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let id = env.execution;
    let record = env.ownership(id).await.unwrap();
    env.close_manager().await;
    // Канала нет, но команду сопровождение успевает получить только при живой службе,
    // поэтому ввод отдаётся заранее, а его конец ложится в журнал уже без отправки.
    env.reopen();
    env.hub_session();
    assert_eq!(env.manager().startup_verify().await.unwrap(), vec![id]);
    env.auto_ack = false;
    let input = Uuid::now_v7();
    env.send(deliver_command(
        input,
        1,
        id,
        1,
        env.conversation,
        &script(r#"{"delay_ms":300},{"delta":"tail"}"#),
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    env.close_manager().await;
    let journal = env.exec_dir().join("sj");
    let written = timeout(WAIT, async {
        loop {
            let reader = canterlot_journal::Journal::open_read_only(&journal).unwrap();
            let bodies = reader.read_from(reader.first_seq(), usize::MAX).unwrap();
            if bodies.iter().any(|record| {
                matches!(
                    canterlot_proto::worker::v1::RecordBody::decode(record.payload.as_slice())
                        .unwrap()
                        .body,
                    Some(Body::RunFinished(ref f)) if f.input_command_id == input.to_string()
                )
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    written.unwrap();
    assert!(env.guard.signal(
        (record.supervisor_pid, record.supervisor_start_ticks),
        rustix::process::Signal::KILL
    ));
    while alive(record.supervisor_pid) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    env.reopen();
    env.committed.clear();
    env.seen.clear();
    env.bodies.clear();
    env.hub_session();
    env.manager().startup_verify().await.unwrap();
    env.wait("the tail of the dead journal", |b| finished(b, input))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(exit_of(&env.cj().await, id), None, "the hub has not acked");
    let (journal_id, up_to) = env
        .committed
        .iter()
        .map(|(journal, seq)| (*journal, *seq))
        .next()
        .unwrap();
    env.manager().on_sj_ack(journal_id, up_to);
    env.auto_ack = true;
    env.wait_exit("supervisor_died").await;
}

#[tokio::test]
async fn deliver_input_for_closed_generation_is_deferred() {
    let mut env = Env::new().await;
    env.hub_session();
    let id = Uuid::now_v7();
    env.send(deliver_command(
        id,
        1,
        Uuid::now_v7(),
        1,
        env.conversation,
        "late",
    ))
    .await;
    env.start(2).await;
    env.wait_running().await;
    env.send(deliver_command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        env.conversation,
        "old generation",
    ))
    .await;
    let cj = env
        .wait_cj("two deferrals", |cj| {
            cj.iter()
                .filter(
                    |b| matches!(b, Body::DispatchDeferred(f) if f.reason == "generation_closed"),
                )
                .count()
                == 2
        })
        .await;
    assert!(
        cj.iter()
            .any(|b| matches!(b, Body::DispatchDeferred(f) if f.command_id == id.to_string()))
    );
    assert!(
        !env.bodies
            .iter()
            .any(|b| matches!(b, Body::TransmissionStarted(_)))
    );
    env.stop().await;
}

#[tokio::test]
async fn redelivered_attempt_is_handed_over_again() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let first = Uuid::now_v7();
    env.send(deliver_command(
        first,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"delta":"a"},{"delay_ms":800}"#),
    ))
    .await;
    let second = Uuid::now_v7();
    for attempt in [1, 2] {
        env.send(deliver_command(
            second,
            attempt,
            env.execution,
            1,
            env.conversation,
            "second",
        ))
        .await;
    }
    env.wait("second turn and the deferral of its higher attempt", |b| {
        finished(b, second)
            && b.iter().any(|x| matches!(x, Body::DispatchDeferred(f) if f.command_id == second.to_string() && f.dispatch_attempt == 2))
    })
    .await;
    let transmitted = env
        .bodies
        .iter()
        .filter(|b| matches!(b, Body::TransmissionStarted(f) if f.command_id == second.to_string()))
        .count();
    assert_eq!(transmitted, 1, "a command is never transmitted twice");
    assert!(
        env.bodies.iter().any(|b| matches!(b, Body::DispatchDeferred(f) if f.command_id == second.to_string() && f.dispatch_attempt == 2)),
        "the higher attempt reached the supervisor"
    );
    env.stop().await;
}

#[tokio::test]
async fn cancel_and_withdraw_before_handoff_are_answered_by_control_service() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let (cancelled, withdrawn) = (Uuid::now_v7(), Uuid::now_v7());
    env.send(command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        Kind::CancelInput(CancelInput {
            target_command_id: cancelled.to_string(),
            target_dispatch_attempt: 1,
        }),
    ))
    .await;
    env.send(command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        Kind::WithdrawInput(WithdrawInput {
            target_command_id: withdrawn.to_string(),
            target_dispatch_attempt: 1,
        }),
    ))
    .await;
    let cj = env
        .wait_cj("answers in CJ", |cj| {
            cj.iter().any(|b| matches!(b, Body::CancelWon(f) if f.command_id == cancelled.to_string()))
                && cj.iter().any(|b| matches!(b, Body::InputWithdrawn(f) if f.command_id == withdrawn.to_string()))
        })
        .await;
    assert!(
        !cj.iter()
            .any(|b| matches!(b, Body::CancelWon(f) if f.command_id == withdrawn.to_string()))
    );
    env.thread()
        .call(|store| {
            let store = store.as_mut().unwrap();
            let seq = store.cj_cursor()?.last_seq;
            store.on_cj_ack(seq)?;
            Ok(())
        })
        .await
        .unwrap();
    env.close_manager().await;
    env.reopen();
    assert_eq!(
        env.manager().startup_verify().await.unwrap(),
        vec![env.execution]
    );
    env.hub_session();
    for target in [cancelled, withdrawn] {
        env.send(deliver_command(
            target,
            1,
            env.execution,
            1,
            env.conversation,
            "never handed over",
        ))
        .await;
    }
    let probe = Uuid::now_v7();
    env.send(deliver_command(
        probe,
        1,
        env.execution,
        1,
        env.conversation,
        "probe",
    ))
    .await;
    env.wait("probe turn", |b| finished(b, probe)).await;
    for target in [cancelled, withdrawn] {
        assert!(
            !env.bodies.iter().any(
                |b| matches!(b, Body::TransmissionStarted(f) if f.command_id == target.to_string())
            ),
            "the supervisor never saw the cancelled or withdrawn input"
        );
    }
    assert_eq!(env.stored_commands().await, 0, "everything settled");
    env.stop().await;
}

#[tokio::test]
async fn interrupt_for_old_generation_is_rejected_stale() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(2).await;
    env.wait_running().await;
    let stale = Uuid::now_v7();
    env.send(command(
        stale,
        1,
        env.execution,
        1,
        Kind::Interrupt(Interrupt {}),
    ))
    .await;
    let gone = Uuid::now_v7();
    env.send(command(
        gone,
        1,
        Uuid::now_v7(),
        2,
        Kind::Interrupt(Interrupt {}),
    ))
    .await;
    let live = Uuid::now_v7();
    env.send(command(
        live,
        1,
        env.execution,
        2,
        Kind::Interrupt(Interrupt {}),
    ))
    .await;
    let cj = env
        .wait_cj("two interrupt facts", |cj| {
            cj.iter()
                .filter(|b| matches!(b, Body::InterruptStatus(_)))
                .count()
                == 2
        })
        .await;
    let state_of = |id: Uuid| {
        cj.iter().find_map(|b| match b {
            Body::InterruptStatus(f) if f.command_id == id.to_string() => Some(f.state),
            _ => None,
        })
    };
    assert_eq!(state_of(stale), Some(InterruptState::RejectedStale as i32));
    assert_eq!(state_of(gone), Some(InterruptState::Superseded as i32));
    env.wait("forwarded to the supervisor", |b| {
        b.iter().any(|x| {
            matches!(x, Body::InterruptStatus(f) if f.command_id == live.to_string() && f.state == InterruptState::NotApplicable as i32)
        })
    })
    .await;
    env.stop().await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn stop_before_spawn_cancels_start() {
    let mut env = Env::new().await;
    env.manager().set_spawn_delay(Duration::from_millis(500));
    let start = start_command(env.execution, 1, env.conversation, "synthetic", &env.work);
    env.send(start.clone()).await;
    let stop = command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        Kind::StopExecution(StopExecution { force: false }),
    );
    env.send(stop.clone()).await;
    let id = env.execution;
    let cj = env
        .wait_cj("start_cancelled", |cj| {
            cj.iter().any(|b| {
                matches!(b, Body::ExecutionState(f) if f.execution_id == id.to_string() && f.state == "stopped" && f.outcome == "start_cancelled")
            })
        })
        .await;
    assert_eq!(
        cj.iter()
            .filter(|b| matches!(b, Body::ExecutionState(_)))
            .count(),
        1
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !env.root
            .path()
            .join("store/executions")
            .join(id.to_string())
            .exists()
    );
    assert!(env.ownership(id).await.is_none());
    assert!(supervisors_of(env.root.path()).is_empty());
    assert!(env.terminal(&start).await && env.terminal(&stop).await);
    assert_eq!(env.manager().observations()[0].outcome, "start_cancelled");
    env.close_manager().await;
    env.reopen();
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
    assert_eq!(env.manager().observations()[0].outcome, "start_cancelled");
    let up_to = env
        .thread()
        .call(|store| {
            let store = store.as_mut().unwrap();
            let up_to = store.cj_cursor()?.last_seq;
            store.on_cj_ack(up_to)?;
            Ok(up_to)
        })
        .await
        .unwrap();
    env.manager().on_cj_ack(up_to);
    assert!(env.manager().observations().is_empty());
}

#[tokio::test]
async fn terminal_facts_release_inbox_prefix() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let ids = [Uuid::now_v7(), Uuid::now_v7()];
    let mut commands = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let command = deliver_command(
            *id,
            1,
            env.execution,
            1,
            env.conversation,
            &format!("turn {index}"),
        );
        commands.push(command.clone());
        env.send(command).await;
    }
    env.wait("both accepted", |b| {
        ids.iter().all(|id| {
            b.iter()
                .any(|x| matches!(x, Body::InputAccepted(f) if f.command_id == id.to_string()))
        })
    })
    .await;
    for command in &commands {
        assert!(
            env.terminal(command).await,
            "an accepted input is a terminal fact"
        );
    }
    assert_eq!(
        env.stored_commands().await,
        0,
        "the inbox prefix is released"
    );
    env.stop().await;
}

#[tokio::test]
async fn second_instance_is_refused_by_the_store_lock() {
    let env = Env::new().await;
    let error = match WorkerStore::open(&env.root.path().join("store")) {
        Ok(_) => panic!("a second store on the same data_dir must be refused"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        StoreError::Journal(canterlot_journal::JournalError::Locked)
    ));
    let text = error.to_string();
    assert!(
        !text.contains(&env.root.path().to_string_lossy().into_owned()),
        "{text}"
    );
}

#[tokio::test]
async fn wave2_n1_closed_replies_are_durable_cj_receipts() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    env.auto_ack = false;
    env.stop().await;
    for _ in 0..50 {
        for cancel in [true, false] {
            let target = Uuid::now_v7();
            let delivery = deliver_command(target, 1, env.execution, 1, env.conversation, "late");
            env.send(delivery.clone()).await;
            env.wait_cj("closed delivery", |b| b.iter().any(|b| matches!(b, Body::DispatchDeferred(f) if f.command_id == target.to_string() && f.reason == "generation_closed"))).await;
            let kind = if cancel {
                Kind::CancelInput(CancelInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                })
            } else {
                Kind::WithdrawInput(WithdrawInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                })
            };
            let control = command(Uuid::now_v7(), 1, env.execution, 1, kind);
            env.send(control.clone()).await;
            env.wait_cj("closed control", |b| {
                b.iter().any(|b| match b {
                    Body::CancelWon(f) if cancel => f.command_id == target.to_string(),
                    Body::InputWithdrawn(f) if !cancel => f.command_id == target.to_string(),
                    _ => false,
                })
            })
            .await;
            assert!(env.terminal(&delivery).await && env.terminal(&control).await);
        }
    }
    let cursor = env.manager().journal_cursors().pop().unwrap();
    assert_eq!(
        env.committed[&Uuid::parse_str(&cursor.journal_id).unwrap()],
        cursor.last_seq
    );
    env.manager().on_sj_ack(
        Uuid::parse_str(&cursor.journal_id).unwrap(),
        cursor.last_seq,
    );
    timeout(WAIT, async {
        while !env.manager().observations().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(env.guard.members().is_empty());
}

#[tokio::test]
async fn terminal_sj_observation_and_cursor_remain_until_ack() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    env.auto_ack = false;
    env.stop().await;
    assert_eq!(env.manager().observations()[0].state, "stopped");
    let cursor = env.manager().journal_cursors().pop().unwrap();
    assert!(cursor.last_seq > cursor.acked_seq_known_to_worker);
    let dir = env
        .root
        .path()
        .join("store/executions")
        .join(env.execution.to_string());
    assert!(dir.exists());
    env.manager().on_sj_ack(
        Uuid::parse_str(&cursor.journal_id).unwrap(),
        cursor.last_seq,
    );
    timeout(WAIT, async {
        while !env.manager().observations().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(env.manager().journal_cursors().is_empty());
    assert!(!dir.exists());
}

#[tokio::test]
async fn timeout_cleans_supervisor_and_cli() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async {
        let mut env = Env::new().await;
        env.hub_session();
        env.start(1).await;
        env.wait_running().await;
        let record = env.ownership(env.execution).await.unwrap();
        assert_eq!(cli_children(record.supervisor_pid).len(), 1);
        sender.send(env.root.path().to_path_buf()).unwrap();
        timeout(Duration::from_millis(100), async move {
            let _env = env;
            std::future::pending::<()>().await;
        })
        .await
    });
    let root = receiver.await.unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(supervisors_of(&root).is_empty());
    assert!(!processes().iter().any(|(pid, _, _)| {
        fs::read_link(format!("/proc/{pid}/exe"))
            .is_ok_and(|exe| exe == Path::new(env!("CARGO_BIN_EXE_canterlot-worker")))
            && fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(&root))
    }));
}

#[tokio::test]
async fn reattach_delivers_stored_input_missing_from_received_table() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let before = env.ownership(env.execution).await.unwrap();
    let input = Uuid::now_v7();
    let command = deliver_command(
        input,
        1,
        env.execution,
        1,
        env.conversation,
        "stored before handoff",
    );
    let copy = command.clone();
    env.thread()
        .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
        .await
        .unwrap();
    env.close_manager().await;
    env.reopen();
    assert_eq!(
        env.manager().startup_verify().await.unwrap(),
        vec![env.execution]
    );
    env.hub_session();
    env.wait("recovered input completes", |b| finished(b, input))
        .await;
    assert!(env.terminal(&command).await);
    assert_eq!(
        env.bodies
            .iter()
            .filter(
                |b| matches!(b, Body::TransmissionStarted(f) if f.command_id == input.to_string())
            )
            .count(),
        1
    );
    let after = env.ownership(env.execution).await.unwrap();
    assert_eq!(
        (before.supervisor_pid, before.supervisor_start_ticks),
        (after.supervisor_pid, after.supervisor_start_ticks)
    );
    assert_eq!(cli_children(before.supervisor_pid).len(), 1);
    env.stop().await;
}

#[tokio::test]
async fn failed_spawn_observation_remains_until_cj_ack() {
    let mut env = Env::new().await;
    env.close_manager().await;
    env.config.supervisor_exe = Some(env.root.path().join("missing-supervisor"));
    env.reopen();
    env.start(1).await;
    env.wait_cj("spawn rejection", |cj| {
        rejected_reasons(cj).contains(&"supervisor_start_failed".into())
    })
    .await;
    timeout(WAIT, async {
        while env.manager().observations()[0].state != "failed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(env.manager().observations()[0].outcome, "start_rejected");
    let up_to = env
        .thread()
        .call(|store| {
            let store = store.as_mut().unwrap();
            let seq = store.cj_cursor()?.last_seq;
            store.on_cj_ack(seq)?;
            Ok(seq)
        })
        .await
        .unwrap();
    env.manager().on_cj_ack(up_to);
    assert!(env.manager().observations().is_empty());
    assert!(supervisors_of(env.root.path()).is_empty());
}

#[tokio::test]
async fn wave_w_owner_lost_retires_only_after_cj_ack_across_restart() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    env.detach_descendant().await;
    env.kill_supervisor().await;
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    assert_eq!(env.manager().observations()[0].state, "owner_lost");
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    let seq = env
        .thread()
        .call(|store| {
            let store = store.as_mut().unwrap();
            let seq = store.cj_cursor()?.last_seq;
            store.on_cj_ack(seq)?;
            Ok(seq)
        })
        .await
        .unwrap();
    env.manager().on_cj_ack(seq);
    assert!(env.manager().observations().is_empty());
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    assert!(env.manager().observations().is_empty());
    assert!(env.ownership(id).await.is_some());
}

#[tokio::test]
async fn wave_w_cancelled_start_survives_cj_compaction_and_restart() {
    let mut env = Env::new().await;
    let stop = command(
        Uuid::now_v7(),
        1,
        env.execution,
        1,
        Kind::StopExecution(StopExecution { force: false }),
    );
    env.send(stop).await;
    env.thread()
        .call(|store| {
            let store = store.as_mut().unwrap();
            store.on_cj_ack(store.cj_cursor()?.last_seq)?;
            Ok(())
        })
        .await
        .unwrap();
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    let start = start_command(env.execution, 1, env.conversation, "synthetic", &env.work);
    env.send(start.clone()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(env.terminal(&start).await);
    assert!(env.guard.members().is_empty());
    assert!(env.ownership(env.execution).await.is_none());
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn wave_w_failed_control_writes_are_retried_after_reattach() {
    for control in ["cancel", "withdraw", "interrupt", "stop"] {
        let mut env = Env::new().await;
        env.hub_session();
        env.start(1).await;
        env.wait_running().await;
        let target = Uuid::now_v7();
        let input = deliver_command(
            target,
            1,
            env.execution,
            1,
            env.conversation,
            &script(r#"{"delta":"x"},{"hang":true}"#),
        );
        env.send(input).await;
        env.wait("intent", |b| {
            b.iter().any(
                |b| matches!(b, Body::TransmissionStarted(f) if f.command_id == target.to_string()),
            )
        })
        .await;
        let kind = match control {
            "cancel" => Kind::CancelInput(CancelInput {
                target_command_id: target.to_string(),
                target_dispatch_attempt: 1,
            }),
            "withdraw" => Kind::WithdrawInput(WithdrawInput {
                target_command_id: target.to_string(),
                target_dispatch_attempt: 1,
            }),
            "interrupt" => Kind::Interrupt(Interrupt {}),
            _ => Kind::StopExecution(StopExecution { force: true }),
        };
        let cmd = command(Uuid::now_v7(), 1, env.execution, 1, kind);
        env.manager().fail_next_ipc_send();
        env.send(cmd.clone()).await;
        env.wait("retried control", |b| {
            b.iter().any(|b| match (control, b) {
                ("cancel", Body::CancelLost(f)) => f.command_id == target.to_string(),
                ("withdraw", Body::WithdrawRefused(f)) => f.command_id == target.to_string(),
                ("interrupt", Body::InterruptStatus(f)) => f.command_id == cmd.command_id,
                ("stop", Body::JournalClosed(_)) => true,
                _ => false,
            })
        })
        .await;
        assert!(env.terminal(&cmd).await);
        if control != "stop" {
            env.stop().await;
        }
    }
}

#[tokio::test]
async fn wave_w_acceptance_unconfirmed_and_cancel_lost_release_inbox() {
    let mut env = Env::new().await;
    env.close_manager().await;
    env.config.supervisor.transmit_deadline = Duration::from_millis(150);
    env.reopen();
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let id = Uuid::now_v7();
    let input = deliver_command(
        id,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"hang":true}"#),
    );
    env.send(input.clone()).await;
    env.wait("unconfirmed", |b| {
        b.iter()
            .any(|b| matches!(b, Body::AcceptanceUnconfirmed(f) if f.command_id == id.to_string()))
    })
    .await;
    assert!(env.terminal(&input).await);
    assert_eq!(env.stored_commands().await, 0);
    env.stop().await;
}

#[tokio::test]
async fn late_delivery_proof_records_input_accepted_after_unconfirmed() {
    let mut env = Env::new().await;
    env.close_manager().await;
    env.config.supervisor.transmit_deadline = Duration::from_millis(150);
    env.reopen();
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let id = Uuid::now_v7();
    let input = deliver_command(
        id,
        1,
        env.execution,
        1,
        env.conversation,
        &script(r#"{"delay_ms":700},{"delta":"late"}"#),
    );
    env.send(input.clone()).await;
    env.wait("late acceptance", |b| {
        let unconfirmed = b.iter().position(
            |b| matches!(b, Body::AcceptanceUnconfirmed(f) if f.command_id == id.to_string()),
        );
        let accepted = b
            .iter()
            .position(|b| matches!(b, Body::InputAccepted(f) if f.command_id == id.to_string()));
        matches!((unconfirmed, accepted), (Some(u), Some(a)) if u < a)
    })
    .await;
    env.wait("turn finished", |b| {
        b.iter()
            .any(|b| matches!(b, Body::RunFinished(f) if f.input_command_id == id.to_string()))
    })
    .await;
    env.stop().await;
}

#[tokio::test]
async fn wave_w_dead_ownership_keeps_deliver_cancel_withdraw_pending_until_the_exit() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let detached = env.detach_descendant().await;
    env.kill_supervisor().await;
    let id = env.execution;
    env.wait_cj("owner lost", |cj| owner_lost(cj, id) == 1)
        .await;
    let target = Uuid::now_v7();
    let mut commands = vec![deliver_command(
        target,
        1,
        id,
        1,
        env.conversation,
        "pending",
    )];
    commands.push(command(
        Uuid::now_v7(),
        1,
        id,
        1,
        Kind::CancelInput(CancelInput {
            target_command_id: target.to_string(),
            target_dispatch_attempt: 1,
        }),
    ));
    commands.push(command(
        Uuid::now_v7(),
        1,
        id,
        1,
        Kind::WithdrawInput(WithdrawInput {
            target_command_id: target.to_string(),
            target_dispatch_attempt: 1,
        }),
    ));
    for cmd in &commands {
        env.send(cmd.clone()).await;
    }
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    for cmd in &commands {
        assert!(!env.terminal(cmd).await);
    }
    assert_eq!(env.stored_commands().await, 3);
    assert!(!env.cj().await.iter().any(|b| matches!(
        b,
        Body::DispatchDeferred(_) | Body::CancelWon(_) | Body::InputWithdrawn(_)
    )));
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(detached).unwrap()).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    env.hub_session();
    let cj = env.wait_exit("supervisor_died").await;
    assert!(
        cj.iter()
            .any(|b| matches!(b, Body::CancelWon(f) if f.command_id == target.to_string()))
    );
    assert_eq!(env.stored_commands().await, 0, "nothing stays in the inbox");
}

#[tokio::test]
async fn worker_restart_with_acked_open_request_reannounces_and_delivers_decision() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(9).await;
    env.wait_running().await;
    env.send(deliver_command(Uuid::now_v7(), 1, env.execution, 9, env.conversation,
        &script(r#"{"request_permission":{"tool_name":"Bash","command":"cargo test","args":{}}},{"delta":"resumed"}"#))).await;
    env.wait("request opened", |b| {
        b.iter().any(|b| matches!(b, Body::RequestOpened(_)))
    })
    .await;
    let request = env
        .bodies
        .iter()
        .find_map(|b| match b {
            Body::RequestOpened(r) => Some(r.request_id.clone()),
            _ => None,
        })
        .unwrap();
    let supervisor = env.ownership(env.execution).await.unwrap().supervisor_pid;
    env.close_manager().await;
    env.reopen();
    env.bodies.clear();
    assert_eq!(
        env.manager().startup_verify().await.unwrap(),
        vec![env.execution]
    );
    env.hub_session();
    env.wait("reannounced acknowledged request", |b| {
        b.iter()
            .any(|b| matches!(b, Body::RequestOpened(r) if r.request_id == request))
    })
    .await;
    assert_eq!(
        env.ownership(env.execution).await.unwrap().supervisor_pid,
        supervisor
    );
    let decision = command(
        Uuid::now_v7(),
        1,
        env.execution,
        9,
        Kind::DeliverDecision(canterlot_proto::worker::v1::DeliverDecision {
            request_id: request,
            decision_id: Uuid::now_v7().to_string(),
            generation: 9,
            kind: "permission".into(),
            decision_payload_json: br#"{"kind":"permission","behavior":"allow","message":null}"#
                .to_vec(),
        }),
    );
    env.send(decision.clone()).await;
    env.wait("decision applied", |b| {
        b.iter().any(|b| matches!(b, Body::DecisionApplied(_)))
    })
    .await;
    timeout(WAIT, async {
        while !env.terminal(&decision).await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    env.stop().await;
    env.close_manager().await;
}

#[tokio::test]
async fn decision_envelope_generation_is_checked_before_ipc() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(9).await;
    env.wait_running().await;
    let decision = command(
        Uuid::now_v7(),
        1,
        env.execution,
        8,
        Kind::DeliverDecision(canterlot_proto::worker::v1::DeliverDecision {
            request_id: Uuid::now_v7().to_string(),
            decision_id: Uuid::now_v7().to_string(),
            generation: 9,
            kind: "permission".into(),
            decision_payload_json: br#"{"kind":"permission","behavior":"allow"}"#.to_vec(),
        }),
    );
    env.send(decision.clone()).await;
    env.wait_cj("stale envelope decision", |b| {
        b.iter()
            .any(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "stale_generation"))
    })
    .await;
    assert!(env.terminal(&decision).await);
    env.stop().await;
    env.close_manager().await;
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

impl Env {
    fn git_source(&self) -> PathBuf {
        let source = fs::canonicalize(&self.work).unwrap().join("repo");
        fs::create_dir_all(&source).unwrap();
        git_in(&source, &["init", "-q", "-b", "main"]);
        fs::write(source.join("a.txt"), "alpha\n").unwrap();
        git_in(&source, &["add", "-A"]);
        git_in(&source, &["commit", "-q", "-m", "first"]);
        source
    }

    fn worktree_dir(&self) -> PathBuf {
        self.root
            .path()
            .join("store/worktrees")
            .join(self.conversation.to_string())
    }

    fn worktree_start(&self, generation: u64, source: &Path, workdir: &Path) -> Command {
        let mut command = start_command(
            self.execution,
            generation,
            self.conversation,
            "synthetic",
            workdir,
        );
        if let Some(Kind::StartExecution(start)) = &mut command.kind {
            start.worktree = Some(canterlot_proto::worker::v1::WorktreeSpec {
                source_workdir: source.to_string_lossy().into_owned(),
                branch: "canterlot/0190abcd".into(),
                base: "HEAD".into(),
            });
        }
        command
    }

    fn remove_command(&self, path: &Path, force: bool) -> Command {
        Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            execution_id: String::new(),
            execution_generation: 0,
            kind: Some(Kind::RemoveWorktree(
                canterlot_proto::worker::v1::RemoveWorktree {
                    source_workdir: self.work.join("repo").to_string_lossy().into_owned(),
                    path: path.to_string_lossy().into_owned(),
                    branch: "canterlot/0190abcd".into(),
                    force,
                },
            )),
        }
    }
}

fn removed_outcomes(cj: &[Body]) -> Vec<(String, String, bool)> {
    cj.iter()
        .filter_map(|b| match b {
            Body::WorktreeRemoved(f) => Some((f.outcome.clone(), f.detail.clone(), f.branch_kept)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn start_with_worktree_runs_cli_there() {
    let mut env = Env::new().await;
    let source = env.git_source();
    let target = env.worktree_dir();
    env.hub_session();
    env.send(env.worktree_start(1, &source, &target)).await;
    env.wait_running().await;
    assert_eq!(fs::read_to_string(target.join("a.txt")).unwrap(), "alpha\n");
    assert_eq!(
        git_in(&target, &["symbolic-ref", "--short", "HEAD"]),
        "canterlot/0190abcd"
    );
    let record = env.ownership(env.execution).await.unwrap();
    let cwd = fs::read_link(format!("/proc/{}/cwd", record.cli_pid)).unwrap();
    assert_eq!(cwd, fs::canonicalize(&target).unwrap());
    assert!(
        fs::read_dir(&source)
            .unwrap()
            .flatten()
            .all(|e| e.file_name() == ".git" || e.file_name() == "a.txt"),
        "the source tree gets nothing"
    );
    env.stop().await;
}

#[tokio::test]
async fn resumed_start_reuses_the_existing_worktree() {
    let mut env = Env::new().await;
    let source = env.git_source();
    let target = env.worktree_dir();
    env.hub_session();
    env.send(env.worktree_start(1, &source, &target)).await;
    env.wait_running().await;
    fs::write(target.join("wip.txt"), "wip\n").unwrap();
    env.stop().await;
    env.execution = Uuid::now_v7();
    env.send(env.worktree_start(2, &source, &target)).await;
    env.wait_running().await;
    assert_eq!(fs::read_to_string(target.join("wip.txt")).unwrap(), "wip\n");
    env.stop().await;
}

#[tokio::test]
async fn start_with_a_bad_worktree_is_rejected_before_any_supervisor() {
    let env = Env::new().await;
    let source = env.git_source();
    let target = env.worktree_dir();
    let plain = env.work.join("plain");
    fs::create_dir(&plain).unwrap();
    let mut unknown_base = env.worktree_start(1, &source, &target);
    if let Some(Kind::StartExecution(start)) = &mut unknown_base.kind {
        start.worktree.as_mut().unwrap().base = "no-such-ref".into();
    }
    env.send(unknown_base).await;
    let other = |generation, source: &Path, workdir: &Path| {
        let mut command = env.worktree_start(generation, source, workdir);
        command.execution_id = Uuid::now_v7().to_string();
        command
    };
    env.send(other(
        2,
        &plain,
        &env.root.path().join("store/worktrees").join("n1"),
    ))
    .await;
    env.send(other(
        3,
        Path::new("/etc"),
        &env.root.path().join("store/worktrees").join("n2"),
    ))
    .await;
    env.send(other(4, &source, &env.work.join("elsewhere")))
        .await;
    env.send(other(
        5,
        &source,
        &env.root.path().join("store/worktrees/../n3"),
    ))
    .await;
    let cj = env
        .wait_cj("five rejections", |cj| rejected_reasons(cj).len() >= 5)
        .await;
    let mut reasons = rejected_reasons(&cj);
    reasons.sort();
    assert_eq!(
        reasons,
        [
            "workdir_not_allowed",
            "workdir_not_allowed",
            "workdir_not_allowed",
            "worktree_base_unknown",
            "worktree_not_git",
        ]
    );
    assert!(!target.exists(), "a failed preparation leaves no copy");
    assert!(
        git_in(&source, &["branch", "--list", "canterlot/*"]).is_empty(),
        "no branch is left behind"
    );
    assert_eq!(
        fs::read_dir(env.root.path().join("store/executions"))
            .map(|d| d.count())
            .unwrap_or(0),
        0,
        "a rejected start never creates an execution directory"
    );
}

#[tokio::test]
async fn remove_worktree_answers_through_cj_and_survives_a_restart() {
    let mut env = Env::new().await;
    let source = env.git_source();
    let target = env.worktree_dir();
    env.hub_session();
    env.send(env.worktree_start(1, &source, &target)).await;
    env.wait_running().await;
    env.stop().await;
    fs::write(target.join("wip.txt"), "wip\n").unwrap();
    let dirty = env.remove_command(&target, false);
    env.send(dirty.clone()).await;
    let cj = env
        .wait_cj("dirty answer", |cj| !removed_outcomes(cj).is_empty())
        .await;
    assert_eq!(
        removed_outcomes(&cj),
        [("dirty".into(), "dirty".into(), false)]
    );
    assert!(env.terminal(&dirty).await);
    let outside = env.remove_command(&env.work, true);
    env.send(outside.clone()).await;
    let cj = env
        .wait_cj("refusal", |cj| removed_outcomes(cj).len() >= 2)
        .await;
    assert_eq!(
        removed_outcomes(&cj)[1],
        ("failed".into(), "path_outside_root".into(), false)
    );
    assert!(env.terminal(&outside).await);
    assert!(target.exists());
    // Команда принята, но управляющая служба упала до исполнения: после старта она доигрывается.
    let forced = env.remove_command(&target, true);
    let copy = forced.clone();
    env.thread()
        .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
        .await
        .unwrap();
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    let cj = env
        .wait_cj("replayed removal", |cj| removed_outcomes(cj).len() >= 3)
        .await;
    assert_eq!(
        removed_outcomes(&cj)[2],
        ("removed".into(), String::new(), false)
    );
    assert!(env.terminal(&forced).await);
    assert!(!target.exists());
}

fn capability_rejected(cj: &[Body], id: Uuid) -> bool {
    cj.iter().any(|b| {
        matches!(b, Body::CommandRejected(f) if f.command_id == id.to_string() && f.reason == "capability_missing")
    })
}

fn steer_command(id: Uuid, env: &Env) -> Command {
    command(
        id,
        1,
        env.execution,
        1,
        Kind::SteerInput(canterlot_proto::worker::v1::SteerInput {
            input_id: Uuid::now_v7().to_string(),
            conversation_id: env.conversation.to_string(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text: "now".into() })),
            }],
            ..Default::default()
        }),
    )
}

// Сопровождение без списка возможностей (как 1.0.0 и старше): новые виды команд ему не
// уходят, хаб получает отказ, и связь остаётся.
#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn supervisor_without_capabilities_gets_no_compact_or_steer() {
    let mut env = Env::new().await;
    env.manager().strip_ipc_capabilities();
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let compact = Uuid::now_v7();
    env.send(command(
        compact,
        1,
        env.execution,
        1,
        Kind::CompactSession(canterlot_proto::worker::v1::CompactSession {}),
    ))
    .await;
    let steer = Uuid::now_v7();
    env.send(steer_command(steer, &env)).await;
    env.wait_cj("both refused", |cj| {
        capability_rejected(cj, compact) && capability_rejected(cj, steer)
    })
    .await;
    let probe = Uuid::now_v7();
    env.send(deliver_command(
        probe,
        1,
        env.execution,
        1,
        env.conversation,
        "probe",
    ))
    .await;
    env.wait("the link still delivers", |b| finished(b, probe))
        .await;
    env.stop().await;
}

#[tokio::test]
async fn supervisor_with_capabilities_gets_compact() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let compact = Uuid::now_v7();
    env.send(command(
        compact,
        1,
        env.execution,
        1,
        Kind::CompactSession(canterlot_proto::worker::v1::CompactSession {}),
    ))
    .await;
    env.wait("compaction answered by the supervisor", |b| {
        b.iter()
            .any(|x| matches!(x, Body::CompactionStatus(f) if f.command_id == compact.to_string()))
    })
    .await;
    let cj = env.wait_cj("nothing refused", |_| true).await;
    assert!(!capability_rejected(&cj, compact));
    env.stop().await;
}

/// Сборка под видом выпуска: жёсткая ссылка в `releases/<sha>/bin`, чтобы `/proc/<pid>/exe`
/// сопровождения указывал в каталог выпуска. Ссылка требует той же файловой системы, что target.
fn release_exe(sha: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let bin = dir.path().join("releases").join(sha).join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("canterlot-worker");
    fs::hard_link(env!("CARGO_BIN_EXE_canterlot-worker"), &exe).unwrap();
    (dir, exe)
}

/// Сопровождение выпуска `sha` отвечает без списка возможностей; ответ — отказаны ли
/// сжатие и вливание.
#[cfg(feature = "fault-injection")]
async fn legacy_release(sha: &str) -> (bool, bool) {
    let (_release, exe) = release_exe(sha);
    let mut env = Env::with_supervisor_exe(exe).await;
    env.manager().strip_ipc_capabilities();
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let compact = Uuid::now_v7();
    env.send(command(
        compact,
        1,
        env.execution,
        1,
        Kind::CompactSession(canterlot_proto::worker::v1::CompactSession {}),
    ))
    .await;
    let steer = Uuid::now_v7();
    env.send(steer_command(steer, &env)).await;
    // Каждая команда получает ответ: отказ управляющей службы в CJ или итог сопровождения в SJ.
    let answered = |id: Uuid, cj: &[Body], sj: &[Body]| {
        capability_rejected(cj, id)
            || sj.iter().any(|b| match b {
                Body::CompactionStatus(f) => f.command_id == id.to_string(),
                Body::DispatchDeferred(f) => f.command_id == id.to_string(),
                Body::InputRejected(f) => f.command_id == id.to_string(),
                _ => false,
            })
    };
    let cj = timeout(WAIT, async {
        loop {
            env.pump(Duration::from_millis(100)).await;
            let cj = env.cj().await;
            if answered(compact, &cj, &env.bodies) && answered(steer, &cj, &env.bodies) {
                return cj;
            }
        }
    })
    .await
    .expect("both commands answered");
    let outcome = (
        capability_rejected(&cj, compact),
        capability_rejected(&cj, steer),
    );
    env.stop().await;
    outcome
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn legacy_supervisor_of_v1_0_0_gets_compact_and_steer() {
    assert_eq!(legacy_release("8be10fce886b").await, (false, false));
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn legacy_supervisor_of_an_older_release_gets_compact_only() {
    assert_eq!(legacy_release("d3efc1e").await, (false, true));
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn legacy_supervisor_of_an_unknown_release_gets_neither() {
    assert_eq!(legacy_release("deadbeefcafe").await, (true, true));
}

async fn accept_only(env: &Env, command: Command) -> Accept {
    let copy = command.clone();
    let accept = env
        .thread()
        .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
        .await
        .unwrap();
    assert!(
        matches!(
            accept,
            Accept::Stored { .. } | Accept::Redelivered { .. } | Accept::Duplicate
        ),
        "{accept:?}"
    );
    accept
}

// Перезапуск + первая попытка отдают принятый `StartExecution` дважды:
// исполнение должно запуститься ровно один раз.
#[tokio::test]
async fn startup_verify_then_redelivered_start_spawns_once() {
    let mut env = Env::new().await;
    env.hub_session();
    let start = start_command(env.execution, 1, env.conversation, "synthetic", &env.work);
    // Команда принята в хранилище, но не отдана исполнению.
    accept_only(&env, start.clone()).await;
    // Перезапуск воркера: `startup_verify` отдаёт команду.
    env.manager().startup_verify().await.unwrap();
    // Первая попытка: повторная отдача той же команды.
    env.manager()
        .handle(start.clone(), Accept::Redelivered { cj_seq: 0 })
        .await
        .unwrap();
    env.wait_running().await;
    let supervisors = supervisors_of(env.root.path());
    assert_eq!(
        supervisors.len(),
        1,
        "single supervisor, got {supervisors:?}"
    );
    env.stop().await;
    assert_eq!(env.stored_commands().await, 0, "nothing stays in the inbox");
}

// Перезапуск + первая попытка отдают принятую отмену дважды:
// итог должен записаться ровно один раз.
#[tokio::test]
async fn redelivered_cancel_after_restart_cancels_once() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let target = Uuid::now_v7().to_string();
    let cancel = command(
        Uuid::now_v7(),
        1,
        env.execution,
        env.generation_of_execution(),
        Kind::CancelInput(CancelInput {
            target_command_id: target.clone(),
            target_dispatch_attempt: 1,
        }),
    );
    // Команда принята в хранилище, но не отдана исполнению.
    accept_only(&env, cancel.clone()).await;
    // Перезапуск воркера: `startup_verify` отдаёт команду.
    env.close_manager().await;
    env.reopen();
    env.manager().startup_verify().await.unwrap();
    env.hub_session();
    // Первая попытка: повторная отдача той же команды.
    env.manager()
        .handle(cancel.clone(), Accept::Redelivered { cj_seq: 0 })
        .await
        .unwrap();
    let cj = env
        .wait_cj("cancel won", |cj| {
            cj.iter()
                .any(|b| matches!(b, Body::CancelWon(f) if f.command_id == target))
        })
        .await;
    let won = cj
        .iter()
        .filter(|b| matches!(b, Body::CancelWon(f) if f.command_id == target))
        .count();
    assert_eq!(won, 1, "cancel outcome must be recorded exactly once");
    assert!(env.terminal(&cancel).await);
    env.stop().await;
    assert_eq!(env.stored_commands().await, 0, "nothing stays in the inbox");
}
