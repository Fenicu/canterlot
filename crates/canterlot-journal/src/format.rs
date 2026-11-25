use std::io::{self, Read, Write};

use crate::{JournalError, JournalId, Record};

pub(crate) const RECORD_HEADER_BYTES: u64 = 16;
pub(crate) const HEADER_BYTES: u64 = 28;
pub(crate) const ACK_BYTES: u64 = 12;

pub(crate) fn encode_header(id: JournalId, max_record_bytes: u32) -> [u8; HEADER_BYTES as usize] {
    let mut bytes = [0; HEADER_BYTES as usize];
    bytes[..4].copy_from_slice(b"CNTJ");
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[8..24].copy_from_slice(id.0.as_bytes());
    bytes[24..].copy_from_slice(&max_record_bytes.to_le_bytes());
    bytes
}

pub(crate) fn decode_header(bytes: &[u8]) -> Result<(JournalId, u32), JournalError> {
    if bytes.len() != HEADER_BYTES as usize || &bytes[..4] != b"CNTJ" {
        return Err(JournalError::NotAJournal);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 1 {
        return Err(JournalError::UnsupportedVersion(version));
    }
    Ok((
        JournalId(uuid::Uuid::from_bytes(bytes[8..24].try_into().unwrap())),
        u32::from_le_bytes(bytes[24..].try_into().unwrap()),
    ))
}

pub(crate) fn encode_ack(seq: u64) -> [u8; ACK_BYTES as usize] {
    let mut bytes = [0; ACK_BYTES as usize];
    bytes[..8].copy_from_slice(&seq.to_le_bytes());
    let crc = crc32c::crc32c(&bytes[..8]);
    bytes[8..].copy_from_slice(&crc.to_le_bytes());
    bytes
}

pub(crate) fn decode_ack(bytes: &[u8]) -> Result<u64, JournalError> {
    if bytes.len() != ACK_BYTES as usize {
        return Err(JournalError::NotAJournal);
    }
    let crc = u32::from_le_bytes(bytes[8..].try_into().unwrap());
    if crc != crc32c::crc32c(&bytes[..8]) {
        return Err(JournalError::NotAJournal);
    }
    Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
}

pub(crate) fn write_record(writer: &mut impl Write, seq: u64, payload: &[u8]) -> io::Result<()> {
    let mut header = [0; RECORD_HEADER_BYTES as usize];
    header[..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    header[4..8].copy_from_slice(&checksum(seq, payload).to_le_bytes());
    header[8..].copy_from_slice(&seq.to_le_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)
}

/// Чем кончилось чтение записи с текущей позиции.
pub(crate) enum Scanned {
    Record(Record),
    /// Запись оборвана концом данных: заголовок неполон либо payload короче
    /// объявленной допустимой длины. Так выглядит незавершённый append, но так же
    /// выглядит и испорченная длина: различает их [`cut_is_torn`].
    Cut,
    /// Запись цела по длине, но негодна: длина сверх предела или неверная CRC32C.
    Invalid,
}

pub(crate) fn scan_record(
    reader: &mut impl Read,
    remaining: u64,
    max_record_bytes: u32,
) -> io::Result<Scanned> {
    if remaining < RECORD_HEADER_BYTES {
        return Ok(Scanned::Cut);
    }
    let mut header = [0; RECORD_HEADER_BYTES as usize];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header[..4].try_into().unwrap());
    if len > max_record_bytes {
        return Ok(Scanned::Invalid);
    }
    if u64::from(len) > remaining - RECORD_HEADER_BYTES {
        return Ok(Scanned::Cut);
    }
    let crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let seq = u64::from_le_bytes(header[8..].try_into().unwrap());
    let mut payload = vec![0; len as usize];
    reader.read_exact(&mut payload)?;
    if checksum(seq, &payload) != crc {
        return Ok(Scanned::Invalid);
    }
    Ok(Scanned::Record(Record { seq, payload }))
}

/// Доказуемо ли оборванный хвост — недописанный append, а не порча длины.
/// `tail` — байты от начала оборванной записи до конца файла, `expected_seq` —
/// номер, который append выдал бы этой записи. Незавершённый append оставляет
/// префикс своего пакета: заголовок с ожидаемым номером и неполный payload, за
/// которым ничего нет. Если CRC сходится по всем байтам до конца файла, запись
/// дописана и испорчена только длина; если дальше лежит целая запись с номером
/// не меньше ожидаемого, append вернулся и его номера уже выданы. Обе картины и
/// чужой номер в заголовке — порча, хвост нужно запечатать.
pub(crate) fn cut_is_torn(tail: &[u8], expected_seq: u64, max_record_bytes: u32) -> bool {
    let header_bytes = RECORD_HEADER_BYTES as usize;
    if tail.len() < header_bytes {
        return true;
    }
    let crc = u32::from_le_bytes(tail[4..8].try_into().unwrap());
    let seq = u64::from_le_bytes(tail[8..16].try_into().unwrap());
    if seq != expected_seq || checksum(seq, &tail[header_bytes..]) == crc {
        return false;
    }
    // Записей после оборванной не больше, чем заголовков помещается в хвост.
    let max_seq = expected_seq.saturating_add((tail.len() / header_bytes) as u64);
    !(header_bytes..=tail.len() - header_bytes).any(|at| {
        let header = &tail[at..at + header_bytes];
        let seq = u64::from_le_bytes(header[8..].try_into().unwrap());
        let len = u32::from_le_bytes(header[..4].try_into().unwrap());
        if !(expected_seq..=max_seq).contains(&seq)
            || len > max_record_bytes
            || len as usize > tail.len() - at - header_bytes
        {
            return false;
        }
        let crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let start = at + header_bytes;
        checksum(seq, &tail[start..start + len as usize]) == crc
    })
}

pub(crate) fn read_record(
    reader: &mut impl Read,
    remaining: u64,
    max_record_bytes: u32,
) -> io::Result<Option<Record>> {
    Ok(match scan_record(reader, remaining, max_record_bytes)? {
        Scanned::Record(record) => Some(record),
        Scanned::Cut | Scanned::Invalid => None,
    })
}

fn checksum(seq: u64, payload: &[u8]) -> u32 {
    crc32c::crc32c_append(crc32c::crc32c(&seq.to_le_bytes()), payload)
}
