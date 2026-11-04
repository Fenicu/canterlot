//! `limits.report`: все провайдеры и их окна, свежесть наблюдения, порядок с текущим первым.

use std::sync::Arc;

use canterlot_hub::{
    config::SessionToolsConfig,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit,
};
use chrono::{TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
        },
        alarms_enabled: true,
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_report_lists_every_provider_with_windows_and_freshness(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let fresh = now - TimeDelta::minutes(2);
    let stale = now - TimeDelta::minutes(30);
    let reset = now + TimeDelta::hours(2);
    for (provider, used, seen, monthly) in [
        ("claude", 120, fresh, None),
        ("codex", 340, stale, None),
        ("opencode", 93, fresh, Some(7)),
    ] {
        sqlx::query(
            "insert into provider_limits(provider,five_hour_used_permille,five_hour_resets_at,
               five_hour_observed_at,weekly_used_permille,weekly_resets_at,weekly_observed_at,
               monthly_used_permille,monthly_resets_at,monthly_observed_at)
             values($1,$2,$3,$4,$5,$3,$4,$6,$3,$4)",
        )
        .bind(provider)
        .bind(used)
        .bind(reset)
        .bind(seen)
        .bind(used + 100)
        .bind(monthly)
        .execute(&pool)
        .await
        .unwrap();
    }
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let report = session_tools::call(
        &context(&pool),
        &caller,
        "limits.report",
        json!({"provider": "claude"}),
    )
    .await
    .unwrap();
    let providers = report["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 3);
    assert_eq!(providers[0]["provider"], "claude");
    let text = report["text"].as_str().unwrap();
    assert!(
        text.starts_with("Claude API budget: five-hour window 12% used, resets "),
        "{text}"
    );
    assert!(text.contains("weekly window 22% used"));
    assert!(text.contains("Codex API budget"));
    assert!(text.contains("OpenCode API budget"));
    assert!(text.contains("monthly window 0.7% used"));
    assert!(text.contains("Observed:"));
    assert!(
        text.contains("codex 29 min ago") || text.contains("codex 30 min ago"),
        "{text}"
    );
    let claude = &providers[0]["windows"];
    assert_eq!(claude[0]["fresh"], true);
    assert_eq!(claude[1]["window"], "weekly");
    assert_eq!(claude[2]["window"], "monthly");
    assert_eq!(claude[2]["absent"], true);
    let codex = &providers[1]["windows"];
    assert_eq!(codex[0]["fresh"], false);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_report_without_data_says_so(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let report = session_tools::call(&context(&pool), &caller, "limits.report", json!({}))
        .await
        .unwrap();
    assert_eq!(report["providers"].as_array().unwrap().len(), 0);
    assert_eq!(report["text"], "no limit data yet in this session");
}
