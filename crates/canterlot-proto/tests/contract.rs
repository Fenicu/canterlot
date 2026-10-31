use canterlot_proto::limits::{
    LimitError, MAX_EVENT_BATCH_BYTES, MAX_FILE_CHUNK_BYTES, MAX_INPUT_META_BYTES,
    MAX_INPUT_TEXT_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, check_deliver_input,
    check_record_batch,
};
use canterlot_proto::worker::v1::{
    BlockDelta, Command, ContentBlock, DeliverInput, Hello, InputDispatchDeferred, JournalCursor,
    JournalRecord, JournalRecords, RecordBody, StartExecution, StreamKind, TextBlock, command,
    content_block, record_body,
};
use prost::Message;

fn deliver_input(text: String) -> Command {
    Command {
        command_id: "019a1b10-0000-7000-8000-000000000001".into(),
        dispatch_attempt: 2,
        execution_id: "019a1b10-0000-7000-8000-000000000002".into(),
        execution_generation: 3,
        kind: Some(command::Kind::DeliverInput(DeliverInput {
            input_id: "019a1b10-0000-7000-8000-000000000003".into(),
            conversation_id: "019a1b10-0000-7000-8000-000000000004".into(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock { text })),
            }],
            ..Default::default()
        })),
    }
}

fn record_batch(append: String) -> JournalRecords {
    JournalRecords {
        journal_id: "019a1b10-0000-7000-8000-000000000005".into(),
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::BlockDelta(BlockDelta {
                    block_id: "019a1b10-0000-7000-8000-000000000006".into(),
                    base_rev: 1,
                    rev: 2,
                    append,
                })),
            }),
        }],
    }
}

#[test]
fn command_roundtrip() {
    let command = deliver_input("Hello, world!".into());
    let decoded = Command::decode(command.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, command);
}

#[test]
fn dispatch_deferred_record_roundtrip() {
    let deferred = InputDispatchDeferred {
        command_id: "019a1b10-0000-7000-8000-000000000001".into(),
        dispatch_attempt: 7,
        reason: "no_live_execution".into(),
    };
    let mut expected =
        b"\x0a\x24019a1b10-0000-7000-8000-000000000001\x10\x07\x1a\x11no_live_execution".to_vec();
    assert_eq!(deferred.encode_to_vec(), expected);
    let body = RecordBody {
        body: Some(record_body::Body::DispatchDeferred(deferred)),
    };
    let mut expected_body = vec![0x92, 0x01, expected.len() as u8];
    expected_body.append(&mut expected);
    assert_eq!(body.encode_to_vec(), expected_body);
    let record = JournalRecord {
        seq: 42,
        body: Some(body),
    };
    assert_eq!(
        JournalRecord::decode(record.encode_to_vec().as_slice()).unwrap(),
        record
    );
}

#[test]
fn unknown_command_kind_is_none_not_default() {
    let encoded = [
        0x0a, 0x06, b'f', b'u', b't', b'u', b'r', b'e', 0x9a, 0x06, 0x03, 0x08, 0x96, 0x01,
    ];
    let decoded = Command::decode(encoded.as_slice()).unwrap();
    assert!(decoded.kind.is_none());
    assert_eq!(decoded.command_id, "future");
}

#[test]
fn deliver_input_text_limit_is_bytes() {
    let text = "é".repeat(MAX_INPUT_TEXT_BYTES / 2);
    assert_eq!(text.len(), 262_144);
    assert!(text.chars().count() < text.len());
    assert_eq!(check_deliver_input(&deliver_input(text.clone())), Ok(()));

    let oversized = format!("{text}a");
    assert_eq!(oversized.len(), 262_145);
    assert_eq!(
        check_deliver_input(&deliver_input(oversized)),
        Err(LimitError::InputText { actual: 262_145 })
    );
}

#[test]
fn deliver_input_message_limit() {
    let mut command = deliver_input(String::new());
    let Some(command::Kind::DeliverInput(input)) = &mut command.kind else {
        unreachable!();
    };
    input.content = vec![ContentBlock::default(); MAX_MESSAGE_BYTES / 2];
    let actual = command.encoded_len();
    assert!(actual > 1_048_576);
    assert_eq!(
        check_deliver_input(&command),
        Err(LimitError::Message { actual })
    );
}

#[test]
fn record_batch_limit() {
    let batch = record_batch("x".repeat(MAX_EVENT_BATCH_BYTES));
    let actual = batch.encoded_len();
    assert!(actual > 524_288);
    assert_eq!(
        check_record_batch(&batch),
        Err(LimitError::EventBatch { actual })
    );
}

#[test]
fn hello_has_no_credential_field() {
    let hello = Hello {
        installation_id: "019a1b10-0000-7000-8000-000000000007".into(),
        machine_id: "019a1b10-0000-7000-8000-000000000008".into(),
        restore_generation: "019a1b10-0000-7000-8000-00000000000b".into(),
        protocol_version: PROTOCOL_VERSION,
        required_capabilities: vec!["journal_v1".into()],
        optional_capabilities: vec!["text_blocks_v1".into()],
        connection_id: "019a1b10-0000-7000-8000-000000000009".into(),
        cj_journal_id: "019a1b10-0000-7000-8000-000000000005".into(),
        cursors: vec![JournalCursor {
            journal_id: "019a1b10-0000-7000-8000-000000000005".into(),
            first_stored_seq: 1,
            last_seq: 10,
            acked_seq_known_to_worker: 9,
        }],
        boot_id: "019a1b10-0000-7000-8000-00000000000a".into(),
        stream_kind: StreamKind::Control as i32,
        ..Default::default()
    };
    assert_eq!(
        Hello::decode(hello.encode_to_vec().as_slice()).unwrap(),
        hello
    );
}

#[test]
fn limits_match_contract() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(MAX_MESSAGE_BYTES, 1_048_576);
    assert_eq!(MAX_INPUT_TEXT_BYTES, 262_144);
    assert_eq!(MAX_INPUT_META_BYTES, 65_536);
    assert_eq!(MAX_EVENT_BATCH_BYTES, 524_288);
    assert_eq!(MAX_FILE_CHUNK_BYTES, 131_072);
    assert_eq!(canterlot_proto::limits::IPC_MAX_MESSAGE_BYTES, 1_048_576);
    assert_eq!(canterlot_proto::limits::IPC_PROTOCOL_VERSION, 1);
    assert_eq!(canterlot_proto::limits::MAX_SJ_BATCH_RECORDS, 256);
    assert_eq!(canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES, 16_384);
    assert_eq!(
        canterlot_proto::limits::BLOCK_FINAL_INLINE_MAX_BYTES,
        65_536
    );
}

#[test]
fn deliver_input_text_limit_sums_content_blocks() {
    let mut command = deliver_input("é".repeat(MAX_INPUT_TEXT_BYTES / 2));
    let Some(command::Kind::DeliverInput(input)) = &mut command.kind else {
        unreachable!();
    };
    input.content.push(ContentBlock {
        block: Some(content_block::Block::Text(TextBlock { text: "a".into() })),
    });
    assert_eq!(
        check_deliver_input(&command),
        Err(LimitError::InputText { actual: 262_145 })
    );
}

#[test]
fn deliver_input_metadata_limit_includes_wire_overhead() {
    let text = "é".repeat(100);
    let text_len = text.len();
    let mut command = deliver_input(text);
    command.command_id = "m".repeat(MAX_INPUT_META_BYTES - 1000);
    let padding = MAX_INPUT_META_BYTES - (command.encoded_len() - text_len);
    command.command_id.push_str(&"m".repeat(padding));
    assert_eq!(command.encoded_len() - text_len, 65_536);
    assert_eq!(check_deliver_input(&command), Ok(()));

    command.command_id.push('m');
    assert_eq!(command.encoded_len() - text_len, 65_537);
    assert_eq!(
        check_deliver_input(&command),
        Err(LimitError::InputMeta { actual: 65_537 })
    );
}

#[test]
fn message_limit_applies_to_other_command_kinds() {
    let mut command = Command {
        kind: Some(command::Kind::StartExecution(StartExecution {
            workdir: "x".repeat(MAX_MESSAGE_BYTES - 100),
            ..Default::default()
        })),
        ..Default::default()
    };
    let padding = MAX_MESSAGE_BYTES - command.encoded_len();
    let Some(command::Kind::StartExecution(start)) = &mut command.kind else {
        unreachable!();
    };
    start.workdir.push_str(&"x".repeat(padding));
    assert_eq!(command.encoded_len(), 1_048_576);
    assert_eq!(check_deliver_input(&command), Ok(()));

    let Some(command::Kind::StartExecution(start)) = &mut command.kind else {
        unreachable!();
    };
    start.workdir.push('x');
    assert_eq!(command.encoded_len(), 1_048_577);
    assert_eq!(
        check_deliver_input(&command),
        Err(LimitError::Message { actual: 1_048_577 })
    );
}

#[test]
fn record_batch_limit_includes_wire_overhead() {
    let batch = record_batch("x".repeat(MAX_EVENT_BATCH_BYTES - 1000));
    let append_len = MAX_EVENT_BATCH_BYTES - (batch.encoded_len() - (MAX_EVENT_BATCH_BYTES - 1000));
    let batch = record_batch("x".repeat(append_len));
    assert_eq!(batch.encoded_len(), 524_288);
    assert_eq!(check_record_batch(&batch), Ok(()));

    let batch = record_batch("x".repeat(append_len + 1));
    assert_eq!(batch.encoded_len(), 524_289);
    assert_eq!(
        check_record_batch(&batch),
        Err(LimitError::EventBatch { actual: 524_289 })
    );
}

#[test]
fn empty_messages_pass_size_checks() {
    assert_eq!(check_deliver_input(&Command::default()), Ok(()));
    assert_eq!(check_record_batch(&JournalRecords::default()), Ok(()));
}

#[test]
fn enum_names_are_type_prefixed_and_numbers_stay_stable() {
    use canterlot_proto::worker::v1::RejectReason;
    for (number, name) in [
        (0, "STREAM_KIND_UNSPECIFIED"),
        (1, "STREAM_KIND_CONTROL"),
        (2, "STREAM_KIND_DATA"),
    ] {
        assert_eq!(StreamKind::try_from(number).unwrap().as_str_name(), name);
        assert_eq!(StreamKind::from_str_name(name).unwrap() as i32, number);
    }
    for (number, name) in [
        "UNSPECIFIED",
        "CREDENTIAL_REVOKED",
        "INSTALLATION_MISMATCH",
        "PROTOCOL_INCOMPATIBLE",
        "UNKNOWN_REQUIRED_CAPABILITY",
        "IDENTITY_IN_USE",
        "CONNECTION_BINDING_MISMATCH",
        "HUB_NOT_OPERATIONAL",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("REJECT_REASON_{name}");
        assert_eq!(
            RejectReason::try_from(number as i32).unwrap().as_str_name(),
            name
        );
        assert_eq!(
            RejectReason::from_str_name(&name).unwrap() as i32,
            number as i32
        );
    }
}

#[test]
fn controls_preserve_target_id_field_one_and_attempt_field_two() {
    use canterlot_proto::worker::v1::{CancelInput, WithdrawInput};
    let encoded = [0x0a, 1, b'x', 0x10, 7];
    assert_eq!(
        CancelInput::decode(encoded.as_slice())
            .unwrap()
            .encode_to_vec(),
        encoded
    );
    assert_eq!(
        WithdrawInput::decode(encoded.as_slice())
            .unwrap()
            .encode_to_vec(),
        encoded
    );
    let old = [0x0a, 1, b'x'];
    assert_eq!(
        CancelInput::decode(old.as_slice()).unwrap().encode_to_vec(),
        old
    );
    assert_eq!(
        WithdrawInput::decode(old.as_slice())
            .unwrap()
            .encode_to_vec(),
        old
    );
}

#[test]
fn debug_redacts_text_at_all_message_levels() {
    let marker = "PRIVATE_PROTO_MARKER_123";
    let command = deliver_input(marker.into());
    let Some(command::Kind::DeliverInput(input)) = &command.kind else {
        unreachable!()
    };
    let Some(content_block::Block::Text(text)) = &input.content[0].block else {
        unreachable!()
    };
    for value in [
        format!("{text:?}"),
        format!("{input:?}"),
        format!("{command:?}"),
    ] {
        assert!(!value.contains(marker));
        assert!(value.contains(&marker.len().to_string()));
    }
}

#[test]
fn local_state_reset_rejection_has_stable_wire_value() {
    use canterlot_proto::worker::v1::{RejectReason, Rejected};
    let reason = RejectReason::try_from(8).expect("local_state_reset enum value");
    assert_eq!(reason.as_str_name(), "REJECT_REASON_LOCAL_STATE_RESET");
    let rejected = Rejected {
        reason: 8,
        detail: String::new(),
        retryable: false,
    };
    assert_eq!(rejected.encode_to_vec(), [8, 8]);
    assert_eq!(Rejected::decode([8, 8].as_slice()).unwrap(), rejected);
}

fn assert_roundtrip<M>(message: M)
where
    M: Message + Default + PartialEq + std::fmt::Debug,
{
    assert_eq!(
        M::decode(message.encode_to_vec().as_slice()).unwrap(),
        message
    );
}

#[test]
fn exec_records_roundtrip() {
    use canterlot_proto::limits::{
        BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES, MAX_SJ_BATCH_RECORDS,
    };
    use canterlot_proto::worker::v1::{
        AdapterDiagnostic, BlockFinal, BlockStarted, CommandStored, ExecutionState, InputAccepted,
        InputTransmissionStarted, InterruptState, InterruptStatus, JournalOpened, WithdrawRefused,
    };

    let variants = [
        (
            19,
            record_body::Body::JournalOpened(JournalOpened {
                execution_id: "execution".into(),
                generation: 2,
                supervisor_protocol_version: 1,
            }),
        ),
        (
            20,
            record_body::Body::InterruptStatus(InterruptStatus {
                command_id: "command".into(),
                dispatch_attempt: 3,
                state: InterruptState::SentToAgent as i32,
            }),
        ),
        (
            21,
            record_body::Body::WithdrawRefused(WithdrawRefused {
                command_id: "command".into(),
                dispatch_attempt: 3,
                reason: "too_late".into(),
            }),
        ),
        (
            22,
            record_body::Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: "execution".into(),
                kind: "unknown_optional_event".into(),
                event_type: "future".into(),
                ..Default::default()
            }),
        ),
    ];
    for (tag, variant) in variants {
        let body = RecordBody {
            body: Some(variant),
        };
        let encoded = body.encode_to_vec();
        assert_eq!(&encoded[..2], &[((tag << 3 | 2) & 0x7f) as u8 | 0x80, 1]);
        assert_roundtrip(body);
    }
    assert_roundtrip(ExecutionState {
        execution_id: "execution".into(),
        generation: 2,
        state: "running".into(),
        outcome: String::new(),
        native_conversation_id: "native".into(),
    });
    assert_roundtrip(BlockStarted {
        block_id: "block".into(),
        rev: 1,
        kind: "text".into(),
        input_command_id: "command".into(),
        parent_block_id: "parent".into(),
        ..Default::default()
    });
    assert_roundtrip(CommandStored {
        command_id: "command".into(),
        dispatch_attempt: 3,
        machine_time_ms: 1000,
    });
    assert_roundtrip(InputTransmissionStarted {
        command_id: "command".into(),
        dispatch_attempt: 3,
        machine_time_ms: 1001,
    });
    assert_roundtrip(InputAccepted {
        command_id: "command".into(),
        dispatch_attempt: 3,
        machine_time_ms: 1002,
    });

    let final_block = BlockFinal {
        block_id: "block".into(),
        rev: 3,
        text: String::new(),
        checksum: "a".repeat(64),
        text_omitted: true,
        ..Default::default()
    };
    assert_roundtrip(final_block.clone());
    let mut batch = record_batch("é".repeat(MAX_BLOCK_DELTA_BYTES / 2));
    batch.records.push(JournalRecord {
        seq: 2,
        body: Some(RecordBody {
            body: Some(record_body::Body::BlockFinal(final_block)),
        }),
    });
    while batch.records.len() < MAX_SJ_BATCH_RECORDS {
        batch.records.push(JournalRecord {
            seq: batch.records.len() as u64 + 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::BlockDelta(BlockDelta {
                    block_id: "block".into(),
                    base_rev: 1,
                    rev: 2,
                    append: "x".into(),
                })),
            }),
        });
    }
    assert_eq!(batch.records.len(), 256);
    assert!(batch.encoded_len() <= MAX_EVENT_BATCH_BYTES);
    assert_eq!(check_record_batch(&batch), Ok(()));
    assert_roundtrip(batch.clone());
    let mut inline_batch = batch;
    inline_batch.records[1].body = Some(RecordBody {
        body: Some(record_body::Body::BlockFinal(BlockFinal {
            text: "x".repeat(BLOCK_FINAL_INLINE_MAX_BYTES),
            text_omitted: false,
            ..Default::default()
        })),
    });
    assert_eq!(check_record_batch(&inline_batch), Ok(()));
    assert_roundtrip(inline_batch);
    for (number, name) in [
        "INTERRUPT_STATE_UNSPECIFIED",
        "SENT_TO_AGENT",
        "ACKNOWLEDGED",
        "COMPLETED",
        "NOT_APPLICABLE",
        "SUPERSEDED",
        "REJECTED_STALE",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            InterruptState::try_from(number as i32)
                .unwrap()
                .as_str_name(),
            name
        );
    }
}

#[test]
fn ipc_messages_roundtrip_and_limit() {
    use canterlot_proto::limits::{IPC_MAX_MESSAGE_BYTES, IPC_PROTOCOL_VERSION, check_ipc_message};
    use canterlot_proto::worker::{
        ipc,
        v1::{CancelInput, WithdrawInput},
    };
    let messages = [
        ipc::control_to_supervisor::Msg::Attach(ipc::Attach {
            protocol_version: IPC_PROTOCOL_VERSION,
            secret: vec![42; 32],
            execution_id: "execution".into(),
            generation: 3,
        }),
        ipc::control_to_supervisor::Msg::StartCli(ipc::StartCli {
            profile: "synthetic".into(),
            workdir: "/tmp/w".into(),
            native_conversation_id: "native".into(),
            ..Default::default()
        }),
        ipc::control_to_supervisor::Msg::Deliver(ipc::Deliver {
            command: Some(deliver_input("hello".into())),
            ..Default::default()
        }),
        ipc::control_to_supervisor::Msg::Cancel(CancelInput {
            target_command_id: "command".into(),
            target_dispatch_attempt: 3,
        }),
        ipc::control_to_supervisor::Msg::Withdraw(WithdrawInput {
            target_command_id: "command".into(),
            target_dispatch_attempt: 3,
        }),
        ipc::control_to_supervisor::Msg::Interrupt(ipc::InterruptRequest {
            command_id: "command".into(),
            dispatch_attempt: 3,
        }),
        ipc::control_to_supervisor::Msg::Stop(ipc::StopRequest {
            command_id: "command".into(),
            dispatch_attempt: 3,
            force: true,
        }),
        ipc::control_to_supervisor::Msg::ShipFrom(ipc::ShipFrom { committed_seq: 7 }),
        ipc::control_to_supervisor::Msg::SjAck(ipc::SjAck { up_to_seq: 8 }),
    ];
    for (index, msg) in messages.into_iter().enumerate() {
        let message = ipc::ControlToSupervisor { msg: Some(msg) };
        assert_eq!(message.encode_to_vec()[0], ((index + 1) << 3 | 2) as u8);
        assert_eq!(check_ipc_message(&message), Ok(()));
        assert_roundtrip(message);
    }
    let messages = [
        ipc::supervisor_to_control::Msg::Attached(ipc::Attached {
            protocol_version: 1,
            pid: 123,
            start_ticks: 456,
            sj_journal_id: "sj".into(),
            last_sj_seq: 8,
            acked_sj_seq: 7,
            received: vec![ipc::ReceivedInput {
                command_id: "command".into(),
                dispatch_attempt: 3,
                state: "received".into(),
            }],
        }),
        ipc::supervisor_to_control::Msg::CliStarted(ipc::CliStarted {
            pid: 124,
            start_ticks: 457,
            native_conversation_id: "native".into(),
        }),
        ipc::supervisor_to_control::Msg::DeliverAck(ipc::DeliverAck {
            command_id: "command".into(),
            dispatch_attempt: 3,
            result: "accepted_for_gate".into(),
        }),
        ipc::supervisor_to_control::Msg::SjBatch(ipc::SjBatch {
            journal_id: "sj".into(),
            records: record_batch("delta".into()).records,
        }),
        ipc::supervisor_to_control::Msg::Exited(ipc::Exited {
            reason: "normal_exit".into(),
        }),
    ];
    for (index, msg) in messages.into_iter().enumerate() {
        let message = ipc::SupervisorToControl { msg: Some(msg) };
        assert_eq!(message.encode_to_vec()[0], ((index + 1) << 3 | 2) as u8);
        assert_eq!(check_ipc_message(&message), Ok(()));
        assert_roundtrip(message);
    }
    let mut deliver = ipc::Deliver {
        command: Some(deliver_input("x".repeat(IPC_MAX_MESSAGE_BYTES - 1000))),
        ..Default::default()
    };
    let padding = IPC_MAX_MESSAGE_BYTES - deliver.encoded_len();
    let Some(command::Kind::DeliverInput(input)) = &mut deliver.command.as_mut().unwrap().kind
    else {
        unreachable!()
    };
    let Some(content_block::Block::Text(text)) = &mut input.content[0].block else {
        unreachable!()
    };
    text.text.push_str(&"x".repeat(padding));
    assert_eq!(deliver.encoded_len(), 1_048_576);
    assert_eq!(check_ipc_message(&deliver), Ok(()));
    assert_roundtrip(deliver.clone());
    let envelope = ipc::ControlToSupervisor {
        msg: Some(ipc::control_to_supervisor::Msg::Deliver(deliver.clone())),
    };
    assert_eq!(
        check_ipc_message(&envelope),
        Err(LimitError::Message {
            actual: envelope.encoded_len()
        })
    );
    let Some(command::Kind::DeliverInput(input)) = &mut deliver.command.as_mut().unwrap().kind
    else {
        unreachable!()
    };
    let Some(content_block::Block::Text(text)) = &mut input.content[0].block else {
        unreachable!()
    };
    text.text.push('x');
    assert_eq!(deliver.encoded_len(), 1_048_577);
    assert_eq!(
        check_ipc_message(&deliver),
        Err(LimitError::Message { actual: 1_048_577 })
    );
}

#[test]
fn block_and_deliver_payloads_are_redacted_in_debug() {
    use canterlot_proto::worker::{ipc, v1::BlockFinal};
    let marker = "PRIVATE_é_PAYLOAD";
    let delta = BlockDelta {
        append: marker.into(),
        ..Default::default()
    };
    let final_block = BlockFinal {
        text: marker.into(),
        ..Default::default()
    };
    let deliver = ipc::Deliver {
        command: Some(deliver_input(marker.into())),
        ..Default::default()
    };
    assert_eq!(
        format!("{delta:?}"),
        format!("BlockDelta {{ append_bytes: {} }}", marker.len())
    );
    assert_eq!(
        format!("{final_block:?}"),
        format!("BlockFinal {{ text_bytes: {} }}", marker.len())
    );
    assert_eq!(
        format!("{deliver:?}"),
        format!(
            "Deliver {{ command_bytes: {}, text_bytes: {} }}",
            deliver.command.as_ref().unwrap().encoded_len(),
            marker.len()
        )
    );
    for debug in [
        format!("{delta:?}"),
        format!("{final_block:?}"),
        format!("{deliver:?}"),
    ] {
        assert!(!debug.contains(marker));
        assert!(debug.contains(&marker.len().to_string()));
        assert!(debug.contains("bytes"));
    }
    for body in [
        record_body::Body::BlockDelta(delta),
        record_body::Body::BlockFinal(final_block),
    ] {
        let batch = JournalRecords {
            journal_id: "sj".into(),
            records: vec![JournalRecord {
                seq: 1,
                body: Some(RecordBody { body: Some(body) }),
            }],
        };
        assert!(!format!("{batch:?}").contains(marker));
        let envelope = ipc::SupervisorToControl {
            msg: Some(ipc::supervisor_to_control::Msg::SjBatch(ipc::SjBatch {
                journal_id: batch.journal_id,
                records: batch.records,
            })),
        };
        assert!(!format!("{envelope:?}").contains(marker));
    }
    let envelope = ipc::ControlToSupervisor {
        msg: Some(ipc::control_to_supervisor::Msg::Deliver(deliver)),
    };
    assert!(!format!("{envelope:?}").contains(marker));
}

#[test]
fn reconcile_report_roundtrip() {
    use canterlot_proto::worker::v1::{
        Accepted, ControlUp, ExecutionObservation, ReconcileReport, control_up,
    };
    let report = ReconcileReport {
        executions: vec![ExecutionObservation {
            execution_id: "execution".into(),
            generation: 3,
            state: "running".into(),
            outcome: "normal_exit".into(),
            supervisor_attached: true,
            sj_journal_id: "sj".into(),
        }],
    };
    assert_roundtrip(report.clone());
    let message = ControlUp {
        msg: Some(control_up::Msg::ReconcileReport(report)),
    };
    assert_eq!(message.encode_to_vec()[0], 0x22);
    assert_roundtrip(message);
    assert_roundtrip(Accepted {
        restore_generation: "generation".into(),
        committed: vec![],
        capabilities: vec!["exec_v1".into()],
    });
    assert!(
        Accepted::decode([].as_slice())
            .unwrap()
            .capabilities
            .is_empty()
    );
}

#[test]
fn ownership_roundtrip() {
    use canterlot_proto::worker::ipc::{Ownership, OwnershipPhase};
    assert_roundtrip(Ownership {
        execution_id: "execution".into(),
        generation: 3,
        conversation_id: "conversation".into(),
        boot_id: "boot".into(),
        exec_dir: "/tmp/execution".into(),
        supervisor_pid: 123,
        supervisor_start_ticks: 456,
        cli_pid: 124,
        cli_start_ticks: 457,
        ipc_socket: "/tmp/execution/ipc.sock".into(),
        secret_sha256: vec![42; 32],
        sj_journal_id: "sj".into(),
        ipc_protocol_version: 1,
        phase: OwnershipPhase::Attached as i32,
        released_reason: "normal_exit".into(),
    });
    for (number, name) in ["UNSPECIFIED", "SPAWNING", "ATTACHED", "RELEASED"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            OwnershipPhase::try_from(number as i32)
                .unwrap()
                .as_str_name(),
            format!("OWNERSHIP_PHASE_{name}")
        );
    }
}

#[test]
fn ipc_authentication_debug_redacts_secret_and_verifier() {
    use canterlot_proto::worker::ipc::{Attach, Ownership};
    let secret = b"PRIVATE_IPC_SECRET".to_vec();
    let attach = Attach {
        secret: secret.clone(),
        ..Default::default()
    };
    let ownership = Ownership {
        secret_sha256: secret.clone(),
        ..Default::default()
    };
    for debug in [format!("{attach:?}"), format!("{ownership:?}")] {
        assert!(!debug.contains("PRIVATE_IPC_SECRET"));
        assert!(!debug.contains(&format!("{secret:?}")));
        assert!(debug.contains(&secret.len().to_string()));
    }
}

#[test]
fn pilot_records_roundtrip() {
    use canterlot_proto::worker::v1::*;
    let observation = LimitObservation {
        provider: "claude".into(),
        window: LimitWindow::FiveHour as i32,
        used_permille: 954,
        resets_at_ms: 123456,
        observed_machine_ms: 123000,
    };
    for (tag, body) in [
        (23, record_body::Body::LimitObserved(observation.clone())),
        (
            24,
            record_body::Body::LimitReached(LimitReached {
                execution_id: "execution".into(),
                provider: "claude".into(),
                window: LimitWindow::Weekly as i32,
                resets_at_ms: 456789,
                input_command_id: "input".into(),
            }),
        ),
        (
            25,
            record_body::Body::AttachmentReady(AttachmentReady {
                command_id: "command".into(),
                dispatch_attempt: 2,
                attachment_id: "attachment".into(),
            }),
        ),
        (
            26,
            record_body::Body::AttachmentFailed(AttachmentFailed {
                command_id: "command".into(),
                dispatch_attempt: 3,
                attachment_id: "attachment".into(),
                reason: "hub_unreachable".into(),
                retryable: true,
            }),
        ),
    ] {
        let record = RecordBody { body: Some(body) };
        assert_eq!(
            &record.encode_to_vec()[..2],
            &[((tag << 3 | 2) & 127) as u8 | 128, 1]
        );
        assert_roundtrip(record);
    }
    let report = MachineReport {
        worker_version: "0.1.0".into(),
        allowed_roots: vec!["/tmp/w".into()],
        harnesses: vec![HarnessInfo {
            harness: "claude_code".into(),
            provider: "claude".into(),
            title: "Клод".into(),
            images: true,
            settings_midsession: true,
            settings_midsession_effort: Some(true),
            permission_modes: vec!["bypass".into()],
            models: vec![ModelInfo {
                id: "model".into(),
                title: "Модель".into(),
                efforts: vec!["high".into()],
                images: Some(true),
                context_window: Some(1_048_576),
            }],
        }],
        max_attachment_bytes: 104_857_600,
        max_attachments_per_input: 10,
        containment: "process_group".into(),
        os: "Debian 13 · 6.12".into(),
        home_dir: "/home/owner".into(),
    };
    let metrics = MachineMetrics {
        machine_time_ms: 123456,
        cpu_cores: 8,
        cpu_permille: 125,
        mem_total_bytes: 16000,
        mem_available_bytes: 8000,
        disks: vec![DiskUsage {
            label: "data".into(),
            total_bytes: 10000,
            free_bytes: 5000,
        }],
        load1_centi: 123,
        load5_centi: 45,
        load15_centi: 67,
        executions: vec![ExecutionProcess {
            execution_id: "execution".into(),
            alive: true,
            cpu_permille: 80,
            rss_bytes: 1000,
        }],
    };
    let limits = LimitsReport {
        observations: vec![observation],
    };
    assert_roundtrip(report.clone());
    assert_roundtrip(metrics.clone());
    assert_roundtrip(limits.clone());
    for (tag, msg) in [
        (5, control_up::Msg::MachineReport(report)),
        (6, control_up::Msg::MachineMetrics(metrics)),
        (7, control_up::Msg::LimitsReport(limits)),
    ] {
        let message = ControlUp { msg: Some(msg) };
        assert_eq!(message.encode_to_vec()[0], tag << 3 | 2);
        assert_roundtrip(message);
    }
    let poll = PollLimits {
        provider: "codex".into(),
    };
    assert_roundtrip(poll.clone());
    let message = ControlDown {
        msg: Some(control_down::Msg::PollLimits(poll)),
    };
    assert_eq!(message.encode_to_vec()[0], 5 << 3 | 2);
    assert_roundtrip(message);
    assert_roundtrip(FetchAttachmentRequest {
        attachment_id: "attachment".into(),
        offset: 131072,
    });
    for (number, name) in [
        (0, "LIMIT_WINDOW_UNSPECIFIED"),
        (1, "LIMIT_WINDOW_FIVE_HOUR"),
        (2, "LIMIT_WINDOW_WEEKLY"),
    ] {
        assert_eq!(LimitWindow::try_from(number).unwrap().as_str_name(), name);
    }
}

#[test]
fn attachment_chunk_and_refs_fit_message_limits() {
    use canterlot_proto::{limits::*, worker::v1::*};
    assert_eq!(MAX_ATTACHMENT_BYTES, 104_857_600);
    assert_eq!(MAX_ATTACHMENTS_PER_INPUT, 10);
    assert_eq!(MAX_ATTACHMENT_NAME_BYTES, 255);
    assert_eq!(MAX_METRICS_EXECUTIONS, 256);
    let chunk = AttachmentChunk {
        offset: 131072,
        data: vec![42; MAX_FILE_CHUNK_BYTES],
        last: true,
    };
    assert!(chunk.encoded_len() <= MAX_MESSAGE_BYTES);
    assert_roundtrip(chunk);
    let mut command = deliver_input("é".repeat(MAX_INPUT_TEXT_BYTES / 2));
    let Some(command::Kind::DeliverInput(input)) = &mut command.kind else {
        unreachable!()
    };
    input
        .content
        .extend((0..MAX_ATTACHMENTS_PER_INPUT).map(|_| ContentBlock {
            block: Some(content_block::Block::Attachment(AttachmentRef {
                attachment_id: "019a1b10-0000-7000-8000-000000000001".into(),
                name: "x".repeat(MAX_ATTACHMENT_NAME_BYTES),
                mime: "application/octet-stream".into(),
                size: MAX_ATTACHMENT_BYTES,
                sha256: "a".repeat(64),
                image: false,
            })),
        }));
    input.settings = Some(SettingsOverride {
        model: "model".into(),
        effort: "high".into(),
        permission_mode: "bypass".into(),
        instructions: None,
    });
    assert_eq!(check_deliver_input(&command), Ok(()));
    assert_roundtrip(command);
}

#[test]
fn pilot_payloads_are_redacted_in_debug() {
    use canterlot_proto::worker::{ipc, v1::*};
    let marker = "PRIVATE_PILOT_MARKER_123";
    let attachment = AttachmentRef {
        attachment_id: marker.into(),
        name: marker.into(),
        mime: marker.into(),
        size: 987654,
        sha256: marker.into(),
        image: true,
    };
    let chunk = AttachmentChunk {
        data: marker.as_bytes().to_vec(),
        ..Default::default()
    };
    let file = ipc::LocalFile {
        path: marker.into(),
        name: marker.into(),
        mime: marker.into(),
        size: 987654,
        image: true,
    };
    for debug in [
        format!("{attachment:?}"),
        format!("{chunk:?}"),
        format!("{file:?}"),
    ] {
        assert!(!debug.contains(marker));
        assert!(debug.contains("987654") || debug.contains(&marker.len().to_string()));
    }
    let deliver = ipc::Deliver {
        command: None,
        files: vec![file],
    };
    assert_roundtrip(deliver.clone());
    assert!(!format!("{deliver:?}").contains(marker));
    for debug in [
        format!(
            "{:?}",
            BlockStarted {
                summary: marker.into(),
                tool_name: marker.into(),
                agent: marker.into(),
                model: marker.into(),
                ..Default::default()
            }
        ),
        format!(
            "{:?}",
            BlockFinal {
                summary: marker.into(),
                text: marker.into(),
                ..Default::default()
            }
        ),
        format!(
            "{:?}",
            PhaseChanged {
                summary: marker.into(),
                tool_name: marker.into(),
                ..Default::default()
            }
        ),
    ] {
        assert!(!debug.contains(marker));
    }
}

#[test]
fn n_minus_one_decoders_skip_new_fields() {
    use canterlot_proto::worker::v1::*;
    #[derive(Clone, PartialEq, Message)]
    struct OldRunFinished {
        #[prost(string, tag = "1")]
        execution_id: String,
        #[prost(string, tag = "2")]
        input_command_id: String,
    }
    #[derive(Clone, PartialEq, Message)]
    struct OldBlockStarted {
        #[prost(string, tag = "1")]
        block_id: String,
        #[prost(uint64, tag = "2")]
        rev: u64,
        #[prost(string, tag = "3")]
        kind: String,
        #[prost(string, tag = "4")]
        input_command_id: String,
        #[prost(string, tag = "5")]
        parent_block_id: String,
    }
    #[derive(Clone, PartialEq, Message)]
    struct OldStartExecution {
        #[prost(string, tag = "1")]
        conversation_id: String,
        #[prost(string, tag = "2")]
        harness: String,
        #[prost(string, tag = "3")]
        workdir: String,
        #[prost(string, tag = "4")]
        native_conversation_id: String,
    }
    let old = OldRunFinished {
        execution_id: "execution".into(),
        input_command_id: "input".into(),
    };
    let new = RunFinished {
        adopted: false,
        execution_id: old.execution_id.clone(),
        input_command_id: old.input_command_id.clone(),
        usage: vec![ModelUsage {
            model: "model".into(),
            agent: "main".into(),
            input_tokens: 100,
            output_tokens: 30,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
        }],
        context_tokens: 1000,
        context_window_tokens: 200000,
        duration_ms: 1234,
        machine_time_ms: 123456,
    };
    assert_eq!(
        OldRunFinished::decode(new.encode_to_vec().as_slice()).unwrap(),
        old
    );
    assert_roundtrip(new);
    let old = OldBlockStarted {
        block_id: "block".into(),
        rev: 2,
        kind: "tool".into(),
        input_command_id: "input".into(),
        parent_block_id: "parent".into(),
    };
    let new = BlockStarted {
        block_id: old.block_id.clone(),
        rev: old.rev,
        kind: old.kind.clone(),
        input_command_id: old.input_command_id.clone(),
        parent_block_id: old.parent_block_id.clone(),
        agent: "main".into(),
        model: "model".into(),
        tool_name: "shell".into(),
        summary: "Проверка".into(),
        approved_by: String::new(),
    };
    assert_eq!(
        OldBlockStarted::decode(new.encode_to_vec().as_slice()).unwrap(),
        old
    );
    assert_roundtrip(new);
    let old = OldStartExecution {
        conversation_id: "conversation".into(),
        harness: "codex".into(),
        workdir: "/tmp/w".into(),
        native_conversation_id: "native".into(),
    };
    let new = StartExecution {
        conversation_id: old.conversation_id.clone(),
        harness: old.harness.clone(),
        workdir: old.workdir.clone(),
        native_conversation_id: old.native_conversation_id.clone(),
        model: "model".into(),
        effort: "high".into(),
        permission_mode: "bypass".into(),
        instructions: None,
        capabilities: vec!["pilot_v1".into()],
    };
    assert_eq!(
        OldStartExecution::decode(new.encode_to_vec().as_slice()).unwrap(),
        old
    );
    assert_roundtrip(new);
}

#[test]
fn provider_table_maps_harnesses() {
    use canterlot_proto::harness::*;
    assert_eq!(provider_of("synthetic-codex"), Some("codex"));
    assert_eq!(provider_of("unknown"), None);
    assert_eq!(
        harnesses_of("claude"),
        ["claude_code", "synthetic", "synthetic-exit-on-interrupt"]
    );
    assert_eq!(harnesses_of("codex"), ["codex", "synthetic-codex"]);
    assert_eq!(provider_of("opencode"), Some("opencode"));
    assert_eq!(harnesses_of("opencode"), ["opencode", "synthetic-opencode"]);
    assert!(harnesses_of("unknown").is_empty());
    for &(harness, provider) in HARNESS_PROVIDERS {
        assert_eq!(provider_of(harness), Some(provider));
    }
}

#[test]
fn start_execution_capabilities_machine_report_os_and_limits_snapshot_roundtrip() {
    use canterlot_proto::worker::{ipc, v1::*};
    let start = StartExecution {
        capabilities: vec!["pilot_v1".into()],
        ..Default::default()
    };
    assert_eq!(start.encode_to_vec(), b"\x4a\x08pilot_v1");
    assert_roundtrip(start);
    let report = MachineReport {
        os: "Debian 13 · 6.12".into(),
        ..Default::default()
    };
    assert_eq!(report.encode_to_vec()[0], 8 << 3 | 2);
    assert_roundtrip(report);
    let report = MachineReport {
        home_dir: "/home/owner".into(),
        ..Default::default()
    };
    assert_eq!(report.encode_to_vec()[0], 9 << 3 | 2);
    assert_roundtrip(report);
    let suggest = SuggestWorkdirs {
        request_id: "request".into(),
        path: "/home/owner/private-name".into(),
    };
    assert_roundtrip(suggest.clone());
    assert!(!format!("{suggest:?}").contains("private-name"));
    let message = ControlDown {
        msg: Some(control_down::Msg::SuggestWorkdirs(suggest)),
    };
    assert_eq!(message.encode_to_vec()[0], 7 << 3 | 2);
    assert_roundtrip(message);
    let suggestions = WorkdirSuggestions {
        request_id: "request".into(),
        directories: vec!["/home/owner/private-name".into()],
    };
    assert_roundtrip(suggestions.clone());
    assert!(!format!("{suggestions:?}").contains("private-name"));
    let message = ControlUp {
        msg: Some(control_up::Msg::WorkdirSuggestions(suggestions)),
    };
    assert_eq!(message.encode_to_vec()[0], 8 << 3 | 2);
    assert_roundtrip(message);
    let snapshot = LimitsSnapshot {
        provider: "claude".into(),
        current: vec![LimitObservation {
            provider: "claude".into(),
            window: LimitWindow::Weekly as i32,
            used_permille: 954,
            resets_at_ms: 123456,
            observed_machine_ms: 123000,
        }],
        weekly_reserve_active: true,
    };
    assert_roundtrip(snapshot.clone());
    let message = ControlDown {
        msg: Some(control_down::Msg::LimitsSnapshot(snapshot)),
    };
    assert_eq!(message.encode_to_vec()[0], 6 << 3 | 2);
    assert_roundtrip(message);
    assert_roundtrip(ipc::StartCli {
        profile: "synthetic".into(),
        workdir: "/tmp/w".into(),
        native_conversation_id: "native".into(),
        model: "model".into(),
        effort: "high".into(),
        permission_mode: "bypass".into(),
        instructions: None,
        pilot_v1: true,
        todo_v1: true,
        compact_v1: true,
    });
    assert_roundtrip(ipc::CompactRequest {
        command_id: "command".into(),
        dispatch_attempt: 2,
    });
    assert_roundtrip(PhaseChanged {
        execution_id: "execution".into(),
        phase: "executing_tool".into(),
        tool_name: "shell".into(),
        summary: "Проверка".into(),
    });
    assert_roundtrip(BlockFinal {
        summary: "Готово".into(),
        ..Default::default()
    });
}

#[test]
fn hello_and_heartbeat_carry_machine_time_and_old_decoders_skip_it() {
    use canterlot_proto::worker::v1::Heartbeat;
    #[derive(Clone, PartialEq, Message)]
    struct OldHello {
        #[prost(string, tag = "1")]
        installation_id: String,
        #[prost(string, tag = "2")]
        machine_id: String,
        #[prost(string, tag = "3")]
        restore_generation: String,
        #[prost(uint32, tag = "4")]
        protocol_version: u32,
        #[prost(string, repeated, tag = "5")]
        required_capabilities: Vec<String>,
        #[prost(string, repeated, tag = "6")]
        optional_capabilities: Vec<String>,
        #[prost(string, tag = "7")]
        connection_id: String,
        #[prost(string, tag = "8")]
        cj_journal_id: String,
        #[prost(message, repeated, tag = "9")]
        cursors: Vec<JournalCursor>,
        #[prost(string, tag = "10")]
        boot_id: String,
        #[prost(enumeration = "StreamKind", tag = "11")]
        stream_kind: i32,
    }
    #[derive(Clone, PartialEq, Message)]
    struct OldHeartbeat {
        #[prost(uint64, tag = "1")]
        seq: u64,
    }
    for stream_kind in [StreamKind::Control, StreamKind::Data] {
        let old = OldHello {
            installation_id: "installation".into(),
            machine_id: "machine".into(),
            restore_generation: "restore".into(),
            protocol_version: 1,
            required_capabilities: vec!["journal_v1".into()],
            optional_capabilities: vec!["pilot_v1".into()],
            connection_id: "connection".into(),
            cj_journal_id: "cj".into(),
            cursors: vec![JournalCursor {
                journal_id: "sj".into(),
                first_stored_seq: 1,
                last_seq: 3,
                acked_seq_known_to_worker: 2,
            }],
            boot_id: "boot".into(),
            stream_kind: stream_kind as i32,
        };
        let mut bytes = old.encode_to_vec();
        bytes.extend([0x60, 0x96, 0x01]);
        let hello = Hello::decode(bytes.as_slice()).unwrap();
        assert_eq!(hello.machine_time_ms, 150);
        assert_eq!(
            OldHello::decode(hello.encode_to_vec().as_slice()).unwrap(),
            old
        );
        assert_roundtrip(hello);
    }
    let heartbeat = Heartbeat {
        seq: 9,
        machine_time_ms: 150,
    };
    assert_eq!(heartbeat.encode_to_vec(), [8, 9, 16, 150, 1]);
    assert_eq!(
        OldHeartbeat::decode(heartbeat.encode_to_vec().as_slice()).unwrap(),
        OldHeartbeat { seq: 9 }
    );
    assert_eq!(
        Heartbeat::decode([8, 9].as_slice())
            .unwrap()
            .machine_time_ms,
        0
    );
    assert_roundtrip(heartbeat);
}

#[test]
fn media_messages_roundtrip_fit_limits_and_redact_bytes() {
    use canterlot_proto::{
        limits::{MAX_MEDIA_BYTES, MEDIA_MIMES, check_ipc_message},
        worker::{ipc, v1::*},
    };
    assert_eq!(MAX_MEDIA_BYTES, 20 * 1024 * 1024);
    assert_eq!(
        MEDIA_MIMES,
        ["image/png", "image/jpeg", "image/webp", "image/gif"]
    );
    let marker = b"PRIVATE_MEDIA_MARKER_123";
    let header = MediaChunk {
        part: Some(media_chunk::Part::Header(MediaHeader {
            media_id: "019a1b10-0000-7000-8000-000000000001".into(),
            execution_id: "019a1b10-0000-7000-8000-000000000002".into(),
            block_id: "019a1b10-0000-7000-8000-000000000003".into(),
            mime: "image/png".into(),
            size: MAX_MEDIA_BYTES,
            sha256: "a".repeat(64),
        })),
    };
    let data = MediaChunk {
        part: Some(media_chunk::Part::Data(vec![7; MAX_FILE_CHUNK_BYTES])),
    };
    assert!(data.encoded_len() <= MAX_MESSAGE_BYTES);
    assert_roundtrip(header.clone());
    assert_roundtrip(data);
    assert_roundtrip(MediaStored {
        media_id: "m".into(),
    });
    let secret = MediaChunk {
        part: Some(media_chunk::Part::Data(marker.to_vec())),
    };
    let debug = format!("{secret:?}");
    assert!(!debug.contains("PRIVATE_MEDIA_MARKER"), "{debug}");
    assert!(debug.contains(&marker.len().to_string()));
    assert!(format!("{header:?}").contains("20971520"));
    let record = RecordBody {
        body: Some(record_body::Body::BlockMedia(BlockMedia {
            block_id: "b".into(),
            media_id: "m".into(),
            mime: "image/webp".into(),
            size: 42,
            sha256: "b".repeat(64),
            width: 640,
            height: 480,
        })),
    };
    assert_roundtrip(record.clone());
    // Декодер без поля 27 видит пустое тело, а не другое событие.
    #[derive(Clone, PartialEq, Message)]
    struct OldRecordBody {
        #[prost(message, optional, tag = "1")]
        command_stored: Option<canterlot_proto::worker::v1::CommandStored>,
    }
    assert_eq!(
        OldRecordBody::decode(record.encode_to_vec().as_slice()).unwrap(),
        OldRecordBody::default()
    );
    let upload = ipc::SupervisorToControl {
        msg: Some(ipc::supervisor_to_control::Msg::MediaUpload(
            ipc::MediaUpload {
                media_id: "m".into(),
                block_id: "b".into(),
                mime: "image/gif".into(),
                size: 9,
                sha256: "c".repeat(64),
            },
        )),
    };
    let result = ipc::ControlToSupervisor {
        msg: Some(ipc::control_to_supervisor::Msg::MediaResult(
            ipc::MediaResult {
                media_id: "m".into(),
                stored: false,
                reason: "conflict".into(),
            },
        )),
    };
    assert_eq!(check_ipc_message(&upload), Ok(()));
    assert_eq!(check_ipc_message(&result), Ok(()));
    assert_roundtrip(upload);
    assert_roundtrip(result);
}

#[test]
fn instructions_and_agent_call_messages_are_stable() {
    use canterlot_proto::{
        limits::{MAX_AGENT_CALL_BYTES, MAX_PRESET_TEXT_BYTES},
        worker::{ipc, v1::*},
    };
    assert_eq!(MAX_PRESET_TEXT_BYTES, 65_536);
    assert_eq!(MAX_AGENT_CALL_BYTES, 262_144);
    // Instructions: mode=1, text=2, preset_id=3, revision=4, title=5.
    let instructions = Instructions {
        mode: "preset".into(),
        text: "текст".into(),
        preset_id: "id".into(),
        revision: 2,
        title: "T".into(),
    };
    let bytes = instructions.encode_to_vec();
    assert_eq!(&bytes[..7], &[0x0a, 6, b'p', b'r', b'e', b's', b'e']);
    assert!(bytes.windows(2).any(|w| w == [0x2a, 1]));
    assert_roundtrip(instructions.clone());
    // StartExecution.instructions=10, SettingsOverride.instructions=4.
    let start = StartExecution {
        instructions: Some(instructions.clone()),
        ..Default::default()
    };
    assert!(start.encode_to_vec().contains(&0x52));
    assert_roundtrip(start);
    let settings = SettingsOverride {
        instructions: Some(instructions),
        ..Default::default()
    };
    assert!(settings.encode_to_vec().contains(&0x22));
    assert_roundtrip(settings);
    // AgentCallRequest 1..=5, AgentCallResponse 1..=3.
    let request = AgentCallRequest {
        execution_id: "execution".into(),
        generation: 3,
        call_id: "call".into(),
        tool: "canterlot_presets_list".into(),
        arguments_json: "{}".into(),
    };
    let bytes = request.encode_to_vec();
    assert_eq!(&bytes[..11], b"\x0a\x09execution");
    assert!(bytes.windows(2).any(|w| w == [0x1a, 4]));
    assert_roundtrip(request);
    assert_roundtrip(AgentCallResponse {
        result_json: "{}".into(),
        error_code: "hub_timeout".into(),
        error_message: "превышено ожидание".into(),
    });
    // ipc: StartCli.instructions=9, AgentCall 1..=3, AgentCallResult 1..=4.
    let start_cli = ipc::StartCli {
        instructions: Some(Instructions {
            mode: "clean".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(start_cli.encode_to_vec().contains(&0x4a));
    assert_roundtrip(start_cli);
    assert_roundtrip(ipc::AgentCall {
        call_id: "call".into(),
        tool: "t".into(),
        arguments_json: "{}".into(),
    });
    assert_roundtrip(ipc::AgentCallResult {
        call_id: "call".into(),
        result_json: "{}".into(),
        error_code: "hub_timeout".into(),
        error_message: String::new(),
    });
    // SupervisorToControl.agent_call=9, ControlToSupervisor.agent_call_result=12.
    let up = ipc::SupervisorToControl {
        msg: Some(ipc::supervisor_to_control::Msg::AgentCall(ipc::AgentCall {
            call_id: "call".into(),
            ..Default::default()
        })),
    };
    assert_eq!(up.encode_to_vec()[0], 0x4a);
    assert_roundtrip(up);
    let down = ipc::ControlToSupervisor {
        msg: Some(ipc::control_to_supervisor::Msg::AgentCallResult(
            ipc::AgentCallResult::default(),
        )),
    };
    assert_eq!(down.encode_to_vec()[0], 0x62);
    assert_roundtrip(down);
}

#[test]
fn todo_updated_roundtrips_as_record_34_and_debug_hides_the_text() {
    use canterlot_proto::worker::v1::{RecordBody, TodoItem, TodoStatus, TodoUpdated, record_body};
    let marker = "PRIVATE_TODO_MARKER";
    let fact = TodoUpdated {
        execution_id: "e".into(),
        generation: 3,
        revision: 41,
        lane_block_id: "b".into(),
        items: vec![TodoItem {
            text: marker.into(),
            status: TodoStatus::InProgress as i32,
        }],
        omitted: 7,
    };
    let body = RecordBody {
        body: Some(record_body::Body::TodoUpdated(fact.clone())),
    };
    let encoded = body.encode_to_vec();
    // Поле 34 с длиной: варинт тега занимает два байта.
    assert_eq!(&encoded[..2], &[0x92, 0x02]);
    assert_eq!(RecordBody::decode(encoded.as_slice()).unwrap(), body);
    for debug in [
        format!("{fact:?}"),
        format!("{body:?}"),
        format!("{:?}", fact.items[0]),
    ] {
        assert!(!debug.contains(marker), "{debug}");
    }
    assert!(format!("{fact:?}").contains("items: 1"));
}

#[test]
fn compaction_records_keep_their_numbers() {
    use canterlot_proto::worker::v1::{
        CompactSession, CompactionState, CompactionStatus, SessionStuck, TodoStatus, command,
    };
    assert_roundtrip(SessionStuck {
        execution_id: "execution".into(),
        generation: 3,
    });
    assert_roundtrip(CompactionStatus {
        command_id: "command".into(),
        dispatch_attempt: 1,
        state: CompactionState::SentToAgent as i32,
    });
    for (number, name) in [
        "UNSPECIFIED",
        "SENT_TO_AGENT",
        "COMPLETED",
        "FAILED",
        "NOT_APPLICABLE",
        "SUPERSEDED",
        "REJECTED_STALE",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            CompactionState::try_from(number as i32)
                .unwrap()
                .as_str_name(),
            format!("COMPACTION_STATE_{name}")
        );
    }
    assert_eq!(TodoStatus::Cancelled as i32, 4);
    let debug = format!("{:?}", command::Kind::CompactSession(CompactSession {}));
    assert!(debug.starts_with("CompactSession"));
}

#[test]
fn adapter_diagnostic_field_names_are_additive() {
    use canterlot_proto::worker::v1::AdapterDiagnostic;
    let old = AdapterDiagnostic {
        execution_id: "execution".into(),
        kind: "unknown_optional_event".into(),
        event_type: "future".into(),
        ..Default::default()
    };
    let old_wire = old.encode_to_vec();
    let decoded = AdapterDiagnostic::decode(old_wire.as_slice()).unwrap();
    assert!(decoded.field_names.is_empty());
    let new = AdapterDiagnostic {
        field_names: vec!["alpha".into(), "beta".into()],
        ..old.clone()
    };
    assert_roundtrip(new.clone());
    // Новое поле четвёртое: старый разбор пропускает его, а старые записи читаются без него.
    assert_eq!(&new.encode_to_vec()[..old_wire.len()], old_wire.as_slice());
}
