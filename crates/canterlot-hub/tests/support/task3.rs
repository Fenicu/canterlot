#![allow(dead_code)]
use canterlot_hub::{
    blocks::ExecutionRef,
    config::HubTimers,
    delivery::{
        ContentBlock, SubmitInput, SubmittedInput,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        submit_input,
    },
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;
pub struct Run {
    pub seed: Seed,
    pub execution: executions::StartedExecution,
    pub journal: Uuid,
    pub ingest: Ingest,
    pub seq: u64,
}
impl Run {
    pub async fn new(pool: &PgPool) -> Self {
        let seed = testkit::seed(pool).await;
        let execution =
            executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        let mut r = Self {
            seed,
            execution,
            journal: Uuid::now_v7(),
            ingest: Ingest::new(pool.clone()),
            seq: 0,
        };
        r.send(vec![Body::JournalOpened(JournalOpened {
            execution_id: r.execution.execution_id.to_string(),
            generation: r.execution.generation as u64,
            supervisor_protocol_version: 1,
        })])
        .await;
        r
    }
    pub fn reference(&self) -> ExecutionRef {
        ExecutionRef {
            execution_id: self.execution.execution_id,
            conversation_id: self.seed.conversation_id,
            machine_id: self.seed.machine_id,
        }
    }
    pub async fn send(&mut self, bodies: Vec<Body>) {
        let batch = self.batch(bodies);
        assert_eq!(
            self.ingest
                .apply_batch(
                    self.seed.machine_id,
                    &self.seed.credential_digest(),
                    JournalKind::Sj,
                    &batch
                )
                .await
                .unwrap(),
            IngestOutcome::Committed {
                up_to_seq: self.seq
            }
        );
    }
    pub fn batch(&mut self, bodies: Vec<Body>) -> JournalRecords {
        JournalRecords {
            journal_id: self.journal.to_string(),
            records: bodies
                .into_iter()
                .map(|body| {
                    self.seq += 1;
                    JournalRecord {
                        seq: self.seq,
                        body: Some(RecordBody { body: Some(body) }),
                    }
                })
                .collect(),
        }
    }
    pub async fn input(&self, pool: &PgPool) -> SubmittedInput {
        let i = submit_input(
            pool,
            SubmitInput {
                owner_id: self.seed.owner_id,
                conversation_id: self.seed.conversation_id,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: "input".into(),
                }],
                received_at: Utc::now(),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            dispatch_next(
                pool,
                self.seed.conversation_id,
                GateInputs {
                    machine_available: true,
                    machine_reconciled: true,
                    session_capabilities: vec!["pilot_v1".into()],
                },
                Uuid::now_v7()
            )
            .await
            .unwrap(),
            DispatchOutcome::Dispatched { .. }
        ));
        i
    }
}
pub fn started(id: Uuid, kind: &str) -> Body {
    Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        kind: kind.into(),
        agent: "Explore".into(),
        model: "synthetic-small".into(),
        tool_name: "Read".into(),
        summary: "task".into(),
        ..Default::default()
    })
}
pub fn accepted(command: Uuid, ms: i64) -> Body {
    Body::InputAccepted(InputAccepted {
        command_id: command.to_string(),
        dispatch_attempt: 1,
        machine_time_ms: ms as u64,
    })
}
pub fn finished(r: &Run, command: Uuid, ms: i64) -> Body {
    Body::RunFinished(RunFinished {
        execution_id: r.execution.execution_id.to_string(),
        input_command_id: command.to_string(),
        duration_ms: 1000,
        machine_time_ms: ms as u64,
        context_tokens: 123,
        context_window_tokens: 200000,
        usage: vec![ModelUsage {
            model: "synthetic-small".into(),
            agent: "Explore".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 3,
            cache_write_tokens: 4,
        }],
    })
}
pub fn observation(used: u32, ms: i64) -> LimitObservation {
    LimitObservation {
        provider: "claude".into(),
        window: LimitWindow::Weekly as i32,
        used_permille: used,
        observed_machine_ms: ms as u64,
        resets_at_ms: 0,
    }
}
pub fn report() -> MachineReport {
    MachineReport {
        worker_version: "test".into(),
        allowed_roots: vec!["/tmp".into()],
        harnesses: vec![HarnessInfo {
            harness: "synthetic".into(),
            provider: "claude".into(),
            title: "Тестовый".into(),
            models: vec![
                ModelInfo {
                    id: "synthetic-small".into(),
                    title: "Малая".into(),
                    efforts: vec!["low".into()],
                },
                ModelInfo {
                    id: "synthetic-large".into(),
                    title: "Большая".into(),
                    efforts: vec!["high".into()],
                },
            ],
            ..Default::default()
        }],

        max_attachment_bytes: canterlot_proto::limits::MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: canterlot_proto::limits::MAX_ATTACHMENTS_PER_INPUT as u32,
        containment: "process_group".into(),
        os: "linux".into(),
    }
}
pub fn metrics(ms: u64) -> MachineMetrics {
    MachineMetrics {
        machine_time_ms: ms,
        cpu_cores: 4,
        cpu_permille: 200,
        mem_total_bytes: 1000,
        mem_available_bytes: 400,
        disks: vec![DiskUsage {
            label: "data".into(),
            total_bytes: 1000,
            free_bytes: 400,
        }],
        ..Default::default()
    }
}
pub async fn count(pool: &PgPool, query: &'static str) -> i64 {
    sqlx::query_scalar(query).fetch_one(pool).await.unwrap()
}
pub const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
pub struct Client {
    pub tx: tokio::sync::mpsc::Sender<ControlUp>,
    pub stream: tonic::Streaming<ControlDown>,
    pub connection: Uuid,
}
pub async fn connect(
    hub: &testkit::TestHub,
    seed: &Seed,
    journal: Uuid,
    pilot: bool,
    clock: u64,
) -> Client {
    use tokio_stream::wrappers::ReceiverStream;
    use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};
    let channel = Endpoint::from_shared(format!("https://{}", hub.addr))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&hub.pki.ca_pem))
                .domain_name("localhost"),
        )
        .unwrap()
        .connect()
        .await
        .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let connection = Uuid::now_v7();
    tx.send(ControlUp {
        msg: Some(control_up::Msg::Hello(Hello {
            installation_id: hub.installation.installation_id.to_string(),
            restore_generation: hub.installation.restore_generation.to_string(),
            machine_id: seed.machine_id.to_string(),
            connection_id: connection.to_string(),
            cj_journal_id: journal.to_string(),
            boot_id: Uuid::now_v7().to_string(),
            protocol_version: 1,
            stream_kind: StreamKind::Control as i32,
            optional_capabilities: if pilot {
                vec!["pilot_v1".into()]
            } else {
                vec![]
            },
            machine_time_ms: clock,
            ..Default::default()
        })),
    })
    .await
    .unwrap();
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    // Тестовая учётная запись использует UUID машины как токен.
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", seed.machine_id).parse().unwrap(),
    );
    let mut client = worker_control_client::WorkerControlClient::new(channel);
    let mut stream = client.control(req).await.unwrap().into_inner();
    let down = tokio::time::timeout(WAIT, stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Some(control_down::Msg::HelloReply(HelloReply {
        result: Some(hello_reply::Result::Accepted(a)),
    })) = down.msg
    else {
        panic!("expected accepted")
    };
    assert_eq!(a.capabilities.contains(&"pilot_v1".into()), pilot);
    Client {
        tx,
        stream,
        connection,
    }
}
pub async fn wire_seed(pool: &PgPool) -> Seed {
    use sha2::{Digest, Sha256};
    let s = testkit::seed(pool).await;
    sqlx::query("update machine_credentials set token_sha256 = $2 where machine_id = $1")
        .bind(s.machine_id)
        .bind(Sha256::digest(s.machine_id.to_string().as_bytes()).to_vec())
        .execute(pool)
        .await
        .unwrap();
    s
}
pub async fn barrier(c: &mut Client) {
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::CjRecords(JournalRecords {
            journal_id: String::new(),
            records: vec![],
        })),
    })
    .await
    .unwrap();
}
pub async fn wait_until(pool: &PgPool, query: &'static str) {
    tokio::time::timeout(WAIT, async {
        loop {
            if count(pool, query).await > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
pub fn registry() -> canterlot_hub::session::Registry {
    canterlot_hub::session::Registry::new(HubTimers::default())
}
