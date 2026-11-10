use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use canterlot_proto::worker::v1::{CancelInput, command};
use tempfile::TempDir;

use super::*;

fn private_dir() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AppendPoint {
    Inbox,
    Cj,
    Receipt,
    InboxAck,
    InboxCompact,
}

thread_local! {
    static OPEN_CRASH_AFTER: Cell<Option<AppendPoint>> = const { Cell::new(None) };
}

pub(super) fn take_open_crash_point() -> Option<AppendPoint> {
    OPEN_CRASH_AFTER.take()
}

impl WorkerStore {
    pub(super) fn after_append(&self, point: AppendPoint) {
        assert_ne!(self.crash_after, Some(point), "simulated crash");
    }
}

fn recover_after_crash(point: AppendPoint, rejected: bool, repair_crashes: &[AppendPoint]) {
    let dir = private_dir();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 3,
        kind: (!rejected).then(|| command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let journal_id = store.cj_journal_id().unwrap();
    store.crash_after = Some(point);
    assert!(catch_unwind(AssertUnwindSafe(|| store.accept_command(&cmd))).is_err());
    assert_eq!(store.inbox.last_seq(), u64::from(!rejected));
    assert_eq!(store.cj.last_seq(), u64::from(point != AppendPoint::Inbox));
    assert_eq!(
        store.receipts.last_seq(),
        u64::from(point == AppendPoint::Receipt)
    );
    drop(store);

    for &repair_point in repair_crashes {
        OPEN_CRASH_AFTER.set(Some(repair_point));
        assert!(catch_unwind(|| WorkerStore::open(dir.path())).is_err());
        assert_eq!(open_journal(&dir.path().join("cj")).unwrap().last_seq(), 1);
        assert_eq!(
            open_journal(&dir.path().join("receipts"))
                .unwrap()
                .last_seq(),
            u64::from(repair_point == AppendPoint::Receipt)
        );
    }

    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_journal_id().unwrap(), journal_id);
    let facts = store.cj_records_from(1, usize::MAX).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].seq, 1);
    if rejected {
        assert_eq!(
            facts[0].body.as_ref().unwrap().body,
            Some(record_body::Body::CommandRejected(CommandRejected {
                command_id: cmd.command_id.clone(),
                dispatch_attempt: cmd.dispatch_attempt,
                reason: "unsupported_command".into(),
            }))
        );
    } else {
        assert!(matches!(
            &facts[0].body.as_ref().unwrap().body,
            Some(record_body::Body::CommandStored(CommandStored { command_id, dispatch_attempt, machine_time_ms }))
                if command_id == &cmd.command_id && *dispatch_attempt == cmd.dispatch_attempt
                    && *machine_time_ms > 0 && *machine_time_ms <= super::machine_time_ms()
        ));
    }
    let receipts = store.receipts.read_from(1, usize::MAX).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt: ReceiptEntry =
        toml::from_str(std::str::from_utf8(&receipts[0].payload).unwrap()).unwrap();
    assert_eq!(receipt.command_id.to_string(), cmd.command_id);
    assert_eq!(receipt.attempt, cmd.dispatch_attempt);
    assert_eq!(matches!(receipt.class, ReceiptClass::Rejected), rejected);

    let mut redelivery = cmd.clone();

    redelivery.kind = if rejected {
        Some(command::Kind::CancelInput(CancelInput::default()))
    } else {
        None
    };
    assert_eq!(
        store.accept_command(&redelivery).unwrap(),
        Accept::Duplicate
    );
    assert_eq!(store.cj.last_seq(), 1);
    assert_eq!(store.receipts.last_seq(), 1);
    let inbox = store.inbox.read_from(1, usize::MAX).unwrap();
    assert_eq!(inbox.len(), usize::from(!rejected));
    if !rejected {
        assert_eq!(Command::decode(inbox[0].payload.as_slice()).unwrap(), cmd);
    }
    drop(store);

    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj.last_seq(), 1);
    assert_eq!(store.receipts.last_seq(), 1);
    store.on_cj_ack(1).unwrap();
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(
        store.accept_command(&redelivery).unwrap(),
        Accept::Duplicate
    );
    assert_eq!(store.cj.first_seq(), 2);
    assert_eq!(store.cj.last_seq(), 1);
    assert_eq!(store.inbox.last_seq(), u64::from(!rejected));
    assert_eq!(store.receipts.last_seq(), 1);
}

#[test]
fn crash_after_inbox_append_is_reconciled() {
    recover_after_crash(AppendPoint::Inbox, false, &[]);
}

#[test]
fn crash_after_stored_cj_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, false, &[]);
}

#[test]
fn crash_after_stored_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Receipt, false, &[]);
}

#[test]
fn crash_after_rejected_cj_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, true, &[]);
}

#[test]
fn crash_after_rejected_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Receipt, true, &[]);
}

#[test]
fn crash_after_reconciled_cj_append_is_reconciled() {
    recover_after_crash(AppendPoint::Inbox, false, &[AppendPoint::Cj]);
}

#[test]
fn crash_after_reconciled_inbox_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Inbox, false, &[AppendPoint::Receipt]);
}

#[test]
fn crash_after_reconciled_stored_cj_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, false, &[AppendPoint::Receipt]);
}

#[test]
fn crash_after_reconciled_rejected_cj_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, true, &[AppendPoint::Receipt]);
}

#[test]
fn repeated_reconciliation_crashes_are_reconciled() {
    recover_after_crash(
        AppendPoint::Inbox,
        false,
        &[AppendPoint::Cj, AppendPoint::Receipt],
    );
}

#[test]
fn write_failure_after_inbox_poison_all_calls_until_reopen() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    store.fail_after = Some(AppendPoint::Inbox);
    assert!(store.accept_command(&cmd).is_err());
    assert!(matches!(
        store.accept_command(&cmd),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.cj_journal_id(),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(store.cj_cursor(), Err(StoreError::NeedsReopen)));
    assert!(matches!(
        store.cj_records_from(1, 1024),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(store.on_cj_ack(0), Err(StoreError::NeedsReopen)));
    assert!(matches!(
        store.restore_generation(),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.set_restore_generation(Uuid::now_v7()),
        Err(StoreError::NeedsReopen)
    ));
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
}

#[test]
fn ack_beyond_end_is_not_store_loss_and_generation_errors_poison_the_store() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    // Ошибка протокола хаба: журнал не тронут, хранилище не закрыто.
    assert!(matches!(
        store.on_cj_ack(1),
        Err(StoreError::CjAckBeyondEnd)
    ));
    assert!(store.on_cj_ack(0).is_ok());
    assert!(store.restore_generation().is_ok());
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    std::fs::create_dir(dir.path().join("hub.toml")).unwrap();
    assert!(store.set_restore_generation(Uuid::now_v7()).is_err());
    assert!(matches!(store.cj_cursor(), Err(StoreError::NeedsReopen)));
    drop(store);
    std::fs::remove_dir(dir.path().join("hub.toml")).unwrap();
    assert!(WorkerStore::open(dir.path()).unwrap().cj_cursor().is_ok());
}

#[test]
fn invalid_command_id_leaves_all_journals_and_receipts_unchanged() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let mut cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    store.accept_command(&cmd).unwrap();
    let before = [&store.cj, &store.inbox, &store.receipts]
        .map(|journal| journal.read_from(1, usize::MAX).unwrap());
    for id in ["invalid".into(), "x".repeat(MAX_MESSAGE_BYTES)] {
        cmd.command_id = id;
        assert_eq!(
            store.accept_command(&cmd).unwrap(),
            Accept::DroppedInvalidId
        );
    }
    let after = [&store.cj, &store.inbox, &store.receipts]
        .map(|journal| journal.read_from(1, usize::MAX).unwrap());
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(before.len(), after.len());
        for (before, after) in before.iter().zip(after) {
            assert_eq!(before.seq, after.seq);
            assert_eq!(before.payload, after.payload);
        }
    }
    assert_eq!(store.index.len(), 1);
    drop(store);
    let store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.index.len(), 1);
    assert_eq!(store.receipts.last_seq(), 1);
    assert_eq!(store.cj.last_seq(), 1);
    assert_eq!(store.inbox.last_seq(), 1);
}

#[test]
fn invalid_command_id_emits_one_warning_without_command_data() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let output = Output(Arc::default());
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(move || writer.clone())
        .finish();
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let cmd = Command {
        command_id: "private-invalid-id".into(),
        execution_id: "private-payload".into(),
        ..Default::default()
    };
    tracing::subscriber::with_default(subscriber, || {
        assert_eq!(
            store.accept_command(&cmd).unwrap(),
            Accept::DroppedInvalidId
        );
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(log.contains("WARN"));
    assert!(log.contains("invalid_command_id"));
    assert!(!log.contains(&cmd.command_id));
    assert!(!log.contains(&cmd.execution_id));
}

fn assert_poisoned(store: &mut WorkerStore, cmd: &Command) {
    assert!(matches!(
        store.command_state(&cmd.command_id),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.inbox_command(&cmd.command_id, cmd.dispatch_attempt),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.mark_terminal(&cmd.command_id, cmd.dispatch_attempt),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.ownership_mut(),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.append_cj_fact(record_body::Body::CommandStored(CommandStored::default())),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(
        store.accept_command(cmd),
        Err(StoreError::NeedsReopen)
    ));
    assert!(matches!(store.cj_cursor(), Err(StoreError::NeedsReopen)));
}

#[test]
fn fact_and_terminal_write_failures_require_reopen() {
    use canterlot_proto::worker::v1::InputTransmissionStarted;
    for point in [AppendPoint::Cj, AppendPoint::Receipt] {
        let dir = private_dir();
        let cmd = Command {
            command_id: Uuid::now_v7().to_string(),
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
            ..Default::default()
        };
        let mut store = WorkerStore::open(dir.path()).unwrap();
        store.accept_command(&cmd).unwrap();
        store.fail_after = Some(point);
        assert!(
            store
                .append_cj_fact(record_body::Body::TransmissionStarted(
                    InputTransmissionStarted {
                        command_id: cmd.command_id.clone(),
                        dispatch_attempt: cmd.dispatch_attempt,
                        machine_time_ms: 1,
                    }
                ))
                .is_err()
        );
        assert_poisoned(&mut store, &cmd);
        drop(store);
        let mut store = WorkerStore::open(dir.path()).unwrap();
        let mut next = cmd.clone();
        next.dispatch_attempt += 1;
        assert!(matches!(
            store.accept_command(&next).unwrap(),
            Accept::Rejected {
                reason: "already_transmitted",
                ..
            }
        ));
    }
    for point in [
        AppendPoint::Receipt,
        AppendPoint::InboxAck,
        AppendPoint::InboxCompact,
    ] {
        let dir = private_dir();
        let cmd = Command {
            command_id: Uuid::now_v7().to_string(),
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
            ..Default::default()
        };
        let mut store = WorkerStore::open(dir.path()).unwrap();
        store.accept_command(&cmd).unwrap();
        store.fail_after = Some(point);
        assert!(
            store
                .mark_terminal(&cmd.command_id, cmd.dispatch_attempt)
                .is_err()
        );
        assert_poisoned(&mut store, &cmd);
        drop(store);
        let store = WorkerStore::open(dir.path()).unwrap();
        assert!(
            store
                .command_state(&cmd.command_id)
                .unwrap()
                .unwrap()
                .terminal
        );
        assert_eq!(store.inbox.acked_seq(), 1);
        assert_eq!(store.inbox.first_seq(), 2);
    }
}

#[test]
fn terminal_crashes_at_every_boundary_finish_compaction_on_open() {
    for point in [
        AppendPoint::Receipt,
        AppendPoint::InboxAck,
        AppendPoint::InboxCompact,
    ] {
        let dir = private_dir();
        let cmd = Command {
            command_id: Uuid::now_v7().to_string(),
            kind: Some(command::Kind::CancelInput(CancelInput::default())),
            ..Default::default()
        };
        let mut store = WorkerStore::open(dir.path()).unwrap();
        store.accept_command(&cmd).unwrap();
        store.crash_after = Some(point);
        assert!(
            catch_unwind(AssertUnwindSafe(
                || store.mark_terminal(&cmd.command_id, cmd.dispatch_attempt)
            ))
            .is_err()
        );
        drop(store);
        for repair in [AppendPoint::InboxAck, AppendPoint::InboxCompact] {
            OPEN_CRASH_AFTER.set(Some(repair));
            assert!(catch_unwind(|| WorkerStore::open(dir.path())).is_err());
        }
        let mut store = WorkerStore::open(dir.path()).unwrap();
        assert_eq!(store.inbox.acked_seq(), 1);
        assert_eq!(store.inbox.first_seq(), 2);
        assert!(
            store
                .command_state(&cmd.command_id)
                .unwrap()
                .unwrap()
                .terminal
        );
        assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
    }
}

#[test]
fn ownership_append_error_poisons_parent_store() {
    use canterlot_proto::worker::ipc::Ownership;
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        ..Default::default()
    };
    let record = Ownership {
        execution_id: Uuid::now_v7().to_string(),
        exec_dir: "x".repeat(MAX_MESSAGE_BYTES + 1),
        ..Default::default()
    };
    assert!(store.ownership_mut().unwrap().append(record).is_err());
    assert_poisoned(&mut store, &cmd);
    drop(store);
    let store = WorkerStore::open(dir.path()).unwrap();
    assert!(store.ownership().live().is_empty());
    assert_eq!(store.cj_cursor().unwrap().last_seq, 0);
}

#[test]
fn pilot_receipt_survives_crash_before_receipt_append() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        kind: Some(command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    store.crash_after = Some(AppendPoint::Inbox);
    assert!(
        catch_unwind(AssertUnwindSafe(
            || store.accept_command_with_pilot(&cmd, true)
        ))
        .is_err()
    );
    drop(store);
    let store = WorkerStore::open(dir.path()).unwrap();
    assert!(
        store
            .command_state(&cmd.command_id)
            .unwrap()
            .unwrap()
            .pilot_v1
    );
}
