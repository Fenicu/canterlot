use canterlot_hub::{machine_info, testkit};
use canterlot_proto::worker::v1::{MachineReport, ModelInfo};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn versions() -> Vec<String> {
    vec!["cli_versions_v1".into()]
}

fn versioned(installed: &str, latest: &str, minimum: &str, models: &[&str]) -> MachineReport {
    let mut report = crate::support::task3::report();
    let harness = &mut report.harnesses[0];
    harness.harness = "codex".into();
    harness.provider = "codex".into();
    harness.installed_version = installed.into();
    harness.latest_version = latest.into();
    harness.min_version = minimum.into();
    harness.update_command = "npm i -g @openai/codex@latest".into();
    harness.models = models
        .iter()
        .map(|id| ModelInfo {
            id: (*id).into(),
            title: (*id).into(),
            ..Default::default()
        })
        .collect();
    report
}

async fn apply(pool: &PgPool, seed: &testkit::Seed, report: &MachineReport, capability: bool) {
    let caps = versions();
    machine_info::apply_report_with_capabilities(
        pool,
        seed.owner_id,
        seed.machine_id,
        report,
        if capability { &caps } else { &[] },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

type Stored = (String, String, String, Value);
async fn stored(pool: &PgPool) -> Vec<Stored> {
    sqlx::query_as(
        "select kind,harness,dedup_key,payload from machine_updates order by created_at,id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn kinds(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar("select kind from machine_updates order by created_at,id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn cli_updated_fires_only_when_installed_version_changes() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.0.0", "0.9.0", &["a"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, Vec::<String>::new());
    apply(
        &pool,
        &seed,
        &versioned("1.1.0", "1.1.0", "0.9.0", &["a"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["cli_updated"]);
    let rows = stored(&pool).await;
    assert_eq!(rows[0].3["from"], "1.0.0");
    assert_eq!(rows[0].3["to"], "1.1.0");
    apply(
        &pool,
        &seed,
        &versioned("1.1.0", "1.1.0", "0.9.0", &["a"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["cli_updated"]);
    apply(
        &pool,
        &seed,
        &versioned("1.2.0", "1.2.0", "0.9.0", &["a"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["cli_updated", "cli_updated"]);
    Ok(())
}
