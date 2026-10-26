use sha2::Digest as _;
use std::time::Duration;

use canterlot_hub::{
    config::HubTimers,
    delivery::{
        dispatch::{GateInputs, dispatch_next},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions,
    machines::{self, NewMachine},
    model::ExecutionState,
    session::Availability,
    testkit::{self, TestHub, spawn_test_hub},
};
use canterlot_proto::{limits::MAX_MESSAGE_BYTES, worker::v1::*};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request, Streaming,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

fn timers() -> HubTimers {
    HubTimers {
        command_stored_receipt: Duration::from_millis(250),
        dispatch_sweep: Duration::from_millis(25),
        ..HubTimers::default()
    }
}

struct Worker {
    control: mpsc::Sender<ControlUp>,
    down: Streaming<ControlDown>,
    data: mpsc::Sender<DataUp>,
    data_down: Streaming<DataDown>,
    journal: Uuid,
    seq: u64,
}

async fn channel(hub: &TestHub) -> Channel {
    Endpoint::from_shared(format!("https://{}", hub.addr))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&hub.pki.ca_pem))
                .domain_name("localhost"),
        )
        .unwrap()
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .connect()
        .await
        .unwrap()
}

fn request<T>(value: T, machine: &NewMachine) -> Request<T> {
    let mut request = Request::new(value);
    let mut token = format!("Bearer {}", machine.token)
        .parse::<tonic::metadata::MetadataValue<_>>()
        .unwrap();
    token.set_sensitive(true);
    request.metadata_mut().insert("authorization", token);
    request
}

impl Worker {
    async fn connect(hub: &TestHub, machine: &NewMachine) -> Self {
        Self::connect_with_journal(hub, machine, Uuid::now_v7()).await
    }

    async fn connect_with_journal(hub: &TestHub, machine: &NewMachine, journal: Uuid) -> Self {
        let hello = Hello {
            installation_id: hub.installation.installation_id.to_string(),
            restore_generation: hub.installation.restore_generation.to_string(),
            machine_id: machine.machine_id.to_string(),
            connection_id: Uuid::now_v7().to_string(),
            cj_journal_id: journal.to_string(),
            boot_id: Uuid::now_v7().to_string(),
            protocol_version: canterlot_hub::grpc::PROTOCOL_VERSION,
            stream_kind: StreamKind::Control as i32,
            ..Hello::default()
        };
        let (control, rx) = mpsc::channel(8);
        control
            .send(ControlUp {
                msg: Some(control_up::Msg::Hello(hello.clone())),
            })
            .await
            .unwrap();
        let mut client = worker_control_client::WorkerControlClient::new(channel(hub).await)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut down = client
            .control(request(ReceiverStream::new(rx), machine))
            .await
            .unwrap()
            .into_inner();
        let reply = down.message().await.unwrap().unwrap();
        assert!(matches!(
            reply.msg,
            Some(control_down::Msg::HelloReply(HelloReply {
                result: Some(hello_reply::Result::Accepted(_))
            }))
        ));
        let (data, rx) = mpsc::channel(8);
        data.send(DataUp {
            msg: Some(data_up::Msg::Hello(Hello {
                stream_kind: StreamKind::Data as i32,
                ..hello
            })),
        })
        .await
        .unwrap();
        let mut client = worker_data_client::WorkerDataClient::new(channel(hub).await)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut data_down = client
            .data(request(ReceiverStream::new(rx), machine))
            .await
            .unwrap()
            .into_inner();
        let reply = data_down.message().await.unwrap().unwrap();
        assert!(matches!(
            reply.msg,
            Some(data_down::Msg::HelloReply(HelloReply {
                result: Some(hello_reply::Result::Accepted(_))
            }))
        ));
        Self {
            control,
            down,
            data,
            data_down,
            journal,
            seq: 0,
        }
    }

    async fn command(&mut self) -> Command {
        let message = timeout(Duration::from_secs(3), self.down.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Some(control_down::Msg::Command(command)) = message.msg else {
            panic!("expected command")
        };
        command
    }

    async fn stored(&mut self, command: &Command) {
        self.seq += 1;
        self.control
            .send(ControlUp {
                msg: Some(control_up::Msg::CjRecords(JournalRecords {
                    journal_id: self.journal.to_string(),
                    records: vec![JournalRecord {
                        seq: self.seq,
                        body: Some(RecordBody {
                            body: Some(record_body::Body::CommandStored(CommandStored {
                                machine_time_ms: 0,
                                command_id: command.command_id.clone(),
                                dispatch_attempt: command.dispatch_attempt,
                            })),
                        }),
                    }],
                })),
            })
            .await
            .unwrap();
        let ack = timeout(Duration::from_secs(3), self.down.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(ack.msg, Some(control_down::Msg::CjAck(JournalRangeAck { up_to_seq, .. })) if up_to_seq == self.seq)
        );
    }

    async fn finish(&mut self, command: &Command) {
        let bodies = [
            record_body::Body::InputAccepted(InputAccepted {
                machine_time_ms: 0,
                command_id: command.command_id.clone(),
                dispatch_attempt: command.dispatch_attempt,
            }),
            record_body::Body::RunFinished(RunFinished {
                execution_id: command.execution_id.clone(),
                input_command_id: command.command_id.clone(),
            }),
        ];
        self.data
            .send(DataUp {
                msg: Some(data_up::Msg::SjRecords(JournalRecords {
                    journal_id: Uuid::now_v7().to_string(),
                    records: bodies
                        .into_iter()
                        .enumerate()
                        .map(|(index, body)| JournalRecord {
                            seq: index as u64 + 1,
                            body: Some(RecordBody { body: Some(body) }),
                        })
                        .collect(),
                })),
            })
            .await
            .unwrap();
        let ack = timeout(Duration::from_secs(3), self.data_down.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            ack.msg,
            Some(data_down::Msg::SjAck(JournalRangeAck { up_to_seq: 2, .. }))
        ));
    }
}

async fn setup(pool: &PgPool, hub: &TestHub) -> (NewMachine, Uuid) {
    let machine = machines::add(pool, hub.installation.owner_id, "synthetic")
        .await
        .unwrap();
    let conversation =
        testkit::seed_conversation(pool, hub.installation.owner_id, machine.machine_id).await;
    let execution = executions::start(
        pool,
        hub.installation.owner_id,
        conversation,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = now() where id = $1",
    )
    .bind(execution.command_id)
    .execute(pool)
    .await
    .unwrap();
    (machine, conversation)
}

async fn input(pool: &PgPool, hub: &TestHub, conversation_id: Uuid) -> SubmittedInput {
    submit_input(
        pool,
        SubmitInput {
            owner_id: hub.installation.owner_id,
            conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "synthetic input".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
}

async fn state(pool: &PgPool, command: Uuid) -> (String, String, bool, bool) {
    sqlx::query_as("select c.delivery_state, i.state, c.stored_at is not null, c.completed_at is not null from commands c join inputs i on i.command_id = c.id where c.id = $1")
        .bind(command).fetch_one(pool).await.unwrap()
}

async fn stop(hub: TestHub) {
    hub.shutdown.send(()).unwrap();
    timeout(Duration::from_secs(3), hub.join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queued_input_reaches_live_worker_and_becomes_on_machine(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    assert_eq!(
        hub.registry.availability(machine.machine_id),
        Availability::Available
    );
    let submitted = input(&pool, &hub, conversation).await;
    let command = worker.command().await;
    assert_eq!(command.command_id, submitted.command_id.to_string());
    assert_eq!(command.dispatch_attempt, 1);
    assert!(
        matches!(command.kind, Some(command::Kind::DeliverInput(ref value)) if value.input_id == submitted.input_id.to_string())
    );
    assert_eq!(
        state(&pool, submitted.command_id).await,
        ("pending".into(), "dispatched".into(), false, false)
    );
    for _ in 0..3 {
        sqlx::query("select pg_notify('canterlot_outbox', $1)")
            .bind(machine.machine_id.to_string())
            .execute(&pool)
            .await
            .unwrap();
    }
    worker.stored(&command).await;
    assert_eq!(
        state(&pool, submitted.command_id).await,
        ("stored_on_machine".into(), "on_machine".into(), true, false)
    );
    assert!(
        timeout(Duration::from_millis(600), worker.down.message())
            .await
            .is_err()
    );
    stop(hub).await;
    assert!(worker.down.message().await.unwrap().is_none());
    assert!(worker.data_down.message().await.unwrap().is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nothing_is_sent_to_unavailable_machine(pool: PgPool) {
    let hub = spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_secs(60),
            ..timers()
        },
    )
    .await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let submitted = input(&pool, &hub, conversation).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        hub.registry.availability(machine.machine_id),
        Availability::Unavailable
    );
    assert_eq!(
        state(&pool, submitted.command_id).await,
        ("held".into(), "queued".into(), false, false)
    );
    let mut worker = Worker::connect(&hub, &machine).await;
    let command = worker.command().await;
    assert_eq!(command.command_id, submitted.command_id.to_string());
    worker.stored(&command).await;
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn submit_and_committed_receipts_trigger_without_waiting_for_sweep(pool: PgPool) {
    let hub = spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_secs(60),
            ..timers()
        },
    )
    .await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    let submit = || SubmitInput {
        owner_id: hub.installation.owner_id,
        conversation_id: conversation,
        idempotency_key: Uuid::now_v7().to_string(),
        content: vec![ContentBlock::Text {
            text: "synthetic input".into(),
        }],
        received_at: Utc::now(),
        correlation_id: Uuid::now_v7(),
    };
    let first = hub.dispatcher.submit_input(&pool, submit()).await.unwrap();
    let command = worker.command().await;
    assert_eq!(command.command_id, first.command_id.to_string());
    worker.stored(&command).await;
    let second = hub.dispatcher.submit_input(&pool, submit()).await.unwrap();
    assert!(
        timeout(Duration::from_millis(50), worker.down.message())
            .await
            .is_err()
    );
    worker.finish(&command).await;
    let next = worker.command().await;
    assert_eq!(next.command_id, second.command_id.to_string());
    assert_eq!(state(&pool, first.command_id).await.1, "accepted");
    worker.stored(&next).await;
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pending_commands_are_sent_in_created_at_order(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let start: Uuid = sqlx::query_scalar("update commands set delivery_state = 'pending', completed_at = null, created_at = now() - interval '2 hours' where conversation_id = $1 returning id")
        .bind(conversation).fetch_one(&pool).await.unwrap();
    let interrupt = Uuid::now_v7();
    sqlx::query("insert into commands (id, machine_id, conversation_id, execution_id, execution_generation, kind, payload, delivery_state, created_at) select $1, machine_id, conversation_id, execution_id, execution_generation, 'interrupt', '{}', 'pending', now() - interval '1 hour' from commands where id = $2")
        .bind(interrupt).bind(start).execute(&pool).await.unwrap();
    let submitted = input(&pool, &hub, conversation).await;
    dispatch_next(
        &pool,
        conversation,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let mut worker = Worker::connect(&hub, &machine).await;
    let first = worker.command().await;
    let second = worker.command().await;
    let third = worker.command().await;
    assert_eq!(
        [
            first.command_id.as_str(),
            second.command_id.as_str(),
            third.command_id.as_str()
        ],
        [
            start.to_string(),
            interrupt.to_string(),
            submitted.command_id.to_string()
        ]
    );
    for command in [&first, &second, &third] {
        worker.stored(command).await;
    }
    stop(hub).await;
}

fn command_row(kind: &str, payload: Value) -> canterlot_hub::outbox::CommandRow {
    canterlot_hub::outbox::CommandRow {
        id: Uuid::now_v7(),
        machine_id: Uuid::now_v7(),
        conversation_id: Some(Uuid::now_v7()),
        execution_id: Some(Uuid::now_v7()),
        execution_generation: Some(1),
        kind: kind.into(),
        payload,
        dispatch_attempt: 1,
    }
}

#[test]
fn to_proto_maps_all_command_variants_and_rejects_unknown_kinds() {
    use canterlot_hub::outbox::{OutboxError, to_proto};
    let target = Uuid::now_v7();
    let cases = [
        (
            "deliver_input",
            json!({"input_id": Uuid::now_v7(), "conversation_id": Uuid::now_v7(), "content": [{"type":"text", "text":"synthetic"}]}),
        ),
        (
            "start_execution",
            json!({"harness":"synthetic", "workdir":"/tmp/w", "native_conversation_id":"native"}),
        ),
        (
            "cancel_input",
            json!({"target_command_id":target, "target_dispatch_attempt":7}),
        ),
        (
            "withdraw_input",
            json!({"target_command_id":target, "target_dispatch_attempt":7}),
        ),
        ("interrupt", json!({})),
        ("stop_execution", json!({"force":true})),
    ];
    for (kind, payload) in cases {
        let row = command_row(kind, payload);
        let command = to_proto(&row).unwrap();
        assert_eq!(command.command_id, row.id.to_string());
        assert_eq!(command.execution_id, row.execution_id.unwrap().to_string());
        assert_eq!(command.execution_generation, 1);
        assert_eq!(command.dispatch_attempt, 1);
        match command.kind.unwrap() {
            command::Kind::DeliverInput(value) => assert_eq!(value.content.len(), 1),
            command::Kind::StartExecution(value) => {
                assert_eq!(
                    value.conversation_id,
                    row.conversation_id.unwrap().to_string()
                );
                assert_eq!(value.native_conversation_id, "native");
                assert_eq!(value.harness, "synthetic");
                assert_eq!(value.workdir, "/tmp/w");
            }
            command::Kind::CancelInput(value) => {
                assert_eq!(value.target_command_id, target.to_string());
                assert_eq!(value.target_dispatch_attempt, 7);
            }
            command::Kind::WithdrawInput(value) => {
                assert_eq!(value.target_command_id, target.to_string());
                assert_eq!(value.target_dispatch_attempt, 7);
            }
            command::Kind::Interrupt(_) => assert_eq!(kind, "interrupt"),
            command::Kind::StopExecution(value) => assert!(value.force),
        }
    }
    assert!(matches!(
        to_proto(&command_row("unknown", json!({}))),
        Err(OutboxError::InvalidCommand)
    ));
}

#[test]
fn to_proto_checks_utf8_metadata_and_control_envelope_limits() {
    use canterlot_hub::outbox::{OutboxError, to_proto};
    use prost::Message;
    let row = |content| {
        command_row(
            "deliver_input",
            json!({
                "input_id":Uuid::now_v7(), "conversation_id":Uuid::now_v7(), "content":content,
            }),
        )
    };
    assert!(to_proto(&row(json!([{"type":"text", "text":"я".repeat(131_072)}]))).is_ok());
    assert!(matches!(
        to_proto(&row(json!([{"type":"text", "text":"я".repeat(131_073)}]))),
        Err(OutboxError::ResourceLimit)
    ));
    assert!(matches!(
        to_proto(&row(json!(vec![json!({"type":"text", "text":""}); 20_000]))),
        Err(OutboxError::ResourceLimit)
    ));
    let mut start = command_row(
        "start_execution",
        json!({"harness":"synthetic", "workdir":"/tmp/w"}),
    );
    let mut command = to_proto(&start).unwrap();
    let Some(command::Kind::StartExecution(payload)) = command.kind.as_mut() else {
        unreachable!()
    };
    payload.native_conversation_id = "x".repeat(MAX_MESSAGE_BYTES - 256);
    let mut padding = MAX_MESSAGE_BYTES - command.encoded_len();
    if let Some(command::Kind::StartExecution(payload)) = command.kind.as_mut() {
        payload
            .native_conversation_id
            .push_str(&"x".repeat(padding));
    }
    padding = command.encoded_len();
    assert_eq!(padding, MAX_MESSAGE_BYTES);
    if let Some(command::Kind::StartExecution(payload)) = command.kind {
        start.payload["native_conversation_id"] = json!(payload.native_conversation_id);
    }
    assert!(matches!(to_proto(&start), Err(OutboxError::ResourceLimit)));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn missing_receipt_resends_once_then_restarts_session(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    let submitted = input(&pool, &hub, conversation).await;
    let first = worker.command().await;
    assert!(
        timeout(Duration::from_millis(75), worker.down.message())
            .await
            .is_err()
    );
    let second = worker.command().await;
    assert_eq!(first, second);
    assert!(
        timeout(Duration::from_millis(75), worker.down.message())
            .await
            .is_err()
    );
    let ended = timeout(Duration::from_secs(3), worker.down.message())
        .await
        .unwrap()
        .unwrap();
    assert!(ended.is_none(), "a third command must never be sent");
    assert!(
        timeout(Duration::from_secs(1), worker.data_down.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert_eq!(
        hub.registry.availability(machine.machine_id),
        Availability::Unavailable
    );
    assert_eq!(
        state(&pool, submitted.command_id).await,
        ("pending".into(), "dispatched".into(), false, false)
    );
    let mut reconnected = Worker::connect_with_journal(&hub, &machine, worker.journal).await;
    let replay = reconnected.command().await;
    assert_eq!(replay, first);
    reconnected.stored(&replay).await;
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn oversize_command_is_rejected_not_sent(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let submitted = input(&pool, &hub, conversation).await;
    sqlx::query("update commands set payload = jsonb_set(payload, '{content}', $2) where id = $1")
        .bind(submitted.command_id)
        .bind(json!([{"type":"text", "text":"x".repeat(262_145)}]))
        .execute(&pool)
        .await
        .unwrap();
    let mut worker = Worker::connect(&hub, &machine).await;
    timeout(Duration::from_secs(3), async {
        loop {
            if state(&pool, submitted.command_id).await.0 == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        state(&pool, submitted.command_id).await,
        ("completed".into(), "rejected".into(), false, true)
    );
    let (result, reason, slot): (Value, String, String) = sqlx::query_as("select c.result, i.reject_reason, q.run_slot from commands c join inputs i on i.command_id = c.id join queue_state q on q.conversation_id = c.conversation_id where c.id = $1")
        .bind(submitted.command_id).fetch_one(&pool).await.unwrap();
    assert_eq!(result, json!({"rejected":"resource_limit"}));
    assert_eq!(reason, "resource_limit");
    assert_eq!(slot, "free");
    assert!(
        timeout(Duration::from_millis(100), worker.down.message())
            .await
            .is_err()
    );
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn restart_redelivers_pending_commands(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let submitted = input(&pool, &hub, conversation).await;
    dispatch_next(
        &pool,
        conversation,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(state(&pool, submitted.command_id).await.0, "pending");
    let installation = hub.installation;
    stop(hub).await;
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    assert_eq!(hub.installation, installation);
    let mut worker = Worker::connect(&hub, &machine).await;
    let command = worker.command().await;
    assert_eq!(command.command_id, submitted.command_id.to_string());
    worker.stored(&command).await;
    assert!(
        timeout(Duration::from_millis(600), worker.down.message())
            .await
            .is_err()
    );
    assert_eq!(state(&pool, submitted.command_id).await.1, "on_machine");
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn live_revocation_closes_both_streams_and_blocks_delivery_and_ingest(pool: PgPool) {
    use canterlot_hub::grpc::Ingest as _;
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    assert!(machines::revoke(&pool, machine.machine_id).await.unwrap());
    let submitted = input(&pool, &hub, conversation).await;
    let batch = JournalRecords {
        journal_id: worker.journal.to_string(),
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::CommandStored(CommandStored {
                    machine_time_ms: 0,
                    command_id: submitted.command_id.to_string(),
                    dispatch_attempt: 1,
                })),
            }),
        }],
    };
    let ingest = canterlot_hub::ingest::Ingest::new(pool.clone());
    let error = ingest
        .cj_records(
            machine.machine_id,
            &sha2::Sha256::digest(machine.token.as_bytes()),
            batch.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    // Проверяем сетевой путь, даже если уведомление уже закрыло входную очередь.
    let _ = worker
        .control
        .send(ControlUp {
            msg: Some(control_up::Msg::CjRecords(batch)),
        })
        .await;
    let (control, data) = timeout(Duration::from_secs(2), async {
        tokio::join!(worker.down.message(), worker.data_down.message())
    })
    .await
    .unwrap();
    assert_eq!(control.unwrap_err().code(), tonic::Code::Unauthenticated);
    assert_eq!(data.unwrap_err().code(), tonic::Code::Unauthenticated);
    assert!(hub.registry.session_info(machine.machine_id).is_none());
    assert_eq!(
        hub.registry.availability(machine.machine_id),
        Availability::Unavailable
    );
    let stored: bool =
        sqlx::query_scalar("select stored_at is not null from commands where id = $1")
            .bind(submitted.command_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!stored);
    let cursors: i64 = sqlx::query_scalar("select count(*) from journal_cursors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cursors, 0);
    stop(hub).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn degraded_outbox_resends_past_older_unsent_row_without_spinning(pool: PgPool) {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let queries = Arc::new(AtomicU64::new(0));
    let count = queries.clone();
    let measured = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .before_acquire(move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(true) })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let hub = spawn_test_hub(
        measured,
        HubTimers {
            command_stored_receipt: Duration::from_millis(500),
            dispatch_sweep: Duration::from_millis(50),
            ..timers()
        },
    )
    .await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    input(&pool, &hub, conversation).await;
    let command = worker.command().await;
    drop(worker.data_down);
    drop(worker.data);
    timeout(Duration::from_secs(2), async {
        while hub.registry.availability(machine.machine_id) != Availability::Degraded {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let older = Uuid::now_v7();
    sqlx::query("insert into commands (id, machine_id, conversation_id, execution_id, execution_generation, kind, payload, delivery_state, created_at) select $1, machine_id, conversation_id, execution_id, execution_generation, 'interrupt', '{}', 'pending', created_at - interval '1 hour' from commands where id = $2")
        .bind(older).bind(Uuid::parse_str(&command.command_id).unwrap()).execute(&pool).await.unwrap();
    let before = queries.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(650)).await;
    let calls = queries.load(Ordering::Relaxed) - before;
    eprintln!("outbox_database_acquires={calls}");
    assert!(calls < 100, "database acquire count: {calls}");
    let message = timeout(Duration::from_millis(100), worker.down.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(message.msg, Some(control_down::Msg::Command(value)) if value == command));
    assert!(
        timeout(Duration::from_secs(2), worker.down.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    stop(hub).await;
}

#[derive(Clone)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn diagnostics_cover_sessions_ingest_resend_and_restart_without_secrets(pool: PgPool) {
    let buffer = LogBuffer(Default::default());
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(buffer.clone())
            .finish(),
    )
    .unwrap();
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    let submitted = input(&pool, &hub, conversation).await;
    let command = worker.command().await;
    assert_eq!(worker.command().await, command);
    assert!(
        timeout(Duration::from_secs(2), worker.down.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let ingest = canterlot_hub::ingest::Ingest::new(pool.clone());
    let journal = Uuid::now_v7();
    assert!(
        ingest
            .apply_batch(
                machine.machine_id,
                &sha2::Sha256::digest(machine.token.as_bytes()),
                canterlot_hub::ingest::JournalKind::Cj,
                &JournalRecords {
                    journal_id: journal.to_string(),
                    records: vec![JournalRecord { seq: 1, body: None }],
                }
            )
            .await
            .is_err()
    );
    // Другой data_dir с тем же credential должен дать диагностируемый отказ.
    let hello = Hello {
        installation_id: Uuid::now_v7().to_string(),
        restore_generation: hub.installation.restore_generation.to_string(),
        machine_id: machine.machine_id.to_string(),
        connection_id: Uuid::now_v7().to_string(),
        cj_journal_id: Uuid::now_v7().to_string(),
        boot_id: Uuid::now_v7().to_string(),
        protocol_version: canterlot_hub::grpc::PROTOCOL_VERSION,
        stream_kind: StreamKind::Control as i32,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel(1);
    tx.send(ControlUp {
        msg: Some(control_up::Msg::Hello(hello)),
    })
    .await
    .unwrap();
    let mut client = worker_control_client::WorkerControlClient::new(channel(&hub).await);
    let mut rejected = client
        .control(request(ReceiverStream::new(rx), &machine))
        .await
        .unwrap()
        .into_inner();
    assert!(matches!(
        rejected.message().await.unwrap().unwrap().msg,
        Some(control_down::Msg::HelloReply(HelloReply {
            result: Some(hello_reply::Result::Rejected(_))
        }))
    ));
    stop(hub).await;
    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    for kind in [
        "session_accepted",
        "session_rejected",
        "session_closed",
        "unknown_record",
        "outbox_resend",
        "outbox_restart",
    ] {
        assert!(logs.contains(kind), "missing diagnostic: {kind}");
    }
    for field in [
        "machine_id=",
        "connection_id=",
        "journal_id=",
        "command_id=",
    ] {
        assert!(logs.contains(field));
    }
    assert!(logs.contains(&submitted.command_id.to_string()));
    assert!(logs.contains(&journal.to_string()));
    for secret in [
        &machine.token,
        "synthetic input",
        "postgres://",
        "postgresql://",
    ] {
        assert!(!logs.contains(secret));
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn revoked_outbox_backstop_blocks_pending_command_before_notification(pool: PgPool) {
    let hub = spawn_test_hub(pool.clone(), timers()).await;
    let (machine, conversation) = setup(&pool, &hub).await;
    let mut worker = Worker::connect(&hub, &machine).await;
    sqlx::query(
        "update machine_credentials set revoked_at = clock_timestamp() where machine_id = $1",
    )
    .bind(machine.machine_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine.machine_id)
        .bind(vec![42_u8; 32])
        .execute(&pool)
        .await
        .unwrap();
    let submitted = input(&pool, &hub, conversation).await;
    dispatch_next(
        &pool,
        conversation,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    if let Ok(result) = timeout(Duration::from_millis(300), worker.down.message()).await {
        assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
    }
    assert_eq!(state(&pool, submitted.command_id).await.0, "pending");
    stop(hub).await;
}
