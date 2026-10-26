use std::{collections::VecDeque, path::Path};

use canterlot_journal::{Journal, JournalId, JournalOptions};
use canterlot_proto::{
    limits::{MAX_EVENT_BATCH_BYTES, MAX_SJ_BATCH_RECORDS, check_record_batch},
    worker::{
        ipc::SjBatch,
        v1::{JournalRecord, JournalRecords, RecordBody, record_body::Body},
    },
};
use prost::Message;
use uuid::Uuid;

use super::SupervisorError;

const WINDOW: usize = 4;

struct Ship {
    next: u64,
    inflight: VecDeque<u64>,
}

pub(super) struct Sj {
    journal: Journal,
    journal_id: Uuid,
    ship: Option<Ship>,
}

impl Sj {
    pub fn create(exec_dir: &Path) -> Result<Sj, SupervisorError> {
        let journal_id = Uuid::now_v7();
        let journal = Journal::create(
            &exec_dir.join("sj"),
            JournalId(journal_id),
            JournalOptions::default(),
        )?;
        Ok(Sj {
            journal,
            journal_id,
            ship: None,
        })
    }

    pub fn journal_id(&self) -> Uuid {
        self.journal_id
    }

    pub fn last_seq(&self) -> u64 {
        self.journal.last_seq()
    }

    pub fn acked_seq(&self) -> u64 {
        self.journal.acked_seq()
    }

    pub fn append(&mut self, body: Body) -> Result<u64, SupervisorError> {
        let payload = RecordBody { body: Some(body) }.encode_to_vec();
        Ok(self.journal.append(&payload)?)
    }

    pub fn shipping(&self) -> bool {
        self.ship.is_some()
    }

    pub fn enable_ship(&mut self, committed: u64) {
        self.ship = Some(Ship {
            next: committed.saturating_add(1),
            inflight: VecDeque::new(),
        });
    }

    pub fn disable_ship(&mut self) {
        self.ship = None;
    }

    pub fn fully_acked(&self) -> bool {
        self.journal.acked_seq() >= self.journal.last_seq()
    }

    /// Ack за пределами журнала игнорируется: хаб не может подтвердить ненаписанное.
    pub fn ack(&mut self, up_to: u64) -> Result<(), SupervisorError> {
        if up_to > self.journal.last_seq() {
            return Ok(());
        }
        if let Some(ship) = &mut self.ship {
            while ship.inflight.front().is_some_and(|last| *last <= up_to) {
                ship.inflight.pop_front();
            }
            ship.next = ship.next.max(up_to + 1);
        }
        self.journal.ack(up_to)?;
        self.journal.compact()?;
        Ok(())
    }

    pub fn next_batch(&mut self) -> Result<Option<SjBatch>, SupervisorError> {
        let Some(ship) = &mut self.ship else {
            return Ok(None);
        };
        if ship.inflight.len() >= WINDOW {
            return Ok(None);
        }
        let from = ship.next.max(self.journal.first_seq());
        if from > self.journal.last_seq() {
            return Ok(None);
        }
        let mut batch = JournalRecords {
            journal_id: self.journal_id.to_string(),
            records: Vec::new(),
        };
        for record in self.journal.read_from(from, MAX_EVENT_BATCH_BYTES)? {
            if batch.records.len() == MAX_SJ_BATCH_RECORDS {
                break;
            }
            batch.records.push(JournalRecord {
                seq: record.seq,
                body: Some(RecordBody::decode(record.payload.as_slice())?),
            });
            if check_record_batch(&batch).is_err() && batch.records.len() > 1 {
                batch.records.pop();
                break;
            }
        }
        let Some(last) = batch.records.last().map(|record| record.seq) else {
            return Ok(None);
        };
        ship.next = last + 1;
        ship.inflight.push_back(last);
        Ok(Some(SjBatch {
            journal_id: batch.journal_id,
            records: batch.records,
        }))
    }
}
