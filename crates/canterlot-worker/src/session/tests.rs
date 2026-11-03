use super::*;

#[allow(dead_code)]
#[path = "../../tests/support/mod.rs"]
pub(super) mod support;

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
