//! HTTP владельца для правила автопинка: чтение, пауза, снятие (B2).

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit::{self, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    client: TestClient,
    session: Uuid,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Машина')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let session = testkit::seed_conversation(&pool, owner, machine).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            client,
            session,
            _dir: dir,
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> TestResponse {
        self.client.send(method, path, key, body).await
    }
}

#[tokio::test]
async fn owner_creates_pauses_resumes_and_clears_a_rule() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let path = format!("/api/v1/conversations/{}/nudge", f.session);

    let created: Value = f
        .send(
            Method::PUT,
            &path,
            Some("nudge_put_1"),
            Some(json!({"idle_minutes": 5, "max_nudges": 2})),
        )
        .await
        .json();
    assert_eq!(created["session_id"], f.session.to_string());
    assert_eq!(created["set_by"], "owner");
    assert!(created["set_by_session"].is_null());
    assert_eq!(created["state"], "watching");
    assert_eq!(created["idle_minutes"], 5);
    assert_eq!(created["max_consecutive"], 2);

    let read: Value = f.send(Method::GET, &path, None, None).await.json();
    assert_eq!(read["rule"]["state"], "watching");
    assert!(read["events"].is_array());

    let paused: Value = f
        .send(
            Method::PUT,
            &path,
            Some("nudge_put_2"),
            Some(json!({"paused": true})),
        )
        .await
        .json();
    assert_eq!(paused["state"], "paused");
    let resumed: Value = f
        .send(
            Method::PUT,
            &path,
            Some("nudge_put_3"),
            Some(json!({"paused": false})),
        )
        .await
        .json();
    assert_eq!(resumed["state"], "watching");

    let cleared = f
        .send(Method::DELETE, &path, Some("nudge_delete"), None)
        .await;
    assert_eq!(cleared.status.as_u16(), 204);
    let read: Value = f.send(Method::GET, &path, None, None).await.json();
    assert!(read["rule"].is_null());
    // Снятие идемпотентно: повторное удаление тоже 204.
    let again = f
        .send(Method::DELETE, &path, Some("nudge_delete_2"), None)
        .await;
    assert_eq!(again.status.as_u16(), 204);
}
