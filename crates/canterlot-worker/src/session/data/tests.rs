use super::*;
use crate::{
    exec::TestSignal,
    machine::limits::LiveLimits,
    session::tests::support::{Behavior, Observed, ProtocolHub},
    session_store::StoreThread,
};

fn limit_observed(seq: u64, window: LimitWindow) -> JournalRecord {
    JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::LimitObserved(LimitObservation {
                provider: "claude".into(),
                window: window as i32,
                used_permille: 50,
                resets_at_ms: 1_794_877_200_000,
                ..Default::default()
            })),
        }),
    }
}

fn limit_reached(seq: u64, window: LimitWindow) -> JournalRecord {
    JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::LimitReached(LimitReached {
                execution_id: Uuid::now_v7().to_string(),
                provider: "claude".into(),
                window: window as i32,
                resets_at_ms: 1_794_877_200_000,
                ..Default::default()
            })),
        }),
    }
}

fn wire_records(packed: VecDeque<Batch>) -> Vec<JournalRecord> {
    packed
        .into_iter()
        .flat_map(|(message, _, _)| match message.msg {
            Some(data_up::Msg::SjRecords(batch)) => batch.records,
            _ => Vec::new(),
        })
        .collect()
}

fn record(seq: u64, size: usize) -> JournalRecord {
    JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::BlockDelta(BlockDelta {
                append: "x".repeat(size),
                ..Default::default()
            })),
        }),
    }
}

#[test]
fn full_sj_batch_is_split_to_include_the_data_envelope() {
    let id = Uuid::now_v7();
    let mut batch = JournalRecords {
        journal_id: id.to_string(),
        records: (1..=31)
            .map(|seq| record(seq, 16_384))
            .chain(std::iter::once(record(32, 10_000)))
            .collect(),
    };
    let slack = MAX_EVENT_BATCH_BYTES - batch.encoded_len();
    if let Some(record_body::Body::BlockDelta(delta)) = batch
        .records
        .last_mut()
        .unwrap()
        .body
        .as_mut()
        .unwrap()
        .body
        .as_mut()
    {
        delta.append.push_str(&"x".repeat(slack));
    }
    assert_eq!(batch.encoded_len(), MAX_EVENT_BATCH_BYTES);
    let packed = pack(id, batch.records, true).unwrap();
    assert_eq!(packed.len(), 2);
    let mut seqs = Vec::new();
    for (message, journal, _) in packed {
        assert_eq!(journal, id);
        assert!(message.encoded_len() <= MAX_EVENT_BATCH_BYTES);
        let Some(data_up::Msg::SjRecords(batch)) = message.msg else {
            panic!("missing batch")
        };
        seqs.extend(batch.records.iter().map(|r| r.seq));
    }
    assert_eq!(seqs, (1..=32).collect::<Vec<_>>());
}

#[test]
fn record_over_the_batch_limit_does_not_fail_the_shared_stream() {
    let id = Uuid::now_v7();
    let records = vec![
        record(1, 10),
        record(2, MAX_EVENT_BATCH_BYTES),
        record(3, 10),
        record(4, MAX_EVENT_BATCH_BYTES * 2),
    ];
    let mut bodies = Vec::new();
    for (message, journal, last) in pack(id, records, true).unwrap() {
        assert_eq!(journal, id);
        assert!(message.encoded_len() <= MAX_EVENT_BATCH_BYTES);
        let Some(data_up::Msg::SjRecords(batch)) = message.msg else {
            panic!("missing batch")
        };
        assert_eq!(batch.records.last().unwrap().seq, last);
        bodies.extend(batch.records);
    }
    let seqs: Vec<_> = bodies.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    for record in [&bodies[1], &bodies[3]] {
        let body = record.body.as_ref().and_then(|b| b.body.as_ref());
        let Some(record_body::Body::AdapterDiagnostic(stub)) = body else {
            panic!("oversized record must become a stub: {body:?}")
        };
        assert_eq!(stub.event_type, "record_too_large");
        assert!(Uuid::parse_str(&stub.execution_id).is_ok());
    }
}

fn fable_frame_records() -> Vec<JournalRecord> {
    let frame: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../crates/canterlot-proto/tests/fixtures/fable_rate_limit.json"
    ))
    .unwrap();
    let mut tracker = crate::adapter::claude_code::limits::LimitTracker::default();
    tracker
        .on_rate_limit(&frame)
        .into_iter()
        .enumerate()
        .filter_map(|(index, fact)| match fact {
            crate::adapter::LimitFact::Observed {
                window,
                used_permille,
                resets_at_ms,
            } => Some(JournalRecord {
                seq: index as u64 + 1,
                body: Some(RecordBody {
                    body: Some(record_body::Body::LimitObserved(LimitObservation {
                        provider: "claude".into(),
                        window: window.into(),
                        used_permille: u32::from(used_permille),
                        resets_at_ms: resets_at_ms.unwrap_or_default(),
                        ..Default::default()
                    })),
                }),
            }),
            crate::adapter::LimitFact::Reached { .. } => None,
        })
        .collect()
}

#[test]
fn the_verbatim_fable_frame_never_ships_window_four_to_an_old_hub() {
    let records = wire_records(pack(Uuid::now_v7(), fable_frame_records(), false).unwrap());
    assert_eq!(
        records.iter().map(|record| record.seq).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(records.iter().all(|record| {
        !matches!(
            record.body.as_ref().and_then(|body| body.body.as_ref()),
            Some(record_body::Body::LimitObserved(observation))
                if observation.window == LimitWindow::WeeklyFable as i32
        )
    }));
}

#[test]
fn an_old_hub_gets_a_placeholder_for_a_fable_observation_and_weekly_for_its_reached() {
    let id = Uuid::now_v7();
    let records = wire_records(
        pack(
            id,
            vec![
                limit_observed(1, LimitWindow::WeeklyFable),
                limit_reached(2, LimitWindow::WeeklyFable),
                limit_observed(3, LimitWindow::Weekly),
            ],
            false,
        )
        .unwrap(),
    );
    assert_eq!(
        records.iter().map(|record| record.seq).collect::<Vec<_>>(),
        [1, 2, 3],
        "journal sequence stays continuous"
    );
    assert!(
        records
            .iter()
            .all(|record| record.body.as_ref().is_some_and(|body| {
                !matches!(
                    body.body.as_ref(),
                    Some(record_body::Body::LimitObserved(observation))
                        if observation.window == LimitWindow::WeeklyFable as i32
                )
            })),
        "an old hub must not see window 4"
    );
    let Some(record_body::Body::AdapterDiagnostic(stub)) =
        records[0].body.as_ref().and_then(|body| body.body.as_ref())
    else {
        panic!("fable observation must become a harmless diagnostic");
    };
    assert_eq!(stub.event_type, "limit_window_unsupported");
    assert_eq!(stub.execution_id, Uuid::nil().to_string());
    let Some(record_body::Body::LimitReached(reached)) =
        records[1].body.as_ref().and_then(|body| body.body.as_ref())
    else {
        panic!("fable reached must stay a reached fact");
    };
    assert_eq!(reached.window, LimitWindow::Weekly as i32);
    assert_eq!(reached.resets_at_ms, 1_794_877_200_000);
}

#[test]
fn a_negotiated_hub_keeps_window_four_on_the_wire() {
    let records = wire_records(
        pack(
            Uuid::now_v7(),
            vec![
                limit_observed(1, LimitWindow::WeeklyFable),
                limit_reached(2, LimitWindow::WeeklyFable),
            ],
            true,
        )
        .unwrap(),
    );
    let Some(record_body::Body::LimitObserved(observation)) =
        records[0].body.as_ref().and_then(|body| body.body.as_ref())
    else {
        panic!("fable observation kept");
    };
    assert_eq!(observation.window, LimitWindow::WeeklyFable as i32);
    let Some(record_body::Body::LimitReached(reached)) =
        records[1].body.as_ref().and_then(|body| body.body.as_ref())
    else {
        panic!("fable reached kept");
    };
    assert_eq!(reached.window, LimitWindow::WeeklyFable as i32);
}

#[tokio::test]
async fn an_old_hub_acks_the_substituted_fable_journal_to_the_end() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(16);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let journal = Uuid::now_v7();
    sender
        .send(SjRelay {
            journal_id: journal,
            records: vec![
                limit_observed(1, LimitWindow::WeeklyFable),
                limit_reached(2, LimitWindow::WeeklyFable),
            ],
        })
        .await
        .unwrap();
    for seq in 3..=10 {
        sender
            .send(SjRelay {
                journal_id: journal,
                records: vec![limit_observed(seq, LimitWindow::Weekly)],
            })
            .await
            .unwrap();
    }
    let (stop, mut active) = watch::channel(true);
    let task = tokio::spawn(async move {
        let result = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &WorkerTimers::default(),
            &mut active,
            (
                HashMap::new(),
                true,
                false,
                JournalDiagnostics::default(),
                LiveLimits::default(),
            ),
        )
        .await;
        manager.shutdown().await;
        result
    });
    let mut seqs = Vec::new();
    timeout(Duration::from_secs(3), async {
        while seqs.len() < 10 {
            if let Observed::Data(message) = hub.event().await
                && let Some(data_up::Msg::SjRecords(batch)) = message.msg
            {
                for record in batch.records {
                    assert!(
                        !matches!(
                            record.body.as_ref().and_then(|body| body.body.as_ref()),
                            Some(record_body::Body::LimitObserved(observation))
                                if observation.window == LimitWindow::WeeklyFable as i32
                        ),
                        "window 4 reached an old hub"
                    );
                    seqs.push(record.seq);
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(seqs, (1..=10).collect::<Vec<_>>());
    assert!(!task.is_finished(), "the execution stream keeps running");
    drop(stop);
    drop(sender);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    thread.close().await.unwrap();
}

async fn link(hub: &ProtocolHub) -> (mpsc::Sender<DataUp>, tonic::Streaming<DataDown>) {
    let endpoint = Endpoint::from_shared(hub.config.hub_url.clone())
        .unwrap()
        .tls_config(ClientTlsConfig::new().ca_certificate(Certificate::from_pem(
            fs::read(&hub.config.ca_cert).unwrap(),
        )))
        .unwrap();
    let channel = endpoint.connect().await.unwrap();
    let mut client = WorkerDataClient::with_interceptor(
        channel,
        Bearer("Bearer protocol-test".parse().unwrap()),
    );
    let (tx, rx) = mpsc::channel(1);
    tx.send(DataUp {
        msg: Some(data_up::Msg::Hello(Hello {
            cj_journal_id: Uuid::now_v7().to_string(),
            ..Default::default()
        })),
    })
    .await
    .unwrap();
    let mut inbound = client
        .data(ReceiverStream::new(rx))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(
        inbound.message().await.unwrap().unwrap().msg,
        Some(data_down::Msg::HelloReply(_))
    ));
    (tx, inbound)
}

#[tokio::test]
async fn sj_window_stops_at_eight_and_lost_head_retries_only_once() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        sj_ack: false,
        ..Default::default()
    })
    .await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(16);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let journal = Uuid::now_v7();
    for seq in 1..=12 {
        sender
            .send(SjRelay {
                journal_id: journal,
                records: vec![record(seq, 0)],
            })
            .await
            .unwrap();
    }
    let task = tokio::spawn(async move {
        let (_active, mut active) = watch::channel(true);
        let result = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &WorkerTimers {
                range_ack: Duration::from_millis(100),
                ..Default::default()
            },
            &mut active,
            (
                HashMap::new(),
                true,
                true,
                JournalDiagnostics::default(),
                LiveLimits::default(),
            ),
        )
        .await;
        manager.shutdown().await;
        result
    });
    let mut seqs = Vec::new();
    timeout(Duration::from_secs(3), async {
        while seqs.len() < 9 {
            if let Observed::Data(message) = hub.event().await
                && let Some(data_up::Msg::SjRecords(batch)) = message.msg
            {
                seqs.push(batch.records[0].seq);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(&seqs[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(seqs[8], 1);
    assert!(matches!(
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::Timeout)
    ));
    while let Ok(event) = hub.events.try_recv() {
        assert!(!matches!(event, Observed::Data(_)));
    }
    drop(sender);
    thread.close().await.unwrap();
}

#[tokio::test]
async fn another_journal_progress_and_duplicate_acks_do_not_postpone_the_head() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        sj_ack: false,
        ..Default::default()
    })
    .await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(8);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let first = Uuid::now_v7();
    let other = Uuid::now_v7();
    sender
        .send(SjRelay {
            journal_id: first,
            records: vec![record(1, 0)],
        })
        .await
        .unwrap();
    sender
        .send(SjRelay {
            journal_id: other,
            records: (1..=6).map(|seq| record(seq, 0)).collect(),
        })
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let (_active, mut active) = watch::channel(true);
        let result = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &WorkerTimers {
                range_ack: Duration::from_millis(100),
                ..Default::default()
            },
            &mut active,
            (
                HashMap::new(),
                true,
                true,
                JournalDiagnostics::default(),
                LiveLimits::default(),
            ),
        )
        .await;
        manager.shutdown().await;
        result
    });
    let mut batches = 0;
    while batches < 2 {
        if let Observed::Data(_) = hub.event().await {
            batches += 1;
        }
    }
    let started = Instant::now();
    for seq in 1..=6 {
        hub.send_data(DataDown {
            msg: Some(data_down::Msg::SjAck(JournalRangeAck {
                journal_id: other.to_string(),
                up_to_seq: seq,
            })),
        })
        .await;
        hub.send_data(DataDown {
            msg: Some(data_down::Msg::SjAck(JournalRangeAck {
                journal_id: first.to_string(),
                up_to_seq: 0,
            })),
        })
        .await;
        sleep(Duration::from_millis(25)).await;
    }
    assert!(matches!(
        timeout(Duration::from_millis(130), task)
            .await
            .unwrap()
            .unwrap(),
        Err(WorkerError::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_millis(280));
    drop(sender);
    thread.close().await.unwrap();
}

#[tokio::test]
async fn stale_post_reattach_batch_cannot_jump_the_committed_cursor() {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        sj_ack: false,
        ..Default::default()
    })
    .await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(8);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let journal = Uuid::now_v7();
    for seq in [7, 3] {
        sender
            .send(SjRelay {
                journal_id: journal,
                records: vec![record(seq, 0)],
            })
            .await
            .unwrap();
    }
    let (stop, mut active) = watch::channel(true);
    let task = tokio::spawn(async move {
        let result = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &WorkerTimers::default(),
            &mut active,
            (
                HashMap::from([(journal, 2)]),
                true,
                true,
                JournalDiagnostics::default(),
                LiveLimits::default(),
            ),
        )
        .await;
        manager.shutdown().await;
        result
    });
    let first = timeout(Duration::from_secs(3), async {
        loop {
            if let Observed::Data(message) = hub.event().await
                && let Some(data_up::Msg::SjRecords(batch)) = message.msg
            {
                break batch.records[0].seq;
            }
        }
    })
    .await
    .unwrap();
    drop(stop);
    task.await.unwrap().unwrap_err();
    drop(sender);
    thread.close().await.unwrap();
    assert_eq!(first, 3, "stale batch skipped the committed prefix");
}

async fn fail_shipped_batch(status: Status) -> (Vec<String>, Result<(), WorkerError>) {
    let (mut hub, store) = ProtocolHub::start(Behavior {
        sj_ack: false,
        ..Default::default()
    })
    .await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(8);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let diagnostics = JournalDiagnostics::default();
    let (_active, mut active) = watch::channel(true);
    let timers = WorkerTimers::default();
    let session = || {
        (
            HashMap::new(),
            true,
            true,
            diagnostics.clone(),
            LiveLimits::default(),
        )
    };
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    let relayed = |journal_id| SjRelay {
        journal_id,
        records: vec![record(1, 0)],
    };

    let (tx, mut inbound) = link(&hub).await;
    sender.send(relayed(first)).await.unwrap();
    let failed = {
        let ship = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &timers,
            &mut active,
            session(),
        );
        tokio::pin!(ship);
        loop {
            tokio::select! {
                result = &mut ship => panic!("ship ended early: {result:?}"),
                event = hub.event() => if let Observed::Data(_) = event { break },
            }
        }
        hub.fail_data(status).await;
        ship.await
    };

    let (tx, mut inbound) = link(&hub).await;
    sender.send(relayed(first)).await.unwrap();
    sender.send(relayed(second)).await.unwrap();
    let mut shipped = Vec::new();
    {
        let ship = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &timers,
            &mut active,
            session(),
        );
        tokio::pin!(ship);
        timeout(Duration::from_secs(3), async {
            while !shipped.contains(&second.to_string()) {
                tokio::select! {
                    result = &mut ship => panic!("ship ended early: {result:?}"),
                    event = hub.event() => if let Observed::Data(DataUp {
                        msg: Some(data_up::Msg::SjRecords(batch)),
                    }) = event {
                        shipped.push(batch.journal_id);
                    },
                }
            }
        })
        .await
        .unwrap();
    }
    manager.shutdown().await;
    drop(sender);
    thread.close().await.unwrap();
    let first = first.to_string();
    (
        shipped.into_iter().filter(|id| *id == first).collect(),
        failed,
    )
}

// Поток общий: журнал, который хаб не принимает, не должен рвать его заново на каждом подключении.
#[tokio::test]
async fn refused_journal_stays_parked_after_reattach_while_others_keep_shipping() {
    let (resent, failed) = fail_shipped_batch(Status::invalid_argument("unknown_record")).await;
    assert!(matches!(
        failed,
        Err(WorkerError::Hub { code: tonic::Code::InvalidArgument, ref message })
            if message == "unknown_record"
    ));
    assert_eq!(resent, Vec::<String>::new());
}

#[tokio::test]
async fn database_failure_does_not_park_the_journal() {
    let (resent, _) = fail_shipped_batch(Status::internal("database")).await;
    assert_eq!(resent.len(), 1);
}

#[tokio::test]
async fn pilot_sj_is_withheld_while_other_sj_ships_and_allowed_prefix_is_contiguous() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(8);
    let runtime = tempfile::Builder::new()
        .prefix("cl-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let manager = ExecutionManager::new_with_runtime_dir(
        Default::default(),
        thread.client(),
        sender.clone(),
        Some(runtime.path().to_path_buf()),
    );
    let hidden = Uuid::now_v7();
    let mixed = Uuid::now_v7();
    let ordinary = Uuid::now_v7();
    let fact = |seq| JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::LimitObserved(LimitObservation {
                provider: "claude".into(),
                ..Default::default()
            })),
        }),
    };
    for (id, records) in [
        (hidden, vec![fact(4)]),
        (mixed, vec![record(1, 0), fact(2), record(3, 0)]),
        (ordinary, vec![record(1, 0)]),
    ] {
        sender
            .send(SjRelay {
                journal_id: id,
                records,
            })
            .await
            .unwrap();
    }
    let diagnostics = JournalDiagnostics::default();
    let check = diagnostics.clone();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let (stop, mut active) = watch::channel(true);
        let timers = WorkerTimers::default();
        let ship = ship(
            &tx,
            &mut inbound,
            &manager,
            &mut relay,
            &timers,
            &mut active,
            (
                HashMap::from([(hidden, 3)]),
                false,
                true,
                diagnostics,
                LiveLimits::default(),
            ),
        );
        let result = tokio::select! {result=ship=>result,_=shutdown_rx.changed()=>Ok(())};
        drop(stop);
        manager.shutdown().await;
        result
    });
    let mut batches = Vec::new();
    while batches.len() < 2 {
        if let Observed::Data(m) = hub.event().await
            && let Some(data_up::Msg::SjRecords(batch)) = m.msg
        {
            batches.push(batch);
        }
    }
    assert!(batches.iter().any(|b| b.journal_id == ordinary.to_string()));
    assert!(batches.iter().any(|b| b.journal_id == mixed.to_string()
        && b.records.len() == 1
        && b.records[0].seq == 1));
    assert!(batches.iter().all(|b| {
        b.journal_id != hidden.to_string()
            && b.records
                .iter()
                .all(|r| !r.body.as_ref().is_some_and(crate::store::requires_pilot))
    }));
    assert_eq!(check.0.lock().unwrap().len(), 2);
    sender
        .send(SjRelay {
            journal_id: hidden,
            records: vec![fact(4)],
        })
        .await
        .unwrap();
    shutdown.send(true).unwrap();
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(sender);
    assert_eq!(check.0.lock().unwrap().len(), 2);
    thread.close().await.unwrap();
}

/// Запись, которую прочитал наблюдатель мёртвого сопровождения, против уже идущего цикла данных.
struct Resend {
    hub: ProtocolHub,
    sender: mpsc::Sender<SjRelay>,
    marker: Uuid,
    marker_seq: u64,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), WorkerError>>,
    thread: StoreThread,
}

struct ResendSetup {
    sj_ack: bool,
    pilot: bool,
    cursors: HashMap<Uuid, u64>,
    parked: Option<Uuid>,
    /// Журналы и признак потерянного сопровождения.
    actors: Vec<(Uuid, bool)>,
}

impl Resend {
    async fn start(setup: ResendSetup) -> (Self, Vec<crate::exec::TestActor>) {
        let (hub, store) = ProtocolHub::start(Behavior {
            sj_ack: setup.sj_ack,
            ..Default::default()
        })
        .await;
        let (tx, mut inbound) = link(&hub).await;
        let thread = StoreThread::new(store);
        let (sender, mut relay) = mpsc::channel(8);
        let runtime = tempfile::Builder::new()
            .prefix("cl-rt-")
            .tempdir_in("/tmp")
            .unwrap();
        let manager = ExecutionManager::new_with_runtime_dir(
            Default::default(),
            thread.client(),
            sender.clone(),
            Some(runtime.path().to_path_buf()),
        );
        let actors = setup
            .actors
            .iter()
            .map(|(journal, lost)| manager.test_actor(*journal, *lost))
            .collect();
        let diagnostics = JournalDiagnostics::default();
        if let Some(journal) = setup.parked {
            diagnostics.withheld(journal);
        }
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let (pilot, cursors) = (setup.pilot, setup.cursors);
        let task = tokio::spawn(async move {
            let (stop, mut active) = watch::channel(true);
            let timers = WorkerTimers::default();
            let ship = ship(
                &tx,
                &mut inbound,
                &manager,
                &mut relay,
                &timers,
                &mut active,
                (cursors, pilot, true, diagnostics, LiveLimits::default()),
            );
            let result = tokio::select! {result=ship=>result,_=shutdown_rx.changed()=>Ok(())};
            drop(stop);
            manager.shutdown().await;
            result
        });
        (
            Self {
                hub,
                sender,
                marker: Uuid::now_v7(),
                marker_seq: 0,
                shutdown,
                task,
                thread,
            },
            actors,
        )
    }

    async fn relay(&self, journal: Uuid, records: Vec<JournalRecord>) {
        self.sender
            .send(SjRelay {
                journal_id: journal,
                records,
            })
            .await
            .unwrap();
    }

    /// Канал упорядочен: когда хаб увидел метку, всё посланное раньше уже обработано.
    /// Возвращает пакеты, дошедшие до хаба, без самой метки.
    async fn settle(&mut self) -> Vec<JournalRecords> {
        self.marker_seq += 1;
        self.relay(self.marker, vec![record(self.marker_seq, 0)])
            .await;
        let marker = self.marker.to_string();
        let mut seen = Vec::new();
        timeout(Duration::from_secs(3), async {
            loop {
                if let Observed::Data(message) = self.hub.event().await
                    && let Some(data_up::Msg::SjRecords(batch)) = message.msg
                {
                    if batch.journal_id == marker {
                        return;
                    }
                    seen.push(batch);
                }
            }
        })
        .await
        .unwrap();
        seen
    }

    async fn close(self) {
        self.shutdown.send(true).unwrap();
        timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(self.sender);
        self.thread.close().await.unwrap();
    }
}

/// Настоящее подтверждение идёт от хаба отдельно и может отстать от метки.
async fn signals_after_ack(actor: &mut crate::exec::TestActor) -> Vec<TestSignal> {
    timeout(Duration::from_secs(3), async {
        loop {
            let signals = actor.signals();
            if !signals.is_empty() {
                return signals;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn seqs(batches: &[JournalRecords]) -> Vec<u64> {
    batches
        .iter()
        .flat_map(|batch| batch.records.iter().map(|r| r.seq))
        .collect()
}

fn records(range: std::ops::RangeInclusive<u64>) -> Vec<JournalRecord> {
    range.map(|seq| record(seq, 0)).collect()
}

fn setup(cursors: &[(Uuid, u64)], actors: &[(Uuid, bool)]) -> ResendSetup {
    ResendSetup {
        sj_ack: true,
        pilot: true,
        cursors: cursors.iter().copied().collect(),
        parked: None,
        actors: actors.to_vec(),
    }
}

#[tokio::test]
async fn resend_of_committed_records_is_answered_to_the_lost_journal_alone() {
    let (dead, other) = (Uuid::now_v7(), Uuid::now_v7());
    let (mut bench, mut actors) = Resend::start(setup(
        &[(dead, 9), (other, 9)],
        &[(dead, true), (other, true)],
    ))
    .await;
    bench.relay(dead, records(8..=9)).await;
    assert!(bench.settle().await.is_empty(), "the hub sees no duplicate");
    assert_eq!(actors[0].signals(), [TestSignal::Ack(9)]);
    assert!(actors[1].signals().is_empty());
    bench.close().await;
}

#[tokio::test]
async fn resend_never_repeats_itself_or_reaches_the_hub_twice() {
    let dead = Uuid::now_v7();
    let (mut bench, mut actors) = Resend::start(setup(&[(dead, 7)], &[(dead, true)])).await;
    bench.relay(dead, records(8..=9)).await;
    assert_eq!(seqs(&bench.settle().await), [8, 9]);
    assert_eq!(
        signals_after_ack(&mut actors[0]).await,
        [TestSignal::Ack(9)]
    );
    bench.relay(dead, records(8..=9)).await;
    assert!(bench.settle().await.is_empty());
    assert_eq!(
        actors[0].signals(),
        [TestSignal::Ack(9)],
        "one answer, once"
    );
    assert!(bench.settle().await.is_empty());
    assert!(
        actors[0].signals().is_empty(),
        "nothing retransmits on its own"
    );
    bench.close().await;
}

#[tokio::test]
async fn resend_ahead_of_the_ack_waits_for_the_real_one() {
    let dead = Uuid::now_v7();
    let (mut bench, mut actors) = Resend::start(ResendSetup {
        sj_ack: false,
        ..setup(&[(dead, 7)], &[(dead, true)])
    })
    .await;
    bench.relay(dead, records(8..=9)).await;
    assert_eq!(seqs(&bench.settle().await), [8, 9]);
    bench.relay(dead, records(8..=9)).await;
    bench.settle().await;
    assert!(
        actors[0].signals().is_empty(),
        "sent is not committed: only the hub may say so"
    );
    bench.close().await;
}

#[tokio::test]
async fn partly_sent_resend_ships_only_the_tail_in_order() {
    let dead = Uuid::now_v7();
    let (mut bench, mut actors) = Resend::start(setup(&[(dead, 9)], &[(dead, true)])).await;
    bench.relay(dead, records(8..=11)).await;
    assert_eq!(seqs(&bench.settle().await), [10, 11]);
    assert_eq!(
        signals_after_ack(&mut actors[0]).await,
        [TestSignal::Ack(11)]
    );
    bench.close().await;
}

#[tokio::test]
async fn resend_past_a_gap_still_asks_for_a_replay() {
    let dead = Uuid::now_v7();
    let (mut bench, mut actors) = Resend::start(setup(&[(dead, 3)], &[(dead, true)])).await;
    bench.relay(dead, records(6..=7)).await;
    assert!(bench.settle().await.is_empty());
    assert_eq!(actors[0].signals(), [TestSignal::Replay(3)]);
    bench.close().await;
}

#[tokio::test]
async fn live_journal_hears_nothing_about_records_it_already_sent() {
    let live = Uuid::now_v7();
    let (mut bench, mut actors) = Resend::start(setup(&[(live, 9)], &[(live, false)])).await;
    bench.relay(live, records(8..=9)).await;
    assert!(bench.settle().await.is_empty());
    assert!(
        actors[0].signals().is_empty(),
        "an extra ack would reach the supervisor and may release the execution"
    );
    bench.relay(live, records(9..=10)).await;
    assert_eq!(seqs(&bench.settle().await), [10]);
    assert_eq!(
        signals_after_ack(&mut actors[0]).await,
        [TestSignal::Ack(10)]
    );
    bench.close().await;
}

#[tokio::test]
async fn withheld_records_get_no_answer_but_committed_ones_do() {
    let parked = Uuid::now_v7();
    let fresh = Uuid::now_v7();
    let fact = |seq| JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::LimitObserved(LimitObservation {
                provider: "claude".into(),
                ..Default::default()
            })),
        }),
    };
    let (mut bench, mut actors) = Resend::start(ResendSetup {
        pilot: false,
        parked: Some(parked),
        ..setup(&[(parked, 3), (fresh, 3)], &[(parked, true), (fresh, true)])
    })
    .await;
    bench.relay(parked, vec![fact(4)]).await;
    bench.relay(fresh, vec![fact(4)]).await;
    assert!(bench.settle().await.is_empty());
    assert!(actors[0].signals().is_empty(), "the hub has not seen it");
    assert!(
        actors[1].signals().is_empty(),
        "cut to nothing, nothing sent"
    );

    bench.relay(parked, records(2..=3)).await;
    assert!(bench.settle().await.is_empty());
    assert_eq!(
        actors[0].signals(),
        [TestSignal::Ack(3)],
        "committed anyway"
    );
    bench.close().await;
}
