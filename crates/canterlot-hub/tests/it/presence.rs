//! `POST /api/v1/presence`: отметка «чат открыт» требует сеанс владельца и живёт
//! только в памяти хаба.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit::{self, TestClient},
};
use serde_json::json;
use sqlx::PgPool;
use tokio::time::Instant;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn presence_requires_a_session_and_records_visibility(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let presence = deps.presence.clone();
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Машина')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;

    let app = http::router(AppState::new(deps));
    // CSRF пропускаем, а сеанса нет: отказ обязан быть про личность, не про Origin.
    let anonymous = TestClient::in_process(app.clone())
        .with_header("Origin", testkit::TEST_ORIGIN)
        .with_header("X-Canterlot-Client", "1");
    let client = TestClient::in_process(app).as_owner();
    let body = json!({"conversation_id": conversation, "visible": true});

    let refused = anonymous
        .send(Method::POST, "/api/v1/presence", None, Some(body.clone()))
        .await;
    assert_eq!(refused.status.as_u16(), 401);
    assert!(!presence.is_visible(conversation, Instant::now()));

    let accepted = client
        .send(Method::POST, "/api/v1/presence", None, Some(body))
        .await;
    assert_eq!(accepted.status.as_u16(), 204);
    assert!(presence.is_visible(conversation, Instant::now()));

    let hidden = client
        .send(
            Method::POST,
            "/api/v1/presence",
            None,
            Some(json!({"conversation_id": conversation, "visible": false})),
        )
        .await;
    assert_eq!(hidden.status.as_u16(), 204);
    assert!(!presence.is_visible(conversation, Instant::now()));

    let unknown = client
        .send(
            Method::POST,
            "/api/v1/presence",
            None,
            Some(json!({"conversation_id": Uuid::now_v7(), "visible": true})),
        )
        .await;
    assert_eq!(unknown.status.as_u16(), 404);
}
