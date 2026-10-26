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
        }),
        ipc::control_to_supervisor::Msg::Deliver(ipc::Deliver {
            command: Some(deliver_input("hello".into())),
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
