//! Заготовки сообщений: CRUD через настоящий роутер, ревизии, области и события.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit,
    testkit::TestClient,
};
use serde_json::{Value, json};
use sqlx::PgPool;

struct Fx {
    client: TestClient,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self { client, _dir: dir }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> testkit::TestResponse {
        let response = self.client.send(method, path, key, body).await;
        assert_eq!(response.headers["cache-control"], "no-store");
        response
    }

    async fn create(&self, key: &str, body: Value) -> Value {
        let response = self
            .request(
                Method::POST,
                "/api/v1/message-snippets",
                Some(key),
                Some(body),
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            201,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    fn error_code(response: &testkit::TestResponse) -> Value {
        response.json::<Value>()["error"]["code"].clone()
    }
}

#[tokio::test]
async fn crud_filters_scopes_and_rejects_stale_revision() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let global = fx
        .create(
            "snip-global",
            json!({"scope":"global","title":"Общая","body":"Привет"}),
        )
        .await;
    let project = fx
        .create(
            "snip-project",
            json!({"scope":"project","project_key":"/tmp/primer","title":"Проектная","body":"Текст"}),
        )
        .await;
    assert_eq!(global["scope"], "global");
    assert_eq!(global["project_key"], "");
    assert_eq!(global["revision"], 1);
    assert_eq!(project["project_key"], "/tmp/primer");

    let listed: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=global",
            None,
            None,
        )
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], global["id"]);
    let listed: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=project&project_key=%2Ftmp%2Fprimer",
            None,
            None,
        )
        .await
        .json();
    assert_eq!(listed[0]["id"], project["id"]);
    let other: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=project&project_key=%2Ftmp%2Fdrugoe",
            None,
            None,
        )
        .await
        .json();
    assert!(other.as_array().unwrap().is_empty());

    let id = global["id"].as_str().unwrap();
    let updated: Value = fx
        .request(
            Method::PATCH,
            &format!("/api/v1/message-snippets/{id}"),
            Some("snip-update"),
            Some(json!({"body":"Новый текст","base_revision":1})),
        )
        .await
        .json();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["body"], "Новый текст");

    let stale = fx
        .request(
            Method::PATCH,
            &format!("/api/v1/message-snippets/{id}"),
            Some("snip-stale"),
            Some(json!({"body":"Устаревший","base_revision":1})),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&stale), "message_snippet_revision_conflict");
    assert_eq!(
        stale.json::<Value>()["error"]["details"]["current_revision"],
        2
    );

    let reorder = fx
        .request(
            Method::PUT,
            "/api/v1/message-snippets/order",
            Some("snip-order"),
            Some(json!({"scope":"global","ids":[global["id"], project["id"]]})),
        )
        .await;
    // Проектная заготовка не входит в глобальную область: Invalid/NotFound.
    assert!(!reorder.status.is_success());
}
