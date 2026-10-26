use super::*;

#[allow(dead_code)]
#[path = "../../tests/support/mod.rs"]
pub(super) mod support;

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
