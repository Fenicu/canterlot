//! `GET /api/v1/limits`: прогноз по темпу в окнах и в снимке списка.

use crate::support::task7 as support;

use chrono::{DateTime, TimeDelta, Utc};
use support::Fixture;

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

#[tokio::test]
async fn the_forecast_lasts_until_the_reset() {
    let pool = canterlot_hub::testkit::db().await;
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
