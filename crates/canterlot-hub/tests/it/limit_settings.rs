use crate::support::task7 as support;

use axum::http::Method;
use canterlot_hub::{config::LimitsConfig, limit_settings};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::Fixture;
use uuid::Uuid;

async fn put(f: &Fixture, key: &str, body: Value) -> axum::http::StatusCode {
    f.client
        .send(Method::PUT, "/api/v1/limit-settings", Some(key), Some(body))
        .await
        .status
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn settings_default_save_reset_and_config_follow_owner(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn invalid_settings_are_rejected_with_a_clear_code(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for (key, settings) in [
        (
            "limits-danger-above-warn",
            json!({"warn_remaining_pct": 10, "danger_remaining_pct": 11, "reserve_remaining_pct": 5}),
        ),
        (
            "limits-too-big",
            json!({"warn_remaining_pct": 100, "danger_remaining_pct": 5, "reserve_remaining_pct": 5}),
        ),
    ] {
        let response = f
            .client
            .send(
                Method::PUT,
                "/api/v1/limit-settings",
                Some(key),
                Some(json!({"settings": settings})),
            )
            .await;
        assert_eq!(
            response.status,
            422,
            "{key}: {}",
            String::from_utf8_lossy(&response.body)
        );
        assert_eq!(
            response.json::<Value>()["error"]["code"],
            "limit_settings_invalid"
        );
    }
    // Отрицательное число не проходит разбор в u16: это тоже 4xx, но общий invalid_request.
    let response = f
        .client
        .send(
            Method::PUT,
            "/api/v1/limit-settings",
            Some("limits-negative"),
            Some(
                json!({"settings": {"warn_remaining_pct": 20, "danger_remaining_pct": 5, "reserve_remaining_pct": -1}}),
            ),
        )
        .await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json::<Value>()["error"]["code"], "invalid_request");
    // Пустое тело не сбрасывает: поле `settings` обязательно.
    let response = f
        .client
        .send(
            Method::PUT,
            "/api/v1/limit-settings",
            Some("limits-empty"),
            Some(json!({})),
        )
        .await;
    assert_eq!(response.status, 400);
    let view: Value = f.get("/api/v1/limit-settings").await;
    assert_eq!(view["is_default"], true);
}
