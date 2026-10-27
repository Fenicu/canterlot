use super::*;
use crate::{
    session::tests::support::{Behavior, Observed, ProtocolHub},
    session_store::StoreThread,
};

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
    let packed = pack(id, batch.records).unwrap();
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
    let manager = ExecutionManager::new(Default::default(), thread.client(), sender.clone());
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
            (HashMap::new(), true, JournalDiagnostics::default()),
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
    let manager = ExecutionManager::new(Default::default(), thread.client(), sender.clone());
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
            (HashMap::new(), true, JournalDiagnostics::default()),
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
    let manager = ExecutionManager::new(Default::default(), thread.client(), sender.clone());
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
                JournalDiagnostics::default(),
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

#[tokio::test]
async fn pilot_sj_is_withheld_while_other_sj_ships_and_allowed_prefix_is_contiguous() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    let (tx, mut inbound) = link(&hub).await;
    let thread = StoreThread::new(store);
    let (sender, mut relay) = mpsc::channel(8);
    let manager = ExecutionManager::new(Default::default(), thread.client(), sender.clone());
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
            (HashMap::from([(hidden, 3)]), false, diagnostics),
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
