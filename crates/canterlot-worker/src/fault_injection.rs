use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use canterlot_proto::worker::v1::{Command, JournalCursor, JournalRecord};

#[derive(Clone, Debug)]
pub struct StoreSnapshot {
    pub at: Instant,
    pub cursor: JournalCursor,
    pub inbox: Vec<Command>,
    pub cj: Vec<JournalRecord>,
    pub receipt_count: u64,
}

#[derive(Clone, Default)]
pub struct StoreProbe {
    snapshots: Arc<Mutex<Vec<StoreSnapshot>>>,
    duplicates: Arc<AtomicU64>,
}

impl StoreProbe {
    pub fn snapshots(&self) -> Vec<StoreSnapshot> {
        self.snapshots.lock().unwrap().clone()
    }

    pub fn duplicate_count(&self) -> u64 {
        self.duplicates.load(Ordering::Relaxed)
    }

    pub(crate) fn duplicate(&self) {
        self.duplicates.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record(&self, snapshot: StoreSnapshot) {
        self.snapshots.lock().unwrap().push(snapshot);
    }
}
