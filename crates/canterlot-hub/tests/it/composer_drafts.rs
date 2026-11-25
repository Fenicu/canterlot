//! Черновик поля ввода: серверная копия, общая для устройств владельца.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit::{self, TestClient},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    conversation: Uuid,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        testkit::seed_catalog(&pool, machine).await;
        let conversation = testkit::seed_conversation(&pool, owner, machine).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            conversation,
            _dir: dir,
        }
    }

    async fn put(&self, id: Uuid, body: Value, status: u16) -> Value {
        let response = self
            .client
            .send(
                Method::PUT,
                &format!("/api/v1/conversations/{id}/draft"),
                Some(&Uuid::now_v7().to_string()),
                Some(body),
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            status,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        }
    }

    async fn get(&self, id: Uuid) -> Value {
        let response = self
            .client
            .send(
                Method::GET,
                &format!("/api/v1/conversations/{id}/draft"),
                None,
                None,
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    async fn draft_events(&self) -> Vec<Value> {
        sqlx::query_scalar(
            "select payload from events where scope=$1 and kind='draft_changed' order by scope_seq",
        )
        .bind(format!("conversation:{}", self.conversation))
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn stored_text(&self) -> Option<String> {
        sqlx::query_scalar("select text from composer_drafts where conversation_id=$1")
            .bind(self.conversation)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn put_get_round_trip_keeps_revision_and_updated_at() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let empty = f.get(f.conversation).await;
    assert_eq!(empty["text"], "");
    assert_eq!(empty["revision"], 0);
    assert!(empty["updated_at"].is_null());

    let first = f
        .put(
            f.conversation,
            json!({"text":"привет с телефона","base_revision":0,"device_id":"phone"}),
            200,
        )
        .await;
    assert_eq!(first["text"], "привет с телефона");
    let first_revision = first["revision"].as_i64().unwrap();
    assert!(first_revision > 0);
    assert!(first["updated_at"].as_str().unwrap().contains('T'));

    let loaded = f.get(f.conversation).await;
    assert_eq!(loaded["text"], "привет с телефона");
    assert_eq!(loaded["revision"], first_revision);

    // Устаревшая база не перезаписывает свежий черновик.
    f.put(
        f.conversation,
        json!({"text":"продолжил на компьютере","base_revision":first_revision - 1,"device_id":"desktop"}),
        409,
    )
    .await;

    let second = f
        .put(
            f.conversation,
            json!({"text":"продолжил на компьютере","base_revision":first_revision,"device_id":"desktop"}),
            200,
        )
        .await;
    assert_eq!(second["text"], "продолжил на компьютере");
    assert!(second["revision"].as_i64().unwrap() > first_revision);
}

#[tokio::test]
async fn sending_an_input_clears_the_draft_in_the_same_transaction() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    f.put(
        f.conversation,
        json!({"text":"сейчас отправлю","base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;

    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/inputs", f.conversation),
            Some("send-clears-draft"),
            Some(json!({
                "content":[{"type":"text","text":"сейчас отправлю"}],
                "device_id":"phone"
            })),
        )
        .await;
    assert_eq!(
        response.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert!(f.stored_text().await.is_none());
    let events = f.draft_events().await;
    let last = events.last().unwrap();
    assert_eq!(last["text"], "");
    assert_eq!(last["device_id"], "phone");
}
