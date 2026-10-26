use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::format::{self, RECORD_HEADER_BYTES};
use crate::{CorruptedTail, JournalError};

#[derive(Debug)]
pub(crate) struct RecordPosition {
    pub seq: u64,
    pub offset: u64,
    pub len: u32,
}

#[derive(Debug)]
pub(crate) struct Segment {
    pub first_seq: u64,
    pub path: PathBuf,
    pub bytes: u64,
    pub records: Vec<RecordPosition>,
}

impl Segment {
    pub fn create(dir: &Path, first_seq: u64) -> io::Result<Self> {
        let path = dir.join(format!("seg-{first_seq:020}.log"));
        let file = private_file().write(true).create_new(true).open(&path)?;
        file.sync_all()?;
        sync_dir(dir)?;
        Ok(Self {
            first_seq,
            path,
            bytes: 0,
            records: Vec::new(),
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.records
            .last()
            .map_or(self.first_seq - 1, |record| record.seq)
    }

    pub fn scan(
        path: PathBuf,
        first_seq: u64,
        last_valid_seq: &mut u64,
        max_record_bytes: u32,
    ) -> io::Result<(Self, Option<u64>)> {
        let mut file = File::open(&path)?;
        let bytes = file.metadata()?.len();
        let mut segment = Self {
            first_seq,
            path,
            bytes,
            records: Vec::new(),
        };
        if last_valid_seq.checked_add(1) != Some(first_seq) {
            return Ok((segment, Some(0)));
        }
        let mut offset = 0;
        while offset < bytes {
            let Some(record) = format::read_record(&mut file, bytes - offset, max_record_bytes)?
            else {
                return Ok((segment, Some(offset)));
            };
            if last_valid_seq.checked_add(1) != Some(record.seq) {
                return Ok((segment, Some(offset)));
            }
            let len = record.payload.len() as u32;
            *last_valid_seq = record.seq;
            segment.records.push(RecordPosition {
                seq: record.seq,
                offset,
                len,
            });
            offset += RECORD_HEADER_BYTES + u64::from(len);
        }
        Ok((segment, None))
    }
}

pub(crate) fn list(dir: &Path) -> Result<Vec<(u64, PathBuf)>, JournalError> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("seg-") || !name.ends_with(".log") {
            continue;
        }
        let digits = &name[4..name.len() - 4];
        if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(JournalError::NotAJournal);
        }
        let seq = digits
            .parse::<u64>()
            .map_err(|_| JournalError::NotAJournal)?;
        if seq == 0 || !entry.file_type()?.is_file() {
            return Err(JournalError::NotAJournal);
        }
        segments.push((seq, entry.path()));
    }
    segments.sort_unstable_by_key(|(seq, _)| *seq);
    if segments.is_empty() {
        return Err(JournalError::NotAJournal);
    }
    Ok(segments)
}

pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

pub(crate) fn atomic_replace(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let temporary = dir.join(format!("{name}.tmp"));
    let mut file = private_file()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, dir.join(name))?;
    sync_dir(dir)
}

pub(crate) fn quarantine(
    dir: &Path,
    affected: &[(u64, PathBuf)],
    offset: u64,
    from_seq: u64,
) -> io::Result<CorruptedTail> {
    let base = dir.join("quarantine");
    if !base.exists() {
        private_dir(&base)?;
    }
    sync_dir(dir)?;
    let destination_name = |path: &Path, offset| {
        format!(
            "{}-{offset}.bin",
            path.file_name().unwrap().to_str().unwrap()
        )
    };
    let collision = affected.iter().enumerate().any(|(index, (_, path))| {
        let name = destination_name(path, if index == 0 { offset } else { 0 });
        base.join(&name).exists() || base.join(format!("{name}.tmp")).exists()
    });
    let destination = if collision {
        let path = base.join(uuid::Uuid::now_v7().to_string());
        private_dir(&path)?;
        sync_dir(&base)?;
        path
    } else {
        base
    };
    let quarantine_path = destination.join(destination_name(&affected[0].1, offset));
    let mut bytes = 0;
    for (index, (_, path)) in affected.iter().enumerate() {
        let cut = if index == 0 { offset } else { 0 };
        let mut source = File::open(path)?;
        source.seek(SeekFrom::Start(cut))?;
        let name = destination_name(path, cut);
        let temporary = destination.join(format!("{name}.tmp"));
        let mut target = private_file()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        bytes += io::copy(&mut source, &mut target)?;
        target.sync_all()?;
        fs::rename(temporary, destination.join(name))?;
        sync_dir(&destination)?;
    }
    sync_dir(dir)?;
    // Карантин устойчив до удаления исходного хвоста.
    for (_, path) in affected.iter().skip(1).rev() {
        fs::remove_file(path)?;
        sync_dir(dir)?;
    }
    let file = private_file().write(true).open(&affected[0].1)?;
    file.set_len(offset)?;
    file.sync_all()?;
    Ok(CorruptedTail {
        from_seq,
        quarantine_path,
        bytes,
    })
}

pub(crate) fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    options.mode(0o600);
    options
}

pub(crate) fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path)
}
