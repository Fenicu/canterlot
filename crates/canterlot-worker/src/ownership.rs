use std::{
    collections::HashMap,
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

pub struct OwnershipStore {
    journal: Journal,
    latest: HashMap<Uuid, Ownership>,
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

    pub fn compact(&mut self) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::NeedsReopen);
        }
        let result = (|| {
            self.forget_superseded();
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

    /// Убирает из памяти (а снимком и с диска) записи исполнений, которые больше ничему не
    /// нужны: запись освобождена, каталог исполнения уже убран, а в её разговоре есть
    /// поколение старше. Новое поколение хаб начинает только после итога прежнего, так что
    /// итог такого исполнения давно в PG. Запись старшего поколения остаётся и освобождённой:
    /// по ней отклоняются запуски с устаревшим поколением. Без этого записи всех когда-либо
    /// запущенных исполнений копились бы в памяти и переписывались в каждом снимке.
    fn forget_superseded(&mut self) {
        let mut newest: HashMap<&str, u64> = HashMap::new();
        for record in self.latest.values() {
            let generation = newest.entry(record.conversation_id.as_str()).or_default();
            *generation = (*generation).max(record.generation);
        }
        let superseded: Vec<Uuid> = self
            .latest
            .iter()
            .filter(|(_, record)| {
                record.phase == OwnershipPhase::Released as i32
                    && !record.conversation_id.is_empty()
                    && newest
                        .get(record.conversation_id.as_str())
                        .is_some_and(|generation| record.generation < *generation)
                    && !record.exec_dir.is_empty()
                    && !Path::new(&record.exec_dir).exists()
            })
            .map(|(id, _)| *id)
            .collect();
        for id in superseded {
            self.latest.remove(&id);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn released(conversation: &str, generation: u64, exec_dir: &Path) -> Ownership {
        Ownership {
            execution_id: Uuid::now_v7().to_string(),
            conversation_id: conversation.into(),
            generation,
            phase: OwnershipPhase::Released as i32,
            released_reason: "user_stop".into(),
            exec_dir: exec_dir.to_string_lossy().into_owned(),
            secret_sha256: vec![7; 32],
            ..Default::default()
        }
    }

    /// Записи всех когда-либо запущенных исполнений не копятся: после компактации от
    /// разговора остаётся запись старшего поколения, по которой отклоняются устаревшие запуски.
    #[test]
    fn compaction_drops_released_records_superseded_in_their_conversation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ownership");
        let gone = root.path().join("executions/gone");
        let kept_dir = root.path().join("executions/still-here");
        std::fs::create_dir_all(&kept_dir).unwrap();
        let mut store = OwnershipStore::open(&path).unwrap();
        let busy = Uuid::now_v7().to_string();
        let quiet = Uuid::now_v7().to_string();
        // Каталог старого исполнения ещё не убран: запись нужна уборке.
        let lingering = released(&busy, 1, &kept_dir);
        store.append(lingering.clone()).unwrap();
        let mut old = Vec::new();
        for generation in 2..=200 {
            let record = released(&busy, generation, &gone);
            old.push(record.execution_id.clone());
            store.append(record).unwrap();
        }
        let live = Ownership {
            phase: OwnershipPhase::Attached as i32,
            released_reason: String::new(),
            ..released(&busy, 201, &gone)
        };
        store.append(live.clone()).unwrap();
        let single = released(&quiet, 5, &gone);
        store.append(single.clone()).unwrap();
        store.compact().unwrap();

        let check = |store: &OwnershipStore| {
            assert_eq!(
                store.latest.len(),
                3,
                "one conversation does not keep 200 records"
            );
            for id in &old {
                assert!(store.get(Uuid::parse_str(id).unwrap()).is_none());
            }
            for record in [&lingering, &live, &single] {
                let id = Uuid::parse_str(&record.execution_id).unwrap();
                assert_eq!(store.get(id), Some(record));
            }
            assert_eq!(store.max_generation(&busy), 201);
            assert_eq!(store.max_generation(&quiet), 5);
        };
        check(&store);
        drop(store);
        check(&OwnershipStore::open(&path).unwrap());
    }

    /// Старшее поколение разговора остаётся и освобождённым: без него следующий запуск
    /// с тем же или меньшим поколением прошёл бы проверку.
    #[test]
    fn the_newest_generation_of_a_conversation_survives_compaction() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ownership");
        let gone = root.path().join("executions/gone");
        let mut store = OwnershipStore::open(&path).unwrap();
        let conversation = Uuid::now_v7().to_string();
        for generation in 1..=40 {
            store
                .append(released(&conversation, generation, &gone))
                .unwrap();
        }
        store.compact().unwrap();
        assert_eq!(store.latest.len(), 1);
        assert_eq!(store.max_generation(&conversation), 40);
    }
}
