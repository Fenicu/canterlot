use std::{fs, path::Path, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    executions, machines,
    testkit::{self, TestHub, spawn_test_hub},
};
use canterlot_proto::worker::v1::{CancelInput, Command, ControlDown, command, control_down};
use canterlot_worker::{
    config::{WorkerConfig, WorkerTimers},
    session::{SessionState, StoreErrorKind, Worker, WorkerError},
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
    let store = WorkerStore::open(&data_dir).unwrap();
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
        execution: Default::default(),
        opencode: None,
        antigravity: None,
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

async fn wait_backoff(state: &mut watch::Receiver<SessionState>) {
    timeout(Duration::from_secs(8), async {
        loop {
            if matches!(*state.borrow_and_update(), SessionState::Backoff { .. }) {
                break;
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
    let runtime_dir = config.data_dir.join("runtime");
    let worker = Worker::new(config, store).with_runtime_dir(runtime_dir);
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

// Настоящие хаб и воркер: запись вида, которого хаб не знает, остаётся на машине, а сверка стоит.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn cj_record_the_hub_cannot_read_stops_the_session_and_stays_on_the_worker(pool: PgPool) {
    use canterlot_journal::{Journal, JournalOptions};
    use canterlot_worker::session::ErrorKind;
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let journal = store.cj_journal_id().unwrap();
    drop(store);
    let (mut cj, _) =
        Journal::open(&config.data_dir.join("cj"), JournalOptions::default()).unwrap();
    // Поле 99 в oneof: так выглядит запись воркера новее хаба.
    cj.append(&[0x98, 0x06, 0x01]).unwrap();
    drop(cj);
    let store = WorkerStore::open(&config.data_dir).unwrap();
    let (stop, mut state, join) = start(config.clone(), store);
    timeout(Duration::from_secs(8), async {
        loop {
            if matches!(
                *state.borrow_and_update(),
                SessionState::Backoff {
                    last_error: ErrorKind::Hub(tonic::Code::InvalidArgument)
                }
            ) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!hub.registry.is_reconciled(machine));
    let stopped: Vec<(String, i64, String)> =
        sqlx::query_as("select kind, seq, reason from journal_stops where journal_id = $1")
            .bind(journal)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stopped, [("cj".to_owned(), 1, "unknown_record".to_owned())]);
    let events: i64 = sqlx::query_scalar(
        "select count(*) from events where kind = 'machine_journal_stops_changed'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
    stop_worker(stop, join).await;
    stop_hub(hub).await;
    let store = WorkerStore::open(&config.data_dir).unwrap();
    let cursor = store.cj_cursor().unwrap();
    assert_eq!((cursor.last_seq, cursor.acked_seq_known_to_worker), (1, 0));
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_connects_and_goes_live(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    assert_eq!(store.restore_generation().unwrap(), None);
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
    wait_backoff(&mut state).await;
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
    let machine = config.machine_id;
    let path = config.data_dir.clone();
    let conversation =
        testkit::seed_conversation(&pool, hub.installation.owner_id, config.machine_id).await;
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    timeout(Duration::from_secs(5), async {
        while !hub.registry.is_reconciled(machine) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let execution = executions::start(
        &pool,
        hub.installation.owner_id,
        conversation,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let stored: bool =
                sqlx::query_scalar("select stored_at is not null from commands where id = $1")
                    .bind(execution.command_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let acked = fs::read(path.join("cj/ack"))
                .is_ok_and(|bytes| u64::from_le_bytes(bytes[..8].try_into().unwrap()) >= 1);
            if stored && acked {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop_worker(stop, join).await;
    let store = WorkerStore::open(&path).unwrap();
    assert!(store.cj_cursor().unwrap().last_seq >= 1);
    assert!(store.cj_cursor().unwrap().acked_seq_known_to_worker >= 1);
    stop_hub(hub).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn no_listening_socket_on_worker(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (directory, config, store) = fixture(&pool, &hub).await;
    let path = directory.path().join("worker.toml");
    fs::write(&path, format!("hub_url = {:?}\nca_cert = {:?}\ntoken_file = {:?}\nmachine_id = {:?}\ninstallation_id = {:?}\nrestore_generation = {:?}\ndata_dir = {:?}\n", config.hub_url, config.ca_cert, config.token_file, config.machine_id.to_string(), config.installation_id.to_string(), config.restore_generation.to_string(), config.data_dir)).unwrap();
    drop(store);
    let runtime = directory.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args(["run", "--config"])
        .arg(path)
        .env("XDG_RUNTIME_DIR", &runtime)
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
    // Сокеты проверяются через копии дескрипторов воркера, а не по
    // /proc/<pid>/net/tcp: эта таблица общая для сетевого пространства, и
    // пока соседние тесты открывают и закрывают соединения, постраничное
    // чтение отдаёт одни строки дважды, а другие пропускает.
    let pid = child.id().unwrap();
    let pidfd = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(pid as i32).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    let mut connected = 0;
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let entry = entry.unwrap();
        let Ok(target) = fs::read_link(entry.path()) else {
            continue;
        };
        if !target.to_str().is_some_and(|t| t.starts_with("socket:[")) {
            continue;
        }
        let number: i32 = entry.file_name().to_str().unwrap().parse().unwrap();
        let socket = match rustix::process::pidfd_getfd(
            &pidfd,
            number,
            rustix::process::PidfdGetfdFlags::empty(),
        ) {
            Ok(socket) => socket,
            // Дескриптор закрылся между чтением каталога и копированием.
            Err(rustix::io::Errno::BADF) => continue,
            Err(error) => panic!("pidfd_getfd({number}): {error}"),
        };
        let domain = rustix::net::sockopt::socket_domain(&socket).unwrap();
        if domain != rustix::net::AddressFamily::INET && domain != rustix::net::AddressFamily::INET6
        {
            continue;
        }
        if rustix::net::sockopt::socket_type(&socket).unwrap() != rustix::net::SocketType::STREAM {
            continue;
        }
        assert!(
            !rustix::net::sockopt::socket_acceptconn(&socket).unwrap(),
            "worker owns a listener"
        );
        if rustix::net::getpeername(&socket).is_ok_and(|peer| peer.is_some()) {
            connected += 1;
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
async fn poisoned_store_waits_for_storage_then_requests_a_restart(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    let path = config.data_dir.clone();
    let receipt = segment(&path.join("receipts"));
    let backup = path.join("receipt-backup");
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
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
    wait_backoff(&mut state).await;
    fs::rename(backup, receipt).unwrap();
    let error = timeout(Duration::from_secs(3), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, WorkerError::StorageRestart), "{error}");
    // Следующий запуск службы открывает то же хранилище и доигрывает сохранённое.
    let store = WorkerStore::open(&path).unwrap();
    assert!(store.cj_cursor().unwrap().last_seq >= 1);
    drop(stop);
    stop_hub(hub).await;
}

mod support;

use support::{Behavior, Observed, ProtocolHub};

#[tokio::test]
async fn configured_generation_is_persisted_and_binds_two_verified_connections() {
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
    assert_eq!(control.restore_generation, generation.to_string());
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
    wait_backoff(&mut state).await;
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
    wait_backoff(&mut state).await;
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
    wait_backoff(&mut state).await;
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
        wait_backoff(&mut state).await;
        assert!(hub.events.try_recv().is_err());
        stop_worker(stop, join).await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn retryable_identity_rejection_backs_off_without_displacing_live_worker(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, config, store) = fixture(&pool, &hub).await;
    let machine = config.machine_id;
    use std::os::unix::fs::PermissionsExt;
    let second_dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let mut second_config = config.clone();
    second_config.data_dir = second_dir.path().to_owned();
    // Коллизия живого сеанса проверяется на клоне CJ, без сброса локального состояния.
    canterlot_journal::Journal::create(
        &second_dir.path().join("cj"),
        canterlot_journal::JournalId(store.cj_journal_id().unwrap()),
        canterlot_journal::JournalOptions::default(),
    )
    .unwrap();
    let second_store = WorkerStore::open(second_dir.path()).unwrap();
    let (stop, mut state, join) = start(config, store);
    wait_state(&mut state, SessionState::Live).await;
    let connection = hub.registry.session_info(machine).unwrap().connection_id;
    let (second_stop, mut second_state, second_join) = start(second_config, second_store);
    wait_backoff(&mut second_state).await;
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

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wrong_configured_generation_is_rejected_by_hub(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
    let (_directory, mut config, store) = fixture(&pool, &hub).await;
    config.restore_generation = Uuid::now_v7();
    let machine = config.machine_id;
    let path = config.data_dir.clone();
    let generation = config.restore_generation;
    let (stop, mut state, join) = start(config, store);
    wait_backoff(&mut state).await;
    assert!(hub.registry.session_info(machine).is_none());
    stop_worker(stop, join).await;
    assert_eq!(
        WorkerStore::open(&path)
            .unwrap()
            .restore_generation()
            .unwrap(),
        Some(generation)
    );
    stop_hub(hub).await;
}

#[tokio::test]
async fn accepted_generation_mismatch_stops_without_adopting_hub_generation() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let generation = Uuid::now_v7();
    hub.config.restore_generation = generation;
    let path = hub.config.data_dir.clone();
    let (_stop, state, join) = start(hub.config.clone(), store);
    assert!(matches!(
        timeout(Duration::from_secs(2), join)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::RestoreGenerationChanged)
    ));
    assert_ne!(*state.borrow(), SessionState::Live);
    let Observed::Hello(hello, _) = hub.event().await else {
        panic!("expected control hello");
    };
    assert_eq!(hello.restore_generation, generation.to_string());
    assert!(hub.events.try_recv().is_err());
    assert_eq!(
        WorkerStore::open(&path)
            .unwrap()
            .restore_generation()
            .unwrap(),
        Some(generation)
    );
}

#[tokio::test]
async fn persisted_generation_mismatch_refuses_start_without_overwrite() {
    let (mut hub, mut store) = ProtocolHub::start(Behavior::default()).await;
    let persisted = Uuid::now_v7();
    store.set_restore_generation(persisted).unwrap();
    let path = hub.config.data_dir.clone();
    let (_stop, state, join) = start(hub.config.clone(), store);
    assert!(matches!(
        timeout(Duration::from_secs(1), join)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::GenerationMismatch)
    ));
    assert_eq!(*state.borrow(), SessionState::Disconnected);
    assert!(hub.events.try_recv().is_err());
    assert_eq!(
        WorkerStore::open(&path)
            .unwrap()
            .restore_generation()
            .unwrap(),
        Some(persisted)
    );
}

fn cancel_command() -> Command {
    Command {
        command_id: Uuid::now_v7().to_string(),
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    }
}

fn seed_backlog(store: WorkerStore, path: &Path, count: usize, size: usize) -> WorkerStore {
    use canterlot_journal::{Journal, JournalOptions};
    use canterlot_proto::worker::v1::{CommandRejected, RecordBody, record_body};
    use prost::Message;
    drop(store);
    let (mut journal, _) = Journal::open(&path.join("cj"), JournalOptions::default()).unwrap();
    for _ in 0..count {
        journal
            .append(
                &RecordBody {
                    body: Some(record_body::Body::CommandRejected(CommandRejected {
                        command_id: Uuid::now_v7().to_string(),
                        dispatch_attempt: 1,
                        reason: "x".repeat(size),
                    })),
                }
                .encode_to_vec(),
            )
            .unwrap();
    }
    drop(journal);
    WorkerStore::open(path).unwrap()
}

async fn live_without_backoff(state: &mut watch::Receiver<SessionState>) {
    timeout(Duration::from_secs(15), async {
        loop {
            let current = state.borrow_and_update().clone();
            assert!(
                !matches!(current, SessionState::Backoff { .. }),
                "unexpected restart: {current:?}"
            );
            if current == SessionState::Live {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn steadily_acking_backlog_does_not_restart_and_batches_have_256_records_at_most() {
    use canterlot_proto::worker::v1::control_up;
    let (mut hub, store) = ProtocolHub::start(Behavior {
        range_ack_delay: Duration::from_millis(180),
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_millis(120);
    hub.config.timers.heartbeat_ack = Duration::from_millis(300);
    let store = seed_backlog(store, &hub.config.data_dir, 2304, 0);
    let (stop, mut state, join) = start(hub.config.clone(), store);
    live_without_backoff(&mut state).await;
    let mut hellos = 0;
    let mut ranges = std::collections::BTreeSet::new();
    while let Ok(event) = hub.events.try_recv() {
        match event {
            Observed::Hello(_, _) => hellos += 1,
            Observed::Data(_) => {}
            Observed::Control(message) => {
                if let Some(control_up::Msg::CjRecords(batch)) = message.msg {
                    assert!(batch.records.len() <= 256);
                    canterlot_proto::limits::check_record_batch(&batch).unwrap();
                    ranges.insert((batch.records[0].seq, batch.records.last().unwrap().seq));
                }
            }
        }
    }
    assert_eq!(hellos, 2);
    assert_eq!(ranges.len(), 9);
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn ack_ahead_of_ship_cursor_does_not_restart() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        ack_override: Some(4000),
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_secs(2);
    hub.config.timers.heartbeat_ack = Duration::from_millis(300);
    let store = seed_backlog(store, &hub.config.data_dir, 4000, 1500);
    let (stop, mut state, join) = start(hub.config.clone(), store);
    live_without_backoff(&mut state).await;
    sleep(Duration::from_millis(150)).await;
    assert_eq!(*state.borrow(), SessionState::Live);
    stop_worker(stop, join).await;
}

#[tokio::test(flavor = "current_thread")]
async fn large_command_burst_during_large_replay_keeps_heartbeat_acks_timely() {
    use canterlot_proto::worker::v1::{
        ContentBlock, DeliverInput, TextBlock, content_block, control_up,
    };
    let commands: Vec<_> = (0..64)
        .map(|_| Command {
            kind: Some(command::Kind::DeliverInput(DeliverInput {
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock {
                        text: "x".repeat(262_144),
                    })),
                }],
                ..Default::default()
            })),
            ..cancel_command()
        })
        .collect();
    let (mut hub, store) = ProtocolHub::start(Behavior {
        commands_on_first_batch: commands,
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_secs(3);
    hub.config.timers.heartbeat = Duration::from_millis(30);
    // При загрузке CPU fake hub поздно читает heartbeat: даём ACK 1 с и допускаем один повтор seq, сохраняя проверку прогресса и отсутствия рестарта.
    hub.config.timers.heartbeat_ack = Duration::from_secs(1);
    let path = hub.config.data_dir.clone();
    let store = seed_backlog(store, &path, 48, 200_000);
    let (stop, mut state, join) = start(hub.config.clone(), store);
    let mut last_heartbeat = 0;
    let mut heartbeat_resent = false;
    let mut hellos = 0;
    let mut committed = 0;
    timeout(Duration::from_secs(15), async {
        while committed < 112 || last_heartbeat < 3 {
            assert!(
                !matches!(*state.borrow_and_update(), SessionState::Backoff { .. }),
                "unexpected restart"
            );
            match hub.event().await {
                Observed::Hello(_, _) => {
                    hellos += 1;
                    assert!(hellos <= 2, "unexpected reconnect");
                }
                Observed::Data(_) => {}
                Observed::Control(message) => match message.msg {
                    Some(control_up::Msg::CjRecords(batch)) => {
                        committed = committed.max(batch.records.last().unwrap().seq)
                    }
                    Some(control_up::Msg::Heartbeat(h)) => {
                        assert!(
                            h.seq >= last_heartbeat,
                            "heartbeat sequence moved backwards"
                        );
                        if h.seq == last_heartbeat {
                            assert!(!heartbeat_resent, "heartbeat resent more than once");
                            heartbeat_resent = true;
                        } else {
                            heartbeat_resent = false;
                            last_heartbeat = h.seq;
                        }
                    }
                    _ => {}
                },
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(hellos, 2);
    live_without_backoff(&mut state).await;
    stop_worker(stop, join).await;
    let store = WorkerStore::open(&path).unwrap();
    assert_eq!(store.cj_cursor().unwrap().last_seq, 112);
}

#[tokio::test]
async fn invalid_ids_are_dropped_and_over_limit_commands_rejected_without_restart() {
    use canterlot_proto::worker::v1::{
        ContentBlock, DeliverInput, TextBlock, content_block, control_up, record_body,
    };
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let path = hub.config.data_dir.clone();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let invalid = Command {
        command_id: "raw invalid id".into(),
        ..cancel_command()
    };
    let large = Command {
        kind: Some(command::Kind::DeliverInput(DeliverInput {
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "x".repeat(262_145),
                })),
            }],
            ..Default::default()
        })),
        ..cancel_command()
    };
    let good = cancel_command();
    for command in [&invalid, &large, &good] {
        hub.send(ControlDown {
            msg: Some(control_down::Msg::Command(command.clone())),
        })
        .await;
    }
    let mut facts = Vec::new();
    timeout(Duration::from_secs(3), async {
        while facts.len() < 2 {
            assert!(!matches!(*state.borrow(), SessionState::Backoff { .. }));
            if let Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::CjRecords(batch)) = message.msg
            {
                facts.extend(
                    batch
                        .records
                        .into_iter()
                        .map(|r| r.body.unwrap().body.unwrap()),
                );
            }
        }
    })
    .await
    .unwrap();
    let record_body::Body::CommandRejected(rejected) = &facts[0] else {
        panic!("missing rejection");
    };
    assert_eq!(rejected.command_id, large.command_id);
    assert_eq!(rejected.reason, "resource_limit");
    assert!(matches!(facts[1], record_body::Body::CommandStored(_)));
    stop_worker(stop, join).await;
    let mut store = WorkerStore::open(&path).unwrap();
    assert_eq!(
        store.accept_command(&invalid).unwrap(),
        canterlot_worker::store::Accept::DroppedInvalidId
    );
}

#[tokio::test]
async fn slow_range_progress_resets_head_timer() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        range_ack_delay: Duration::from_millis(900),
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_millis(600);
    hub.config.timers.heartbeat_ack = Duration::from_millis(500);
    let store = seed_backlog(store, &hub.config.data_dir, 20, 200_000);
    let (stop, mut state, join) = start(hub.config.clone(), store);
    live_without_backoff(&mut state).await;
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn hub_status_codes_are_visible_during_backoff_and_unauthenticated_is_terminal() {
    for code in [tonic::Code::PermissionDenied, tonic::Code::InvalidArgument] {
        let (mut hub, store) = ProtocolHub::start(Behavior {
            status: Some((code, "foreign_journal")),
            ..Default::default()
        })
        .await;
        let (stop, mut state, join) = start(hub.config.clone(), store);
        wait_backoff(&mut state).await;
        assert!(format!("{:?}", *state.borrow()).contains(&format!("Hub({code:?})")));
        assert!(!join.is_finished());
        stop_worker(stop, join).await;
        assert!(matches!(hub.event().await, Observed::Hello(_, _)));
    }
    let (hub, store) = ProtocolHub::start(Behavior {
        status: Some((tonic::Code::Unauthenticated, "invalid machine credential")),
        ..Default::default()
    })
    .await;
    let (_stop, state, join) = start(hub.config.clone(), store);
    assert!(matches!(
        timeout(Duration::from_secs(2), join)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::Hub {
            code: tonic::Code::Unauthenticated,
            ..
        })
    ));
    assert!(matches!(
        *state.borrow(),
        SessionState::Rejected {
            retryable: false,
            ..
        }
    ));
}

#[tokio::test]
async fn sealed_or_corrupt_store_is_fatal_when_the_restarted_worker_reopens_it() {
    use std::io::Write;
    for kind in ["cj", "inbox", "metadata"] {
        let (hub, store) = ProtocolHub::start(Behavior::default()).await;
        let path = hub.config.data_dir.clone();
        let receipt = segment(&path.join("receipts"));
        let backup = path.join("receipt-backup");
        let (stop, mut state, join) = start(hub.config.clone(), store);
        wait_state(&mut state, SessionState::Live).await;
        fs::rename(&receipt, &backup).unwrap();
        hub.send(ControlDown {
            msg: Some(control_down::Msg::Command(cancel_command())),
        })
        .await;
        wait_backoff(&mut state).await;
        let expected = if kind == "metadata" {
            fs::write(path.join("hub.toml"), "not valid metadata\n").unwrap();
            StoreErrorKind::Metadata
        } else {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(segment(&path.join(kind)))
                .unwrap();
            file.write_all(b"torn tail").unwrap();
            file.sync_all().unwrap();
            if kind == "cj" {
                StoreErrorKind::CjSealed
            } else {
                StoreErrorKind::LocalSealed
            }
        };
        let error = timeout(Duration::from_secs(2), join)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, WorkerError::StorageRestart), "{error}");
        fs::rename(&backup, &receipt).unwrap();
        let reopened = match WorkerStore::open(&path) {
            Ok(_) => panic!("a restarted worker must not reopen a sealed or corrupt store"),
            Err(error) => error,
        };
        assert_eq!(reopened.kind(), expected, "{reopened}");
        drop(stop);
    }
}

#[tokio::test]
async fn reconnect_retries_transient_token_and_ca_reads_with_one_second_spacing() {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["token", "permissions", "ca"] {
        let (hub, store) = ProtocolHub::start(Behavior::default()).await;
        let path = if mode == "ca" {
            &hub.config.ca_cert
        } else {
            &hub.config.token_file
        };
        let saved = fs::read(path).unwrap();
        let (stop, mut state, join) = start(hub.config.clone(), store);
        wait_state(&mut state, SessionState::Live).await;
        if mode == "permissions" {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        hub.fail(tonic::Status::unavailable("reconnect")).await;
        wait_backoff(&mut state).await;
        sleep(Duration::from_millis(1500)).await;
        assert!(!join.is_finished(), "read failure must be retried");
        fs::write(path, saved).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        wait_state(&mut state, SessionState::Live).await;
        stop_worker(stop, join).await;
    }
}

#[tokio::test]
async fn persistent_reconnect_read_error_is_fatal_after_three_retries() {
    let (hub, store) = ProtocolHub::start(Behavior::default()).await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    fs::remove_file(&hub.config.token_file).unwrap();
    let started = tokio::time::Instant::now();
    hub.fail(tonic::Status::unavailable("reconnect")).await;
    let error = timeout(Duration::from_secs(5), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(error, WorkerError::Config(_)));
    assert!(started.elapsed() >= Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_millis(4200));
    drop(stop);
}

#[tokio::test]
async fn sigterm_closes_streams_and_releases_journal_locks() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let config = &hub.config;
    let path = config.data_dir.parent().unwrap().join("worker.toml");
    fs::write(&path, format!("hub_url = {:?}\nca_cert = {:?}\ntoken_file = {:?}\nmachine_id = {:?}\ninstallation_id = {:?}\nrestore_generation = {:?}\ndata_dir = {:?}\n", config.hub_url, config.ca_cert, config.token_file, config.machine_id.to_string(), config.installation_id.to_string(), config.restore_generation.to_string(), config.data_dir)).unwrap();
    let data_dir = config.data_dir.clone();
    drop(store);
    let runtime = data_dir.parent().unwrap().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args(["run", "--config"])
        .arg(path)
        .env("XDG_RUNTIME_DIR", &runtime)
        .kill_on_drop(true)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut hellos = 0;
    while hellos < 2 {
        if let Observed::Hello(_, _) = hub.event().await {
            hellos += 1;
        }
    }
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &child.id().unwrap().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let output = timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(WorkerStore::open(&data_dir).is_ok());
    let log = String::from_utf8(output.stderr).unwrap();
    assert!(log.contains("disconnected"));
    assert!(!log.contains("protocol-test"));
}

#[tokio::test]
async fn stalled_outbound_write_does_not_block_inbound_command_or_ack() {
    use canterlot_proto::worker::v1::{JournalRangeAck, control_up};
    let (mut hub, store) = ProtocolHub::start(Behavior {
        range_ack: false,
        pause_after_first_batch: Duration::from_secs(3),
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_secs(5);
    hub.config.timers.heartbeat_ack = Duration::from_secs(5);
    let path = hub.config.data_dir.clone();
    let store = seed_backlog(store, &path, 80, 200_000);
    let journal_id = store.cj_journal_id().unwrap().to_string();
    let inbox = segment(&path.join("inbox"));
    let initial = fs::metadata(&inbox).unwrap().len();
    let (stop, state, join) = start(hub.config.clone(), store);
    loop {
        if let Observed::Control(message) = hub.event().await
            && matches!(message.msg, Some(control_up::Msg::CjRecords(_)))
        {
            break;
        }
    }
    sleep(Duration::from_millis(600)).await;
    hub.send(ControlDown {
        msg: Some(control_down::Msg::CjAck(JournalRangeAck {
            journal_id,
            up_to_seq: 64,
        })),
    })
    .await;
    sleep(Duration::from_millis(600)).await;
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(cancel_command())),
    })
    .await;
    timeout(Duration::from_millis(300), async {
        while fs::metadata(&inbox).unwrap().len() == initial {
            assert!(!matches!(*state.borrow(), SessionState::Backoff { .. }));
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop_worker(stop, join).await;
    assert_eq!(
        WorkerStore::open(&path)
            .unwrap()
            .cj_cursor()
            .unwrap()
            .last_seq,
        81
    );
}

#[tokio::test]
async fn partial_range_progress_resets_retry_budget_for_the_same_batch() {
    use canterlot_proto::worker::v1::{JournalRangeAck, control_up};
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        range_ack: false,
        ..Default::default()
    })
    .await;
    hub.config.timers.range_ack = Duration::from_millis(120);
    hub.config.timers.heartbeat_ack = Duration::from_millis(300);
    for _ in 0..3 {
        store.accept_command(&cancel_command()).unwrap();
    }
    let journal_id = store.cj_journal_id().unwrap().to_string();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    let mut sends = 0;
    for up_to_seq in 1..=3 {
        while sends < up_to_seq + 1 {
            assert!(!matches!(*state.borrow(), SessionState::Backoff { .. }));
            match hub.event().await {
                Observed::Hello(_, _) => assert_eq!(sends, 0),
                Observed::Data(_) => {}
                Observed::Control(message) => {
                    if let Some(control_up::Msg::CjRecords(batch)) = message.msg {
                        assert_eq!(batch.records.first().unwrap().seq, 1);
                        assert_eq!(batch.records.last().unwrap().seq, 3);
                        sends += 1;
                    }
                }
            }
        }
        sleep(Duration::from_millis(60)).await;
        hub.send(ControlDown {
            msg: Some(control_down::Msg::CjAck(JournalRangeAck {
                journal_id: journal_id.clone(),
                up_to_seq,
            })),
        })
        .await;
    }
    live_without_backoff(&mut state).await;
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn duplicate_range_acks_do_not_extend_the_deadline() {
    use canterlot_proto::worker::v1::JournalRangeAck;
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        range_ack: false,
        ..Default::default()
    })
    .await;
    store.accept_command(&cancel_command()).unwrap();
    let journal_id = store.cj_journal_id().unwrap().to_string();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    loop {
        if let Observed::Control(message) = hub.event().await
            && matches!(
                message.msg,
                Some(canterlot_proto::worker::v1::control_up::Msg::CjRecords(_))
            )
        {
            break;
        }
    }
    timeout(Duration::from_secs(1), async {
        loop {
            if matches!(*state.borrow_and_update(), SessionState::Backoff { .. }) {
                break;
            }
            sleep(Duration::from_millis(30)).await;
            if matches!(*state.borrow(), SessionState::Backoff { .. }) {
                break;
            }
            hub.send(ControlDown {
                msg: Some(control_down::Msg::CjAck(JournalRangeAck {
                    journal_id: journal_id.clone(),
                    up_to_seq: 0,
                })),
            })
            .await;
        }
    })
    .await
    .unwrap();
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn accepted_commands_are_routed_to_execution_manager() {
    use canterlot_proto::worker::v1::{
        DeliverInput, StartExecution, control_up, record_body::Body,
    };
    for enabled in [false, true] {
        let (mut hub, store) = ProtocolHub::start(Behavior {
            capabilities: if enabled {
                vec!["exec_v1".into()]
            } else {
                vec![]
            },
            ..Default::default()
        })
        .await;
        let path = hub.config.data_dir.clone();
        let (stop, mut state, join) = start(hub.config.clone(), store);
        wait_state(&mut state, SessionState::Live).await;
        let command = Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            execution_id: Uuid::now_v7().to_string(),
            execution_generation: 1,
            kind: Some(if enabled {
                command::Kind::DeliverInput(DeliverInput::default())
            } else {
                command::Kind::StartExecution(StartExecution {
                    conversation_id: Uuid::now_v7().to_string(),
                    harness: "synthetic".into(),
                    workdir: path.to_string_lossy().into_owned(),
                    native_conversation_id: String::new(),
                    instructions: Some(canterlot_proto::worker::v1::Instructions {
                        mode: "clean".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            }),
        };
        hub.send(ControlDown {
            msg: Some(control_down::Msg::Command(command.clone())),
        })
        .await;
        let mut facts = Vec::new();
        timeout(Duration::from_secs(3), async {
            while facts.len() < 2 {
                if let Observed::Control(message) = hub.event().await
                    && let Some(control_up::Msg::CjRecords(batch)) = message.msg
                {
                    facts.extend(
                        batch
                            .records
                            .into_iter()
                            .map(|r| r.body.unwrap().body.unwrap()),
                    );
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(&facts[0], Body::CommandStored(f) if f.command_id == command.command_id));
        if enabled {
            assert!(
                matches!(&facts[1], Body::DispatchDeferred(f) if f.reason == "generation_closed")
            );
        } else {
            assert!(
                matches!(&facts[1], Body::CommandRejected(f) if f.reason == "hub_capability_missing")
            );
        }
        stop_worker(stop, join).await;
        let store = WorkerStore::open(&path).unwrap();
        assert!(
            store
                .command_state(&command.command_id)
                .unwrap()
                .unwrap()
                .terminal
        );
        assert!(store.stored_commands().unwrap().is_empty());
        assert!(!path.join("executions").exists());
    }
}

#[path = "support/task8.rs"]
mod task8;

async fn pilot_worker() -> (
    ProtocolHub,
    watch::Sender<bool>,
    JoinHandle<Result<(), WorkerError>>,
    canterlot_worker::session::LimitsCache,
) {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into()],
        ..Default::default()
    })
    .await;
    hub.config.machine.metrics_interval = Duration::from_millis(45);
    let worker = Worker::new(hub.config.clone(), store)
        .with_runtime_dir(hub.config.data_dir.join("runtime"));
    let cache = worker.limits_cache();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    (hub, stop, join, cache)
}
async fn next_pilot(
    hub: &mut ProtocolHub,
    wanted: impl Fn(&canterlot_proto::worker::v1::control_up::Msg) -> bool,
) -> canterlot_proto::worker::v1::control_up::Msg {
    loop {
        if let Observed::Control(message) = hub.event().await
            && let Some(msg) = message.msg
            && wanted(&msg)
        {
            return msg;
        }
    }
}
#[path = "support/frozen_clock.rs"]
mod frozen_clock;

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use canterlot_proto::worker::v1::{MachineMetrics, control_up::Msg};
use canterlot_worker::machine::metrics::{ExecutionProbe, MetricsError, MetricsSource};
use frozen_clock::Timeline;

#[derive(Default)]
struct ScriptedMetrics {
    calls: AtomicU32,
    entered: tokio::sync::Notify,
    gate: Option<tokio::sync::Semaphore>,
}
impl ScriptedMetrics {
    fn instant() -> Arc<Self> {
        Arc::default()
    }
    fn held() -> Arc<Self> {
        Arc::new(Self {
            gate: Some(tokio::sync::Semaphore::new(0)),
            ..Default::default()
        })
    }
    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}
#[async_trait::async_trait]
impl MetricsSource for ScriptedMetrics {
    async fn sample(&self, _: Vec<ExecutionProbe>) -> Result<MachineMetrics, MetricsError> {
        let number = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.entered.notify_one();
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        Ok(MachineMetrics {
            machine_time_ms: 1,
            load1_centi: number,
            ..Default::default()
        })
    }
}

struct FrozenWorker {
    hub: ProtocolHub,
    state: watch::Receiver<SessionState>,
    stop: watch::Sender<bool>,
    join: JoinHandle<Result<(), WorkerError>>,
}
impl FrozenWorker {
    async fn start(pilot: bool, metrics_interval: Duration, metrics: Arc<ScriptedMetrics>) -> Self {
        let (mut hub, store) = ProtocolHub::start(Behavior {
            capabilities: if pilot {
                vec!["pilot_v1".into()]
            } else {
                vec![]
            },
            ..Default::default()
        })
        .await;
        hub.config.machine.metrics_interval = metrics_interval;
        let worker = Worker::new(hub.config.clone(), store)
            .with_runtime_dir(hub.config.data_dir.join("runtime"))
            .with_metrics_source(metrics);
        let state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        Self {
            hub,
            state,
            stop,
            join,
        }
    }
    async fn finish(self) {
        stop_worker(self.stop, self.join).await;
    }
    async fn control(&mut self) -> Msg {
        loop {
            if let Observed::Control(message) = self.hub.event().await {
                return message.msg.expect("control message has a body");
            }
        }
    }
    async fn machine_report(&mut self) {
        match self.control().await {
            Msg::MachineReport(_) => {}
            other => panic!("expected the machine report, got {other:?}"),
        }
    }
    async fn heartbeat(&mut self, seq: u64) {
        match self.control().await {
            Msg::Heartbeat(beat) => assert_eq!(beat.seq, seq),
            other => panic!("expected heartbeat {seq}, got {other:?}"),
        }
    }
    async fn sample(&mut self, number: u32) {
        match self.control().await {
            Msg::MachineMetrics(sample) => assert_eq!(sample.load1_centi, number),
            other => panic!("expected metrics sample {number}, got {other:?}"),
        }
    }
}

// Часы стоят, пока тест не сдвинет их сам: порядок и сроки не зависят от нагрузки машины.
#[tokio::test(start_paused = true)]
async fn machine_report_follows_accepted_only_when_pilot_capability_is_echoed() {
    frozen_clock::run(async {
        for pilot in [false, true] {
            let mut worker =
                FrozenWorker::start(pilot, Duration::from_secs(30), ScriptedMetrics::instant())
                    .await;
            let mut hellos = 0;
            while hellos < 2 {
                match worker.hub.event().await {
                    Observed::Hello(hello, _) => {
                        assert!(hello.optional_capabilities.contains(&"pilot_v1".into()));
                        hellos += 1;
                    }
                    Observed::Control(message) => {
                        panic!("{:?} before the handshake finished", message.msg)
                    }
                    Observed::Data(_) => {}
                }
            }
            wait_state(&mut worker.state, SessionState::Live).await;
            let timeline = Timeline::start();
            if pilot {
                worker.machine_report().await;
            }
            let heartbeat = worker.hub.config.timers.heartbeat;
            for seq in 1..=3 {
                timeline.at(heartbeat * seq as u32).await;
                worker.heartbeat(seq).await;
            }
            worker.finish().await;
        }
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn metrics_arrive_every_interval_without_delaying_heartbeats() {
    frozen_clock::run(async {
        let interval = Duration::from_millis(35);
        let metrics = ScriptedMetrics::instant();
        let mut worker = FrozenWorker::start(true, interval, metrics.clone()).await;
        worker.machine_report().await;
        let timeline = Timeline::start();
        let heartbeat = worker.hub.config.timers.heartbeat;
        let mut due: Vec<_> = (1..=5)
            .map(|k| (heartbeat * k, true))
            .chain((1..=4).map(|k| (interval * k, false)))
            .collect();
        due.sort();
        assert!(
            due.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "heartbeat and metrics deadlines must not coincide in the checked window"
        );
        let (mut beats, mut samples) = (0, 0);
        for (at, is_heartbeat) in due {
            timeline.at(at).await;
            if is_heartbeat {
                beats += 1;
                worker.heartbeat(beats).await;
            } else {
                samples += 1;
                worker.sample(samples).await;
            }
        }
        assert_eq!(metrics.calls(), 4);
        worker.finish().await;
    })
    .await;
}
#[tokio::test(start_paused = true)]
async fn a_slow_metrics_sample_neither_delays_heartbeats_nor_overlaps_the_next_one() {
    frozen_clock::run(async {
        let interval = Duration::from_millis(35);
        let metrics = ScriptedMetrics::held();
        let mut worker = FrozenWorker::start(true, interval, metrics.clone()).await;
        worker.machine_report().await;
        let timeline = Timeline::start();
        let heartbeat = worker.hub.config.timers.heartbeat;
        timeline.at(heartbeat).await;
        worker.heartbeat(1).await;
        timeline.at(interval).await;
        metrics.entered.notified().await;
        for seq in 2..=4 {
            timeline.at(heartbeat * seq as u32).await;
            worker.heartbeat(seq).await;
        }
        assert_eq!(metrics.calls(), 1);
        metrics.gate.as_ref().unwrap().add_permits(1);
        worker.sample(1).await;
        worker.finish().await;
    })
    .await;
}
#[tokio::test]
async fn full_control_queue_drops_metrics_not_heartbeats() {
    use canterlot_proto::worker::v1::{ControlUp, Heartbeat, MachineMetrics, control_up::Msg};
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    tx.try_send(ControlUp {
        msg: Some(Msg::Heartbeat(Heartbeat {
            seq: 1,
            machine_time_ms: 1,
        })),
    })
    .unwrap();
    assert!(!canterlot_worker::session::try_send_metrics(
        &tx,
        MachineMetrics::default()
    ));
    assert!(matches!(rx.recv().await.unwrap().msg,Some(Msg::Heartbeat(h)) if h.seq==1));
    tx.try_send(ControlUp {
        msg: Some(Msg::Heartbeat(Heartbeat {
            seq: 2,
            machine_time_ms: 2,
        })),
    })
    .unwrap();
    assert!(matches!(rx.recv().await.unwrap().msg,Some(Msg::Heartbeat(h)) if h.seq==2));
}
#[cfg(feature = "synthetic-adapter")]
#[tokio::test]
async fn poll_limits_request_is_answered_from_the_source() {
    use canterlot_proto::worker::v1::control_up::Msg;
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into()],
        ..Default::default()
    })
    .await;
    let dir = hub.config.data_dir.join("synthetic-limits");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("claude.json"),r#"{"five_hour":{"used_permille":3,"resets_at_ms":1800000000000},"weekly":{"used_permille":960}}"#).unwrap();
    let (stop, _, join) = start(hub.config.clone(), store);
    next_pilot(&mut hub, |m| matches!(m, Msg::MachineReport(_))).await;
    hub.poll_limits("claude").await;
    let Msg::LimitsReport(report) =
        next_pilot(&mut hub, |m| matches!(m, Msg::LimitsReport(_))).await
    else {
        panic!()
    };
    assert_eq!(report.observations.len(), 2);
    assert_eq!(report.observations[0].provider, "claude");
    assert_eq!(report.observations[0].used_permille, 3);
    assert!(report.observations[0].observed_machine_ms > 0);
    assert_eq!(report.observations[1].used_permille, 960);
    stop_worker(stop, join).await;
}
#[tokio::test]
async fn workdir_suggestions_are_answered_over_the_control_stream() {
    use canterlot_proto::worker::v1::{SuggestWorkdirs, control_up::Msg};
    assert!(canterlot_worker::session::OPTIONAL_CAPABILITIES.contains(&"workdir_suggest_v1"));
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into(), "workdir_suggest_v1".into()],
        ..Default::default()
    })
    .await;
    let root = hub.config.data_dir.join("root");
    fs::create_dir_all(root.join("project")).unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    hub.config.execution.allowed_workdir_roots = vec![root.clone()];
    let (stop, _, join) = start(hub.config.clone(), store);
    next_pilot(&mut hub, |m| matches!(m, Msg::MachineReport(_))).await;
    hub.send(ControlDown {
        msg: Some(control_down::Msg::SuggestWorkdirs(SuggestWorkdirs {
            request_id: "request".into(),
            path: format!("{}/pro", root.display()),
        })),
    })
    .await;
    let Msg::WorkdirSuggestions(reply) =
        next_pilot(&mut hub, |m| matches!(m, Msg::WorkdirSuggestions(_))).await
    else {
        panic!()
    };
    assert_eq!(reply.request_id, "request");
    assert_eq!(reply.directories, [format!("{}/project", root.display())]);
    stop_worker(stop, join).await;
}
#[tokio::test]
async fn path_probe_answers_with_the_same_protection_as_share_file() {
    use canterlot_proto::worker::v1::{
        ProbePath, control_up::Msg, path_probe::Outcome as ProbeReply,
    };
    assert!(canterlot_worker::session::OPTIONAL_CAPABILITIES.contains(&"path_probe_v1"));
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into(), "path_probe_v1".into()],
        ..Default::default()
    })
    .await;
    let root = hub.config.data_dir.join("root");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("gate.rc"), b"0\n").unwrap();
    fs::write(root.join(".env"), b"TOKEN=1").unwrap();
    hub.config.execution.allowed_workdir_roots = vec![root.clone()];
    let (stop, _, join) = start(hub.config.clone(), store);
    next_pilot(&mut hub, |m| matches!(m, Msg::MachineReport(_))).await;

    let ask = |request_id: &str, path: String, read_head_bytes: u32| ProbePath {
        request_id: request_id.into(),
        path,
        read_head_bytes,
    };
    hub.send(ControlDown {
        msg: Some(control_down::Msg::ProbePath(ask(
            "head",
            root.join("gate.rc").display().to_string(),
            256,
        ))),
    })
    .await;
    let Msg::PathProbe(reply) = next_pilot(&mut hub, |m| matches!(m, Msg::PathProbe(_))).await
    else {
        panic!()
    };
    assert_eq!(reply.request_id, "head");
    let Some(ProbeReply::Exists(exists)) = reply.outcome else {
        panic!("{reply:?}")
    };
    assert_eq!(exists.size, 2);
    assert_eq!(exists.head, "0\n");
    assert_eq!(exists.head_omitted, "");

    hub.send(ControlDown {
        msg: Some(control_down::Msg::ProbePath(ask(
            "secret",
            root.join(".env").display().to_string(),
            0,
        ))),
    })
    .await;
    let Msg::PathProbe(reply) = next_pilot(&mut hub, |m| matches!(m, Msg::PathProbe(_))).await
    else {
        panic!()
    };
    assert_eq!(reply.request_id, "secret");
    let Some(ProbeReply::NotAllowed(reason)) = reply.outcome else {
        panic!("{reply:?}")
    };
    assert_eq!(reason.reason, "denied");

    hub.send(ControlDown {
        msg: Some(control_down::Msg::ProbePath(ask(
            "missing",
            root.join("absent.rc").display().to_string(),
            0,
        ))),
    })
    .await;
    let Msg::PathProbe(reply) = next_pilot(&mut hub, |m| matches!(m, Msg::PathProbe(_))).await
    else {
        panic!()
    };
    assert!(matches!(reply.outcome, Some(ProbeReply::Missing(_))));
    stop_worker(stop, join).await;
}

/// С согласованной `resource_lease_v1` воркер шлёт полное состояние очереди при
/// подключении и после каждого изменения.
#[tokio::test]
async fn lease_state_is_sent_on_connect_and_on_change() {
    use canterlot_proto::worker::v1::control_up::Msg;
    assert!(canterlot_worker::session::OPTIONAL_CAPABILITIES.contains(&"resource_lease_v1"));
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into(), "resource_lease_v1".into()],
        ..Default::default()
    })
    .await;
    let socket = hub.config.data_dir.join("runtime/canterlot/lease.sock");
    let release = hub.config.data_dir.join("release");
    let (stop, _, join) = start(hub.config.clone(), store);
    let Msg::LeaseState(initial) = next_pilot(&mut hub, |m| matches!(m, Msg::LeaseState(_))).await
    else {
        panic!()
    };
    assert_eq!(initial.slots, 2);
    assert!(initial.holders.is_empty() && initial.waiters.is_empty());
    assert!(initial.observed_at_ms > 0);

    timeout(Duration::from_secs(10), async {
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut wrapper = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args([
            "lease",
            "run",
            "--kind",
            "gate",
            "--label",
            "hub mirror",
            "--",
        ])
        .args([
            "sh",
            "-c",
            &format!("while [ ! -e {release:?} ]; do sleep 0.05; done"),
        ])
        .env("CANTERLOT_LEASE_SOCKET", &socket)
        .env_remove("CANTERLOT_LEASE_HELD")
        .env(
            "CANTERLOT_EXECUTION_GROUP",
            "0190a000-0000-7000-8000-000000000001",
        )
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let Msg::LeaseState(held) = next_pilot(
        &mut hub,
        |m| matches!(m, Msg::LeaseState(state) if !state.holders.is_empty()),
    )
    .await
    else {
        panic!()
    };
    let holder = &held.holders[0];
    assert_eq!(holder.kind, "gate");
    assert_eq!(holder.weight, 2);
    assert_eq!(holder.label, "hub mirror");
    assert_eq!(holder.holder, "execution");
    assert_eq!(holder.execution_id, "0190a000-0000-7000-8000-000000000001");
    assert!(holder.since_ms > 0);
    fs::write(&release, b"").unwrap();
    assert!(wrapper.wait().await.unwrap().success());
    let Msg::LeaseState(empty) = next_pilot(
        &mut hub,
        |m| matches!(m, Msg::LeaseState(state) if state.holders.is_empty()),
    )
    .await
    else {
        panic!()
    };
    assert!(empty.waiters.is_empty());
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn concurrent_path_probes_are_all_answered_or_explicitly_busy() {
    use canterlot_proto::worker::v1::{
        ProbePath, control_up::Msg, path_probe::Outcome as ProbeReply,
    };
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into(), "path_probe_v1".into()],
        ..Default::default()
    })
    .await;
    let root = hub.config.data_dir.join("root");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("gate.rc"), b"0\n").unwrap();
    hub.config.execution.allowed_workdir_roots = vec![root.clone()];
    let (stop, _, join) = start(hub.config.clone(), store);
    next_pilot(&mut hub, |m| matches!(m, Msg::MachineReport(_))).await;

    let mut asked = Vec::new();
    for index in 0..12 {
        let request_id = format!("probe-{index}");
        asked.push(request_id.clone());
        hub.send(ControlDown {
            msg: Some(control_down::Msg::ProbePath(ProbePath {
                request_id,
                path: root.join("gate.rc").display().to_string(),
                read_head_bytes: 0,
            })),
        })
        .await;
    }
    let mut answered = Vec::new();
    for _ in 0..asked.len() {
        let Msg::PathProbe(reply) = next_pilot(&mut hub, |m| matches!(m, Msg::PathProbe(_))).await
        else {
            panic!()
        };
        assert!(
            matches!(
                reply.outcome,
                Some(ProbeReply::Exists(_) | ProbeReply::Error(_))
            ),
            "every probe gets a result or an explicit busy error: {}",
            reply.request_id
        );
        answered.push(reply.request_id);
    }
    asked.sort();
    answered.sort();
    assert_eq!(answered, asked, "no request is dropped");
    stop_worker(stop, join).await;
}

#[tokio::test(start_paused = true)]
async fn report_is_resent_after_reconnect_and_metrics_resume() {
    frozen_clock::run(async {
        let interval = Duration::from_millis(35);
        let metrics = ScriptedMetrics::instant();
        let mut worker = FrozenWorker::start(true, interval, metrics.clone()).await;
        let heartbeat = worker.hub.config.timers.heartbeat;
        worker.machine_report().await;
        let first = Timeline::start();
        first.at(heartbeat).await;
        worker.heartbeat(1).await;
        first.at(interval).await;
        worker.sample(1).await;
        worker.hub.fail(tonic::Status::unavailable("outage")).await;
        wait_backoff(&mut worker.state).await;
        tokio::time::advance(worker.hub.config.timers.backoff_max).await;
        let mut hellos = 0;
        loop {
            match worker.hub.event().await {
                Observed::Hello(..) => hellos += 1,
                Observed::Control(message) => match message.msg {
                    Some(Msg::MachineReport(_)) => break,
                    other => panic!("{other:?} before the machine report"),
                },
                Observed::Data(_) => {}
            }
        }
        assert_eq!(hellos, 2);
        let second = Timeline::start();
        second.at(heartbeat).await;
        worker.heartbeat(1).await;
        second.at(interval).await;
        worker.sample(2).await;
        second.at(heartbeat * 2).await;
        worker.heartbeat(2).await;
        second.at(interval * 2).await;
        worker.sample(3).await;
        worker.finish().await;
    })
    .await;
}
#[tokio::test]
async fn limits_snapshot_is_cached_volatile_and_replaced_by_the_latest() {
    use canterlot_proto::worker::v1::{LimitObservation, LimitsSnapshot, control_up::Msg};
    let (mut hub, stop, join, cache) = pilot_worker().await;
    next_pilot(&mut hub, |m| matches!(m, Msg::MachineReport(_))).await;
    for used in [960, 40] {
        hub.send(ControlDown {
            msg: Some(control_down::Msg::LimitsSnapshot(LimitsSnapshot {
                provider: "claude".into(),
                current: vec![LimitObservation {
                    used_permille: used,
                    ..Default::default()
                }],
                weekly_reserve_active: used == 960,
            })),
        })
        .await;
        timeout(Duration::from_secs(2), async {
            loop {
                if cache.get("claude").is_some_and(|s| {
                    s.observations[0].used_permille == used
                        && s.weekly_reserve_active == (used == 960)
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert!(cache.get("codex").is_none());
    stop_worker(stop, join).await;
    let empty = canterlot_worker::session::LimitsCache::default();
    assert!(empty.get("claude").is_none());
}
#[cfg(feature = "synthetic-adapter")]
#[tokio::test]
async fn journal_with_pilot_records_is_withheld_from_a_session_without_pilot_v1_and_others_keep_shipping()
 {
    use canterlot_proto::worker::v1::{
        ContentBlock, DeliverInput, LimitObservation, StartExecution, TextBlock, control_up::Msg,
        data_up, record_body::Body,
    };
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        capabilities: vec!["exec_v1".into()],
        ..Default::default()
    })
    .await;
    let _guard = task8::process_guard::ProcessGuard::new(&hub.config.data_dir);
    hub.config.execution.allowed_workdir_roots = vec![hub.config.data_dir.clone()];
    hub.config.execution.supervisor_exe = Some(env!("CARGO_BIN_EXE_canterlot-worker").into());
    let journal = store.cj_journal_id().unwrap().to_string();
    store
        .append_cj_fact(Body::LimitObserved(LimitObservation {
            provider: "claude".into(),
            used_permille: 960,
            ..Default::default()
        }))
        .unwrap();
    assert!(
        store
            .journal_next_requires_pilot(Uuid::parse_str(&journal).unwrap())
            .unwrap()
    );
    let (stop, _, join) = start(hub.config.clone(), store);
    next_pilot(&mut hub, |m| matches!(m, Msg::ReconcileReport(_))).await;
    let execution = Uuid::now_v7().to_string();
    let conversation = Uuid::now_v7().to_string();
    let cmd = |kind| Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        execution_id: execution.clone(),
        execution_generation: 1,
        kind: Some(kind),
    };
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(cmd(
            command::Kind::StartExecution(StartExecution {
                conversation_id: conversation.clone(),
                harness: "synthetic".into(),
                workdir: hub.config.data_dir.to_string_lossy().into_owned(),
                instructions: Some(canterlot_proto::worker::v1::Instructions {
                    mode: "clean".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ))),
    })
    .await;
    let deliver = cmd(command::Kind::DeliverInput(DeliverInput {
        input_id: Uuid::now_v7().to_string(),
        conversation_id: conversation,
        content: vec![ContentBlock {
            block: Some(canterlot_proto::worker::v1::content_block::Block::Text(
                TextBlock {
                    text: "answer".into(),
                },
            )),
        }],
        ..Default::default()
    }));
    let input_id = deliver.command_id.clone();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(deliver)),
    })
    .await;
    let mut beats = 0;
    let mut finished = false;
    while beats < 4 || !finished {
        match hub.event().await {
            Observed::Hello(h, _) => assert!(!h.cursors.iter().any(|c| c.journal_id == journal)),
            Observed::Control(m) => match m.msg {
                Some(Msg::CjRecords(b)) => assert_ne!(b.journal_id, journal),
                Some(Msg::Heartbeat(_)) => beats += 1,
                _ => {}
            },
            Observed::Data(m) => {
                if let Some(data_up::Msg::SjRecords(b)) = m.msg {
                    assert_ne!(b.journal_id, journal);
                    finished|=b.records.iter().any(|r|matches!(r.body.as_ref().and_then(|b|b.body.as_ref()),Some(Body::RunFinished(f)) if f.input_command_id==input_id));
                }
            }
        }
    }
    stop_worker(stop, join).await;
}
#[tokio::test]
async fn acked_cj_prefix_does_not_diverge_when_the_journal_is_withheld() {
    use canterlot_proto::worker::v1::{
        InputTransmissionStarted, LimitObservation, control_up::Msg, record_body::Body,
    };
    let (mut hub, mut store) = ProtocolHub::start(Behavior::default()).await;
    store
        .append_cj_fact(Body::TransmissionStarted(InputTransmissionStarted {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            machine_time_ms: 1,
        }))
        .unwrap();
    store.on_cj_ack(1).unwrap();
    store
        .append_cj_fact(Body::LimitObserved(LimitObservation {
            provider: "claude".into(),
            ..Default::default()
        }))
        .unwrap();
    let journal = store.cj_journal_id().unwrap().to_string();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let mut beats = 0;
    while beats < 2 {
        match hub.event().await {
            Observed::Control(m) => match m.msg {
                Some(Msg::CjRecords(b)) => assert_ne!(b.journal_id, journal),
                Some(Msg::Heartbeat(_)) => beats += 1,
                _ => {}
            },
            Observed::Hello(h, _) => assert!(!h.cursors.iter().any(|c| c.journal_id == journal)),
            _ => {}
        }
    }
    stop_worker(stop, join).await;
}
#[tokio::test]
async fn hello_and_heartbeat_carry_the_machine_clock() {
    use canterlot_proto::worker::v1::control_up::Msg;
    let (mut hub, stop, join, _) = pilot_worker().await;
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };
    let mut hellos = 0;
    loop {
        match hub.event().await {
            Observed::Hello(h, _) => {
                hellos += 1;
                assert!(h.machine_time_ms > now() - 5000 && h.machine_time_ms <= now() + 1000)
            }
            Observed::Control(m) => {
                if let Some(Msg::Heartbeat(h)) = m.msg {
                    assert!(h.machine_time_ms > now() - 5000 && h.machine_time_ms <= now() + 1000);
                    break;
                }
            }
            _ => {}
        }
    }
    assert_eq!(hellos, 2);
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn machine_watch_goes_out_at_once_when_free_disk_crosses_the_warn_threshold() {
    use canterlot_proto::worker::v1::control_up::Msg;
    assert!(canterlot_worker::session::OPTIONAL_CAPABILITIES.contains(&"watchdog_v1"));
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["pilot_v1".into(), "watchdog_v1".into()],
        ..Default::default()
    })
    .await;
    hub.config.machine.metrics_interval = Duration::from_millis(45);
    // Любой диск ниже порога: первая же выборка метрик — пересечение вниз.
    hub.config.machine.disk_warn_free_gb = u64::MAX / 1_000_000_000;
    // Домашний каталог тест не обходит; поток замеров и так ждёт минуту.
    hub.config.machine.hotspot_roots = Some(Vec::new());
    let (stop, _, join) = start(hub.config.clone(), store);
    let started = std::time::Instant::now();
    let Msg::MachineWatch(watch) = tokio::time::timeout(
        Duration::from_secs(20),
        next_pilot(&mut hub, |m| matches!(m, Msg::MachineWatch(_))),
    )
    .await
    .expect("watch before the five-minute cadence") else {
        panic!()
    };
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(watch.observed_at_ms > 0);
    assert!(watch.hotspots.is_empty(), "sizes are not measured yet");
    assert!(
        watch.leaked.is_empty(),
        "no released executions in a fresh store"
    );
    stop_worker(stop, join).await;
}
