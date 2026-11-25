//! Избранные сообщения разговора: хранение, HTTP API и событие `bookmark_changed`.

use axum::http::Method;
use canterlot_hub::{
    delivery::{self, ContentBlock, SubmitInput},
    http::{self, AppState},
    testkit::{self, TestClient, TestResponse},
};
use chrono::{TimeDelta, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
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
        Self::execution(&pool, conversation).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            owner,
            conversation,
            _dir: dir,
        }
    }

    async fn execution(pool: &PgPool, conversation: Uuid) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state)
             values($1,$2,1,'stopped')",
        )
        .bind(id)
        .bind(conversation)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn input(&self, conversation: Uuid, text: &str, seq: i64) -> Uuid {
        let submitted = delivery::submit_input(
            &self.pool,
            SubmitInput {
                owner_id: self.owner,
                conversation_id: conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text: text.into() }],
                received_at: Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap();
        sqlx::query("update inputs set feed_seq=$2,created_at=$3 where id=$1")
            .bind(submitted.input_id)
            .bind(seq)
            .bind(Utc::now() + TimeDelta::milliseconds(seq))
            .execute(&self.pool)
            .await
            .unwrap();
        submitted.input_id
    }

    fn path(&self, conversation: Uuid, kind: &str, target: Uuid) -> String {
        format!("/api/v1/conversations/{conversation}/bookmarks/{kind}/{target}")
    }

    async fn put(&self, conversation: Uuid, kind: &str, target: Uuid) -> TestResponse {
        self.client
            .send(
                Method::PUT,
                &self.path(conversation, kind, target),
                None,
                None,
            )
            .await
    }

    async fn delete(&self, conversation: Uuid, kind: &str, target: Uuid) -> TestResponse {
        self.client
            .send(
                Method::DELETE,
                &self.path(conversation, kind, target),
                None,
                None,
            )
            .await
    }

    async fn list(&self, conversation: Uuid) -> Vec<Value> {
        let response = self
            .client
            .send(
                Method::GET,
                &format!("/api/v1/conversations/{conversation}/bookmarks"),
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
}

#[tokio::test]
async fn input_bookmark_add_is_idempotent_and_delete_is_idempotent() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let input = f.input(f.conversation, "  Привет,\n мир  ", 17).await;

    let created = f.put(f.conversation, "input", input).await;
    assert_eq!(
        created.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&created.body)
    );
    let first: Value = created.json();
    assert_eq!(first["conversation_id"], f.conversation.to_string());
    assert_eq!(first["target_kind"], "input");
    assert_eq!(first["target_id"], input.to_string());
    assert_eq!(first["input_id"], input.to_string());
    assert!(first["block_id"].is_null());
    assert_eq!(first["role"], "owner");
    assert_eq!(first["kind"], "message");
    assert_eq!(first["seq"], 17);
    assert_eq!(first["jump_seq"], 17);
    assert_eq!(first["excerpt"], "Привет, мир");
    assert!(first["created_at"].as_str().unwrap().contains('T'));
    assert!(first["target_created_at"].as_str().unwrap().contains('T'));

    let repeated = f.put(f.conversation, "input", input).await;
    assert_eq!(repeated.status.as_u16(), 200);
    let second: Value = repeated.json();
    assert_eq!(second["id"], first["id"]);
    assert_eq!(second["created_at"], first["created_at"]);

    assert_eq!(f.list(f.conversation).await.len(), 1);
    assert_eq!(f.delete(f.conversation, "input", input).await.status, 204);
    assert_eq!(f.delete(f.conversation, "input", input).await.status, 204);
    assert!(f.list(f.conversation).await.is_empty());
}
