use crate::support::task7 as support;

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

async fn low_count(pool: &PgPool) -> i16 {
    sqlx::query_scalar("select weekly_low_count from provider_limits where provider='claude'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_exit_confirmations_do_not_release_a_raised_reserve(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 5, "limits-base").await;
    observe(&f, 960, weekly_reset, now).await;
    f.state.scheduler.tick(now).await.unwrap();
    // Два подтверждения ниже старого порога 950‰ и его выхода 930‰.
    observe(&f, 900, weekly_reset, now + TimeDelta::seconds(601)).await;
    observe(&f, 900, weekly_reset, now + TimeDelta::seconds(1202)).await;
    assert_eq!(low_count(&pool).await, 2);

    // Новый резерв 20% (порог 800‰) ниже текущего расхода 900‰: старые подтверждения
    // не должны снять только что подтверждённый резерв.
    reserve(&f, 20, "limits-raised").await;
    assert!(reserve_flag(&pool).await);
    assert!(held(&f).await);
    assert_eq!(low_count(&pool).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn colors_only_save_keeps_an_active_reserve_inside_the_hysteresis_band(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 5, "limits-base").await;
    observe(&f, 960, weekly_reset, now).await;
    f.state.scheduler.tick(now).await.unwrap();
    assert!(held(&f).await);
    // 94% расхода: ниже порога 950‰, но выше выхода 930‰ — резерв законно держится.
    observe(&f, 940, weekly_reset, now + TimeDelta::seconds(601)).await;

    assert_eq!(
        put(
            &f,
            "limits-colors-only",
            json!({"settings": {"warn_remaining_pct": 30, "danger_remaining_pct": 10, "reserve_remaining_pct": 5}})
        )
        .await,
        200
    );
    assert!(reserve_flag(&pool).await);
    assert!(held(&f).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lowering_the_reserve_below_the_hysteresis_exit_releases(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 20, "limits-high").await;
    observe(&f, 850, weekly_reset, now).await;
    f.state.scheduler.tick(now).await.unwrap();
    assert!(held(&f).await);

    // Порог 950‰, выход 930‰: расход 850‰ ниже выхода, резерв снимается.
    reserve(&f, 5, "limits-low").await;
    assert!(!reserve_flag(&pool).await);
    assert!(!held(&f).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_provider_that_left_the_catalog_is_still_released(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    reserve(&f, 5, "limits-base").await;
    observe(&f, 960, weekly_reset, now).await;
    f.state.scheduler.tick(now).await.unwrap();
    assert!(held(&f).await);
    sqlx::query("delete from machine_harnesses where machine_id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();

    reserve(&f, 0, "limits-off").await;
    assert!(!reserve_flag(&pool).await);
    assert!(!held(&f).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reserve_change_and_ingest_lock_providers_in_one_order(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title) values($1,'synthetic-codex','codex','Codex') on conflict do nothing")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    for provider in ["claude", "codex"] {
        sqlx::query("insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,weekly_observed_at,weekly_reserve_active) values($1,960,$2,$3,true) on conflict(provider) do update set weekly_used_permille=960,weekly_resets_at=$2,weekly_observed_at=$3,weekly_reserve_active=true")
            .bind(provider)
            .bind(weekly_reset)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
    }

    // Как отчёт лимитов: держим codex и следом берём счётчик событий владельца.
    let mut ingest = pool.begin().await.unwrap();
    sqlx::query("select provider from provider_limits where provider='codex' for update")
        .execute(&mut *ingest)
        .await
        .unwrap();
    let client = f.client.clone();
    let change = tokio::spawn(async move {
        client
            .send(
                Method::PUT,
                "/api/v1/limit-settings",
                Some("limits-concurrency"),
                Some(json!({"settings": {"warn_remaining_pct": 20, "danger_remaining_pct": 5, "reserve_remaining_pct": 0}})),
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    sqlx::query("insert into scope_counters(scope,last_seq) values($1,1) on conflict(scope) do update set last_seq=scope_counters.last_seq+1")
        .bind(format!("owner:{}:conversations", f.seed.owner_id))
        .execute(&mut *ingest)
        .await
        .unwrap();
    ingest.commit().await.unwrap();

    let response = tokio::time::timeout(std::time::Duration::from_secs(10), change)
        .await
        .expect("reserve change must not deadlock")
        .unwrap();
    assert_eq!(response.status, 200);
    for provider in ["claude", "codex"] {
        let active: bool = sqlx::query_scalar(
            "select weekly_reserve_active from provider_limits where provider=$1",
        )
        .bind(provider)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!active, "{provider} must be released");
    }
}
