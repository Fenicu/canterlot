use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use canterlot_journal::Journal;
use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::ipc::{Ownership, OwnershipPhase},
};
use prost::Message;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::{StoreError, open_journal, write_private_atomic};

#[derive(Default, Deserialize, Serialize)]
struct Snapshot {
    seq: u64,
    records: Vec<Vec<u8>>,
}

#[derive(Default, Deserialize, Serialize)]
struct TerminalAcks {
    executions: HashSet<Uuid>,
}

pub struct OwnershipStore {
    journal: Journal,
    latest: HashMap<Uuid, Ownership>,
    terminal_acked: HashSet<Uuid>,
    terminal_path: PathBuf,
    poisoned: bool,
    snapshot_path: PathBuf,
    #[cfg(feature = "fault-injection")]
    probe: Option<crate::fault_injection::StoreProbe>,
}

impl OwnershipStore {
    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        let journal = open_journal(path)?;
        if journal.is_sealed().is_some() {
            return Err(StoreError::LocalSealed("ownership"));
        }
        let snapshot_path = path.join("snapshot.toml");
        let snapshot: Snapshot = match std::fs::read_to_string(&snapshot_path) {
            Ok(text) => toml::from_str(&text)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
            Err(error) => return Err(error.into()),
        };
        let terminal_path = path.join("terminal.toml");
        let terminal_acked = match std::fs::read_to_string(&terminal_path) {
            Ok(text) => toml::from_str::<TerminalAcks>(&text)?.executions,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(error) => return Err(error.into()),
        };
        let mut latest = HashMap::new();
        for bytes in snapshot.records {
            let record = Ownership::decode(bytes.as_slice())?;
            latest.insert(Uuid::parse_str(&record.execution_id)?, record);
        }
        let mut from_seq = journal.first_seq().max(snapshot.seq.saturating_add(1));
        while from_seq <= journal.last_seq() {
            for record in journal.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                from_seq = record.seq + 1;
                let ownership = Ownership::decode(record.payload.as_slice())?;
                latest.insert(Uuid::parse_str(&ownership.execution_id)?, ownership);
            }
        }
        Ok(Self {
            journal,
            latest,
            terminal_acked,
            terminal_path,
            poisoned: false,
            snapshot_path,
            #[cfg(feature = "fault-injection")]
            probe: None,
        })
    }

    pub fn append(&mut self, record: Ownership) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::NeedsReopen);
        }
        let result = (|| {
            let id = Uuid::parse_str(&record.execution_id)?;
            self.journal.append(&record.encode_to_vec())?;
            #[cfg(feature = "fault-injection")]
            if let Some(probe) = &self.probe {
                probe.appended(crate::fault_injection::StoreAppendPoint::Ownership);
            }
            self.latest.insert(id, record);
            if self.journal.last_seq() - self.journal.acked_seq()
                >= (self.latest.len() as u64 * 2).max(32)
            {
                self.compact()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub fn get(&self, execution_id: Uuid) -> Option<&Ownership> {
        self.latest.get(&execution_id)
    }

    /// Терминальные факты CJ, подтверждённые хабом: только такие записи можно забыть.
    pub(crate) fn acknowledge_terminal(
        &mut self,
        executions: impl IntoIterator<Item = Uuid>,
    ) -> Result<(), StoreError> {
        let before = self.terminal_acked.len();
        self.terminal_acked.extend(executions);
        if self.terminal_acked.len() != before {
            write_private_atomic(
                &self.terminal_path,
                toml::to_string(&TerminalAcks {
                    executions: self.terminal_acked.clone(),
                })?
                .as_bytes(),
            )?;
        }
        Ok(())
    }

    pub fn compact(&mut self) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::NeedsReopen);
        }
        let result = (|| {
            self.latest
                .retain(|id, record| !retired(id, record, &self.terminal_acked));
            let before = self.terminal_acked.len();
            self.terminal_acked
                .retain(|id| self.latest.contains_key(id));
            if self.terminal_acked.len() != before {
                write_private_atomic(
                    &self.terminal_path,
                    toml::to_string(&TerminalAcks {
                        executions: self.terminal_acked.clone(),
                    })?
                    .as_bytes(),
                )?;
            }
            let snapshot = Snapshot {
                seq: self.journal.last_seq(),
                records: self.latest.values().map(Message::encode_to_vec).collect(),
            };
            write_private_atomic(&self.snapshot_path, toml::to_string(&snapshot)?.as_bytes())?;
            self.journal.ack(snapshot.seq)?;
            self.journal.compact()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub fn live(&self) -> Vec<&Ownership> {
        self.latest
            .values()
            .filter(|record| {
                record.phase != OwnershipPhase::Released as i32
                    && record.released_reason != "owner_lost_reported"
            })
            .collect()
    }

    /// Записи, по которым процесс ещё может быть жив, в том числе потерянные.
    pub(crate) fn all(&self) -> Vec<Ownership> {
        self.latest.values().cloned().collect()
    }

    pub(crate) fn unreleased(&self) -> Vec<Ownership> {
        self.latest
            .values()
            .filter(|record| record.phase != OwnershipPhase::Released as i32)
            .cloned()
            .collect()
    }

    pub(crate) fn released(&self) -> Vec<Ownership> {
        self.latest
            .values()
            .filter(|record| record.phase == OwnershipPhase::Released as i32)
            .cloned()
            .collect()
    }

    pub fn max_generation(&self, conversation_id: &str) -> u64 {
        self.latest
            .values()
            .filter(|record| record.conversation_id == conversation_id)
            .map(|record| record.generation)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    #[cfg(feature = "fault-injection")]
    pub(crate) fn set_probe(&mut self, probe: Option<crate::fault_injection::StoreProbe>) {
        self.probe = probe;
    }
}

fn retired(id: &Uuid, record: &Ownership, terminal_acked: &HashSet<Uuid>) -> bool {
    record.phase == OwnershipPhase::Released as i32
        && !record.exec_dir.is_empty()
        && !Path::new(&record.exec_dir).exists()
        && terminal_acked.contains(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_drops_only_released_acknowledged_records_without_exec_dir() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ownership");
        let existing = root.path().join("execution");
        std::fs::create_dir(&existing).unwrap();
        let mut store = OwnershipStore::open(&path).unwrap();
        let record = |id: Uuid, phase: OwnershipPhase, exec_dir: &Path| Ownership {
            execution_id: id.to_string(),
            generation: 1,
            phase: phase as i32,
            exec_dir: exec_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let kept_existing = record(Uuid::now_v7(), OwnershipPhase::Released, &existing);
        let kept_unacknowledged = record(
            Uuid::now_v7(),
            OwnershipPhase::Released,
            &root.path().join("missing"),
        );
        let dropped = record(
            Uuid::now_v7(),
            OwnershipPhase::Released,
            &root.path().join("missing"),
        );
        let live = record(
            Uuid::now_v7(),
            OwnershipPhase::Attached,
            &root.path().join("missing"),
        );
        let no_terminal = record(
            Uuid::now_v7(),
            OwnershipPhase::Released,
            &root.path().join("missing"),
        );
        for record in [
            &kept_existing,
            &kept_unacknowledged,
            &dropped,
            &live,
            &no_terminal,
        ] {
            store.append(record.clone()).unwrap();
        }

        store
            .acknowledge_terminal([
                Uuid::parse_str(&kept_existing.execution_id).unwrap(),
                Uuid::parse_str(&dropped.execution_id).unwrap(),
                Uuid::parse_str(&live.execution_id).unwrap(),
            ])
            .unwrap();
        store.compact().unwrap();

        for kept in [&kept_existing, &kept_unacknowledged, &live, &no_terminal] {
            assert_eq!(
                store.get(Uuid::parse_str(&kept.execution_id).unwrap()),
                Some(kept)
            );
        }
        assert!(
            store
                .get(Uuid::parse_str(&dropped.execution_id).unwrap())
                .is_none()
        );
        drop(store);

        let store = OwnershipStore::open(&path).unwrap();
        assert!(
            store
                .get(Uuid::parse_str(&dropped.execution_id).unwrap())
                .is_none()
        );
        assert!(
            store
                .get(Uuid::parse_str(&live.execution_id).unwrap())
                .is_some()
        );
    }
    #[test]
    fn compaction_preserves_latest_records_and_generation_after_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ownership");
        let mut store = OwnershipStore::open(&path).unwrap();
        let id = Uuid::now_v7();
        let conversation = Uuid::now_v7().to_string();
        for generation in 1..=100 {
            store
                .append(Ownership {
                    execution_id: id.to_string(),
                    conversation_id: conversation.clone(),
                    generation,
                    phase: OwnershipPhase::Attached as i32,
                    ..Default::default()
                })
                .unwrap();
        }
        assert!(store.journal.first_seq() > 1);
        assert!(store.journal.last_seq() - store.journal.first_seq() < 32);
        drop(store);
        let store = OwnershipStore::open(&path).unwrap();
        assert_eq!(store.max_generation(&conversation), 100);
        assert_eq!(store.live().len(), 1);
        assert_eq!(store.get(id).unwrap().generation, 100);
    }
}
