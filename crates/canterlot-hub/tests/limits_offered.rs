//! Лимиты показываются только для провайдеров, чей харнесс есть в каталоге машин владельца.
//! Строки и история остальных остаются в базе и возвращаются вместе с харнессом.

#[path = "support/task3.rs"]
mod support;
use canterlot_hub::{
    config::LimitsConfig,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    machine_info, snapshot, testkit,
};
use canterlot_proto::worker::v1::HarnessInfo;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use support::report;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn observe(pool: &PgPool, s: &testkit::Seed, provider: &str, window: Window, used: u16) {
    let when = Utc::now() - Duration::minutes(5);
    let mut tx = pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        s.owner_id,
        &Observation {
            provider: provider.into(),
            window,
            used_permille: used,
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

async fn shown(pool: &PgPool, owner_id: Uuid) -> Vec<String> {
    let mut conn = pool.acquire().await.unwrap();
    limits::shown(&mut conn, owner_id)
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.provider)
        .collect()
}

async fn changes(pool: &PgPool, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("select payload from events where kind=$1 order by scope_seq")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn harness(id: &str, provider: &str) -> HarnessInfo {
    HarnessInfo {
        harness: id.into(),
        provider: provider.into(),
        title: id.into(),
        ..Default::default()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn providers_outside_the_catalog_are_hidden_but_their_rows_and_history_stay(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, s.machine_id, "claude").await;
    observe(&pool, &s, "claude", Window::Weekly, 410).await;
    observe(&pool, &s, "antigravity", Window::FiveHour, 200).await;
    observe(&pool, &s, "antigravity", Window::Weekly, 300).await;

    assert_eq!(shown(&pool, s.owner_id).await, ["claude"]);
    let list = snapshot::conversations(&pool, s.owner_id).await?;
    let listed: Vec<String> = serde_json::to_value(&list.data)?["limits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["provider"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        listed,
        ["claude"],
        "the list snapshot hides antigravity too"
    );
    // Событие о свежем показании провайдера вне каталога клиентам не уходит.
    let providers: Vec<Value> = changes(&pool, "limits_changed")
        .await
        .into_iter()
        .map(|p| p["provider"].clone())
        .collect();
    assert_eq!(providers, ["claude"]);

    let mut conn = pool.acquire().await?;
    let stored: Vec<String> = limits::current(&mut conn)
        .await?
        .into_iter()
        .map(|p| p.provider)
        .collect();
    assert_eq!(stored, ["antigravity", "claude"], "rows are not deleted");
    let history: i64 =
        sqlx::query_scalar("select count(*) from limit_snapshots where provider='antigravity'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(history, 2, "history is not deleted");

    // Другой владелец со своим харнессом antigravity не открывает его лимиты этому.
    let other = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, other.machine_id, "antigravity").await;
    assert_eq!(shown(&pool, s.owner_id).await, ["claude"]);
    assert_eq!(shown(&pool, other.owner_id).await, ["antigravity"]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_harness_leaving_and_returning_tells_clients_and_brings_the_windows_back(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    let mut with = report();
    with.harnesses.push(harness("antigravity", "antigravity"));
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &with, Uuid::now_v7()).await?;
    observe(&pool, &s, "claude", Window::Weekly, 410).await;
    observe(&pool, &s, "antigravity", Window::Weekly, 300).await;
    assert_eq!(shown(&pool, s.owner_id).await, ["antigravity", "claude"]);
    let before = changes(&pool, "limits_providers_changed").await.len();

    // Харнесс выключен: каталог без antigravity.
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &report(), Uuid::now_v7()).await?;
    assert_eq!(shown(&pool, s.owner_id).await, ["claude"]);
    let offered = changes(&pool, "limits_providers_changed").await;
    assert_eq!(offered.len(), before + 1);
    assert_eq!(
        offered.last().unwrap()["providers"],
        serde_json::json!(["claude"])
    );

    // Тот же каталог повторно событий не порождает.
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &report(), Uuid::now_v7()).await?;
    assert_eq!(
        changes(&pool, "limits_providers_changed").await.len(),
        before + 1
    );

    // Харнесс включён снова: провайдер возвращается вместе с сохранённым окном.
    let weekly_before = changes(&pool, "limits_changed").await.len();
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &with, Uuid::now_v7()).await?;
    assert_eq!(shown(&pool, s.owner_id).await, ["antigravity", "claude"]);
    let offered = changes(&pool, "limits_providers_changed").await;
    assert_eq!(
        offered.last().unwrap()["providers"],
        serde_json::json!(["antigravity", "claude"])
    );
    let returned = changes(&pool, "limits_changed").await;
    let back: Vec<&Value> = returned[weekly_before..]
        .iter()
        .filter(|p| p["provider"] == "antigravity")
        .collect();
    assert_eq!(back.len(), 1, "only the stored weekly window is resent");
    assert_eq!(back[0]["window"], "weekly");
    assert_eq!(back[0]["used_permille"], 300);
    Ok(())
}
