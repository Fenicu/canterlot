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
async fn inline_final_without_deltas_raises_no_checksum_mismatch(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![start(id), final_fact(id, 1, "целиком в финале")])
        .await;
    assert_eq!(block(&pool, id).await.2, "final");
    let seen = events(&pool, id).await;
    assert_eq!(kinds(&seen), ["block_started", "block_final"]);
    Ok(())
}

// Супервизор открывает блок ревизией 1, поэтому ревизия не говорит, были ли дельты.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn inline_final_after_a_supervisor_start_raises_no_checksum_mismatch(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    let Body::BlockStarted(mut opened) = start(id) else {
        unreachable!()
    };
    opened.rev = 1;
    run.send(vec![
        Body::BlockStarted(opened),
        final_fact(id, 2, "bad\n\nExited with code 3"),
    ])
    .await;
    assert_eq!(block(&pool, id).await.2, "final");
    assert!(
        !events(&pool, id)
            .await
            .iter()
            .any(|e| e.0 == "block_checksum_mismatch")
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

const NUL_TEXT: &str = "a\0b \u{fffd} c\0";
const NUL_STORED: &str = "a\u{fffd}b \u{fffd} c\u{fffd}";

fn omitted_final(id: Uuid, rev: u64, checksum: String) -> Body {
    Body::BlockFinal(BlockFinal {
        block_id: id.to_string(),
        rev,
        checksum,
        text_omitted: true,
        ..Default::default()
    })
}
async fn stored_checksum(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("select checksum from blocks where block_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
fn kinds(events: &[(String, i64, Value)]) -> Vec<&str> {
    events.iter().map(|event| event.0.as_str()).collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_in_deltas_and_final_is_replaced_and_keeps_the_checksum_valid(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    let (head, tail) = NUL_TEXT.split_at(2);
    run.send(vec![
        start(id),
        delta(id, 0, 1, head),
        delta(id, 1, 2, tail),
        final_fact(id, 3, NUL_TEXT),
    ])
    .await;
    let row = block(&pool, id).await;
    assert_eq!((row.0.as_str(), row.2.as_str()), (NUL_STORED, "final"));
    assert_eq!(
        stored_checksum(&pool, id).await.as_deref(),
        Some(hash(NUL_STORED).as_str())
    );
    let events = events(&pool, id).await;
    assert_eq!(
        kinds(&events),
        ["block_started", "block_delta", "block_delta", "block_final"]
    );
    let sent: String = events[1..3]
        .iter()
        .map(|event| event.2["append"].as_str().unwrap())
        .collect();
    assert_eq!(sent, NUL_STORED);
    assert_eq!(events[3].2["text"], NUL_STORED);
    assert_eq!(events[3].2["checksum"], hash(NUL_STORED));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_in_a_final_without_deltas_is_replaced(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![start(id), final_fact(id, 1, NUL_TEXT)]).await;
    let row = block(&pool, id).await;
    assert_eq!((row.0.as_str(), row.2.as_str()), (NUL_STORED, "final"));
    assert_eq!(
        stored_checksum(&pool, id).await.as_deref(),
        Some(hash(NUL_STORED).as_str())
    );
    Ok(())
}

// Текст крупнее финала хаб знает только по дельтам, а воркер считал сумму по оригиналу.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn omitted_final_of_nul_deltas_is_verified_against_the_original_text(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        delta(id, 0, 1, NUL_TEXT),
        omitted_final(id, 2, hash(NUL_TEXT)),
    ])
    .await;
    let row = block(&pool, id).await;
    assert_eq!((row.0.as_str(), row.2.as_str()), (NUL_STORED, "final"));
    assert_eq!(
        stored_checksum(&pool, id).await.as_deref(),
        Some(hash(NUL_STORED).as_str())
    );
    let events = events(&pool, id).await;
    assert_eq!(
        kinds(&events),
        ["block_started", "block_delta", "block_final"]
    );
    assert_eq!(events[2].2["checksum"], hash(NUL_STORED));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wrong_checksum_over_nul_text_is_still_a_mismatch(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let inline = Uuid::now_v7();
    let Body::BlockFinal(mut fact) = final_fact(inline, 2, NUL_TEXT) else {
        unreachable!()
    };
    fact.checksum = hash("другой текст");
    run.send(vec![
        start(inline),
        delta(inline, 0, 1, NUL_TEXT),
        Body::BlockFinal(fact),
    ])
    .await;
    assert_eq!(block(&pool, inline).await.2, "final");
    assert!(kinds(&events(&pool, inline).await).contains(&"block_checksum_mismatch"));

    let omitted = Uuid::now_v7();
    run.send(vec![
        start(omitted),
        delta(omitted, 0, 1, NUL_TEXT),
        omitted_final(omitted, 2, hash("другой текст")),
    ])
    .await;
    let row = block(&pool, omitted).await;
    assert_eq!((row.0.as_str(), row.2.as_str()), (NUL_STORED, "incomplete"));
    assert!(kinds(&events(&pool, omitted).await).contains(&"block_checksum_mismatch"));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn repeated_final_of_nul_text_is_recognised_as_the_same_final(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let inline = Uuid::now_v7();
    run.send(vec![
        start(inline),
        delta(inline, 0, 1, NUL_TEXT),
        final_fact(inline, 2, NUL_TEXT),
        final_fact(inline, 2, NUL_TEXT),
    ])
    .await;
    let omitted = Uuid::now_v7();
    run.send(vec![
        start(omitted),
        delta(omitted, 0, 1, NUL_TEXT),
        omitted_final(omitted, 2, hash(NUL_TEXT)),
        omitted_final(omitted, 2, hash(NUL_TEXT)),
    ])
    .await;
    for id in [inline, omitted] {
        assert!(!kinds(&events(&pool, id).await).contains(&"block_conflicting_final"));
    }
    run.send(vec![final_fact(inline, 2, "a\0другое")]).await;
    assert!(kinds(&events(&pool, inline).await).contains(&"block_conflicting_final"));
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_approved_by_mark_is_not_stored_or_broadcast(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        rev: 0,
        kind: "tool_call".into(),
        tool_name: "Bash".into(),
        approved_by: "x".repeat(100_000),
        ..Default::default()
    })])
    .await;
    let stored: Option<String> =
        sqlx::query_scalar("select approved_by from blocks where block_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(stored, None);
    let ev = events(&pool, id).await;
    assert_eq!(ev[0].0, "block_started");
    assert!(ev[0].2["approved_by"].is_null(), "{}", ev[0].2);
    // Ограничение схемы: только известная отметка.
    let rejected = sqlx::query("update blocks set approved_by='owner' where block_id=$1")
        .bind(id)
        .execute(&pool)
        .await;
    assert!(
        rejected.is_err(),
        "blocks.approved_by accepts arbitrary text"
    );
    Ok(())
}

// После падения новое исполнение читает хвост истории и шлёт те же идентификаторы. Хаб их не
// задваивает, а результат инструмента садится под вызов прежнего исполнения: разговор один.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resumed_execution_repeats_nothing_and_closes_the_old_call(pool: PgPool) -> TestResult {
    let mut first = Run::new(&pool).await;
    let text = Uuid::now_v7();
    let call = Uuid::now_v7();
    first
        .send(vec![
            start(text),
            final_fact(text, 2, "готово"),
            Body::BlockStarted(BlockStarted {
                block_id: call.to_string(),
                rev: 1,
                kind: "tool_call".into(),
                ..Default::default()
            }),
        ])
        .await;
    for (state, outcome) in [
        (ExecutionState::OwnerLost, None),
        (ExecutionState::Failed, Some("supervisor_died")),
    ] {
        executions::set_state(
            &pool,
            first.execution.execution_id,
            state,
            outcome,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }
    let before = events(&pool, text).await.len();
    let execution = executions::start(
        &pool,
        first.seed.owner_id,
        first.seed.conversation_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let mut second = Run {
        seed: first.seed,
        execution,
        journal: Uuid::now_v7(),
        ingest: Ingest::new(pool.clone()),
        seq: 0,
    };
    let result = Uuid::now_v7();
    second
        .send(vec![
            Body::JournalOpened(JournalOpened {
                execution_id: second.execution.execution_id.to_string(),
                generation: second.execution.generation as u64,
                supervisor_protocol_version: 1,
            }),
            start(text),
            final_fact(text, 2, "готово"),
            Body::BlockStarted(BlockStarted {
                block_id: result.to_string(),
                rev: 1,
                kind: "tool_result".into(),
                parent_block_id: call.to_string(),
                ..Default::default()
            }),
            final_fact(result, 2, "прерван"),
        ])
        .await;
    assert_eq!(block(&pool, text).await.0, "готово");
    assert_eq!(
        events(&pool, text).await.len(),
        before,
        "nothing is repeated"
    );
    let parent: Option<Uuid> =
        sqlx::query_scalar("select parent_block_id from blocks where block_id=$1")
            .bind(result)
            .fetch_optional(&pool)
            .await?
            .flatten();
    assert_eq!(parent, Some(call), "the result is not lost");
    assert_eq!(block(&pool, result).await.0, "прерван");
    Ok(())
}

async fn chunks(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from block_chunks where block_id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

// Дельты нужны, только пока блок пишется: потом текст лежит в blocks.content.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sealed_blocks_keep_no_chunks(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let [done, omitted, broken, open] = [(); 4].map(|_| Uuid::now_v7());
    run.send(vec![
        start(done),
        delta(done, 0, 1, "раз"),
        delta(done, 1, 2, " два"),
        final_fact(done, 3, "раз два"),
        start(omitted),
        delta(omitted, 0, 1, "длинный"),
        omitted_final(omitted, 2, hash("длинный")),
        start(broken),
        delta(broken, 0, 1, "обрыв"),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: broken.to_string(),
            rev: 2,
            reason: "interrupted".into(),
        }),
        start(open),
        delta(open, 0, 1, "пишется"),
    ])
    .await;
    for id in [done, omitted, broken] {
        assert_eq!(chunks(&pool, id).await, 0);
    }
    assert_eq!(block(&pool, done).await.0, "раз два");
    assert_eq!(block(&pool, omitted).await.0, "длинный");
    assert_eq!(block(&pool, broken).await.0, "обрыв");
    assert_eq!(chunks(&pool, open).await, 1);
    assert_eq!(block(&pool, open).await.0, "пишется");
    Ok(())
}

// Сумму оригинала с NUL хаб помнит и без дельт: поздний финал после обрыва проверяется по ней.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn omitted_nul_final_after_incomplete_is_verified_without_chunks(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![
        start(id),
        delta(id, 0, 1, NUL_TEXT),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: id.to_string(),
            rev: 1,
            reason: "interrupted".into(),
        }),
    ])
    .await;
    assert_eq!(chunks(&pool, id).await, 0);
    run.send(vec![omitted_final(id, 2, hash(NUL_TEXT))]).await;
    let row = block(&pool, id).await;
    assert_eq!((row.0.as_str(), row.2.as_str()), (NUL_STORED, "final"));
    assert!(!kinds(&events(&pool, id).await).contains(&"block_checksum_mismatch"));
    Ok(())
}

// Дельты блоков, закрытых до исправления, убираются пачками; пишущийся блок не трогается.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purge_sealed_chunks_removes_leftovers_in_batches(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let [plain, nul, open] = [(); 3].map(|_| Uuid::now_v7());
    run.send(vec![
        start(plain),
        delta(plain, 0, 1, "текст"),
        final_fact(plain, 2, "текст"),
        start(nul),
        delta(nul, 0, 1, NUL_TEXT),
        omitted_final(nul, 2, hash(NUL_TEXT)),
        start(open),
        delta(open, 0, 1, "пишется"),
    ])
    .await;
    // Состояние до исправления: дельты остались, суммы оригинала нет.
    sqlx::query("update blocks set raw_checksum = null")
        .execute(&pool)
        .await?;
    sqlx::query("insert into block_chunks (block_id, seq, text) values ($1, 1, 'текст')")
        .bind(plain)
        .execute(&pool)
        .await?;
    sqlx::query("insert into block_chunks (block_id, seq, text, raw) values ($1, 1, $2, $3)")
        .bind(nul)
        .bind(NUL_STORED)
        .bind(NUL_TEXT.as_bytes())
        .execute(&pool)
        .await?;
    let purged = canterlot_hub::blocks::purge_sealed_chunks(&pool, 1).await?;
    assert_eq!(purged, 2);
    assert_eq!(
        (chunks(&pool, plain).await, chunks(&pool, nul).await),
        (0, 0)
    );
    assert_eq!(chunks(&pool, open).await, 1);
    assert_eq!(
        canterlot_hub::blocks::purge_sealed_chunks(&pool, 1).await?,
        0
    );
    run.send(vec![omitted_final(nul, 2, hash(NUL_TEXT))]).await;
    assert!(!kinds(&events(&pool, nul).await).contains(&"block_conflicting_final"));
    Ok(())
}

/// Блок в 1 МБ закрывается: индекс поиска берёт только первые 32768 символов, и
/// `to_tsvector` не упирается в свой предел 1 МБ внутри транзакции закрытия.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn megabyte_text_block_finalizes_and_is_indexed(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    let piece = "слово word ".repeat(1000);
    let mut bodies = vec![start(id)];
    let mut text = String::new();
    let mut rev = 0;
    while text.len() < 1024 * 1024 {
        bodies.push(delta(id, rev, rev + 1, &piece));
        text.push_str(&piece);
        rev += 1;
    }
    bodies.push(Body::BlockFinal(BlockFinal {
        block_id: id.to_string(),
        rev: rev + 1,
        text: String::new(),
        checksum: hash(&text),
        text_omitted: true,
        ..Default::default()
    }));
    // Пачка журнала ограничена по размеру: дельты идут несколькими пачками.
    while !bodies.is_empty() {
        let rest = bodies.split_off(bodies.len().min(8));
        run.send(bodies).await;
        bodies = rest;
    }
    let (stored, _, state, _) = block(&pool, id).await;
    assert_eq!(state, "final");
    assert_eq!(stored.len(), text.len());
    let (chunks, finished): (i64, bool) = sqlx::query_as(
        "select (select count(*) from block_chunks where block_id = $1),
                finished_at is not null from blocks where block_id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert_eq!((chunks, finished), (0, true));
    let found: bool = sqlx::query_scalar(
        "select canterlot_block_doc(content) @@ websearch_to_tsquery('russian', 'слова')
         from blocks where block_id = $1 and kind = 'text' and state <> 'streaming'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert!(found);
    Ok(())
}
