#![forbid(unsafe_code)]
#![cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
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
    config::{ExecutionConfig, SupervisorTimers, WorkerConfig, WorkerTimers},
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

fn is_supervisor(pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline"))
        .is_ok_and(|args| args.split(|b| *b == 0).any(|arg| arg == b"supervise"))
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
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let root = tempfile::tempdir().unwrap();
        let guard = Arc::new(ProcessGuard::new(root.path()));
        let history: ProcessHistory = Arc::default();
        let process_guard = guard.clone();
        let observed = history.clone();
        let watcher = tokio::spawn(async move {
            loop {
                for member in process_guard.members() {
                    observed
                        .lock()
                        .unwrap()
                        .insert((member, is_supervisor(member.0)));
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
        script.push_str(&format!(
            "exec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_canterlot-worker")
        ));
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let config = WorkerConfig {
            hub_url: format!("https://localhost:{}", hub.addr.port()),
            ca_cert,
            token_file,
            machine_id: machine.machine_id,
            installation_id: hub.installation.installation_id,
            restore_generation: hub.installation.restore_generation,
            data_dir: root.path().join("store"),
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
                supervisor: SupervisorTimers {
                    stop_grace: Duration::from_millis(500),
                    stop_retry: Duration::from_millis(300),
                    ..Default::default()
                },
                ..Default::default()
            },
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
        until("worker live", async || {
            *self.worker.as_ref().unwrap().state.borrow() == SessionState::Live
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
        let members = self.guard.members();
        let supervisors: Vec<_> = members
            .iter()
            .copied()
            .filter(|(pid, _)| is_supervisor(*pid))
            .collect();
        let cli: Vec<_> = members
            .iter()
            .copied()
            .filter(|(pid, _)| !is_supervisor(*pid))
            .collect();
        assert_eq!(supervisors.len(), 1);
        assert_eq!(cli.len(), 1);
        assert!(same_process(supervisors[0]) && same_process(cli[0]));
        self.history
            .lock()
            .unwrap()
            .extend([(supervisors[0], true), (cli[0], false)]);
        (supervisors[0], cli[0])
    }

    fn assert_same_processes(&self, identities: (Identity, Identity)) {
        assert_eq!(self.identities(), identities);
        let history = self.history.lock().unwrap();
        assert_eq!(
            history.iter().filter(|(_, supervisor)| *supervisor).count(),
            1
        );
        assert_eq!(
            history.iter().filter(|(_, supervisor)| !supervisor).count(),
            1
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
        assert_eq!(states, ["streaming"]);
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
            StopExecutionOutcome::NothingToStop => panic!("execution unexpectedly absent"),
        }
    }

    fn sj_snapshot(&self) -> (TempDir, Journal) {
        for _ in 0..100 {
            // Открытый inode переживает compaction; временные файлы ACK не входят в снимок.
            let sources: std::io::Result<Vec<_>> = fs::read_dir(self.exec_dir().join("sj"))
                .unwrap()
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
                return (copy, journal);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("could not obtain a complete SJ snapshot");
    }

    fn sj(&self) -> (Uuid, u64, Vec<Body>) {
        let (_copy, journal) = self.sj_snapshot();
        let mut next = journal.first_seq();
        let mut bodies = BTreeMap::new();
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
        // После ACK сегмент удалён; метаданные принятых записей объединяются по seq.
        for batch in self
            .faults()
            .batches()
            .iter()
            .filter(|b| b.journal_id == journal.id().0.to_string())
        {
            for record in &batch.records {
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
        (
            journal.id().0,
            journal.last_seq(),
            bodies.into_values().collect(),
        )
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
        if self.state().await != "owner_lost" {
            self.wait_state("stopped").await;
            until("journal closed", async || sqlx::query_scalar::<_, bool>("select coalesce(bool_or(closed), false) from journal_cursors where execution_id = $1")
                .bind(self.execution).fetch_one(&self.pool).await.unwrap()).await;
            self.assert_cursor(true).await;
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
        let (script, _) = answer(100, 80);
        let first = fixture.submit(script).await;
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
        assert!(bodies.iter().any(|b| matches!(b, Body::InterruptStatus(f) if f.command_id == interrupt.to_string() && f.state == InterruptState::Completed as i32)));
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
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    fixture
        .faults()
        .set_sj_batch_pause(Duration::from_millis(80));
    let steps: Vec<_> = (0..18000)
        .map(|_| json!({"delta":"x"}))
        .chain([json!({"hang":true})])
        .collect();
    let input = fixture.submit(json!({"synthetic":steps}).to_string()).await;
    until("18000 deltas durable locally", async || {
        fixture
            .sj()
            .2
            .iter()
            .filter(|b| matches!(b, Body::BlockDelta(_)))
            .count()
            == 18000
    })
    .await;
    let (journal, length, _) = fixture.sj();
    let committed: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        length > committed as u64 + 1024,
        "must exercise an SJ backlog"
    );
    let started = Instant::now();
    let (interrupt, _) = fixture.interrupt().await;
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
    assert_eq!(fixture.event_count("block_delta").await, 18000);
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
        block.1 == "x".repeat(18000),
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
async fn killed_supervisor_is_reported_owner_lost_and_never_respawned(pool: PgPool) {
    let fixture = Fixture::new(&pool, "synthetic", &[]).await;
    let identities = fixture.identities();
    let input = fixture
        .submit(json!({"synthetic":[{"delta":"x"},{"hang":true}]}).to_string())
        .await;
    fixture.wait_deltas(1).await;
    fixture.assert_cursor(false).await;
    assert!(fixture.guard.signal(identities.0, Signal::KILL));
    fixture.wait_state("owner_lost").await;
    assert!(fixture.paused("owner_lost").await);
    assert!(!same_process(identities.0));
    for _ in 0..15 {
        assert!(
            !fixture
                .guard
                .members()
                .iter()
                .any(|(pid, _)| is_supervisor(*pid))
        );
        assert!(fixture.guard.members().iter().all(|p| *p == identities.1));
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(fixture.history.lock().unwrap().len(), 2);
    assert_eq!(fixture.input_state(input.input_id).await, "accepted");
    assert_eq!(fixture.slot().await, "running");
    assert_eq!(fixture.cli_receipts(input.command_id), [1]);
    fixture.assert_contiguous().await;
    fixture.finish().await;
}
