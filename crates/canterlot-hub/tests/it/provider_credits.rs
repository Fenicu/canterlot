//! Сбросы лимитов Codex: показ, а не трата. Хранятся по машине и провайдеру, а показ берёт
//! самую свежую строку среди машин владельца.

use canterlot_hub::{
    config::{HubTimers, LimitsConfig},
    grpc::KNOWN_OPTIONAL_CAPABILITIES,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    session::CAPABILITY_PROVIDER_CREDITS_V1,
    testkit,
};
use canterlot_proto::worker::v1::{
    ControlUp, LimitsReport, ProviderCredits, ResetCredit, control_up,
};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::task3 as ingest_support;
use crate::support::task7 as support;

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

async fn record(
    pool: &PgPool,
    machine_id: Uuid,
    credits: &ProviderCredits,
    observed_at: DateTime<Utc>,
) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let changed = limits::record_credits_in_tx(
        &mut tx,
        machine_id,
        &credits.provider,
        credits,
        observed_at,
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    changed
}

async fn own_machine(pool: &PgPool, owner_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,$3)")
        .bind(id)
        .bind(owner_id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn observe_weekly(pool: &PgPool, seed: &testkit::Seed, provider: &str) {
    let when = Utc::now() - Duration::minutes(5);
    let mut tx = pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        seed.owner_id,
        &Observation {
            provider: provider.into(),
            account: "owner".into(),
            window: Window::Weekly,
            used_permille: 410,
            resets_at: Some(when + Duration::hours(3)),
            observed_at: when,
            source: ObservationSource::Poll,
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

/// Показ берёт самую свежую строку: счёт общий на учётную запись, а строки разные на машинах.
#[tokio::test]
async fn the_freshest_machine_is_shown_across_two_machines() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "codex").await;
    observe_weekly(&pool, &seed, "codex").await;
    let laptop = own_machine(&pool, seed.owner_id, "ноутбук").await;

    let soon = at(Duration::days(3));
    let later = at(Duration::days(20));
    assert!(
        record(
            &pool,
            seed.machine_id,
            &resets(2, &[soon, later]),
            at(Duration::seconds(-60))
        )
        .await
    );
    // Другая машина обновила счёт позже: показывается её число, а не первая строка.
    assert!(
        record(
            &pool,
            laptop,
            &resets(5, &[later]),
            at(Duration::seconds(-1))
        )
        .await
    );

    let mut conn = pool.acquire().await?;
    let shown = limits::shown(&mut conn, seed.owner_id).await?;
    let codex = shown.iter().find(|p| p.provider == "codex").unwrap();
    let credits = codex.credits.as_ref().expect("credits shown");
    assert_eq!(credits.reset_count, Some(5));
    assert_eq!(credits.resets.len(), 1);
    Ok(())
}

/// Что машина не знает, не стирается: прежние сбросы остаются.
#[tokio::test]
async fn unknown_resets_keep_the_stored_values() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let soon = at(Duration::days(3));
    assert!(
        record(
            &pool,
            seed.machine_id,
            &resets(2, &[soon]),
            at(Duration::seconds(-300))
        )
        .await
    );
    let bare = ProviderCredits {
        provider: "codex".into(),
        ..Default::default()
    };
    assert!(
        !record(&pool, seed.machine_id, &bare, at(Duration::seconds(-100))).await,
        "a report that knows nothing changes nothing"
    );
    let mut conn = pool.acquire().await?;
    let credits = limits::credits(&mut conn, seed.owner_id, "codex", "owner")
        .await?
        .unwrap();
    assert_eq!(credits.reset_count, Some(2));
    assert_eq!(credits.resets.len(), 1);

    // Ноль доступных сбросов — знание, а не пустота: показ очищается.
    assert!(
        record(
            &pool,
            seed.machine_id,
            &resets(0, &[]),
            at(Duration::seconds(-1))
        )
        .await
    );
    let credits = limits::credits(&mut conn, seed.owner_id, "codex", "owner")
        .await?
        .unwrap();
    assert_eq!(credits.reset_count, Some(0));
    assert!(credits.resets.is_empty());
    Ok(())
}

/// Старое наблюдение не откатывает показ: строку двигает только более позднее.
#[tokio::test]
async fn an_older_report_does_not_replace_the_credits() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    assert!(
        record(
            &pool,
            seed.machine_id,
            &resets(2, &[]),
            at(Duration::seconds(-1))
        )
        .await
    );
    assert!(
        !record(
            &pool,
            seed.machine_id,
            &resets(9, &[]),
            at(Duration::seconds(-60))
        )
        .await,
        "a stale record is ignored"
    );
    let mut conn = pool.acquire().await?;
    let credits = limits::credits(&mut conn, seed.owner_id, "codex", "owner")
        .await?
        .unwrap();
    assert_eq!(credits.reset_count, Some(2));
    Ok(())
}

/// Поля API: счёт, заголовок и дата каждого сброса.
#[tokio::test]
async fn the_limits_api_names_the_resets() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let fixture = support::Fixture::new(pool.clone()).await;
    testkit::offer_provider(&pool, fixture.seed.machine_id, "codex").await;
    observe_weekly(
        &pool,
        &testkit::Seed {
            owner_id: fixture.seed.owner_id,
            machine_id: fixture.seed.machine_id,
            conversation_id: fixture.seed.conversation_id,
        },
        "codex",
    )
    .await;
    let expires = at(Duration::days(5));
    record(
        &pool,
        fixture.seed.machine_id,
        &resets(1, &[expires]),
        at(Duration::seconds(-5)),
    )
    .await;

    let limits = fixture.get("/api/v1/limits").await;
    let codex = limits
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "codex")
        .expect("codex limits");
    let credits = &codex["credits"];
    assert_eq!(credits["reset_count"], 1);
    assert_eq!(credits["resets"][0]["title"], "Full reset (Weekly + 5 hr)");
    assert_eq!(
        credits["resets"][0]["expires_at"],
        expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    assert!(credits["observed_at"].is_string());
    Ok(())
}

/// Хаб объявляет возможность: без неё воркер сбросы не шлёт, а хаб их не ждёт.
#[test]
fn the_hub_announces_the_provider_credits_capability() {
    assert!(KNOWN_OPTIONAL_CAPABILITIES.contains(&CAPABILITY_PROVIDER_CREDITS_V1));
}
