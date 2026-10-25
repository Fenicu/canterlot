use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use canterlot_journal::{Journal, JournalError, JournalId, JournalOptions};
use canterlot_proto::limits::{self, LimitError, MAX_MESSAGE_BYTES};
use canterlot_proto::worker::v1::{
    Command, CommandRejected, CommandStored, JournalCursor, JournalRecord, RecordBody, command,
    record_body,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    Stored { cj_seq: u64 },
    Duplicate,
    Rejected { cj_seq: u64, reason: &'static str },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("journal error: {0}")]
    Journal(#[from] JournalError),
    #[error("CJ is sealed; local recovery is required before connecting")]
    CjSealed,
    #[error("local journal {0} is sealed; local recovery is required")]
    LocalSealed(&'static str),
    #[error("invalid command ID: {0}")]
    InvalidCommandId(#[from] uuid::Error),
    #[error("command exceeds protocol limits: {0:?}")]
    Limits(LimitError),
    #[error("invalid CJ record: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("invalid local metadata: {0}")]
    Metadata(#[from] toml::de::Error),
    #[error("cannot encode local metadata: {0}")]
    EncodeMetadata(#[from] toml::ser::Error),
    #[error("local metadata is not UTF-8: {0}")]
    MetadataUtf8(#[from] std::str::Utf8Error),
}

#[derive(Serialize, Deserialize)]
struct ReceiptEntry {
    command_id: Uuid,
    attempt: u32,
    class: ReceiptClass,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptClass {
    Stored,
    Rejected,
}

#[derive(Default, Serialize, Deserialize)]
struct HubState {
    restore_generation: Option<Uuid>,
}

pub struct WorkerStore {
    data_dir: PathBuf,
    cj: Journal,
    inbox: Journal,
    receipts: Journal,
    index: HashMap<Uuid, ReceiptEntry>,
    hub: HubState,
    #[cfg(test)]
    crash_after: Option<tests::AppendPoint>,
}

impl WorkerStore {
    pub fn open(data_dir: &Path) -> Result<WorkerStore, StoreError> {
        fs::create_dir_all(data_dir)?;
        let cj = open_journal(&data_dir.join("cj"))?;
        if cj.is_sealed().is_some() {
            return Err(StoreError::CjSealed);
        }
        let inbox = open_journal(&data_dir.join("inbox"))?;
        let receipts = open_journal(&data_dir.join("receipts"))?;
        for (name, journal) in [("inbox", &inbox), ("receipts", &receipts)] {
            if journal.is_sealed().is_some() {
                return Err(StoreError::LocalSealed(name));
            }
        }
        let hub = match fs::read_to_string(data_dir.join("hub.toml")) {
            Ok(text) => toml::from_str(&text)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => HubState::default(),
            Err(error) => return Err(error.into()),
        };
        let mut store = Self {
            data_dir: data_dir.to_path_buf(),
            cj,
            inbox,
            receipts,
            index: HashMap::new(),
            hub,
            #[cfg(test)]
            crash_after: tests::take_open_crash_point(),
        };
        store.reconcile()?;
        Ok(store)
    }

    fn reconcile(&mut self) -> Result<(), StoreError> {
        let mut from_seq = self.receipts.first_seq();
        while from_seq <= self.receipts.last_seq() {
            for record in self.receipts.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                let entry: ReceiptEntry = toml::from_str(std::str::from_utf8(&record.payload)?)?;
                self.index.insert(entry.command_id, entry);
                from_seq = record.seq + 1;
            }
        }

        let mut from_seq = self.cj.first_seq();
        while from_seq <= self.cj.last_seq() {
            for record in self.cj.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                from_seq = record.seq + 1;
                let (command_id, attempt, class) =
                    match RecordBody::decode(record.payload.as_slice())?.body {
                        Some(record_body::Body::CommandStored(fact)) => {
                            (fact.command_id, fact.dispatch_attempt, ReceiptClass::Stored)
                        }
                        Some(record_body::Body::CommandRejected(fact)) => (
                            fact.command_id,
                            fact.dispatch_attempt,
                            ReceiptClass::Rejected,
                        ),
                        _ => continue,
                    };
                let command_id = Uuid::parse_str(&command_id)?;
                if !self.index.contains_key(&command_id) {
                    self.append_receipt(ReceiptEntry {
                        command_id,
                        attempt,
                        class,
                    })?;
                }
            }
        }

        let mut from_seq = self.inbox.first_seq();
        while from_seq <= self.inbox.last_seq() {
            for record in self.inbox.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                from_seq = record.seq + 1;
                let cmd = Command::decode(record.payload.as_slice())?;
                let command_id = Uuid::parse_str(&cmd.command_id)?;
                if !self.index.contains_key(&command_id) {
                    self.cj.append(
                        &RecordBody {
                            body: Some(record_body::Body::CommandStored(CommandStored {
                                command_id: cmd.command_id,
                                dispatch_attempt: cmd.dispatch_attempt,
                            })),
                        }
                        .encode_to_vec(),
                    )?;
                    #[cfg(test)]
                    self.after_append(tests::AppendPoint::Cj);
                    self.append_receipt(ReceiptEntry {
                        command_id,
                        attempt: cmd.dispatch_attempt,
                        class: ReceiptClass::Stored,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn append_receipt(&mut self, entry: ReceiptEntry) -> Result<(), StoreError> {
        self.receipts.append(toml::to_string(&entry)?.as_bytes())?;
        #[cfg(test)]
        self.after_append(tests::AppendPoint::Receipt);
        self.index.insert(entry.command_id, entry);
        Ok(())
    }

    pub fn cj_journal_id(&self) -> Uuid {
        self.cj.id().0
    }

    pub fn accept_command(&mut self, cmd: &Command) -> Result<Accept, StoreError> {
        let command_id = Uuid::parse_str(&cmd.command_id)?;
        if self.index.contains_key(&command_id) {
            return Ok(Accept::Duplicate);
        }
        limits::check_deliver_input(cmd).map_err(StoreError::Limits)?;
        let (body, class) = match &cmd.kind {
            None => (
                record_body::Body::CommandRejected(CommandRejected {
                    command_id: cmd.command_id.clone(),
                    dispatch_attempt: cmd.dispatch_attempt,
                    reason: "unsupported_command".into(),
                }),
                ReceiptClass::Rejected,
            ),
            Some(
                command::Kind::DeliverInput(_)
                | command::Kind::CancelInput(_)
                | command::Kind::WithdrawInput(_)
                | command::Kind::Interrupt(_)
                | command::Kind::StartExecution(_)
                | command::Kind::StopExecution(_),
            ) => {
                self.inbox.append(&cmd.encode_to_vec())?;
                #[cfg(test)]
                self.after_append(tests::AppendPoint::Inbox);
                (
                    record_body::Body::CommandStored(CommandStored {
                        command_id: cmd.command_id.clone(),
                        dispatch_attempt: cmd.dispatch_attempt,
                    }),
                    ReceiptClass::Stored,
                )
            }
        };
        let cj_seq = self
            .cj
            .append(&RecordBody { body: Some(body) }.encode_to_vec())?;
        #[cfg(test)]
        self.after_append(tests::AppendPoint::Cj);
        let result = match class {
            ReceiptClass::Stored => Accept::Stored { cj_seq },
            ReceiptClass::Rejected => Accept::Rejected {
                cj_seq,
                reason: "unsupported_command",
            },
        };
        let entry = ReceiptEntry {
            command_id,
            attempt: cmd.dispatch_attempt,
            class,
        };
        self.append_receipt(entry)?;
        Ok(result)
    }

    pub fn cj_cursor(&self) -> JournalCursor {
        JournalCursor {
            journal_id: self.cj_journal_id().to_string(),
            first_stored_seq: self.cj.first_seq(),
            last_seq: self.cj.last_seq(),
            acked_seq_known_to_worker: self.cj.acked_seq(),
        }
    }

    pub fn cj_records_from(
        &self,
        from_seq: u64,
        max_bytes: usize,
    ) -> Result<Vec<JournalRecord>, StoreError> {
        self.cj
            .read_from(from_seq, max_bytes)?
            .into_iter()
            .map(|record| {
                Ok(JournalRecord {
                    seq: record.seq,
                    body: Some(RecordBody::decode(record.payload.as_slice())?),
                })
            })
            .collect()
    }

    pub fn on_cj_ack(&mut self, up_to: u64) -> Result<(), StoreError> {
        self.cj.ack(up_to)?;
        self.cj.compact()?;
        Ok(())
    }

    pub fn restore_generation(&self) -> Option<Uuid> {
        self.hub.restore_generation
    }

    pub fn set_restore_generation(&mut self, g: Uuid) -> Result<(), StoreError> {
        let hub = HubState {
            restore_generation: Some(g),
        };
        let bytes = toml::to_string(&hub)?;
        let temporary = self.data_dir.join(format!(".hub-{}.tmp", Uuid::now_v7()));
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, self.data_dir.join("hub.toml"))?;
            File::open(&self.data_dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        self.hub = hub;
        Ok(())
    }
}

fn open_journal(path: &Path) -> Result<Journal, StoreError> {
    let options = JournalOptions::default();
    if path.join("header").try_exists()? {
        Ok(Journal::open(path, options)?.0)
    } else {
        Ok(Journal::create(path, JournalId(Uuid::now_v7()), options)?)
    }
}

#[cfg(test)]
mod tests;
