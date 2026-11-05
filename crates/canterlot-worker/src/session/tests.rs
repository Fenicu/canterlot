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

#[tokio::test(flavor = "current_thread")]
async fn an_enospc_store_write_ends_the_attempt_and_the_worker_reopens_and_continues() {
    use tracing::instrument::WithSubscriber;

    let (mut hub, mut store) = support::ProtocolHub::start(support::Behavior::default()).await;
    initialize_generation(&mut store, hub.config.restore_generation).unwrap();
    store.fail_next_write_with_disk_full();
    hub.config.timers.backoff_min = Duration::from_secs(2);
    hub.config.timers.backoff_max = Duration::from_secs(2);
    let worker = Worker::new(hub.config.clone(), store);
    let client = worker.store.client();
    let mut state = worker.state();
    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown).with_subscriber(subscriber));
    timeout(Duration::from_secs(3), async {
        loop {
            if *state.borrow_and_update() == SessionState::Live {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

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
    timeout(Duration::from_secs(3), async {
        loop {
            if matches!(*state.borrow_and_update(), SessionState::Backoff { .. }) {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let closed = client
        .call(|slot| {
            crate::session_store::store_mut(slot)?.cj_cursor()?;
            Ok(())
        })
        .await;
    assert!(
        matches!(closed, Err(WorkerError::Store(StoreError::NeedsReopen))),
        "{closed:?}"
    );

    timeout(Duration::from_secs(8), async {
        loop {
            if *state.borrow_and_update() == SessionState::Live {
                break;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let second = Uuid::now_v7();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: second.to_string(),
            dispatch_attempt: 1,
            execution_id: Uuid::now_v7().to_string(),
            execution_generation: 1,
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
        })),
    })
    .await;
    timeout(Duration::from_secs(3), async {
        loop {
            let id = second.to_string();
            let stored = client
                .call(move |slot| Ok(crate::session_store::store_mut(slot)?.command_state(&id)?))
                .await
                .unwrap();
            if stored.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    drop(client);
    stop.send_replace(true);
    join.await.unwrap().unwrap();
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert_eq!(logs.matches("kind=\"disk_full\"").count(), 1, "{logs}");
    assert_eq!(logs.matches("kind=\"disk_recovered\"").count(), 1, "{logs}");
}
