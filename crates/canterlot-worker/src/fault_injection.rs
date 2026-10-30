use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use prost::Message;

use canterlot_proto::worker::v1::{Command, JournalCursor};

#[derive(Clone, Default)]
pub struct SessionProbe {
    sent: Arc<Mutex<std::collections::HashMap<u64, Instant>>>,
    sends: Arc<Mutex<Vec<Instant>>>,
    acks: Arc<Mutex<Vec<std::time::Duration>>>,
    data_attaches: Arc<AtomicU64>,
}

impl SessionProbe {
    pub(crate) fn data_attached(&self) {
        self.data_attaches.fetch_add(1, Ordering::Relaxed);
    }
    pub fn data_attaches(&self) -> u64 {
        self.data_attaches.load(Ordering::Relaxed)
    }
    pub(crate) fn sent(&self, seq: u64) {
        let at = Instant::now();
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(seq, at);
        push(
            &mut self.sends.lock().unwrap_or_else(|e| e.into_inner()),
            at,
        );
    }

    pub(crate) fn ack(&self, seq: u64) {
        if let Some(at) = self
            .sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&seq)
        {
            push(
                &mut self.acks.lock().unwrap_or_else(|e| e.into_inner()),
                at.elapsed(),
            );
        }
    }

    pub fn heartbeat_acks(&self) -> Vec<std::time::Duration> {
        self.acks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Промежутки между отправками: стоп сеанса виден, даже если ни один heartbeat не был в полёте.
    pub fn heartbeat_gaps(&self) -> Vec<std::time::Duration> {
        self.sends
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect()
    }
}

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

/// Читается только в режиме supervise при включённой feature.
pub struct SupervisorFaults {
    abort_at: Option<String>,
    hold_gate: bool,
    hold_attach: bool,
    hold_decision: bool,
    survivors: std::sync::atomic::AtomicU32,
    errors: std::sync::atomic::AtomicU32,
}

impl SupervisorFaults {
    pub fn from_env() -> Self {
        Self {
            abort_at: std::env::var("CANTERLOT_FAULT_ABORT_AT").ok(),
            hold_gate: std::env::var("CANTERLOT_FAULT_HOLD_AT").as_deref() == Ok("before_gate"),
            hold_attach: std::env::var("CANTERLOT_FAULT_HOLD_AT").as_deref() == Ok("before_attach"),
            hold_decision: std::env::var("CANTERLOT_FAULT_HOLD_AT").as_deref()
                == Ok("after_decision_intention"),
            errors: std::sync::atomic::AtomicU32::new(
                std::env::var("CANTERLOT_FAULT_TERMINATE_ERRORS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            ),
            survivors: std::sync::atomic::AtomicU32::new(
                std::env::var("CANTERLOT_FAULT_TERMINATE_SURVIVORS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0),
            ),
        }
    }

    pub fn abort_at(&self, point: &str) {
        if self.abort_at.as_deref() == Some(point) {
            std::process::abort();
        }
    }

    pub fn will_abort(&self, point: &str) -> bool {
        self.abort_at.as_deref() == Some(point)
    }

    pub fn gate_held(&self, exec_dir: &std::path::Path) -> bool {
        self.hold_gate && !exec_dir.join("fault_release").exists()
    }
    pub fn attach_held(&self) -> bool {
        self.hold_attach
    }
    pub fn decision_held(&self) -> bool {
        self.hold_decision
    }

    pub fn take_survivor(&self) -> bool {
        self.survivors
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok()
    }

    pub fn take_error(&self) -> bool {
        self.errors
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreAppendPoint {
    Inbox,
    Cj,
    Receipt,
    InboxAck,
    InboxCompact,
    Ownership,
}

#[derive(Clone, Default)]
pub struct StoreProbe {
    snapshots: Arc<Mutex<Vec<StoreSnapshot>>>,
    duplicates: Arc<AtomicU64>,
    scanned_inbox: Arc<AtomicU64>,
    crash: Arc<Mutex<Option<StoreAppendPoint>>>,
}

impl StoreProbe {
    pub fn crash_after(&self, point: StoreAppendPoint) {
        *self.crash.lock().unwrap_or_else(|error| error.into_inner()) = Some(point);
    }

    pub fn scanned_inbox_records(&self) -> u64 {
        self.scanned_inbox.load(Ordering::Relaxed)
    }

    pub(crate) fn scan_inbox(&self) {
        self.scanned_inbox.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn appended(&self, point: StoreAppendPoint) {
        let crash = *self.crash.lock().unwrap_or_else(|error| error.into_inner());
        assert_ne!(crash, Some(point), "simulated crash");
    }

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
