use std::panic::{AssertUnwindSafe, catch_unwind};

use canterlot_proto::worker::v1::CancelInput;
use tempfile::TempDir;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AppendPoint {
    Inbox,
    Cj,
    Receipt,
}

impl WorkerStore {
    pub(super) fn after_append(&self, point: AppendPoint) {
        assert_ne!(self.crash_after, Some(point), "simulated crash");
    }
}

fn recover_after_crash(point: AppendPoint, rejected: bool) {
    let dir = TempDir::new().unwrap();
    let cmd = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 3,
        kind: (!rejected).then(|| command::Kind::CancelInput(CancelInput::default())),
        ..Default::default()
    };
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let journal_id = store.cj_journal_id();
    store.crash_after = Some(point);
    assert!(catch_unwind(AssertUnwindSafe(|| store.accept_command(&cmd))).is_err());
    assert_eq!(store.inbox.last_seq(), u64::from(!rejected));
    assert_eq!(store.cj.last_seq(), u64::from(point != AppendPoint::Inbox));
    assert_eq!(
        store.receipts.last_seq(),
        u64::from(point == AppendPoint::Receipt)
    );
    drop(store);

    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_journal_id(), journal_id);
    let facts = store.cj_records_from(1, usize::MAX).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].seq, 1);
    let expected = if rejected {
        record_body::Body::CommandRejected(CommandRejected {
            command_id: cmd.command_id.clone(),
            dispatch_attempt: cmd.dispatch_attempt,
            reason: "unsupported_command".into(),
        })
    } else {
        record_body::Body::CommandStored(CommandStored {
            command_id: cmd.command_id.clone(),
            dispatch_attempt: cmd.dispatch_attempt,
        })
    };
    assert_eq!(facts[0].body.as_ref().unwrap().body, Some(expected));
    let receipts = store.receipts.read_from(1, usize::MAX).unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt: ReceiptEntry =
        toml::from_str(std::str::from_utf8(&receipts[0].payload).unwrap()).unwrap();
    assert_eq!(receipt.command_id.to_string(), cmd.command_id);
    assert_eq!(receipt.attempt, cmd.dispatch_attempt);
    assert_eq!(matches!(receipt.class, ReceiptClass::Rejected), rejected);

    let mut redelivery = cmd.clone();
    redelivery.dispatch_attempt += 1;
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
    recover_after_crash(AppendPoint::Inbox, false);
}

#[test]
fn crash_after_stored_cj_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, false);
}

#[test]
fn crash_after_stored_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Receipt, false);
}

#[test]
fn crash_after_rejected_cj_append_is_reconciled() {
    recover_after_crash(AppendPoint::Cj, true);
}

#[test]
fn crash_after_rejected_receipt_append_is_reconciled() {
    recover_after_crash(AppendPoint::Receipt, true);
}
