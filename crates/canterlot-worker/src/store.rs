use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use canterlot_journal::{Journal, JournalError, JournalId, JournalOptions};
use canterlot_proto::limits::{self, LimitError, MAX_MESSAGE_BYTES};
use canterlot_proto::worker::v1::{
    Command, CommandRejected, CommandStored, JournalCursor, JournalRecord, RecordBody, record_body,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::ownership::OwnershipStore;

#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    Stored { cj_seq: u64 },
    Redelivered { cj_seq: u64 },
    Duplicate,
    Stale,
    DroppedInvalidId,
    Rejected { cj_seq: u64, reason: &'static str },
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("insecure_data_dir")]
    InsecureDataDir,
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

#[derive(Clone, Serialize, Deserialize)]
struct ReceiptEntry {
    command_id: String,
    attempt: u32,
    class: ReceiptClass,
    #[serde(default)]
    inbox_seq: Option<u64>,
    #[serde(default)]
    terminal: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    transmitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandState {
    pub attempt: u32,
    pub class: ReceiptClass,
    pub inbox_seq: Option<u64>,
    pub terminal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptClass {
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
    ownership: OwnershipStore,
    index: HashMap<String, ReceiptEntry>,
    hub: HubState,
    #[cfg(test)]
    crash_after: Option<tests::AppendPoint>,
}

impl WorkerStore {
    #[cfg(feature = "fault-injection")]
    pub fn with_probe(mut self, probe: crate::fault_injection::StoreProbe) -> Self {
        self.ownership.set_probe(Some(probe.clone()));
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
        Self::open_inner(
            data_dir,
            #[cfg(feature = "fault-injection")]
            None,
        )
    }

    #[cfg(feature = "fault-injection")]
    pub fn open_with_probe(
        data_dir: &Path,
        probe: crate::fault_injection::StoreProbe,
    ) -> Result<WorkerStore, StoreError> {
        Self::open_inner(data_dir, Some(probe))
    }

    fn open_inner(
        data_dir: &Path,
        #[cfg(feature = "fault-injection")] probe: Option<crate::fault_injection::StoreProbe>,
    ) -> Result<WorkerStore, StoreError> {
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(data_dir)?;
            if fs::metadata(data_dir)?.permissions().mode() & 0o077 != 0 {
                return Err(StoreError::InsecureDataDir);
            }
        }
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
        let ownership = OwnershipStore::open(&data_dir.join("ownership"))?;
        let hub = match fs::read_to_string(data_dir.join("hub.toml")) {
            Ok(text) => toml::from_str(&text)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => HubState::default(),
            Err(error) => return Err(error.into()),
        };
        let mut store = Self {
            #[cfg(feature = "fault-injection")]
            probe,
            poisoned: false,
            #[cfg(test)]
            fail_after: None,
            data_dir: data_dir.to_path_buf(),
            cj,
            inbox,
            receipts,
            ownership,
            index: HashMap::new(),
            hub,
            #[cfg(test)]
            crash_after: tests::take_open_crash_point(),
        };
        #[cfg(feature = "fault-injection")]
        store.ownership.set_probe(store.probe.clone());
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

        let legacy = self
            .index
            .values()
            .any(|entry| !entry.terminal && entry.inbox_seq.is_none());
        let highest = self
            .index
            .values()
            .filter_map(|entry| entry.inbox_seq)
            .max()
            .unwrap_or(0);
        let mut from_seq = if legacy {
            self.inbox.first_seq()
        } else {
            highest.saturating_add(1).max(self.inbox.first_seq())
        };
        let mut pending = Vec::new();
        let mut inbox_seqs = HashMap::new();
        while from_seq <= self.inbox.last_seq() {
            for record in self.inbox.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                from_seq = record.seq + 1;
                #[cfg(feature = "fault-injection")]
                if let Some(probe) = &self.probe {
                    probe.scan_inbox();
                }
                let cmd = Command::decode(record.payload.as_slice())?;
                inbox_seqs.insert(
                    (receipt_id(&cmd.command_id), cmd.dispatch_attempt),
                    record.seq,
                );
                pending.push((record.seq, cmd));
            }
        }

        let mut facts = Vec::new();
        let mut transmitted = HashSet::new();
        let mut from_seq = self.cj.first_seq();
        while from_seq <= self.cj.last_seq() {
            for record in self.cj.read_from(from_seq, MAX_MESSAGE_BYTES)? {
                from_seq = record.seq + 1;
                match RecordBody::decode(record.payload.as_slice())?.body {
                    Some(record_body::Body::CommandStored(fact)) => facts.push((
                        receipt_id(&fact.command_id),
                        fact.dispatch_attempt,
                        ReceiptClass::Stored,
                    )),
                    Some(record_body::Body::CommandRejected(fact)) => facts.push((
                        receipt_id(&fact.command_id),
                        fact.dispatch_attempt,
                        ReceiptClass::Rejected,
                    )),
                    Some(record_body::Body::TransmissionStarted(fact)) => {
                        transmitted.insert(receipt_id(&fact.command_id));
                    }
                    _ => {}
                }
            }
        }
        for (command_id, attempt, class) in facts {
            if self
                .index
                .get(&command_id)
                .is_none_or(|entry| entry.attempt < attempt)
            {
                let inbox_seq = if class == ReceiptClass::Stored {
                    inbox_seqs.get(&(command_id.clone(), attempt)).copied()
                } else {
                    self.index
                        .get(&command_id)
                        .and_then(|entry| entry.inbox_seq)
                };
                let was_transmitted = transmitted.contains(&command_id)
                    || self
                        .index
                        .get(&command_id)
                        .is_some_and(|entry| entry.transmitted);
                let terminal = class == ReceiptClass::Rejected;
                self.append_receipt(ReceiptEntry {
                    command_id,
                    attempt,
                    class,
                    inbox_seq,
                    terminal,
                    transmitted: was_transmitted,
                })?;
            }
        }
        for (inbox_seq, cmd) in pending {
            let command_id = receipt_id(&cmd.command_id);
            if self
                .index
                .get(&command_id)
                .is_none_or(|entry| entry.attempt < cmd.dispatch_attempt)
            {
                self.append_stored_fact(&cmd)?;
                self.append_receipt(ReceiptEntry {
                    transmitted: transmitted.contains(&command_id),
                    command_id,
                    attempt: cmd.dispatch_attempt,
                    class: ReceiptClass::Stored,
                    inbox_seq: Some(inbox_seq),
                    terminal: false,
                })?;
            } else if let Some(entry) = self.index.get(&command_id)
                && entry.attempt == cmd.dispatch_attempt
                && entry.class == ReceiptClass::Stored
                && entry.inbox_seq.is_none()
            {
                let mut entry = entry.clone();
                entry.inbox_seq = Some(inbox_seq);
                self.append_receipt(entry)?;
            }
        }
        let legacy_rejected: Vec<_> = self
            .index
            .values()
            .filter(|entry| entry.class == ReceiptClass::Rejected && !entry.terminal)
            .cloned()
            .collect();
        for mut entry in legacy_rejected {
            entry.terminal = true;
            self.append_receipt(entry)?;
        }
        for command_id in transmitted {
            self.record_transmission(&command_id)?;
        }
        self.release_inbox()?;
        Ok(())
    }

    fn append_receipt(&mut self, entry: ReceiptEntry) -> Result<(), StoreError> {
        self.receipts.append(toml::to_string(&entry)?.as_bytes())?;
        self.append_boundary(AppendBoundary::Receipt)?;
        self.index.insert(entry.command_id.clone(), entry);
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), StoreError> {
        if self.poisoned || self.ownership.is_poisoned() {
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
        let previous = self.index.get(&command_id);
        if let Some(entry) = previous
            && cmd.dispatch_attempt <= entry.attempt
        {
            #[cfg(feature = "fault-injection")]
            {
                if let Some(probe) = &self.probe {
                    probe.duplicate();
                }
                self.record_snapshot()?;
            }
            return Ok(if cmd.dispatch_attempt == entry.attempt {
                Accept::Duplicate
            } else {
                Accept::Stale
            });
        }
        let redelivered = previous.is_some();
        let reason = if previous.is_some_and(|entry| entry.transmitted) {
            Some("already_transmitted")
        } else if limits::check_deliver_input(cmd).is_err() {
            Some("resource_limit")
        } else if cmd.kind.is_none() {
            Some("unsupported_command")
        } else {
            None
        };
        let result = if let Some(reason) = reason {
            self.reject_command(cmd, command_id, reason)
        } else {
            self.append_command(cmd, command_id, redelivered)
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
        self.append_boundary(AppendBoundary::Cj)?;
        let transmitted = self
            .index
            .get(&command_id)
            .is_some_and(|entry| entry.transmitted);
        let inbox_seq = self
            .index
            .get(&command_id)
            .and_then(|entry| entry.inbox_seq);
        self.append_receipt(ReceiptEntry {
            command_id,
            attempt: cmd.dispatch_attempt,
            class: ReceiptClass::Rejected,
            inbox_seq,
            terminal: true,
            transmitted,
        })?;
        Ok(Accept::Rejected { cj_seq, reason })
    }

    fn append_stored_fact(&mut self, cmd: &Command) -> Result<u64, StoreError> {
        let seq = self.cj.append(
            &RecordBody {
                body: Some(record_body::Body::CommandStored(CommandStored {
                    machine_time_ms: machine_time_ms(),
                    command_id: cmd.command_id.clone(),
                    dispatch_attempt: cmd.dispatch_attempt,
                })),
            }
            .encode_to_vec(),
        )?;
        self.append_boundary(AppendBoundary::Cj)?;
        Ok(seq)
    }

    fn append_command(
        &mut self,
        cmd: &Command,
        command_id: String,
        redelivered: bool,
    ) -> Result<Accept, StoreError> {
        let inbox_seq = self.inbox.append(&cmd.encode_to_vec())?;
        self.append_boundary(AppendBoundary::Inbox)?;
        let cj_seq = self.append_stored_fact(cmd)?;
        self.append_receipt(ReceiptEntry {
            command_id,
            attempt: cmd.dispatch_attempt,
            class: ReceiptClass::Stored,
            inbox_seq: Some(inbox_seq),
            terminal: false,
            transmitted: false,
        })?;
        Ok(if redelivered {
            Accept::Redelivered { cj_seq }
        } else {
            Accept::Stored { cj_seq }
        })
    }

    pub fn command_state(&self, command_id: &str) -> Result<Option<CommandState>, StoreError> {
        self.ensure_open()?;
        Ok(self
            .index
            .get(&receipt_id(command_id))
            .map(|entry| CommandState {
                attempt: entry.attempt,
                class: entry.class.clone(),
                inbox_seq: entry.inbox_seq,
                terminal: entry.terminal,
            }))
    }

    pub fn inbox_command(
        &self,
        command_id: &str,
        attempt: u32,
    ) -> Result<Option<Command>, StoreError> {
        self.ensure_open()?;
        let Some(entry) = self.index.get(&receipt_id(command_id)) else {
            return Ok(None);
        };
        let Some(seq) = entry.inbox_seq else {
            return Ok(None);
        };
        if entry.attempt != attempt || entry.terminal || entry.class != ReceiptClass::Stored {
            return Ok(None);
        }
        let record = self.inbox.read_from(seq, 0)?.into_iter().next();
        record
            .map(|record| Command::decode(record.payload.as_slice()).map_err(StoreError::from))
            .transpose()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Непогашенные сохранённые команды в порядке inbox.
    pub fn stored_commands(&self) -> Result<Vec<Command>, StoreError> {
        self.ensure_open()?;
        let mut entries: Vec<u64> = self
            .index
            .values()
            .filter(|entry| !entry.terminal && entry.class == ReceiptClass::Stored)
            .filter_map(|entry| entry.inbox_seq)
            .collect();
        entries.sort_unstable();
        let mut commands = Vec::new();
        for seq in entries {
            if let Some(record) = self.inbox.read_from(seq, 0)?.into_iter().next()
                && record.seq == seq
            {
                commands.push(Command::decode(record.payload.as_slice())?);
            }
        }
        Ok(commands)
    }

    /// Факт `transmission_started` из SJ: следующие попытки команды отклоняются.
    pub fn note_transmitted(&mut self, command_id: &str) -> Result<(), StoreError> {
        self.ensure_open()?;
        let result = self.record_transmission(&receipt_id(command_id));
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    pub fn append_cj_fact(&mut self, body: record_body::Body) -> Result<u64, StoreError> {
        self.ensure_open()?;
        let result = (|| {
            let seq = self.cj.append(
                &RecordBody {
                    body: Some(body.clone()),
                }
                .encode_to_vec(),
            )?;
            self.append_boundary(AppendBoundary::Cj)?;
            if let record_body::Body::TransmissionStarted(fact) = body {
                self.record_transmission(&receipt_id(&fact.command_id))?;
            }
            Ok(seq)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn record_transmission(&mut self, command_id: &str) -> Result<(), StoreError> {
        if let Some(entry) = self.index.get(command_id)
            && !entry.transmitted
        {
            let mut entry = entry.clone();
            entry.transmitted = true;
            self.append_receipt(entry)?;
        }
        Ok(())
    }

    pub fn mark_terminal(&mut self, command_id: &str, attempt: u32) -> Result<(), StoreError> {
        self.ensure_open()?;
        let result = (|| {
            if let Some(entry) = self.index.get(&receipt_id(command_id))
                && entry.attempt == attempt
                && !entry.terminal
            {
                let mut entry = entry.clone();
                entry.terminal = true;
                self.append_receipt(entry)?;
            } else if !self.index.contains_key(&receipt_id(command_id))
                && Uuid::parse_str(command_id).is_ok()
            {
                // Отмена может прийти раньше самого ввода; сохраняем её после compaction CJ.
                self.append_receipt(ReceiptEntry {
                    command_id: receipt_id(command_id),
                    attempt,
                    class: ReceiptClass::Stored,
                    inbox_seq: None,
                    terminal: true,
                    transmitted: false,
                })?;
            }
            self.release_inbox()
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn release_inbox(&mut self) -> Result<(), StoreError> {
        let floor = self
            .index
            .values()
            .filter(|entry| !entry.terminal)
            .filter_map(|entry| entry.inbox_seq)
            .min()
            .map_or(self.inbox.last_seq(), |seq| seq - 1);
        self.inbox.ack(floor)?;
        self.append_boundary(AppendBoundary::InboxAck)?;
        self.inbox.compact()?;
        self.append_boundary(AppendBoundary::InboxCompact)?;
        Ok(())
    }

    pub fn ownership(&self) -> &OwnershipStore {
        &self.ownership
    }

    pub fn ownership_mut(&mut self) -> Result<&mut OwnershipStore, StoreError> {
        self.ensure_open()?;
        Ok(&mut self.ownership)
    }

    fn append_boundary(&self, point: AppendBoundary) -> Result<(), StoreError> {
        #[cfg(test)]
        {
            let test_point = match point {
                AppendBoundary::Inbox => tests::AppendPoint::Inbox,
                AppendBoundary::Cj => tests::AppendPoint::Cj,
                AppendBoundary::Receipt => tests::AppendPoint::Receipt,
                AppendBoundary::InboxAck => tests::AppendPoint::InboxAck,
                AppendBoundary::InboxCompact => tests::AppendPoint::InboxCompact,
            };
            self.after_append(test_point);
            if self.fail_after == Some(test_point) {
                return Err(io::Error::other("injected write failure").into());
            }
        }
        #[cfg(feature = "fault-injection")]
        if let Some(probe) = &self.probe {
            use crate::fault_injection::StoreAppendPoint;
            probe.appended(match point {
                AppendBoundary::Inbox => StoreAppendPoint::Inbox,
                AppendBoundary::Cj => StoreAppendPoint::Cj,
                AppendBoundary::Receipt => StoreAppendPoint::Receipt,
                AppendBoundary::InboxAck => StoreAppendPoint::InboxAck,
                AppendBoundary::InboxCompact => StoreAppendPoint::InboxCompact,
            });
        }
        let _ = point;
        Ok(())
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
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = OpenOptions::new()
                .mode(0o600)
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

fn machine_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn receipt_id(raw: &str) -> String {
    Uuid::parse_str(raw).map_or_else(|_| raw.to_owned(), |id| id.to_string())
}

pub(crate) fn open_journal(path: &Path) -> Result<Journal, StoreError> {
    let options = JournalOptions::default();
    if path.join("header").try_exists()? {
        Ok(Journal::open(path, options)?.0)
    } else {
        Ok(Journal::create(path, JournalId(Uuid::now_v7()), options)?)
    }
}

#[derive(Clone, Copy)]
enum AppendBoundary {
    Inbox,
    Cj,
    Receipt,
    InboxAck,
    InboxCompact,
}

#[cfg(test)]
mod tests;
