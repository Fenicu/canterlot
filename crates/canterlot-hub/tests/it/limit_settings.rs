use crate::support::task7 as support;

use axum::http::Method;
use canterlot_hub::{config::LimitsConfig, limit_settings};
use serde_json::{Value, json};
use support::Fixture;
use uuid::Uuid;

async fn put(f: &Fixture, key: &str, body: Value) -> axum::http::StatusCode {
    f.client
        .send(Method::PUT, "/api/v1/limit-settings", Some(key), Some(body))
        .await
        .status
}

#[tokio::test]
async fn settings_default_save_reset_and_config_follow_owner() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool.clone()).await;
    let defaults = json!({
        "warn_remaining_pct": 20,
        "danger_remaining_pct": 5,
        "reserve_remaining_pct": 5
    });
    let view: Value = f.get("/api/v1/limit-settings").await;
    assert_eq!(
        view,
        json!({
            "settings": defaults,
            "is_default": true,
            "default_settings": defaults
        })
    );

    assert_eq!(
        put(
            &f,
            "limits-save",
            json!({"settings": {"warn_remaining_pct": 35, "danger_remaining_pct": 12, "reserve_remaining_pct": 10}})
        )
        .await,
        200
    );
    let view: Value = f.get("/api/v1/limit-settings").await;
    assert_eq!(view["is_default"], false);
    assert_eq!(
        view["settings"],
        json!({"warn_remaining_pct": 35, "danger_remaining_pct": 12, "reserve_remaining_pct": 10})
    );

    let config: Value = f.get("/api/v1/config").await;
    assert_eq!(config["limit_thresholds"]["warn_remaining_pct"], 35);
    assert_eq!(config["limit_thresholds"]["danger_remaining_pct"], 12);
    assert_eq!(config["limit_thresholds"]["reserve_remaining_pct"], 10);
    // Совместимые поля остаются: резерв как расход и устаревший warn_pcts из конфига.
    assert_eq!(config["limit_thresholds"]["weekly_reserve_pct"], 90);
    assert_eq!(config["limit_thresholds"]["warn_pcts"], json!([80, 90, 95]));

    // Чужая строка не видна: у другого владельца свои умолчания.
    let other = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let other_view = limit_settings::view(&mut conn, other, &LimitsConfig::default())
        .await
        .unwrap();
    assert!(other_view.is_default);
    assert_eq!(other_view.settings.reserve_remaining_pct, 5);

    assert_eq!(
        put(&f, "limits-reset", json!({"settings": null})).await,
        200
    );
    let view: Value = f.get("/api/v1/limit-settings").await;
    assert_eq!(view["is_default"], true);
    assert_eq!(view["settings"], defaults);
    let config: Value = f.get("/api/v1/config").await;
    assert_eq!(config["limit_thresholds"]["warn_remaining_pct"], 20);
    assert_eq!(config["limit_thresholds"]["weekly_reserve_pct"], 95);
}
