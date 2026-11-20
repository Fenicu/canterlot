//! «Продолжить при сбросе»: одноразовое продолжение по паузе лимита без «кранча».
use crate::support::task7 as support;

use axum::http::Method;
use canterlot_hub::{
    blocks::ExecutionRef,
    config::LimitsConfig,
    dispatcher::Dispatcher,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    model::{HoldReason, PauseReason},
    scheduler::Scheduler,
    session::Registry,
};
use canterlot_proto::worker::v1::{LimitReached, LimitWindow};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use support::Fixture;
use uuid::Uuid;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn proto_window(window: Window) -> i32 {
    match window {
        Window::FiveHour => LimitWindow::FiveHour as i32,
        Window::Weekly => LimitWindow::Weekly as i32,
        Window::Monthly => LimitWindow::Monthly as i32,
        Window::WeeklyFable => LimitWindow::WeeklyFable as i32,
    }
}

/// Живое исполнение: у засеянной сессии иначе стоит удержание «нет процесса».
async fn live(f: &Fixture) -> Uuid {
    let execution = f.execution().await;
    sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
        .bind(f.seed.conversation_id)
        .execute(&f.pool)
        .await
        .unwrap();
    execution
}

/// Обычная остановка лимитом: так же, как её видит хаб без «кранча».
async fn plain_stop(
    f: &Fixture,
    execution: Uuid,
    provider: &str,
    window: Window,
    resets_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) {
    let mut tx = f.pool.begin().await.unwrap();
    limits::on_limit_reached_in_tx(
        &mut tx,
        f.seed.owner_id,
        &ExecutionRef {
            execution_id: execution,
            conversation_id: f.seed.conversation_id,
            machine_id: f.seed.machine_id,
        },
        &LimitReached {
            execution_id: execution.to_string(),
            provider: provider.into(),
            window: proto_window(window),
            resets_at_ms: resets_at.map_or(0, |at| at.timestamp_millis() as u64),
            input_command_id: String::new(),
            account: String::new(),
        },
        &LimitsConfig::default(),
        at,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn pause_reasons(pool: &PgPool, id: Uuid) -> Vec<PauseReason> {
    let json: sqlx::types::Json<Vec<PauseReason>> =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    json.0
}

async fn hold_reasons(pool: &PgPool, id: Uuid) -> Vec<HoldReason> {
    let json: sqlx::types::Json<Vec<HoldReason>> =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    json.0
}

async fn crunch_enabled(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("select crunch_enabled from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn resume_pending(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("select resume_pending from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[derive(sqlx::FromRow)]
struct WakeJob {
    state: String,
    payload: Value,
}

async fn wake_jobs(pool: &PgPool, id: Uuid) -> Vec<WakeJob> {
    sqlx::query_as(
        "select state,payload from scheduled_jobs where conversation_id=$1 and kind='wake' order by created_at,id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn observe(
    f: &Fixture,
    provider: &str,
    window: Window,
    used: u16,
    resets_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) {
    let mut tx = f.pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: provider.into(),
            window,
            used_permille: used,
            resets_at,
            observed_at: at,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        at,
        &LimitsConfig::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn tick(pool: &PgPool, at: DateTime<Utc>) -> canterlot_hub::scheduler::TickReport {
    let scheduler = Scheduler::new(pool.clone(), Dispatcher::channel().0, Default::default())
        .with_limits(Registry::new(Default::default()), LimitsConfig::default());
    scheduler.tick(at).await.unwrap()
}

async fn wake_texts(pool: &PgPool, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("select content->0->>'text' from inputs where conversation_id=$1 and idempotency_key like 'wake:%' order by input_seq")
        .bind(id).fetch_all(pool).await.unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn arming_keeps_crunch_off_and_builds_the_same_hold_and_wake_job(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let execution = live(&f).await;
    let at = now();
    let reset = at + TimeDelta::hours(1);
    plain_stop(&f, execution, "claude", Window::FiveHour, Some(reset), at).await;
    assert!(matches!(
        pause_reasons(&pool, f.seed.conversation_id)
            .await
            .as_slice(),
        [PauseReason::LimitReached { .. }]
    ));

    let response = f
        .client
        .send(Method::POST, &f.path("/auto-resume"), Some("auto-a"), None)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json::<Value>(), "armed");

    let id = f.seed.conversation_id;
    assert!(
        !crunch_enabled(&pool, id).await,
        "кранч остаётся выключенным"
    );
    assert!(resume_pending(&pool, id).await);
    assert!(
        pause_reasons(&pool, id).await.is_empty(),
        "пауза лимита стала удержанием"
    );
    let holds = hold_reasons(&pool, id).await;
    assert!(matches!(
        holds.as_slice(),
        [HoldReason::QuotaExhausted { provider, window, wake_at }]
            if provider == "claude" && window == "five_hour" && *wake_at == reset
    ));
    let jobs = wake_jobs(&pool, id).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, "pending");
    assert_eq!(jobs[0].payload["provider"], "claude");
    assert_eq!(jobs[0].payload["window"], "five_hour");
    assert_eq!(jobs[0].payload["one_shot"], true);
    assert_eq!(jobs[0].payload["resets_at"], wire(reset));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_after_verified_reset_continues_once_and_the_next_stop_is_plain(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let execution = live(&f).await;
    let mut at = now();
    let reset = at + TimeDelta::hours(1);
    plain_stop(&f, execution, "claude", Window::FiveHour, Some(reset), at).await;
    let response = f
        .client
        .send(Method::POST, &f.path("/auto-resume"), Some("auto-b"), None)
        .await;
    assert_eq!(response.status, 200);

    // Сброс подтверждён точным показанием после срока.
    observe(
        &f,
        "claude",
        Window::FiveHour,
        20,
        Some(reset + TimeDelta::hours(5)),
        reset + TimeDelta::seconds(30),
    )
    .await;
    tick(&pool, reset + TimeDelta::seconds(120)).await;

    let id = f.seed.conversation_id;
    assert!(hold_reasons(&pool, id).await.is_empty(), "удержание снято");
    assert_eq!(
        wake_texts(&pool, id).await,
        vec![LimitsConfig::default().default_resume_text],
        "продолжение отправлено ровно один раз"
    );
    assert!(!resume_pending(&pool, id).await);
    assert!(!crunch_enabled(&pool, id).await);
    let again = tick(&pool, reset + TimeDelta::seconds(600)).await;
    assert_eq!(again.fired, 0);
    assert_eq!(wake_texts(&pool, id).await.len(), 1);

    // Следующая остановка лимитом — снова обычная пауза, одноразовость не осталась.
    at = reset + TimeDelta::minutes(20);
    plain_stop(
        &f,
        execution,
        "claude",
        Window::FiveHour,
        Some(at + TimeDelta::hours(5)),
        at,
    )
    .await;
    assert!(matches!(
        pause_reasons(&pool, id).await.as_slice(),
        [PauseReason::LimitReached { provider: Some(provider), window: Some(window), .. }]
            if provider == "claude" && window == "five_hour"
    ));
    assert!(hold_reasons(&pool, id).await.is_empty());
    assert!(!resume_pending(&pool, id).await);
    assert_eq!(wake_jobs(&pool, id).await.len(), 1, "нового задания нет");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancel_restores_the_plain_pause_and_cancels_the_wake(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let execution = live(&f).await;
    let at = now();
    let reset = at + TimeDelta::hours(2);
    plain_stop(&f, execution, "claude", Window::FiveHour, Some(reset), at).await;
    let armed = f
        .client
        .send(Method::POST, &f.path("/auto-resume"), Some("auto-c"), None)
        .await;
    assert_eq!(armed.status, 200);

    let cancelled = f
        .client
        .send(
            Method::DELETE,
            &f.path("/auto-resume"),
            Some("cancel-c"),
            None,
        )
        .await;
    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.json::<Value>(), "cancelled");

    let id = f.seed.conversation_id;
    assert!(hold_reasons(&pool, id).await.is_empty());
    assert!(!resume_pending(&pool, id).await);
    assert!(!crunch_enabled(&pool, id).await);
    assert!(matches!(
        pause_reasons(&pool, id).await.as_slice(),
        [PauseReason::LimitReached { provider: Some(provider), window: Some(window), resets_at: Some(at) }]
            if provider == "claude" && window == "five_hour" && *at == reset
    ));
    let jobs = wake_jobs(&pool, id).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].state, "cancelled");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn arming_without_a_limit_pause_is_refused(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let response = f
        .client
        .send(Method::POST, &f.path("/auto-resume"), Some("auto-d"), None)
        .await;
    assert_eq!(response.status, 409);
    assert_eq!(response.json::<Value>()["error"]["code"], "invalid_state");
    let id = f.seed.conversation_id;
    assert!(!resume_pending(&pool, id).await);
    assert!(wake_jobs(&pool, id).await.is_empty());
    assert!(hold_reasons(&pool, id).await.is_empty());
}
