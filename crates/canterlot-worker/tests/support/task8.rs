use super::*;
use canterlot_proto::worker::v1::{StartExecution, control_up, record_body::Body};

#[tokio::test]
async fn unexpected_data_ack_reattaches_without_repeating_reconcile_report() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["exec_v1".into()],
        ..Default::default()
    })
    .await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    hub.send_data(canterlot_proto::worker::v1::DataDown {
        msg: Some(canterlot_proto::worker::v1::data_down::Msg::SjAck(
            canterlot_proto::worker::v1::JournalRangeAck {
                journal_id: Uuid::now_v7().to_string(),
                up_to_seq: 1,
            },
        )),
    })
    .await;
    wait_state(&mut state, SessionState::ControlOnly).await;
    wait_state(&mut state, SessionState::Live).await;
    let mut hellos = Vec::new();
    let mut reports = 0;
    while let Ok(event) = hub.events.try_recv() {
        match event {
            Observed::Hello(hello, _) => hellos.push(hello),
            Observed::Control(message) => {
                if matches!(message.msg, Some(control_up::Msg::ReconcileReport(_))) {
                    reports += 1;
                }
            }
            Observed::Data(message) => assert!(matches!(
                message.msg,
                Some(canterlot_proto::worker::v1::data_up::Msg::SjRecords(_))
            )),
        }
    }
    assert_eq!(hellos.iter().filter(|h| h.stream_kind == 1).count(), 1);
    assert_eq!(hellos.iter().filter(|h| h.stream_kind == 2).count(), 2);
    assert!(
        hellos
            .iter()
            .all(|h| h.connection_id == hellos[0].connection_id)
    );
    assert_eq!(reports, 1);
    stop_worker(stop, join).await;
}

#[tokio::test]
async fn worker_without_exec_capability_echo_rejects_start() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let Observed::Hello(hello, _) = hub.event().await else {
        panic!("missing hello")
    };
    assert_eq!(hello.optional_capabilities, ["exec_v1"]);
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(start_command(
            Uuid::now_v7(),
            Uuid::now_v7(),
            &hub.config.data_dir,
        ))),
    })
    .await;
    loop {
        if let Observed::Control(message) = hub.event().await
            && let Some(control_up::Msg::CjRecords(batch)) = message.msg
            && batch.records.iter().any(|r| matches!(r.body.as_ref().and_then(|b| b.body.as_ref()), Some(Body::CommandRejected(f)) if f.reason == "hub_capability_missing")) { break; }
    }
    stop_worker(stop, join).await;
    assert!(!hub.config.data_dir.join("executions").exists());
}

fn start_command(execution: Uuid, conversation: Uuid, work: &Path) -> Command {
    Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        execution_id: execution.to_string(),
        execution_generation: 1,
        kind: Some(command::Kind::StartExecution(StartExecution {
            conversation_id: conversation.to_string(),
            harness: "synthetic".into(),
            workdir: work.to_string_lossy().into_owned(),
            native_conversation_id: String::new(),
            ..Default::default()
        })),
    }
}

#[tokio::test]
async fn reconcile_waits_for_the_cj_prefix_ack() {
    let (mut hub, mut store) = ProtocolHub::start(Behavior {
        capabilities: vec!["exec_v1".into()],
        range_ack: false,
        ..Default::default()
    })
    .await;
    store.accept_command(&cancel_command()).unwrap();
    let cursor = store.cj_cursor().unwrap();
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Reconciling).await;
    let until = tokio::time::Instant::now() + Duration::from_millis(80);
    while let Ok(Some(event)) = tokio::time::timeout_at(until, hub.events.recv()).await {
        if let Observed::Control(message) = event {
            assert!(!matches!(
                message.msg,
                Some(control_up::Msg::ReconcileReport(_))
            ));
        }
    }
    assert_eq!(*state.borrow(), SessionState::Reconciling);
    hub.send(ControlDown {
        msg: Some(control_down::Msg::CjAck(
            canterlot_proto::worker::v1::JournalRangeAck {
                journal_id: cursor.journal_id,
                up_to_seq: cursor.last_seq,
            },
        )),
    })
    .await;
    wait_state(&mut state, SessionState::Live).await;
    loop {
        if let Observed::Control(message) = hub.event().await
            && let Some(control_up::Msg::ReconcileReport(report)) = message.msg
        {
            assert!(report.executions.is_empty());
            break;
        }
    }
    stop_worker(stop, join).await;
}

#[cfg(feature = "synthetic-adapter")]
#[path = "process_guard.rs"]
mod process_guard;

#[cfg(feature = "synthetic-adapter")]
fn execution_config(config: &mut WorkerConfig) {
    config.execution.allowed_workdir_roots = vec![config.data_dir.clone()];
    config.execution.supervisor_exe = Some(env!("CARGO_BIN_EXE_canterlot-worker").into());
    config.execution.supervisor.stop_grace = Duration::from_millis(300);
}

#[cfg(feature = "synthetic-adapter")]
async fn seed_execution(
    config: &WorkerConfig,
    store: WorkerStore,
    execution: Uuid,
    conversation: Uuid,
) -> WorkerStore {
    use canterlot_worker::{exec::ExecutionManager, session_store::StoreThread};
    let thread = StoreThread::new(store);
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let manager = ExecutionManager::new(config.execution.clone(), thread.client(), tx);
    let command = start_command(execution, conversation, &config.data_dir);
    let copy = command.clone();
    let accept = thread
        .call(move |s| Ok(s.as_mut().unwrap().accept_command(&copy)?))
        .await
        .unwrap();
    manager.handle(command, accept).await.unwrap();
    manager.on_hub_session(Default::default());
    timeout(Duration::from_secs(10), async {
        loop {
            let batch = rx.recv().await.unwrap();
            if batch.records.iter().any(|r| matches!(r.body.as_ref().and_then(|b| b.body.as_ref()), Some(Body::ExecutionState(f)) if f.state == "running")) { break; }
        }
    }).await.unwrap();
    manager.shutdown().await;
    thread.close().await.unwrap();
    WorkerStore::open(&config.data_dir).unwrap()
}

#[cfg(feature = "synthetic-adapter")]
#[tokio::test]
async fn reconcile_report_lists_nonterminal_executions_once_per_connection() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["exec_v1".into()],
        ..Default::default()
    })
    .await;
    let _guard = process_guard::ProcessGuard::new(hub.config.data_dir.parent().unwrap());
    execution_config(&mut hub.config);
    let execution = Uuid::now_v7();
    let store = seed_execution(&hub.config, store, execution, Uuid::now_v7()).await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    for connection in 0..2 {
        wait_state(&mut state, SessionState::Live).await;
        let mut reports = Vec::new();
        let mut hellos = 0;
        let until = tokio::time::Instant::now() + Duration::from_millis(400);
        while let Ok(event) = tokio::time::timeout_at(until, hub.events.recv()).await {
            match event.unwrap() {
                Observed::Hello(_, _) => hellos += 1,
                Observed::Control(message) => {
                    if let Some(control_up::Msg::ReconcileReport(report)) = message.msg {
                        reports.push(report);
                    }
                }
                Observed::Data(_) => {}
            }
        }
        assert_eq!(hellos, 2);
        assert_eq!(reports.len(), 1);
        let observation = &reports[0].executions[0];
        assert_eq!(reports[0].executions.len(), 1);
        assert_eq!(observation.execution_id, execution.to_string());
        assert_eq!(observation.generation, 1);
        assert_eq!(observation.state, "running");
        assert!(observation.outcome.is_empty());
        assert!(observation.supervisor_attached);
        assert!(!observation.sj_journal_id.is_empty());
        if connection == 0 {
            hub.fail(tonic::Status::unavailable("test reconnect")).await;
            wait_backoff(&mut state).await;
        }
    }
    stop_worker(stop, join).await;
}

#[cfg(feature = "synthetic-adapter")]
#[tokio::test]
async fn hello_lists_only_unretired_sj_journals() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        capabilities: vec!["exec_v1".into()],
        ..Default::default()
    })
    .await;
    let _guard = process_guard::ProcessGuard::new(hub.config.data_dir.parent().unwrap());
    execution_config(&mut hub.config);
    let execution = Uuid::now_v7();
    let store = seed_execution(&hub.config, store, execution, Uuid::now_v7()).await;
    let (stop, mut state, join) = start(hub.config.clone(), store);
    wait_state(&mut state, SessionState::Live).await;
    let Observed::Hello(hello, _) = hub.event().await else {
        panic!("missing hello")
    };
    assert_eq!(hello.cursors.len(), 2);
    assert_eq!(hello.cursors[0].journal_id, hello.cj_journal_id);
    let journal = hello.cursors[1].journal_id.clone();
    let connection_id = hello.connection_id.clone();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            execution_id: execution.to_string(),
            execution_generation: 1,
            kind: Some(command::Kind::StopExecution(
                canterlot_proto::worker::v1::StopExecution::default(),
            )),
        })),
    })
    .await;
    loop {
        if let Observed::Data(message) = hub.event().await
            && let Some(canterlot_proto::worker::v1::data_up::Msg::SjRecords(batch)) = message.msg
        {
            assert_eq!(batch.journal_id, journal);
            if batch.records.iter().any(|r| {
                matches!(
                    r.body.as_ref().and_then(|b| b.body.as_ref()),
                    Some(Body::JournalClosed(_))
                )
            }) {
                break;
            }
        }
    }
    sleep(Duration::from_millis(200)).await;
    hub.fail(tonic::Status::unavailable("test retirement"))
        .await;
    wait_backoff(&mut state).await;
    wait_state(&mut state, SessionState::Live).await;
    loop {
        if let Observed::Hello(hello, _) = hub.event().await
            && hello.connection_id != connection_id
        {
            assert_eq!(hello.cursors.len(), 1);
            assert_eq!(hello.cursors[0].journal_id, hello.cj_journal_id);
            break;
        }
    }
    stop_worker(stop, join).await;
}

#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod real {
    use super::*;
    use canterlot_hub::delivery::submit::{ContentBlock, SubmitInput};
    use canterlot_hub::session::Availability;
    use canterlot_proto::worker::v1::StreamKind;
    use support::data_proxy::DataProxy;

    async fn until(mut predicate: impl FnMut() -> bool) {
        timeout(Duration::from_secs(10), async {
            while !predicate() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[sqlx::test(migrations = "../canterlot-hub/migrations")]
    async fn data_binding_mismatch_restarts_the_whole_connection(pool: PgPool) {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let proxy = DataProxy::start(&hub, false, true).await;
        let (_directory, mut config, store) = fixture(&pool, &hub).await;
        config.hub_url = proxy.url.clone();
        let (stop, mut state, join) = start(config, store);
        wait_state(&mut state, SessionState::Live).await;
        let initial = proxy.hellos.lock().unwrap()[0].connection_id.clone();
        proxy
            .reject_data
            .store(true, std::sync::atomic::Ordering::SeqCst);
        hub.faults.fail_data(tonic::Code::Cancelled);
        wait_state(&mut state, SessionState::ControlOnly).await;
        wait_backoff(&mut state).await;
        wait_state(&mut state, SessionState::Live).await;
        let hellos = proxy.hellos.lock().unwrap().clone();
        let controls: Vec<_> = hellos.iter().filter(|h| h.stream_kind == 1).collect();
        assert_eq!(controls.len(), 2);
        assert_ne!(controls[1].connection_id, initial);
        assert_eq!(hellos.iter().filter(|h| h.stream_kind == 2).count(), 3);
        until(|| proxy.reports.lock().unwrap().len() == 2).await;
        stop_worker(stop, join).await;
        drop(proxy);
        stop_hub(hub).await;
    }

    #[sqlx::test(migrations = "../canterlot-hub/migrations")]
    async fn data_stream_failure_reattaches_under_same_connection_id_without_restarting_control(
        pool: PgPool,
    ) {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let proxy = DataProxy::start(&hub, false, true).await;
        let (_directory, mut config, store) = fixture(&pool, &hub).await;
        config.hub_url = proxy.url.clone();
        config.timers.backoff_min = Duration::from_millis(350);
        let machine = config.machine_id;
        let (stop, mut state, join) = start(config, store);
        wait_state(&mut state, SessionState::Live).await;
        until(|| hub.registry.is_reconciled(machine)).await;
        let before = hub.registry.session_info(machine).unwrap();
        proxy.data_release.send(false).unwrap();
        hub.faults.fail_data(tonic::Code::Unavailable);
        wait_state(&mut state, SessionState::ControlOnly).await;
        until(|| hub.registry.availability(machine) == Availability::Degraded).await;
        sleep(Duration::from_millis(120)).await;
        assert!(hub.registry.session_info(machine).unwrap().last_heartbeat > before.last_heartbeat);
        proxy.data_release.send(true).unwrap();
        wait_state(&mut state, SessionState::Live).await;
        assert_eq!(hub.registry.availability(machine), Availability::Available);
        let hellos = proxy.hellos.lock().unwrap().clone();
        assert_eq!(
            hellos
                .iter()
                .filter(|h| h.stream_kind == StreamKind::Control as i32)
                .count(),
            1
        );
        let data: Vec<_> = hellos
            .iter()
            .filter(|h| h.stream_kind == StreamKind::Data as i32)
            .collect();
        assert_eq!(data.len(), 2);
        assert!(
            data.iter()
                .all(|h| h.connection_id == before.connection_id.to_string())
        );
        assert_eq!(proxy.reports.lock().unwrap().len(), 1);
        stop_worker(stop, join).await;
        drop(proxy);
        stop_hub(hub).await;
    }

    async fn seeded(
        pool: &PgPool,
        hub: &TestHub,
        config: &mut WorkerConfig,
        store: WorkerStore,
    ) -> (WorkerStore, Uuid, Uuid) {
        execution_config(config);
        let conversation =
            testkit::seed_conversation(pool, hub.installation.owner_id, config.machine_id).await;
        sqlx::query("update conversations set workdir = $2 where id = $1")
            .bind(conversation)
            .bind(config.data_dir.to_str().unwrap())
            .execute(pool)
            .await
            .unwrap();
        let execution = executions::start(
            pool,
            hub.installation.owner_id,
            conversation,
            Uuid::now_v7(),
        )
        .await
        .unwrap()
        .execution_id;
        (
            seed_execution(config, store, execution, conversation).await,
            execution,
            conversation,
        )
    }

    #[sqlx::test(migrations = "../canterlot-hub/migrations")]
    async fn sj_batches_are_acked_and_compacted(pool: PgPool) {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let proxy = DataProxy::start(&hub, false, true).await;
        let (directory, mut config, store) = fixture(&pool, &hub).await;
        let _guard = process_guard::ProcessGuard::new(directory.path());
        let (store, execution, _) = seeded(&pool, &hub, &mut config, store).await;
        config.hub_url = proxy.url.clone();
        let data_dir = config.data_dir.clone();
        let machine = config.machine_id;
        let (stop, mut state, join) = start(config, store);
        wait_state(&mut state, SessionState::Live).await;
        until(|| !proxy.batches.lock().unwrap().is_empty()).await;
        let journal = proxy.batches.lock().unwrap()[0].journal_id.clone();
        let ack_file = data_dir
            .join("executions")
            .join(execution.to_string())
            .join("sj/ack");
        until(|| {
            fs::read(&ack_file)
                .is_ok_and(|bytes| u64::from_le_bytes(bytes[..8].try_into().unwrap()) > 0)
        })
        .await;
        stop_worker(stop, join).await;
        let ack = u64::from_le_bytes(fs::read(&ack_file).unwrap()[..8].try_into().unwrap());
        let committed: i64 = sqlx::query_scalar(
            "select committed_seq from journal_cursors where machine_id=$1 and journal_id=$2",
        )
        .bind(machine)
        .bind(Uuid::parse_str(&journal).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(committed as u64, ack);
        let batches = proxy.batches.lock().unwrap().clone();
        assert!(batches.iter().all(|b| b.journal_id == journal
            && b.records.len() <= 256
            && canterlot_proto::limits::check_record_batch(b).is_ok()));
        _guard.cleanup();
        let (journal, _) =
            canterlot_journal::Journal::open(ack_file.parent().unwrap(), Default::default())
                .unwrap();
        assert_eq!(journal.acked_seq(), ack);
        assert!(journal.first_seq() > ack);
        drop(proxy);
        stop_hub(hub).await;
    }

    #[sqlx::test(migrations = "../canterlot-hub/migrations")]
    async fn lost_sj_ack_resends_once_then_restarts_data_stream_only(pool: PgPool) {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let proxy = DataProxy::start(&hub, false, true).await;
        proxy
            .drop_sj_acks
            .store(100, std::sync::atomic::Ordering::SeqCst);
        let (directory, mut config, store) = fixture(&pool, &hub).await;
        let _guard = process_guard::ProcessGuard::new(directory.path());
        let (store, execution, _) = seeded(&pool, &hub, &mut config, store).await;
        config.hub_url = proxy.url.clone();
        let ack_file = config
            .data_dir
            .join("executions")
            .join(execution.to_string())
            .join("sj/ack");
        let (stop, mut state, join) = start(config, store);
        wait_state(&mut state, SessionState::Live).await;
        proxy.data_release.send(false).unwrap();
        wait_state(&mut state, SessionState::ControlOnly).await;
        assert_eq!(
            u64::from_le_bytes(fs::read(&ack_file).unwrap()[..8].try_into().unwrap()),
            0
        );
        proxy.data_release.send(true).unwrap();
        wait_state(&mut state, SessionState::Live).await;
        until(|| {
            fs::read(&ack_file).is_ok_and(|b| u64::from_le_bytes(b[..8].try_into().unwrap()) > 0)
        })
        .await;
        let hellos = proxy.hellos.lock().unwrap().clone();
        assert_eq!(hellos.iter().filter(|h| h.stream_kind == 1).count(), 1);
        let data: Vec<_> = hellos.iter().filter(|h| h.stream_kind == 2).collect();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].connection_id, data[1].connection_id);
        assert_eq!(data[1].cursors.len(), 2);
        let batches = proxy.batches.lock().unwrap().clone();
        let first = &batches[0];
        assert_eq!(batches.iter().filter(|b| *b == first).count(), 2);
        assert_eq!(proxy.reports.lock().unwrap().len(), 1);
        stop_worker(stop, join).await;
        drop(proxy);
        stop_hub(hub).await;
    }

    #[sqlx::test(migrations = "../canterlot-hub/migrations")]
    async fn dispatch_is_blocked_until_the_report_and_proceeds_after(pool: PgPool) {
        let hub = spawn_test_hub(pool.clone(), hub_timers()).await;
        let proxy = DataProxy::start(&hub, true, true).await;
        let (directory, mut config, store) = fixture(&pool, &hub).await;
        let _guard = process_guard::ProcessGuard::new(directory.path());
        let (store, _, conversation) = seeded(&pool, &hub, &mut config, store).await;
        config.hub_url = proxy.url.clone();
        let machine = config.machine_id;
        let (stop, mut state, join) = start(config, store);
        wait_state(&mut state, SessionState::Live).await;
        until(|| !proxy.reports.lock().unwrap().is_empty()).await;
        let input = hub
            .dispatcher
            .submit_input(
                &pool,
                SubmitInput {
                    owner_id: hub.installation.owner_id,
                    conversation_id: conversation,
                    idempotency_key: Uuid::now_v7().to_string(),
                    content: vec![ContentBlock::Text {
                        text: "test".into(),
                    }],
                    received_at: chrono::Utc::now(),
                    correlation_id: Uuid::now_v7(),
                },
            )
            .await
            .unwrap();
        sleep(Duration::from_millis(200)).await;
        assert!(!hub.registry.is_reconciled(machine));
        let state_before: String = sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input.input_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state_before, "queued");
        assert!(
            !hub.faults
                .commands()
                .iter()
                .any(|c| c.command_id == input.command_id.to_string())
        );
        proxy.release.send(true).unwrap();
        until(|| {
            hub.registry.is_reconciled(machine)
                && hub
                    .faults
                    .commands()
                    .iter()
                    .any(|c| c.command_id == input.command_id.to_string())
        })
        .await;
        let state_after: String = sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input.input_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(state_after, "queued");
        stop_worker(stop, join).await;
        drop(proxy);
        stop_hub(hub).await;
    }
}
