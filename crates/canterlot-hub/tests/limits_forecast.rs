//! `GET /api/v1/limits`: прогноз по темпу в окнах и в снимке списка.

#[path = "support/task7.rs"]
mod support;

use canterlot_hub::{
    config::LimitsConfig,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use support::Fixture;
use uuid::Uuid;

async fn seed_window(
    f: &Fixture,
    provider: &str,
    window: &str,
    used: i32,
    resets_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    snapshots: &[(DateTime<Utc>, i32)],
) {
    for (at, snapshot_used) in snapshots {
        sqlx::query(
            "insert into limit_snapshots(provider,window_kind,used_permille,resets_at,
               observed_at,source,machine_id)
             values($1,$2,$3,$4,$5,'poll',$6)",
        )
        .bind(provider)
        .bind(window)
        .bind(snapshot_used)
        .bind(resets_at)
        .bind(at)
        .bind(f.seed.machine_id)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let sql = match window {
        "weekly" => {
            "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,
               weekly_observed_at)
             values($1,$2,$3,$4)"
        }
        "five_hour" => {
            "insert into provider_limits(provider,five_hour_used_permille,five_hour_resets_at,
               five_hour_observed_at)
             values($1,$2,$3,$4)"
        }
        "monthly" => {
            "insert into provider_limits(provider,monthly_used_permille,monthly_resets_at,
               monthly_observed_at)
             values($1,$2,$3,$4)"
        }
        _ => panic!("unknown window {window}"),
    };
    sqlx::query(sql)
        .bind(provider)
        .bind(used)
        .bind(resets_at)
        .bind(observed_at)
        .execute(&f.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_fresh_window_has_no_forecast(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    // Сброс через полное окно: начало окна совпадает с показанием, прошло 0 часов.
    seed_window(
        &f,
        "claude",
        "weekly",
        900,
        now + TimeDelta::days(7),
        now,
        &[],
    )
    .await;
    let view = f.get("/api/v1/limits").await;
    let weekly = &view[0]["weekly"];
    assert_eq!(weekly["used_pct"], 90.0);
    assert!(weekly.get("forecast").is_none(), "{weekly}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_forecast_lasts_until_the_reset(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::days(2);
    let snapshots = [
        (now - TimeDelta::days(5), 200),
        (now - TimeDelta::days(1), 500),
    ];
    seed_window(&f, "claude", "weekly", 610, reset, now, &snapshots).await;
    let view = f.get("/api/v1/limits").await;
    let forecast = &view[0]["weekly"]["forecast"];
    // Прирост 410 промилле за 120 часов — около 3 промилле в час.
    assert_eq!(
        forecast["rate_permille_per_hour"].as_i64(),
        Some(3),
        "{forecast}"
    );
    assert!(
        forecast["recent_rate_permille_per_hour"].is_null(),
        "{forecast}"
    );
    assert_eq!(
        forecast["before_reset"].as_bool(),
        Some(false),
        "{forecast}"
    );
    assert!(forecast["exhausts_at"].is_string(), "{forecast}");
    // Снимок списка несёт тот же прогноз.
    let snapshot = f.get("/api/v1/snapshot/conversations").await;
    assert_eq!(
        snapshot["data"]["limits"][0]["weekly"]["forecast"],
        *forecast
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_forecast_runs_out_before_the_reset(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::days(6);
    let snapshots = [
        (now - TimeDelta::days(1), 100),
        (now - TimeDelta::hours(3), 800),
    ];
    seed_window(&f, "claude", "weekly", 900, reset, now, &snapshots).await;
    let view = f.get("/api/v1/limits").await;
    let forecast = &view[0]["weekly"]["forecast"];
    // Прирост 800 промилле за сутки — 33.3 в час, остаток 100 кончится за 3 часа.
    assert_eq!(
        forecast["rate_permille_per_hour"].as_i64(),
        Some(33),
        "{forecast}"
    );
    // Недавний темп: 100 промилле за 3 часа — 33.3 в час.
    assert_eq!(
        forecast["recent_rate_permille_per_hour"].as_i64(),
        Some(33),
        "{forecast}"
    );
    assert_eq!(forecast["before_reset"].as_bool(), Some(true), "{forecast}");
    assert!(forecast["exhausts_at"].is_string(), "{forecast}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_observation_emits_the_recomputed_forecast(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::days(6);
    for (at, used) in [
        (now - TimeDelta::days(1), 100),
        (now - TimeDelta::hours(3), 800),
    ] {
        sqlx::query(
            "insert into limit_snapshots(provider,window_kind,used_permille,resets_at,
               observed_at,source,machine_id)
             values('claude','weekly',$1,$2,$3,'poll',$4)",
        )
        .bind(used)
        .bind(reset)
        .bind(at)
        .bind(f.seed.machine_id)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,
           weekly_observed_at)
         values('claude',800,$1,$2)",
    )
    .bind(reset)
    .bind(now - TimeDelta::hours(3))
    .execute(&f.pool)
    .await
    .unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    let recorded = limits::record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: 900,
            resets_at: Some(reset),
            observed_at: now,
            source: ObservationSource::Poll,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        now,
        &LimitsConfig::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(recorded.provider_changed);
    tx.commit().await.unwrap();
    let payload: Value = sqlx::query_scalar(
        "select payload from events where kind='limits_changed' order by scope_seq desc limit 1",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["window"], "weekly");
    let forecast = payload["forecast"].as_object().expect("forecast object");
    let mut keys: Vec<&str> = forecast.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "before_reset",
            "exhausts_at",
            "rate_permille_per_hour",
            "recent_rate_permille_per_hour"
        ],
        "{payload}"
    );
    assert_eq!(forecast["rate_permille_per_hour"].as_i64(), Some(33));
    assert_eq!(forecast["recent_rate_permille_per_hour"].as_i64(), Some(33));
    assert_eq!(forecast["before_reset"].as_bool(), Some(true));
    assert!(forecast["exhausts_at"].is_string(), "{payload}");
    // Снимок списка несёт тот же прогноз, что и событие.
    let view = f.get("/api/v1/limits").await;
    assert_eq!(view[0]["weekly"]["forecast"], payload["forecast"]);
}
