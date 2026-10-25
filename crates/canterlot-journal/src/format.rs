use std::io::{self, Read, Write};

use crate::{JournalError, JournalId, Record};

pub(crate) const RECORD_HEADER_BYTES: u64 = 16;
pub(crate) const HEADER_BYTES: u64 = 24;
pub(crate) const ACK_BYTES: u64 = 12;

pub(crate) fn encode_header(id: JournalId) -> [u8; HEADER_BYTES as usize] {
    let mut bytes = [0; HEADER_BYTES as usize];
    bytes[..4].copy_from_slice(b"CNTJ");
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[8..].copy_from_slice(id.0.as_bytes());
    bytes
}

pub(crate) fn decode_header(bytes: &[u8]) -> Result<JournalId, JournalError> {
    if bytes.len() != HEADER_BYTES as usize || &bytes[..4] != b"CNTJ" {
        return Err(JournalError::NotAJournal);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != 1 {
        return Err(JournalError::UnsupportedVersion(version));
    }
    Ok(JournalId(uuid::Uuid::from_bytes(
        bytes[8..].try_into().unwrap(),
    )))
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

pub(crate) fn read_record(
    reader: &mut impl Read,
    remaining: u64,
    max_record_bytes: u32,
) -> io::Result<Option<Record>> {
    if remaining < RECORD_HEADER_BYTES {
        return Ok(None);
    }
    let mut header = [0; RECORD_HEADER_BYTES as usize];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header[..4].try_into().unwrap());
    if len > max_record_bytes || u64::from(len) > remaining - RECORD_HEADER_BYTES {
        return Ok(None);
    }
    let crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let seq = u64::from_le_bytes(header[8..].try_into().unwrap());
    let mut payload = vec![0; len as usize];
    reader.read_exact(&mut payload)?;
    if checksum(seq, &payload) != crc {
        return Ok(None);
    }
    Ok(Some(Record { seq, payload }))
}

fn checksum(seq: u64, payload: &[u8]) -> u32 {
    crc32c::crc32c_append(crc32c::crc32c(&seq.to_le_bytes()), payload)
}
