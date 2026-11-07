//! Починка удержанных выдач терминальных вводов.
//!
//! Миграция 0071 завершает `deliver_input`, которые навсегда остались в
//! `held`, хотя их ввод уже принят, отменён или отвергнут. Проверяется, что
//! она трогает ровно такие строки, идемпотентна и не задевает очередь.

use canterlot_hub::{
    delivery::submit::{ContentBlock, SubmitInput, submit_input},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const MIGRATION: &str = include_str!("../migrations/0071_close_held_deliveries.sql");

async fn input(pool: &PgPool, seed: &Seed, text: &str) -> (Uuid, Uuid) {
    let submitted = submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text { text: text.into() }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    (submitted.input_id, submitted.command_id)
}

async fn set_input_state(pool: &PgPool, input_id: Uuid, state: &str) {
    sqlx::query("update inputs set state = $2 where id = $1")
        .bind(input_id)
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
}

#[derive(sqlx::FromRow)]
struct Command {
    delivery_state: String,
    last_status: Option<String>,
    last_status_rank: i32,
    result: Option<Value>,
    completed_at: Option<chrono::DateTime<Utc>>,
}

async fn command(pool: &PgPool, id: Uuid) -> Command {
    sqlx::query_as(
        "select delivery_state, last_status, last_status_rank, result, completed_at
         from commands where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
struct SnapshotRow {
    id: Uuid,
    delivery_state: String,
    last_status_rank: i32,
    result: Option<Value>,
    completed_at: Option<chrono::DateTime<Utc>>,
}

#[sqlx::test(migrations = "./migrations")]
async fn closes_only_held_deliveries_of_terminal_inputs(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let (accepted_id, accepted_cmd) = input(&pool, &seed, "accepted").await;
    let (cancelled_id, cancelled_cmd) = input(&pool, &seed, "cancelled").await;
    let (rejected_id, rejected_cmd) = input(&pool, &seed, "rejected").await;
    let (queued_id, queued_cmd) = input(&pool, &seed, "queued").await;
    let (done_id, done_cmd) = input(&pool, &seed, "done").await;

    set_input_state(&pool, accepted_id, "accepted").await;
    set_input_state(&pool, cancelled_id, "cancelled").await;
    set_input_state(&pool, rejected_id, "rejected").await;
    set_input_state(&pool, done_id, "accepted").await;
    // Уже завершённая выдача: миграция не должна переписывать её итог.
    let done_at = Utc::now() - chrono::TimeDelta::days(1);
    let done_at: chrono::DateTime<Utc> = sqlx::query_scalar(
        "update commands set delivery_state = 'completed', completed_at = $2,
                last_status = 'accepted', last_status_rank = 3, result = $3
         where id = $1 returning completed_at",
    )
    .bind(done_cmd)
    .bind(done_at)
    .bind(json!({"kept": true}))
    .fetch_one(&pool)
    .await?;
    // Удержанное вливание не является выдачей и остаётся как есть.
    let held_steer = Uuid::now_v7();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, kind, payload,
                               target_command_id, delivery_state)
         values ($1, $2, $3, 'steer_input', $4, $5, 'held')",
    )
    .bind(held_steer)
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .bind(json!({"input_id": accepted_id}))
    .bind(accepted_cmd)
    .execute(&pool)
    .await?;

    sqlx::raw_sql(MIGRATION).execute(&pool).await?;

    let accepted = command(&pool, accepted_cmd).await;
    assert_eq!(accepted.delivery_state, "completed");
    assert!(accepted.completed_at.is_some());
    assert_eq!(accepted.last_status.as_deref(), Some("accepted"));
    assert_eq!(accepted.last_status_rank, 3);
    assert_eq!(accepted.result, Some(json!({"superseded": "accepted"})));

    let cancelled = command(&pool, cancelled_cmd).await;
    assert_eq!(cancelled.delivery_state, "completed");
    assert!(cancelled.completed_at.is_some());
    assert_eq!(cancelled.last_status.as_deref(), Some("cancel_won"));
    assert_eq!(cancelled.last_status_rank, 3);
    assert_eq!(cancelled.result, Some(json!({"cancelled": true})));

    let rejected = command(&pool, rejected_cmd).await;
    assert_eq!(rejected.delivery_state, "completed");
    assert!(rejected.completed_at.is_some());
    assert_eq!(rejected.last_status.as_deref(), Some("explicit_rejection"));
    assert_eq!(rejected.result, Some(json!({"superseded": "rejected"})));

    // Ввод в очереди всё ещё ждёт обычной выдачи.
    let queued = command(&pool, queued_cmd).await;
    assert_eq!(queued.delivery_state, "held");
    assert!(queued.completed_at.is_none());
    assert!(queued.result.is_none());
    let queued_state: String = sqlx::query_scalar("select state from inputs where id = $1")
        .bind(queued_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(queued_state, "queued");

    let done = command(&pool, done_cmd).await;
    assert_eq!(done.completed_at, Some(done_at));
    assert_eq!(done.result, Some(json!({"kept": true})));

    let steer = command(&pool, held_steer).await;
    assert_eq!(steer.delivery_state, "held");
    assert!(steer.completed_at.is_none());

    // Повтор ничего не меняет: строка уже завершена и условие не совпадает.
    let before: Vec<SnapshotRow> = sqlx::query_as(
        "select id, delivery_state, last_status_rank, result, completed_at
             from commands order by id",
    )
    .fetch_all(&pool)
    .await?;
    sqlx::raw_sql(MIGRATION).execute(&pool).await?;
    let after: Vec<SnapshotRow> = sqlx::query_as(
        "select id, delivery_state, last_status_rank, result, completed_at
             from commands order by id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(after, before);

    Ok(())
}
