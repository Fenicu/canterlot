#[path = "support/task7.rs"]
mod support;

use axum::http::Method;
use canterlot_hub::{
    activity,
    blocks::ExecutionRef,
    config::LimitsConfig,
    dispatcher::Dispatcher,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    scheduler::Scheduler,
    session::Registry,
    testkit,
};
use canterlot_proto::worker::v1::{LimitReached, LimitWindow};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::time::Duration;
use support::Fixture;
use uuid::Uuid;

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}
fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn enter_reserve(f: &Fixture, weekly_reset: DateTime<Utc>, now: DateTime<Utc>) {
    let mut tx = f.pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: 960,
            resets_at: Some(weekly_reset),
            observed_at: now,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        now,
        &LimitsConfig::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let scheduler = Scheduler::new(f.pool.clone(), Dispatcher::channel().0, Default::default())
        .with_limits(Registry::new(Default::default()), LimitsConfig::default());
    scheduler.tick(now).await.unwrap();
}

async fn live(f: &Fixture, id: Uuid) -> Uuid {
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(execution)
    .bind(id)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    execution
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spend_reserve_endpoint_is_idempotent_and_owner_scoped(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let other = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    enter_reserve(&f, weekly_reset, now).await;
    let path = f.path("/spend-reserve");
    let first = f
        .client
        .send(Method::POST, &path, Some("spend-a"), None)
        .await;
    assert_eq!(first.status, 200);
    assert_eq!(first.json::<Value>(), "continued");
    let replay = f
        .client
        .send(Method::POST, &path, Some("spend-a"), None)
        .await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, first.body);
    let keys: i64 = sqlx::query_scalar("select count(*) from idempotency_keys where key='spend-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(keys, 1);
    let repeated = f
        .client
        .send(Method::POST, &path, Some("spend-b"), None)
        .await;
    assert_eq!(repeated.status, 200);
    assert_eq!(repeated.json::<Value>(), "already_continued");
    let conflict = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{other}/spend-reserve"),
            Some("spend-a"),
            None,
        )
        .await;
    assert_eq!(conflict.status, 409);
    assert_eq!(
        conflict.json::<Value>()["error"]["code"],
        "idempotency_conflict"
    );
    let foreign = testkit::seed(&pool).await;
    let missing = f
        .client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/spend-reserve",
                foreign.conversation_id
            ),
            Some("spend-foreign"),
            None,
        )
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json::<Value>()["error"]["code"], "not_found");
    let unknown = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/spend-reserve", Uuid::now_v7()),
            Some("spend-unknown"),
            None,
        )
        .await;
    assert_eq!(unknown.status, 404);
    let (a, b) = (unknown.json::<Value>(), missing.json::<Value>());
    assert_eq!(a["error"]["code"], b["error"]["code"]);
    assert_eq!(a["error"]["message"], b["error"]["message"]);
    let no_key = f.client.send(Method::POST, &path, None, None).await;
    assert_eq!(no_key.status, 400);
    let detail = f.get(&f.path("")).await;
    assert_eq!(
        detail["crunch"]["reserve_override_until"],
        wire(weekly_reset)
    );
    assert!(
        detail["holds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["reason"] != "weekly_reserve")
    );
    let other_detail = f.get(&format!("/api/v1/conversations/{other}")).await;
    assert!(
        other_detail["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "weekly_reserve")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_endpoint_exposes_reserve_state_and_holds(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = f.seed.conversation_id;
    sqlx::query("update conversations set crunch_enabled=true where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = live(&f, id).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    enter_reserve(&f, weekly_reset, now).await;
    let reset = now + TimeDelta::hours(1);
    let mut tx = pool.begin().await.unwrap();
    limits::on_limit_reached_in_tx(
        &mut tx,
        f.seed.owner_id,
        &ExecutionRef {
            execution_id: execution,
            conversation_id: id,
            machine_id: f.seed.machine_id,
        },
        &LimitReached {
            execution_id: execution.to_string(),
            provider: "claude".into(),
            window: LimitWindow::FiveHour as i32,
            resets_at_ms: reset.timestamp_millis() as u64,
            input_command_id: String::new(),
        },
        &LimitsConfig::default(),
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    activity::refresh(
        &pool,
        id,
        Utc::now(),
        &activity::Thresholds {
            stalled_generating: Duration::from_secs(300),
            stalled_tool: Duration::from_secs(1800),
            idle_quiet: Duration::from_secs(5),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let current = f.get("/api/v1/limits").await;
    let claude = current
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "claude")
        .unwrap();
    assert_eq!(claude["weekly_reserve_active"], true);
    assert_eq!(claude["weekly_reserve_resets_at"], wire(weekly_reset));
    assert_eq!(claude["weekly"]["used_permille"], 960);
    let list = f.get("/api/v1/conversations").await;
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.to_string())
        .unwrap();
    let holds = item["queue"]["hold_reasons"].as_array().unwrap();
    assert!(holds.contains(&serde_json::json!({"reason":"weekly_reserve","provider":"claude","resets_at":wire(weekly_reset)})));
    assert!(holds.contains(&serde_json::json!({"reason":"quota_exhausted","provider":"claude","window":"five_hour","wake_at":wire(reset)})));
    assert_eq!(item["activity"]["state"], "quota_paused");
    let detail = f.get(&f.path("")).await;
    assert_eq!(detail["crunch"]["resume_pending"], true);
    let reasons: Vec<&str> = detail["holds"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["reason"].as_str())
        .collect();
    assert!(reasons.contains(&"weekly_reserve"), "{reasons:?}");
    assert!(reasons.contains(&"quota_exhausted"), "{reasons:?}");
    let snapshot = f.get("/api/v1/snapshot/conversations").await;
    let limits = snapshot["data"]["limits"].as_array().unwrap();
    assert_eq!(limits[0]["weekly_reserve_resets_at"], wire(weekly_reset));
}
