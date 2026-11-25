//! Сбросы лимитов Codex: показ, а не трата. Хранятся по машине и провайдеру, а показ берёт
//! самую свежую строку среди машин владельца.

use canterlot_hub::{
    config::HubTimers,
    limits::{self},
    session::CAPABILITY_PROVIDER_CREDITS_V1,
    testkit,
};
use canterlot_proto::worker::v1::{
    ControlUp, LimitsReport, ProviderCredits, ResetCredit, control_up,
};
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::support::task3 as ingest_support;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn at(offset: Duration) -> DateTime<Utc> {
    DateTime::from_timestamp_micros((Utc::now() + offset).timestamp_micros()).unwrap()
}

fn resets(count: u32, expires: &[DateTime<Utc>]) -> ProviderCredits {
    ProviderCredits {
        provider: "codex".into(),
        resets_known: true,
        reset_count: count,
        resets: expires
            .iter()
            .map(|expires| ResetCredit {
                title: "Full reset (Weekly + 5 hr)".into(),
                expires_at_ms: expires.timestamp_millis() as u64,
                granted_at_ms: 1_793_570_435_000,
            })
            .collect(),
        ..Default::default()
    }
}

/// Отчёт машины идёт тем же путём, что окна: строка на машину и провайдера, и лимиты её видят.
#[tokio::test]
async fn a_limits_report_reaches_the_stored_credits() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = ingest_support::wire_seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "codex").await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let client = ingest_support::connect_with(
        &hub,
        &seed,
        Uuid::now_v7(),
        &["pilot_v1", CAPABILITY_PROVIDER_CREDITS_V1],
        Utc::now().timestamp_millis() as u64,
    )
    .await;
    let expires = at(Duration::days(5));
    client
        .tx
        .send(ControlUp {
            msg: Some(control_up::Msg::LimitsReport(LimitsReport {
                provider_credits: vec![ProviderCredits {
                    observed_machine_ms: Utc::now().timestamp_millis() as u64,
                    ..resets(2, &[expires])
                }],
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    ingest_support::wait_until(&pool, "select count(*) from provider_credits").await;

    let (provider, count, stored): (String, i32, serde_json::Value) = sqlx::query_as(
        "select provider,reset_count,resets from provider_credits where machine_id=$1",
    )
    .bind(seed.machine_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(provider, "codex");
    assert_eq!(count, 2);
    assert_eq!(stored[0]["title"], "Full reset (Weekly + 5 hr)");
    assert_eq!(
        stored[0]["expires_at_ms"].as_u64(),
        Some(expires.timestamp_millis() as u64)
    );
    let mut conn = pool.acquire().await?;
    let credits = limits::credits(&mut conn, seed.owner_id, "codex", "owner")
        .await?
        .unwrap();
    assert_eq!(credits.reset_count, Some(2));
    hub.shutdown.send(()).unwrap();
    hub.join.await.unwrap().unwrap();
    Ok(())
}
