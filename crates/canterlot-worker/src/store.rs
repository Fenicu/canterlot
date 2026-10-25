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
    DroppedInvalidId,
    Rejected { cj_seq: u64, reason: &'static str },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store needs to be reopened after a write failure")]
    NeedsReopen,
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
    command_id: String,
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
    #[cfg(feature = "fault-injection")]
    probe: Option<crate::fault_injection::StoreProbe>,
    poisoned: bool,
    #[cfg(test)]
    fail_after: Option<tests::AppendPoint>,
    data_dir: PathBuf,
    cj: Journal,
    inbox: Journal,
    receipts: Journal,
    index: HashMap<String, ReceiptEntry>,
    hub: HubState,
    #[cfg(test)]
    crash_after: Option<tests::AppendPoint>,
}

impl WorkerStore {
    #[cfg(feature = "fault-injection")]
    pub fn with_probe(mut self, probe: crate::fault_injection::StoreProbe) -> Self {
        self.probe = Some(probe);
        if self.record_snapshot().is_err() {
            eprintln!("fault injection: initial store snapshot failed");
        }
        self
    }

    #[cfg(feature = "fault-injection")]
    pub fn fault_snapshot(&self) -> Result<crate::fault_injection::StoreSnapshot, StoreError> {
        Ok(crate::fault_injection::StoreSnapshot {
            at: std::time::Instant::now(),
            cursor: self.cj_cursor()?,
            inbox: self
                .inbox
                .read_from(self.inbox.first_seq(), usize::MAX)?
                .into_iter()
                .map(|record| {
                    Command::decode(record.payload.as_slice())
                        .map(|command| crate::fault_injection::CommandTrace::from(&command))
                        .map_err(StoreError::from)
                })
                .collect::<Result<_, _>>()?,
            cj: self
                .cj_records_from(self.cj.first_seq(), usize::MAX)?
                .iter()
                .map(crate::fault_injection::RecordTrace::from)
                .collect(),
            receipt_count: self.receipts.last_seq(),
        })
    }

    #[cfg(feature = "fault-injection")]
    fn record_snapshot(&self) -> Result<(), StoreError> {
        if let Some(probe) = &self.probe {
            probe.record(self.fault_snapshot()?);
        }
        Ok(())
    }

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
            #[cfg(feature = "fault-injection")]
            probe: None,
            poisoned: false,
            #[cfg(test)]
            fail_after: None,
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
                self.index.insert(entry.command_id.clone(), entry);
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
                let command_id = receipt_id(&command_id);
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
                let command_id = receipt_id(&cmd.command_id);
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
        self.index.insert(entry.command_id.clone(), entry);
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), StoreError> {
        if self.poisoned {
            Err(StoreError::NeedsReopen)
        } else {
            Ok(())
        }
    }

    pub fn cj_journal_id(&self) -> Result<Uuid, StoreError> {
        self.ensure_open()?;
        Ok(self.cj.id().0)
    }

    pub fn accept_command(&mut self, cmd: &Command) -> Result<Accept, StoreError> {
        self.ensure_open()?;
        let Ok(id) = Uuid::parse_str(&cmd.command_id) else {
            tracing::warn!(error_kind = "invalid_command_id", "command dropped");
            return Ok(Accept::DroppedInvalidId);
        };
        let command_id = id.to_string();
        if self.index.contains_key(&command_id) {
            #[cfg(feature = "fault-injection")]
            {
                if let Some(probe) = &self.probe {
                    probe.duplicate();
                }
                self.record_snapshot()?;
            }
            return Ok(Accept::Duplicate);
        }
        let reason = if limits::check_deliver_input(cmd).is_err() {
            Some("resource_limit")
        } else {
            None
        };
        let result = if let Some(reason) = reason {
            self.reject_command(cmd, command_id, reason)
        } else {
            self.append_command(cmd, command_id)
        };
        if result.is_err() {
            self.poisoned = true;
        }
        #[cfg(feature = "fault-injection")]
        if result.is_ok() {
            self.record_snapshot()?;
        }
        result
    }

    fn reject_command(
        &mut self,
        cmd: &Command,
        command_id: String,
        reason: &'static str,
    ) -> Result<Accept, StoreError> {
        let cj_seq = self.cj.append(
            &RecordBody {
                body: Some(record_body::Body::CommandRejected(CommandRejected {
                    command_id: cmd.command_id.clone(),
                    dispatch_attempt: cmd.dispatch_attempt,
                    reason: reason.into(),
                })),
            }
            .encode_to_vec(),
        )?;
        #[cfg(test)]
        self.after_append(tests::AppendPoint::Cj);
        self.append_receipt(ReceiptEntry {
            command_id,
            attempt: cmd.dispatch_attempt,
            class: ReceiptClass::Rejected,
        })?;
        Ok(Accept::Rejected { cj_seq, reason })
    }

    fn append_command(&mut self, cmd: &Command, command_id: String) -> Result<Accept, StoreError> {
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
                #[cfg(test)]
                if self.fail_after == Some(tests::AppendPoint::Inbox) {
                    return Err(io::Error::other("injected write failure").into());
                }
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

    pub fn cj_cursor(&self) -> Result<JournalCursor, StoreError> {
        self.ensure_open()?;
        Ok(JournalCursor {
            journal_id: self.cj_journal_id()?.to_string(),
            first_stored_seq: self.cj.first_seq(),
            last_seq: self.cj.last_seq(),
            acked_seq_known_to_worker: self.cj.acked_seq(),
        })
    }

    pub fn cj_records_from(
        &self,
        from_seq: u64,
        max_bytes: usize,
    ) -> Result<Vec<JournalRecord>, StoreError> {
        self.ensure_open()?;
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
        self.ensure_open()?;
        let result = (|| {
            self.cj.ack(up_to)?;
            self.cj.compact()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        #[cfg(feature = "fault-injection")]
        if result.is_ok() {
            self.record_snapshot()?;
        }
        result
    }

    pub fn restore_generation(&self) -> Result<Option<Uuid>, StoreError> {
        self.ensure_open()?;
        Ok(self.hub.restore_generation)
    }

    pub fn set_restore_generation(&mut self, g: Uuid) -> Result<(), StoreError> {
        self.ensure_open()?;
        let result = self.write_restore_generation(g);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn write_restore_generation(&mut self, g: Uuid) -> Result<(), StoreError> {
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

fn receipt_id(raw: &str) -> String {
    Uuid::parse_str(raw).map_or_else(|_| raw.to_owned(), |id| id.to_string())
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
