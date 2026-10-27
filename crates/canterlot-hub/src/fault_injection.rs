use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

use canterlot_proto::worker::v1::{
    Command, Hello, JournalCommitted, JournalRangeAck, JournalRecords,
};
use prost::Message;
use tokio::sync::{broadcast, oneshot};
use tonic::Code;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Point {
    BeforeCommandSend,
    BeforeCjCommit,
    BeforeSjCommit,
    BeforeAckSend,
    SchedulerBeforeSubmit,
    DropBusNotice,
}

#[derive(Clone, Debug)]
pub struct Ack {
    pub range: JournalRangeAck,
    pub committed_seq: u64,
    pub dropped: bool,
}

#[derive(Clone, Debug)]
pub struct CommittedGrant {
    pub at: Instant,
    pub journal_id: String,
    pub up_to_seq: u64,
    pub committed_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTrace {
    pub connection_id: uuid::Uuid,
    pub command_id: String,
    pub dispatch_attempt: u32,
    pub kind: Option<std::mem::Discriminant<canterlot_proto::worker::v1::command::Kind>>,
    pub encoded_len: usize,
}

impl From<&Command> for CommandTrace {
    fn from(command: &Command) -> Self {
        Self {
            connection_id: uuid::Uuid::nil(),
            command_id: command.command_id.clone(),
            dispatch_attempt: command.dispatch_attempt,
            kind: command.kind.as_ref().map(std::mem::discriminant),
            encoded_len: command.encoded_len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SjFact {
    TransmissionStarted(String, u32),
    InputWithdrawn(String, u32),
    InterruptStatus(String, i32),
    JournalClosed,
    BlockDelta,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTrace {
    pub seq: u64,
    pub kind: Option<std::mem::Discriminant<canterlot_proto::worker::v1::record_body::Body>>,
    pub encoded_len: usize,
    pub stored_command: Option<(String, u32)>,
    pub sj_fact: Option<SjFact>,
}

impl From<&canterlot_proto::worker::v1::JournalRecord> for RecordTrace {
    fn from(record: &canterlot_proto::worker::v1::JournalRecord) -> Self {
        let body = record.body.as_ref().and_then(|body| body.body.as_ref());
        Self {
            seq: record.seq,
            kind: body.map(std::mem::discriminant),
            encoded_len: record.encoded_len(),
            stored_command: match body {
                Some(canterlot_proto::worker::v1::record_body::Body::CommandStored(fact)) => {
                    Some((fact.command_id.clone(), fact.dispatch_attempt))
                }
                _ => None,
            },
            sj_fact: match body {
                Some(canterlot_proto::worker::v1::record_body::Body::TransmissionStarted(f)) => {
                    Some(SjFact::TransmissionStarted(
                        f.command_id.clone(),
                        f.dispatch_attempt,
                    ))
                }
                Some(canterlot_proto::worker::v1::record_body::Body::InputWithdrawn(f)) => Some(
                    SjFact::InputWithdrawn(f.command_id.clone(), f.dispatch_attempt),
                ),
                Some(canterlot_proto::worker::v1::record_body::Body::InterruptStatus(f)) => {
                    Some(SjFact::InterruptStatus(f.command_id.clone(), f.state))
                }
                Some(canterlot_proto::worker::v1::record_body::Body::JournalClosed(_)) => {
                    Some(SjFact::JournalClosed)
                }
                Some(canterlot_proto::worker::v1::record_body::Body::BlockDelta(_)) => {
                    Some(SjFact::BlockDelta)
                }
                _ => None,
            },
        }
    }
}

fn push<T>(history: &mut Vec<T>, entry: T) {
    if history.len() == 1024 {
        history.remove(0);
    }
    history.push(entry);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTrace {
    pub journal_id: String,
    pub records: Vec<RecordTrace>,
}

#[derive(Default)]
struct Trace {
    pauses: HashMap<Point, VecDeque<oneshot::Sender<oneshot::Sender<()>>>>,
    drops: HashMap<(Point, String), usize>,
    drop_acks: usize,
    drop_sj_acks: usize,
    sj_batch_pause: std::time::Duration,
    goaways: usize,
    hellos: Vec<Hello>,
    batches: Vec<BatchTrace>,
    commands: Vec<CommandTrace>,
    acks: Vec<Ack>,
    grants: Vec<CommittedGrant>,
}

#[derive(Clone)]
pub struct Faults {
    trace: Arc<Mutex<Trace>>,
    failures: broadcast::Sender<Code>,
    data_failures: broadcast::Sender<Code>,
    goaway: broadcast::Sender<()>,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            trace: Arc::default(),
            failures: broadcast::channel(16).0,
            data_failures: broadcast::channel(16).0,
            goaway: broadcast::channel(16).0,
        }
    }
}

impl Faults {
    pub fn drop_next_at(&self, point: Point, scope: &str) {
        *self
            .trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drops
            .entry((point, scope.into()))
            .or_default() += 1;
    }

    pub(crate) fn take_drop(&self, point: Point, scope: &str) -> bool {
        let mut trace = self.trace.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = trace.drops.get_mut(&(point, scope.into()))
            && *count > 0
        {
            *count -= 1;
            if *count == 0 {
                trace.drops.remove(&(point, scope.into()));
            }
            return true;
        }
        false
    }

    pub fn goaway(&self) {
        let _ = self.goaway.send(());
    }

    pub(crate) fn goaway_signals(&self) -> broadcast::Receiver<()> {
        self.goaway.subscribe()
    }

    pub(crate) fn goaway_started(&self) {
        self.trace.lock().unwrap_or_else(|e| e.into_inner()).goaways += 1;
    }

    pub fn goaway_count(&self) -> usize {
        self.trace.lock().unwrap_or_else(|e| e.into_inner()).goaways
    }

    pub fn set_sj_batch_pause(&self, pause: std::time::Duration) {
        self.trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sj_batch_pause = pause;
    }

    pub(crate) async fn pause_sj_batch(&self) {
        let pause = self
            .trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sj_batch_pause;
        tokio::time::sleep(pause).await;
        self.pause(Point::BeforeSjCommit).await;
    }

    pub fn drop_next_sj_acks(&self, count: usize) {
        self.trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drop_sj_acks = count;
    }

    pub fn pause_next(&self, point: Point) -> oneshot::Receiver<oneshot::Sender<()>> {
        let (tx, rx) = oneshot::channel();
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pauses
            .entry(point)
            .or_default()
            .push_back(tx);
        rx
    }

    pub(crate) async fn pause(&self, point: Point) {
        let waiter = self
            .trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pauses
            .get_mut(&point)
            .and_then(VecDeque::pop_front);
        if let Some(waiter) = waiter {
            let (resume, resumed) = oneshot::channel();
            if waiter.send(resume).is_ok() {
                let _ = resumed.await;
            }
        }
    }

    pub fn drop_next_acks(&self, count: usize) {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drop_acks = count;
    }

    pub fn fail_control(&self, code: Code) {
        if self.failures.send(code).is_err() {
            eprintln!("fault injection: no live control stream");
        }
    }

    pub(crate) fn failures(&self) -> broadcast::Receiver<Code> {
        self.failures.subscribe()
    }

    pub fn fail_data(&self, code: Code) {
        let _ = self.data_failures.send(code);
    }

    pub fn data_failures(&self) -> broadcast::Receiver<Code> {
        self.data_failures.subscribe()
    }

    pub(crate) fn hello(&self, hello: Hello) {
        push(
            &mut self
                .trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .hellos,
            hello,
        );
    }

    pub(crate) fn batch(&self, batch: JournalRecords) {
        push(
            &mut self
                .trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .batches,
            BatchTrace {
                journal_id: batch.journal_id,
                records: batch.records.iter().map(RecordTrace::from).collect(),
            },
        );
    }

    pub(crate) fn command(&self, command: Command, connection_id: uuid::Uuid) {
        let mut entry = CommandTrace::from(&command);
        entry.connection_id = connection_id;
        push(
            &mut self
                .trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .commands,
            entry,
        );
    }

    pub(crate) fn accepted(&self, committed: &[JournalCommitted]) {
        let mut trace = self.trace.lock().unwrap_or_else(|error| error.into_inner());
        for cursor in committed {
            push(
                &mut trace.grants,
                CommittedGrant {
                    at: Instant::now(),
                    journal_id: cursor.journal_id.clone(),
                    up_to_seq: cursor.committed_seq,
                    committed_seq: cursor.committed_seq,
                },
            );
        }
    }

    pub(crate) async fn ack(
        &self,
        pool: &sqlx::PgPool,
        machine: uuid::Uuid,
        range: &JournalRangeAck,
    ) -> bool {
        let Ok(journal) = uuid::Uuid::parse_str(&range.journal_id) else {
            eprintln!("fault injection: invalid journal id");
            return false;
        };
        let (committed, sj) = match sqlx::query_as::<_, (i64, String)>(
            "select committed_seq, kind from journal_cursors where machine_id = $1 and journal_id = $2",
        )
        .bind(machine)
        .bind(journal)
        .fetch_optional(pool)
        .await
        {
            Ok(row) => row.map_or((0, false), |(seq, kind)| (seq.try_into().unwrap_or(0), kind == "sj")),
            Err(_) => {
                eprintln!("fault injection: cursor query failed");
                (0, false)
            }
        };
        let dropped = {
            let mut trace = self.trace.lock().unwrap_or_else(|error| error.into_inner());
            let dropped = trace.drop_acks > 0 || (sj && trace.drop_sj_acks > 0);
            trace.drop_acks = trace.drop_acks.saturating_sub(1);
            if sj {
                trace.drop_sj_acks = trace.drop_sj_acks.saturating_sub(1);
            }
            push(
                &mut trace.acks,
                Ack {
                    range: range.clone(),
                    committed_seq: committed,
                    dropped,
                },
            );
            dropped
        };
        self.pause(Point::BeforeAckSend).await;
        if !dropped {
            let mut trace = self.trace.lock().unwrap_or_else(|error| error.into_inner());
            push(
                &mut trace.grants,
                CommittedGrant {
                    at: Instant::now(),
                    journal_id: range.journal_id.clone(),
                    up_to_seq: range.up_to_seq,
                    committed_seq: committed,
                },
            );
        }
        dropped
    }

    pub fn hellos(&self) -> Vec<Hello> {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hellos
            .clone()
    }

    pub fn batches(&self) -> Vec<BatchTrace> {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .batches
            .clone()
    }

    pub fn commands(&self) -> Vec<CommandTrace> {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .commands
            .clone()
    }

    pub fn acks(&self) -> Vec<Ack> {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .acks
            .clone()
    }

    pub fn grants(&self) -> Vec<CommittedGrant> {
        self.trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .grants
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canterlot_proto::worker::v1::{
        BlockDelta, JournalRecord, RecordBody, command, record_body,
    };

    #[test]
    fn histories_are_bounded_and_payloads_are_not_retained() {
        let faults = Faults::default();
        for seq in 0..1030 {
            let command = Command {
                command_id: seq.to_string(),
                kind: Some(command::Kind::DeliverInput(
                    canterlot_proto::worker::v1::DeliverInput {
                        content: vec![canterlot_proto::worker::v1::ContentBlock {
                            block: Some(canterlot_proto::worker::v1::content_block::Block::Text(
                                canterlot_proto::worker::v1::TextBlock {
                                    text: "private command payload".into(),
                                },
                            )),
                        }],
                        ..Default::default()
                    },
                )),
                ..Default::default()
            };
            faults.command(command, uuid::Uuid::nil());
            faults.hello(Hello {
                connection_id: seq.to_string(),
                ..Default::default()
            });
            faults.batch(JournalRecords {
                journal_id: seq.to_string(),
                records: vec![JournalRecord {
                    seq,
                    body: Some(RecordBody {
                        body: Some(record_body::Body::BlockDelta(BlockDelta {
                            append: "private journal payload".into(),
                            ..Default::default()
                        })),
                    }),
                }],
            });
            faults.accepted(&[JournalCommitted {
                journal_id: seq.to_string(),
                committed_seq: seq,
            }]);
        }
        assert!(!format!("{:?}", faults.commands()).contains("private command payload"));
        assert!(!format!("{:?}", faults.batches()).contains("private journal payload"));
        assert_eq!(faults.commands().len(), 1024);
        assert_eq!(faults.commands()[0].command_id, "6");
        assert_eq!(faults.hellos().len(), 1024);
        assert_eq!(faults.hellos()[0].connection_id, "6");
        assert_eq!(faults.batches().len(), 1024);
        assert_eq!(faults.batches()[0].journal_id, "6");
        assert!(
            faults
                .batches()
                .iter()
                .all(|batch| batch.records[0].stored_command.is_none())
        );
        assert_eq!(faults.grants().len(), 1024);
        assert_eq!(faults.grants()[0].committed_seq, 6);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn missing_cursor_and_invalid_id_do_not_panic(pool: sqlx::PgPool) {
        let faults = Faults::default();
        faults.fail_control(Code::Cancelled);
        let range = JournalRangeAck {
            journal_id: uuid::Uuid::now_v7().to_string(),
            up_to_seq: 0,
        };
        assert!(!faults.ack(&pool, uuid::Uuid::now_v7(), &range).await);
        assert_eq!(faults.acks()[0].committed_seq, 0);
        assert!(
            !faults
                .ack(
                    &pool,
                    uuid::Uuid::now_v7(),
                    &JournalRangeAck {
                        journal_id: "invalid".into(),
                        up_to_seq: 1,
                    }
                )
                .await
        );
        pool.close().await;
        let range = JournalRangeAck {
            up_to_seq: 1,
            ..range
        };
        assert!(!faults.ack(&pool, uuid::Uuid::now_v7(), &range).await);
        assert!(
            faults.acks().last().unwrap().range.up_to_seq
                > faults.acks().last().unwrap().committed_seq
        );
    }
}
