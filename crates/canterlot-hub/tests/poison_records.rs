//! Запись одного исполнения не должна останавливать поток данных, общий для всей машины.

use canterlot_hub::{
    executions,
    ingest::{Ingest, IngestError, IngestOutcome, JournalKind},
    model::ExecutionState as State,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Run {
    seed: Seed,
    execution: executions::StartedExecution,
    journal: Uuid,
    ingest: Ingest,
    seq: u64,
}

impl Run {
    async fn new(pool: &PgPool) -> Self {
        let seed = testkit::seed(pool).await;
        let execution =
            executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        for state in [State::Starting, State::Running] {
            executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        let mut run = Self {
            seed,
            execution,
            journal: Uuid::now_v7(),
            ingest: Ingest::new(pool.clone()),
            seq: 0,
        };
        let opened = run.opened();
        run.send(opened).await.unwrap();
        run
    }

    fn opened(&self) -> Body {
        Body::JournalOpened(JournalOpened {
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
            supervisor_protocol_version: 1,
        })
    }

    fn id(&self) -> String {
        self.execution.execution_id.to_string()
    }

    fn batch(&self, bodies: Vec<Body>) -> JournalRecords {
        JournalRecords {
            journal_id: self.journal.to_string(),
            records: bodies
                .into_iter()
                .zip(self.seq + 1..)
                .map(|(body, seq)| JournalRecord {
                    seq,
                    body: Some(RecordBody { body: Some(body) }),
                })
                .collect(),
        }
    }

    async fn send(&mut self, body: Body) -> Result<IngestOutcome, IngestError> {
        let batch = self.batch(vec![body]);
        let outcome = self
            .ingest
            .apply_batch(
                self.seed.machine_id,
                &self.seed.credential_digest(),
                JournalKind::Sj,
                &batch,
            )
            .await?;
        self.seq += 1;
        Ok(outcome)
    }
}

async fn text(pool: &PgPool, query: &'static str, id: Uuid) -> Option<String> {
    sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_in_any_text_of_a_record_never_refuses_the_record(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = run.id();
    let block = Uuid::now_v7();
    let other = Uuid::now_v7();
    let decision = Uuid::now_v7().to_string();
    let request = "request\0id".to_owned();
    let facts = vec![
        (
            "execution_state",
            Body::ExecutionState(ExecutionState {
                execution_id: id.clone(),
                generation: run.execution.generation as u64,
                state: "running".into(),
                outcome: "out\0come".into(),
                native_conversation_id: "native\0id".into(),
            }),
        ),
        (
            "phase_changed",
            Body::PhaseChanged(PhaseChanged {
                execution_id: id.clone(),
                phase: "pha\0se".into(),
                tool_name: "to\0ol".into(),
                summary: "sum\0mary".into(),
            }),
        ),
        (
            "block_started",
            Body::BlockStarted(BlockStarted {
                block_id: block.to_string(),
                rev: 0,
                kind: "tool_result".into(),
                agent: "ag\0ent".into(),
                model: "mo\0del".into(),
                tool_name: "to\0ol".into(),
                summary: "sum\0mary".into(),
                ..Default::default()
            }),
        ),
        (
            "block_final",
            Body::BlockFinal(BlockFinal {
                block_id: block.to_string(),
                rev: 1,
                text: "te\0xt".into(),
                checksum: "check\0sum".into(),
                summary: "sum\0mary".into(),
                ..Default::default()
            }),
        ),
        (
            "block_started_other",
            Body::BlockStarted(BlockStarted {
                block_id: other.to_string(),
                rev: 0,
                kind: "text".into(),
                ..Default::default()
            }),
        ),
        (
            "block_incomplete",
            Body::BlockIncomplete(BlockIncomplete {
                block_id: other.to_string(),
                rev: 1,
                reason: "rea\0son".into(),
            }),
        ),
        (
            "adapter_diagnostic",
            Body::AdapterDiagnostic(AdapterDiagnostic {
                execution_id: id.clone(),
                kind: "ki\0nd".into(),
                event_type: "ty\0pe".into(),
            }),
        ),
        (
            "request_opened",
            Body::RequestOpened(RequestOpened {
                request_id: request.clone(),
                execution_id: id.clone(),
                generation: run.execution.generation as u64,
                kind: "permission".into(),
                payload_json: br#"{"ke\u0000y":["va\u0000lue"]}"#.to_vec(),
            }),
        ),
        (
            "decision_transmission_started",
            Body::DecisionTransmissionStarted(DecisionTransmissionStarted {
                request_id: request.clone(),
                decision_id: decision.clone(),
                machine_time_ms: 1,
            }),
        ),
        (
            "decision_applied",
            Body::DecisionApplied(DecisionApplied {
                request_id: request.clone(),
                decision_id: decision.clone(),
                proof_kind: "pro\0of".into(),
                proof_payload_json: Vec::new(),
            }),
        ),
        (
            "decision_not_applied",
            Body::DecisionNotApplied(DecisionNotApplied {
                request_id: request.clone(),
                decision_id: decision.clone(),
                reason: "rea\0son".into(),
            }),
        ),
        (
            "decision_unconfirmed",
            Body::DecisionUnconfirmed(DecisionUnconfirmed {
                request_id: request.clone(),
                decision_id: decision,
                reason: "rea\0son".into(),
            }),
        ),
        (
            "request_closed",
            Body::RequestClosed(RequestClosed {
                request_id: request,
                reason: "rea\0son".into(),
            }),
        ),
        (
            "todo_updated",
            Body::TodoUpdated(TodoUpdated {
                execution_id: id.clone(),
                generation: run.execution.generation as u64,
                revision: 1,
                items: vec![TodoItem {
                    text: "пу\0нкт".into(),
                    status: TodoStatus::Pending as i32,
                }],
                ..Default::default()
            }),
        ),
        (
            "journal_closed",
            Body::JournalClosed(JournalClosed {
                execution_id: id,
                generation: run.execution.generation as u64,
                reason: "rea\0son".into(),
            }),
        ),
    ];
    let mut refused = Vec::new();
    for (name, body) in facts {
        if run.send(body).await.is_err() {
            refused.push(name);
        }
    }
    assert_eq!(refused, Vec::<&str>::new());
    let execution = run.execution.execution_id;
    assert_eq!(
        text(
            &pool,
            "select native_conversation_id from executions where id = $1",
            execution
        )
        .await
        .as_deref(),
        Some("native\u{fffd}id")
    );
    assert_eq!(
        text(
            &pool,
            "select native_request_id from requests where execution_id = $1",
            execution
        )
        .await
        .as_deref(),
        Some("request\u{fffd}id")
    );
    assert_eq!(
        text(
            &pool,
            "select state from requests where execution_id = $1",
            execution
        )
        .await
        .as_deref(),
        Some("closed_without_decision")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stub_of_an_oversized_record_is_attributed_to_the_journal(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.send(Body::AdapterDiagnostic(AdapterDiagnostic {
        execution_id: Uuid::nil().to_string(),
        kind: "unknown_optional_event".into(),
        event_type: "record_too_large".into(),
    }))
    .await?;
    let shown: Option<String> = sqlx::query_scalar(
        "select payload->>'event_type' from events
         where execution_id = $1 and kind = 'adapter_diagnostic'",
    )
    .bind(run.execution.execution_id)
    .fetch_optional(&pool)
    .await?;
    assert_eq!(shown.as_deref(), Some("record_too_large"));
    Ok(())
}
