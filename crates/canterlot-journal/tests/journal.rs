use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use canterlot_journal::{Journal, JournalError, JournalId, JournalOptions, Record};
use tempfile::TempDir;
use uuid::Uuid;

const RECORD_HEADER_BYTES: usize = 16;

fn create(dir: &Path) -> Journal {
    Journal::create(dir, JournalId(Uuid::now_v7()), JournalOptions::default()).unwrap()
}

fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    paths.sort();
    paths
}

fn copy_journal(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
    }
}

fn assert_records(records: &[Record], expected: &[(u64, &[u8])]) {
    assert_eq!(records.len(), expected.len());
    for (record, (seq, payload)) in records.iter().zip(expected) {
        assert_eq!(record.seq, *seq);
        assert_eq!(record.payload, *payload);
    }
}

#[test]
fn append_and_read_roundtrip() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    assert_eq!(journal.first_seq(), 1);
    assert_eq!(journal.last_seq(), 0);
    assert_eq!(journal.acked_seq(), 0);
    assert_eq!(journal.append(b"first").unwrap(), 1);
    assert_eq!(journal.append(b"second\0").unwrap(), 2);
    assert_eq!(journal.append("третий".as_bytes()).unwrap(), 3);
    assert_records(
        &journal.read_from(2, usize::MAX).unwrap(),
        &[(2, b"second\0"), (3, "третий".as_bytes())],
    );
}

#[test]
fn reopen_preserves_records_and_ack() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    let id = journal.id();
    for _ in 0..5 {
        journal.append(b"record").unwrap();
    }
    journal.ack(3).unwrap();
    journal.ack(2).unwrap();
    drop(journal);
    let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.id().0, id.0);
    assert_eq!(journal.last_seq(), 5);
    assert_eq!(journal.acked_seq(), 3);
    assert!(report.corrupted_tail.is_none());
    assert_records(
        &journal.read_from(1, usize::MAX).unwrap(),
        &[
            (1, b"record"),
            (2, b"record"),
            (3, b"record"),
            (4, b"record"),
            (5, b"record"),
        ],
    );
}

#[test]
fn torn_tail_at_every_offset_keeps_valid_prefix() {
    let original = TempDir::new().unwrap();
    let mut journal = create(original.path());
    journal.append(b"one").unwrap();
    journal.append(b"two-two").unwrap();
    let end_two = fs::metadata(&segments(original.path())[0]).unwrap().len();
    journal.append(b"third payload").unwrap();
    let segment = segments(original.path()).remove(0);
    let original_bytes = fs::read(&segment).unwrap();
    let end_three = original_bytes.len() as u64;
    drop(journal);

    for length in end_two..end_three {
        let copy = TempDir::new().unwrap();
        copy_journal(original.path(), copy.path());
        let path = copy.path().join(segment.file_name().unwrap());
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(length)
            .unwrap();
        let (mut journal, report) = Journal::open(copy.path(), JournalOptions::default()).unwrap();
        assert_eq!(journal.last_seq(), 2, "length {length}");
        assert_records(
            &journal.read_from(1, usize::MAX).unwrap(),
            &[(1, b"one"), (2, b"two-two")],
        );
        if length == end_two {
            assert!(report.corrupted_tail.is_none());
        } else {
            let tail = report.corrupted_tail.unwrap();
            assert_eq!(tail.from_seq, 3, "length {length}");
            assert_eq!(tail.bytes, length - end_two);
            assert!(tail.quarantine_path.is_file());
            assert_eq!(
                tail.quarantine_path.file_name().unwrap().to_str().unwrap(),
                format!(
                    "{}-{end_two}.bin",
                    segment.file_name().unwrap().to_str().unwrap()
                )
            );
            assert_eq!(
                fs::read(tail.quarantine_path).unwrap(),
                original_bytes[end_two as usize..length as usize]
            );
        }
        assert_eq!(fs::metadata(&path).unwrap().len(), end_two);
        assert_eq!(journal.append(b"replacement").unwrap(), 3);
        drop(journal);
        let (journal, report) = Journal::open(copy.path(), JournalOptions::default()).unwrap();
        assert_eq!(journal.last_seq(), 3);
        assert!(report.corrupted_tail.is_none());
    }
}

#[test]
fn bit_flip_in_payload_is_quarantined() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    let end_one = fs::metadata(&segments(dir.path())[0]).unwrap().len();
    journal.append(b"two").unwrap();
    journal.append(b"three").unwrap();
    drop(journal);
    let segment = segments(dir.path()).remove(0);
    let mut bytes = fs::read(&segment).unwrap();
    bytes[end_one as usize + RECORD_HEADER_BYTES] ^= 1;
    fs::write(&segment, &bytes).unwrap();
    let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    assert_records(&journal.read_from(1, usize::MAX).unwrap(), &[(1, b"one")]);
    let tail = report.corrupted_tail.unwrap();
    assert_eq!(tail.from_seq, 2);
    assert_eq!(tail.bytes, bytes.len() as u64 - end_one);
    assert_eq!(
        fs::read(tail.quarantine_path).unwrap(),
        bytes[end_one as usize..]
    );
}

#[test]
fn ack_beyond_end_is_rejected() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    let ack = journal.last_seq() + 1;
    assert!(matches!(
        journal.ack(ack),
        Err(JournalError::AckBeyondEnd { ack: 2, last: 1 })
    ));
    assert_eq!(journal.acked_seq(), 0);
}

#[test]
fn compaction_removes_only_acked_segments() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        segment_max_bytes: 60,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    for _ in 0..10 {
        journal.append(b"data").unwrap();
    }
    let before = segments(dir.path());
    assert!(before.len() >= 3);
    let ack = 5;
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for path in before {
        let first: u64 = path.file_name().unwrap().to_str().unwrap()[4..24]
            .parse()
            .unwrap();
        let last = first + fs::metadata(&path).unwrap().len() / 20 - 1;
        if last <= ack {
            removed.push(path);
        } else {
            kept.push((first, last, path));
        }
    }
    assert!(kept[0].0 < ack && ack < kept[0].1);
    let expected_freed: u64 = removed
        .iter()
        .map(|path| fs::metadata(path).unwrap().len())
        .sum();
    let bytes_before = journal.bytes_on_disk();
    journal.ack(ack).unwrap();
    assert_eq!(journal.compact().unwrap(), expected_freed);
    assert_eq!(journal.bytes_on_disk(), bytes_before - expected_freed);
    assert!(removed.iter().all(|path| !path.exists()));
    assert!(kept.iter().all(|(_, _, path)| path.exists()));
    let expected_first = kept[0].0;
    assert_eq!(journal.first_seq(), expected_first);
    assert!(
        matches!(journal.read_from(1, usize::MAX), Err(JournalError::SeqCompacted { requested: 1, first }) if first == expected_first)
    );
    assert_eq!(journal.compact().unwrap(), 0);
    drop(journal);
    let (mut journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.first_seq(), expected_first);
    assert_eq!(journal.last_seq(), 10);
    assert_eq!(journal.acked_seq(), ack);
    assert!(report.corrupted_tail.is_none());
    assert_eq!(journal.append(b"after compact").unwrap(), 11);
}

#[test]
fn record_too_large_is_rejected() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        max_record_bytes: 8,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    assert!(matches!(
        journal.append(&[0; 9]),
        Err(JournalError::RecordTooLarge { len: 9, max: 8 })
    ));
    assert_eq!(journal.last_seq(), 0);
    assert_eq!(journal.append(b"fits").unwrap(), 1);
}

#[test]
fn append_batch_assigns_contiguous_range() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    journal.append(b"two").unwrap();
    assert_eq!(
        journal
            .append_batch(&[b"three".as_slice(), b"four", b"five", b"six"])
            .unwrap(),
        3..=6
    );
    assert_records(
        &journal.read_from(3, usize::MAX).unwrap(),
        &[(3, b"three"), (4, b"four"), (5, b"five"), (6, b"six")],
    );
    drop(journal);
    let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 6);
    assert!(report.corrupted_tail.is_none());
}

#[test]
fn format_matches_little_endian_layout_and_crc32c() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    let mut expected_header = b"CNTJ".to_vec();
    expected_header.extend_from_slice(&1u32.to_le_bytes());
    expected_header.extend_from_slice(journal.id().0.as_bytes());
    assert_eq!(
        fs::read(dir.path().join("header")).unwrap(),
        expected_header
    );
    journal.append(b"payload").unwrap();
    journal.ack(1).unwrap();
    let mut protected = 1u64.to_le_bytes().to_vec();
    protected.extend_from_slice(b"payload");
    let mut expected_record = 7u32.to_le_bytes().to_vec();
    expected_record.extend_from_slice(&crc32c::crc32c(&protected).to_le_bytes());
    expected_record.extend_from_slice(&protected);
    assert_eq!(fs::read(&segments(dir.path())[0]).unwrap(), expected_record);
    let mut expected_ack = 1u64.to_le_bytes().to_vec();
    expected_ack.extend_from_slice(&crc32c::crc32c(&expected_ack).to_le_bytes());
    assert_eq!(fs::read(dir.path().join("ack")).unwrap(), expected_ack);
    let file_bytes: u64 = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert_eq!(journal.bytes_on_disk(), file_bytes);
}

#[test]
fn corruption_quarantines_all_later_segments() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        segment_max_bytes: 40,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    for _ in 0..6 {
        journal.append(b"data").unwrap();
    }
    drop(journal);
    let paths = segments(dir.path());
    assert_eq!(paths.len(), 3);
    let mut first = fs::read(&paths[0]).unwrap();
    first[20 + RECORD_HEADER_BYTES] ^= 1;
    fs::write(&paths[0], &first).unwrap();
    let tails = [
        first[20..].to_vec(),
        fs::read(&paths[1]).unwrap(),
        fs::read(&paths[2]).unwrap(),
    ];
    let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    let tail = report.corrupted_tail.unwrap();
    assert_eq!(tail.from_seq, 2);
    assert_eq!(
        tail.bytes,
        tails.iter().map(|bytes| bytes.len() as u64).sum::<u64>()
    );
    assert_eq!(segments(dir.path()).len(), 1);
    for (index, (path, expected)) in paths.iter().zip(tails).enumerate() {
        let offset = if index == 0 { 20 } else { 0 };
        let name = format!(
            "{}-{offset}.bin",
            path.file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(
            fs::read(dir.path().join("quarantine").join(name)).unwrap(),
            expected
        );
    }
    drop(journal);
    let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    assert!(report.corrupted_tail.is_none());
}

#[test]
fn bad_length_and_sequence_end_the_valid_prefix() {
    for field in [0, 8] {
        let dir = TempDir::new().unwrap();
        let mut journal = create(dir.path());
        journal.append(b"one").unwrap();
        journal.append(b"two").unwrap();
        drop(journal);
        let path = segments(dir.path()).remove(0);
        let mut bytes = fs::read(&path).unwrap();
        if field == 0 {
            bytes[19..23].copy_from_slice(&u32::MAX.to_le_bytes());
        } else {
            bytes[27..35].copy_from_slice(&9u64.to_le_bytes());
            let checksum = crc32c::crc32c(&bytes[27..]);
            bytes[23..27].copy_from_slice(&checksum.to_le_bytes());
        }
        fs::write(path, bytes).unwrap();
        let (journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
        assert_eq!(journal.last_seq(), 1);
        assert_eq!(report.corrupted_tail.unwrap().from_seq, 2);
    }
}

#[test]
fn read_budget_returns_at_least_one_record() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"12345").unwrap();
    journal.append(b"678").unwrap();
    assert_records(&journal.read_from(1, 0).unwrap(), &[(1, b"12345")]);
    assert_records(&journal.read_from(1, 7).unwrap(), &[(1, b"12345")]);
    assert_eq!(journal.read_from(1, 8).unwrap().len(), 2);
    assert!(journal.read_from(3, 0).unwrap().is_empty());
}

#[test]
fn full_compaction_preserves_sequence_after_reopen() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    journal.append(b"two").unwrap();
    journal.ack(2).unwrap();
    assert_eq!(journal.compact().unwrap(), 38);
    assert_eq!(journal.first_seq(), 3);
    assert_eq!(journal.last_seq(), 2);
    assert!(journal.read_from(3, usize::MAX).unwrap().is_empty());
    drop(journal);
    let (mut journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.first_seq(), 3);
    assert_eq!(journal.last_seq(), 2);
    assert_eq!(journal.acked_seq(), 2);
    assert!(report.corrupted_tail.is_none());
    assert_eq!(journal.append(b"three").unwrap(), 3);
}

#[test]
fn invalid_batch_is_rejected_before_any_write() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        max_record_bytes: 4,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    let before = journal.bytes_on_disk();
    assert!(matches!(
        journal.append_batch(&[b"okay".as_slice(), b"too large"]),
        Err(JournalError::RecordTooLarge { .. })
    ));
    assert_eq!(journal.last_seq(), 0);
    assert_eq!(journal.bytes_on_disk(), before);
    assert!(
        matches!(journal.append_batch(&[]), Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput)
    );
    assert_eq!(journal.append(b"").unwrap(), 1);
}

#[test]
fn metadata_corruption_is_rejected_without_repair() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        Journal::open(dir.path(), JournalOptions::default()),
        Err(JournalError::NotAJournal)
    ));
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    journal.ack(1).unwrap();
    drop(journal);
    let header_path = dir.path().join("header");
    let header = fs::read(&header_path).unwrap();
    let mut invalid_version = header.clone();
    invalid_version[4..8].copy_from_slice(&2u32.to_le_bytes());
    fs::write(&header_path, invalid_version).unwrap();
    assert!(matches!(
        Journal::open(dir.path(), JournalOptions::default()),
        Err(JournalError::UnsupportedVersion(2))
    ));
    fs::write(&header_path, b"invalid").unwrap();
    assert!(matches!(
        Journal::open(dir.path(), JournalOptions::default()),
        Err(JournalError::NotAJournal)
    ));
    fs::write(&header_path, header).unwrap();
    let mut ack = OpenOptions::new()
        .write(true)
        .open(dir.path().join("ack"))
        .unwrap();
    ack.seek(SeekFrom::Start(0)).unwrap();
    ack.write_all(&[2]).unwrap();
    assert!(matches!(
        Journal::open(dir.path(), JournalOptions::default()),
        Err(JournalError::NotAJournal)
    ));
}

#[test]
fn missing_middle_segment_recovers_an_appendable_prefix() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        segment_max_bytes: 40,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    for _ in 0..6 {
        journal.append(b"data").unwrap();
    }
    drop(journal);
    let paths = segments(dir.path());
    fs::remove_file(&paths[1]).unwrap();
    let (mut journal, report) = Journal::open(dir.path(), options).unwrap();
    assert_eq!(journal.last_seq(), 2);
    assert_eq!(report.corrupted_tail.unwrap().from_seq, 3);
    assert_eq!(journal.append(b"replacement").unwrap(), 3);
    drop(journal);
    let (journal, report) = Journal::open(dir.path(), options).unwrap();
    assert_eq!(journal.last_seq(), 3);
    assert_records(
        &journal.read_from(3, usize::MAX).unwrap(),
        &[(3, b"replacement")],
    );
    assert!(report.corrupted_tail.is_none());
}

#[test]
fn quarantine_preserves_previous_and_interrupted_recoveries() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    journal.append(b"two").unwrap();
    drop(journal);
    let segment = segments(dir.path()).remove(0);
    let mut bytes = fs::read(&segment).unwrap();
    bytes[19 + RECORD_HEADER_BYTES] ^= 1;
    fs::write(&segment, bytes).unwrap();
    let base = dir.path().join("quarantine");
    fs::create_dir(&base).unwrap();
    let name = format!("{}-19.bin", segment.file_name().unwrap().to_str().unwrap());
    let temporary = base.join(format!("{name}.tmp"));
    fs::write(&temporary, b"interrupted recovery").unwrap();
    let (mut journal, first_report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    let first_path = first_report.corrupted_tail.unwrap().quarantine_path;
    let first_bytes = fs::read(&first_path).unwrap();
    assert_eq!(journal.append(b"different").unwrap(), 2);
    drop(journal);
    let mut bytes = fs::read(&segment).unwrap();
    bytes[19 + RECORD_HEADER_BYTES] ^= 2;
    fs::write(&segment, bytes).unwrap();
    let (journal, second_report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    let second_path = second_report.corrupted_tail.unwrap().quarantine_path;
    assert_eq!(journal.last_seq(), 1);
    assert_ne!(first_path, second_path);
    assert_eq!(fs::read(first_path).unwrap(), first_bytes);
    assert_eq!(fs::read(temporary).unwrap(), b"interrupted recovery");
    assert_ne!(fs::read(second_path).unwrap(), first_bytes);
}

#[test]
fn batch_rotates_as_a_unit_even_beyond_segment_target() {
    let dir = TempDir::new().unwrap();
    let options = JournalOptions {
        segment_max_bytes: 40,
        ..JournalOptions::default()
    };
    let mut journal = Journal::create(dir.path(), JournalId(Uuid::now_v7()), options).unwrap();
    journal.append(b"data").unwrap();
    assert_eq!(
        journal.append_batch(&[b"data", b"data", b"data"]).unwrap(),
        2..=4
    );
    let paths = segments(dir.path());
    assert_eq!(paths.len(), 2);
    assert_eq!(fs::metadata(&paths[0]).unwrap().len(), 20);
    assert_eq!(fs::metadata(&paths[1]).unwrap().len(), 60);
    drop(journal);
    let (journal, report) = Journal::open(dir.path(), options).unwrap();
    assert_eq!(journal.last_seq(), 4);
    assert!(report.corrupted_tail.is_none());
}

#[test]
fn io_failure_requires_reopen_before_more_mutations() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    let temporary = dir.path().join("ack.tmp");
    fs::create_dir(&temporary).unwrap();
    assert!(matches!(journal.ack(1), Err(JournalError::Io(_))));
    assert!(matches!(journal.append(b"two"), Err(JournalError::Io(_))));
    assert!(matches!(journal.compact(), Err(JournalError::Io(_))));
    fs::remove_dir(temporary).unwrap();
    drop(journal);
    let (mut journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    assert_eq!(journal.acked_seq(), 0);
    assert!(report.corrupted_tail.is_none());
    assert_eq!(journal.append(b"two").unwrap(), 2);
}

#[test]
fn corruption_preserves_ack_and_blocks_reuse_of_acked_sequences() {
    let dir = TempDir::new().unwrap();
    let mut journal = create(dir.path());
    journal.append(b"one").unwrap();
    journal.append(b"two").unwrap();
    journal.ack(2).unwrap();
    drop(journal);
    let path = segments(dir.path()).remove(0);
    let mut bytes = fs::read(&path).unwrap();
    bytes[19 + RECORD_HEADER_BYTES] ^= 1;
    fs::write(path, bytes).unwrap();
    let (mut journal, report) = Journal::open(dir.path(), JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    assert_eq!(journal.acked_seq(), 2);
    assert_eq!(report.corrupted_tail.unwrap().from_seq, 2);
    assert!(
        matches!(journal.append(b"replacement"), Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert!(
        matches!(journal.compact(), Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_records(&journal.read_from(1, usize::MAX).unwrap(), &[(1, b"one")]);
}
