use std::{fs, path::Path, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    delivery::submit::{ContentBlock, SubmitInput},
    executions, machines,
    model::ExecutionState,
    testkit::{self, TestHub, spawn_test_hub},
};
use canterlot_proto::worker::v1::{CancelInput, Command, ControlDown, command, control_down};
use canterlot_worker::{
    config::{WorkerConfig, WorkerTimers},
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;

fn hub_timers() -> HubTimers {
    HubTimers {
        dispatch_sweep: Duration::from_millis(25),
        ..Default::default()
    }
}

async fn fixture(pool: &PgPool, hub: &TestHub) -> (TempDir, WorkerConfig, WorkerStore) {
    use std::os::unix::fs::PermissionsExt;
    let directory = TempDir::new().unwrap();
    let machine = machines::add(pool, hub.installation.owner_id, "session-test")
        .await
        .unwrap();
    let token_file = directory.path().join("token");
    fs::write(&token_file, machine.token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let ca_cert = directory.path().join("ca.pem");
    fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
    let data_dir = directory.path().join("store");
    let mut store = WorkerStore::open(&data_dir).unwrap();
    store
        .set_restore_generation(hub.installation.restore_generation)
        .unwrap();
    let config = WorkerConfig {
        hub_url: format!("https://localhost:{}", hub.addr.port()),
        ca_cert,
        token_file,
        machine_id: machine.machine_id,
        installation_id: hub.installation.installation_id,
        data_dir,
        timers: WorkerTimers {
            handshake: Duration::from_secs(2),
            heartbeat: Duration::from_millis(40),
            heartbeat_ack: Duration::from_millis(200),
            range_ack: Duration::from_millis(300),
            backoff_min: Duration::from_millis(200),
            backoff_max: Duration::from_millis(400),
            backoff_reset_after: Duration::from_millis(500),
            ..Default::default()
        },
    };
    (directory, config, store)
}

async fn wait_state(state: &mut watch::Receiver<SessionState>, expected: SessionState) {
    timeout(Duration::from_secs(8), async {
        loop {
            if *state.borrow_and_update() == expected {
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

fn start(
    config: WorkerConfig,
    store: WorkerStore,
) -> (
    watch::Sender<bool>,
    watch::Receiver<SessionState>,
    JoinHandle<Result<(), WorkerError>>,
) {
    let worker = Worker::new(config, store);
    let state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    (stop, state, tokio::spawn(worker.run(shutdown)))
}

async fn stop_worker(stop: watch::Sender<bool>, join: JoinHandle<Result<(), WorkerError>>) {
    stop.send(true).unwrap();
    timeout(Duration::from_secs(3), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn stop_hub(hub: TestHub) {
    hub.shutdown.send(()).unwrap();
    timeout(Duration::from_secs(3), hub.join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_connects_and_goes_live(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let before = hub.registry.session_info(machine).unwrap();
    assert!(before.data_attached);
    sleep(Duration::from_millis(150)).await;
    assert!(hub.registry.session_info(machine).unwrap().last_heartbeat > before.last_heartbeat);
    stop_worker(stop, join).await;
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_rejected_on_installation_mismatch_stops(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, mut config, store) = fixture(&pool, &hub).await;
    config.installation_id = Uuid::now_v7();
    let (_stop, state, join) = start(config, store);
    assert!(matches!(
        timeout(Duration::from_secs(3), join)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::Rejected {
            reason: 2,
            retryable: false
        })
    ));
    assert_eq!(
        *state.borrow(),
        SessionState::Rejected {
            reason: 2,
            retryable: false
        }
    );
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_reconnects_with_backoff_after_hub_restart(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let addr = hub.addr;
    let cert = hub.pki.server_cert_pem.clone();
    let key = hub.pki.server_key_pem.clone();
    let installation = hub.installation;
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let connection = hub.registry.session_info(machine).unwrap().connection_id;
    stop_hub(hub).await;
    wait_state(&mut state, SessionState::Backoff).await;
    let registry = canterlot_hub::session::Registry::new(hub_timers());
    let grpc =
        canterlot_hub::grpc::HubGrpc::new(pool, registry.clone(), installation, hub_timers());
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let (end, ended) = tokio::sync::oneshot::channel();
    let restarted = tokio::spawn(async move {
        canterlot_hub::grpc::serve_with_listener(grpc, listener, &cert, &key, async {
            let _ = ended.await;
        })
        .await
    });
    let started = tokio::time::Instant::now();
    wait_state(&mut state, SessionState::Live).await;
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert_ne!(
        registry.session_info(machine).unwrap().connection_id,
        connection
    );
    stop_worker(stop, join).await;
    end.send(()).unwrap();
    restarted.await.unwrap().unwrap();
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn received_command_is_stored_and_receipt_shipped(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let conversation =
        testkit::seed_conversation(&pool, hub.installation.owner_id, config.machine_id).await;
    let execution = executions::start(
        &pool,
        hub.installation.owner_id,
        conversation,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = now() where id = $1",
    )
    .bind(execution.command_id)
    .execute(&pool)
    .await
    .unwrap();
    let path = config.data_dir.clone();
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let input = hub
        .dispatcher
        .submit_input(
            &pool,
            SubmitInput {
                owner_id: hub.installation.owner_id,
                conversation_id: conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: "synthetic".into(),
                }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let state: String = sqlx::query_scalar("select state from inputs where id = $1")
                .bind(input.input_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "on_machine" {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop_worker(stop, join).await;
    let store = WorkerStore::open(&path).unwrap();
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn no_listening_socket_on_worker(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (directory, config, store) = fixture(&pool, &hub).await;
    let path = directory.path().join("worker.toml");
    fs::write(&path, format!("hub_url = {:?}\nca_cert = {:?}\ntoken_file = {:?}\nmachine_id = {:?}\ninstallation_id = {:?}\ndata_dir = {:?}\n", config.hub_url, config.ca_cert, config.token_file, config.machine_id.to_string(), config.installation_id.to_string(), config.data_dir)).unwrap();
    drop(store);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args(["run", "--config"])
        .arg(path)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while !hub
            .registry
            .session_info(config.machine_id)
            .is_some_and(|s| s.data_attached)
        {
            assert!(child.try_wait().unwrap().is_none());
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let pid = child.id().unwrap();
    let sockets: Vec<String> = fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|entry| {
            fs::read_link(entry.ok()?.path())
                .ok()?
                .to_str()
                .map(str::to_owned)
        })
        .filter_map(|target| {
            target
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect();
    let mut connected = 0;
    for table in ["tcp", "tcp6"] {
        for row in fs::read_to_string(format!("/proc/{pid}/net/{table}"))
            .unwrap()
            .lines()
            .skip(1)
        {
            let fields: Vec<_> = row.split_whitespace().collect();
            if sockets.iter().any(|inode| inode == fields[9]) {
                assert_ne!(fields[3], "0A", "worker owns a listener");
                if fields[3] == "01" {
                    connected += 1;
                }
            }
        }
    }
    assert_eq!(connected, 2);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    stop_hub(hub).await;
}

fn segment(path: &Path) -> std::path::PathBuf {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "log"))
        .unwrap()
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn poisoned_store_is_reopened_and_reconciled_before_reconnect(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let path = config.data_dir.clone();
    let receipt = segment(&path.join("receipts"));
    let backup = path.join("receipt-backup");
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let connection = hub.registry.session_info(machine).unwrap().connection_id;
    fs::rename(&receipt, &backup).unwrap();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    hub.registry
        .command_sender(machine)
        .unwrap()
        .send(ControlDown {
            msg: Some(control_down::Msg::Command(cmd.clone())),
        })
        .await
        .unwrap();
    wait_state(&mut state, SessionState::Backoff).await;
    fs::rename(backup, receipt).unwrap();
    wait_state(&mut state, SessionState::Live).await;
    assert_ne!(
        hub.registry.session_info(machine).unwrap().connection_id,
        connection
    );
    timeout(Duration::from_secs(3), async {
        loop {
            let committed: Option<i64> = sqlx::query_scalar(
                "select committed_seq from journal_cursors where machine_id = $1",
            )
            .bind(machine)
            .fetch_optional(&pool)
            .await
            .unwrap();
            if committed == Some(1) {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop_worker(stop, join).await;
    let mut store = WorkerStore::open(&path).unwrap();
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    assert_eq!(
        store.accept_command(&cmd).unwrap(),
        canterlot_worker::store::Accept::Duplicate
    );
    stop_hub(hub).await;
}

mod support;

use support::{Behavior, Observed, ProtocolHub};

#[tokio::test]
async fn first_accepted_persists_generation_and_binds_two_verified_connections() {
    let behavior = Behavior::default();
    let generation = behavior.generation;
    let (mut hub, store) = ProtocolHub::start(behavior).await;
    let journal_id = store.cj_journal_id().unwrap();
    let machine = hub.config.machine_id;
    let path = hub.config.data_dir.clone();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let Observed::Hello(control, control_peer) = hub.event().await else {
        panic!("expected control hello");
    };
    let Observed::Hello(data, data_peer) = hub.event().await else {
        panic!("expected data hello");
    };
    assert_eq!(control.stream_kind, 1);
    assert_eq!(data.stream_kind, 2);
    assert_eq!(control.machine_id, machine.to_string());
    assert_eq!(
        control.installation_id,
        hub.config.installation_id.to_string()
    );
    assert_eq!(
        control.protocol_version,
        canterlot_proto::limits::PROTOCOL_VERSION
    );
    assert_eq!(control.cj_journal_id, journal_id.to_string());
    assert_eq!(control.cursors[0].first_stored_seq, 1);
    assert_eq!(control.cursors[0].last_seq, 0);
    assert_eq!(control.cursors[0].acked_seq_known_to_worker, 0);
    assert!(control.restore_generation.is_empty());
    assert_eq!(data.restore_generation, generation.to_string());
    assert_eq!(control.connection_id, data.connection_id);
    assert_eq!(control.cj_journal_id, data.cj_journal_id);
    assert_eq!(
        control.boot_id,
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap()
            .trim()
    );
    assert_eq!(data.boot_id, control.boot_id);
    assert_eq!(
        Uuid::parse_str(&control.connection_id)
            .unwrap()
            .get_version_num(),
        7
    );
    assert_ne!(control_peer, data_peer);
    stop_worker(stop, join).await;
    assert_eq!(
        WorkerStore::open(&path)
            .unwrap()
            .restore_generation()
            .unwrap(),
        Some(generation)
    );
}

#[tokio::test]
async fn missing_heartbeat_ack_resends_once_then_restarts_session() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        heartbeat_ack: false,
        ..Default::default()
    })
    .await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    wait_state(&mut state, SessionState::Backoff).await;
    let mut sequences = Vec::new();
    while let Ok(event) = hub.events.try_recv() {
        if let Observed::Control(message) = event
            && let Some(canterlot_proto::worker::v1::control_up::Msg::Heartbeat(h)) = message.msg
        {
            sequences.push(h.seq);
        }
    }
    assert_eq!(sequences, [1, 1]);
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn missing_range_ack_resends_same_records_without_compaction() {
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        range_ack: false,
        ..Default::default()
    })
    .await;
    store
        .accept_command(&Command {
            command_id: Uuid::now_v7().to_string(),
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
            ..Default::default()
        })
        .unwrap();
    let path = hub.config.data_dir.clone();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Backoff).await;
    let mut batches = Vec::new();
    while let Ok(event) = hub.events.try_recv() {
        if let Observed::Control(message) = event
            && let Some(canterlot_proto::worker::v1::control_up::Msg::CjRecords(batch)) =
                message.msg
        {
            batches.push(batch);
        }
    }
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0], batches[1]);
    stop_worker(stop, join).await;
    let store = WorkerStore::open(&path).unwrap();
    assert_eq!(store.cj_cursor().unwrap().acked_seq_known_to_worker, 0);
    assert_eq!(store.cj_records_from(1, usize::MAX).unwrap().len(), 1);
}

#[tokio::test]
async fn replay_starts_at_committed_plus_one_and_batches_under_limit() {
    use canterlot_journal::{Journal, JournalOptions};
    use canterlot_proto::{
        limits::check_record_batch,
        worker::v1::{CommandRejected, RecordBody, control_up, record_body},
    };
    use prost::Message;
    let (mut hub, store) = ProtocolHub::start(Behavior {
        committed: 1,
        ..Default::default()
    })
    .await;
    let path = hub.config.data_dir.clone();
    drop(store);
    let (mut journal, _) = Journal::open(&path.join("cj"), JournalOptions::default()).unwrap();
    for _ in 0..4 {
        journal
            .append(
                &RecordBody {
                    body: Some(record_body::Body::CommandRejected(CommandRejected {
                        command_id: Uuid::now_v7().to_string(),
                        dispatch_attempt: 1,
                        reason: "x".repeat(200_000),
                    })),
                }
                .encode_to_vec(),
            )
            .unwrap();
    }
    drop(journal);
    let store = WorkerStore::open(&path).unwrap();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let mut batches = Vec::new();
    while let Ok(event) = hub.events.try_recv() {
        if let Observed::Control(message) = event
            && let Some(control_up::Msg::CjRecords(batch)) = message.msg
        {
            batches.push(batch);
        }
    }
    assert_eq!(batches.len(), 2);
    for batch in &batches {
        check_record_batch(batch).unwrap();
    }
    assert_eq!(
        batches
            .iter()
            .flat_map(|batch| batch.records.iter().map(|r| r.seq))
            .collect::<Vec<_>>(),
        [2, 3, 4]
    );
    stop_worker(stop, join).await;
    let store = WorkerStore::open(&path).unwrap();
    assert_eq!(store.cj_cursor().unwrap().acked_seq_known_to_worker, 4);
    assert!(store.cj_records_from(5, usize::MAX).unwrap().is_empty());
}

#[tokio::test]
async fn handshake_timeout_retries_and_shutdown_interrupts_backoff() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        hello_reply: false,
        ..Default::default()
    })
    .await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Backoff).await;
    assert!(matches!(hub.event().await, Observed::Hello(_, _)));
    let started = tokio::time::Instant::now();
    stop_worker(stop, join).await;
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[tokio::test]
async fn tls_rejects_untrusted_ca_and_wrong_hostname() {
    for wrong_ca in [true, false] {
        let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
        if wrong_ca {
            fs::write(&hub.config.ca_cert, testkit::tls::test_pki().ca_pem).unwrap();
        } else {
            hub.config.hub_url = hub.config.hub_url.replace("localhost", "127.0.0.1");
        }
        let (stop, mut state, join) = start(hub.config.clone(), store);
        wait_state(&mut state, SessionState::Backoff).await;
        assert!(hub.events.try_recv().is_err());
        stop_worker(stop, join).await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn retryable_identity_rejection_backs_off_without_displacing_live_worker(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let second_dir = TempDir::new().unwrap();
    let mut second_config = config.clone();
    second_config.data_dir = second_dir.path().to_owned();
    let mut second_store = WorkerStore::open(second_dir.path()).unwrap();
    second_store
        .set_restore_generation(hub.installation.restore_generation)
        .unwrap();
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let connection = hub.registry.session_info(machine).unwrap().connection_id;
    let (second_stop, mut second_state, second_join) = start(second_config, second_store);
    wait_state(&mut second_state, SessionState::Backoff).await;
    assert_eq!(
        hub.registry.session_info(machine).unwrap().connection_id,
        connection
    );
    stop_worker(stop, join).await;
    wait_state(&mut second_state, SessionState::Live).await;
    stop_worker(second_stop, second_join).await;
    stop_hub(hub).await;
}

#[tokio::test]
async fn commands_are_receipted_while_previous_range_ack_is_pending() {
    let cmd = || Command {
        command_id: Uuid::now_v7().to_string(),
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        range_ack: false,
        command_on_first_batch: Some(cmd()),
        ..Default::default()
    })
    .await;
    store.accept_command(&cmd()).unwrap();
    let (stop, _state, join) = start(hub.config.clone(), store);
    let mut batches = Vec::new();
    while batches.len() < 2 {
        if let Observed::Control(message) = hub.event().await
            && let Some(canterlot_proto::worker::v1::control_up::Msg::CjRecords(batch)) =
                message.msg
        {
            batches.push(batch);
        }
    }
    assert_eq!(batches[0].records[0].seq, 1);
    assert_eq!(batches[1].records[0].seq, 2);
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn worker_api_refuses_plaintext_urls() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    hub.config.hub_url = hub.config.hub_url.replace("https://", "http://");
    let (_stop, _state, join) = start(hub.config.clone(), store);
    assert!(matches!(
        timeout(Duration::from_secs(1), join)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::InvalidConfig)
    ));
    assert!(hub.events.try_recv().is_err());
}
