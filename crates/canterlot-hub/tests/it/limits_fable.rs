//! Недельное окно Fable живёт отдельно от недельного: показывается в API, истории и
//! тексте инструмента, а кадры сессий Opus без этого окна не стирают показание.

use std::sync::Arc;

use canterlot_hub::{
    blocks::ExecutionRef,
    config::{LimitsConfig, SessionToolsConfig},
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit,
};
use canterlot_proto::worker::v1::{LimitReached, LimitWindow};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::task7 as support;

use support::Fixture;

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
        memory: Default::default(),
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: std::time::Duration::from_secs(1800),
        routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        cloud: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

async fn observe(
    pool: &PgPool,
    seed: &testkit::Seed,
    window: Window,
    used: u16,
    reset: chrono::DateTime<Utc>,
    at: chrono::DateTime<Utc>,
) {
    let mut tx = pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        seed.owner_id,
        &Observation {
            provider: "claude".into(),
            account: "owner".into(),
            window,
            used_permille: used,
            resets_at: Some(reset),
            observed_at: at,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: seed.machine_id,
            execution_id: None,
        },
        Utc::now(),
        &LimitsConfig::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn fable_window_is_listed_in_the_api_history_and_report() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::days(4);
    observe(
        &f.pool,
        &f.seed,
        Window::WeeklyFable,
        50,
        reset,
        now - TimeDelta::minutes(1),
    )
    .await;

    let view = f.get("/api/v1/limits").await;
    assert_eq!(view[0]["weekly_fable"]["used_pct"], 5.0, "{view}");
    assert_eq!(view[0]["weekly_fable"]["used_permille"], 50, "{view}");

    let from = (now - TimeDelta::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let to = (now + TimeDelta::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let history = f
        .get(&format!(
            "/api/v1/limits/history?provider=claude&from={from}&to={to}"
        ))
        .await;
    let points = history["series"][0]["weekly_fable"].as_array().unwrap();
    assert_eq!(points.len(), 1, "{history}");
    assert_eq!(points[0]["used_permille"], 50, "{history}");

    let caller = Caller {
        owner_id: f.seed.owner_id,
        conversation_id: f.seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: f.seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let report = session_tools::call(
        &context(&f.pool),
        &caller,
        "limits.report",
        json!({"provider": "claude"}),
    )
    .await
    .unwrap();
    let text = report["text"].as_str().unwrap();
    assert!(text.contains("Fable weekly window 5% used"), "{text}");
    let windows = report["providers"][0]["windows"].as_array().unwrap();
    let fable = windows
        .iter()
        .find(|window| window["window"] == "weekly_fable")
        .expect("fable window in report");
    assert_eq!(fable["used_percent"], 5.0, "{fable}");
}

#[tokio::test]
async fn a_fable_limit_pauses_only_that_session_and_no_provider_reserve() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let execution = f.execution().await;
    let reset = Utc::now() + TimeDelta::days(3);
    let mut tx = f.pool.begin().await.unwrap();
    let effect = limits::on_limit_reached_in_tx(
        &mut tx,
        f.seed.owner_id,
        &ExecutionRef {
            execution_id: execution,
            conversation_id: f.seed.conversation_id,
            machine_id: f.seed.machine_id,
        },
        &LimitReached {
            execution_id: execution.to_string(),
            provider: "claude".into(),
            window: LimitWindow::WeeklyFable as i32,
            resets_at_ms: reset.timestamp_millis() as u64,
            input_command_id: String::new(),
            account: String::new(),
        },
        &LimitsConfig::default(),
        Utc::now(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(effect, limits::LimitReachedEffect::Paused);
    let (pause, reserve): (Value, bool) = sqlx::query_as(
        "select q.pause_reasons, coalesce(p.weekly_reserve_active,false)
           from queue_state q left join provider_limits p on p.provider='claude'
          where q.conversation_id=$1",
    )
    .bind(f.seed.conversation_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(pause[0]["window"], "weekly_fable", "{pause}");
    assert!(
        !reserve,
        "a fable limit does not activate the weekly reserve"
    );
}
