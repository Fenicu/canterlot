//! `limits.report`: провайдеры, которые видит PWA, и их окна, свежесть наблюдения,
//! порядок с текущим первым, устаревшие окна без прогноза.

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
        memory: Default::default(),
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: std::time::Duration::from_secs(1800),
        routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        cloud: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

#[tokio::test]
async fn limits_report_lists_every_provider_with_windows_and_freshness() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    for provider in ["claude", "codex", "opencode"] {
        testkit::offer_provider(&pool, seed.machine_id, provider).await;
    }
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
    let find = |name: &str| {
        providers
            .iter()
            .find(|row| row["provider"] == name)
            .unwrap()
    };
    let text = report["text"].as_str().unwrap();
    assert!(
        text.starts_with("Claude API budget: five-hour window 12% used, resets in "),
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
    let claude = &find("claude")["windows"];
    assert_eq!(claude[0]["fresh"], true);
    assert_eq!(claude[1]["window"], "weekly");
    assert_eq!(claude[2]["window"], "weekly_fable");
    assert_eq!(claude[2]["absent"], true);
    assert_eq!(claude[3]["window"], "monthly");
    assert_eq!(claude[3]["absent"], true);
    let codex = &find("codex")["windows"];
    assert_eq!(codex[0]["fresh"], false);
    assert_eq!(codex[0]["stale"], false);
    assert_eq!(codex[0]["reset_passed"], false);
}

fn caller(seed: &testkit::Seed) -> Caller {
    Caller {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    }
}

async fn insert_weekly_with_forecast(
    pool: &PgPool,
    machine_id: Uuid,
    provider: &str,
    reset: chrono::DateTime<Utc>,
    snapshots: &[(chrono::DateTime<Utc>, i32)],
    used: i32,
    observed_at: chrono::DateTime<Utc>,
) {
    for (at, snapshot_used) in snapshots {
        sqlx::query(
            "insert into limit_snapshots(provider,window_kind,used_permille,resets_at,
               observed_at,source,machine_id)
             values($1,'weekly',$2,$3,$4,'poll',$5)",
        )
        .bind(provider)
        .bind(snapshot_used)
        .bind(reset)
        .bind(at)
        .bind(machine_id)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,
           weekly_observed_at)
         values($1,$2,$3,$4)",
    )
    .bind(provider)
    .bind(used)
    .bind(reset)
    .bind(observed_at)
    .execute(pool)
    .await
    .unwrap();
}

/// Провайдер без харнесса в каталоге машин владельца скрыт и в PWA, и в инструменте,
/// включая строку `Observed:`; видимые провайдеры описаны как прежде.
#[tokio::test]
async fn limits_report_hides_providers_the_pwa_hides() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "claude").await;
    testkit::offer_provider(&pool, seed.machine_id, "codex").await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(2);
    for (provider, used) in [("claude", 120), ("codex", 340), ("gemini", 0)] {
        sqlx::query(
            "insert into provider_limits(provider,five_hour_used_permille,five_hour_resets_at,
               five_hour_observed_at)
             values($1,$2,$3,$4)",
        )
        .bind(provider)
        .bind(used)
        .bind(reset)
        .bind(now - TimeDelta::minutes(2))
        .execute(&pool)
        .await
        .unwrap();
    }
    let report = session_tools::call(&context(&pool), &caller(&seed), "limits.report", json!({}))
        .await
        .unwrap();
    let names: Vec<&str> = report["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["provider"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["claude", "codex"]);
    let text = report["text"].as_str().unwrap();
    assert!(!text.contains("Gemini"), "{text}");
    assert!(!text.contains("gemini"), "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].starts_with("Claude API budget: five-hour window 12% used, resets in 1h 59m"),
        "{text}"
    );
    assert!(
        lines[1].starts_with("Codex API budget: five-hour window 34% used, resets in 1h 59m"),
        "{text}"
    );
    assert_eq!(lines[2], "Observed: claude 2 min ago, codex 2 min ago.");
}

/// Ни одного провайдера в каталоге: инструмент говорит, что данных нет, как пустая полоса PWA.
#[tokio::test]
async fn limits_report_without_offered_providers_says_no_data() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    sqlx::query(
        "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,weekly_observed_at)
         values('gemini',1,now()+interval '3 days',now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    let report = session_tools::call(&context(&pool), &caller(&seed), "limits.report", json!({}))
        .await
        .unwrap();
    assert_eq!(report["providers"].as_array().unwrap().len(), 0);
    assert_eq!(report["text"], "no limit data yet in this session");
}

/// Сброс окна уже прошёл: инструмент говорит, что данные устарели, не обещает сброс
/// «меньше чем через минуту» и не даёт прогноз; окно без прошедшего сброса не меняется.
#[tokio::test]
async fn limits_report_marks_a_passed_reset_as_stale() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "codex").await;
    let now = Utc::now();
    let seen = now - TimeDelta::hours(61);
    sqlx::query(
        "insert into provider_limits(provider,five_hour_used_permille,five_hour_resets_at,
           five_hour_observed_at,weekly_used_permille,weekly_resets_at,weekly_observed_at)
         values('codex',420,$1,$2,1,$3,$2)",
    )
    .bind(seen + TimeDelta::hours(3))
    .bind(seen)
    .bind(now + TimeDelta::days(3) + TimeDelta::hours(2) + TimeDelta::minutes(1))
    .execute(&pool)
    .await
    .unwrap();
    let report = session_tools::call(&context(&pool), &caller(&seed), "limits.report", json!({}))
        .await
        .unwrap();
    let text = report["text"].as_str().unwrap();
    assert_eq!(
        text,
        "Codex API budget: five-hour window: data from 61 h ago, reset already passed; \
         weekly window 0.1% used (stale, data from 61 h ago), resets in 3d 2h.\n\
         Observed: codex 61 h ago."
    );
    assert!(!text.contains("less than a minute"), "{text}");
    let windows = &report["providers"][0]["windows"];
    assert_eq!(windows[0]["stale"], true);
    assert_eq!(windows[0]["reset_passed"], true);
    assert_eq!(windows[0]["forecast"], serde_json::Value::Null);
    assert_eq!(windows[1]["stale"], true);
    assert_eq!(windows[1]["reset_passed"], false);
}

/// Прогноз есть у свежего окна и пропадает у окна, чьи данные старше `limits.stale_after`
/// или чей сброс уже прошёл.
#[tokio::test]
async fn limits_report_suppresses_the_forecast_for_stale_windows() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    for provider in ["claude", "codex", "opencode"] {
        testkit::offer_provider(&pool, seed.machine_id, provider).await;
    }
    let now = Utc::now();
    let history = |end: chrono::DateTime<Utc>| {
        vec![
            (end - TimeDelta::days(1), 100),
            (end - TimeDelta::hours(12), 400),
            (end - TimeDelta::hours(3), 800),
        ]
    };
    // Свежее окно: прогноз есть.
    insert_weekly_with_forecast(
        &pool,
        seed.machine_id,
        "claude",
        now + TimeDelta::days(6),
        &history(now),
        900,
        now,
    )
    .await;
    // Наблюдение старше stale_after (35 минут по умолчанию), сброс впереди.
    let old = now - TimeDelta::hours(2);
    insert_weekly_with_forecast(
        &pool,
        seed.machine_id,
        "codex",
        now + TimeDelta::days(6),
        &history(old),
        900,
        old,
    )
    .await;
    // Сброс прошёл.
    let older = now - TimeDelta::days(8);
    insert_weekly_with_forecast(
        &pool,
        seed.machine_id,
        "opencode",
        now - TimeDelta::days(1),
        &history(older),
        900,
        older,
    )
    .await;
    let report = session_tools::call(&context(&pool), &caller(&seed), "limits.report", json!({}))
        .await
        .unwrap();
    let text = report["text"].as_str().unwrap();
    let line = |label: &str| {
        text.lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("{label} missing in {text}"))
    };
    assert!(
        line("Claude API budget").contains("at the average pace"),
        "{text}"
    );
    let codex = line("Codex API budget");
    assert!(!codex.contains("average pace"), "{text}");
    assert!(
        codex.contains("(stale, data from 2 h ago), resets in 5d 23h"),
        "{text}"
    );
    let opencode = line("OpenCode API budget");
    assert!(!opencode.contains("average pace"), "{text}");
    assert!(
        opencode.contains("weekly window: data from 192 h ago, reset already passed"),
        "{text}"
    );
    let find = |name: &str| {
        report["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == name)
            .unwrap()["windows"][1]
            .clone()
    };
    assert!(find("claude")["forecast"].is_object());
    assert_eq!(find("codex")["forecast"], serde_json::Value::Null);
    assert_eq!(find("opencode")["forecast"], serde_json::Value::Null);
}

#[tokio::test]
async fn limits_report_includes_the_countdown_and_the_forecast() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "claude").await;
    testkit::offer_provider(&pool, seed.machine_id, "codex").await;
    let now = Utc::now();
    // Claude: окно недели началось сутки назад, 900 промилле — кончится раньше сброса.
    let claude_reset = now + TimeDelta::days(6);
    // Codex: окно началось пять дней назад, 610 промилле — до сброса хватает.
    let codex_reset = now + TimeDelta::days(2);
    let mut tx = pool.begin().await.unwrap();
    for (provider, reset, snapshots, used) in [
        (
            "claude",
            claude_reset,
            vec![
                (now - TimeDelta::days(1), 100),
                (now - TimeDelta::hours(12), 400),
                (now - TimeDelta::hours(3), 800),
            ],
            900,
        ),
        (
            "codex",
            codex_reset,
            vec![
                (now - TimeDelta::days(5), 200),
                (now - TimeDelta::days(1), 500),
            ],
            610,
        ),
    ] {
        for (observed_at, snapshot_used) in snapshots {
            sqlx::query(
                "insert into limit_snapshots(provider,window_kind,used_permille,resets_at,
                   observed_at,source,machine_id)
                 values($1,'weekly',$2,$3,$4,'poll',$5)",
            )
            .bind(provider)
            .bind(snapshot_used)
            .bind(reset)
            .bind(observed_at)
            .bind(seed.machine_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,
               weekly_observed_at)
             values($1,$2,$3,$4)",
        )
        .bind(provider)
        .bind(used)
        .bind(reset)
        .bind(now)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
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
    let text = report["text"].as_str().unwrap();
    assert!(text.contains("resets in "), "{text}");
    assert!(text.contains("at the average pace ("), "{text}");
    assert!(text.contains("runs out in "), "{text}");
    assert!(text.contains("before the reset"), "{text}");
    assert!(text.contains("lasts until the reset"), "{text}");
    let find = |name: &str| {
        report["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == name)
            .unwrap()
    };
    let claude = &find("claude")["windows"][1]["forecast"];
    assert_eq!(claude["before_reset"], true);
    assert!(claude["exhausts_at"].is_string());
    assert!(claude["rate_permille_per_hour"].as_i64().unwrap() > 0);
    let codex = &find("codex")["windows"][1]["forecast"];
    assert_eq!(codex["before_reset"], false);
    assert_eq!(
        codex["recent_rate_permille_per_hour"],
        serde_json::Value::Null
    );
}

/// Вторая машина владельца с тем же провайдером и другой учётной записью лимитов.
async fn second_account_machine(pool: &PgPool, owner: Uuid, name: &str, account: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,$3)")
        .bind(id)
        .bind(owner)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title,account) values($1,'opencode','opencode','OpenCode',$2)")
        .bind(id)
        .bind(account)
        .execute(pool)
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn limits_report_prints_a_line_per_account_and_names_it_only_when_there_are_several() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "opencode").await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(2);
    let insert = |account: &'static str, used: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into provider_limits(provider,account,weekly_used_permille,weekly_resets_at,weekly_observed_at)
                 values('opencode',$1,$2,$3,$4)",
            )
            .bind(account)
            .bind(used)
            .bind(reset)
            .bind(now - TimeDelta::minutes(1))
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    insert("owner", 120).await;
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let one = session_tools::call(&context(&pool), &caller, "limits.report", json!({}))
        .await
        .unwrap();
    let text = one["text"].as_str().unwrap();
    assert!(
        text.starts_with("OpenCode API budget: weekly window 12% used"),
        "one account keeps today's text: {text}"
    );
    // Вторая учётная запись того же провайдера на другой машине.
    second_account_machine(&pool, seed.owner_id, "machine-b", "b-opencode").await;
    insert("b-opencode", 700).await;
    let two = session_tools::call(&context(&pool), &caller, "limits.report", json!({}))
        .await
        .unwrap();
    let text = two["text"].as_str().unwrap();
    assert!(
        text.contains("OpenCode API budget (b-opencode): weekly window 70% used"),
        "{text}"
    );
    assert!(
        text.contains("OpenCode API budget (owner): weekly window 12% used"),
        "{text}"
    );
    let rows = two["providers"].as_array().unwrap();
    let accounts: Vec<&str> = rows
        .iter()
        .map(|row| row["account"].as_str().unwrap())
        .collect();
    assert_eq!(accounts, vec!["b-opencode", "owner"]);
    assert!(text.contains("Observed: opencode b-opencode"), "{text}");
}

#[tokio::test]
async fn credentials_and_credits_are_listed_per_machine_and_never_mixed_across_accounts() {
    let pool = canterlot_hub::testkit::db().await;
    use canterlot_proto::worker::v1::{ProviderCredits, ResetCredit};
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "opencode").await;
    let b = second_account_machine(&pool, seed.owner_id, "machine-b", "b-opencode").await;
    let now = Utc::now();
    let early = now + TimeDelta::days(2);
    let late = now + TimeDelta::days(9);
    for (machine, expires) in [(seed.machine_id, late), (b, early)] {
        canterlot_hub::limits::record_credential_in_tx(
            &mut pool.acquire().await.unwrap(),
            machine,
            "opencode",
            expires,
            now,
            now,
        )
        .await
        .unwrap();
    }
    for (machine, count) in [(seed.machine_id, 4), (b, 1)] {
        canterlot_hub::limits::record_credits_in_tx(
            &mut pool.acquire().await.unwrap(),
            machine,
            "opencode",
            &ProviderCredits {
                provider: "opencode".into(),
                resets_known: true,
                reset_count: count,
                resets: vec![ResetCredit::default()],
                ..Default::default()
            },
            now,
            now,
        )
        .await
        .unwrap();
    }
    for account in ["owner", "b-opencode"] {
        sqlx::query(
            "insert into provider_limits(provider,account,weekly_used_permille,weekly_resets_at,weekly_observed_at)
             values('opencode',$1,100,$2,$3)",
        )
        .bind(account)
        .bind(now + TimeDelta::hours(2))
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    let rows = canterlot_hub::limits::shown(&mut conn, seed.owner_id)
        .await
        .unwrap();
    let row = |account: &str| {
        rows.iter()
            .find(|row| row.provider == "opencode" && row.account == account)
            .unwrap()
    };
    // B вошёл раньше, но его дата не попадает в строку владельца и наоборот.
    let owner = row("owner");
    assert_eq!(
        owner.credential_expires_at.map(|t| t.timestamp()),
        Some(late.timestamp())
    );
    assert_eq!(owner.machine_credentials.len(), 1);
    assert_eq!(owner.machine_credentials[0].machine_id, seed.machine_id);
    assert_eq!(owner.credits.as_ref().unwrap().reset_count, Some(4));
    assert_eq!(owner.machine_credits.len(), 1);
    let b_row = row("b-opencode");
    assert_eq!(
        b_row.credential_expires_at.map(|t| t.timestamp()),
        Some(early.timestamp())
    );
    assert_eq!(b_row.machine_credentials.len(), 1);
    assert_eq!(b_row.machine_credentials[0].machine_id, b);
    assert_eq!(b_row.machine_credentials[0].machine_name, "machine-b");
    assert_eq!(b_row.credits.as_ref().unwrap().reset_count, Some(1));
    assert_eq!(b_row.machine_credits[0].machine_id, b);
    // Событие лимитов несёт те же списки по машинам.
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::limits::record_observation_in_tx(
        &mut tx,
        seed.owner_id,
        &canterlot_hub::limits::Observation {
            provider: "opencode".into(),
            account: "b-opencode".into(),
            window: canterlot_hub::limits::Window::Weekly,
            used_permille: 150,
            resets_at: Some(now + TimeDelta::hours(2)),
            observed_at: now + TimeDelta::seconds(5),
            source: canterlot_hub::limits::ObservationSource::Poll,
            data_confidence: canterlot_hub::limits::DataConfidence::Exact,
            machine_id: b,
            execution_id: None,
        },
        now + TimeDelta::seconds(5),
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let payload: serde_json::Value = sqlx::query_scalar(
        "select payload from events where kind='limits_changed' and payload->>'account'='b-opencode' order by id desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        payload["machine_credentials"][0]["machine_id"],
        b.to_string()
    );
    assert_eq!(payload["machine_credits"][0]["reset_count"], 1);
}

#[tokio::test]
async fn limits_report_without_data_says_so() {
    let pool = canterlot_hub::testkit::db().await;
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
