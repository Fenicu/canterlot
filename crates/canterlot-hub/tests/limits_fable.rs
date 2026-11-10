//! Недельное окно Fable живёт отдельно от недельного: показывается в API, истории и
//! тексте инструмента, а кадры сессий Opus без этого окна не стирают показание.

use std::sync::Arc;

use canterlot_hub::{
    blocks::ExecutionRef,
    config::{HubTimers, LimitsConfig, SessionToolsConfig},
    ingest::Ingest,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit,
};
use canterlot_proto::worker::v1::{LimitObservation, LimitReached, LimitWindow, record_body::Body};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[path = "support/task3.rs"]
mod ingest_support;
#[path = "support/task7.rs"]
mod support;

use support::Fixture;

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fable_window_is_listed_in_the_api_history_and_report(pool: PgPool) {
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

/// Тот же кадр Claude Code, что проверяет воркер: здесь он входит в хаб как готовый protobuf.
const FABLE_FRAME: &str =
    include_str!("../../canterlot-proto/tests/fixtures/fable_rate_limit.json");

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_verbatim_fable_frame_reaches_the_hub_limits(pool: PgPool) {
    let frame: Value = serde_json::from_str(FABLE_FRAME).unwrap();
    let row = &frame["unifiedWindows"]["seven_day_overage_included"];
    let used = (row["utilization"].as_f64().unwrap() * 1000.0).round() as u32;
    let reset_ms = row["resetsAt"].as_u64().unwrap() * 1000;
    let mut run = ingest_support::Run::new(&pool).await;
    testkit::offer_provider(&pool, run.seed.machine_id, "claude").await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    {
        use sha2::{Digest, Sha256};
        sqlx::query("update machine_credentials set token_sha256=$2 where machine_id=$1")
            .bind(run.seed.machine_id)
            .bind(Sha256::digest(run.seed.machine_id.to_string().as_bytes()).to_vec())
            .execute(&pool)
            .await
            .unwrap();
    }
    let _client = ingest_support::connect(
        &hub,
        &run.seed,
        Uuid::now_v7(),
        true,
        Utc::now().timestamp_millis() as u64,
    )
    .await;
    sqlx::query("update machine_credentials set token_sha256=$2 where machine_id=$1")
        .bind(run.seed.machine_id)
        .bind(run.seed.credential_digest())
        .execute(&pool)
        .await
        .unwrap();
    run.ingest = Ingest::new(pool.clone()).with_registry(hub.registry.clone());
    run.send(vec![Body::LimitObserved(LimitObservation {
        provider: "claude".into(),
        window: LimitWindow::WeeklyFable as i32,
        used_permille: used,
        resets_at_ms: reset_ms,
        observed_machine_ms: Utc::now().timestamp_millis() as u64,
        ..Default::default()
    })])
    .await;
    let mut conn = pool.acquire().await.unwrap();
    let claude = limits::current(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .find(|provider| provider.provider == "claude")
        .unwrap();
    let fable = claude.weekly_fable.expect("weekly_fable stored");
    assert_eq!(fable.used_permille, 50, "the frame's 5% is stored");
    assert_eq!(
        fable.resets_at.map(|at| at.timestamp_millis()),
        Some(reset_ms as i64)
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_opus_observation_without_fable_keeps_the_known_value(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::days(4);
    observe(
        &f.pool,
        &f.seed,
        Window::WeeklyFable,
        50,
        reset,
        now - TimeDelta::minutes(2),
    )
    .await;
    observe(
        &f.pool,
        &f.seed,
        Window::Weekly,
        300,
        reset,
        now - TimeDelta::minutes(1),
    )
    .await;
    observe(
        &f.pool,
        &f.seed,
        Window::FiveHour,
        100,
        now + TimeDelta::hours(2),
        now,
    )
    .await;

    let view: Value = f.get("/api/v1/limits").await;
    assert_eq!(
        view[0]["weekly_fable"]["used_permille"], 50,
        "a batch without the window leaves it stale, not erased: {view}"
    );
    assert_eq!(view[0]["weekly_fable"]["used_pct"], 5.0, "{view}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unknown_window_value_is_rejected_not_stored(pool: PgPool) {
    let _seed = testkit::seed(&pool).await;
    assert!(matches!(
        Window::from_proto(999),
        Err(sqlx::Error::InvalidArgument(_))
    ));
    let windows: Vec<String> =
        sqlx::query_scalar("select distinct window_kind from limit_snapshots")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(windows.is_empty(), "{windows:?}");
}
