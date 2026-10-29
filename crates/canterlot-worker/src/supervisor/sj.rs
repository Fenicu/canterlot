use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
};

use canterlot_journal::{Journal, JournalId, JournalOptions};
use canterlot_proto::{
    limits::{MAX_EVENT_BATCH_BYTES, MAX_SJ_BATCH_RECORDS, check_record_batch},
    worker::{
        ipc::SjBatch,
        v1::{JournalRecord, JournalRecords, RecordBody, RequestOpened, record_body::Body},
    },
};
use prost::Message;
use uuid::Uuid;

use super::SupervisorError;

const WINDOW: usize = 4;

pub(super) struct RequestState {
    pub opened: RequestOpened,
    pub transmission: Option<String>,
    pub terminal: bool,
}

struct Ship {
    next: u64,
    inflight: VecDeque<u64>,
}

pub(super) struct Sj {
    journal: Journal,
    journal_id: Uuid,
    ship: Option<Ship>,
    pending: Vec<Vec<u8>>,
    requests: BTreeMap<String, RequestState>,
    closed_reason: Option<String>,
    #[cfg(test)]
    commits: usize,
}

impl Sj {
    pub fn create(exec_dir: &Path) -> Result<Sj, SupervisorError> {
        let journal = if exec_dir.join("sj").try_exists()? {
            Journal::open(&exec_dir.join("sj"), JournalOptions::default())?.0
        } else {
            Journal::create(
                &exec_dir.join("sj"),
                JournalId(Uuid::now_v7()),
                JournalOptions::default(),
            )?
        };
        let journal_id = journal.id().0;
        let mut sj = Sj {
            journal,
            journal_id,
            ship: None,
            pending: Vec::new(),
            requests: BTreeMap::new(),
            closed_reason: None,
            #[cfg(test)]
            commits: 0,
        };
        let mut from = sj.journal.first_seq();
        while from <= sj.last_seq() {
            for record in sj.journal.read_from(from, MAX_EVENT_BATCH_BYTES)? {
                from = record.seq + 1;
                if let Some(body) = RecordBody::decode(record.payload.as_slice())?.body {
                    sj.observe(&body);
                }
            }
        }
        Ok(sj)
    }

    fn observe(&mut self, body: &Body) {
        match body {
            Body::JournalClosed(f) => self.closed_reason = Some(f.reason.clone()),
            Body::RequestOpened(opened) => {
                self.requests
                    .entry(opened.request_id.clone())
                    .or_insert_with(|| RequestState {
                        opened: opened.clone(),
                        transmission: None,
                        terminal: false,
                    });
            }
            Body::DecisionTransmissionStarted(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id) {
                    request.transmission = Some(f.decision_id.clone());
                }
            }
            Body::DecisionApplied(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id)
                    && request.transmission.as_deref() == Some(&f.decision_id)
                {
                    request.terminal = true;
                }
            }
            Body::DecisionNotApplied(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id)
                    && (f.reason == "request_closed"
                        || request.transmission.as_deref() == Some(&f.decision_id))
                    && !matches!(
                        f.reason.as_str(),
                        "already_transmitted" | "stale_generation" | "invalid_decision"
                    )
                {
                    request.terminal = true;
                }
            }
            Body::RequestClosed(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id) {
                    request.terminal = true;
                }
            }
            _ => {}
        }
    }

    pub fn request(&self, id: &str) -> Option<&RequestState> {
        self.requests.get(id)
    }

    pub fn closed_reason(&self) -> Option<String> {
        self.closed_reason.clone()
    }

    pub fn open_requests(&self) -> Vec<RequestOpened> {
        if self.closed_reason.is_some() {
            return Vec::new();
        }
        self.requests
            .values()
            .filter(|r| !r.terminal)
            .map(|r| r.opened.clone())
            .collect()
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
        if self.closed_reason.is_some() {
            return Err(SupervisorError::Spec);
        }
        self.observe(&body);
        let payload = RecordBody { body: Some(body) }.encode_to_vec();
        self.pending.push(payload);
        Ok(self.journal.last_seq() + self.pending.len() as u64)
    }

    pub fn flush(&mut self) -> Result<(), SupervisorError> {
        if !self.pending.is_empty() {
            let payloads: Vec<&[u8]> = self.pending.iter().map(Vec::as_slice).collect();
            self.journal.append_batch(&payloads)?;
            self.pending.clear();
            #[cfg(test)]
            {
                self.commits += 1;
            }
        }
        Ok(())
    }

    pub fn intent(&mut self, body: Body) -> Result<(), SupervisorError> {
        self.flush()?;
        self.append(body)?;
        self.flush()
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
        // Состояние запросов и запрет повтора нужны до удаления каталога исполнения.
        if self.requests.is_empty() {
            self.journal.compact()?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_shares_commit_but_intent_has_its_own_commit() {
        let root = tempfile::tempdir().unwrap();
        let mut sj = Sj::create(root.path()).unwrap();
        for _ in 0..3 {
            sj.append(Body::PhaseChanged(Default::default())).unwrap();
        }
        assert_eq!(sj.journal.last_seq(), 0);
        sj.flush().unwrap();
        assert_eq!((sj.last_seq(), sj.commits), (3, 1));
        sj.append(Body::PhaseChanged(Default::default())).unwrap();
        sj.intent(Body::TransmissionStarted(Default::default()))
            .unwrap();
        assert_eq!((sj.last_seq(), sj.commits), (5, 3));
        assert!(matches!(
            RecordBody::decode(
                sj.journal.read_from(5, usize::MAX).unwrap()[0]
                    .payload
                    .as_slice()
            )
            .unwrap()
            .body,
            Some(Body::TransmissionStarted(_))
        ));
    }
}
