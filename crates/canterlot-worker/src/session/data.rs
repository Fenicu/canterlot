use std::collections::HashMap;

use super::*;
use crate::config::WorkerTimers;
use tonic::{service::interceptor::InterceptedService, transport::Channel};

type Client = WorkerDataClient<InterceptedService<Channel, Bearer>>;

struct Range {
    message: DataUp,
    journal: Uuid,
    last: u64,
    deadline: Option<Instant>,
    retried: bool,
}

pub(super) fn committed(
    hello: &Hello,
    accepted: &Accepted,
) -> Result<HashMap<Uuid, u64>, WorkerError> {
    let mut committed = HashMap::new();
    for cursor in &hello.cursors {
        let id = Uuid::parse_str(&cursor.journal_id).map_err(|_| WorkerError::Protocol)?;
        let seq = accepted
            .committed
            .iter()
            .find(|c| c.journal_id == cursor.journal_id)
            .map_or(0, |c| c.committed_seq);
        if cursor.journal_id != hello.cj_journal_id
            && (seq > cursor.last_seq
                || seq < cursor.acked_seq_known_to_worker
                || cursor.first_stored_seq > seq.saturating_add(1))
        {
            return Err(WorkerError::StateDivergence);
        }
        committed.insert(id, seq);
    }
    Ok(committed)
}

pub(super) fn session_committed(
    hello: &Hello,
    accepted: &Accepted,
    manager: &ExecutionManager,
) -> Result<HashMap<Uuid, u64>, WorkerError> {
    let mut cursors = committed(hello, accepted)?;
    for local in manager.journal_cursors() {
        let id = Uuid::parse_str(&local.journal_id).map_err(|_| WorkerError::Protocol)?;
        cursors.entry(id).or_insert(local.acked_seq_known_to_worker);
    }
    Ok(cursors)
}

pub(super) struct Stream {
    pub pilot: bool,
    pub diagnostics: JournalDiagnostics,
    pub live_limits: crate::machine::limits::LiveLimits,
    #[cfg(feature = "fault-injection")]
    pub probe: crate::fault_injection::SessionProbe,
    pub client: Client,
    pub hello: Hello,
    pub tx: mpsc::Sender<DataUp>,
    pub inbound: tonic::Streaming<DataDown>,
    pub committed: HashMap<Uuid, u64>,
}

pub(super) async fn run(
    stream: Stream,
    manager: &ExecutionManager,
    relay: &mut mpsc::Receiver<SjRelay>,
    timers: &WorkerTimers,
    attached: watch::Sender<bool>,
    store: crate::session_store::StoreClient,
    mut active: watch::Receiver<bool>,
) -> Result<(), WorkerError> {
    let Stream {
        pilot,
        diagnostics,
        live_limits,
        #[cfg(feature = "fault-injection")]
        probe,
        mut client,
        mut hello,
        mut tx,
        mut inbound,
        committed: mut cursors,
    } = stream;
    #[cfg(feature = "fault-injection")]
    probe.data_attached();
    hello.stream_kind = StreamKind::Data as i32;
    let mut cap = timers.backoff_min;
    let mut live_since = Instant::now();
    loop {
        let error = ship(
            &tx,
            &mut inbound,
            manager,
            relay,
            timers,
            &mut active,
            (cursors, pilot, diagnostics.clone(), live_limits.clone()),
        )
        .await
        .unwrap_err();
        tracing::warn!(error_kind = ?error.kind(), "data stream failed");
        attached.send_replace(false);
        // Закрытие sender освобождает привязку data на хабе до нового Hello.
        drop(tx);
        drop(inbound);
        if live_since.elapsed() >= timers.backoff_reset_after {
            cap = timers.backoff_min;
        }
        loop {
            let delay = Duration::from_nanos(rand::random_range(
                0..=cap.as_nanos().min(u64::MAX as u128) as u64,
            ));
            sleep(delay).await;
            cap = cap.saturating_mul(2).min(timers.backoff_max);
            let result = timeout(timers.handshake, async {
                let cursor = store
                    .call(|slot| Ok(slot.as_ref().ok_or(WorkerError::StoreThread)?.cj_cursor()?))
                    .await?;
                let cj_id =
                    Uuid::parse_str(&cursor.journal_id).map_err(|_| WorkerError::Protocol)?;
                let cj_pilot = store
                    .call(move |slot| {
                        Ok(slot
                            .as_ref()
                            .ok_or(WorkerError::StoreThread)?
                            .journal_next_requires_pilot(cj_id)?)
                    })
                    .await?;
                hello.cursors = (pilot || !cj_pilot)
                    .then_some(cursor)
                    .into_iter()
                    .chain(manager.permitted_cursors(pilot).await?)
                    .collect();
                hello.machine_time_ms = crate::machine::now_ms();
                let (sender, rx) = mpsc::channel(1);
                send(
                    &sender,
                    DataUp {
                        msg: Some(data_up::Msg::Hello(hello.clone())),
                    },
                    timers.handshake,
                )
                .await?;
                let mut stream = client
                    .data(ReceiverStream::new(rx))
                    .await
                    .map_err(status_error)?
                    .into_inner();
                let accepted = match stream
                    .message()
                    .await
                    .map_err(status_error)?
                    .and_then(|m| m.msg)
                {
                    Some(data_down::Msg::HelloReply(reply)) => accept(reply)?,
                    _ => return Err(WorkerError::Protocol),
                };
                let generation = Uuid::parse_str(&hello.restore_generation)
                    .map_err(|_| WorkerError::Protocol)?;
                check_generation(generation, &accepted)?;
                let committed = session_committed(&hello, &accepted, manager)?;
                // Старые элементы восстанавливаются сопровождением от durable cursor.
                while relay.try_recv().is_ok() {}
                if *active.borrow() {
                    manager.on_hub_session(committed.clone());
                }
                Ok((sender, stream, committed))
            })
            .await
            .map_err(|_| WorkerError::Timeout)
            .and_then(|result| result);
            match result {
                Ok((sender, stream, committed)) => {
                    #[cfg(feature = "fault-injection")]
                    probe.data_attached();
                    tx = sender;
                    inbound = stream;
                    cursors = committed;
                    live_since = Instant::now();
                    attached.send_replace(true);
                    break;
                }
                Err(
                    error @ (WorkerError::RestoreGenerationChanged | WorkerError::StateDivergence),
                ) => return Err(error),
                Err(error @ WorkerError::Rejected { reason, .. })
                    if reason == RejectReason::ConnectionBindingMismatch as i32 =>
                {
                    return Err(error);
                }
                Err(
                    error @ WorkerError::Rejected {
                        retryable: false, ..
                    },
                ) => return Err(error),
                Err(
                    error @ WorkerError::Hub {
                        code: tonic::Code::Unauthenticated,
                        ..
                    },
                ) => return Err(error),
                Err(error) => tracing::warn!(error_kind = ?error.kind(), "data reattach failed"),
            }
        }
    }
}

async fn ship(
    tx: &mpsc::Sender<DataUp>,
    inbound: &mut tonic::Streaming<DataDown>,
    manager: &ExecutionManager,
    relay: &mut mpsc::Receiver<SjRelay>,
    timers: &WorkerTimers,
    active: &mut watch::Receiver<bool>,
    session: (
        HashMap<Uuid, u64>,
        bool,
        JournalDiagnostics,
        crate::machine::limits::LiveLimits,
    ),
) -> Result<(), WorkerError> {
    let (cursors, pilot, diagnostics, live_limits) = session;
    let mut withheld = diagnostics.parked();
    let mut pending: VecDeque<Range> = VecDeque::new();
    let mut sent = cursors.clone();
    let mut acknowledged = cursors;
    let mut ready = VecDeque::new();
    let mut retry = None;
    loop {
        let deadline = pending.front().and_then(|p| p.deadline);
        tokio::select! {
            result = active.changed() => { result.map_err(|_| WorkerError::Transport)?; }
            message = inbound.message() => {
                let message = match message {
                    Ok(message) => message.ok_or(WorkerError::Transport)?,
                    Err(status) => {
                        // Хаб отвечает строго по порядку: отказ относится к старейшему диапазону.
                        if refused_record(status.message()) && let Some(head) = pending.front() {
                            diagnostics.refused(head.journal, status.message());
                        }
                        return Err(status_error(status));
                    }
                };
                let Some(data_down::Msg::SjAck(ack)) = message.msg else { return Err(WorkerError::Protocol); };
                let journal = Uuid::parse_str(&ack.journal_id).map_err(|_| WorkerError::Protocol)?;
                if ack.up_to_seq > sent.get(&journal).copied().unwrap_or(0) { return Err(WorkerError::Protocol); }
                let previous = acknowledged.entry(journal).or_default();
                if ack.up_to_seq > *previous {
                    *previous = ack.up_to_seq;
                    manager.on_sj_ack(journal, ack.up_to_seq);
                    let head_progress = pending.front().is_some_and(|head| head.journal == journal);
                    pending.retain(|p| p.journal != journal || p.last > ack.up_to_seq);
                    if head_progress {
                        for range in &mut pending { range.deadline = None; range.retried = false; }
                        if let Some(head) = pending.front_mut() { head.deadline = Some(Instant::now() + timers.range_ack); }
                        retry = None;
                    }
                }
            }
            batch = relay.recv(), if *active.borrow() && ready.is_empty() && pending.len() < 8 => {
                let Some(batch) = batch else { return Err(WorkerError::Execution); };
                let journal = batch.journal_id;
                let after = sent.get(&journal).copied().unwrap_or(0);
                let resent_up_to = batch.records.last().map_or(0, |r| r.seq);
                let mut records: Vec<_> = batch.records.into_iter().filter(|r| r.seq > after).collect();
                if records.is_empty() {
                    // Всё это уже ушло при живом акторе, хаб ответил ему и второй раз не ответит.
                    // Говорить за хаб можно только о том, что он правда подтвердил.
                    if let Some(acked) = acknowledged.get(&journal).copied().filter(|a| *a >= resent_up_to) {
                        manager.on_sj_resend_committed(journal, acked);
                    }
                    continue;
                }
                if withheld.contains(&journal){continue}
                if !pilot && let Some(index)=records.iter().position(|r|r.body.as_ref().is_some_and(crate::store::requires_pilot)) {
                    records.truncate(index);
                    if withheld.insert(journal){diagnostics.withheld(journal);}
                }
                if records.is_empty() { continue; }
                if records[0].seq != after + 1 {
                    manager.replay_sj(journal, after);
                    continue;
                }
                for record in &records {
                    if let Some(RecordBody { body: Some(record_body::Body::LimitObserved(observation)), .. }) = &record.body {
                        live_limits.note_observation(observation);
                    }
                }
                ready = pack(journal, records)?;
            }
            permit = tx.reserve(), if retry.is_some() || (!ready.is_empty() && pending.len() < 8) => {
                let permit = permit.map_err(|_| WorkerError::Transport)?;
                let message = if let Some(message) = retry.take() { message } else {
                    let (message, journal, last) = ready.pop_front().unwrap();
                    sent.insert(journal, last);
                    let deadline = pending.is_empty().then(|| Instant::now() + timers.range_ack);
                    pending.push_back(Range { message: message.clone(), journal, last, deadline, retried: false });
                    message
                };
                permit.send(message);
            }
            _ = async { match deadline { Some(deadline) => sleep_until(deadline).await, None => std::future::pending().await } } => {
                let head = pending.front_mut().unwrap();
                if head.retried { return Err(WorkerError::Timeout); }
                head.retried = true;
                head.deadline = Some(Instant::now() + timers.range_ack);
                retry = Some(head.message.clone());
            }
        }
    }
}

type Batch = (DataUp, Uuid, u64);

fn pack(journal: Uuid, records: Vec<JournalRecord>) -> Result<VecDeque<Batch>, WorkerError> {
    if records.len() > 256 {
        return Err(WorkerError::Protocol);
    }
    let mut result = VecDeque::new();
    let mut batch = JournalRecords {
        journal_id: journal.to_string(),
        records: Vec::new(),
    };
    for record in records {
        batch.records.push(shippable(journal, record));
        let bytes = batch.encoded_len();
        if prost::encoding::key_len(2) + prost::length_delimiter_len(bytes) + bytes
            > MAX_EVENT_BATCH_BYTES
        {
            let record = batch.records.pop().unwrap();
            result.push_back(envelope(journal, batch)?);
            batch = JournalRecords {
                journal_id: journal.to_string(),
                records: vec![record],
            };
        }
    }
    if !batch.records.is_empty() {
        result.push_back(envelope(journal, batch)?);
    }
    Ok(result)
}

/// Поток данных общий для всех сессий машины, поэтому запись, не влезающая в пакет,
/// не должна его рвать: хаб получает с тем же seq отметку без ссылки на исполнение.
fn shippable(journal: Uuid, record: JournalRecord) -> JournalRecord {
    let alone = JournalRecords {
        journal_id: journal.to_string(),
        records: vec![record],
    };
    let bytes = alone.encoded_len();
    let framed = prost::encoding::key_len(2) + prost::length_delimiter_len(bytes) + bytes;
    let mut records = alone.records;
    let record = records.pop().unwrap();
    if framed <= MAX_EVENT_BATCH_BYTES {
        return record;
    }
    tracing::warn!(kind = "record_too_large", %journal, seq = record.seq, "oversized record replaced");
    JournalRecord {
        seq: record.seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::AdapterDiagnostic(
                canterlot_proto::worker::v1::AdapterDiagnostic {
                    execution_id: Uuid::nil().to_string(),
                    kind: "unknown_optional_event".into(),
                    event_type: "record_too_large".into(),
                    ..Default::default()
                },
            )),
        }),
    }
}

fn envelope(journal: Uuid, batch: JournalRecords) -> Result<Batch, WorkerError> {
    let last = batch.records.last().unwrap().seq;
    let message = DataUp {
        msg: Some(data_up::Msg::SjRecords(batch)),
    };
    if message.encoded_len() > MAX_EVENT_BATCH_BYTES {
        return Err(WorkerError::Protocol);
    }
    Ok((message, journal, last))
}

#[cfg(test)]
mod tests;
