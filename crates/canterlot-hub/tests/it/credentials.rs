//! Конец входа Claude на машине: дата из кадра `result` хранится по машине и провайдеру
//! и видна в полосе лимитов и карточке машины.

use canterlot_hub::{
    config::LimitsConfig,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    testkit,
};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::task7 as support;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn at(offset: Duration) -> DateTime<Utc> {
    DateTime::from_timestamp_micros((Utc::now() + offset).timestamp_micros()).unwrap()
}

async fn record(
    pool: &PgPool,
    machine_id: Uuid,
    provider: &str,
    expires_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let changed = limits::record_credential_in_tx(
        &mut tx,
        machine_id,
        provider,
        expires_at,
        observed_at,
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    changed
}

async fn observe_window(pool: &PgPool, s: &testkit::Seed) {
    let when = Utc::now() - Duration::minutes(5);
    let mut tx = pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        s.owner_id,
        &Observation {
            provider: "claude".into(),
            account: "owner".into(),
            window: Window::Weekly,
            used_permille: 410,
            resets_at: Some(when + Duration::hours(3)),
            observed_at: when,
            source: ObservationSource::Poll,
            data_confidence: DataConfidence::Exact,
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn credential_is_stored_per_machine_and_the_earliest_one_is_shown(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, s.machine_id, "claude").await;
    observe_window(&pool, &s).await;
    let laptop = own_machine(&pool, s.owner_id, "ноутбук").await;

    let soon = at(Duration::days(3));
    let later = at(Duration::days(20));
    assert!(
        record(
            &pool,
            s.machine_id,
            "claude",
            soon,
            at(Duration::seconds(-5))
        )
        .await,
        "first date is new"
    );
    assert!(
        record(&pool, laptop, "claude", later, at(Duration::seconds(-5))).await,
        "the same provider on another machine is a separate row"
    );
    // Пересланная старая запись не откатывает дату: обновление приходит позже.
    assert!(
        !record(
            &pool,
            s.machine_id,
            "claude",
            at(Duration::days(1)),
            at(Duration::seconds(-60))
        )
        .await,
        "an older observation does not replace the date"
    );

    let rows: Vec<(String, DateTime<Utc>)> =
        sqlx::query_as("select provider,expires_at from provider_credentials order by machine_id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(rows.len(), 2, "one row per machine and provider");
    assert!(rows.iter().all(|(provider, _)| provider == "claude"));
    assert!(rows.iter().any(|(_, expires)| *expires == soon));
    assert!(rows.iter().any(|(_, expires)| *expires == later));

    let mut conn = pool.acquire().await?;
    let shown = limits::shown(&mut conn, s.owner_id).await?;
    let claude = shown.iter().find(|p| p.provider == "claude").unwrap();
    assert_eq!(
        claude.credential_expires_at,
        Some(soon),
        "the strip names the date that ends first"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_login_date_replaces_the_expired_one(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let expired = at(Duration::days(-1));
    let renewed = at(Duration::days(30));
    assert!(
        record(
            &pool,
            s.machine_id,
            "claude",
            expired,
            at(Duration::days(-2))
        )
        .await
    );
    assert!(
        record(
            &pool,
            s.machine_id,
            "claude",
            renewed,
            at(Duration::seconds(-1))
        )
        .await,
        "a later observation with a new date is stored"
    );
    let stored: DateTime<Utc> = sqlx::query_scalar(
        "select expires_at from provider_credentials where machine_id=$1 and provider='claude'",
    )
    .bind(s.machine_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored, renewed);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_view_names_the_credential_date(pool: PgPool) -> TestResult {
    let fixture = support::Fixture::new(pool.clone()).await;
    let expires = at(Duration::days(4));
    record(
        &pool,
        fixture.seed.machine_id,
        "claude",
        expires,
        at(Duration::seconds(-1)),
    )
    .await;
    observe_window(
        &pool,
        &testkit::Seed {
            owner_id: fixture.seed.owner_id,
            machine_id: fixture.seed.machine_id,
            conversation_id: fixture.seed.conversation_id,
        },
    )
    .await;
    let machines = fixture.get("/api/v1/machines").await;
    let credentials = machines[0]["credentials"].as_array().expect("credentials");
    assert_eq!(credentials.len(), 1, "{machines}");
    assert_eq!(credentials[0]["provider"], "claude");
    assert_eq!(
        credentials[0]["expires_at"],
        expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );

    let limits = fixture.get("/api/v1/limits").await;
    let claude = limits
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == "claude")
        .expect("claude limits");
    assert_eq!(
        claude["credential_expires_at"],
        expires.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    Ok(())
}
