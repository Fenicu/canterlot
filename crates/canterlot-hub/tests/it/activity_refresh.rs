//! Пересчёт активности разговора: водяной знак починки без лишних событий и
//! пороги активности и свежести метрик из конфигурации при отметке прочтения.

use crate::support::task5 as support;

use axum::http::Method;
use canterlot_hub::{
    activity,
    http::{self, AppState},
    testkit::{self, TestClient},
};
use chrono::Utc;
use serde_json::json;
use std::time::Duration;
use support::*;
use uuid::Uuid;

#[tokio::test]
async fn unchanged_refresh_advances_repair_watermark_without_events() {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    notice(&pool, s.conversation_id, "conversation_updated").await;
    let events = count(&pool, "select count(*) from events").await;
    assert_eq!(
        refresh(&pool, s.conversation_id).await,
        activity::Refresh::Unchanged
    );
    let repaired:bool=sqlx::query_scalar("select d.updated_at >= (select max(committed_at) from events where kind='conversation_updated') from conversation_derived d where conversation_id=$1").bind(s.conversation_id).fetch_one(&pool).await.unwrap();
    assert!(repaired);
    assert_eq!(count(&pool, "select count(*) from events").await, events);
}
#[tokio::test]
async fn mark_read_uses_configured_activity_thresholds() {
    let pool = canterlot_hub::testkit::db().await;
    let (s, _) = running(&pool).await;
    let (mut deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    deps.installation.owner_id = s.owner_id;
    deps.activity.stalled_generating = Duration::from_secs(3600);
    activity::refresh(
        &pool,
        s.conversation_id,
        Utc::now(),
        &activity::Thresholds::from(&deps.activity),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let before = count(
        &pool,
        "select count(*) from events where kind='derived_changed'",
    )
    .await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let response = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/read", s.conversation_id),
            None,
            Some(json!({"seq":"0"})),
        )
        .await;
    assert_eq!(response.status, 204);
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_state"],
        "working"
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='derived_changed'"
        )
        .await,
        before
    );
}
#[tokio::test]
async fn mark_read_preserves_configured_metrics_freshness() {
    let pool = canterlot_hub::testkit::db().await;
    let (s, execution) = running(&pool).await;
    sqlx::query("update executions set phase_at=clock_timestamp()-interval '31 minutes',last_event_at=clock_timestamp()-interval '31 minutes' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '31 minutes' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let (mut deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    deps.installation.owner_id = s.owner_id;
    deps.machines.metrics_stale_after = Duration::from_secs(3600);
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp()-interval '5 minutes',1,1,50,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id).bind(json!([{"execution_id":execution,"alive":true,"cpu_permille":0,"descendants":0}])).execute(&pool).await.unwrap();
    activity::refresh_with_metrics(
        &pool,
        s.conversation_id,
        Utc::now(),
        &activity::Thresholds::from(&deps.activity),
        deps.machines.metrics_stale_after,
        true,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let before = row(&pool, s.conversation_id).await["activity_evidence"].clone();
    assert_eq!(before["cpu_busy"], false);
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    assert_eq!(
        client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{}/read", s.conversation_id),
                None,
                Some(json!({"seq":"0"}))
            )
            .await
            .status,
        204
    );
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_evidence"],
        before
    );
}
