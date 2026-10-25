#![forbid(unsafe_code)]

mod error;
mod format;
mod segment;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub use error::JournalError;
use format::RECORD_HEADER_BYTES;
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
    id: JournalId,
    options: JournalOptions,
    segments: Vec<Segment>,
    last_seq: u64,
    acked_seq: u64,
    failed: bool,
}

impl Journal {
    pub fn create(
        dir: &Path,
        id: JournalId,
        opts: JournalOptions,
    ) -> Result<Journal, JournalError> {
        match fs::create_dir(dir) {
            Ok(()) => segment::sync_dir(
                dir.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if fs::read_dir(dir)?.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "journal directory is not empty",
            )
            .into());
        }
        let first = Segment::create(dir, 1)?;
        segment::atomic_replace(dir, "ack", &format::encode_ack(0))?;
        segment::atomic_replace(dir, "header", &format::encode_header(id))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            id,
            options: opts,
            segments: vec![first],
            last_seq: 0,
            acked_seq: 0,
            failed: false,
        })
    }

    pub fn open(dir: &Path, opts: JournalOptions) -> Result<(Journal, OpenReport), JournalError> {
        let metadata = |name| match fs::read(dir.join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(JournalError::NotAJournal),
            result => result.map_err(JournalError::Io),
        };
        let id = format::decode_header(&metadata("header")?)?;
        let acked_seq = format::decode_ack(&metadata("ack")?)?;
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
                report.corrupted_tail = Some(segment::quarantine(
                    dir,
                    &paths[index..],
                    offset,
                    next_seq(last_seq)?,
                )?);
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
        let journal = Self {
            dir: dir.to_path_buf(),
            id,
            options: opts,
            segments,
            last_seq,
            acked_seq,
            failed: false,
        };
        Ok((journal, report))
    }

    pub fn id(&self) -> JournalId {
        self.id
    }

    pub fn append(&mut self, payload: &[u8]) -> Result<u64, JournalError> {
        Ok(*self.append_batch(&[payload])?.start())
    }

    pub fn append_batch(
        &mut self,
        payloads: &[&[u8]],
    ) -> Result<RangeInclusive<u64>, JournalError> {
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
            self.failed = true;
            return Err(error.into());
        }
        self.last_seq = last;
        Ok(first..=last)
    }

    fn write_batch(&mut self, payloads: &[&[u8]], batch_bytes: u64, first: u64) -> io::Result<()> {
        let active = self.segments.last().unwrap();
        if active.bytes > 0
            && active.bytes.saturating_add(batch_bytes) > self.options.segment_max_bytes
        {
            self.segments.push(Segment::create(&self.dir, first)?);
        }
        let active = self.segments.last_mut().unwrap();
        let mut file = OpenOptions::new().append(true).open(&active.path)?;
        for (index, payload) in payloads.iter().enumerate() {
            format::write_record(&mut file, first + index as u64, payload)?;
        }
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
        let first = self.first_seq();
        if from_seq < first {
            return Err(JournalError::SeqCompacted {
                requested: from_seq,
                first,
            });
        }
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for segment in &self.segments {
            if segment.last_seq() < from_seq {
                continue;
            }
            let mut file = File::open(&segment.path)?;
            for position in segment
                .records
                .iter()
                .filter(|record| record.seq >= from_seq)
            {
                if !records.is_empty() && position.len as usize > max_bytes.saturating_sub(bytes) {
                    return Ok(records);
                }
                file.seek(SeekFrom::Start(position.offset))?;
                let record = format::read_record(
                    &mut file,
                    RECORD_HEADER_BYTES + u64::from(position.len),
                    self.options.max_record_bytes,
                )?
                .filter(|record| record.seq == position.seq)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "journal record changed after open",
                    )
                })?;
                bytes = bytes.saturating_add(record.payload.len());
                records.push(record);
            }
        }
        Ok(records)
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

    fn compact_segments(&mut self) -> io::Result<u64> {
        let removable = self
            .segments
            .iter()
            .take_while(|segment| segment.last_seq() <= self.acked_seq)
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
        while self.segments.len() > 1 && self.segments[0].last_seq() <= self.acked_seq {
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
        if self.acked_seq > self.last_seq {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acknowledgement exceeds recovered journal end",
            ));
        }
        Ok(())
    }
}

fn next_seq(last: u64) -> io::Result<u64> {
    last.checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "sequence space exhausted"))
}
