#![forbid(unsafe_code)]

mod error;
mod format;
mod read_only;
mod segment;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub use error::JournalError;
use format::RECORD_HEADER_BYTES;
pub use read_only::ReadOnlyJournal;
use segment::{RecordPosition, Segment};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JournalId(pub uuid::Uuid);

#[derive(Clone, Copy, Debug)]
pub struct JournalOptions {
    pub segment_max_bytes: u64,
    pub max_record_bytes: u32,
}

impl Default for JournalOptions {
    fn default() -> Self {
        Self {
            segment_max_bytes: 8 * 1024 * 1024,
            max_record_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct CorruptedTail {
    pub from_seq: u64,
    pub quarantine_path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct OpenReport {
    pub corrupted_tail: Option<CorruptedTail>,
}

#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    _lock: JournalLock,
    id: JournalId,
    options: JournalOptions,
    segments: Vec<Segment>,
    last_seq: u64,
    acked_seq: u64,
    sealed_from: Option<u64>,
    failed: bool,
    #[cfg(test)]
    write_hook: tests::WriteHook,
}

impl Journal {
    pub fn create(
        dir: &Path,
        id: JournalId,
        opts: JournalOptions,
    ) -> Result<Journal, JournalError> {
        Self::create_inner(
            dir,
            id,
            opts,
            #[cfg(test)]
            |_| Ok(()),
        )
    }

    fn create_inner(
        dir: &Path,
        id: JournalId,
        opts: JournalOptions,
        #[cfg(test)] before_header: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<Journal, JournalError> {
        if dir.join("lock").try_exists()? {
            let _lock = lock_directory(dir)?;
        }
        match fs::read_dir(dir) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "journal directory is not empty",
                    )
                    .into());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let parent = dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let staging = parent.join(format!(".canterlot-journal-{}.tmp", uuid::Uuid::now_v7()));
        segment::private_dir(&staging)?;
        let lock = lock_directory(&staging)?;
        lock.0.sync_all()?;
        let mut first = Segment::create(&staging, 1)?;
        segment::atomic_replace(&staging, "ack", &format::encode_ack(0))?;
        #[cfg(test)]
        before_header(&staging)?;
        segment::atomic_replace(
            &staging,
            "header",
            &format::encode_header(id, opts.max_record_bytes),
        )?;
        if let Err(error) = fs::rename(&staging, dir) {
            if dir.join("lock").try_exists()? {
                let _lock = lock_directory(dir)?;
            }
            return Err(error.into());
        }
        segment::sync_dir(parent)?;
        first.path = dir.join(first.path.file_name().unwrap());
        Ok(Self {
            dir: dir.to_path_buf(),
            _lock: lock,
            id,
            options: opts,
            segments: vec![first],
            last_seq: 0,
            acked_seq: 0,
            sealed_from: None,
            failed: false,
            #[cfg(test)]
            write_hook: tests::WriteHook::default(),
        })
    }

    pub fn open(
        dir: &Path,
        mut opts: JournalOptions,
    ) -> Result<(Journal, OpenReport), JournalError> {
        let (id, max_record_bytes) =
            format::decode_header(&segment::read_metadata(dir, "header")?)?;
        let lock = lock_directory(dir)?;
        opts.max_record_bytes = max_record_bytes;
        let acked_seq = format::decode_ack(&segment::read_metadata(dir, "ack")?)?;
        let mut sealed_from = match fs::read(dir.join("sealed")) {
            Ok(bytes) => {
                let from_seq = format::decode_ack(&bytes)?;
                if from_seq == 0 {
                    return Err(JournalError::NotAJournal);
                }
                Some(from_seq)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let paths = segment::list(dir)?;
        let mut last_seq = paths[0].0 - 1;
        let mut segments = Vec::new();
        let mut report = OpenReport::default();
        for (index, (first_seq, path)) in paths.iter().enumerate() {
            let (mut segment, corrupted_at) = Segment::scan(
                path.clone(),
                *first_seq,
                &mut last_seq,
                opts.max_record_bytes,
            )?;
            if let Some(offset) = corrupted_at {
                let from_seq = next_seq(last_seq)?;
                let sealed = sealed_from.map_or(from_seq, |prior| prior.min(from_seq));
                segment::atomic_replace(dir, "sealed", &format::encode_ack(sealed))?;
                sealed_from = Some(sealed);
                report.corrupted_tail =
                    Some(segment::quarantine(dir, &paths[index..], offset, from_seq)?);
                segment.bytes = offset;
                if segment.first_seq != next_seq(last_seq)? && offset == 0 {
                    fs::remove_file(&segment.path)?;
                    segment::sync_dir(dir)?;
                } else {
                    segments.push(segment);
                }
                break;
            }
            segments.push(segment);
        }
        for segment in &segments {
            OpenOptions::new()
                .write(true)
                .open(&segment.path)?
                .sync_all()?;
        }
        segment::sync_dir(dir)?;
        let journal = Self {
            dir: dir.to_path_buf(),
            _lock: lock,
            id,
            options: opts,
            segments,
            last_seq,
            acked_seq,
            sealed_from,
            failed: false,
            #[cfg(test)]
            write_hook: tests::WriteHook::default(),
        };
        Ok((journal, report))
    }

    /// Снимок журнала без lock и записи: см. [`ReadOnlyJournal`].
    pub fn open_read_only(dir: &Path) -> Result<ReadOnlyJournal, JournalError> {
        read_only::open(dir)
    }

    pub fn id(&self) -> JournalId {
        self.id
    }

    pub fn is_sealed(&self) -> Option<u64> {
        self.sealed_from
    }

    pub fn append(&mut self, payload: &[u8]) -> Result<u64, JournalError> {
        Ok(*self.append_batch(&[payload])?.start())
    }

    pub fn append_batch(
        &mut self,
        payloads: &[&[u8]],
    ) -> Result<RangeInclusive<u64>, JournalError> {
        if let Some(from_seq) = self.sealed_from {
            return Err(JournalError::Sealed { from_seq });
        }
        self.ensure_writable()?;
        if payloads.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty journal batch").into());
        }
        let mut batch_bytes = 0u64;
        for payload in payloads {
            if payload.len() > self.options.max_record_bytes as usize {
                return Err(JournalError::RecordTooLarge {
                    len: payload.len(),
                    max: self.options.max_record_bytes,
                });
            }
            batch_bytes = batch_bytes
                .checked_add(RECORD_HEADER_BYTES + payload.len() as u64)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "batch size overflow")
                })?;
        }
        let first = next_seq(self.last_seq)?;
        let last = self
            .last_seq
            .checked_add(payloads.len() as u64)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "sequence space exhausted")
            })?;
        let result = self.write_batch(payloads, batch_bytes, first);
        if let Err(error) = result {
            let active = self.segments.last().unwrap();
            if let Ok(file) = OpenOptions::new().write(true).open(&active.path) {
                let _ = file.set_len(active.bytes).and_then(|()| file.sync_all());
            }
            self.failed = true;
            return Err(error.into());
        }
        self.last_seq = last;
        Ok(first..=last)
    }

    fn write_batch(&mut self, payloads: &[&[u8]], batch_bytes: u64, first: u64) -> io::Result<()> {
        let capacity = usize::try_from(batch_bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "batch size overflow"))?;
        let mut buffer = Vec::with_capacity(capacity);
        for (index, payload) in payloads.iter().enumerate() {
            format::write_record(&mut buffer, first + index as u64, payload)?;
        }
        let active = self.segments.last().unwrap();
        if active.bytes > 0
            && active.bytes.saturating_add(batch_bytes) > self.options.segment_max_bytes
        {
            self.segments.push(Segment::create(&self.dir, first)?);
        }
        let active = self.segments.last_mut().unwrap();
        let mut file = OpenOptions::new().append(true).open(&active.path)?;
        #[cfg(test)]
        let mut writer = tests::AppendWriter {
            file: &mut file,
            hook: &mut self.write_hook,
        };
        #[cfg(not(test))]
        let writer = &mut file;
        writer.write_all(&buffer)?;
        file.sync_data()?;
        let mut offset = active.bytes;
        for (index, payload) in payloads.iter().enumerate() {
            active.records.push(RecordPosition {
                seq: first + index as u64,
                offset,
                len: payload.len() as u32,
            });
            offset += RECORD_HEADER_BYTES + payload.len() as u64;
        }
        active.bytes = offset;
        Ok(())
    }

    pub fn read_from(&self, from_seq: u64, max_bytes: usize) -> Result<Vec<Record>, JournalError> {
        segment::read_records(
            &self.segments,
            from_seq,
            max_bytes,
            self.options.max_record_bytes,
        )
    }

    pub fn ack(&mut self, up_to: u64) -> Result<(), JournalError> {
        if up_to > self.last_seq {
            return Err(JournalError::AckBeyondEnd {
                ack: up_to,
                last: self.last_seq,
            });
        }
        if up_to <= self.acked_seq {
            return Ok(());
        }
        self.ensure_writable()?;
        if let Err(error) = segment::atomic_replace(&self.dir, "ack", &format::encode_ack(up_to)) {
            self.failed = true;
            return Err(error.into());
        }
        self.acked_seq = up_to;
        Ok(())
    }

    pub fn compact(&mut self) -> Result<u64, JournalError> {
        self.ensure_writable()?;
        let result = self.compact_segments();
        if result.is_err() {
            self.failed = true;
        }
        result.map_err(JournalError::Io)
    }

    /// Удаляет начальные сегменты, чьи записи подтверждены и лежат строго ниже
    /// горизонта: записи открытого запроса (и его передачи решения) остаются
    /// для восстановления после рестарта.
    pub fn compact_up_to(&mut self, horizon: u64) -> Result<u64, JournalError> {
        self.ensure_writable()?;
        let result = self.compact_segments_up_to(horizon);
        if result.is_err() {
            self.failed = true;
        }
        result.map_err(JournalError::Io)
    }

    fn compact_segments(&mut self) -> io::Result<u64> {
        self.compact_segments_up_to(u64::MAX)
    }

    fn compact_segments_up_to(&mut self, horizon: u64) -> io::Result<u64> {
        let removable = self
            .segments
            .iter()
            .take_while(|segment| {
                segment.last_seq() <= self.acked_seq && segment.last_seq() < horizon
            })
            .count();
        if removable == self.segments.len() {
            let active = self.segments.last().unwrap();
            if active.records.is_empty() {
                if removable == 1 {
                    return Ok(0);
                }
            } else {
                self.segments
                    .push(Segment::create(&self.dir, next_seq(self.last_seq)?)?);
            }
        }
        let mut freed = 0;
        while self.segments.len() > 1
            && self.segments[0].last_seq() <= self.acked_seq
            && self.segments[0].last_seq() < horizon
        {
            fs::remove_file(&self.segments[0].path)?;
            segment::sync_dir(&self.dir)?;
            freed += self.segments.remove(0).bytes;
        }
        Ok(freed)
    }

    pub fn first_seq(&self) -> u64 {
        self.segments[0].first_seq
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn acked_seq(&self) -> u64 {
        self.acked_seq
    }

    pub fn bytes_on_disk(&self) -> u64 {
        format::HEADER_BYTES
            + format::ACK_BYTES
            + if self.sealed_from.is_some() {
                format::ACK_BYTES
            } else {
                0
            }
            + self
                .segments
                .iter()
                .map(|segment| segment.bytes)
                .sum::<u64>()
    }

    fn ensure_writable(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "journal must be reopened after an I/O failure",
            ));
        }
        if self.acked_seq > self.last_seq && self.sealed_from.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acknowledgement exceeds recovered journal end",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct JournalLock(File);

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_directory(dir: &Path) -> Result<JournalLock, JournalError> {
    let lock = segment::private_file()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("lock"))?;
    lock.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => JournalError::Locked,
        TryLockError::Error(error) => JournalError::Io(error),
    })?;
    Ok(JournalLock(lock))
}

fn next_seq(last: u64) -> io::Result<u64> {
    last.checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sequence space exhausted"))
}

#[cfg(test)]
mod tests;
