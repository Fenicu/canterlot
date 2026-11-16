//! Служебные строки, которые не будят оркестратор: приём делегата и старт из
//! очереди запусков видны в ленте, но едут агенту приставкой к следующему вводу.

use std::time::Duration;

use canterlot_hub::{
    activity::Thresholds,
    config::{ActivityConfig, LimitsConfig},
    delivery::{
        self, ContentBlock, SubmitInput,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
    },
    executions,
    model::ExecutionState,
    scheduler::{ClaimedJob, JobKind},
    session_tools::jobs,
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed(pool: &PgPool) -> Seed {
    let seed = testkit::seed(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    seed
}

async fn start(pool: &PgPool, seed: &Seed) -> executions::StartedExecution {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

fn accepted_origin(child: Uuid) -> Value {
    json!({
        "kind": "event",
        "from": child,
        "chain_depth": 0,
        "event": {"kind": "accepted", "session_id": child, "worktree": "removed"},
    })
}

fn started_origin(child: Uuid) -> Value {
    json!({
        "kind": "event",
        "chain_depth": 0,
        "event": {"kind": "spawn_queue", "state": "started", "child_id": child},
    })
}

fn failed_origin() -> Value {
    json!({
        "kind": "event",
        "chain_depth": 0,
        "event": {"kind": "spawn_queue", "state": "failed", "detail": "rate"},
    })
}

/// Задание заметки тем же путём, каким его ставит приём и очередь запусков.
async fn note(pool: &PgPool, seed: &Seed, origin: Value, text: &str) -> Uuid {
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: seed.owner_id,
        conversation_id: Some(seed.conversation_id),
        run_at: Utc::now(),
        payload: json!({"action": "note", "text": text, "origin": origin}),
        attempts: 0,
    };
    let thresholds = Thresholds::from(&ActivityConfig::default());
    let mut tx = pool.begin().await.unwrap();
    let outcome = jobs::run_in_tx(
        &mut tx,
        &job,
        Utc::now(),
        Uuid::now_v7(),
        None,
        &Default::default(),
        &thresholds,
        None,
        None,
        &LimitsConfig::default(),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let canterlot_hub::crunch::JobOutcome::Done(value) = outcome else {
        panic!("note job did not finish: {outcome:?}");
    };
    value["input_id"].as_str().unwrap().parse().unwrap()
}

async fn owner_input(pool: &PgPool, seed: &Seed) -> delivery::SubmittedInput {
    delivery::submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "owner:one".to_owned(),
            content: vec![ContentBlock::Text {
                text: "собери релиз".to_owned(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
}

async fn dispatch(pool: &PgPool, seed: &Seed) -> DispatchOutcome {
    dispatch_next(
        pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: Vec::new(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_accepted_event_is_a_quiet_feed_line_and_an_input_prefix(pool: PgPool) {
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let child = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let text = format!(
        "[canterlot] Session \"Делегат\" (id {child}, synthetic, model m) was accepted: it is stopped and archived."
    );
    let note = note(&pool, &seed, accepted_origin(child), &text).await;

    let (state, service, feed, attached): (String, bool, Option<i64>, Option<Uuid>) =
        sqlx::query_as(
            "select state,service_note,feed_seq,note_attached_to from inputs where id=$1",
        )
        .bind(note)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "accepted");
    assert!(service);
    assert!(feed.is_some(), "строка видна в ленте");
    assert_eq!(attached, None);
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1
           and state in ('queued','dispatched','on_machine')",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 0, "служебная строка не начинает ход");
    let runs: i64 = sqlx::query_scalar("select count(*) from runs where conversation_id=$1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    let feed_line: bool = sqlx::query_scalar(
        "select exists(select 1 from events where kind='input_queued'
                        and payload->>'input_id'=$1::text)",
    )
    .bind(note)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(feed_line, "лента получает строку приёма");

    let owner = owner_input(&pool, &seed).await;
    let outcome = dispatch(&pool, &seed).await;
    let DispatchOutcome::Dispatched { command_id, .. } = outcome else {
        panic!("owner input did not dispatch: {outcome:?}");
    };
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let content = payload["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    assert_eq!(content[0]["text"], text);
    assert_eq!(content[1]["text"], "собери релиз");
    let owner_content: Value = sqlx::query_scalar("select content from inputs where id=$1")
        .bind(owner.input_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner_content[0]["text"], "собери релиз", "лента чистая");
    let attached: Option<Uuid> =
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attached, Some(owner.input_id));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_started_queued_spawn_is_quiet(pool: PgPool) {
    let seed = seed(&pool).await;
    let child = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let note = note(
        &pool,
        &seed,
        started_origin(child),
        "[canterlot] Queued spawn q-00000000 started as session \"Делегат\".",
    )
    .await;
    let (state, service): (String, bool) =
        sqlx::query_as("select state,service_note from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "accepted");
    assert!(service);
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and state='queued'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_queued_spawn_still_wakes_the_orchestrator(pool: PgPool) {
    let seed = seed(&pool).await;
    let note = note(
        &pool,
        &seed,
        failed_origin(),
        "[canterlot] Queued spawn q-00000000 failed: rate. Nothing was started.",
    )
    .await;
    let (state, service): (String, bool) =
        sqlx::query_as("select state,service_note from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "queued", "сбой очереди по-прежнему будит");
    assert!(!service);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_note_that_arrives_while_the_input_waits_rides_with_it(pool: PgPool) {
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let owner = owner_input(&pool, &seed).await;
    let note = note(
        &pool,
        &seed,
        accepted_origin(Uuid::now_v7()),
        "[canterlot] Session \"Делегат\" was accepted.",
    )
    .await;
    let outcome = dispatch(&pool, &seed).await;
    let DispatchOutcome::Dispatched { command_id, .. } = outcome else {
        panic!("queued owner input did not dispatch: {outcome:?}");
    };
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let content = payload["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    assert_eq!(
        content[0]["text"],
        "[canterlot] Session \"Делегат\" was accepted."
    );
    assert_eq!(content[1]["text"], "собери релиз");
    let attached: Option<Uuid> =
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attached, Some(owner.input_id));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_quiet_note_does_not_block_its_own_dispatch(pool: PgPool) {
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let note = note(
        &pool,
        &seed,
        accepted_origin(Uuid::now_v7()),
        "[canterlot] Session \"Делегат\" was accepted.",
    )
    .await;
    let owner = owner_input(&pool, &seed).await;
    let outcome = dispatch(&pool, &seed).await;
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { input_id, .. } if input_id == owner.input_id),
        "{outcome:?} for note {note}"
    );
}
