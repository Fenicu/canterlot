use std::{net::SocketAddr, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    grpc::{self, HubGrpc, PROTOCOL_VERSION},
    installation::{self, Installation},
    machines::{self, NewMachine},
    session::{Availability, Registry},
    testkit::tls::{TestPki, test_pki},
};
use canterlot_proto::{limits::MAX_MESSAGE_BYTES, worker::v1::*};
use prost::Message;
use sqlx::PgPool;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Code, Request, Streaming,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Host {
    address: SocketAddr,
    pki: TestPki,
    registry: Registry,
    installation: Installation,
    machine: NewMachine,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

fn timers() -> HubTimers {
    HubTimers {
        handshake: Duration::from_millis(100),
        degraded_after: Duration::from_secs(5),
        unavailable_after: Duration::from_secs(10),
        ..HubTimers::default()
    }
}

impl Host {
    async fn start(pool: PgPool, timers: HubTimers) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let installation = installation::init(&pool, &dir.path().join("manifest"))
            .await
            .unwrap();
        let machine = machines::add(&pool, installation.owner_id, "synthetic")
            .await
            .unwrap();
        let registry = Registry::new(timers.clone());
        let state = HubGrpc::new(pool, registry.clone(), installation, timers);
        let pki = test_pki();
        let cert = pki.server_cert_pem.clone();
        let key = pki.server_key_pem.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            grpc::serve_with_listener(state, listener, &cert, &key, std::future::pending()).await
        });
        Self {
            address,
            pki,
            registry,
            installation,
            machine,
            task,
        }
    }

    async fn channel(&self) -> Channel {
        self.endpoint(&self.pki.ca_pem, "localhost")
            .connect()
            .await
            .unwrap()
    }

    fn endpoint(&self, ca: &str, domain: &str) -> Endpoint {
        Endpoint::from_shared(format!("https://{}", self.address))
            .unwrap()
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(ca))
                    .domain_name(domain),
            )
            .unwrap()
            .connect_timeout(WAIT)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true)
    }

    fn hello(&self, machine: Uuid, connection: Uuid, kind: StreamKind) -> Hello {
        Hello {
            installation_id: self.installation.installation_id.to_string(),
            machine_id: machine.to_string(),
            restore_generation: self.installation.restore_generation.to_string(),
            protocol_version: PROTOCOL_VERSION,
            connection_id: connection.to_string(),
            cj_journal_id: Uuid::now_v7().to_string(),
            boot_id: Uuid::now_v7().to_string(),
            stream_kind: kind as i32,
            ..Hello::default()
        }
    }

    async fn control(&self, hello: Hello) -> (mpsc::Sender<ControlUp>, Streaming<ControlDown>) {
        let (tx, rx) = mpsc::channel(8);
        send(
            &tx,
            ControlUp {
                msg: Some(control_up::Msg::Hello(hello)),
            },
        )
        .await;
        let mut client = worker_control_client::WorkerControlClient::new(self.channel().await)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let response = client
            .control(request(ReceiverStream::new(rx), Some(&self.machine.token)))
            .await
            .unwrap();
        (tx, response.into_inner())
    }

    async fn data(&self, hello: Hello, token: &str) -> (mpsc::Sender<DataUp>, Streaming<DataDown>) {
        let (tx, rx) = mpsc::channel(8);
        send(
            &tx,
            DataUp {
                msg: Some(data_up::Msg::Hello(hello)),
            },
        )
        .await;
        let mut client = worker_data_client::WorkerDataClient::new(self.channel().await)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let response = client
            .data(request(ReceiverStream::new(rx), Some(token)))
            .await
            .unwrap();
        (tx, response.into_inner())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn request<T>(body: T, token: Option<&str>) -> Request<T> {
    let mut request = Request::new(body);
    if let Some(token) = token {
        let mut credential = format!("Bearer {token}")
            .parse::<tonic::metadata::MetadataValue<_>>()
            .unwrap();
        credential.set_sensitive(true);
        request.metadata_mut().insert("authorization", credential);
    }
    request
}

async fn send<T: Message>(tx: &mpsc::Sender<T>, message: T) {
    assert!(message.encoded_len() <= MAX_MESSAGE_BYTES);
    assert!(tx.send(message).await.is_ok());
}

async fn next<T: Message + Default>(stream: &mut Streaming<T>) -> T {
    timeout(WAIT, stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn accepted(stream: &mut Streaming<ControlDown>) -> Accepted {
    let Some(control_down::Msg::HelloReply(HelloReply {
        result: Some(hello_reply::Result::Accepted(value)),
    })) = next(stream).await.msg
    else {
        panic!("expected accepted control handshake")
    };
    value
}

async fn data_accepted(stream: &mut Streaming<DataDown>) -> Accepted {
    let Some(data_down::Msg::HelloReply(HelloReply {
        result: Some(hello_reply::Result::Accepted(value)),
    })) = next(stream).await.msg
    else {
        panic!("expected accepted data handshake")
    };
    value
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cj_ack_waits_for_record_commit(pool: PgPool) -> TestResult {
    let host = Host::start(pool.clone(), timers()).await;
    let conversation = canterlot_hub::testkit::seed_conversation(
        &pool,
        host.installation.owner_id,
        host.machine.machine_id,
    )
    .await;
    let execution = canterlot_hub::executions::start(
        &pool,
        host.installation.owner_id,
        conversation,
        Uuid::now_v7(),
    )
    .await?;
    let hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    let journal = Uuid::parse_str(&hello.cj_journal_id)?;
    let (tx, mut stream) = host.control(hello).await;
    accepted(&mut stream).await;
    sqlx::query("insert into journal_cursors (machine_id, journal_id, kind) values ($1, $2, 'cj')")
        .bind(host.machine.machine_id)
        .bind(journal)
        .execute(&pool)
        .await?;
    sqlx::query("create function block_journal_commit() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(741234); return new; end $$")
        .execute(&pool).await?;
    sqlx::query("create constraint trigger block_journal_commit after update on journal_cursors deferrable initially deferred for each row execute function block_journal_commit()")
        .execute(&pool).await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(741234)")
        .execute(&mut *blocker)
        .await?;
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id: journal.to_string(),
                records: vec![JournalRecord {
                    seq: 1,
                    body: Some(RecordBody {
                        body: Some(record_body::Body::CommandStored(CommandStored {
                            command_id: execution.command_id.to_string(),
                            dispatch_attempt: 0,
                        })),
                    }),
                }],
            })),
        },
    )
    .await;
    timeout(Duration::from_secs(2), async {
        loop {
            let committing: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname = current_database() and query = 'COMMIT' and wait_event = 'advisory')")
                .fetch_one(&pool).await.unwrap();
            if committing { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await?;
    assert!(
        timeout(Duration::from_millis(100), stream.message())
            .await
            .is_err()
    );
    let committed: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(committed, 0);
    let state: String = sqlx::query_scalar("select delivery_state from commands where id = $1")
        .bind(execution.command_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "pending");
    blocker.commit().await?;
    let Some(control_down::Msg::CjAck(ack)) = next(&mut stream).await.msg else {
        panic!("expected journal range ACK");
    };
    assert_eq!(ack.journal_id, journal.to_string());
    assert_eq!(ack.up_to_seq, 1);
    let committed: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(committed, 1);
    let state: (String, bool) =
        sqlx::query_as("select delivery_state, stored_at is not null from commands where id = $1")
            .bind(execution.command_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, ("stored_on_machine".into(), true));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn heartbeats_are_acked_while_a_large_batch_is_ingested(pool: PgPool) -> TestResult {
    let host = Host::start(pool.clone(), timers()).await;
    let hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    let journal = hello.cj_journal_id.clone();
    let (tx, mut stream) = host.control(hello).await;
    accepted(&mut stream).await;
    sqlx::query("create function block_journal_commit() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(741235); return new; end $$")
        .execute(&pool).await?;
    sqlx::query("create constraint trigger block_journal_commit after insert or update on journal_cursors deferrable initially deferred for each row execute function block_journal_commit()")
        .execute(&pool).await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(741235)")
        .execute(&mut *blocker)
        .await?;
    let records = (1..=2000)
        .map(|seq| JournalRecord {
            seq,
            body: Some(RecordBody {
                body: Some(record_body::Body::JournalClosed(JournalClosed::default())),
            }),
        })
        .collect();
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id: journal.clone(),
                records,
            })),
        },
    )
    .await;
    timeout(Duration::from_secs(10), async {
        loop {
            let committing: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname = current_database() and query = 'COMMIT' and wait_event = 'advisory')")
                .fetch_one(&pool).await.unwrap();
            if committing { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    for seq in 1..=3 {
        send(
            &tx,
            ControlUp {
                msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq })),
            },
        )
        .await;
        let Some(control_down::Msg::HeartbeatAck(ack)) =
            timeout(Duration::from_secs(5), stream.message())
                .await??
                .unwrap()
                .msg
        else {
            panic!("expected heartbeat ACK while the batch is blocked");
        };
        assert_eq!(ack.seq, seq);
    }
    blocker.commit().await?;
    let Some(control_down::Msg::CjAck(ack)) = timeout(Duration::from_secs(60), stream.message())
        .await??
        .unwrap()
        .msg
    else {
        panic!("expected CJ ACK");
    };
    assert_eq!(ack.up_to_seq, 2000);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cj_batch_for_another_journal_ends_the_stream(pool: PgPool) -> TestResult {
    let host = Host::start(pool.clone(), timers()).await;
    let (tx, mut stream) = host
        .control(host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control))
        .await;
    accepted(&mut stream).await;
    let foreign = Uuid::now_v7();
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id: foreign.to_string(),
                records: vec![JournalRecord {
                    seq: 1,
                    body: Some(RecordBody {
                        body: Some(record_body::Body::JournalClosed(JournalClosed::default())),
                    }),
                }],
            })),
        },
    )
    .await;
    let error = timeout(Duration::from_secs(2), stream.message())
        .await?
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(error.message(), "invalid_record");
    let cursors: i64 = sqlx::query_scalar("select count(*) from journal_cursors")
        .fetch_one(&pool)
        .await?;
    assert_eq!(cursors, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn journal_gap_reconnect_resumes_from_committed_cursor(pool: PgPool) -> TestResult {
    let host = Host::start(pool, timers()).await;
    let mut hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    let journal_id = hello.cj_journal_id.clone();
    let (tx, mut control) = host.control(hello.clone()).await;
    accepted(&mut control).await;
    let closed = |seq| JournalRecord {
        seq,
        body: Some(RecordBody {
            body: Some(record_body::Body::JournalClosed(JournalClosed::default())),
        }),
    };
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id: journal_id.clone(),
                records: vec![closed(1), closed(3)],
            })),
        },
    )
    .await;
    let error = timeout(Duration::from_secs(2), control.message())
        .await?
        .unwrap_err();
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert_eq!(error.message(), "journal_gap");
    timeout(Duration::from_secs(2), async {
        while host
            .registry
            .session_info(host.machine.machine_id)
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    hello.connection_id = Uuid::now_v7().to_string();
    let (tx, mut control) = host.control(hello).await;
    assert_eq!(
        accepted(&mut control).await.committed,
        vec![JournalCommitted {
            journal_id: journal_id.clone(),
            committed_seq: 1,
        }]
    );
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id: journal_id.clone(),
                records: vec![JournalRecord { seq: 1, body: None }, closed(2), closed(3)],
            })),
        },
    )
    .await;
    let Some(control_down::Msg::CjAck(ack)) = next(&mut control).await.msg else {
        panic!("expected CJ ACK");
    };
    assert_eq!(
        ack,
        JournalRangeAck {
            journal_id: journal_id.clone(),
            up_to_seq: 3
        }
    );
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(JournalRecords {
                journal_id,
                records: vec![JournalRecord {
                    seq: 4,
                    body: Some(RecordBody::default()),
                }],
            })),
        },
    )
    .await;
    let error = timeout(Duration::from_secs(2), control.message())
        .await?
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(error.message(), "unknown_record");
    Ok(())
}

async fn rejected(stream: &mut Streaming<ControlDown>, reason: RejectReason, retryable: bool) {
    let Some(control_down::Msg::HelloReply(HelloReply {
        result: Some(hello_reply::Result::Rejected(value)),
    })) = next(stream).await.msg
    else {
        panic!("expected rejected control handshake")
    };
    assert_eq!(value.reason, reason as i32);
    assert_eq!(value.retryable, retryable);
    assert!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
}

async fn data_rejected(stream: &mut Streaming<DataDown>, reason: RejectReason, retryable: bool) {
    let Some(data_down::Msg::HelloReply(HelloReply {
        result: Some(hello_reply::Result::Rejected(value)),
    })) = next(stream).await.msg
    else {
        panic!("expected rejected data handshake")
    };
    assert_eq!(value.reason, reason as i32);
    assert_eq!(value.retryable, retryable);
    assert!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
}

async fn heartbeat(tx: &mpsc::Sender<ControlUp>, stream: &mut Streaming<ControlDown>, seq: u64) {
    send(
        tx,
        ControlUp {
            msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq })),
        },
    )
    .await;
    assert!(
        matches!(next(stream).await.msg, Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq: actual })) if seq == actual)
    );
}

async fn changed(
    changes: &mut tokio::sync::broadcast::Receiver<(Uuid, Availability)>,
    machine: Uuid,
    expected: Availability,
) {
    timeout(WAIT, async {
        loop {
            if changes.recv().await.unwrap() == (machine, expected) {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn handshake_accepts_and_reports_cursors(pool: PgPool) -> TestResult {
    let host = Host::start(pool.clone(), timers()).await;
    let other = machines::add(&pool, host.installation.owner_id, "other").await?;
    let journals = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    for (machine, journal, kind, seq) in [
        (host.machine.machine_id, journals[0], "cj", 17_i64),
        (host.machine.machine_id, journals[1], "sj", 29),
        (other.machine_id, journals[2], "sj", 55),
    ] {
        sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, committed_seq) values ($1,$2,$3,$4)").bind(machine).bind(journal).bind(kind).bind(seq).execute(&pool).await?;
    }
    let connection = Uuid::now_v7();
    let mut hello = host.hello(host.machine.machine_id, connection, StreamKind::Control);
    hello.optional_capabilities = vec!["future_optional".into()];
    let (tx, mut control) = host.control(hello).await;
    let reply = accepted(&mut control).await;
    assert_eq!(
        reply.restore_generation,
        host.installation.restore_generation.to_string()
    );
    assert_eq!(reply.committed.len(), 2);
    for (id, seq) in [(journals[0], 17), (journals[1], 29)] {
        assert!(
            reply
                .committed
                .iter()
                .any(|cursor| cursor.journal_id == id.to_string() && cursor.committed_seq == seq)
        );
    }
    assert_eq!(
        host.registry
            .command_sender(host.machine.machine_id)
            .unwrap()
            .max_capacity(),
        64
    );
    let (_data_tx, mut data) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    assert_eq!(data_accepted(&mut data).await, reply);
    assert_eq!(
        host.registry.availability(host.machine.machine_id),
        Availability::Available
    );
    heartbeat(&tx, &mut control, 77).await;
    Ok(())
}

async fn denied(host: &Host, token: Option<&str>) {
    let (_tx, rx) = mpsc::channel::<ControlUp>(1);
    let mut client = worker_control_client::WorkerControlClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let error = client
        .control(request(ReceiverStream::new(rx), token))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unauthenticated);
    let (_tx, rx) = mpsc::channel::<DataUp>(1);
    let mut client = worker_data_client::WorkerDataClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let error = client
        .data(request(ReceiverStream::new(rx), token))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unauthenticated);
}

#[sqlx::test(migrations = "./migrations")]
async fn missing_or_wrong_token_is_unauthenticated(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    denied(&host, None).await;
    denied(&host, Some("unknown-synthetic-token")).await;
    assert!(
        host.registry
            .command_sender(host.machine.machine_id)
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn revoked_credential_is_rejected(pool: PgPool) {
    let host = Host::start(pool.clone(), timers()).await;
    assert!(
        machines::revoke(&pool, host.machine.machine_id)
            .await
            .unwrap()
    );
    denied(&host, Some(&host.machine.token)).await;
    assert_eq!(
        host.registry.availability(host.machine.machine_id),
        Availability::Unavailable
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn installation_mismatch_is_rejected(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let mut hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    hello.installation_id = Uuid::now_v7().to_string();
    let (_tx, mut stream) = host.control(hello).await;
    rejected(&mut stream, RejectReason::InstallationMismatch, false).await;
    assert!(
        host.registry
            .command_sender(host.machine.machine_id)
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn restore_generation_mismatch_is_not_operational(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let mut hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    hello.restore_generation = Uuid::now_v7().to_string();
    let (_tx, mut stream) = host.control(hello).await;
    rejected(&mut stream, RejectReason::HubNotOperational, true).await;
    assert!(
        host.registry
            .command_sender(host.machine.machine_id)
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn unknown_required_capability_is_rejected(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let mut hello = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    hello.required_capabilities = vec!["unknown_required".into()];
    let (_tx, mut stream) = host.control(hello).await;
    rejected(&mut stream, RejectReason::UnknownRequiredCapability, false).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn silent_stream_times_out(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let (_tx, rx) = mpsc::channel::<ControlUp>(1);
    let mut client = worker_control_client::WorkerControlClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let mut stream = client
        .control(request(ReceiverStream::new(rx), Some(&host.machine.token)))
        .await
        .unwrap()
        .into_inner();
    let error = timeout(Duration::from_secs(2), stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    let (_tx, rx) = mpsc::channel::<DataUp>(1);
    let mut client = worker_data_client::WorkerDataClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let mut stream = client
        .data(request(ReceiverStream::new(rx), Some(&host.machine.token)))
        .await
        .unwrap()
        .into_inner();
    let error = timeout(Duration::from_secs(2), stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
}

#[sqlx::test(migrations = "./migrations")]
async fn second_live_identity_is_rejected(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let connection = Uuid::now_v7();
    let (tx, mut first) = host
        .control(host.hello(host.machine.machine_id, connection, StreamKind::Control))
        .await;
    accepted(&mut first).await;
    let (_tx0, mut control_only_collision) = host
        .control(host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control))
        .await;
    rejected(
        &mut control_only_collision,
        RejectReason::IdentityInUse,
        true,
    )
    .await;
    let (_data_tx, mut data) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut data).await;
    let sender = host
        .registry
        .command_sender(host.machine.machine_id)
        .unwrap();
    let (_tx2, mut second) = host
        .control(host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control))
        .await;
    rejected(&mut second, RejectReason::IdentityInUse, true).await;
    assert!(
        sender.same_channel(
            &host
                .registry
                .command_sender(host.machine.machine_id)
                .unwrap()
        )
    );
    assert_eq!(
        host.registry.availability(host.machine.machine_id),
        Availability::Available
    );
    heartbeat(&tx, &mut first, 1).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn stale_session_is_replaced(pool: PgPool) {
    let host = Host::start(
        pool,
        HubTimers {
            handshake: Duration::from_secs(5),
            degraded_after: Duration::from_secs(1),
            unavailable_after: Duration::from_secs(3),
            ..timers()
        },
    )
    .await;
    let mut changes = host.registry.subscribe_changes();
    let connection = Uuid::now_v7();
    let (_tx, mut old) = host
        .control(host.hello(host.machine.machine_id, connection, StreamKind::Control))
        .await;
    accepted(&mut old).await;
    let old_sender = host
        .registry
        .command_sender(host.machine.machine_id)
        .unwrap();
    let (_data_tx, mut old_data) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut old_data).await;
    changed(
        &mut changes,
        host.machine.machine_id,
        Availability::Unavailable,
    )
    .await;
    let (new_tx, mut new) = host
        .control(host.hello(host.machine.machine_id, connection, StreamKind::Control))
        .await;
    accepted(&mut new).await;
    for result in [
        timeout(WAIT, old.message())
            .await
            .unwrap()
            .map(|v| v.is_none()),
        timeout(WAIT, old_data.message())
            .await
            .unwrap()
            .map(|v| v.is_none()),
    ] {
        assert!(result.unwrap_or(true));
    }
    assert!(old_sender.is_closed());
    heartbeat(&new_tx, &mut new, 81).await;
    assert!(
        host.registry
            .command_sender(host.machine.machine_id)
            .is_some()
    );
    changed(
        &mut changes,
        host.machine.machine_id,
        Availability::Degraded,
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn data_stream_connection_binding_requires_same_machine(pool: PgPool) {
    let host = Host::start(pool.clone(), timers()).await;
    let other = machines::add(&pool, host.installation.owner_id, "other")
        .await
        .unwrap();
    let connection = Uuid::now_v7();
    let (tx, mut control) = host
        .control(host.hello(host.machine.machine_id, connection, StreamKind::Control))
        .await;
    accepted(&mut control).await;
    for (machine, id, token) in [
        (other.machine_id, connection, other.token.as_str()),
        (
            host.machine.machine_id,
            Uuid::now_v7(),
            host.machine.token.as_str(),
        ),
        (host.machine.machine_id, connection, other.token.as_str()),
    ] {
        let (_tx, mut data) = host
            .data(host.hello(machine, id, StreamKind::Data), token)
            .await;
        data_rejected(&mut data, RejectReason::ConnectionBindingMismatch, true).await;
    }
    let (data_tx, mut data) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut data).await;
    let mut changes = host.registry.subscribe_changes();
    drop(data_tx);
    assert!(data.message().await.unwrap().is_none());
    changed(
        &mut changes,
        host.machine.machine_id,
        Availability::Degraded,
    )
    .await;
    let (_tx2, mut data2) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut data2).await;
    assert_eq!(
        host.registry.availability(host.machine.machine_id),
        Availability::Available
    );
    heartbeat(&tx, &mut control, 19).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn heartbeat_drives_availability(pool: PgPool) {
    let host = Host::start(
        pool,
        HubTimers {
            handshake: Duration::from_secs(5),
            degraded_after: Duration::from_secs(1),
            unavailable_after: Duration::from_secs(3),
            ..timers()
        },
    )
    .await;
    let machine = host.machine.machine_id;
    assert_eq!(
        host.registry.availability(machine),
        Availability::Unavailable
    );
    let mut changes = host.registry.subscribe_changes();
    let connection = Uuid::now_v7();
    let (tx, mut control) = host
        .control(host.hello(machine, connection, StreamKind::Control))
        .await;
    accepted(&mut control).await;
    changed(&mut changes, machine, Availability::Degraded).await;
    let (_data_tx, mut data) = host
        .data(
            host.hello(machine, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut data).await;
    for expected in [
        Availability::Available,
        Availability::Degraded,
        Availability::Unavailable,
    ] {
        assert_eq!(
            timeout(WAIT, changes.recv()).await.unwrap().unwrap(),
            (machine, expected)
        );
    }
    heartbeat(&tx, &mut control, 6).await;
    assert_eq!(
        timeout(WAIT, changes.recv()).await.unwrap().unwrap(),
        (machine, Availability::Available)
    );
    drop(tx);
    assert!(control.message().await.unwrap().is_none());
    assert!(
        timeout(WAIT, data.message())
            .await
            .unwrap()
            .map(|v| v.is_none())
            .unwrap_or(true)
    );
    changed(&mut changes, machine, Availability::Unavailable).await;
    assert!(host.registry.session_info(machine).is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn incompatible_protocol_and_wrong_stream_kind_are_rejected(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let base = host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control);
    for hello in [
        Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            ..base.clone()
        },
        Hello {
            stream_kind: StreamKind::Data as i32,
            ..base.clone()
        },
    ] {
        let (_tx, mut control) = host.control(hello).await;
        rejected(&mut control, RejectReason::ProtocolIncompatible, false).await;
    }
    let (_tx, mut data) = host.data(base, &host.machine.token).await;
    data_rejected(&mut data, RejectReason::ConnectionBindingMismatch, true).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn journal_ingestion_acks_both_streams(pool: PgPool) {
    let host = Host::start(pool.clone(), timers()).await;
    let connection = Uuid::now_v7();
    let hello = host.hello(host.machine.machine_id, connection, StreamKind::Control);
    let cj_journal = hello.cj_journal_id.clone();
    let sj_journal = Uuid::now_v7().to_string();
    let records = |journal_id| JournalRecords {
        journal_id,
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::JournalClosed(JournalClosed::default())),
            }),
        }],
    };
    let (tx, mut control) = host.control(hello).await;
    accepted(&mut control).await;
    let (data_tx, mut data) = host
        .data(
            host.hello(host.machine.machine_id, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut data).await;
    send(
        &data_tx,
        DataUp {
            msg: Some(data_up::Msg::SjRecords(records(sj_journal.clone()))),
        },
    )
    .await;
    let Some(data_down::Msg::SjAck(ack)) = next(&mut data).await.msg else {
        panic!("expected SJ ACK");
    };
    assert_eq!(
        ack,
        JournalRangeAck {
            journal_id: sj_journal.clone(),
            up_to_seq: 1
        }
    );
    heartbeat(&tx, &mut control, 1).await;
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::CjRecords(records(cj_journal.clone()))),
        },
    )
    .await;
    let Some(control_down::Msg::CjAck(ack)) = next(&mut control).await.msg else {
        panic!("expected CJ ACK");
    };
    assert_eq!(
        ack,
        JournalRangeAck {
            journal_id: cj_journal.clone(),
            up_to_seq: 1
        }
    );
    let cursors: Vec<(Uuid, String, i64)> = sqlx::query_as("select journal_id, kind, committed_seq from journal_cursors where machine_id = $1 order by kind")
        .bind(host.machine.machine_id).fetch_all(&pool).await.unwrap();
    assert_eq!(
        cursors,
        vec![
            (Uuid::parse_str(&cj_journal).unwrap(), "cj".into(), 1),
            (Uuid::parse_str(&sj_journal).unwrap(), "sj".into(), 1)
        ]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn tls_rejects_untrusted_ca_and_wrong_name(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let _verified_channel = host.channel().await;
    assert!(
        host.endpoint(&test_pki().ca_pem, "localhost")
            .connect()
            .await
            .is_err()
    );
    assert!(
        host.endpoint(&host.pki.ca_pem, "wrong.invalid")
            .connect()
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn data_reattach_and_control_cancellation_preserve_session_ownership(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let machine = host.machine.machine_id;
    let connection = Uuid::now_v7();
    let (tx, mut control) = host
        .control(host.hello(machine, connection, StreamKind::Control))
        .await;
    accepted(&mut control).await;
    let (_old_tx, mut old) = host
        .data(
            host.hello(machine, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut old).await;
    let (_new_tx, mut new) = host
        .data(
            host.hello(machine, connection, StreamKind::Data),
            &host.machine.token,
        )
        .await;
    data_accepted(&mut new).await;
    assert!(
        timeout(Duration::from_secs(2), old.message())
            .await
            .unwrap()
            .map(|v| v.is_none())
            .unwrap_or(true)
    );
    let info = host.registry.session_info(machine).unwrap();
    assert_eq!(info.machine_id, machine);
    assert_eq!(info.connection_id, connection);
    assert!(info.data_attached);
    heartbeat(&tx, &mut control, 9).await;
    assert!(host.registry.session_info(machine).unwrap().last_heartbeat > info.last_heartbeat);
    assert_eq!(host.registry.availability(machine), Availability::Available);
    drop(tx);
    drop(control);
    assert!(
        timeout(Duration::from_secs(2), new.message())
            .await
            .unwrap()
            .map(|v| v.is_none())
            .unwrap_or(true)
    );
    assert!(host.registry.session_info(machine).is_none());
    assert_eq!(
        host.registry.availability(machine),
        Availability::Unavailable
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn transport_delivery_and_disconnect_do_not_complete_commands(pool: PgPool) -> TestResult {
    let host = Host::start(pool.clone(), timers()).await;
    let conversation = canterlot_hub::testkit::seed_conversation(
        &pool,
        host.installation.owner_id,
        host.machine.machine_id,
    )
    .await;
    let started = canterlot_hub::executions::start(
        &pool,
        host.installation.owner_id,
        conversation,
        Uuid::now_v7(),
    )
    .await?;
    let (tx, mut control) = host
        .control(host.hello(host.machine.machine_id, Uuid::now_v7(), StreamKind::Control))
        .await;
    accepted(&mut control).await;
    let sender = host
        .registry
        .command_sender(host.machine.machine_id)
        .unwrap();
    let command = Command {
        command_id: started.command_id.to_string(),
        execution_id: started.execution_id.to_string(),
        execution_generation: started.generation as u64,
        ..Command::default()
    };
    send(
        &sender,
        ControlDown {
            msg: Some(control_down::Msg::Command(command.clone())),
        },
    )
    .await;
    assert_eq!(
        next(&mut control).await.msg,
        Some(control_down::Msg::Command(command))
    );
    drop(tx);
    assert!(
        timeout(Duration::from_secs(2), control.message())
            .await?
            .unwrap()
            .is_none()
    );
    let (execution, delivery, stored): (String, String, bool) = sqlx::query_as("select e.state, c.delivery_state, c.stored_at is not null from executions e join commands c on c.execution_id = e.id where c.id = $1").bind(started.command_id).fetch_one(&pool).await?;
    assert_eq!(
        (execution.as_str(), delivery.as_str(), stored),
        ("created", "pending", false)
    );
    assert!(
        host.registry
            .session_info(host.machine.machine_id)
            .is_none()
    );
    assert!(sender.is_closed());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn first_message_must_be_hello_on_both_streams(pool: PgPool) {
    let host = Host::start(pool, timers()).await;
    let mut client = worker_control_client::WorkerControlClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let (tx, rx) = mpsc::channel(1);
    send(
        &tx,
        ControlUp {
            msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq: 1 })),
        },
    )
    .await;
    let mut control = client
        .control(request(ReceiverStream::new(rx), Some(&host.machine.token)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        control.message().await.unwrap_err().code(),
        Code::InvalidArgument
    );
    let mut client = worker_data_client::WorkerDataClient::new(host.channel().await)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let (tx, rx) = mpsc::channel(1);
    send(
        &tx,
        DataUp {
            msg: Some(data_up::Msg::SjRecords(JournalRecords::default())),
        },
    )
    .await;
    let mut data = client
        .data(request(ReceiverStream::new(rx), Some(&host.machine.token)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        data.message().await.unwrap_err().code(),
        Code::InvalidArgument
    );
    assert!(
        host.registry
            .session_info(host.machine.machine_id)
            .is_none()
    );
}
