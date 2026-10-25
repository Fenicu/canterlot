use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

use canterlot_proto::worker::v1::{
    Command, Hello, JournalCommitted, JournalRangeAck, JournalRecords,
};
use tokio::sync::{broadcast, oneshot};
use tonic::Code;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Point {
    BeforeCommandSend,
    BeforeCjCommit,
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

#[derive(Default)]
struct Trace {
    pauses: HashMap<Point, VecDeque<oneshot::Sender<oneshot::Sender<()>>>>,
    drop_acks: usize,
    hellos: Vec<Hello>,
    batches: Vec<JournalRecords>,
    commands: Vec<Command>,
    acks: Vec<Ack>,
    grants: Vec<CommittedGrant>,
}

#[derive(Clone)]
pub struct Faults {
    trace: Arc<Mutex<Trace>>,
    failures: broadcast::Sender<Code>,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            trace: Arc::default(),
            failures: broadcast::channel(16).0,
        }
    }
}

impl Faults {
    pub fn pause_next(&self, point: Point) -> oneshot::Receiver<oneshot::Sender<()>> {
        let (tx, rx) = oneshot::channel();
        self.trace
            .lock()
            .unwrap()
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
            .unwrap()
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
        self.trace.lock().unwrap().drop_acks = count;
    }

    pub fn fail_control(&self, code: Code) {
        self.failures.send(code).expect("live control stream");
    }

    pub(crate) fn failures(&self) -> broadcast::Receiver<Code> {
        self.failures.subscribe()
    }

    pub(crate) fn hello(&self, hello: Hello) {
        self.trace.lock().unwrap().hellos.push(hello);
    }

    pub(crate) fn batch(&self, batch: JournalRecords) {
        self.trace.lock().unwrap().batches.push(batch);
    }

    pub(crate) fn command(&self, command: Command) {
        self.trace.lock().unwrap().commands.push(command);
    }

    pub(crate) fn accepted(&self, committed: &[JournalCommitted]) {
        let mut trace = self.trace.lock().unwrap();
        for cursor in committed {
            trace.grants.push(CommittedGrant {
                at: Instant::now(),
                journal_id: cursor.journal_id.clone(),
                up_to_seq: cursor.committed_seq,
                committed_seq: cursor.committed_seq,
            });
        }
    }

    pub(crate) async fn ack(
        &self,
        pool: &sqlx::PgPool,
        machine: uuid::Uuid,
        range: &JournalRangeAck,
    ) -> bool {
        let committed: i64 = sqlx::query_scalar(
            "select committed_seq from journal_cursors where machine_id = $1 and journal_id = $2",
        )
        .bind(machine)
        .bind(uuid::Uuid::parse_str(&range.journal_id).unwrap())
        .fetch_one(pool)
        .await
        .expect("committed journal cursor");
        let mut trace = self.trace.lock().unwrap();
        let dropped = trace.drop_acks > 0;
        trace.drop_acks = trace.drop_acks.saturating_sub(1);
        if !dropped {
            trace.grants.push(CommittedGrant {
                at: Instant::now(),
                journal_id: range.journal_id.clone(),
                up_to_seq: range.up_to_seq,
                committed_seq: committed.try_into().unwrap(),
            });
        }
        trace.acks.push(Ack {
            range: range.clone(),
            committed_seq: committed.try_into().unwrap(),
            dropped,
        });
        dropped
    }

    pub fn hellos(&self) -> Vec<Hello> {
        self.trace.lock().unwrap().hellos.clone()
    }

    pub fn batches(&self) -> Vec<JournalRecords> {
        self.trace.lock().unwrap().batches.clone()
    }

    pub fn commands(&self) -> Vec<Command> {
        self.trace.lock().unwrap().commands.clone()
    }

    pub fn acks(&self) -> Vec<Ack> {
        self.trace.lock().unwrap().acks.clone()
    }

    pub fn grants(&self) -> Vec<CommittedGrant> {
        self.trace.lock().unwrap().grants.clone()
    }
}
