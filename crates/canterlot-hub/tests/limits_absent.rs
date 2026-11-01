//! Окно лимита, которого у провайдера нет (план без пятичасового лимита): хранится как отсутствие,
//! история остаётся, возвращение окна снимает отметку.

use canterlot_hub::{
    config::LimitsConfig,
    limits::{self, Observation, ObservationSource, Window},
    testkit,
};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn at(offset_minutes: i64) -> DateTime<Utc> {
    let base = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    base + Duration::minutes(offset_minutes)
}

async fn observe(pool: &PgPool, s: &testkit::Seed, window: Window, used: u16, when: DateTime<Utc>) {
    let mut tx = pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        s.owner_id,
        &Observation {
            provider: "claude".into(),
            window,
            used_permille: used,
            resets_at: Some(when + Duration::hours(3)),
            observed_at: when,
            source: ObservationSource::Poll,
            machine_id: s.machine_id,
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

async fn absent(pool: &PgPool, s: &testkit::Seed, when: DateTime<Utc>) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let changed = limits::record_absence_in_tx(
        &mut tx,
        s.owner_id,
        "claude",
        Window::FiveHour,
        when,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    changed
}

async fn claude(pool: &PgPool) -> limits::ProviderLimits {
    let mut conn = pool.acquire().await.unwrap();
    limits::current(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.provider == "claude")
        .unwrap()
}

async fn last_change(pool: &PgPool) -> serde_json::Value {
    sqlx::query_scalar(
        "select payload from events where kind='limits_changed' order by scope_seq desc limit 1",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn changes(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from events where kind='limits_changed'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn absence_hides_the_window_keeps_history_and_tells_clients(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    observe(&pool, &s, Window::FiveHour, 440, at(-30)).await;
    observe(&pool, &s, Window::Weekly, 530, at(-30)).await;
    assert!(!claude(&pool).await.five_hour_absent);
    let before = changes(&pool).await;

    assert!(absent(&pool, &s, at(-10)).await);
    let state = claude(&pool).await;
    assert!(state.five_hour.is_none() && state.five_hour_absent);
    assert_eq!(state.weekly.unwrap().used_permille, 530);
    let history: i64 = sqlx::query_scalar(
        "select count(*) from limit_snapshots where provider='claude' and window_kind='five_hour'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(history, 1, "history is not deleted");
    let payload = last_change(&pool).await;
    assert_eq!(payload["window"], "five_hour");
    assert!(payload["used_permille"].is_null() && payload["resets_at"].is_null());
    assert_eq!(changes(&pool).await, before + 1);

    // Повтор того же отсутствия новых событий не порождает.
    assert!(!absent(&pool, &s, at(-5)).await);
    assert_eq!(changes(&pool).await, before + 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_reading_from_before_the_absence_does_not_bring_the_window_back(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    observe(&pool, &s, Window::Weekly, 530, at(-30)).await;
    assert!(absent(&pool, &s, at(-10)).await);
    observe(&pool, &s, Window::FiveHour, 440, at(-20)).await;
    let state = claude(&pool).await;
    assert!(state.five_hour.is_none() && state.five_hour_absent);

    observe(&pool, &s, Window::FiveHour, 100, at(-1)).await;
    let state = claude(&pool).await;
    assert!(!state.five_hour_absent);
    assert_eq!(state.five_hour.unwrap().used_permille, 100);
    let payload = last_change(&pool).await;
    assert_eq!(payload["used_permille"], 100);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn absence_of_a_window_that_was_never_seen_is_still_recorded(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    assert!(absent(&pool, &s, at(-1)).await);
    let state = claude(&pool).await;
    assert!(state.five_hour.is_none() && state.five_hour_absent);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_the_five_hour_window_can_be_absent(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let mut tx = pool.begin().await?;
    let error = limits::record_absence_in_tx(
        &mut tx,
        s.owner_id,
        "claude",
        Window::Weekly,
        at(0),
        Uuid::now_v7(),
    )
    .await;
    assert!(error.is_err());
    Ok(())
}
