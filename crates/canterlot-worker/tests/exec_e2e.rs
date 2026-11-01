#![forbid(unsafe_code)]
#![cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use canterlot_hub::{
    blocks,
    config::HubTimers,
    delivery::{
        control::{self, StopExecutionOutcome, StopOutcome},
        submit::{self, ContentBlock, SubmitInput, SubmittedInput},
    },
    executions,
    fault_injection::{Faults, Point, SjFact},
    machines,
    testkit::{self, TestHub, spawn_test_hub, spawn_test_hub_with_faults},
};
use canterlot_journal::{Journal, JournalOptions};
use canterlot_proto::worker::v1::{InterruptState, RecordBody, command, record_body::Body};
use canterlot_worker::{
    adapter::opencode::replay::Script,
    config::{ExecutionConfig, OpenCodeConfig, SupervisorTimers, WorkerConfig, WorkerTimers},
    fault_injection::{SessionProbe, StoreProbe},
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use prost::Message;
use rustix::process::Signal;
use serde_json::{Value, json};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};
use tonic::Code;
use uuid::Uuid;

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/opencode.rs"]
mod opencode;
use opencode::fake_exec;
#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

const WAIT: Duration = Duration::from_secs(60);
type Identity = (u32, u64);
type ProcessHistory = Arc<Mutex<BTreeSet<(Identity, bool)>>>;

fn hub_timers() -> HubTimers {
    HubTimers {
        dispatch_sweep: Duration::from_millis(20),
        ..Default::default()
    }
}

async fn until(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

fn script(steps: &str) -> String {
    format!(r#"{{"synthetic":[{steps}]}}"#)
}

fn answer(count: usize, delay_ms: u64) -> (String, String) {
    let mut steps = Vec::new();
    let mut expected = String::new();
    for index in 0..count {
        let delta = format!("{index:04}λ|");
        expected.push_str(&delta);
        steps.push(json!({"delta":delta}));
        if delay_ms > 0 {
            steps.push(json!({"delay_ms":delay_ms}));
        }
    }
    (json!({"synthetic":steps}).to_string(), expected)
}

fn process_role_at(proc: &Path, expected_ticks: u64) -> Option<bool> {
    const PF_FORKNOEXEC: u64 = 0x40;
    let live_execution = || {
        let stat = fs::read_to_string(proc.join("stat")).ok()?;
        let fields: Vec<_> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
        Some(
            !matches!(fields.first(), Some(&"Z") | Some(&"X"))
                && fields.get(19)?.parse::<u64>().ok()? == expected_ticks
                && fields.get(6)?.parse::<u64>().ok()? & PF_FORKNOEXEC == 0,
        )
    };
    // До exec ребёнок показывает exe и argv родителя. Проверяем флаг до cmdline,
    // чтобы exec между чтениями не выдал унаследованный argv за новую роль.
    if !live_execution()? {
        return None;
    }
    let args = fs::read(proc.join("cmdline")).ok()?;
    let supervisor = args.split(|b| *b == 0).any(|arg| arg == b"supervise");
    if !supervisor && !args.split(|b| *b == 0).any(|arg| arg == b"synthetic-cli") {
        return None;
    }
    live_execution()?.then_some(supervisor)
}

fn process_observation(identity: Identity) -> Option<(Identity, bool)> {
    process_role_at(&PathBuf::from(format!("/proc/{}", identity.0)), identity.1)
        .map(|supervisor| (identity, supervisor))
}

#[test]
fn process_observation_waits_for_exec_without_hiding_second_supervisor() {
    let proc = tempfile::tempdir().unwrap();
    let child = (27, 208356280);
    let supervisor = ((20, 208356235), true);
    let mut history = BTreeSet::from([supervisor]);
    // Снимок CLI перед execve из воспроизведения под strace: argv ещё от родителя.
    let forked_stat =
        "27 (canterlot-worke) R 20 27 20 0 -1 4194368 1 0 0 0 0 0 0 0 20 0 1 0 208356280\n";
    fs::write(proc.path().join("stat"), forked_stat).unwrap();
    fs::write(proc.path().join("cmdline"), b"worker\0supervise\0").unwrap();
    history.extend(process_role_at(proc.path(), child.1).map(|role| (child, role)));
    assert_eq!(history, BTreeSet::from([supervisor]));

    fs::write(
        proc.path().join("stat"),
        forked_stat.replace("4194368", "4194304"),
    )
    .unwrap();
    fs::write(proc.path().join("cmdline"), b"worker\0synthetic-cli\0").unwrap();
    history.extend(process_role_at(proc.path(), child.1).map(|role| (child, role)));
    assert_eq!(history, BTreeSet::from([supervisor, (child, false)]));

    // Если ребёнок действительно исполнил supervise, второй владелец учитывается.
    fs::write(proc.path().join("cmdline"), b"worker\0supervise\0").unwrap();
    history.extend(process_role_at(proc.path(), child.1).map(|role| (child, role)));
    assert_eq!(
        history.iter().filter(|(_, supervisor)| *supervisor).count(),
        2
    );
}

#[test]
fn process_observation_rejects_stale_or_exited_process() {
    let proc = tempfile::tempdir().unwrap();
    let stat = "27 (canterlot-worke) R 20 27 20 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 208356280\n";
    fs::write(proc.path().join("stat"), stat).unwrap();
    fs::write(proc.path().join("cmdline"), b"worker\0supervise\0").unwrap();
    assert_eq!(process_role_at(proc.path(), 208356280), Some(true));
    assert_eq!(process_role_at(proc.path(), 208356279), None);
    for state in ["Z", "X"] {
        fs::write(
            proc.path().join("stat"),
            stat.replace(") R ", &format!(") {state} ")),
        )
        .unwrap();
        assert_eq!(process_role_at(proc.path(), 208356280), None);
    }
    fs::write(proc.path().join("stat"), stat).unwrap();
    fs::write(proc.path().join("cmdline"), b"").unwrap();
    assert_eq!(process_role_at(proc.path(), 208356280), None);
}

fn same_process((pid, ticks): Identity) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(end) = stat.rfind(')') else {
        return false;
    };
    let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
    !matches!(fields.first(), Some(&"Z") | Some(&"X"))
        && fields.get(19).and_then(|s| s.parse::<u64>().ok()) == Some(ticks)
}

struct RunningWorker {
    stop: watch::Sender<bool>,
    state: watch::Receiver<SessionState>,
    join: JoinHandle<Result<(), WorkerError>>,
}

impl Drop for RunningWorker {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.join.abort();
    }
}

impl RunningWorker {
    async fn stop(mut self) {
        self.stop.send(true).unwrap();
        timeout(WAIT, &mut self.join)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

struct Fixture {
    worker: Option<RunningWorker>,
    watcher: JoinHandle<()>,
    guard: Arc<ProcessGuard>,
    root: TempDir,
    hub: Option<TestHub>,
    pool: PgPool,
    config: WorkerConfig,
    owner: Uuid,
    conversation: Uuid,
    execution: Uuid,
    start_command: Uuid,
    store_probe: StoreProbe,
    session_probe: SessionProbe,
    history: ProcessHistory,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.watcher.abort();
        self.worker.take();
        if let Some(hub) = self.hub.take() {
            let _ = hub.shutdown.send(());
        }
        self.guard.cleanup();
    }
}

impl Fixture {
    async fn new(pool: &PgPool, profile: &str, env: &[(&str, &str)]) -> Self {
        Self::new_with(pool, profile, env, false, true).await
    }

    async fn new_with(
        pool: &PgPool,
        profile: &str,
        env: &[(&str, &str)],
        long_dir: bool,
        auto_start: bool,
    ) -> Self {
        Self::new_full(pool, profile, env, long_dir, auto_start, None).await
    }

    async fn new_full(
        pool: &PgPool,
        profile: &str,
        env: &[(&str, &str)],
        long_dir: bool,
        auto_start: bool,
        opencode: Option<OpenCodeConfig>,
    ) -> Self {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let root = tempfile::tempdir().unwrap();
        let guard = Arc::new(ProcessGuard::new(root.path()));
        let history: ProcessHistory = Arc::default();
        let process_guard = guard.clone();
        let observed = history.clone();
        let watcher = tokio::spawn(async move {
            loop {
                for (member, supervisor) in process_guard
                    .members()
                    .into_iter()
                    .filter_map(process_observation)
                {
                    observed.lock().unwrap().insert((member, supervisor));
                }
                sleep(Duration::from_millis(10)).await;
            }
        });
        let machine = machines::add(pool, hub.installation.owner_id, "exec-e2e")
            .await
            .unwrap();
        let token_file = root.path().join("token");
        fs::write(&token_file, machine.token).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let ca_cert = root.path().join("ca.pem");
        fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
        let workdir = root.path().join("work");
        fs::create_dir(&workdir).unwrap();
        let wrapper = root.path().join("supervisor");
        let mut script = format!(
            "#!/bin/sh\nexport CANTERLOT_SYNTHETIC_STDIN_LOG='{}'\n",
            root.path().join("receipts").display()
        );
        for (key, value) in env {
            script.push_str(&format!("export {key}='{value}'\n"));
        }
        // Сопровождение старого релиза под новым воркером: README, «Проверки».
        let supervisor = std::env::var("CANTERLOT_E2E_SUPERVISOR_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_canterlot-worker").to_owned());
        script.push_str(&format!("exec '{supervisor}' \"$@\"\n"));
        fake_exec::write_executable(&wrapper, script, 0o700);
        let config = WorkerConfig {
            machine: Default::default(),
            attachments: Default::default(),
            claude_code: None,
            codex: None,
            hub_url: format!("https://localhost:{}", hub.addr.port()),
            ca_cert,
            token_file,
            machine_id: machine.machine_id,
            installation_id: hub.installation.installation_id,
            restore_generation: hub.installation.restore_generation,
            data_dir: if long_dir {
                root.path().join("d".repeat(80)).join("store")
            } else {
                root.path().join("store")
            },
            timers: WorkerTimers {
                handshake: Duration::from_secs(5),
                heartbeat: Duration::from_millis(100),
                heartbeat_ack: Duration::from_secs(5),
                range_ack: Duration::from_secs(3),
                backoff_min: Duration::from_millis(250),
                backoff_max: Duration::from_millis(500),
                ..Default::default()
            },
            execution: ExecutionConfig {
                allowed_workdir_roots: vec![workdir.clone()],
                supervisor_exe: Some(wrapper),
                spawn_timeout: Duration::from_secs(2),
                supervisor: SupervisorTimers {
                    stop_grace: Duration::from_millis(500),
                    stop_retry: Duration::from_millis(300),
                    ..Default::default()
                },
            },
            opencode,
        };
        let owner = hub.installation.owner_id;
        let conversation = testkit::seed_conversation(pool, owner, machine.machine_id).await;
        sqlx::query("update conversations set workdir = $2, harness = $3 where id = $1")
            .bind(conversation)
            .bind(workdir.to_str().unwrap())
            .bind(profile)
            .execute(pool)
            .await
            .unwrap();
        let mut fixture = Self {
            worker: None,
            watcher,
            guard,
            root,
            hub: Some(hub),
            pool: pool.clone(),
            config,
            owner,
            conversation,
            execution: Uuid::nil(),
            start_command: Uuid::nil(),
            store_probe: StoreProbe::default(),
            session_probe: SessionProbe::default(),
            history,
        };
        fixture.start_worker().await;
        if !auto_start {
            return fixture;
        }
        let started = executions::start(pool, owner, conversation, Uuid::now_v7())
            .await
            .unwrap();
        fixture.execution = started.execution_id;
        fixture.start_command = started.command_id;
        fixture.wait_state("running").await;
        until("start command completed", async || {
            fixture.command_done(fixture.start_command).await
        })
        .await;
        let native: Option<String> =
            sqlx::query_scalar("select native_conversation_id from executions where id = $1")
                .bind(fixture.execution)
                .fetch_one(pool)
                .await
                .unwrap();
        assert!(native.is_some_and(|id| id.starts_with("synthetic-")));
        fixture.identities();
        fixture
    }

    fn hub(&self) -> &TestHub {
        self.hub.as_ref().unwrap()
    }
    fn faults(&self) -> &Faults {
        &self.hub().faults
    }
    fn exec_dir(&self) -> PathBuf {
        self.config
            .data_dir
            .join("executions")
            .join(self.execution.to_string())
    }

    async fn start_worker(&mut self) {
        let store = WorkerStore::open(&self.config.data_dir)
            .unwrap()
            .with_probe(self.store_probe.clone());
        let worker =
            Worker::new(self.config.clone(), store).with_session_probe(self.session_probe.clone());
        let state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        self.worker = Some(RunningWorker {
            stop,
            state,
            join: tokio::spawn(worker.run(shutdown)),
        });
        // Live выставляется при отправке сверки, раньше её применения хабом.
        until("worker live", async || {
            *self.worker.as_ref().unwrap().state.borrow() == SessionState::Live
                && self.hub().registry.is_reconciled(self.config.machine_id)
        })
        .await;
    }

    async fn stop_worker(&mut self) {
        self.worker.take().unwrap().stop().await;
    }

    async fn restart_hub(&mut self) {
        let hub = self.hub.take().unwrap();
        let addr = hub.addr;
        let pki = hub.pki.clone();
        let faults = hub.faults.clone();
        hub.shutdown.send(()).unwrap();
        timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
        self.hub = Some(
            spawn_test_hub_with_faults(self.pool.clone(), hub_timers(), Some(addr), pki, faults)
                .await,
        );
        until("worker reconnected", async || {
            *self.worker.as_ref().unwrap().state.borrow() == SessionState::Live
                && self.hub().registry.is_reconciled(self.config.machine_id)
        })
        .await;
    }

    async fn state(&self) -> String {
        sqlx::query_scalar("select state from executions where id = $1")
            .bind(self.execution)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn wait_state(&self, state: &str) {
        until(state, async || self.state().await == state).await;
    }

    fn identities(&self) -> (Identity, Identity) {
        let (supervisors, cli): (Vec<_>, Vec<_>) = self
            .guard
            .members()
            .into_iter()
            .filter_map(process_observation)
            .partition(|(_, supervisor)| *supervisor);
        assert_eq!(supervisors.len(), 1);
        assert_eq!(cli.len(), 1);
        let (supervisor, cli) = (supervisors[0].0, cli[0].0);
        assert!(same_process(supervisor) && same_process(cli));
        self.history
            .lock()
            .unwrap()
            .extend([(supervisor, true), (cli, false)]);
        (supervisor, cli)
    }

    fn assert_same_processes(&self, identities: (Identity, Identity)) {
        assert_eq!(self.identities(), identities);
        let history = self.history.lock().unwrap();
        assert_eq!(
            history.iter().filter(|(_, supervisor)| *supervisor).count(),
            1,
            "process history: {history:?}"
        );
        assert_eq!(
            history.iter().filter(|(_, supervisor)| !supervisor).count(),
            1,
            "process history: {history:?}"
        );
    }

    async fn submit(&self, text: String) -> SubmittedInput {
        let result = submit::submit_input(
            &self.pool,
            SubmitInput {
                owner_id: self.owner,
                conversation_id: self.conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap();
        self.hub().dispatcher.trigger(self.conversation);
        result
    }

    async fn input_state(&self, input: Uuid) -> String {
        sqlx::query_scalar("select state from inputs where id = $1")
            .bind(input)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn command_done(&self, command: Uuid) -> bool {
        sqlx::query_scalar("select delivery_state = 'completed' from commands where id = $1")
            .bind(command)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn wait_deltas(&self, count: i64) {
        until("streamed deltas", async || {
            self.event_count("block_delta").await >= count
        })
        .await;
        assert_eq!(self.state().await, "running");
        let states: Vec<String> =
            sqlx::query_scalar("select state from blocks where execution_id = $1")
                .bind(self.execution)
                .fetch_all(&self.pool)
                .await
                .unwrap();
        assert_eq!(states.len(), 1);
        assert!(matches!(states[0].as_str(), "streaming" | "final"));
    }

    async fn event_count(&self, kind: &str) -> i64 {
        sqlx::query_scalar("select count(*) from events where execution_id = $1 and kind = $2")
            .bind(self.execution)
            .bind(kind)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn slot(&self) -> String {
        sqlx::query_scalar("select run_slot from queue_state where conversation_id = $1")
            .bind(self.conversation)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn paused(&self, reason: &str) -> bool {
        sqlx::query_scalar("select pause_reasons @> jsonb_build_array(jsonb_build_object('reason', $2::text)) from queue_state where conversation_id = $1")
            .bind(self.conversation)
            .bind(reason)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn interrupt(&self) -> (Uuid, Option<Uuid>) {
        let StopOutcome::Requested {
            interrupt_command_id,
            withdraw_command_id,
        } = control::request_stop(
            &self.pool,
            self.owner,
            self.conversation,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        assert!(self.paused("user_stop").await);
        (interrupt_command_id.unwrap(), withdraw_command_id)
    }

    async fn stop_execution(&self, force: bool) -> Uuid {
        let result = control::request_stop_execution(
            &self.pool,
            self.owner,
            self.conversation,
            force,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        match result {
            StopExecutionOutcome::Requested { command_id, .. }
            | StopExecutionOutcome::AlreadyRequested { command_id } => command_id,
            StopExecutionOutcome::NothingToStop | StopExecutionOutcome::OwnerLost => {
                panic!("execution unexpectedly absent")
            }
        }
    }

    fn sj_snapshot(&self) -> Option<(TempDir, Journal)> {
        for _ in 0..100 {
            // Открытый inode переживает compaction; временные файлы ACK не входят в снимок.
            let entries = match fs::read_dir(self.exec_dir().join("sj")) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => panic!("SJ snapshot failed: {}", error.kind()),
            };
            let sources: std::io::Result<Vec<_>> = entries
                .filter_map(|entry| match entry {
                    Ok(entry) => {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if matches!(name_str.as_ref(), "header" | "ack" | "sealed")
                            || (name_str.starts_with("seg-") && name_str.ends_with(".log"))
                        {
                            Some(fs::File::open(entry.path()).map(|file| (name, file)))
                        } else {
                            None
                        }
                    }
                    Err(error) => Some(Err(error)),
                })
                .collect();
            let sources = match sources {
                Ok(sources) => sources,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("SJ snapshot open failed: {}", error.kind()),
            };
            let copy = tempfile::tempdir().unwrap();
            for (name, mut source) in sources {
                let mut target = fs::File::create(copy.path().join(name)).unwrap();
                std::io::copy(&mut source, &mut target).unwrap();
            }
            if let Ok((journal, report)) = Journal::open(copy.path(), JournalOptions::default())
                && report.corrupted_tail.is_none()
                && journal.is_sealed().is_none()
            {
                return Some((copy, journal));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("could not obtain a complete SJ snapshot");
    }

    fn sj(&self) -> (Uuid, u64, Vec<Body>) {
        let disk = self.sj_snapshot();
        let batches = self
            .hub
            .as_ref()
            .map(|hub| hub.faults.batches())
            .unwrap_or_default();
        let journal_id = disk.as_ref().map(|(_, j)| j.id().0).unwrap_or_else(|| {
            batches
                .iter()
                .find(|b| b.records.iter().any(|r| r.sj_fact.is_some()))
                .map(|b| Uuid::parse_str(&b.journal_id).unwrap())
                .expect("missing SJ trace")
        });
        let mut last_seq = disk.as_ref().map_or(0, |(_, j)| j.last_seq());
        let mut bodies = BTreeMap::new();
        if let Some((_, journal)) = &disk {
            let mut next = journal.first_seq();
            while next <= journal.last_seq() {
                for record in journal.read_from(next, 524288).unwrap() {
                    next = record.seq + 1;
                    bodies.insert(
                        record.seq,
                        RecordBody::decode(record.payload.as_slice())
                            .unwrap()
                            .body
                            .unwrap(),
                    );
                }
            }
        }
        // После ACK сегмент удалён; метаданные принятых записей объединяются по seq.
        for batch in batches
            .iter()
            .filter(|b| b.journal_id == journal_id.to_string())
        {
            for record in &batch.records {
                last_seq = last_seq.max(record.seq);
                let body = match &record.sj_fact {
                    Some(SjFact::TransmissionStarted(id, attempt)) => Body::TransmissionStarted(
                        canterlot_proto::worker::v1::InputTransmissionStarted {
                            command_id: id.clone(),
                            dispatch_attempt: *attempt,
                            ..Default::default()
                        },
                    ),
                    Some(SjFact::InputWithdrawn(id, attempt)) => {
                        Body::InputWithdrawn(canterlot_proto::worker::v1::InputWithdrawn {
                            command_id: id.clone(),
                            dispatch_attempt: *attempt,
                        })
                    }
                    Some(SjFact::InterruptStatus(id, state)) => {
                        Body::InterruptStatus(canterlot_proto::worker::v1::InterruptStatus {
                            command_id: id.clone(),
                            state: *state,
                            ..Default::default()
                        })
                    }
                    Some(SjFact::JournalClosed) => Body::JournalClosed(Default::default()),
                    Some(SjFact::BlockDelta) => Body::BlockDelta(Default::default()),
                    None => continue,
                };
                bodies.entry(record.seq).or_insert(body);
            }
        }
        (journal_id, last_seq, bodies.into_values().collect())
    }

    async fn assert_cursor(&self, closed: bool) {
        let journal = self.sj().0;
        until("SJ cursor caught up", async || {
            let length = self.sj().1;
            sqlx::query_scalar::<_, i64>(
                "select committed_seq from journal_cursors where journal_id = $1",
            )
            .bind(journal)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
                == Some(length as i64)
        })
        .await;
        let row: (Uuid, Uuid, String, bool) = sqlx::query_as("select machine_id, execution_id, kind, closed from journal_cursors where journal_id = $1")
            .bind(journal).fetch_one(&self.pool).await.unwrap();
        assert_eq!(
            row,
            (self.config.machine_id, self.execution, "sj".into(), closed)
        );
        for ack in self.faults().acks() {
            assert!(ack.range.up_to_seq <= ack.committed_seq);
        }
    }

    fn cli_receipts(&self, command: Uuid) -> Vec<u32> {
        fs::read_to_string(self.root.path().join("receipts"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .inspect(|row| {
                let fields = row.as_object().unwrap();
                assert_eq!(fields.len(), 3);
                assert!(
                    fields
                        .keys()
                        .all(|key| matches!(key.as_str(), "type" | "command_id" | "attempt"))
                );
            })
            .filter(|row| row["command_id"].as_str() == Some(command.to_string().as_str()))
            .map(|row| row["attempt"].as_u64().unwrap() as u32)
            .collect()
    }

    async fn assert_answer(&self, input: &SubmittedInput, expected: &str, deltas: usize) {
        until("answer finalized and slot free", async || {
            self.event_count("block_final").await == 1 && self.slot().await == "free"
        })
        .await;
        assert_eq!(self.input_state(input.input_id).await, "accepted");
        let command: (String, String, bool, bool) = sqlx::query_as("select delivery_state, last_status, stored_at is not null, completed_at is not null from commands where id = $1")
            .bind(input.command_id).fetch_one(&self.pool).await.unwrap();
        assert_eq!(command, ("completed".into(), "accepted".into(), true, true));
        let rows: Vec<(Uuid, String, String, i64, String, Uuid)> = sqlx::query_as("select block_id, state, content, rev, checksum, input_command_id from blocks where execution_id = $1")
            .bind(self.execution).fetch_all(&self.pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let (block, state, content, rev, checksum, input_command) = &rows[0];
        assert_eq!(state, "final");
        assert!(content == expected, "block content mismatch");
        assert_eq!(checksum, &blocks::checksum(expected));
        assert_eq!(*input_command, input.command_id);
        let events: Vec<(String, Value)> = sqlx::query_as("select kind, payload from events where execution_id = $1 and kind in ('block_started','block_delta','block_final') order by scope_seq")
            .bind(self.execution).fetch_all(&self.pool).await.unwrap();
        assert_eq!(events.first().unwrap().0, "block_started");
        assert_eq!(events.last().unwrap().0, "block_final");
        assert_eq!(events.len(), deltas + 2);
        let mut previous = None;
        let mut accumulated = String::new();
        for (kind, payload) in events {
            assert_eq!(
                payload["block_id"].as_str(),
                Some(block.to_string().as_str())
            );
            let current = payload["rev"].as_i64().unwrap();
            if let Some(last) = previous {
                assert!(current > last, "block revisions must grow");
            }
            if kind == "block_delta" {
                assert_eq!(payload["base_rev"].as_i64(), previous);
                accumulated.push_str(payload["append"].as_str().unwrap());
            }
            previous = Some(current);
        }
        assert!(accumulated == expected, "delta event content mismatch");
        assert_eq!(previous, Some(*rev));
        assert_eq!(self.event_count("run_finished").await, 1);
        assert_eq!(
            self.cli_receipts(input.command_id),
            [self.attempt(input.command_id).await as u32]
        );
        self.assert_cursor(false).await;
        self.assert_contiguous().await;
    }

    async fn attempt(&self, command: Uuid) -> i32 {
        sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
            .bind(command)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn assert_contiguous(&self) {
        let bad: i64 = sqlx::query_scalar("select count(*) from (select e.scope from events e join scope_counters s on s.scope = e.scope group by e.scope, s.last_seq having min(e.scope_seq) <> 1 or max(e.scope_seq) <> count(*) or max(e.scope_seq) <> s.last_seq) gaps")
            .fetch_one(&self.pool).await.unwrap();
        assert_eq!(bad, 0, "events must be contiguous in every scope");
        let diagnostics: i64 = sqlx::query_scalar("select count(*) from events where kind in ('block_revision_gap','block_checksum_mismatch','block_conflicting_final','journal_record_mismatch','state_divergence')")
            .fetch_one(&self.pool).await.unwrap();
        assert_eq!(diagnostics, 0);
    }

    async fn assert_never_left_running(&self) {
        assert_eq!(self.state().await, "running");
        let states: Vec<String> = sqlx::query_scalar("select payload->>'state' from events where execution_id = $1 and kind = 'execution_state_changed' order by scope_seq")
            .bind(self.execution).fetch_all(&self.pool).await.unwrap();
        assert_eq!(states, ["created", "starting", "running"]);
    }

    async fn finish(mut self) {
        if self.state().await == "running" {
            self.stop_execution(true).await;
        }
        if !matches!(self.state().await.as_str(), "owner_lost" | "failed") {
            self.wait_state("stopped").await;
            until("journal closed", async || sqlx::query_scalar::<_, bool>("select coalesce(bool_or(closed), false) from journal_cursors where execution_id = $1")
                .bind(self.execution).fetch_one(&self.pool).await.unwrap()).await;
            self.assert_cursor(true).await;
            until("acked execution directory removed", async || {
                !self.exec_dir().exists()
            })
            .await;
        }
        self.stop_worker().await;
        let hub = self.hub.take().unwrap();
        hub.shutdown.send(()).unwrap();
        timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
        self.guard.cleanup();
    }
}

fn transmissions(bodies: &[Body], command: Uuid) -> Vec<u32> {
    bodies
        .iter()
        .filter_map(|body| match body {
            Body::TransmissionStarted(fact) if fact.command_id == command.to_string() => {
                Some(fact.dispatch_attempt)
            }
            _ => None,
        })
        .collect()
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn conversation_turn_end_to_end(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let before = fixture.faults().pause_next(Point::BeforeSjCommit);
    let (script, expected) = answer(8, 80);
    let input = fixture.submit(script).await;
    let resume = timeout(WAIT, before).await.unwrap().unwrap();
    until("input on machine", async || {
        fixture.input_state(input.input_id).await == "on_machine"
    })
    .await;
    assert_eq!(fixture.slot().await, "dispatching");
    resume.send(()).unwrap();
    fixture.wait_deltas(1).await;
    fixture.assert_answer(&input, &expected, 8).await;
    let states: Vec<String> = sqlx::query_scalar("select payload->>'state' from events where conversation_id = $1 and kind in ('input_queued', 'input_state_changed') and payload->>'input_id' = $2 order by scope_seq")
        .bind(fixture.conversation).bind(input.input_id.to_string()).fetch_all(&pool).await.unwrap();
    assert_eq!(
        states,
        [
            "queued",
            "dispatched",
            "on_machine",
            "transmitting",
            "accepted"
        ]
    );
    assert_eq!(transmissions(&fixture.sj().2, input.command_id), [1]);
    fixture.assert_never_left_running().await;
    fixture.assert_same_processes(identities);
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stream_break_mid_answer_yields_exact_block_text(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let (script, expected) = answer(50, 100);
    let input = fixture.submit(script).await;
    fixture.wait_deltas(3).await;
    fixture.faults().drop_next_sj_acks(6);
    fixture.faults().fail_data(Code::Cancelled);
    fixture.wait_deltas(8).await;
    let hellos = fixture.faults().hellos().len();
    fixture.faults().fail_control(Code::Unavailable);
    until("control reconnected", async || {
        fixture.faults().hellos().len() > hellos
    })
    .await;
    fixture.wait_deltas(15).await;
    fixture.restart_hub().await;
    fixture.assert_answer(&input, &expected, 50).await;
    assert!(fixture.faults().acks().iter().any(|ack| ack.dropped));
    fixture.assert_same_processes(identities);
    fixture.assert_never_left_running().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn control_service_restart_keeps_execution_and_answer(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let (script, expected) = answer(50, 80);
    let input = fixture.submit(script).await;
    fixture.wait_deltas(4).await;
    fixture.stop_worker().await;
    assert!(same_process(identities.0) && same_process(identities.1));
    sleep(Duration::from_millis(250)).await;
    fixture.start_worker().await;
    fixture.assert_answer(&input, &expected, 50).await;
    fixture.assert_same_processes(identities);
    fixture.assert_never_left_running().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn interrupt_pauses_queue_first_and_nothing_transmits_after(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    sqlx::raw_sql(
        "create function check_interrupt_pause() returns trigger language plpgsql as $$
        begin
          if new.kind = 'interrupt' and not exists (
            select 1 from queue_state where conversation_id = new.conversation_id
              and pause_reasons @> '[{\"reason\":\"user_stop\"}]'::jsonb
          ) then raise exception 'interrupt precedes queue pause'; end if;
          return new;
        end $$;
        create trigger check_interrupt_pause before insert on commands
          for each row execute function check_interrupt_pause();",
    )
    .execute(&pool)
    .await
    .unwrap();
    for profile in ["synthetic", "synthetic-exit-on-interrupt"] {
        let fixture = Fixture::new(&pool, profile, &[]).await;
        let identities = fixture.identities();
        // Ход держится до прерывания: скрипт со сроком обгонял прерывание под нагрузкой.
        let first = fixture
            .submit(script(
                r#"{"delta":"0000λ|"},{"delta":"0001λ|"},{"delta":"0002λ|"},{"hang":true}"#,
            ))
            .await;
        let second = fixture.submit(answer(2, 0).0).await;
        fixture.wait_deltas(3).await;
        let before = fixture.faults().pause_next(Point::BeforeCommandSend);
        let (interrupt, _) = fixture.interrupt().await;
        let resume = timeout(WAIT, before).await.unwrap().unwrap();
        assert!(fixture.paused("user_stop").await);
        assert!(
            !fixture
                .store_probe
                .snapshots()
                .iter()
                .any(|s| s.cj.iter().any(|r| r
                    .stored_command
                    .as_ref()
                    .is_some_and(|(id, _)| *id == interrupt.to_string())))
        );
        let sent = fixture.faults().commands().len();
        resume.send(()).unwrap();
        until("interrupt completed", async || {
            fixture.command_done(interrupt).await
        })
        .await;
        until("slot freed after interrupt", async || {
            fixture.slot().await == "free"
        })
        .await;
        sleep(Duration::from_millis(300)).await;
        assert!(fixture.paused("user_stop").await);
        assert_eq!(fixture.input_state(first.input_id).await, "accepted");
        assert_eq!(fixture.input_state(second.input_id).await, "queued");
        assert!(fixture.faults().commands()[sent..].iter().all(|c| c.kind
            != Some(std::mem::discriminant(&command::Kind::DeliverInput(
                Default::default()
            )))));
        until("interrupt completion in SJ", async || {
            fixture.sj().2.iter().any(|b| matches!(b, Body::InterruptStatus(f) if f.command_id == interrupt.to_string() && f.state == InterruptState::Completed as i32))
        })
        .await;
        let bodies = fixture.sj().2;
        assert_eq!(transmissions(&bodies, first.command_id), [1]);
        assert!(transmissions(&bodies, second.command_id).is_empty());
        assert_eq!(
            bodies
                .iter()
                .filter(|b| matches!(b, Body::TransmissionStarted(_)))
                .count(),
            1
        );
        assert_eq!(fixture.cli_receipts(first.command_id), [1]);
        assert!(fixture.cli_receipts(second.command_id).is_empty());
        let states: Vec<String> =
            sqlx::query_scalar("select state from blocks where execution_id = $1")
                .bind(fixture.execution)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(states.len(), 1);
        assert!(
            states
                .iter()
                .all(|s| matches!(s.as_str(), "incomplete" | "final"))
        );
        if profile == "synthetic-exit-on-interrupt" {
            fixture.wait_state("stopped").await;
            let outcome: String =
                sqlx::query_scalar("select outcome from executions where id = $1")
                    .bind(fixture.execution)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(outcome, "user_stop");
            assert!(!same_process(identities.1));
        } else {
            fixture.assert_never_left_running().await;
            fixture.assert_same_processes(identities);
        }
        fixture.assert_contiguous().await;
        fixture.finish().await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stop_execution_reaches_stopped_only_after_group_empty(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    for force in [false, true] {
        let fixture = Fixture::new(
            &pool,
            "synthetic",
            &[("CANTERLOT_FAULT_TERMINATE_SURVIVORS", "4")],
        )
        .await;
        let identities = fixture.identities();
        let input = fixture
            .submit(
                json!({"synthetic":[{"delta":"x"},{"detach_child":true},{"hang":true}]})
                    .to_string(),
            )
            .await;
        fixture.wait_deltas(1).await;
        until("escaped child", async || fixture.guard.members().len() == 3).await;
        let child = fixture
            .guard
            .members()
            .into_iter()
            .find(|p| *p != identities.0 && *p != identities.1)
            .unwrap();
        let child_stat = fs::read_to_string(format!("/proc/{}/stat", child.0)).unwrap();
        let cli_stat = fs::read_to_string(format!("/proc/{}/stat", identities.1.0)).unwrap();
        let cli_fields: Vec<_> = cli_stat[cli_stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .collect();
        let child_fields: Vec<_> = child_stat[child_stat.rfind(')').unwrap() + 1..]
            .split_whitespace()
            .collect();
        assert_ne!(child_fields[3], cli_fields[3], "child escaped CLI session");
        assert_eq!(
            child_fields[1].parse::<u32>().unwrap(),
            identities.0.0,
            "double-fork child adopted by supervisor"
        );
        let stop = fixture.stop_execution(force).await;
        fixture.wait_state("stopping").await;
        for _ in 0..4 {
            assert!(same_process(child));
            assert_eq!(fixture.state().await, "stopping");
            sleep(Duration::from_millis(80)).await;
        }
        fixture.wait_state("stopped").await;
        assert!(!same_process(child) && !same_process(identities.1));
        let outcome: String = sqlx::query_scalar("select outcome from executions where id = $1")
            .bind(fixture.execution)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(outcome, if force { "force_stopped" } else { "user_stop" });
        assert_eq!(fixture.slot().await, "free");
        until("stop command completed", async || {
            fixture.command_done(stop).await
        })
        .await;
        until("journal closed", async || {
            fixture
                .sj()
                .2
                .iter()
                .any(|b| matches!(b, Body::JournalClosed(_)))
        })
        .await;
        fixture.assert_cursor(true).await;
        assert_eq!(transmissions(&fixture.sj().2, input.command_id), [1]);
        let open: i64 = sqlx::query_scalar(
            "select count(*) from blocks where execution_id = $1 and state = 'streaming'",
        )
        .bind(fixture.execution)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(open, 0);
        fixture.assert_contiguous().await;
        fixture.finish().await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn large_answer_backlog_does_not_starve_control_session(pool: PgPool) {
    let _gate = e2e_gate::acquire_exclusive().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    // Пауза 80 мс на пакет до 256 записей плюс ~4,5 мс на запись держат приём хаба около
    // 220 записей в секунду: 4000 дельт локально опережают курсор хаба больше чем на 1024.
    const DELTAS: usize = 4000;
    fixture
        .faults()
        .set_sj_batch_pause(Duration::from_millis(80));
    let steps: Vec<_> = (0..DELTAS)
        .map(|_| json!({"delta":"x"}))
        .chain([json!({"hang":true})])
        .collect();
    let input = fixture.submit(json!({"synthetic":steps}).to_string()).await;
    until("deltas durable locally", async || {
        fixture
            .sj()
            .2
            .iter()
            .filter(|b| matches!(b, Body::BlockDelta(_)))
            .count()
            == DELTAS
    })
    .await;
    let (journal, length, _) = fixture.sj();
    let started = Instant::now();
    let (interrupt, _) = fixture.interrupt().await;
    // Курсор хаба только растёт: прочитанный после отправки прерывания он ограничивает
    // отставание в момент отправки снизу.
    let committed: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        length > committed as u64 + 1024,
        "the interrupt must be sent behind an SJ backlog of more than 1024 records: local {length}, hub {committed}"
    );
    until("interrupt stored locally", async || {
        fixture.store_probe.snapshots().iter().any(|s| {
            s.cj.iter().any(|r| {
                r.stored_command
                    .as_ref()
                    .is_some_and(|(id, _)| *id == interrupt.to_string())
            })
        })
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    fixture.faults().set_sj_batch_pause(Duration::ZERO);
    timeout(Duration::from_secs(180), async {
        while !fixture.command_done(interrupt).await {
            assert_eq!(fixture.faults().hellos().len(), 1);
            assert!(
                fixture
                    .session_probe
                    .heartbeat_acks()
                    .iter()
                    .all(|elapsed| *elapsed < fixture.config.timers.heartbeat_ack)
            );
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("interrupt completion behind SJ backlog");
    let acks = fixture.session_probe.heartbeat_acks();
    assert!(acks.len() >= 3);
    assert!(
        acks.iter()
            .all(|elapsed| *elapsed < fixture.config.timers.heartbeat_ack)
    );
    assert_eq!(fixture.faults().hellos().len(), 1);
    fixture.faults().set_sj_batch_pause(Duration::ZERO);
    fixture.assert_cursor(false).await;
    assert_eq!(fixture.event_count("block_delta").await, DELTAS as i64);
    assert_eq!(fixture.slot().await, "free");
    assert_eq!(fixture.input_state(input.input_id).await, "accepted");
    let block: (String, String) =
        sqlx::query_as("select state, content from blocks where execution_id = $1")
            .bind(fixture.execution)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(block.0, "incomplete");
    assert!(
        block.1 == "x".repeat(DELTAS),
        "backlog block content mismatch"
    );
    assert_eq!(transmissions(&fixture.sj().2, input.command_id), [1]);
    fixture.assert_never_left_running().await;
    fixture.assert_same_processes(identities);
    fixture.assert_contiguous().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn lost_sj_acks_do_not_duplicate_blocks(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let ack_start = fixture.faults().acks().len();
    fixture.faults().drop_next_sj_acks(usize::MAX);
    let (script, expected) = answer(60, 30);
    let input = fixture.submit(script).await;
    fixture.wait_deltas(3).await;
    fixture.faults().fail_data(Code::Cancelled);
    fixture.wait_deltas(15).await;
    fixture.faults().fail_control(Code::Unavailable);
    fixture.assert_answer(&input, &expected, 60).await;
    let journal = fixture.sj().0.to_string();
    until("SJ replay observed", async || {
        let mut seen = BTreeSet::new();
        fixture
            .faults()
            .batches()
            .iter()
            .filter(|batch| batch.journal_id == journal)
            .flat_map(|batch| &batch.records)
            .any(|record| !seen.insert(record.seq))
    })
    .await;
    fixture.assert_answer(&input, &expected, 60).await;
    let acks: Vec<_> = fixture
        .faults()
        .acks()
        .into_iter()
        .skip(ack_start)
        .filter(|a| a.range.journal_id == journal)
        .collect();
    assert!(acks.len() > 3 && acks.iter().all(|ack| ack.dropped));
    fixture.faults().drop_next_sj_acks(0);
    fixture.faults().fail_data(Code::Cancelled);
    fixture.assert_same_processes(identities);
    fixture.assert_never_left_running().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn withdrawn_input_is_redispatched_with_new_attempt_and_transmitted_once(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(
        &pool,
        "synthetic",
        &[("CANTERLOT_FAULT_HOLD_AT", "before_gate")],
    )
    .await;
    let identities = fixture.identities();
    let (script, expected) = answer(4, 30);
    let input = fixture.submit(script).await;
    until("input on machine", async || {
        fixture.input_state(input.input_id).await == "on_machine"
    })
    .await;
    assert!(transmissions(&fixture.sj().2, input.command_id).is_empty());
    let (interrupt, withdraw) = fixture.interrupt().await;
    let withdraw = withdraw.expect("withdraw command required");
    until("withdraw completed", async || {
        fixture.command_done(withdraw).await
            && fixture.command_done(interrupt).await
            && fixture.input_state(input.input_id).await == "queued"
    })
    .await;
    let bodies = fixture.sj().2;
    assert!(bodies.iter().any(|b| matches!(b, Body::InputWithdrawn(f) if f.command_id == input.command_id.to_string() && f.dispatch_attempt == 1)));
    assert!(bodies.iter().any(|b| matches!(b, Body::InterruptStatus(f) if f.command_id == interrupt.to_string() && f.state == InterruptState::NotApplicable as i32)));
    assert!(transmissions(&bodies, input.command_id).is_empty());
    assert!(fixture.cli_receipts(input.command_id).is_empty());
    assert_eq!(fixture.slot().await, "free");
    fs::write(fixture.exec_dir().join("fault_release"), []).unwrap();
    control::continue_queue(
        &pool,
        fixture.owner,
        fixture.conversation,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    fixture.hub().dispatcher.trigger(fixture.conversation);
    fixture.assert_answer(&input, &expected, 4).await;
    assert_eq!(fixture.attempt(input.command_id).await, 2);
    assert_eq!(transmissions(&fixture.sj().2, input.command_id), [2]);
    let stored: BTreeSet<_> = fixture
        .faults()
        .batches()
        .iter()
        .flat_map(|b| &b.records)
        .filter_map(|r| r.stored_command.as_ref())
        .filter(|(id, _)| *id == input.command_id.to_string())
        .map(|(_, attempt)| *attempt)
        .collect();
    assert_eq!(stored, BTreeSet::from([1, 2]));
    fixture.assert_same_processes(identities);
    fixture.assert_never_left_running().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn transport_errors_never_stop_an_execution(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let (script, expected) = answer(80, 80);
    let input = fixture.submit(script).await;
    fixture.wait_deltas(3).await;
    for (index, code) in [Code::Cancelled, Code::DeadlineExceeded, Code::Unavailable]
        .into_iter()
        .enumerate()
    {
        let hellos = fixture.faults().hellos().len();
        fixture.faults().fail_control(code);
        until("control reconnected after transport error", async || {
            fixture.faults().hellos().len() > hellos
        })
        .await;
        fixture.wait_deltas((index as i64 + 1) * 10).await;
        fixture.assert_never_left_running().await;
        fixture.assert_same_processes(identities);
    }
    let hellos = fixture.faults().hellos().len();
    fixture.faults().goaway();
    until("GOAWAY issued", async || {
        fixture.faults().goaway_count() == 1
    })
    .await;
    fixture.wait_deltas(40).await;
    assert_eq!(fixture.faults().hellos().len(), hellos);
    fixture.assert_never_left_running().await;
    fixture.assert_same_processes(identities);
    fixture.restart_hub().await;
    fixture.assert_answer(&input, &expected, 80).await;
    fixture.assert_never_left_running().await;
    fixture.assert_same_processes(identities);
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn killed_supervisor_is_never_respawned_and_exits_when_the_group_empties(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let input = fixture
        .submit(json!({"synthetic":[{"delta":"x"},{"hang":true}]}).to_string())
        .await;
    fixture.wait_deltas(1).await;
    fixture.assert_cursor(false).await;
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    fixture.wait_state("failed").await;
    assert!(fixture.paused("owner_lost").await);
    assert!(!same_process(identities.0));
    for _ in 0..15 {
        assert!(
            !fixture.guard.members().iter().any(
                |member| process_observation(*member).is_some_and(|(_, supervisor)| supervisor)
            )
        );
        assert!(fixture.guard.members().iter().all(|p| *p == identities.1));
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(fixture.history.lock().unwrap().len(), 2);
    assert_eq!(fixture.input_state(input.input_id).await, "accepted");
    assert_eq!(fixture.slot().await, "free");
    fixture.assert_contiguous().await;
    fixture.finish().await;
}

impl Fixture {
    async fn outcome(&self) -> Option<String> {
        sqlx::query_scalar("select outcome from executions where id = $1")
            .bind(self.execution)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn resume(&mut self) {
        let resumed = executions::start_with_key(
            &self.pool,
            self.owner,
            self.conversation,
            &Uuid::now_v7().to_string(),
            (true, true),
            Uuid::now_v7(),
            #[cfg(feature = "fault-injection")]
            &Default::default(),
        )
        .await
        .unwrap();
        self.execution = resumed.execution_id;
        self.wait_state("running").await;
        self.hub().dispatcher.trigger(self.conversation);
    }

    async fn assert_answers(&self) {
        let input = self
            .submit(json!({"synthetic":[{"delta":"after"}]}).to_string())
            .await;
        until("the resumed session accepts input", async || {
            self.input_state(input.input_id).await == "accepted"
        })
        .await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn killed_supervisor_with_nothing_left_fails_and_the_session_resumes(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .submit(json!({"synthetic":[{"delta":"x"},{"hang":true}]}).to_string())
        .await;
    fixture.wait_deltas(1).await;
    assert!(fixture.guard.signal(identities.1, Signal::KILL));
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    fixture.wait_state("failed").await;
    assert_eq!(fixture.outcome().await.as_deref(), Some("supervisor_died"));
    assert!(fixture.paused("fatal_error").await);
    assert_eq!(fixture.event_count("execution_fact_rejected").await, 0);
    let open: i64 = sqlx::query_scalar(
        "select count(*) from blocks where execution_id = $1 and state = 'streaming'",
    )
    .bind(fixture.execution)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(open, 0, "nothing keeps streaming after the exit");
    until("execution directory removed", async || {
        !fixture.exec_dir().exists()
    })
    .await;
    let dead = fixture.exec_dir();
    fixture.resume().await;
    assert_ne!(dead, fixture.exec_dir());
    assert!(!fixture.paused("owner_lost").await);
    assert!(!fixture.paused("fatal_error").await);
    fixture.assert_answers().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn orphan_process_waits_for_the_owner_and_stop_leads_to_the_resolve(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .submit(
            json!({"synthetic":[{"detach_child":true},{"delta":"x"},{"hang":true}]}).to_string(),
        )
        .await;
    fixture.wait_deltas(1).await;
    let orphan = fixture
        .guard
        .members()
        .into_iter()
        .find(|(pid, _)| {
            fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|line| line.windows(14).any(|part| part == b"__detach_sleep"))
        })
        .expect("detached descendant");
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    fixture.wait_state("owner_lost").await;
    sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        fixture.state().await,
        "owner_lost",
        "the group is not empty"
    );
    assert!(same_process(orphan), "nobody kills it uninvited");
    assert_eq!(
        control::request_stop_execution(
            &fixture.pool,
            fixture.owner,
            fixture.conversation,
            true,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        StopExecutionOutcome::OwnerLost
    );
    control::request_resolve_owner_lost(
        &fixture.pool,
        fixture.owner,
        fixture.conversation,
        &Uuid::now_v7().to_string(),
    )
    .await
    .unwrap();
    fixture.wait_state("failed").await;
    assert_eq!(fixture.outcome().await.as_deref(), Some("supervisor_died"));
    assert!(!same_process(orphan), "the rest of the group is gone");
    fixture.resume().await;
    fixture.assert_answers().await;
    fixture.finish().await;
}

const SPEC_B644882: &str = include_str!("fixtures/spec/b644882ac458.toml");

impl Fixture {
    /// Повторяет сбой обновления: новый воркер не читает спеку и докладывает `owner_lost`.
    async fn lose_owner_to_a_bad_spec(&mut self) -> String {
        let spec = self.exec_dir().join("spec.toml");
        let text = fs::read_to_string(&spec).unwrap();
        self.stop_worker().await;
        fs::write(&spec, "[broken").unwrap();
        self.start_worker().await;
        self.wait_state("owner_lost").await;
        assert!(self.paused("owner_lost").await);
        text
    }

    async fn request_state(&self) -> Option<String> {
        sqlx::query_scalar("select state from requests where execution_id = $1")
            .bind(self.execution)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
    }

    /// Исправленный воркер: спека осталась от старого релиза, сопровождение то же.
    async fn restart_fixed_worker(&mut self, spec: &str) {
        self.stop_worker().await;
        let mut table: toml::Table = toml::from_str(spec).unwrap();
        let frozen: toml::Table = toml::from_str(SPEC_B644882).unwrap();
        for key in ["claude_code", "codex", "opencode"] {
            table.insert(key.into(), frozen[key].clone());
        }
        fs::write(
            self.exec_dir().join("spec.toml"),
            toml::to_string(&table).unwrap(),
        )
        .unwrap();
        self.start_worker().await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn lost_owner_returns_with_buffered_journal_and_queued_input(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let mut steps = vec![json!({"delta":"a"}), json!({"delay_ms":1500})];
    let mut expected = String::from("a");
    for index in 0..60 {
        let delta = format!("b{index:02}|");
        expected.push_str(&delta);
        steps.push(json!({"delta":delta}));
        steps.push(json!({"delay_ms":20}));
    }
    let first = fixture.submit(json!({"synthetic":steps}).to_string()).await;
    fixture.wait_deltas(1).await;
    let spec = fixture.lose_owner_to_a_bad_spec().await;
    let queued = fixture
        .submit(json!({"synthetic":[{"delta":"queued"},{"hang":true}]}).to_string())
        .await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(fixture.input_state(queued.input_id).await, "queued");
    until("output buffered while lost", async || {
        fixture.sj_snapshot().is_some_and(|(_dir, journal)| {
            journal
                .read_from(journal.first_seq(), usize::MAX)
                .unwrap()
                .iter()
                .any(|record| {
                    RecordBody::decode(record.payload.as_slice())
                        .ok()
                        .and_then(|body| body.body)
                        .is_some_and(|body| matches!(body, Body::RunFinished(_)))
                })
        })
    })
    .await;
    let before = fixture.event_count("block_delta").await;

    fixture.restart_fixed_worker(&spec).await;
    assert_eq!(fixture.state().await, "running");
    assert!(!fixture.paused("owner_lost").await);
    fixture.assert_same_processes(identities);
    assert_eq!(fixture.input_state(first.input_id).await, "accepted");
    until("buffered journal shipped", async || {
        sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from blocks where execution_id = $1 and state = 'final' and content = $2)",
        )
        .bind(fixture.execution)
        .bind(&expected)
        .fetch_one(&fixture.pool)
        .await
        .unwrap()
    })
    .await;
    assert!(fixture.event_count("block_delta").await > before);
    until("queued input delivered", async || {
        fixture.input_state(queued.input_id).await == "accepted"
    })
    .await;
    assert_eq!(fixture.event_count("execution_fact_rejected").await, 0);
    fixture.assert_contiguous().await;
    fixture.assert_same_processes(identities);
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn request_waiting_through_the_loss_is_answered_after_the_return(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let steps = json!([
        {"request_permission":{"tool_name":"Bash","command":"cargo test","args":{}}},
        {"delta":"resumed"},
    ]);
    fixture.submit(json!({"synthetic":steps}).to_string()).await;
    until("request opened", async || {
        fixture.request_state().await.as_deref() == Some("open")
    })
    .await;
    let spec = fixture.lose_owner_to_a_bad_spec().await;
    assert_eq!(
        fixture.request_state().await.as_deref(),
        Some("closed_without_decision")
    );
    fixture.restart_fixed_worker(&spec).await;
    assert_eq!(fixture.state().await, "running");
    until("request reopened", async || {
        fixture.request_state().await.as_deref() == Some("open")
    })
    .await;
    let (request, generation): (Uuid, i64) =
        sqlx::query_as("select id, generation from requests where execution_id = $1")
            .bind(fixture.execution)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let mut tx = fixture.pool.begin().await.unwrap();
    canterlot_hub::requests::save_decision_in_tx(
        &mut tx,
        fixture.conversation,
        request,
        generation,
        &canterlot_proto::requests::DecisionPayload::Permission {
            behavior: canterlot_proto::requests::PermissionBehavior::Allow,
            message: None,
        },
        &Uuid::now_v7().to_string(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    until("decision applied", async || {
        fixture.request_state().await.as_deref() == Some("decision_applied")
    })
    .await;
    fixture.assert_same_processes(identities);
    fixture.assert_contiguous().await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_d1a_dead_supervisor_then_control_restart(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .submit(json!({"synthetic":[{"delta":"x"},{"hang":true}]}).to_string())
        .await;
    fixture.wait_deltas(1).await;
    fixture.assert_cursor(false).await;
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    until("lost", async || fixture.paused("owner_lost").await).await;
    fixture.stop_worker().await;
    fixture.start_worker().await;
    fixture.wait_state("failed").await;
    assert_eq!(fixture.outcome().await.as_deref(), Some("supervisor_died"));
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_d1b_supervisor_dies_while_control_down(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .submit(json!({"synthetic":[{"delta":"x"},{"hang":true}]}).to_string())
        .await;
    fixture.wait_deltas(1).await;
    fixture.assert_cursor(false).await;
    fixture.stop_worker().await;
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    until("supervisor died", async || !same_process(identities.0)).await;
    fixture.start_worker().await;
    fixture.wait_state("failed").await;
    assert!(fixture.paused("owner_lost").await);
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_d3_exit_during_outage_reattaches_draining_supervisor(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let mut steps = vec![json!({"delta":"a"}), json!({"delay_ms":1500})];
    for index in 0..40 {
        steps.push(json!({"delta":format!("b{index:02}| ")}));
        steps.push(json!({"delay_ms":20}));
    }
    steps.push(json!({"exit":0}));
    fixture.submit(json!({"synthetic":steps}).to_string()).await;
    fixture.wait_deltas(1).await;
    let hub = fixture.hub.take().unwrap();
    let addr = hub.addr;
    let pki = hub.pki.clone();
    let faults = hub.faults.clone();
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
    fixture.stop_worker().await;
    until("local journal closed", async || {
        fixture
            .sj()
            .2
            .iter()
            .any(|b| matches!(b, Body::JournalClosed(_)))
    })
    .await;
    sleep(Duration::from_secs(2)).await;
    assert!(
        same_process(identities.0),
        "draining supervisor exited without ACK"
    );
    assert!(!same_process(identities.1));
    fixture.hub =
        Some(spawn_test_hub_with_faults(pool.clone(), hub_timers(), Some(addr), pki, faults).await);
    fixture.start_worker().await;
    fixture.wait_state("stopped").await;
    until("closed ingested", async || {
        fixture.event_count("journal_closed").await == 1
    })
    .await;
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_d5_dead_sj_intent_is_never_deferred_by_cj(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new(
        &pool,
        "synthetic",
        &[("CANTERLOT_FAULT_ABORT_AT", "before_first_byte")],
    )
    .await;
    fixture.faults().drop_next_acks(100);
    let input = fixture.submit("hello".into()).await;
    fixture.wait_state("owner_lost").await;
    assert_eq!(transmissions(&fixture.sj().2, input.command_id), [1]);
    // Намерение уходит хабу до первого байта, а сопровождение падает сразу за ним: дошло ли оно, гонка.
    let unresolved = ["on_machine", "transmitting"];
    let state = fixture.input_state(input.input_id).await;
    assert!(unresolved.contains(&state.as_str()), "{state}");
    fixture.stop_worker().await;
    fixture.faults().drop_next_acks(0);
    fixture.start_worker().await;
    fixture.wait_state("failed").await;
    until("the undetermined input", async || {
        fixture.input_state(input.input_id).await == "acceptance_unconfirmed"
    })
    .await;
    fixture.stop_worker().await;
    let store = WorkerStore::open(&fixture.config.data_dir).unwrap();
    assert!(!store.cj_records_from(store.cj_cursor().unwrap().first_stored_seq, usize::MAX).unwrap().iter().any(|r|
        matches!(r.body.as_ref().and_then(|b| b.body.as_ref()), Some(Body::DispatchDeferred(f)) if f.command_id == input.command_id.to_string())));
    drop(store);
    fixture.start_worker().await;
    assert_eq!(
        transmissions(&fixture.sj().2, input.command_id),
        [1],
        "a started transmission is never repeated"
    );
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_d6_stop_behind_slow_sj_backlog(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .faults()
        .set_sj_batch_pause(Duration::from_millis(200));
    let mut steps: Vec<Value> = (0..3000).map(|_| json!({"delta":"x"})).collect();
    steps.push(json!({"hang":true}));
    fixture.submit(json!({"synthetic":steps}).to_string()).await;
    until("3000 local deltas", async || {
        fixture
            .sj()
            .2
            .iter()
            .filter(|b| matches!(b, Body::BlockDelta(_)))
            .count()
            == 3000
    })
    .await;
    let stop = fixture.stop_execution(true).await;
    until("local journal closed", async || {
        fixture
            .sj()
            .2
            .iter()
            .any(|b| matches!(b, Body::JournalClosed(_)))
    })
    .await;
    sleep(Duration::from_secs(2)).await;
    assert!(
        same_process(identities.0),
        "draining supervisor exited behind backlog"
    );
    fixture.faults().set_sj_batch_pause(Duration::ZERO);
    fixture.wait_state("stopped").await;
    until("stop completed", async || fixture.command_done(stop).await).await;
    until("closed ingested", async || {
        fixture.event_count("journal_closed").await == 1
    })
    .await;
    fixture.finish().await;
}

#[test]
fn wave2_n3_runtime_wipe_and_replacement() {
    let _gate = e2e_gate::acquire_blocking();
    for case in ["canterlot", "runtime", "replacement"] {
        let runtime = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wave2_n3_runtime_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CANTERLOT_WAVE2_RUNTIME_CASE", case)
            .env("XDG_RUNTIME_DIR", runtime.path().join("runtime"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
#[ignore]
async fn wave2_n3_runtime_child(pool: PgPool) {
    use canterlot_proto::worker::ipc::{self, control_to_supervisor::Msg};
    use canterlot_worker::supervisor::ipc::{read_message, write_message};
    use std::os::unix::fs::MetadataExt;
    let Ok(case) = std::env::var("CANTERLOT_WAVE2_RUNTIME_CASE") else {
        return;
    };
    let mut fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let socket = runtime
        .join("canterlot")
        .join(format!("{}.sock", fixture.execution));
    let old = fs::metadata(&socket).unwrap();
    fixture.stop_worker().await;
    if case == "replacement" {
        fs::remove_file(&socket).unwrap();
        let replacement = tokio::net::UnixListener::bind(&socket).unwrap();
        assert_ne!(fs::metadata(&socket).unwrap().ino(), old.ino());
        drop(replacement);
    } else {
        fs::remove_dir_all(if case == "runtime" {
            runtime.clone()
        } else {
            runtime.join("canterlot")
        })
        .unwrap();
    }
    fixture.config.execution.supervisor.attach_wait = Duration::from_secs(8);
    fixture.start_worker().await;
    assert_eq!(
        fixture.state().await,
        "running",
        "live supervisor was declared owner_lost"
    );
    fixture.assert_same_processes(identities);
    assert_eq!(
        fs::metadata(runtime.join("canterlot"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let input = fixture
        .submit(json!({"synthetic":[{"delta":"tail"},{"hang":true}]}).to_string())
        .await;
    until("tail delivered", async || {
        fixture.command_done(input.command_id).await
    })
    .await;
    fixture.wait_deltas(1).await;
    fixture.stop_worker().await;
    let mut connection = tokio::net::UnixStream::connect(&socket).await.unwrap();
    write_message(
        &mut connection,
        &ipc::ControlToSupervisor {
            msg: Some(Msg::Attach(ipc::Attach {
                protocol_version: 1,
                secret: fs::read(fixture.exec_dir().join("secret")).unwrap(),
                execution_id: fixture.execution.to_string(),
                generation: 1,
            })),
        },
    )
    .await
    .unwrap();
    let reply: ipc::SupervisorToControl = read_message(&mut connection).await.unwrap();
    assert!(matches!(
        reply.msg,
        Some(ipc::supervisor_to_control::Msg::Attached(_))
    ));
    write_message(
        &mut connection,
        &ipc::ControlToSupervisor {
            msg: Some(Msg::Stop(ipc::StopRequest {
                force: true,
                ..Default::default()
            })),
        },
    )
    .await
    .unwrap();
    until("draining", async || {
        fixture
            .sj()
            .2
            .iter()
            .any(|b| matches!(b, Body::JournalClosed(_)))
    })
    .await;
    drop(connection);
    assert!(same_process(identities.0));
    fs::remove_dir_all(&runtime).unwrap();
    fixture.start_worker().await;
    fixture.wait_state("stopped").await;
    until("tail shipped", async || {
        fixture.event_count("journal_closed").await == 1
    })
    .await;
    assert_eq!(
        blocks::read_text(
            &mut pool.acquire().await.unwrap(),
            sqlx::query_scalar::<_, Uuid>("select block_id from blocks where execution_id = $1")
                .bind(fixture.execution)
                .fetch_one(&pool)
                .await
                .unwrap()
        )
        .await
        .unwrap(),
        "tail"
    );
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave2_n1_draining_delivery_releases_slot(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let paused = fixture.faults().pause_next(Point::BeforeSjCommit);
    let stop = fixture.stop_execution(true).await;
    let resume = timeout(WAIT, paused).await.unwrap().unwrap();
    until("draining", async || {
        fixture
            .sj()
            .2
            .iter()
            .any(|b| matches!(b, Body::JournalClosed(_)))
    })
    .await;
    // Хаб ещё видит running; снимаем только пользовательскую паузу для позднего dispatch.
    sqlx::query("update queue_state set pause_reasons = '[]' where conversation_id = $1")
        .bind(fixture.conversation)
        .execute(&pool)
        .await
        .unwrap();
    let input = fixture.submit("late".into()).await;
    timeout(
        Duration::from_secs(5),
        until("deferred", async || {
            sqlx::query_scalar::<_, bool>(
                "select last_status = 'dispatch_deferred' from commands where id = $1",
            )
            .bind(input.command_id)
            .fetch_one(&pool)
            .await
            .unwrap_or(false)
        }),
    )
    .await
    .expect("draining delivery has no receipt");
    assert_eq!(fixture.input_state(input.input_id).await, "queued");
    assert_eq!(fixture.slot().await, "free");
    assert!(fixture.cli_receipts(input.command_id).is_empty());
    let result: Value = sqlx::query_scalar("select result from commands where id = $1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["reason"], "generation_closed");
    resume.send(()).unwrap();
    until("stop completed", async || fixture.command_done(stop).await).await;
    fixture.finish().await;
}

#[test]
fn wave_w_d2_runtime_socket_paths() {
    let _gate = e2e_gate::acquire_blocking();
    for case in ["short", "long"] {
        let runtime = tempfile::tempdir().unwrap();
        let path = if case == "long" {
            runtime.path().join("r".repeat(100))
        } else {
            runtime.path().to_path_buf()
        };
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wave_w_runtime_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CANTERLOT_W_RUNTIME_CASE", case)
            .env("XDG_RUNTIME_DIR", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "runtime {case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
#[ignore]
async fn wave_w_runtime_child(pool: PgPool) {
    let Ok(case) = std::env::var("CANTERLOT_W_RUNTIME_CASE") else {
        return;
    };
    let mut fixture = Fixture::new_with(&pool, "synthetic", &[], true, false).await;
    let started = executions::start(&pool, fixture.owner, fixture.conversation, Uuid::now_v7())
        .await
        .unwrap();
    fixture.execution = started.execution_id;
    fixture.start_command = started.command_id;
    if case == "short" {
        fixture.wait_state("running").await;
        let socket_dir =
            PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).join("canterlot");
        assert_eq!(
            fs::metadata(&socket_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let socket = socket_dir.join(format!("{}.sock", fixture.execution));
        assert!(socket.exists());
        assert!(!fixture.exec_dir().join("ipc.sock").exists());
        fixture.finish().await;
        assert!(!socket.exists());
    } else {
        fixture.wait_state("failed").await;
        let result: (String, Value) =
            sqlx::query_as("select delivery_state, result from commands where id = $1")
                .bind(started.command_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            result,
            ("completed".into(), json!({"rejected":"ipc_path_too_long"}))
        );
        let outcome: Option<String> =
            sqlx::query_scalar("select outcome from executions where id = $1")
                .bind(started.execution_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(outcome.as_deref(), Some("start_rejected"));
        assert!(fixture.guard.members().is_empty());
        assert!(matches!(
            control::request_stop_execution(
                &pool,
                fixture.owner,
                fixture.conversation,
                false,
                &Uuid::now_v7().to_string(),
                Uuid::now_v7()
            )
            .await
            .unwrap(),
            StopExecutionOutcome::NothingToStop
        ));
        let next = executions::start(&pool, fixture.owner, fixture.conversation, Uuid::now_v7())
            .await
            .unwrap();
        fixture.execution = next.execution_id;
        fixture.wait_state("failed").await;
        fixture.stop_worker().await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_data_reattach_discards_stale_sj_batches(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let paused = fixture.faults().pause_next(Point::BeforeSjCommit);
    let (script, expected) = answer(1200, 0);
    let input = fixture.submit(script).await;
    let resume = timeout(WAIT, paused).await.unwrap().unwrap();
    until("local backlog", async || {
        fixture
            .sj()
            .2
            .iter()
            .filter(|b| matches!(b, Body::BlockDelta(_)))
            .count()
            == 1200
    })
    .await;
    let hellos = fixture.session_probe.data_attaches();
    fixture.faults().fail_data(Code::Cancelled);
    resume.send(()).unwrap();
    fixture.assert_answer(&input, &expected, 1200).await;
    assert_eq!(
        fixture.session_probe.data_attaches(),
        hellos + 1,
        "stale batches forced another handshake"
    );
    fixture.finish().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_pre_attach_timeout_kills_own_child_and_rejects_start(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new_with(
        &pool,
        "synthetic",
        &[("CANTERLOT_FAULT_HOLD_AT", "before_attach")],
        false,
        false,
    )
    .await;
    let started = executions::start(&pool, fixture.owner, fixture.conversation, Uuid::now_v7())
        .await
        .unwrap();
    fixture.execution = started.execution_id;
    fixture.start_command = started.command_id;
    until("own supervisor spawned", async || {
        !fixture.guard.members().is_empty()
    })
    .await;
    let members = fixture.guard.members();
    assert_eq!(members.len(), 1);
    assert_eq!(process_observation(members[0]), Some((members[0], true)));
    fixture.wait_state("failed").await;
    let result: Value = sqlx::query_scalar("select result from commands where id = $1")
        .bind(started.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result, json!({"rejected":"supervisor_start_failed"}));
    assert!(!same_process(members[0]));
    assert!(fixture.guard.members().is_empty());
    assert!(matches!(
        control::request_stop_execution(
            &pool,
            fixture.owner,
            fixture.conversation,
            false,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7()
        )
        .await
        .unwrap(),
        StopExecutionOutcome::NothingToStop
    ));
    fixture.stop_worker().await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wave_w_spawning_ownership_reports_owner_lost_from_created(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let mut fixture = Fixture::new_with(&pool, "synthetic", &[], false, false).await;
    fixture.stop_worker().await;
    let started = executions::start(&pool, fixture.owner, fixture.conversation, Uuid::now_v7())
        .await
        .unwrap();
    fixture.execution = started.execution_id;
    let mut store = WorkerStore::open(&fixture.config.data_dir).unwrap();
    store
        .ownership_mut()
        .unwrap()
        .append(canterlot_proto::worker::ipc::Ownership {
            execution_id: started.execution_id.to_string(),
            conversation_id: fixture.conversation.to_string(),
            generation: 1,
            phase: canterlot_proto::worker::ipc::OwnershipPhase::Spawning as i32,
            ..Default::default()
        })
        .unwrap();
    drop(store);
    fixture.start_worker().await;
    fixture.wait_state("owner_lost").await;
    assert!(fixture.guard.members().is_empty());
    fixture.finish().await;
}

// Весь путь до PostgreSQL: разрыв потока и перезапуск воркера посреди хода OpenCode. Хаб хранит
// каждый блок один раз, текст целиком, без расхождений ревизий, а ввод ушёл один раз.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn opencode_stream_cut_and_worker_restart_reach_the_hub_once(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let script = Script::from_capture("recovery", Some("cut_midturn"))
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    let fake = opencode::Fake::new(script);
    let mut fixture = Fixture::new_full(
        &pool,
        "opencode",
        &[],
        false,
        false,
        Some(OpenCodeConfig {
            executable: fake.exe.clone(),
            owner_config: Some(fake.dir.path().join("owner-config")),
            providers: vec!["fake".into()],
            ..Default::default()
        }),
    )
    .await;
    sqlx::query("update conversations set model = 'fake/fake-1', effort = 'high' where id = $1")
        .bind(fixture.conversation)
        .execute(&pool)
        .await
        .unwrap();
    let started = executions::start(&pool, fixture.owner, fixture.conversation, Uuid::now_v7())
        .await
        .unwrap();
    fixture.execution = started.execution_id;
    fixture.start_command = started.command_id;
    fixture.wait_state("running").await;
    fixture.submit("начни".into()).await;
    until("first block", async || {
        fixture.event_count("block_started").await >= 1
    })
    .await;
    fixture.stop_worker().await;
    fixture.start_worker().await;
    let texts = async || -> Vec<(String, String)> {
        sqlx::query_as(
            "select state, coalesce(content, '') from blocks where execution_id = $1
             order by coalesce(content, '')",
        )
        .bind(fixture.execution)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    until("every block final", async || {
        let blocks = texts().await;
        blocks.len() == 5 && blocks.iter().all(|(state, _)| state == "final")
    })
    .await;
    let contents: Vec<String> = texts().await.into_iter().map(|(_, text)| text).collect();
    assert_eq!(
        contents,
        [
            "echo из разрыва",
            "думаю ",
            "из разрыва\n",
            "первый кусок",
            "после разрыва"
        ]
    );
    let bad: i64 = sqlx::query_scalar(
        "select count(*) from events where kind in
         ('block_revision_gap','block_conflicting_final','journal_record_mismatch')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bad, 0);
    let prompts = fake
        .requests()
        .iter()
        .filter(|request| {
            request["method"] == "POST" && opencode::path_of(request).ends_with("/prompt")
        })
        .count();
    assert_eq!(prompts, 1, "no prompt is sent twice");
    fixture.finish().await;
}

// Поток данных общий для машины: запись одного разговора не должна держать другой.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn nul_in_one_session_leaves_the_machine_stream_running(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let other = testkit::seed_conversation(&pool, fixture.owner, fixture.config.machine_id).await;
    sqlx::query("update conversations set workdir = $2 where id = $1")
        .bind(other)
        .bind(fixture.root.path().join("work").to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let second = executions::start(&pool, fixture.owner, other, Uuid::now_v7())
        .await
        .unwrap();
    until("second execution running", async || {
        sqlx::query_scalar::<_, String>("select state from executions where id = $1")
            .bind(second.execution_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "running"
    })
    .await;
    let (script, expected) = answer(3, 0);
    fixture
        .submit(json!({"synthetic":[{"delta":"a\u{0}b"}]}).to_string())
        .await;
    submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: fixture.owner,
            conversation_id: other,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text { text: script }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    fixture.hub().dispatcher.trigger(other);
    let content = async |execution: Uuid| -> Option<String> {
        sqlx::query_scalar("select content from blocks where execution_id = $1 and state = 'final'")
            .bind(execution)
            .fetch_optional(&pool)
            .await
            .unwrap()
    };
    until("both answers final", async || {
        content(fixture.execution).await.is_some() && content(second.execution_id).await.is_some()
    })
    .await;
    assert_eq!(
        content(fixture.execution).await.as_deref(),
        Some("a\u{fffd}b")
    );
    assert!(content(second.execution_id).await.as_deref() == Some(expected.as_str()));
    let mismatches: i64 = sqlx::query_scalar(
        "select count(*) from events where execution_id = $1 and kind = 'block_checksum_mismatch'",
    )
    .bind(fixture.execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mismatches, 0);
    control::request_stop_execution(
        &pool,
        fixture.owner,
        other,
        true,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    until("second execution stopped", async || {
        sqlx::query_scalar::<_, String>("select state from executions where id = $1")
            .bind(second.execution_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "stopped"
    })
    .await;
    fixture.finish().await;
}

// Запись, которую база не примет и при повторе, останавливает своё исполнение, а не машину.
#[test]
fn refused_records_are_the_error_strings_of_the_hub() {
    use canterlot_hub::ingest::IngestError;
    let hub = [
        IngestError::UnknownRecord,
        IngestError::InvalidRecord,
        IngestError::RejectedValue,
        IngestError::ForeignJournal,
        IngestError::ForeignExecution,
        IngestError::JournalKindMismatch,
        IngestError::Execution,
    ]
    .map(|error| error.to_string());
    assert_eq!(hub, canterlot_worker::session::REFUSED_RECORDS);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn refused_record_stops_its_execution_and_leaves_the_machine_stream_running(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let other = testkit::seed_conversation(&pool, fixture.owner, fixture.config.machine_id).await;
    sqlx::query("update conversations set workdir = $2 where id = $1")
        .bind(other)
        .bind(fixture.root.path().join("work").to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let second = executions::start(&pool, fixture.owner, other, Uuid::now_v7())
        .await
        .unwrap();
    let state = async |execution: Uuid| -> String {
        sqlx::query_scalar("select state from executions where id = $1")
            .bind(execution)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    until("second execution running", async || {
        state(second.execution_id).await == "running"
    })
    .await;
    sqlx::raw_sql(
        "create function refuse_chunk() returns trigger language plpgsql as $$
         begin
           if new.text like '%неприемлемо%' then
             raise exception 'value' using errcode = '22023';
           end if;
           return new;
         end $$;
         create trigger refuse_chunk before insert on block_chunks
         for each row execute function refuse_chunk();",
    )
    .execute(&pool)
    .await
    .unwrap();
    fixture
        .submit(json!({"synthetic":[{"delta":"неприемлемо"}]}).to_string())
        .await;
    let (script, expected) = answer(3, 0);
    submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: fixture.owner,
            conversation_id: other,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text { text: script }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    fixture.hub().dispatcher.trigger(other);
    until("refused execution stopped", async || {
        state(fixture.execution).await == "stopped"
    })
    .await;
    assert!(fixture.paused("fatal_error").await);
    until("journal of the refused execution closed", async || {
        sqlx::query_scalar::<_, bool>("select closed from journal_cursors where execution_id = $1")
            .bind(fixture.execution)
            .fetch_one(&pool)
            .await
            .unwrap()
    })
    .await;
    let content = async || -> Option<String> {
        sqlx::query_scalar("select content from blocks where execution_id = $1 and state = 'final'")
            .bind(second.execution_id)
            .fetch_optional(&pool)
            .await
            .unwrap()
    };
    until("answer of the other session final", async || {
        content().await.is_some()
    })
    .await;
    assert!(content().await.as_deref() == Some(expected.as_str()));
    let shown: i64 = sqlx::query_scalar(
        "select count(*) from events where execution_id = $1 and kind = 'adapter_diagnostic'
         and payload->>'kind' = 'journal_record_rejected'",
    )
    .bind(fixture.execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(shown, 1);
    control::request_stop_execution(
        &pool,
        fixture.owner,
        other,
        true,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    until("second execution stopped", async || {
        state(second.execution_id).await == "stopped"
    })
    .await;
    fixture.finish().await;
}
