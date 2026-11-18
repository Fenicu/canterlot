use canterlot_hub::{machine_info, machine_updates, testkit};
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

async fn count(pool: &PgPool, query: &'static str) -> i64 {
    sqlx::query_scalar(query).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn version_available_is_deduped_per_latest_version(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let first = versioned("1.0.0", "1.1.0", "0.9.0", &["a"]);
    apply(&pool, &seed, &first, true).await;
    apply(&pool, &seed, &first, true).await;
    assert_eq!(kinds(&pool).await, vec!["version_available"]);
    let rows = stored(&pool).await;
    assert_eq!(rows[0].3["latest_version"], "1.1.0");
    assert_eq!(rows[0].3["update_command"], "npm i -g @openai/codex@latest");
    let next = versioned("1.0.0", "1.2.0", "0.9.0", &["a"]);
    apply(&pool, &seed, &next, true).await;
    assert_eq!(
        kinds(&pool).await,
        vec!["version_available", "version_available"]
    );
    // Installing the update removes the event: installed catches up with latest.
    let caught_up = versioned("1.2.0", "1.2.0", "0.9.0", &["a"]);
    apply(&pool, &seed, &caught_up, true).await;
    assert_eq!(kinds(&pool).await.len(), 3);
    apply(&pool, &seed, &caught_up, true).await;
    assert_eq!(kinds(&pool).await.len(), 3);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn below_minimum_is_deduped_per_installed_version(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let first = versioned("0.9.0", "1.1.0", "1.0.0", &["a"]);
    apply(&pool, &seed, &first, true).await;
    apply(&pool, &seed, &first, true).await;
    assert_eq!(
        kinds(&pool).await,
        vec!["version_available", "below_minimum"]
    );
    let rows = stored(&pool).await;
    assert_eq!(rows[1].3["installed_version"], "0.9.0");
    assert_eq!(rows[1].3["min_version"], "1.0.0");
    let lower = versioned("0.8.0", "1.1.0", "1.0.0", &["a"]);
    apply(&pool, &seed, &lower, true).await;
    assert_eq!(
        kinds(&pool).await,
        vec![
            "version_available",
            "below_minimum",
            "below_minimum",
            "cli_updated"
        ]
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cli_updated_fires_only_when_installed_version_changes(pool: PgPool) -> TestResult {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn first_report_is_the_model_baseline_and_changes_are_grouped(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.0.0", "0.9.0", &["a", "b"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, Vec::<String>::new());
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.0.0", "0.9.0", &["b", "c"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["models_added", "models_removed"]);
    let rows = stored(&pool).await;
    assert_eq!(rows[0].3["models"], serde_json::json!(["c"]));
    assert_eq!(rows[1].3["models"], serde_json::json!(["a"]));
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.0.0", "0.9.0", &["b", "c"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await.len(), 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_without_the_capability_has_no_version_events(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    apply(
        &pool,
        &seed,
        &versioned("0.9.0", "1.1.0", "1.0.0", &["a"]),
        false,
    )
    .await;
    assert_eq!(kinds(&pool).await, Vec::<String>::new());
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.1.0", "1.0.0", &["b"]),
        false,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["models_added", "models_removed"]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn prerelease_versions_compare_by_the_worker_rules(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    apply(
        &pool,
        &seed,
        &versioned("1.0.0-rc.2", "1.0.0", "0.9.0", &["a"]),
        true,
    )
    .await;
    assert_eq!(kinds(&pool).await, vec!["version_available"]);
    let lower = versioned("1.0.0-rc.2", "1.0.0-rc.10", "0.9.0", &["a"]);
    apply(&pool, &seed, &lower, true).await;
    assert_eq!(kinds(&pool).await.len(), 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_report_apply_leaves_no_events_and_no_state(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::raw_sql("create function fail_updates() returns trigger language plpgsql as $$ begin raise exception 'updates_outage'; end $$; create trigger fail_updates before insert on machine_updates for each row execute function fail_updates();").execute(&pool).await?;
    let result = machine_info::apply_report_with_capabilities(
        &pool,
        seed.owner_id,
        seed.machine_id,
        &versioned("1.0.0", "1.1.0", "0.9.0", &["a"]),
        &versions(),
        Uuid::now_v7(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        count(&pool, "select count(*) from machine_updates").await,
        0
    );
    assert_eq!(
        count(&pool, "select count(*) from machine_harnesses").await,
        0
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn repeated_report_after_restart_does_not_duplicate_events(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let report = versioned("1.0.0", "1.1.0", "0.9.0", &["a"]);
    apply(&pool, &seed, &report, true).await;
    assert_eq!(kinds(&pool).await.len(), 1);
    // Свежее соединение и свежий контекст: всё состояние живёт в базе.
    let restarted = sqlx::pool::PoolOptions::new()
        .connect_with((*pool.connect_options()).clone())
        .await?;
    apply(&restarted, &seed, &report, true).await;
    assert_eq!(kinds(&pool).await.len(), 1);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_update'"
        )
        .await,
        1
    );
    restarted.close().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_new_item_emits_a_live_machine_event(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    apply(
        &pool,
        &seed,
        &versioned("0.9.0", "1.1.0", "1.0.0", &["a"]),
        true,
    )
    .await;
    apply(
        &pool,
        &seed,
        &versioned("1.0.0", "1.1.0", "1.0.0", &["b"]),
        true,
    )
    .await;
    let live: Vec<Value> = sqlx::query_scalar(
        "select payload from events where kind='machine_update' order by scope_seq",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(live.len(), 5);
    assert_eq!(live[0]["kind"], "version_available");
    assert_eq!(live[1]["kind"], "below_minimum");
    assert_eq!(live[2]["kind"], "cli_updated");
    assert_eq!(live[3]["kind"], "models_added");
    assert_eq!(live[4]["kind"], "models_removed");
    for item in &live {
        assert_eq!(item["machine"], "synthetic");
        assert_eq!(item["harness"], "codex");
        assert!(item["id"].is_string());
        assert!(item["created_at"].is_string());
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retention_keeps_the_last_five_hundred_items(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    for i in 0..520 {
        sqlx::query("insert into machine_updates(id,owner_id,machine_id,kind,harness,dedup_key,payload,created_at) values($1,$2,$3,'models_added','codex',$4,'{}',clock_timestamp() - $5 * interval '1 day')")
            .bind(Uuid::now_v7())
            .bind(seed.owner_id)
            .bind(seed.machine_id)
            .bind(format!("key-{i}"))
            .bind(i)
            .execute(&pool)
            .await?;
    }
    // Всё старше 90 дней уходит, даже если записей меньше пятисот.
    assert_eq!(machine_updates::purge(&pool).await?, 430);
    assert_eq!(
        count(&pool, "select count(*) from machine_updates").await,
        90
    );
    // Свежие записи сверх пятисот также уходят.
    for i in 0..600 {
        sqlx::query("insert into machine_updates(id,owner_id,machine_id,kind,harness,dedup_key,payload,created_at) values($1,$2,$3,'models_added','codex',$4,'{}',clock_timestamp() - interval '1 hour')")
            .bind(Uuid::now_v7())
            .bind(seed.owner_id)
            .bind(seed.machine_id)
            .bind(format!("fresh-{i}"))
            .execute(&pool)
            .await?;
    }
    assert_eq!(machine_updates::purge(&pool).await?, 190);
    assert_eq!(
        count(&pool, "select count(*) from machine_updates").await,
        500
    );
    Ok(())
}
