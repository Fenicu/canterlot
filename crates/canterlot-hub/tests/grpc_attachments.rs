#[path = "support/task9.rs"]
mod support;

use std::time::Duration;

use canterlot_hub::{
    config::HubTimers,
    grpc::{self, HubGrpc},
    ingest::{Ingest, IngestOutcome, JournalKind},
    machines, testkit,
};
use canterlot_proto::{limits::MAX_FILE_CHUNK_BYTES, worker::v1::*};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tonic::{
    Code, Request,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    files: support::Fixture,
    hub: HubGrpc,
    token: String,
    attachment: Uuid,
    input: Uuid,
    command: Uuid,
    bytes: Vec<u8>,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let files = support::Fixture::new(pool.clone()).await;
        let token = Uuid::now_v7().to_string();
        sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
            .bind(files.machine)
            .bind(Sha256::digest(token.as_bytes()).as_slice())
            .execute(&pool)
            .await
            .unwrap();
        let hub = HubGrpc::new(
            pool.clone(),
            files.state.registry.clone(),
            files.state.installation,
            HubTimers::default(),
        )
        .with_blob_store(files.store.clone());
        let bytes = support::content(3 * MAX_FILE_CHUNK_BYTES + 97, 23);
        let attachment = files
            .ready("fixture.bin", "application/octet-stream", &bytes)
            .await;
        let response = files
            .submit(
                &format!("fetch_{}", Uuid::now_v7().simple()),
                "input",
                &[attachment],
            )
            .await;
        assert_eq!(response.status, 201);
        let value: Value = response.json();
        let input = value["input_id"].as_str().unwrap().parse().unwrap();
        let command = value["command_id"].as_str().unwrap().parse().unwrap();
        sqlx::query("update inputs set state='on_machine' where id=$1")
            .bind(input)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("update commands set dispatch_attempt=1, delivery_state='stored_on_machine' where id=$1")
            .bind(command).execute(&pool).await.unwrap();
        Self {
            files,
            hub,
            token,
            attachment,
            input,
            command,
            bytes,
        }
    }

    fn request(&self, attachment: Uuid, offset: u64) -> Request<FetchAttachmentRequest> {
        request(
            FetchAttachmentRequest {
                attachment_id: attachment.to_string(),
                offset,
            },
            &self.token,
        )
    }

    async fn fetch(
        &self,
        attachment: Uuid,
        offset: u64,
    ) -> Result<
        tonic::Response<<HubGrpc as worker_data_server::WorkerData>::FetchAttachmentStream>,
        tonic::Status,
    > {
        worker_data_server::WorkerData::fetch_attachment(
            &self.hub,
            self.request(attachment, offset),
        )
        .await
    }

    fn ready(&self) -> record_body::Body {
        record_body::Body::AttachmentReady(AttachmentReady {
            command_id: self.command.to_string(),
            dispatch_attempt: 1,
            attachment_id: self.attachment.to_string(),
        })
    }

    fn failed(&self, retryable: bool) -> record_body::Body {
        record_body::Body::AttachmentFailed(AttachmentFailed {
            command_id: self.command.to_string(),
            dispatch_attempt: 1,
            attachment_id: self.attachment.to_string(),
            reason: if retryable {
                "hub_unreachable"
            } else {
                "checksum_mismatch"
            }
            .into(),
            retryable,
        })
    }

    async fn ingest(&self, bodies: Vec<record_body::Body>) {
        let batch = JournalRecords {
            journal_id: Uuid::now_v7().to_string(),
            records: bodies
                .into_iter()
                .enumerate()
                .map(|(i, body)| JournalRecord {
                    seq: i as u64 + 1,
                    body: Some(RecordBody { body: Some(body) }),
                })
                .collect(),
        };
        let ingest = Ingest::new(self.files.pool.clone());
        for _ in 0..2 {
            assert_eq!(
                ingest
                    .apply_batch(
                        self.files.machine,
                        &Sha256::digest(self.token.as_bytes()),
                        JournalKind::Cj,
                        &batch
                    )
                    .await
                    .unwrap(),
                IngestOutcome::Committed {
                    up_to_seq: batch.records.len() as u64
                }
            );
        }
    }

    async fn link(&self) -> (String, Option<String>, Option<bool>) {
        sqlx::query_as("select machine_state,machine_reason,retryable from input_attachments where input_id=$1 and attachment_id=$2")
            .bind(self.input).bind(self.attachment).fetch_one(&self.files.pool).await.unwrap()
    }

    async fn input_state(&self) -> (String, Option<String>) {
        sqlx::query_as("select state,reject_reason from inputs where id=$1")
            .bind(self.input)
            .fetch_one(&self.files.pool)
            .await
            .unwrap()
    }

    async fn machine_events(&self) -> Vec<Value> {
        self.files
            .events("attachment_state_changed")
            .await
            .into_iter()
            .filter(|event| event.get("input_id").is_some())
            .collect()
    }

    async fn refresh(&self) {
        let config = canterlot_hub::config::ActivityConfig::default();
        canterlot_hub::activity::refresh(
            &self.files.pool,
            self.files.conversation,
            chrono::Utc::now(),
            &canterlot_hub::activity::Thresholds {
                stalled_generating: config.stalled_generating,
                stalled_tool: config.stalled_tool,
                idle_quiet: config.idle_quiet,
                idle_background_max: std::time::Duration::from_secs(15 * 60),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }
}

fn request<T>(body: T, token: &str) -> Request<T> {
    let mut req = Request::new(body);
    let mut bearer = format!("Bearer {token}")
        .parse::<tonic::metadata::MetadataValue<_>>()
        .unwrap();
    bearer.set_sensitive(true);
    req.metadata_mut().insert("authorization", bearer);
    req
}

async fn server(
    hub: HubGrpc,
) -> (
    worker_data_client::WorkerDataClient<Channel>,
    CancellationToken,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let pki = testkit::tls::test_pki();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let task = tokio::spawn(async move {
        grpc::serve_with_listener(
            hub,
            listener,
            &pki.server_cert_pem,
            &pki.server_key_pem,
            shutdown.cancelled_owned(),
        )
        .await
    });
    let channel = Endpoint::from_shared(format!("https://{address}"))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(pki.ca_pem))
                .domain_name("localhost"),
        )
        .unwrap()
        .connect_timeout(WAIT)
        .connect()
        .await
        .unwrap();
    (
        worker_data_client::WorkerDataClient::new(channel),
        stop,
        task,
    )
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_attachment_streams_in_128k_chunks_from_offset(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    for start in [
        0,
        37,
        MAX_FILE_CHUNK_BYTES as u64 + 11,
        f.bytes.len() as u64,
    ] {
        let mut stream = client
            .fetch_attachment(f.request(f.attachment, start))
            .await
            .unwrap()
            .into_inner();
        let mut offset = start;
        let mut bytes = Vec::new();
        let mut last = false;
        let mut chunks = 0;
        while let Some(chunk) = timeout(WAIT, stream.message()).await.unwrap().unwrap() {
            assert!(!last);
            assert_eq!(chunk.offset, offset);
            let remaining = f.bytes.len() - offset as usize;
            assert_eq!(chunk.data.len(), remaining.min(MAX_FILE_CHUNK_BYTES));
            offset += chunk.data.len() as u64;
            last = chunk.last;
            assert_eq!(last, offset == f.bytes.len() as u64);
            bytes.extend(chunk.data);
            chunks += 1;
        }
        assert!(last);
        assert!(chunks >= 1);
        assert!(bytes == f.bytes[start as usize..]);
    }
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap().unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_rejects_attachment_of_another_machine_or_unlinked(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let unlinked = f
        .files
        .ready("unlinked.bin", "application/octet-stream", &[1])
        .await;
    assert_eq!(
        f.fetch(unlinked, 0).await.err().unwrap().code(),
        Code::NotFound
    );
    assert_eq!(
        f.fetch(Uuid::now_v7(), 0).await.err().unwrap().code(),
        Code::NotFound
    );
    let other = machines::add(&f.files.pool, f.files.owner, "other")
        .await
        .unwrap();
    let error = worker_data_server::WorkerData::fetch_attachment(
        &f.hub,
        request(
            FetchAttachmentRequest {
                attachment_id: f.attachment.to_string(),
                offset: 0,
            },
            &other.token,
        ),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code(), Code::NotFound);
    let error = worker_data_server::WorkerData::fetch_attachment(
        &f.hub,
        Request::new(FetchAttachmentRequest::default()),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code(), Code::Unauthenticated);
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_refuses_cancelled_or_rejected_inputs(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for state in ["cancelled", "rejected"] {
        sqlx::query("update inputs set state=$2 where id=$1")
            .bind(f.input)
            .bind(state)
            .execute(&f.files.pool)
            .await
            .unwrap();
        assert_eq!(
            f.fetch(f.attachment, 0).await.err().unwrap().code(),
            Code::NotFound
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_concurrency_is_limited_per_machine(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(f.fetch(f.attachment, 0).await.unwrap());
    }
    assert_eq!(
        f.fetch(f.attachment, 0).await.err().unwrap().code(),
        Code::ResourceExhausted
    );
    let other = Fixture::new(pool).await;
    let other_hub = f.hub.clone().with_blob_store(other.files.store.clone());
    let independent = worker_data_server::WorkerData::fetch_attachment(
        &other_hub,
        other.request(other.attachment, 0),
    )
    .await
    .unwrap();
    drop(independent);
    drop(streams.pop());
    let replacement = f.fetch(f.attachment, 0).await.unwrap();
    drop(replacement);
    drop(streams);
    let mut completed = f.fetch(f.attachment, 0).await.unwrap().into_inner();
    while let Some(chunk) = completed.next().await {
        chunk.unwrap();
    }
    let mut again = Vec::new();
    for _ in 0..4 {
        again.push(f.fetch(f.attachment, 0).await.unwrap());
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_does_not_hold_a_pg_connection(pool: PgPool) {
    let limited = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let f = Fixture::new(limited.clone()).await;
    let mut streams = Vec::new();
    for _ in 0..4 {
        streams.push(f.fetch(f.attachment, 0).await.unwrap().into_inner());
    }
    for stream in &mut streams {
        assert!(stream.next().await.unwrap().is_ok());
    }
    let (a, b) = timeout(WAIT, async {
        let a = limited.acquire().await.unwrap();
        let b = limited.acquire().await.unwrap();
        (a, b)
    })
    .await
    .unwrap();
    drop((a, b));
    drop(streams);
    limited.close().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn fetch_offset_beyond_size_is_out_of_range(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for offset in [f.bytes.len() as u64 + 1, u64::MAX] {
        assert_eq!(
            f.fetch(f.attachment, offset).await.err().unwrap().code(),
            Code::OutOfRange
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn attachment_ready_updates_link_and_emits_event(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.ingest(vec![f.failed(true), f.ready(), f.ready()]).await;
    assert_eq!(f.link().await, ("ready".into(), None, None));
    assert_eq!(f.input_state().await, ("on_machine".into(), None));
    let events = f.machine_events().await;
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[1],
        json!({"input_id":f.input,"attachment_id":f.attachment,
        "machine_state":"ready","machine_reason":null})
    );
    assert!(
        canterlot_hub::conversations::attachments_pending(
            &mut f.files.pool.acquire().await.unwrap(),
            f.files.conversation
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn nonretryable_attachment_failure_rejects_input_before_transmission(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.ingest(vec![f.failed(false), f.failed(false)]).await;
    assert_eq!(
        f.link().await,
        (
            "failed".into(),
            Some("checksum_mismatch".into()),
            Some(false)
        )
    );
    assert_eq!(f.input_state().await, ("on_machine".into(), None));
    assert_eq!(f.machine_events().await.len(), 1);
    f.ingest(vec![record_body::Body::CommandRejected(CommandRejected {
        command_id: f.command.to_string(),
        dispatch_attempt: 1,
        reason: "attachment_unavailable".into(),
    })])
    .await;
    assert_eq!(
        f.input_state().await,
        ("rejected".into(), Some("attachment_unavailable".into()))
    );
    let status: String = sqlx::query_scalar("select last_status from commands where id=$1")
        .bind(f.command)
        .fetch_one(&f.files.pool)
        .await
        .unwrap();
    assert_eq!(status, "rejected_before_transmission");
    assert_eq!(
        f.files
            .count("select count(*) from events where kind='input_state_changed' and payload->>'cause'='transmission_started'")
            .await,
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn retryable_attachment_failure_only_updates_the_reason(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.ingest(vec![f.failed(true), f.failed(true)]).await;
    assert_eq!(
        f.link().await,
        ("pending".into(), Some("hub_unreachable".into()), Some(true))
    );
    assert_eq!(f.input_state().await, ("on_machine".into(), None));
    assert_eq!(
        f.machine_events().await,
        vec![json!({"input_id":f.input,"attachment_id":f.attachment,
        "machine_state":"pending","machine_reason":"hub_unreachable"})]
    );
    f.refresh().await;
    let expected = json!({"reason":"attachments_pending","input_id":f.input,"machine_reason":"hub_unreachable"});
    let holds: Value = sqlx::query_scalar(
        "select hold_reasons from conversation_derived where conversation_id=$1",
    )
    .bind(f.files.conversation)
    .fetch_one(&f.files.pool)
    .await
    .unwrap();
    assert!(holds.as_array().unwrap().contains(&expected));
    let projected = f.files.events("derived_changed").await;
    assert!(
        projected.last().unwrap()["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    let detail = canterlot_hub::conversations::detail(
        &mut f.files.pool.acquire().await.unwrap(),
        f.files.owner,
        f.files.conversation,
    )
    .await
    .unwrap()
    .unwrap();
    let detail = serde_json::to_value(detail).unwrap();
    assert_eq!(
        detail["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|hold| *hold == &expected)
            .count(),
        1
    );
    f.ingest(vec![f.ready()]).await;
    f.refresh().await;
    let projected = f.files.events("derived_changed").await;
    assert!(
        !projected.last().unwrap()["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hold| hold["reason"] == "attachments_pending")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn attachment_records_ignore_stale_attempts_and_unlinked_files(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut stale_ready = f.ready();
    let record_body::Body::AttachmentReady(ready) = &mut stale_ready else {
        unreachable!()
    };
    ready.dispatch_attempt = 0;
    let mut stale_failed = f.failed(false);
    let record_body::Body::AttachmentFailed(failed) = &mut stale_failed else {
        unreachable!()
    };
    failed.dispatch_attempt = 2;
    let mut unlinked = f.ready();
    let record_body::Body::AttachmentReady(ready) = &mut unlinked else {
        unreachable!()
    };
    ready.attachment_id = Uuid::now_v7().to_string();
    f.ingest(vec![stale_ready, stale_failed, unlinked]).await;
    assert_eq!(f.link().await, ("pending".into(), None, None));
    assert!(f.machine_events().await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn attachments_hold_until_every_link_is_ready(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let second = f
        .files
        .ready("second.bin", "application/octet-stream", &[2])
        .await;
    sqlx::query("insert into input_attachments(input_id,attachment_id,position) values($1,$2,1)")
        .bind(f.input)
        .bind(second)
        .execute(&f.files.pool)
        .await
        .unwrap();
    f.ingest(vec![f.ready()]).await;
    assert_eq!(
        canterlot_hub::conversations::attachments_pending(
            &mut f.files.pool.acquire().await.unwrap(),
            f.files.conversation
        )
        .await
        .unwrap(),
        vec![(f.input, None)]
    );
    let mut ready = f.ready();
    let record_body::Body::AttachmentReady(fact) = &mut ready else {
        unreachable!()
    };
    fact.attachment_id = second.to_string();
    f.ingest(vec![ready]).await;
    assert!(
        canterlot_hub::conversations::attachments_pending(
            &mut f.files.pool.acquire().await.unwrap(),
            f.files.conversation
        )
        .await
        .unwrap()
        .is_empty()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn attachment_records_of_another_machine_are_ignored(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = machines::add(&f.files.pool, f.files.owner, "other")
        .await
        .unwrap();
    let ingest = Ingest::new(f.files.pool.clone());
    let batch = JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: vec![f.ready(), f.failed(true), f.failed(false)]
            .into_iter()
            .enumerate()
            .map(|(i, body)| JournalRecord {
                seq: i as u64 + 1,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    };
    assert_eq!(
        ingest
            .apply_batch(
                other.machine_id,
                &Sha256::digest(other.token.as_bytes()),
                JournalKind::Cj,
                &batch
            )
            .await
            .unwrap(),
        IngestOutcome::Committed { up_to_seq: 3 }
    );
    assert_eq!(ingest.ignored_record_count(), 3);
    assert_eq!(f.link().await, ("pending".into(), None, None));
    assert!(f.machine_events().await.is_empty());
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrations = "./migrations")]
async fn hub_shutdown_ends_open_fetch_streams_with_unavailable(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
    let f = Fixture::new(pool).await;
    let pause = f.hub.faults.pause_next(Point::BeforeFetchChunk);
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let mut stream = client
        .fetch_attachment(f.request(f.attachment, 0))
        .await
        .unwrap()
        .into_inner();
    let resume = timeout(WAIT, pause).await.unwrap().unwrap();
    stop.cancel();
    assert_eq!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
    timeout(WAIT, task).await.unwrap().unwrap().unwrap();
    drop(resume);
    assert_eq!(f.input_state().await, ("on_machine".into(), None));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_execution_attempt_resets_attachment_readiness_atomically(pool: PgPool) {
    use canterlot_hub::{
        delivery::{
            dispatch::{DispatchOutcome, GateInputs, dispatch_next},
            receipts::{InputReceipt, apply_input_receipt},
        },
        executions,
        model::ExecutionState,
    };
    let f = Fixture::new(pool.clone()).await;
    f.ingest(vec![f.ready()]).await;
    assert_eq!(f.link().await.0, "ready");
    apply_input_receipt(&pool, f.command, 1, InputReceipt::Withdrawn, Uuid::now_v7())
        .await
        .unwrap();
    let exec = executions::start(&pool, f.files.owner, f.files.conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, exec.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let correlation = Uuid::now_v7();
    let result = dispatch_next(
        &pool,
        f.files.conversation,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        correlation,
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    assert_eq!(f.link().await, ("pending".into(), None, None));
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        !canterlot_hub::conversations::attachments_pending(&mut conn, f.files.conversation)
            .await
            .unwrap()
            .is_empty()
    );
    let event: Value = sqlx::query_scalar(
        "select payload from events where kind='attachment_state_changed' and correlation_id=$1",
    )
    .bind(correlation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event["machine_state"], "pending");
    assert_eq!(event["input_id"], f.input.to_string());
    f.ingest(vec![f.ready()]).await;
    assert_eq!(f.link().await.0, "pending");
    let ready = record_body::Body::AttachmentReady(AttachmentReady {
        command_id: f.command.to_string(),
        dispatch_attempt: 2,
        attachment_id: f.attachment.to_string(),
    });
    f.ingest(vec![ready.clone()]).await;
    let before = f.machine_events().await.len();
    f.ingest(vec![ready]).await;
    assert_eq!(f.link().await, ("ready".into(), None, None));
    assert_eq!(f.machine_events().await.len(), before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_attempt_clears_attachment_failure_reason_and_retryability(pool: PgPool) {
    use canterlot_hub::{
        delivery::{
            dispatch::{DispatchOutcome, GateInputs, dispatch_next},
            receipts::{InputReceipt, apply_input_receipt},
        },
        executions,
        model::ExecutionState,
    };
    let f = Fixture::new(pool.clone()).await;
    f.ingest(vec![f.failed(true)]).await;
    assert_eq!(
        f.link().await,
        ("pending".into(), Some("hub_unreachable".into()), Some(true))
    );
    apply_input_receipt(&pool, f.command, 1, InputReceipt::Withdrawn, Uuid::now_v7())
        .await
        .unwrap();
    let exec = executions::start(&pool, f.files.owner, f.files.conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, exec.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let result = dispatch_next(
        &pool,
        f.files.conversation,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        result,
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    assert_eq!(f.link().await, ("pending".into(), None, None));
}

/// Пустое вложение уходит машине одним кадром: смещение 0, без данных, `last`.
#[sqlx::test(migrations = "./migrations")]
async fn fetch_of_an_empty_attachment_is_one_last_empty_chunk(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let empty = f.files.ready("empty.txt", "text/plain", b"").await;
    let response = f
        .files
        .submit(
            &format!("fetch_{}", Uuid::now_v7().simple()),
            "empty",
            &[empty],
        )
        .await;
    assert_eq!(response.status, 201, "{:?}", response.body);
    let value: Value = response.json();
    let input: Uuid = value["input_id"].as_str().unwrap().parse().unwrap();
    let command: Uuid = value["command_id"].as_str().unwrap().parse().unwrap();
    sqlx::query("update inputs set state='on_machine' where id=$1")
        .bind(input)
        .execute(&f.files.pool)
        .await
        .unwrap();
    sqlx::query(
        "update commands set dispatch_attempt=1, delivery_state='stored_on_machine' where id=$1",
    )
    .bind(command)
    .execute(&f.files.pool)
    .await
    .unwrap();
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let mut stream = client
        .fetch_attachment(f.request(empty, 0))
        .await
        .unwrap()
        .into_inner();
    let chunk = timeout(WAIT, stream.message())
        .await
        .unwrap()
        .unwrap()
        .expect("one chunk");
    assert_eq!((chunk.offset, chunk.data.len(), chunk.last), (0, 0, true));
    assert!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let error = f.fetch(empty, 1).await.err().expect("offset beyond end");
    assert_eq!(error.code(), tonic::Code::OutOfRange);
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap().unwrap();
}
