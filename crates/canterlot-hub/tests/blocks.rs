use canterlot_hub::{
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
        for state in [ExecutionState::Starting, ExecutionState::Running] {
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
        run.send(vec![Body::JournalOpened(JournalOpened {
            execution_id: run.execution.execution_id.to_string(),
            generation: run.execution.generation as u64,
            supervisor_protocol_version: 1,
        })])
        .await;
        run
    }
    async fn send(&mut self, bodies: Vec<Body>) {
        let records = bodies
            .into_iter()
            .map(|body| {
                self.seq += 1;
                JournalRecord {
                    seq: self.seq,
                    body: Some(RecordBody { body: Some(body) }),
                }
            })
            .collect();
        let batch = JournalRecords {
            journal_id: self.journal.to_string(),
            records,
        };
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
}
fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn start(id: Uuid) -> Body {
    Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        rev: 0,
        kind: "text".into(),
        input_command_id: String::new(),
        parent_block_id: String::new(),
        ..Default::default()
    })
}
fn delta(id: Uuid, base: u64, rev: u64, text: &str) -> Body {
    Body::BlockDelta(BlockDelta {
        block_id: id.to_string(),
        base_rev: base,
        rev,
        append: text.into(),
    })
}
fn final_fact(id: Uuid, rev: u64, text: &str) -> Body {
    Body::BlockFinal(BlockFinal {
        block_id: id.to_string(),
        rev,
        text: text.into(),
        checksum: hash(text),
        text_omitted: false,
        ..Default::default()
    })
}
async fn block(pool: &PgPool, id: Uuid) -> (String, i64, String, Option<String>) {
    let mut conn = pool.acquire().await.unwrap();
    let text = canterlot_hub::blocks::read_text(&mut conn, id)
        .await
        .unwrap();
    let (rev, state, reason) =
        sqlx::query_as("select rev, state, incomplete_reason from blocks where block_id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    (text, rev, state, reason)
}
async fn events(pool: &PgPool, id: Uuid) -> Vec<(String, i64, Value)> {
    sqlx::query_as("select kind, scope_seq, payload from events where payload->>'block_id' = $1 order by scope_seq").bind(id.to_string()).fetch_all(pool).await.unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_lifecycle_materializes_text(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        delta(id, 0, 1, "one"),
        delta(id, 1, 2, " two"),
        delta(id, 2, 3, " три"),
        final_fact(id, 4, "one two три"),
    ])
    .await;
    assert_eq!(
        block(&pool, id).await,
        ("one two три".into(), 4, "final".into(), None)
    );
    let ev = events(&pool, id).await;
    assert_eq!(
        ev.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        [
            "block_started",
            "block_delta",
            "block_delta",
            "block_delta",
            "block_final"
        ]
    );
    assert!(ev.windows(2).all(|w| w[1].1 == w[0].1 + 1));
    assert_eq!(ev[1].2["append"], "one");
    assert_eq!(ev[4].2["text"], "one two три");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn duplicate_and_stale_deltas_do_not_change_text(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        delta(id, 0, 1, "a"),
        delta(id, 0, 1, "a"),
        delta(id, 0, 0, "x"),
        start(id),
    ])
    .await;
    assert_eq!(block(&pool, id).await.0, "a");
    assert_eq!(events(&pool, id).await.len(), 2);
    assert_eq!(run.ingest.ignored_record_count(), 3);
    run.send(vec![delta(id, 3, 4, "gap"), delta(id, 0, 2, "overlap")])
        .await;
    assert_eq!(block(&pool, id).await.0, "a");
    assert_eq!(block(&pool, id).await.1, 1);
    assert_eq!(
        events(&pool, id)
            .await
            .iter()
            .filter(|e| e.0 == "block_revision_gap")
            .count(),
        2
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_delta_after_final_is_ignored(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        final_fact(id, 2, "final"),
        delta(id, 2, 3, "late"),
        final_fact(id, 9, "final"),
        final_fact(id, 10, "conflict"),
    ])
    .await;
    assert_eq!(
        block(&pool, id).await,
        ("final".into(), 2, "final".into(), None)
    );
    assert_eq!(run.ingest.ignored_record_count(), 2);
    assert_eq!(
        events(&pool, id)
            .await
            .iter()
            .filter(|e| e.0 == "block_conflicting_final")
            .count(),
        1
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_checksum_mismatch_final_wins_with_diagnostic(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        delta(id, 0, 1, "partial"),
        final_fact(id, 2, "corrected"),
    ])
    .await;
    assert_eq!(block(&pool, id).await.0, "corrected");
    assert_eq!(
        events(&pool, id)
            .await
            .iter()
            .filter(|e| e.0 == "block_checksum_mismatch")
            .count(),
        1
    );
    let other = Uuid::now_v7();
    let Body::BlockFinal(mut fact) = final_fact(other, 1, "body") else {
        unreachable!()
    };
    fact.checksum = hash("different");
    run.send(vec![start(other), Body::BlockFinal(fact)]).await;
    assert_eq!(block(&pool, other).await.2, "final");
    assert_eq!(
        events(&pool, other)
            .await
            .iter()
            .filter(|e| e.0 == "block_checksum_mismatch")
            .count(),
        1
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn oversized_final_uses_checksum_only_and_hub_verifies(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    let piece = "я".repeat(8192);
    let content = piece.repeat(5);
    run.send(vec![start(id)]).await;
    for rev in 1..=5 {
        run.send(vec![delta(id, rev - 1, rev, &piece)]).await;
    }
    run.send(vec![Body::BlockFinal(BlockFinal {
        block_id: id.to_string(),
        rev: 6,
        text: String::new(),
        checksum: hash(&content),
        text_omitted: true,
        ..Default::default()
    })])
    .await;
    let row = block(&pool, id).await;
    assert!(row.0 == content);
    assert_eq!((row.1, row.2.as_str()), (6, "final"));
    assert!(
        !events(&pool, id)
            .await
            .iter()
            .any(|e| e.0 == "block_checksum_mismatch")
    );
    let other = Uuid::now_v7();
    run.send(vec![
        start(other),
        delta(other, 0, 1, "partial"),
        Body::BlockFinal(BlockFinal {
            block_id: other.to_string(),
            rev: 2,
            checksum: hash("missing"),
            text_omitted: true,
            text: String::new(),
            ..Default::default()
        }),
    ])
    .await;
    assert_eq!(block(&pool, other).await.0, "partial");
    assert!(
        events(&pool, other)
            .await
            .iter()
            .any(|e| e.0 == "block_checksum_mismatch")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn journal_closed_marks_streaming_blocks_incomplete_and_late_final_wins(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let c = Uuid::now_v7();
    run.send(vec![
        start(a),
        delta(a, 0, 1, "partial"),
        start(b),
        final_fact(b, 1, "done"),
        start(c),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: c.to_string(),
            rev: 1,
            reason: "interrupted".into(),
        }),
    ])
    .await;
    run.send(vec![
        Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
            execution_id: run.execution.execution_id.to_string(),
            generation: run.execution.generation as u64,
            state: "stopping".into(),
            ..Default::default()
        }),
        Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
            execution_id: run.execution.execution_id.to_string(),
            generation: run.execution.generation as u64,
            state: "stopped".into(),
            outcome: "normal_exit".into(),
            ..Default::default()
        }),
    ])
    .await;
    run.send(vec![Body::JournalClosed(JournalClosed {
        execution_id: run.execution.execution_id.to_string(),
        generation: run.execution.generation as u64,
        reason: "normal_exit".into(),
    })])
    .await;
    assert_eq!(
        block(&pool, a).await,
        (
            "partial".into(),
            1,
            "incomplete".into(),
            Some("normal_exit".into())
        )
    );
    assert_eq!(block(&pool, b).await.2, "final");
    assert_eq!(block(&pool, c).await.3.as_deref(), Some("interrupted"));
    assert!(
        sqlx::query_scalar::<_, bool>("select closed from journal_cursors where journal_id = $1")
            .bind(run.journal)
            .fetch_one(&pool)
            .await?
    );
    run.send(vec![final_fact(a, 2, "complete"), final_fact(c, 2, "late")])
        .await;
    assert_eq!(
        block(&pool, a).await,
        ("complete".into(), 2, "final".into(), None)
    );
    assert_eq!(block(&pool, c).await.2, "final");
    assert_eq!(
        events(&pool, a)
            .await
            .iter()
            .filter(|e| e.0 == "block_incomplete")
            .count(),
        1
    );
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "stopped");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn blocks_of_one_machine_never_affect_another(pool: PgPool) -> TestResult {
    let mut victim = Run::new(&pool).await;
    let mut attacker = Run::new(&pool).await;
    let id = Uuid::now_v7();
    victim.send(vec![start(id), delta(id, 0, 1, "owned")]).await;
    let before = block(&pool, id).await;
    let before_events = events(&pool, id).await.len();
    attacker
        .send(vec![
            start(id),
            delta(id, 1, 2, "foreign"),
            final_fact(id, 3, "foreign"),
            Body::BlockIncomplete(BlockIncomplete {
                block_id: id.to_string(),
                rev: 4,
                reason: "foreign".into(),
            }),
            Body::JournalClosed(JournalClosed {
                execution_id: attacker.execution.execution_id.to_string(),
                generation: attacker.execution.generation as u64,
                reason: "normal_exit".into(),
            }),
        ])
        .await;
    assert_eq!(block(&pool, id).await, before);
    assert_eq!(events(&pool, id).await.len(), before_events);
    assert_eq!(attacker.ingest.ignored_record_count(), 4);
    Ok(())
}

#[test]
fn checksum_is_sha256_of_utf8() {
    assert_eq!(
        canterlot_hub::blocks::checksum(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        canterlot_hub::blocks::checksum("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(canterlot_hub::blocks::checksum("ёж"), hash("ёж"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sj_record_bounds_are_checked_before_applying_any_record(pool: PgPool) -> TestResult {
    use canterlot_hub::ingest::IngestError;
    use canterlot_proto::limits::{
        BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES, MAX_SJ_BATCH_RECORDS,
    };
    let run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    for bodies in [
        vec![
            start(id),
            delta(id, 0, 1, &"x".repeat(MAX_BLOCK_DELTA_BYTES + 1)),
        ],
        vec![
            start(id),
            final_fact(id, 1, &"x".repeat(BLOCK_FINAL_INLINE_MAX_BYTES + 1)),
        ],
        vec![
            Body::CommandStored(CommandStored {
                command_id: Uuid::now_v7().to_string(),
                dispatch_attempt: 0,
                machine_time_ms: 0
            });
            MAX_SJ_BATCH_RECORDS + 1
        ],
    ] {
        let batch = JournalRecords {
            journal_id: run.journal.to_string(),
            records: bodies
                .into_iter()
                .enumerate()
                .map(|(index, body)| JournalRecord {
                    seq: index as u64 + 2,
                    body: Some(RecordBody { body: Some(body) }),
                })
                .collect(),
        };
        assert!(matches!(
            run.ingest
                .apply_batch(
                    run.seed.machine_id,
                    &run.seed.credential_digest(),
                    JournalKind::Sj,
                    &batch
                )
                .await,
            Err(IngestError::BatchLimit)
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "select committed_seq from journal_cursors where journal_id = $1"
            )
            .bind(run.journal)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from blocks")
                .fetch_one(&pool)
                .await?,
            0
        );
    }
    Ok(())
}

fn start_approved(id: Uuid) -> Body {
    Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        rev: 0,
        kind: "tool_call".into(),
        tool_name: "Bash".into(),
        approved_by: "cli_policy".into(),
        ..Default::default()
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cli_policy_mark_survives_ingest_to_block_json(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![start_approved(id)]).await;
    let mut conn = pool.acquire().await?;
    let stored: Option<String> =
        sqlx::query_scalar("select approved_by from blocks where block_id=$1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    assert_eq!(stored.as_deref(), Some("cli_policy"));
    let ev = events(&pool, id).await;
    assert_eq!(ev[0].0, "block_started");
    assert_eq!(ev[0].2["approved_by"], "cli_policy");
    let page = canterlot_hub::history::page(
        &mut conn,
        run.seed.owner_id,
        run.seed.conversation_id,
        canterlot_hub::history::HistoryCursor::Tail,
        10,
    )
    .await?
    .expect("history page");
    let view = page
        .items
        .iter()
        .find_map(|item| match item {
            canterlot_hub::history::TimelineItem::Block(view) if view.id == id => Some(view),
            _ => None,
        })
        .expect("tool call block in snapshot");
    assert_eq!(view.approved_by.as_deref(), Some("cli_policy"));
    Ok(())
}
