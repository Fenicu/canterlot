#![allow(dead_code)]
use axum::http::Method;
use canterlot_hub::{
    delivery::{self, ContentBlock, SubmitInput, SubmittedInput},
    events::{self, NewEvent, Scope, StoredEvent},
    http::{self, AppState},
    testkit::{self, Seed, TestClient},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub struct Fixture {
    pub pool: PgPool,
    pub seed: Seed,
    pub client: TestClient,
    pub state: AppState,
    _dir: tempfile::TempDir,
}
impl Fixture {
    pub async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner_id = deps.installation.owner_id;
        let machine_id = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
            .bind(machine_id)
            .bind(owner_id)
            .execute(&pool)
            .await
            .unwrap();
        testkit::seed_catalog(&pool, machine_id).await;
        let conversation_id = testkit::seed_conversation(&pool, owner_id, machine_id).await;
        sqlx::query("update conversations set model='synthetic-small',effort='high',permission_mode='bypass' where id=$1")
            .bind(conversation_id).execute(&pool).await.unwrap();
        let state = AppState::new(deps);
        let client = TestClient::in_process(http::router(state.clone())).as_owner();
        Self {
            pool,
            seed: Seed {
                owner_id,
                machine_id,
                conversation_id,
            },
            client,
            state,
            _dir: dir,
        }
    }
    pub fn path(&self, suffix: &str) -> String {
        format!(
            "/api/v1/conversations/{}{suffix}",
            self.seed.conversation_id
        )
    }
    pub async fn get(&self, path: &str) -> Value {
        let r = self.client.send(Method::GET, path, None, None).await;
        assert_eq!(r.status.as_u16(), 200);
        assert_eq!(r.headers["cache-control"], "no-store");
        assert!(r.headers.contains_key("x-server-time-ms"));
        r.json()
    }
    pub async fn input(&self, key: &str) -> SubmittedInput {
        input(
            &self.pool,
            self.seed.owner_id,
            self.seed.conversation_id,
            key,
        )
        .await
    }
    pub async fn emit(&self, kind: &'static str, payload: Value) -> StoredEvent {
        emit(
            &self.pool,
            Scope::Conversation {
                id: self.seed.conversation_id,
            },
            kind,
            payload,
        )
        .await
    }
    pub async fn execution(&self) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
        )
        .bind(id)
        .bind(self.seed.conversation_id)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }
    pub async fn block(&self, execution: Uuid, seq: i64, kind: &str, text: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,content,agent,model,tool_name,summary,finished_at) values($1,$2,$3,$4,'final',1,$5,$6,'Explore','small','Agent','task',clock_timestamp())")
            .bind(id).bind(execution).bind(self.seed.conversation_id).bind(kind).bind(seq).bind(text).execute(&self.pool).await.unwrap();
        id
    }
}
pub async fn input(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    key: &str,
) -> SubmittedInput {
    delivery::submit_input(
        pool,
        SubmitInput {
            owner_id,
            conversation_id,
            idempotency_key: key.into(),
            content: vec![ContentBlock::Text {
                text: "test".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
}
pub async fn emit(pool: &PgPool, scope: Scope, kind: &'static str, payload: Value) -> StoredEvent {
    let mut tx = pool.begin().await.unwrap();
    let conversation_id = match scope {
        Scope::Conversation { id } => Some(id),
        _ => None,
    };
    let e = events::append(
        &mut tx,
        NewEvent {
            scope,
            kind,
            conversation_id,
            execution_id: None,
            payload,
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    e
}
pub fn seq(v: &Value) -> i64 {
    v.as_str().unwrap().parse().unwrap()
}
pub fn keys(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            seq(if v["kind"] == "input" {
                &v["feed_seq"]
            } else {
                &v["created_seq"]
            })
        })
        .collect()
}
pub fn thresholds() -> canterlot_hub::activity::Thresholds {
    canterlot_hub::activity::Thresholds {
        stalled_generating: std::time::Duration::from_secs(300),
        stalled_tool: std::time::Duration::from_secs(1800),
        idle_quiet: std::time::Duration::from_secs(5),
    }
}
pub fn empty() -> Value {
    json!({})
}
