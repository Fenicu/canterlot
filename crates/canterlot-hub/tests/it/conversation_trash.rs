//! Корзина владельца: ручное удаление прячет сессию на срок хранения, возврат и
//! окончательная уборка идут через тот же путь, что и прежнее удаление.

use crate::support::task9 as support;

use axum::http::Method;
use canterlot_hub::testkit::TestResponse;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

async fn method(f: &Fixture, method: Method, path: &str, key: &str) -> TestResponse {
    f.client.send(method, path, Some(key), None).await
}

async fn trash(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::DELETE,
        &format!("/api/v1/conversations/{id}"),
        key,
    )
    .await
}

async fn restore(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::POST,
        &format!("/api/v1/conversations/{id}/restore"),
        key,
    )
    .await
}

async fn purge(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::DELETE,
        &format!("/api/v1/conversations/{id}/trash"),
        key,
    )
    .await
}

fn code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn trashed_at(pool: &PgPool, id: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("select trashed_at from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn restore_is_idempotent_and_returns_the_session_as_it_was() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    sqlx::query("update conversations set archived_at=clock_timestamp() where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(trash(&f, f.conversation, "restore-trash").await.status, 204);
    assert_eq!(
        trash(&f, f.conversation, "restore-trash-again")
            .await
            .status,
        204
    );

    let first = restore(&f, f.conversation, "restore-1").await;
    assert_eq!(first.status, 200, "{:?}", first.body);
    assert_eq!(first.json::<Value>()["trashed"], json!(false));
    assert!(trashed_at(&f.pool, f.conversation).await.is_none());
    let archived: bool =
        sqlx::query_scalar("select archived_at is not null from conversations where id=$1")
            .bind(f.conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(archived, "restore keeps the archive flag");

    assert_eq!(
        restore(&f, f.conversation, "restore-1").await.status,
        200,
        "same key replays"
    );
    assert_eq!(
        restore(&f, f.conversation, "restore-2").await.status,
        200,
        "restore is a no-op on a live session"
    );
    let listed = f
        .client
        .send(
            Method::GET,
            "/api/v1/conversations?filter=archived",
            None,
            None,
        )
        .await;
    let listed: Value = listed.json();
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == json!(f.conversation))
    );

    let unknown = restore(&f, Uuid::now_v7(), "restore-unknown").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(code(&unknown), "not_found");
}

#[tokio::test]
async fn purge_endpoint_requires_trash_and_is_idempotent() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let active = f.conversation().await;
    let response = purge(&f, active, "not-trashed").await;
    assert_eq!(response.status, 409);
    assert_eq!(code(&response), "invalid_state");

    assert_eq!(trash(&f, active, "endpoint-trash").await.status, 204);
    let file = f
        .ready_in(active, "gone.txt", "text/plain", &content(32, 3))
        .await;
    let blob = f.blob_path(file);
    assert!(blob.exists());
    assert_eq!(purge(&f, active, "purge-1").await.status, 204);
    assert_eq!(purge(&f, active, "purge-1").await.status, 204, "replay");
    assert!(!blob.exists());
    let again = purge(&f, active, "purge-2").await;
    assert_eq!(again.status, 404);
    assert_eq!(code(&again), "conversation_deleted");
    let unknown = purge(&f, Uuid::now_v7(), "purge-unknown").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(code(&unknown), "not_found");
}
