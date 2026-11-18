use crate::support::task7::Fixture;

use axum::http::Method;
use serde_json::{Value, json};

async fn send(
    f: &Fixture,
    method: Method,
    path: &str,
    key: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let r = f.client.send(method, path, Some(key), body).await;
    (r.status.as_u16(), r.json())
}

fn body(f: &Fixture, cron: &str) -> Value {
    json!({
        "name": "Утренний обзор",
        "cron": cron,
        "target": {"kind": "existing_session", "conversation_id": f.seed.conversation_id},
        "prompt": "посмотреть коммиты",
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_list_get_update_and_delete_a_schedule(pool: sqlx::PgPool) {
    let f = Fixture::new(pool).await;
    let (code, created) = send(
        &f,
        Method::POST,
        "/api/v1/schedules",
        "s-create",
        Some(body(&f, "0 9 * * 1-5")),
    )
    .await;
    assert_eq!(code, 200, "{created}");
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["revision"], 1);
    assert_eq!(created["enabled"], true);
    assert!(created["next_fire_at"].as_str().is_some());

    let (code, list) = send(&f, Method::GET, "/api/v1/schedules", "unused", None).await;
    assert_eq!(code, 200);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id);

    let (code, got) = send(
        &f,
        Method::GET,
        &format!("/api/v1/schedules/{id}"),
        "unused",
        None,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(got["name"], "Утренний обзор");

    let (code, updated) = send(
        &f,
        Method::PATCH,
        &format!("/api/v1/schedules/{id}"),
        "s-update",
        Some(json!({"base_revision": 1, "name": "Вечерний обзор"})),
    )
    .await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated["name"], "Вечерний обзор");
    assert_eq!(updated["revision"], 2);

    let (code, _) = send(
        &f,
        Method::PATCH,
        &format!("/api/v1/schedules/{id}"),
        "s-stale",
        Some(json!({"base_revision": 1, "name": "Устаревшее имя"})),
    )
    .await;
    assert_eq!(code, 409, "stale revision must be refused");

    let (code, _) = send(
        &f,
        Method::DELETE,
        &format!("/api/v1/schedules/{id}"),
        "s-delete",
        None,
    )
    .await;
    assert_eq!(code, 200);
    let (code, _) = send(
        &f,
        Method::GET,
        &format!("/api/v1/schedules/{id}"),
        "unused",
        None,
    )
    .await;
    assert_eq!(code, 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pause_resume_and_preview(pool: sqlx::PgPool) {
    let f = Fixture::new(pool).await;
    let (code, created) = send(
        &f,
        Method::POST,
        "/api/v1/schedules",
        "p-create",
        Some(body(&f, "0 9 * * *")),
    )
    .await;
    assert_eq!(code, 200, "{created}");
    let id = created["id"].as_str().unwrap();

    let (code, paused) = send(
        &f,
        Method::POST,
        &format!("/api/v1/schedules/{id}/pause"),
        "p-pause",
        None,
    )
    .await;
    assert_eq!(code, 200, "{paused}");
    assert_eq!(paused["enabled"], false);
    let (code, resumed) = send(
        &f,
        Method::POST,
        &format!("/api/v1/schedules/{id}/resume"),
        "p-resume",
        None,
    )
    .await;
    assert_eq!(code, 200, "{resumed}");
    assert_eq!(resumed["enabled"], true);

    let (code, preview) = send(
        &f,
        Method::POST,
        "/api/v1/schedules/preview",
        "unused",
        Some(json!({"cron": "0 9 * * *", "timezone": "Europe/Moscow", "count": 3})),
    )
    .await;
    assert_eq!(code, 200, "{preview}");
    let fires = preview.as_array().unwrap();
    assert_eq!(fires.len(), 3);
    assert!(
        fires
            .windows(2)
            .all(|w| w[1].as_str().unwrap() > w[0].as_str().unwrap())
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn invalid_cron_and_too_frequent_are_refused(pool: sqlx::PgPool) {
    let f = Fixture::new(pool).await;
    let (code, _) = send(
        &f,
        Method::POST,
        "/api/v1/schedules",
        "i-bad",
        Some(body(&f, "nope")),
    )
    .await;
    assert_eq!(code, 422);
    let (code, _) = send(
        &f,
        Method::POST,
        "/api/v1/schedules",
        "i-fast",
        Some(body(&f, "*/7 * * * *")),
    )
    .await;
    assert_eq!(code, 422);
}
