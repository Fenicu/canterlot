use std::{collections::HashMap, path::Path};

use canterlot_journal::Journal;
use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::ipc::{Ownership, OwnershipPhase},
};
use prost::Message;
use uuid::Uuid;

use crate::store::{StoreError, open_journal};

pub struct OwnershipStore {
    journal: Journal,
    latest: HashMap<Uuid, Ownership>,
    poisoned: bool,
    #[cfg(feature = "fault-injection")]
    probe: Option<crate::fault_injection::StoreProbe>,
}

impl OwnershipStore {
    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        let journal = open_journal(path)?;
        if journal.is_sealed().is_some() {
            return Err(StoreError::LocalSealed("ownership"));
        }
        let mut latest = HashMap::new();
        let mut from_seq = journal.first_seq();
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

    pub fn live(&self) -> Vec<&Ownership> {
        self.latest
            .values()
            .filter(|record| record.phase != OwnershipPhase::Released as i32)
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
