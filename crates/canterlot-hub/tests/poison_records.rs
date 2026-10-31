//! Запись одного исполнения не должна останавливать поток данных, общий для всей машины.

use canterlot_hub::{
    executions,
    grpc::Ingest as _,
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
                ..Default::default()
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
        ..Default::default()
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

fn started(id: Uuid) -> Body {
    Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        rev: 0,
        kind: "text".into(),
        ..Default::default()
    })
}

fn undecodable_request(run: &Run) -> Body {
    Body::RequestOpened(RequestOpened {
        request_id: "request".into(),
        execution_id: run.id(),
        generation: run.execution.generation as u64,
        kind: "permission".into(),
        payload_json: br#"{"text":"\ud83d"}"#.to_vec(),
    })
}

async fn ship(run: &Run, batch: JournalRecords) -> Result<u64, tonic::Status> {
    run.ingest
        .sj_records(run.seed.machine_id, &run.seed.credential_digest(), batch)
        .await
        .map(|ack| ack.up_to_seq)
}

async fn cursor(pool: &PgPool, journal: Uuid) -> i64 {
    sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
        .bind(journal)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn diagnostics(pool: &PgPool, execution: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "select payload->>'event_type' from events where execution_id = $1
         and kind = 'adapter_diagnostic' and payload->>'kind' = 'journal_record_rejected'",
    )
    .bind(execution)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn stop_commands(pool: &PgPool, execution: Uuid) -> Vec<(serde_json::Value, String)> {
    sqlx::query_as(
        "select payload, delivery_state from commands
         where execution_id = $1 and kind = 'stop_execution'",
    )
    .bind(execution)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn fatal_pause(pool: &PgPool, conversation: Uuid) -> bool {
    sqlx::query_scalar(
        "select pause_reasons @> '[{\"reason\":\"fatal_error\"}]'::jsonb
         from queue_state where conversation_id = $1",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap()
}

// Суррогат без пары не проходит ни serde_json, ни jsonb: повтор дал бы тот же отказ.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn refused_record_fails_its_execution_and_the_journal_moves_on(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    let (before, after) = (Uuid::now_v7(), Uuid::now_v7());
    let batch = run.batch(vec![
        started(before),
        undecodable_request(&run),
        started(after),
    ]);
    assert_eq!(ship(&run, batch.clone()).await?, 4);
    assert_eq!(cursor(&pool, run.journal).await, 4);
    let execution = run.execution.execution_id;
    assert_eq!(
        diagnostics(&pool, execution).await,
        ["invalid_record seq=3"]
    );
    assert!(fatal_pause(&pool, run.seed.conversation_id).await);
    let commands = stop_commands(&pool, execution).await;
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(commands[0].0["force"], true);
    let blocks: i64 = sqlx::query_scalar("select count(*) from blocks where block_id = any($1)")
        .bind([before, after])
        .fetch_one(&pool)
        .await?;
    assert_eq!(blocks, 2);
    assert_eq!(run.ingest.ignored_record_count(), 1);

    assert_eq!(ship(&run, batch).await?, 4, "a repeat is a duplicate");
    assert_eq!(diagnostics(&pool, execution).await.len(), 1);
    assert_eq!(stop_commands(&pool, execution).await.len(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn value_refused_by_the_database_is_contained_like_an_invalid_record(
    pool: PgPool,
) -> TestResult {
    let run = Run::new(&pool).await;
    sqlx::raw_sql(
        "create function refuse_block() returns trigger language plpgsql as $$
         begin raise exception 'value' using errcode = '22023'; end $$;
         create trigger refuse_block before insert on blocks
         for each row execute function refuse_block();",
    )
    .execute(&pool)
    .await?;
    let batch = run.batch(vec![started(Uuid::now_v7())]);
    assert_eq!(ship(&run, batch).await?, 2);
    assert_eq!(
        diagnostics(&pool, run.execution.execution_id).await,
        ["rejected_value seq=2"]
    );
    Ok(())
}

// Отказ не из-за значения проходит сам или лечится хабом, а запись потерялась бы навсегда.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn other_database_failures_keep_the_record_for_a_retry(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    sqlx::raw_sql(
        "create function refuse_block() returns trigger language plpgsql as $$
         begin raise exception 'outage'; end $$;
         create trigger refuse_block before insert on blocks
         for each row execute function refuse_block();",
    )
    .execute(&pool)
    .await?;
    let batch = run.batch(vec![started(Uuid::now_v7())]);
    let error = ship(&run, batch.clone()).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(cursor(&pool, run.journal).await, 1);
    let execution = run.execution.execution_id;
    assert!(diagnostics(&pool, execution).await.is_empty());
    assert!(stop_commands(&pool, execution).await.is_empty());
    sqlx::query("drop trigger refuse_block on blocks")
        .execute(&pool)
        .await?;
    assert_eq!(ship(&run, batch).await?, 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn finished_execution_is_not_stopped_again(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    let execution = run.execution.execution_id;
    executions::set_state(&pool, execution, State::Stopping, None, Uuid::now_v7()).await?;
    executions::set_state(
        &pool,
        execution,
        State::Stopped,
        Some("normal_exit"),
        Uuid::now_v7(),
    )
    .await?;
    let batch = run.batch(vec![Body::ExecutionState(ExecutionState {
        execution_id: run.id(),
        generation: run.execution.generation as u64,
        state: "unknown".into(),
        ..Default::default()
    })]);
    assert_eq!(ship(&run, batch).await?, 2);
    assert_eq!(diagnostics(&pool, execution).await.len(), 1);
    assert!(stop_commands(&pool, execution).await.is_empty());
    Ok(())
}

// Без исполнения журнала сбой не к чему приписать.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn journal_without_an_execution_still_refuses_the_batch(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    let batch = JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(started(Uuid::now_v7())),
            }),
        }],
    };
    let error = ship(&run, batch).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    Ok(())
}
