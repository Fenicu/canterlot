#![cfg(feature = "fault-injection")]

use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    delivery::{
        dispatch::{self, DispatchOutcome, GateInputs},
        submit::{self, ContentBlock, SubmitInput},
    },
    executions,
    fault_injection::{BatchTrace, Faults, Point},
    machines,
    model::ExecutionState,
    testkit::{self, TestHub, spawn_test_hub, spawn_test_hub_with_faults},
};
use canterlot_proto::worker::v1::RejectReason;
use canterlot_worker::{
    config::{WorkerConfig, WorkerTimers},
    fault_injection::{StoreProbe, StoreSnapshot},
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tonic::Code;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(20);

#[allow(dead_code)]
mod support;
use support::data_proxy::DataProxy;

type Pause = oneshot::Receiver<oneshot::Sender<()>>;

fn hub_timers() -> HubTimers {
    HubTimers {
        dispatch_sweep: Duration::from_millis(25),
        ..Default::default()
    }
}

async fn until(mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition timed out");
}

async fn hit(pause: Pause) -> oneshot::Sender<()> {
    timeout(WAIT, pause).await.unwrap().unwrap()
}

struct RunningWorker {
    stop: watch::Sender<bool>,
    state: watch::Receiver<SessionState>,
    join: JoinHandle<Result<(), WorkerError>>,
}

impl RunningWorker {
    async fn state(&mut self, expected: SessionState) {
        timeout(WAIT, async {
            loop {
                if *self.state.borrow_and_update() == expected {
                    break;
                }
                self.state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn stop(self) {
        self.stop.send(true).unwrap();
        timeout(WAIT, self.join).await.unwrap().unwrap().unwrap();
    }
}

async fn stop_hub(hub: TestHub) {
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
}

struct Input {
    conversation: Uuid,
    input: Uuid,
    command: Uuid,
    execution: Uuid,
}

struct Fixture {
    pool: PgPool,
    _directory: TempDir,
    config: WorkerConfig,
    owner: Uuid,
    inputs: Vec<Input>,
    probe: StoreProbe,
    _proxy: DataProxy,
}

impl Fixture {
    async fn new(pool: &PgPool, hub: &TestHub) -> Self {
        let directory = TempDir::new().unwrap();
        // Проверки 1.2a явно согласуют сеанс без exec_v1.
        let proxy = DataProxy::start(hub, false, false).await;
        let machine = machines::add(pool, hub.installation.owner_id, "e2e")
            .await
            .unwrap();
        let token_file = directory.path().join("token");
        fs::write(&token_file, machine.token).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let ca_cert = directory.path().join("ca.pem");
        fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
        let config = WorkerConfig {
            machine: Default::default(),
            attachments: Default::default(),
            claude_code: None,
            codex: None,
            hub_url: proxy.url.clone(),
            ca_cert,
            token_file,
            machine_id: machine.machine_id,
            installation_id: hub.installation.installation_id,
            restore_generation: hub.installation.restore_generation,
            data_dir: directory.path().join("store"),
            timers: WorkerTimers {
                handshake: Duration::from_secs(5),
                heartbeat: Duration::from_millis(100),
                heartbeat_ack: Duration::from_secs(5),
                range_ack: Duration::from_secs(3),
                backoff_min: Duration::from_millis(250),
                backoff_max: Duration::from_millis(500),
                ..Default::default()
            },
            execution: Default::default(),
            opencode: None,
        };
        let mut fixture = Self {
            pool: pool.clone(),
            _directory: directory,
            config,
            owner: hub.installation.owner_id,
            inputs: vec![],
            probe: StoreProbe::default(),
            _proxy: proxy,
        };
        fixture.add_input().await;
        fixture
    }

    async fn add_input(&mut self) {
        let conversation =
            testkit::seed_conversation(&self.pool, self.owner, self.config.machine_id).await;
        let execution = Uuid::now_v7();
        let mut tx = self.pool.begin().await.unwrap();
        // Порядок блокировок как у хаба: сначала queue_state, иначе взаимоблокировка с его транзакцией.
        sqlx::query("select 1 from queue_state where conversation_id = $1 for update")
            .bind(conversation)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'created')")
            .bind(execution).bind(conversation).execute(&mut *tx).await.unwrap();
        // Подготовка исполнения атомарна: start_execution не попадает в живой outbox.
        sqlx::query("insert into commands (id, machine_id, conversation_id, execution_id, execution_generation, kind, payload, delivery_state, completed_at) values ($1, $2, $3, $4, 1, 'start_execution', '{}', 'completed', now())")
            .bind(Uuid::now_v7()).bind(self.config.machine_id).bind(conversation).bind(execution)
            .execute(&mut *tx).await.unwrap();
        for state in [
            ExecutionState::Created,
            ExecutionState::Starting,
            ExecutionState::Running,
        ] {
            executions::set_state_in_tx(&mut tx, execution, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let input = submit::submit_input(
            &self.pool,
            SubmitInput {
                owner_id: self.owner,
                conversation_id: conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text: "e2e".into() }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap();
        let dispatched = dispatch::dispatch_next(
            &self.pool,
            conversation,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        // При live-сеансе диспетчер мог успеть первым.
        assert!(
            matches!(dispatched, DispatchOutcome::Dispatched { command_id, .. } if command_id == input.command_id)
                || matches!(dispatched, DispatchOutcome::Blocked(_))
        );
        self.inputs.push(Input {
            conversation,
            input: input.input_id,
            command: input.command_id,
            execution,
        });
    }

    fn start(&self) -> RunningWorker {
        let store = WorkerStore::open(&self.config.data_dir)
            .unwrap()
            .with_probe(self.probe.clone());
        let worker = Worker::new(self.config.clone(), store);
        let state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        RunningWorker {
            stop,
            state,
            join: tokio::spawn(worker.run(shutdown)),
        }
    }

    fn snapshot(&self) -> StoreSnapshot {
        self.probe.snapshots().last().unwrap().clone()
    }

    async fn committed(&self) -> u64 {
        let seq: i64 = sqlx::query_scalar(
            "select coalesce(max(committed_seq), 0) from journal_cursors where machine_id = $1",
        )
        .bind(self.config.machine_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        seq.try_into().unwrap()
    }

    async fn settled(&self) {
        let expected = self.inputs.len() as u64;
        until(async || {
            self.committed().await == expected
                && self.snapshot().cursor.acked_seq_known_to_worker == expected
        })
        .await;
    }

    async fn assert_db(&self, states: &[&str], committed: u64) {
        assert_eq!(states.len(), self.inputs.len());
        for (input, state) in self.inputs.iter().zip(states) {
            let row: (String, Uuid, i32, String, Option<String>, bool, bool) = sqlx::query_as(
                "select i.state, i.command_id, c.dispatch_attempt, c.delivery_state, c.last_status, c.stored_at is not null, c.completed_at is not null from inputs i join commands c on c.id = i.command_id where i.id = $1")
                .bind(input.input).fetch_one(&self.pool).await.unwrap();
            let stored = *state == "on_machine";
            assert_eq!(
                row,
                (
                    state.to_string(),
                    input.command,
                    1,
                    if stored {
                        "stored_on_machine"
                    } else {
                        "pending"
                    }
                    .into(),
                    stored.then(|| "stored".into()),
                    stored,
                    false
                )
            );
            let events: Vec<String> = sqlx::query_scalar(
                "select payload->>'state' from events where kind in ('input_queued', 'input_state_changed') and payload->>'input_id' = $1 order by scope_seq")
                .bind(input.input.to_string()).fetch_all(&self.pool).await.unwrap();
            let mut expected = vec!["queued", "dispatched"];
            if stored {
                expected.push("on_machine");
            }
            assert_eq!(events, expected);
            let execution: String =
                sqlx::query_scalar("select state from executions where id = $1")
                    .bind(input.execution)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap();
            assert_eq!(execution, "running");
            let count: i64 =
                sqlx::query_scalar("select count(*) from inputs where conversation_id = $1")
                    .bind(input.conversation)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 1);
        }
        let counts: (i64, i64) =
            sqlx::query_as("select (select count(*) from inputs), (select count(*) from commands)")
                .fetch_one(&self.pool)
                .await
                .unwrap();
        assert_eq!(
            counts,
            (self.inputs.len() as i64, 2 * self.inputs.len() as i64)
        );
        let cursors: Vec<(Uuid, String, i64)> = sqlx::query_as(
            "select journal_id, kind, committed_seq from journal_cursors where machine_id = $1",
        )
        .bind(self.config.machine_id)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        if committed == 0 {
            assert!(
                cursors.is_empty()
                    || cursors
                        == vec![(
                            Uuid::parse_str(&self.snapshot().cursor.journal_id).unwrap(),
                            "cj".into(),
                            0
                        )]
            );
        } else {
            assert_eq!(
                cursors,
                vec![(
                    Uuid::parse_str(&self.snapshot().cursor.journal_id).unwrap(),
                    "cj".into(),
                    committed as i64
                )]
            );
        }
        // Хаб асинхронно дописывает события машины; счётчики и события читаем одним снимком.
        let mut snapshot = self
            .pool
            .begin_with("begin isolation level repeatable read")
            .await
            .unwrap();
        let scopes: Vec<(String, i64)> =
            sqlx::query_as("select scope, last_seq from scope_counters order by scope")
                .fetch_all(&mut *snapshot)
                .await
                .unwrap();
        assert!(!scopes.is_empty());
        for (scope, last) in scopes {
            let seqs: Vec<i64> = sqlx::query_scalar(
                "select scope_seq from events where scope = $1 order by scope_seq",
            )
            .bind(scope)
            .fetch_all(&mut *snapshot)
            .await
            .unwrap();
            assert_eq!(seqs, (1..=last).collect::<Vec<_>>());
        }
        snapshot.rollback().await.unwrap();
        assert!(self.snapshot().cursor.acked_seq_known_to_worker <= committed);
    }

    fn assert_local(&self, snapshot: &StoreSnapshot, acked: u64) {
        let count = self.inputs.len() as u64;
        assert_eq!(snapshot.cursor.last_seq, count);
        assert_eq!(snapshot.cursor.acked_seq_known_to_worker, acked);
        assert_eq!(snapshot.inbox.len() as u64, count);
        assert_eq!(snapshot.receipt_count, count);
        let mut commands: Vec<_> = snapshot
            .inbox
            .iter()
            .map(|command| {
                assert_eq!(command.dispatch_attempt, 1);
                Uuid::parse_str(&command.command_id).unwrap()
            })
            .collect();
        commands.sort();
        let mut expected: Vec<_> = self.inputs.iter().map(|input| input.command).collect();
        expected.sort();
        assert_eq!(commands, expected);
        let mut facts = BTreeMap::new();
        let history = self.probe.snapshots();
        for sample in &history {
            assert!(sample.cursor.acked_seq_known_to_worker <= acked);
            for record in &sample.cj {
                if let Some(previous) = facts.insert(record.seq, record.clone()) {
                    assert_eq!(previous, *record);
                }
            }
        }
        assert_eq!(facts.len() as u64, count);
        let mut stored = vec![];
        for (seq, record) in facts {
            assert!((1..=count).contains(&seq));
            let (command_id, dispatch_attempt) = record.stored_command.expect("unexpected CJ fact");
            assert_eq!(dispatch_attempt, 1);
            stored.push(Uuid::parse_str(&command_id).unwrap());
        }
        stored.sort();
        assert_eq!(stored, expected);
        assert_eq!(
            snapshot.cj.iter().map(|r| r.seq).collect::<Vec<_>>(),
            ((acked + 1)..=count).collect::<Vec<_>>()
        );
        assert!(
            history
                .windows(2)
                .all(|pair| pair[0].cursor.acked_seq_known_to_worker
                    <= pair[1].cursor.acked_seq_known_to_worker)
        );
    }

    fn reopen_and_assert(&self, acked: u64) {
        let store = WorkerStore::open(&self.config.data_dir).unwrap();
        assert_eq!(
            store.restore_generation().unwrap(),
            Some(self.config.restore_generation)
        );
        self.assert_local(&store.fault_snapshot().unwrap(), acked);
    }
}

fn assert_acks(faults: &Faults, fixture: &Fixture) {
    for ack in faults.acks() {
        assert!(ack.range.up_to_seq <= ack.committed_seq);
    }
    let grants = faults.grants();
    for sample in fixture.probe.snapshots() {
        let acked = sample.cursor.acked_seq_known_to_worker;
        if acked == 0 {
            continue;
        }
        assert!(
            grants.iter().any(|grant| grant.at <= sample.at
                && grant.journal_id == sample.cursor.journal_id
                && acked <= grant.up_to_seq
                && grant.up_to_seq <= grant.committed_seq),
            "worker cursor advanced without an earlier committed hub grant"
        );
    }
}

fn first_batch(faults: &Faults) -> BatchTrace {
    faults.batches().first().unwrap().clone()
}

fn sent_in_current_session(faults: &Faults) -> bool {
    let Some(hello) = faults.hellos().last().cloned() else {
        return false;
    };
    faults
        .commands()
        .iter()
        .any(|command| command.connection_id.to_string() == hello.connection_id)
}

async fn restart(pool: &PgPool, hub: TestHub, faults: Faults) -> TestHub {
    let addr = hub.addr;
    let pki = hub.pki.clone();
    let installation = hub.installation;
    stop_hub(hub).await;
    let restarted =
        spawn_test_hub_with_faults(pool.clone(), hub_timers(), Some(addr), pki, faults).await;
    assert_eq!(
        restarted.installation.installation_id,
        installation.installation_id
    );
    assert_eq!(
        restarted.installation.restore_generation,
        installation.restore_generation
    );
    restarted
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stream_break_before_ack_replays_same_range(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let fixture = Fixture::new(&pool, &hub).await;
    let first = hub.faults.pause_next(Point::BeforeCjCommit);
    hub.faults.drop_next_acks(1);
    let mut worker = fixture.start();
    let first_resume = hit(first).await;
    let original = first_batch(&hub.faults);
    assert_eq!(
        original.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![1]
    );
    fixture.assert_db(&["dispatched"], 0).await;
    fixture.assert_local(&fixture.snapshot(), 0);
    assert!(hub.faults.acks().is_empty());
    let replay = hub.faults.pause_next(Point::BeforeCjCommit);
    hub.faults.fail_control(Code::Cancelled);
    let resume = hit(replay).await;
    assert!(first_resume.is_closed());
    assert_eq!(hub.faults.batches()[1], original);
    let hellos = hub.faults.hellos();
    assert!(hellos.len() >= 2);
    assert_eq!(hellos[0].cj_journal_id, hellos[1].cj_journal_id);
    assert_ne!(hellos[0].connection_id, hellos[1].connection_id);
    assert_eq!(hellos[1].cursors[0].acked_seq_known_to_worker, 0);
    assert_eq!(hellos[1].cursors[0].last_seq, 1);
    fixture.assert_db(&["dispatched"], 0).await;
    fixture.assert_local(&fixture.snapshot(), 0);
    let before_ack = hub.faults.pause_next(Point::BeforeAckSend);
    resume.send(()).unwrap();
    let ack_resume = hit(before_ack).await;
    let ack = hub.faults.acks().last().unwrap().clone();
    let committed_at_send = fixture.committed().await;
    assert_eq!(committed_at_send, 1);
    assert!(ack.range.up_to_seq <= committed_at_send);
    assert_eq!(fixture.snapshot().cursor.acked_seq_known_to_worker, 0);
    ack_resume.send(()).unwrap();
    until(async || hub.faults.acks().iter().any(|ack| ack.dropped)).await;
    fixture.assert_db(&["on_machine"], 1).await;
    assert_eq!(fixture.snapshot().cursor.acked_seq_known_to_worker, 0);
    hub.faults.fail_control(Code::Cancelled);
    fixture.settled().await;
    worker.state(SessionState::Live).await;
    fixture.assert_db(&["on_machine"], 1).await;
    assert_acks(&hub.faults, &fixture);
    worker.stop().await;
    fixture.reopen_and_assert(1);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn hub_restart_redelivers_without_duplicates(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let fixture = Fixture::new(&pool, &hub).await;
    let before_send = hub.faults.pause_next(Point::BeforeCommandSend);
    let worker = fixture.start();
    let unsent = hit(before_send).await;
    assert!(hub.faults.commands().is_empty());
    assert_eq!(fixture.snapshot().inbox.len(), 0);
    assert_eq!(fixture.snapshot().cursor.last_seq, 0);
    fixture.assert_db(&["dispatched"], 0).await;

    let second = Faults::default();
    let before_commit = second.pause_next(Point::BeforeCjCommit);
    let hub = restart(&pool, hub, second).await;
    assert!(unsent.is_closed());
    let uncommitted = hit(before_commit).await;
    let sent = hub.faults.commands();
    assert!(!sent.is_empty());
    assert!(
        sent.iter()
            .all(|c| c.command_id == fixture.inputs[0].command.to_string())
    );
    fixture.assert_db(&["dispatched"], 0).await;
    fixture.assert_local(&fixture.snapshot(), 0);

    let third = Faults::default();
    let replay = third.pause_next(Point::BeforeCjCommit);
    let redelivery = third.pause_next(Point::BeforeCommandSend);
    let hub = restart(&pool, hub, third).await;
    assert!(uncommitted.is_closed());
    let redelivery_resume = hit(redelivery).await;
    fixture.assert_db(&["dispatched"], 0).await;
    redelivery_resume.send(()).unwrap();
    let replay_resume = hit(replay).await;
    until(async || fixture.probe.duplicate_count() >= 1).await;
    let redelivered = hub.faults.commands()[0].clone();
    assert_ne!(redelivered.connection_id, sent[0].connection_id);
    assert_eq!(
        (
            redelivered.command_id,
            redelivered.dispatch_attempt,
            redelivered.kind,
            redelivered.encoded_len
        ),
        (
            sent[0].command_id.clone(),
            sent[0].dispatch_attempt,
            sent[0].kind,
            sent[0].encoded_len
        )
    );
    fixture.assert_local(&fixture.snapshot(), 0);
    replay_resume.send(()).unwrap();
    fixture.settled().await;
    fixture.assert_db(&["on_machine"], 1).await;
    assert_acks(&hub.faults, &fixture);
    worker.stop().await;
    fixture.reopen_and_assert(1);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn transport_errors_do_not_repeat_commands(pool: PgPool) {
    let mut hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let fixture = Fixture::new(&pool, &hub).await;
    let pause = hub.faults.pause_next(Point::BeforeCjCommit);
    let mut worker = fixture.start();
    let mut resume = hit(pause).await;
    let original = first_batch(&hub.faults);
    until(async || sent_in_current_session(&hub.faults)).await;
    let mut restarts = 0;
    for _ in 0..2 {
        for code in [Code::Cancelled, Code::DeadlineExceeded] {
            fixture.assert_db(&["dispatched"], 0).await;
            fixture.assert_local(&fixture.snapshot(), 0);
            let pause = hub.faults.pause_next(Point::BeforeCjCommit);
            hub.faults.fail_control(code);
            let next = hit(pause).await;
            assert!(resume.is_closed());
            resume = next;
            assert_eq!(hub.faults.batches().last().unwrap(), &original);
            restarts += 1;
            until(async || {
                fixture.probe.duplicate_count() >= restarts && sent_in_current_session(&hub.faults)
            })
            .await;
        }
        let next_faults = Faults::default();
        let pause = next_faults.pause_next(Point::BeforeCjCommit);
        assert_acks(&hub.faults, &fixture);
        hub = restart(&pool, hub, next_faults).await;
        let next = hit(pause).await;
        assert!(resume.is_closed());
        resume = next;
        assert_eq!(first_batch(&hub.faults), original);
        restarts += 1;
        until(async || {
            fixture.probe.duplicate_count() >= restarts && sent_in_current_session(&hub.faults)
        })
        .await;
        assert!(!worker.join.is_finished());
    }
    fixture.assert_db(&["dispatched"], 0).await;
    resume.send(()).unwrap();
    fixture.settled().await;
    for code in [Code::Cancelled, Code::DeadlineExceeded] {
        let connections = hub.faults.hellos().len();
        hub.faults.fail_control(code);
        until(async || hub.faults.hellos().len() > connections).await;
        worker.state(SessionState::Live).await;
        fixture.assert_db(&["on_machine"], 1).await;
    }
    assert_acks(&hub.faults, &fixture);
    worker.stop().await;
    fixture.reopen_and_assert(1);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn lost_acks_do_not_duplicate_facts(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let mut fixture = Fixture::new(&pool, &hub).await;
    hub.faults.drop_next_acks(usize::MAX);
    let mut worker = fixture.start();
    for count in 1..=3 {
        until(async || {
            fixture.committed().await == count
                && hub
                    .faults
                    .acks()
                    .iter()
                    .any(|ack| ack.range.up_to_seq == count && ack.dropped)
        })
        .await;
        fixture
            .assert_db(&vec!["on_machine"; count as usize], count)
            .await;
        fixture.assert_local(&fixture.snapshot(), 0);
        if count < 3 {
            fixture.add_input().await;
        }
    }
    // Первый диапазон повторяется по таймеру; второй таймаут пересоздаёт сеанс.
    until(async || hub.faults.hellos().len() >= 2).await;
    fixture.settled().await;
    worker.state(SessionState::Live).await;
    let acks = hub.faults.acks();
    assert!(acks.len() >= 4);
    assert!(acks.iter().all(|ack| ack.dropped));
    let batches = hub.faults.batches();
    assert!(batches.iter().filter(|batch| **batch == batches[0]).count() >= 2);
    fixture.assert_db(&["on_machine"; 3], 3).await;
    assert_acks(&hub.faults, &fixture);
    worker.stop().await;
    fixture.reopen_and_assert(3);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn revoked_machine_cannot_reconnect(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let mut fixture = Fixture::new(&pool, &hub).await;
    let mut worker = fixture.start();
    fixture.settled().await;
    let connections = hub.faults.hellos().len();
    hub.faults.fail_control(Code::Cancelled);
    until(async || hub.faults.hellos().len() > connections).await;
    worker.state(SessionState::Live).await;
    assert!(!worker.join.is_finished());
    let pause = hub.faults.pause_next(Point::BeforeCjCommit);
    fixture.add_input().await;
    let uncommitted = hit(pause).await;
    assert_eq!(*worker.state.borrow(), SessionState::Live);
    fixture.assert_db(&["on_machine", "dispatched"], 1).await;
    fixture.assert_local(&fixture.snapshot(), 1);
    assert!(
        machines::revoke(&pool, fixture.config.machine_id)
            .await
            .unwrap()
    );
    assert!(
        machines::verify(
            &pool,
            &canterlot_worker::config::read_token(&fixture.config.token_file).unwrap()
        )
        .await
        .unwrap()
        .is_none()
    );
    hub.faults.fail_control(Code::Cancelled);
    let result = timeout(WAIT, &mut worker.join).await.unwrap().unwrap();
    assert!(matches!(
        result,
        Err(WorkerError::Hub {
            code: Code::Unauthenticated,
            ..
        })
    ));
    assert_eq!(
        *worker.state.borrow(),
        SessionState::Rejected {
            reason: RejectReason::CredentialRevoked as i32,
            retryable: false
        }
    );
    assert!(uncommitted.is_closed());
    fixture.assert_db(&["on_machine", "dispatched"], 1).await;
    assert_acks(&hub.faults, &fixture);
    fixture.reopen_and_assert(1);
    let revoked: bool = sqlx::query_scalar(
        "select revoked_at is not null from machine_credentials where machine_id = $1",
    )
    .bind(fixture.config.machine_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(revoked);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn invalid_command_id_does_not_wedge_real_hub_cj_cursor(pool: PgPool) {
    use canterlot_proto::{
        limits::MAX_MESSAGE_BYTES,
        worker::v1::{Command, ControlDown, control_down},
    };
    use prost::Message;

    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let mut fixture = Fixture::new(&pool, &hub).await;
    let pause = hub.faults.pause_next(Point::BeforeCommandSend);
    let mut worker = fixture.start();
    let resume = hit(pause).await;
    worker.state(SessionState::Live).await;
    let connection = hub
        .registry
        .session_info(fixture.config.machine_id)
        .unwrap()
        .connection_id;
    let sender = hub
        .registry
        .command_sender(fixture.config.machine_id)
        .unwrap();
    let invalid = ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: "invalid".repeat(40_000),
            ..Default::default()
        })),
    };
    assert!(invalid.encoded_len() <= MAX_MESSAGE_BYTES);
    sender.send(invalid).await.unwrap();
    // Первая валидная команда уже взята хабом; следующая пойдёт за неверной.
    resume.send(()).unwrap();
    fixture.settled().await;
    until(async || {
        hub.faults
            .commands()
            .iter()
            .any(|command| Uuid::parse_str(&command.command_id).is_err())
    })
    .await;
    fixture.add_input().await;
    fixture.settled().await;
    fixture.assert_db(&["on_machine", "on_machine"], 2).await;
    fixture.assert_local(&fixture.snapshot(), 2);
    assert_eq!(
        hub.registry
            .session_info(fixture.config.machine_id)
            .unwrap()
            .connection_id,
        connection
    );
    assert_eq!(hub.faults.hellos().len(), 1);
    assert!(matches!(*worker.state.borrow(), SessionState::Live));
    worker.stop().await;
    fixture.reopen_and_assert(2);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn rotated_token_reconnects_same_data_dir_without_cursor_divergence(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let mut fixture = Fixture::new(&pool, &hub).await;
    let mut worker = fixture.start();
    fixture.settled().await;
    worker.state(SessionState::Live).await;
    let before = fixture.snapshot().cursor;
    let old = canterlot_worker::config::read_token(&fixture.config.token_file).unwrap();
    let token = machines::rotate_token(&pool, fixture.config.machine_id, false, false)
        .await
        .unwrap()
        .unwrap();
    let result = timeout(Duration::from_secs(2), &mut worker.join)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(WorkerError::Hub {
            code: Code::Unauthenticated,
            ..
        })
    ));
    assert!(machines::verify(&pool, &old).await.unwrap().is_none());
    fs::write(&fixture.config.token_file, token).unwrap();
    fixture.reopen_and_assert(1);
    let mut worker = fixture.start();
    worker.state(SessionState::Live).await;
    fixture.add_input().await;
    fixture.settled().await;
    let after = fixture.snapshot().cursor;
    assert_eq!(after.journal_id, before.journal_id);
    assert_eq!(after.acked_seq_known_to_worker, 2);
    fixture.assert_db(&["on_machine", "on_machine"], 2).await;
    assert_acks(&hub.faults, &fixture);
    worker.stop().await;
    fixture.reopen_and_assert(2);
    stop_hub(hub).await;
}
