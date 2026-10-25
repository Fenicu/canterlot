use std::io::Write;

use super::*;

#[derive(Debug, Default)]
pub(super) struct WriteHook {
    fail_after: Option<usize>,
    calls: usize,
}

pub(super) struct AppendWriter<'a> {
    pub file: &'a mut File,
    pub hook: &'a mut WriteHook,
}

impl Write for AppendWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hook.calls += 1;
        let limit = self.hook.fail_after.unwrap_or(bytes.len()).min(bytes.len());
        if limit == 0 && !bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "injected write failure",
            ));
        }
        let written = self.file.write(&bytes[..limit])?;
        if let Some(remaining) = &mut self.hook.fail_after {
            *remaining -= written;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[test]
fn failed_append_rolls_back_before_reopen() {
    for segment_max_bytes in [0, u64::MAX] {
        for batch in [false, true] {
            let payloads: &[&[u8]] = if batch {
                &[b"two", b"three"]
            } else {
                &[b"two"]
            };
            let batch_bytes: usize = payloads.iter().map(|p| 16 + p.len()).sum();
            for fail_after in 0..batch_bytes {
                let dir = tempfile::tempdir().unwrap();
                let options = JournalOptions {
                    segment_max_bytes,
                    ..JournalOptions::default()
                };
                let mut journal =
                    Journal::create(dir.path(), JournalId(uuid::Uuid::now_v7()), options).unwrap();
                journal.append(b"one").unwrap();
                let before = fs::read(&journal.segments[0].path).unwrap();
                journal.write_hook.fail_after = Some(fail_after);
                let result = if batch {
                    journal.append_batch(payloads).map(|range| *range.end())
                } else {
                    journal.append(payloads[0])
                };
                assert!(
                    matches!(result, Err(JournalError::Io(error)) if error.kind() == io::ErrorKind::StorageFull)
                );
                assert_eq!(journal.last_seq(), 1);
                assert_eq!(
                    journal.read_from(1, usize::MAX).unwrap(),
                    vec![Record {
                        seq: 1,
                        payload: b"one".to_vec()
                    }]
                );
                assert!(matches!(journal.append(b"retry"), Err(JournalError::Io(_))));
                assert!(matches!(journal.ack(1), Err(JournalError::Io(_))));
                assert!(matches!(journal.compact(), Err(JournalError::Io(_))));
                assert_eq!(
                    fs::read(&journal.segments[0].path).unwrap()[..before.len()],
                    before
                );
                drop(journal);
                let (mut journal, report) = Journal::open(dir.path(), options).unwrap();
                assert_eq!(
                    journal.last_seq(),
                    1,
                    "batch={batch}, fail_after={fail_after}, segment_max_bytes={segment_max_bytes}"
                );
                assert!(
                    report.corrupted_tail.is_none(),
                    "batch={batch}, fail_after={fail_after}, segment_max_bytes={segment_max_bytes}"
                );
                assert_eq!(journal.is_sealed(), None);
                assert!(!dir.path().join("quarantine").exists());
                assert_eq!(journal.append(b"replacement").unwrap(), 2);
                drop(journal);
                let (journal, report) = Journal::open(dir.path(), options).unwrap();
                assert!(report.corrupted_tail.is_none());
                assert_eq!(journal.is_sealed(), None);
                assert_eq!(journal.last_seq(), 2);
            }
        }
    }
}

#[test]
fn batch_uses_one_contiguous_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = Journal::create(
        dir.path(),
        JournalId(uuid::Uuid::now_v7()),
        JournalOptions::default(),
    )
    .unwrap();
    journal.append_batch(&[b"one", b"two", b"three"]).unwrap();
    assert_eq!(journal.write_hook.calls, 1);
}

#[test]
fn interrupted_create_leaves_only_a_sibling_temporary_directory() {
    for existing_empty in [false, true] {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("journal");
        if existing_empty {
            fs::create_dir(&dir).unwrap();
        }
        let mut leftover = PathBuf::new();
        let result = Journal::create_inner(
            &dir,
            JournalId(uuid::Uuid::now_v7()),
            JournalOptions::default(),
            |staging| {
                leftover = staging.to_path_buf();
                Err(io::Error::other("simulated crash before header"))
            },
        );
        assert!(matches!(result, Err(JournalError::Io(_))));
        assert!(leftover.is_dir());
        assert!(!leftover.join("header").exists());
        assert!(leftover.join("ack").is_file());
        let mut journal = Journal::create(
            &dir,
            JournalId(uuid::Uuid::now_v7()),
            JournalOptions::default(),
        )
        .unwrap();
        assert_ne!(leftover, dir);
        assert_eq!(leftover.parent(), dir.parent());
        assert_eq!(journal.append(b"one").unwrap(), 1);
        assert!(leftover.join("ack").is_file());
        drop(journal);
        let (journal, report) = Journal::open(&dir, JournalOptions::default()).unwrap();
        assert_eq!(journal.last_seq(), 1);
        assert!(report.corrupted_tail.is_none());
    }
}

#[test]
fn overlapping_create_returns_locked_after_another_creator_publishes() {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("journal");
    let mut owner = None;
    let result = Journal::create_inner(
        &dir,
        JournalId(uuid::Uuid::now_v7()),
        JournalOptions::default(),
        |_| {
            owner = Some(
                Journal::create(
                    &dir,
                    JournalId(uuid::Uuid::now_v7()),
                    JournalOptions::default(),
                )
                .unwrap(),
            );
            Ok(())
        },
    );
    assert!(matches!(result, Err(JournalError::Locked)));
    assert_eq!(owner.as_mut().unwrap().append(b"one").unwrap(), 1);
    drop(owner);
    let (journal, report) = Journal::open(&dir, JournalOptions::default()).unwrap();
    assert_eq!(journal.last_seq(), 1);
    assert!(report.corrupted_tail.is_none());
}
