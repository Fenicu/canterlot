use std::io;
use std::path::Path;

use crate::segment::{self, Segment};
use crate::{JournalError, JournalId, Record, format, next_seq};

const SNAPSHOT_ATTEMPTS: usize = 16;

/// Снимок журнала только для чтения. Не берёт lock, ничего не пишет, не запечатывает
/// и не обрезает, поэтому безопасен рядом с живым писателем и для журнала умершего
/// владельца. Снимок кончается на последней полной записи с верной контрольной
/// суммой; недописанный или повреждённый хвост последнего сегмента считается ещё не
/// записанным. Порча до хвоста (в непоследнем сегменте или разрыв между сегментами)
/// возвращает `JournalError::Corrupted`. Свежие данные дают только новый
/// `open_read_only`; после открытия писатель может удалить сегмент компакцией, тогда
/// чтение вернёт `Io(NotFound)`.
#[derive(Debug)]
pub struct ReadOnlyJournal {
    id: JournalId,
    max_record_bytes: u32,
    segments: Vec<Segment>,
    last_seq: u64,
    acked_seq: u64,
}

impl ReadOnlyJournal {
    pub fn id(&self) -> JournalId {
        self.id
    }

    pub fn first_seq(&self) -> u64 {
        self.segments[0].first_seq
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// ACK читается после списка сегментов, поэтому `first_seq() <= acked_seq() + 1`;
    /// писатель мог успеть записать и подтвердить запись позже снимка сегментов.
    pub fn acked_seq(&self) -> u64 {
        self.acked_seq
    }

    pub fn read_from(&self, from_seq: u64, max_bytes: usize) -> Result<Vec<Record>, JournalError> {
        segment::read_records(&self.segments, from_seq, max_bytes, self.max_record_bytes)
    }
}

pub(crate) fn open(dir: &Path) -> Result<ReadOnlyJournal, JournalError> {
    let (id, max_record_bytes) = format::decode_header(&segment::read_metadata(dir, "header")?)?;
    let mut attempt = 1;
    loop {
        match snapshot(dir, id, max_record_bytes) {
            // Писатель успел удалить сегмент компакцией или откатить хвост.
            Err(JournalError::Io(error))
                if attempt < SNAPSHOT_ATTEMPTS
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
                    ) =>
            {
                attempt += 1;
            }
            result => return result,
        }
    }
}

fn snapshot(
    dir: &Path,
    id: JournalId,
    max_record_bytes: u32,
) -> Result<ReadOnlyJournal, JournalError> {
    let paths = segment::list(dir)?;
    let last_index = paths.len() - 1;
    let mut last_seq = paths[0].0 - 1;
    let mut segments = Vec::with_capacity(paths.len());
    for (index, (first_seq, path)) in paths.into_iter().enumerate() {
        if last_seq.checked_add(1) != Some(first_seq) {
            return Err(JournalError::Corrupted {
                from_seq: next_seq(last_seq)?,
            });
        }
        let (segment, stopped_at) =
            Segment::scan(path, first_seq, &mut last_seq, max_record_bytes)?;
        if stopped_at.is_some() && index != last_index {
            return Err(JournalError::Corrupted {
                from_seq: next_seq(last_seq)?,
            });
        }
        segments.push(segment);
    }
    let acked_seq = format::decode_ack(&segment::read_metadata(dir, "ack")?)?;
    Ok(ReadOnlyJournal {
        id,
        max_record_bytes,
        segments,
        last_seq,
        acked_seq,
    })
}
