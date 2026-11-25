//! «Отложить»: срок возврата сессии, ранний возврат ради владельца и тишина пушей об итогах.
use crate::support::{task5 as activity_support, task7 as http_support};

use axum::http::Method;
use canterlot_hub::activity::{self};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn snoozed(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select snoozed_until from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn snooze(pool: &PgPool, id: Uuid, until: DateTime<Utc>) {
    sqlx::query("update conversations set snoozed_until=$2 where id=$1")
        .bind(id)
        .bind(until)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_endpoint_sets_and_clears_an_absolute_snooze() {
    let pool = canterlot_hub::testkit::db().await;
    let f = http_support::Fixture::new(pool.clone()).await;
    let until = now() + TimeDelta::hours(3);
    let set = f
        .client
        .send(
            Method::PUT,
            &f.path("/snooze"),
            Some("snooze-a"),
            Some(json!({"until": wire(until)})),
        )
        .await;
    assert_eq!(set.status, 200);
    assert_eq!(snoozed(&pool, f.seed.conversation_id).await, Some(until));
    assert_eq!(set.json::<Value>()["snoozed_until"], wire(until));
    let listed = f.get("/api/v1/conversations").await;
    assert_eq!(listed["items"][0]["snoozed_until"], wire(until));
    let detail = f.get(&f.path("")).await;
    assert_eq!(detail["snoozed_until"], wire(until));

    let clear = f
        .client
        .send(Method::DELETE, &f.path("/snooze"), Some("snooze-b"), None)
        .await;
    assert_eq!(clear.status, 204);
    assert_eq!(snoozed(&pool, f.seed.conversation_id).await, None);
    assert_eq!(f.get(&f.path("")).await["snoozed_until"], Value::Null);
}

#[tokio::test]
async fn a_question_wakes_a_snoozed_session() {
    let pool = canterlot_hub::testkit::db().await;
    let (s, execution) = activity_support::running(&pool).await;
    snooze(&pool, s.conversation_id, now() + TimeDelta::hours(8)).await;
    assert_eq!(
        activity_support::refresh(&pool, s.conversation_id).await,
        activity::Refresh::Changed
    );
    assert!(snoozed(&pool, s.conversation_id).await.is_some());

    sqlx::query("update executions set phase='awaiting_answer' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    activity_support::refresh(&pool, s.conversation_id).await;
    assert_eq!(
        snoozed(&pool, s.conversation_id).await,
        None,
        "вопрос владельцу возвращает сессию"
    );
}
