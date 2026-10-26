#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use canterlot_proto::worker::{
    ipc::{Ownership, OwnershipPhase},
    v1::{
        CancelInput, Command, ContentBlock, DeliverInput, Interrupt, InterruptState,
        StartExecution, StopExecution, TextBlock, WithdrawInput, command::Kind, content_block,
        record_body::Body,
    },
};
use canterlot_worker::{
    config::{ExecutionConfig, SupervisorTimers},
    exec::{ExecutionManager, SjRelay},
    session_store::StoreThread,
    store::{Accept, StoreError, WorkerStore},
};
use tempfile::TempDir;
use tokio::{sync::mpsc, time::timeout};
use uuid::Uuid;

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
        }),
    )
}

struct Env {
    root: TempDir,
    work: PathBuf,
    config: ExecutionConfig,
    thread: Option<StoreThread>,
    manager: Option<ExecutionManager>,
    relay: mpsc::Receiver<SjRelay>,
    bodies: Vec<Body>,
    seen: HashSet<(Uuid, u64)>,
    committed: HashMap<Uuid, u64>,
    execution: Uuid,
    conversation: Uuid,
}

impl Drop for Env {
    fn drop(&mut self) {
        for pid in supervisors_of(self.root.path()) {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }
}

fn open_store(root: &Path) -> StoreThread {
    StoreThread::new(WorkerStore::open(&root.join("store")).unwrap())
}

impl Env {
    async fn new() -> Env {
        let root = TempDir::new().unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let config = ExecutionConfig {
            allowed_workdir_roots: vec![work.clone()],
            supervisor_exe: Some(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))),
            spawn_timeout: Duration::from_secs(10),
            supervisor: SupervisorTimers {
                stop_grace: Duration::from_secs(1),
                stop_retry: Duration::from_millis(200),
                ..Default::default()
            },
        };
        let thread = open_store(root.path());
        let (sender, relay) = mpsc::channel(64);
        let manager = ExecutionManager::new(config.clone(), thread.client(), sender);
        Env {
            root,
            work,
            config,
            thread: Some(thread),
            manager: Some(manager),
            relay,
            bodies: Vec::new(),
            seen: HashSet::new(),
            committed: HashMap::new(),
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
                self.manager().on_sj_ack(batch.journal_id, last);
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
            "timed out waiting for: {what}; bodies: {:?}",
            self.bodies.iter().map(kind).collect::<Vec<_>>()
        );
    }

    async fn wait_running(&mut self) {
        self.wait("running", |b| b.iter().any(|x| state(x, "running")))
            .await;
    }

    async fn cj(&self) -> Vec<Body> {
        self.thread()
            .call(|store| {
                Ok(store
                    .as_ref()
                    .unwrap()
                    .cj_records_from(1, usize::MAX)?
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
        .unwrap_or_else(|_| panic!("timed out waiting for CJ: {what}"))
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
        self.manager = Some(ExecutionManager::new(
            self.config.clone(),
            thread.client(),
            sender,
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
        Body::JournalClosed(_) => "journal_closed",
        Body::CommandStored(_) => "command_stored",
        _ => "other",
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
    assert!(!record.boot_id.is_empty() && record.ipc_socket.ends_with("ipc.sock"));
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
    for entry in fs::read_dir(env.root.path().join("store/executions")).unwrap() {
        assert_eq!(
            entry.unwrap().file_name().to_string_lossy(),
            first.to_string(),
            "a rejected start never creates an execution directory"
        );
    }
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
        .arg("60")
        .spawn()
        .unwrap();
    let sleeper = unrelated.id();
    let sleeper_ticks = ticks(sleeper).unwrap();
    let mut originals = supervisors_of(env.root.path());
    assert_eq!(originals.len(), 3);
    env.close_manager().await;
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
    assert!(env.manager().startup_verify().await.unwrap().is_empty());
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
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[tokio::test]
async fn supervisor_death_reports_owner_lost() {
    let mut env = Env::new().await;
    env.hub_session();
    env.start(1).await;
    env.wait_running().await;
    let record = env.ownership(env.execution).await.unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &record.supervisor_pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let id = env.execution;
    env.wait_cj("owner_lost", |cj| owner_lost(cj, id) == 1)
        .await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(owner_lost(&env.cj().await, id), 1, "reported once");
    assert!(supervisors_of(env.root.path()).is_empty(), "no respawn");
    assert_eq!(
        env.ownership(id).await.unwrap().phase,
        OwnershipPhase::Attached as i32,
        "nothing is released"
    );
    let observations = env.manager().observations();
    assert_eq!(observations[0].state, "owner_lost");
    assert!(!observations[0].supervisor_attached);
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
    let env = Env::new().await;
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
