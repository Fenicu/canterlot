#![allow(dead_code)]
use canterlot_hub::{
    activity::{self, Thresholds},
    config::SchedulerConfig,
    delivery::{ContentBlock, SubmitInput, submit_input},
    dispatcher::Dispatcher,
    events::{self, NewEvent, Scope},
    scheduler::{self, NewScheduled, ScheduledMessage, Scheduler},
    testkit::{self, Seed},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

pub fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
    }
}
pub fn content() -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: "task5 input".into(),
    }]
}
pub async fn running(pool: &PgPool) -> (Seed, Uuid) {
    let s = testkit::seed(pool).await;
    let e = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state,phase,phase_at,last_event_at) values($1,$2,1,'running','generating',clock_timestamp()-interval '10 minutes',clock_timestamp()-interval '10 minutes')").bind(e).bind(s.conversation_id).execute(pool).await.unwrap();
    sqlx::query("update queue_state set run_slot='running',run_slot_input_id=$2,hold_reasons='[]' where conversation_id=$1").bind(s.conversation_id).bind(Uuid::now_v7()).execute(pool).await.unwrap();
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(s.machine_id)
        .execute(pool)
        .await
        .unwrap();
    (s, e)
}
pub async fn refresh(pool: &PgPool, id: Uuid) -> activity::Refresh {
    activity::refresh(pool, id, Utc::now(), &thresholds(), Uuid::now_v7())
        .await
        .unwrap()
}
pub async fn row(pool: &PgPool, id: Uuid) -> Value {
    sqlx::query_scalar("select to_jsonb(d) from conversation_derived d where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
pub async fn count(pool: &PgPool, query: &'static str) -> i64 {
    sqlx::query_scalar(query).fetch_one(pool).await.unwrap()
}
pub async fn notice(pool: &PgPool, id: Uuid, kind: &'static str) {
    let mut tx = pool.begin().await.unwrap();
    events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Conversation { id },
            kind,
            conversation_id: Some(id),
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
pub fn scheduler(pool: &PgPool, config: SchedulerConfig) -> Scheduler {
    Scheduler::new(pool.clone(), Dispatcher::channel().0, config)
}
pub async fn scheduled(pool: &PgPool, s: &Seed, key: &str, now: DateTime<Utc>) -> ScheduledMessage {
    scheduler::schedule_message(
        pool,
        s.owner_id,
        key,
        NewScheduled {
            conversation_id: s.conversation_id,
            run_at: now + TimeDelta::seconds(2),
            content: content(),
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}
pub async fn input(pool: &PgPool, s: &Seed, key: &str) {
    submit_input(
        pool,
        SubmitInput {
            owner_id: s.owner_id,
            conversation_id: s.conversation_id,
            idempotency_key: key.into(),
            content: content(),
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
}
pub async fn until_state(pool: &PgPool, id: Uuid, wanted: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state: Option<String> = sqlx::query_scalar(
                "select activity_state from conversation_derived where conversation_id=$1",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap();
            if state.as_deref() == Some(wanted) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
