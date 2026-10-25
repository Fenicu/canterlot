use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use prost::Message;

use canterlot_proto::worker::v1::{Command, JournalCursor};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandTrace {
    pub command_id: String,
    pub dispatch_attempt: u32,
    pub kind: Option<std::mem::Discriminant<canterlot_proto::worker::v1::command::Kind>>,
    pub encoded_len: usize,
}

impl From<&Command> for CommandTrace {
    fn from(command: &Command) -> Self {
        Self {
            command_id: command.command_id.clone(),
            dispatch_attempt: command.dispatch_attempt,
            kind: command.kind.as_ref().map(std::mem::discriminant),
            encoded_len: command.encoded_len(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordTrace {
    pub seq: u64,
    pub kind: Option<std::mem::Discriminant<canterlot_proto::worker::v1::record_body::Body>>,
    pub encoded_len: usize,
    pub stored_command: Option<(String, u32)>,
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
        }
    }
}

fn push<T>(history: &mut Vec<T>, entry: T) {
    if history.len() == 1024 {
        history.remove(0);
    }
    history.push(entry);
}

#[derive(Clone, Debug)]
pub struct StoreSnapshot {
    pub at: Instant,
    pub cursor: JournalCursor,
    pub inbox: Vec<CommandTrace>,
    pub cj: Vec<RecordTrace>,
    pub receipt_count: u64,
}

#[derive(Clone, Default)]
pub struct StoreProbe {
    snapshots: Arc<Mutex<Vec<StoreSnapshot>>>,
    duplicates: Arc<AtomicU64>,
}

impl StoreProbe {
    pub fn snapshots(&self) -> Vec<StoreSnapshot> {
        self.snapshots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn duplicate_count(&self) -> u64 {
        self.duplicates.load(Ordering::Relaxed)
    }

    pub(crate) fn duplicate(&self) {
        self.duplicates.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record(&self, snapshot: StoreSnapshot) {
        push(
            &mut self
                .snapshots
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            snapshot,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_keeps_last_1024_snapshots() {
        let probe = StoreProbe::default();
        for seq in 0..1030 {
            probe.record(StoreSnapshot {
                at: Instant::now(),
                cursor: JournalCursor {
                    last_seq: seq,
                    ..Default::default()
                },
                inbox: vec![],
                cj: vec![],
                receipt_count: seq,
            });
        }
        let snapshots = probe.snapshots();
        assert_eq!(snapshots.len(), 1024);
        assert_eq!(snapshots[0].cursor.last_seq, 6);
        assert_eq!(snapshots[1023].cursor.last_seq, 1029);
    }
}
