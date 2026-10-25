use canterlot_proto::limits::{
    LimitError, MAX_EVENT_BATCH_BYTES, MAX_FILE_CHUNK_BYTES, MAX_INPUT_META_BYTES,
    MAX_INPUT_TEXT_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, check_deliver_input,
    check_record_batch,
};
use canterlot_proto::worker::v1::{
    BlockDelta, Command, ContentBlock, DeliverInput, Hello, JournalCursor, JournalRecord,
    JournalRecords, RecordBody, StartExecution, StreamKind, TextBlock, command, content_block,
    record_body,
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
