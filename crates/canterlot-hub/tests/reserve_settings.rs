#[path = "support/task7.rs"]
mod support;

use axum::http::Method;
use canterlot_hub::{
    config::LimitsConfig,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    model::HoldReason,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::Fixture;
use uuid::Uuid;

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

async fn put(f: &Fixture, key: &str, body: Value) -> axum::http::StatusCode {
    f.client
        .send(Method::PUT, "/api/v1/limit-settings", Some(key), Some(body))
        .await
        .status
}

async fn reserve(f: &Fixture, value: u16, key: &str) {
    assert_eq!(
        put(
            f,
            key,
            json!({"settings": {"warn_remaining_pct": 20, "danger_remaining_pct": 5, "reserve_remaining_pct": value}})
        )
        .await,
        200
    );
}

async fn observe(f: &Fixture, used: u16, resets_at: DateTime<Utc>, now: DateTime<Utc>) {
    let mut tx = f.pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: used,
            resets_at: Some(resets_at),
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
}

async fn reserve_flag(pool: &PgPool) -> bool {
    sqlx::query_scalar(
        "select coalesce((select weekly_reserve_active from provider_limits where provider='claude'),false)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn held(f: &Fixture) -> bool {
    let holds: sqlx::types::Json<Vec<HoldReason>> =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(f.seed.conversation_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    holds.0.iter().any(
        |hold| matches!(hold, HoldReason::WeeklyReserve { provider, .. } if provider == "claude"),
    )
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_fresh_weekly_reading_uses_the_saved_reserve(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 20, "limits-raise").await;
    // Осталось 15% недели: при резерве 20% показание включает резерв.
    observe(&f, 850, weekly_reset, now).await;
    assert!(reserve_flag(&pool).await);
    f.state.scheduler.tick(now).await.unwrap();
    assert!(held(&f).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn changing_the_reserve_releases_and_holds_immediately(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 0, "limits-off").await;
    observe(&f, 850, weekly_reset, now).await;
    assert!(!reserve_flag(&pool).await);
    assert!(!held(&f).await);

    // Резерв 20% поднимает порог выше текущего расхода: сессия удерживается без нового показания.
    reserve(&f, 20, "limits-hold").await;
    assert!(reserve_flag(&pool).await);
    assert!(held(&f).await);

    // Отключение резерва снимает удержание тем же путём, что и подтверждённый сброс окна.
    reserve(&f, 0, "limits-release").await;
    assert!(!reserve_flag(&pool).await);
    assert!(!held(&f).await);
}
