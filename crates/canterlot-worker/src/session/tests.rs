use super::*;

#[allow(dead_code)]
#[path = "../../tests/support/mod.rs"]
pub(super) mod support;

#[test]
fn disabled_antigravity_is_not_probed_or_retried() {
    let config: WorkerConfig = toml::from_str(&format!(
        r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "{}"
installation_id = "{}"
restore_generation = "{}"
data_dir = "/tmp/store"

[opencode]
executable = "/bin/false"

[antigravity]
enabled = false
"#,
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    ))
    .unwrap();
    let probes = catalog::Probe::configured(&config);
    assert!(
        matches!(probes.as_slice(), [catalog::Probe::OpenCode(_)]),
        "a disabled harness must stay out of probes"
    );
}

#[test]
fn test_runtime_dir_stays_inside_storage() {
    let root = tempfile::tempdir().unwrap();
    let runtime = default_runtime_dir(&root.path().join("store")).unwrap();
    assert!(runtime.starts_with(root.path()));
}

#[test]
fn hub_status_preserves_code_and_static_message() {
    for code in [
        tonic::Code::PermissionDenied,
        tonic::Code::InvalidArgument,
        tonic::Code::Unauthenticated,
    ] {
        let error = status_error(Status::new(code, "foreign_journal"));
        assert!(
            matches!(error, WorkerError::Hub { code: actual, message } if actual == code && message == "foreign_journal")
        );
    }
}

#[derive(Clone, Default)]
struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Хаб держит стоп, пока его не обновят: повторы идут, но причина должна быть видна и не тонуть в них.
#[tokio::test(flavor = "current_thread")]
async fn hub_refusal_of_a_journal_record_is_logged_once_with_its_reason() {
    use tracing::instrument::WithSubscriber;
    let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
        status: Some((tonic::Code::InvalidArgument, "unknown_record")),
        ..Default::default()
    })
    .await;
    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let worker = Worker::new(hub.config.clone(), store);
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown).with_subscriber(subscriber));
    for _ in 0..3 {
        timeout(Duration::from_secs(3), async {
            while !matches!(hub.event().await, support::Observed::Hello(..)) {}
        })
        .await
        .unwrap();
    }
    stop.send_replace(true);
    join.await.unwrap().unwrap();
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        logs.matches("kind=\"journal_refused\"").count(),
        1,
        "{logs}"
    );
    assert!(logs.contains("reason=unknown_record"), "{logs}");
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_store_does_not_delay_inbound_or_heartbeat_timers() {
    let (mut hub, store) = support::ProtocolHub::start(support::Behavior::default()).await;
    let worker = Worker::new(hub.config.clone(), store);
    let mut state = worker.state();
    let async_thread = std::thread::current().id();
    let delay = worker.store.call(move |_| {
        assert_ne!(std::thread::current().id(), async_thread);
        std::thread::sleep(Duration::from_millis(350));
        Ok(())
    });
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    timeout(Duration::from_secs(3), async {
        while *state.borrow_and_update() != SessionState::Live {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let delay = tokio::spawn(delay);
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: Uuid::now_v7().to_string(),
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
            ..Default::default()
        })),
    })
    .await;
    let mut last = 0;
    let started = Instant::now();
    timeout(Duration::from_secs(3), async {
        while started.elapsed() < Duration::from_millis(400) {
            assert!(!matches!(*state.borrow(), SessionState::Backoff { .. }));
            if let support::Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::Heartbeat(h)) = message.msg
            {
                assert!(h.seq > last, "heartbeat ACK processing stalled");
                last = h.seq;
            }
        }
    })
    .await
    .unwrap();
    assert!(last >= 5);
    delay.await.unwrap().unwrap();
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}

#[allow(dead_code)]
#[path = "../../tests/support/frozen_clock.rs"]
mod frozen_clock;

struct WaitingWorker {
    hub: support::ProtocolHub,
    state: watch::Receiver<SessionState>,
    stop: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), WorkerError>>,
    client: Option<crate::session_store::StoreClient>,
    logs: Logs,
}

impl WaitingWorker {
    async fn start(backoff: Duration, fail: bool) -> Self {
        use tracing::instrument::WithSubscriber;

        let (hub, mut store) = support::ProtocolHub::start(support::Behavior::default()).await;
        let mut hub = hub;
        initialize_generation(&mut store, hub.config.restore_generation).unwrap();
        if fail {
            store.fail_next_write_with_disk_full();
        }
        hub.config.timers.backoff_min = backoff;
        hub.config.timers.backoff_max = backoff;
        let worker = Worker::new(hub.config.clone(), store);
        let state = worker.state();
        let client = worker.store.client();
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown).with_subscriber(subscriber));
        Self {
            hub,
            state,
            stop,
            join,
            client: Some(client),
            logs,
        }
    }

    async fn wait(&mut self, wanted: impl Fn(&SessionState) -> bool) {
        loop {
            {
                let state = self.state.borrow_and_update();
                if wanted(&state) {
                    return;
                }
            }
            self.state.changed().await.unwrap();
        }
    }

    async fn wait_live(&mut self) {
        self.wait(|state| *state == SessionState::Live).await;
    }

    async fn wait_storage_wait(&mut self) {
        self.wait(|state| matches!(state, SessionState::Backoff { .. }))
            .await;
    }

    fn log_text(&self) -> String {
        String::from_utf8(self.logs.0.lock().unwrap().clone()).unwrap()
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.hub.config.data_dir.clone()
    }

    async fn finish(mut self) -> Result<(), WorkerError> {
        self.client.take();
        self.stop.send_replace(true);
        self.join.await.unwrap()
    }

    async fn finish_restart(mut self) -> (Result<(), WorkerError>, String) {
        let logs = self.logs.clone();
        self.client.take();
        let result = self.join.await.unwrap();
        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        (result, text)
    }
}

async fn send_cancel(hub: &support::ProtocolHub) {
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            execution_id: Uuid::now_v7().to_string(),
            execution_generation: 1,
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
        })),
    })
    .await;
}

fn make_unwritable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
}

fn make_writable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_enospc_store_write_waits_for_storage_without_reconnecting() {
    frozen_clock::run(async {
        let mut worker = WaitingWorker::start(Duration::from_millis(100), true).await;
        worker.wait_live().await;
        let data_dir = worker.data_dir();
        make_unwritable(&data_dir);
        send_cancel(&worker.hub).await;
        worker.wait_storage_wait().await;
        assert_eq!(
            worker.log_text().matches("kind=\"disk_full\"").count(),
            1,
            "{}",
            worker.log_text()
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert!(matches!(
            *worker.state.borrow(),
            SessionState::Backoff { .. }
        ));
        assert!(!worker.join.is_finished());
        make_writable(&data_dir);
        worker.finish().await.unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn storage_wait_keeps_waiting_while_probes_keep_failing() {
    frozen_clock::run(async {
        let mut worker = WaitingWorker::start(Duration::from_millis(100), true).await;
        worker.wait_live().await;
        let data_dir = worker.data_dir();
        make_unwritable(&data_dir);
        send_cancel(&worker.hub).await;
        worker.wait_storage_wait().await;
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert!(matches!(
                *worker.state.borrow(),
                SessionState::Backoff { .. }
            ));
            assert!(!worker.join.is_finished());
        }
        make_writable(&data_dir);
        worker.finish().await.unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn storage_recovery_returns_the_restart_exit() {
    frozen_clock::run(async {
        let mut worker = WaitingWorker::start(Duration::from_millis(100), true).await;
        worker.wait_live().await;
        send_cancel(&worker.hub).await;
        worker.wait_storage_wait().await;
        assert_eq!(
            worker.log_text().matches("kind=\"disk_full\"").count(),
            1,
            "{}",
            worker.log_text()
        );
        tokio::time::advance(Duration::from_millis(100)).await;
        let (result, logs) = worker.finish_restart().await;
        let error = result.unwrap_err();
        assert!(matches!(error, WorkerError::StorageRestart));
        assert_eq!(error.exit_code(), Some(STORAGE_RESTART_EXIT_CODE));
        assert_eq!(logs.matches("kind=\"disk_recovered\"").count(), 1, "{logs}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_during_the_storage_wait_exits_normally() {
    frozen_clock::run(async {
        let mut worker = WaitingWorker::start(Duration::from_millis(100), true).await;
        worker.wait_live().await;
        let data_dir = worker.data_dir();
        make_unwritable(&data_dir);
        send_cancel(&worker.hub).await;
        worker.wait_storage_wait().await;
        make_writable(&data_dir);
        worker.finish().await.unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_command_against_a_closed_store_slot_enters_storage_wait_without_panicking() {
    frozen_clock::run(async {
        let mut worker = WaitingWorker::start(Duration::from_millis(100), false).await;
        worker.wait_live().await;
        worker
            .client
            .as_ref()
            .unwrap()
            .call(|slot| {
                slot.take();
                Ok(())
            })
            .await
            .unwrap();
        send_cancel(&worker.hub).await;
        worker.wait_storage_wait().await;
        assert!(matches!(
            *worker.state.borrow(),
            SessionState::Backoff { .. }
        ));
        worker.finish().await.unwrap();
    })
    .await;
}

async fn wait_live(state: &mut watch::Receiver<SessionState>) {
    timeout(Duration::from_secs(8), async {
        while *state.borrow_and_update() != SessionState::Live {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn wait_backoff(state: &mut watch::Receiver<SessionState>) {
    timeout(Duration::from_secs(8), async {
        while !matches!(*state.borrow_and_update(), SessionState::Backoff { .. }) {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

fn cancel_command() -> (Command, String, String) {
    let target = Uuid::now_v7().to_string();
    let command = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        execution_id: Uuid::now_v7().to_string(),
        execution_generation: 1,
        kind: Some(command::Kind::CancelInput(CancelInput {
            target_command_id: target.clone(),
            target_dispatch_attempt: 1,
        })),
    };
    let command_id = command.command_id.clone();
    (command, command_id, target)
}

fn seed_stored_cancels(store: &mut WorkerStore, count: usize) -> Vec<String> {
    let mut targets = Vec::with_capacity(count);
    for _ in 0..count {
        let (command, _, target) = cancel_command();
        assert!(matches!(
            store.accept_command(&command).unwrap(),
            Accept::Stored { .. }
        ));
        targets.push(target);
    }
    targets
}

async fn collect_cancel_wons(
    hub: &mut support::ProtocolHub,
    count: usize,
    gate: &tokio::sync::Notify,
) -> Vec<String> {
    let mut seen = Vec::with_capacity(count);
    let result = timeout(Duration::from_secs(15), async {
        while seen.len() < count {
            // Тестовый шлагбаум открывает по одной передаче, как и боевой слот.
            gate.notify_one();
            if let support::Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::CjRecords(batch)) = message.msg
            {
                for record in batch.records {
                    if let Some(record_body::Body::CancelWon(fact)) =
                        record.body.and_then(|body| body.body)
                    {
                        seen.push(fact.command_id);
                    }
                }
            }
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "only {} of {count} cancel outcomes arrived: {seen:?}",
            seen.len()
        );
    }
    seen
}

async fn wait_command_stored(hub: &mut support::ProtocolHub, command_id: &str) {
    timeout(Duration::from_secs(8), async {
        loop {
            if let support::Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::CjRecords(batch)) = message.msg
                && batch.records.iter().any(|record| {
                    matches!(
                        record.body.as_ref().and_then(|body| body.body.as_ref()),
                        Some(record_body::Body::CommandStored(fact))
                            if fact.command_id == command_id
                    )
                })
            {
                return;
            }
        }
    })
    .await
    .unwrap();
}

async fn wait_cancel_won(hub: &mut support::ProtocolHub, target: &str) {
    timeout(Duration::from_secs(8), async {
        loop {
            if let support::Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::CjRecords(batch)) = message.msg
                && batch.records.iter().any(|record| {
                    matches!(
                        record.body.as_ref().and_then(|body| body.body.as_ref()),
                        Some(record_body::Body::CancelWon(fact))
                            if fact.command_id == target
                    )
                })
            {
                return;
            }
        }
    })
    .await
    .unwrap();
}

// Команда принята в хранилище и подтверждена хабу, но сеанс оборвался до передачи
// её исполнению: после переподключения команда должна дойти до исполнения.
#[tokio::test]
async fn stored_but_unhanded_command_is_routed_after_reconnect() {
    let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".to_owned()],
        ..Default::default()
    })
    .await;
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut worker = Worker::new(hub.config.clone(), store);
    worker.command_gate = Some(gate.clone());
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    wait_live(&mut state).await;
    let (command, command_id, target) = cancel_command();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(command)),
    })
    .await;
    wait_command_stored(&mut hub, &command_id).await;
    hub.fail(Status::unavailable("drop")).await;
    wait_backoff(&mut state).await;
    wait_live(&mut state).await;
    gate.notify_one();
    wait_cancel_won(&mut hub, &target).await;
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}

// Повторная отдача принятой команды идёт через обычный слот исполнения:
// пока передача ждёт, пульс и приём журнала не останавливаются.
#[tokio::test]
async fn replay_does_not_block_the_session_loop_while_handoff_pends() {
    let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".to_owned()],
        ..Default::default()
    })
    .await;
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut worker = Worker::new(hub.config.clone(), store);
    worker.command_gate = Some(gate.clone());
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    wait_live(&mut state).await;
    let (command, command_id, _) = cancel_command();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(command)),
    })
    .await;
    wait_command_stored(&mut hub, &command_id).await;
    let mut heartbeats = 0;
    timeout(Duration::from_secs(5), async {
        while heartbeats < 5 {
            if let support::Observed::Control(message) = hub.event().await
                && let Some(control_up::Msg::Heartbeat(_)) = message.msg
            {
                heartbeats += 1;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(*state.borrow(), SessionState::Live);
    gate.notify_one();
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}

// Накопленная очередь доигрывания не должна закрывать приём входящего потока:
// иначе пульс перестаёт подтверждаться и попытка рвётся на том же backlog.
#[tokio::test]
async fn a_replay_backlog_does_not_stop_heartbeats() {
    let (mut hub, mut store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".to_owned()],
        ..Default::default()
    })
    .await;
    let _targets = seed_stored_cancels(&mut store, 40);
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut worker = Worker::new(hub.config.clone(), store);
    worker.command_gate = Some(gate.clone());
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    wait_live(&mut state).await;
    let mut heartbeats = 0;
    timeout(Duration::from_secs(5), async {
        while heartbeats < 5 {
            tokio::select! {
                event = hub.event() => {
                    if let support::Observed::Control(message) = event
                        && let Some(control_up::Msg::Heartbeat(_)) = message.msg
                    {
                        heartbeats += 1;
                    }
                }
                changed = state.changed() => {
                    changed.unwrap();
                    assert_eq!(
                        *state.borrow_and_update(),
                        SessionState::Live,
                        "the replay backlog must not reconnect the session"
                    );
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(*state.borrow(), SessionState::Live);
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}

// После переподключения большой backlog доигрывается по одной команде
// в порядке inbox, и каждая команда доходит до исполнения ровно один раз.
#[tokio::test]
async fn a_replay_backlog_is_handed_over_in_inbox_order() {
    let (mut hub, mut store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".to_owned()],
        ..Default::default()
    })
    .await;
    let targets = seed_stored_cancels(&mut store, 40);
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    let mut worker = Worker::new(hub.config.clone(), store);
    worker.command_gate = Some(gate.clone());
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    wait_live(&mut state).await;
    hub.fail(Status::unavailable("drop")).await;
    wait_backoff(&mut state).await;
    wait_live(&mut state).await;
    let delivered = collect_cancel_wons(&mut hub, targets.len(), &gate).await;
    assert_eq!(delivered, targets, "inbox order, each command once");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut extra = 0;
    while let Ok(support::Observed::Control(message)) = hub.events.try_recv() {
        if let Some(control_up::Msg::CjRecords(batch)) = message.msg {
            extra += batch
                .records
                .iter()
                .filter(|record| {
                    matches!(
                        record.body.as_ref().and_then(|body| body.body.as_ref()),
                        Some(record_body::Body::CancelWon(_))
                    )
                })
                .count();
        }
    }
    assert_eq!(extra, 0, "no command outcome may be repeated");
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}

// Команда, уже отданная исполнению, после переподключения не исполняется
// дважды: повторная отдача пропущенных команд не должна дублировать итог.
#[tokio::test]
async fn handled_command_is_not_routed_twice_after_reconnect() {
    let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".to_owned()],
        ..Default::default()
    })
    .await;
    let worker = Worker::new(hub.config.clone(), store);
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    wait_live(&mut state).await;
    let (command, _, target) = cancel_command();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(command)),
    })
    .await;
    wait_cancel_won(&mut hub, &target).await;
    hub.fail(Status::unavailable("drop")).await;
    wait_backoff(&mut state).await;
    wait_live(&mut state).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut won = 0;
    while let Ok(event) = hub.events.try_recv() {
        if let support::Observed::Control(message) = event
            && let Some(control_up::Msg::CjRecords(batch)) = message.msg
        {
            won += batch
                .records
                .iter()
                .filter(|record| {
                    matches!(
                        record.body.as_ref().and_then(|body| body.body.as_ref()),
                        Some(record_body::Body::CancelWon(fact))
                            if fact.command_id == target
                    )
                })
                .count();
        }
    }
    assert_eq!(won, 1, "command outcome must be delivered exactly once");
    stop.send_replace(true);
    join.await.unwrap().unwrap();
}
