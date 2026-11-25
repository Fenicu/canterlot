//! Поиск по истории: `POST /api/v1/search` и инструмент `history.search`.

use std::{sync::Arc, time::Duration};

use axum::http::Method;
use canterlot_hub::{
    config::SessionToolsConfig,
    delivery::{self, ContentBlock, SubmitInput},
    http::{self, AppState},
    session_tools::{self, Caller, ToolContext, ToolError, ToolErrorCode, settle::SettleConfig},
    testkit::{self, TestClient},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    owner: Uuid,
    machine: Uuid,
    root: Uuid,
    client: TestClient,
    context: ToolContext,
    _dir: tempfile::TempDir,
}

impl Fx {
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
        let state = AppState::new(deps);
        let client = TestClient::in_process(http::router(state)).as_owner();
        let context = ToolContext {
            memory: Default::default(),
            pool: pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            limits: Default::default(),
            settle: SettleConfig {
                idle_quiet: Duration::from_secs(5),
                nudge_enabled: true,
                idle_background_max: Duration::from_secs(15 * 60),
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: Duration::from_secs(1800),
            routing_queue_horizon: Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: None,
            registry: canterlot_hub::session::Registry::new(Default::default()),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };
        let mut fx = Self {
            pool,
            owner,
            machine,
            root: Uuid::nil(),
            client,
            context,
            _dir: dir,
        };
        fx.root = fx.conversation("Корень", None, 0).await;
        fx
    }

    async fn conversation(&self, title: &str, parent: Option<Uuid>, depth: i32) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.machine).await;
        sqlx::query(
            "update conversations set title=$2,parent_conversation_id=$3,spawn_depth=$4 where id=$1",
        )
        .bind(id)
        .bind(title)
        .bind(parent)
        .bind(depth)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'stopped')",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn block_at(
        &self,
        conversation: Uuid,
        text: &str,
        at: DateTime<Utc>,
        seq: i64,
        parent: Option<Uuid>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,
                                content,agent,model,created_at,parent_block_id)
             select $1,e.id,$2,'text','final',1,$3,$4,'main','small',$5,$6
             from executions e where e.conversation_id=$2",
        )
        .bind(id)
        .bind(conversation)
        .bind(seq)
        .bind(text)
        .bind(at)
        .bind(parent)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn block(&self, conversation: Uuid, text: &str) -> Uuid {
        self.block_at(conversation, text, Utc::now(), 1, None).await
    }

    /// Ввод в ленте с `feed_seq` и происхождением; `fed = false` — ещё не доставлен.
    async fn input_with(
        &self,
        conversation: Uuid,
        text: &str,
        origin: Option<Value>,
        fed: bool,
    ) -> (Uuid, i64) {
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
        let seq: i64 = sqlx::query_scalar(
            "update inputs set origin=$2,
                    feed_seq=case when $3 then (select coalesce(max(feed_seq),1000)+1 from inputs) end
             where id=$1 returning coalesce(feed_seq,0)",
        )
        .bind(submitted.input_id)
        .bind(origin)
        .bind(fed)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (submitted.input_id, seq)
    }

    async fn input(&self, conversation: Uuid, text: &str) -> Uuid {
        self.input_with(conversation, text, None, true).await.0
    }

    async fn http(&self, body: Value) -> Value {
        let response = self
            .client
            .send(Method::POST, "/api/v1/search", None, Some(body))
            .await;
        assert_eq!(response.status.as_u16(), 200, "{:?}", response.body);
        response.json()
    }

    async fn mcp(&self, from: Uuid, args: Value) -> Result<Value, ToolError> {
        let caller = Caller {
            owner_id: self.owner,
            conversation_id: from,
            execution_id: Uuid::now_v7(),
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        };
        session_tools::call(&self.context, &caller, "history.search", args).await
    }
}

fn ids(page: &Value, field: &str) -> Vec<Uuid> {
    page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|hit| hit[field].as_str().map(|id| id.parse().unwrap()))
        .collect()
}

fn sessions(page: &Value) -> Vec<Uuid> {
    let mut all: Vec<Uuid> = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["session_id"].as_str().unwrap().parse().unwrap())
        .collect();
    all.sort();
    all.dedup();
    all
}

#[tokio::test]
async fn russian_and_english_word_forms_match() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let ru = f
        .block(
            f.root,
            "Сборка проекта упала: тестов много, миграции базы не прошли",
        )
        .await;
    let en = f
        .block(f.root, "The tests were running when the builds failed")
        .await;
    let message = f.input(f.root, "Проверь миграцию и почини сборку").await;
    for (query, blocks, inputs) in [
        ("сборки", vec![ru], vec![message]),
        ("тестами", vec![ru], vec![]),
        ("миграций", vec![ru], vec![message]),
        ("test", vec![en], vec![]),
        ("run", vec![en], vec![]),
        ("build", vec![en], vec![]),
        ("\"сборка проекта\"", vec![ru], vec![]),
        ("сборка -проекта", vec![], vec![message]),
        ("tests or миграцию", vec![en, ru], vec![message]),
        ("отсутствующее", vec![], vec![]),
    ] {
        let page = f.http(json!({"q": query})).await;
        let mut found = ids(&page, "block_id");
        found.sort();
        let mut want = blocks.clone();
        want.sort();
        assert_eq!(found, want, "{query}: {page}");
        assert_eq!(ids(&page, "input_id"), inputs, "{query}: {page}");
        assert_eq!(page["total"], json!(blocks.len() + inputs.len()), "{query}");
    }
}

#[tokio::test]
async fn a_delegate_searches_only_its_family() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let parent = f.conversation("Родитель", Some(f.root), 1).await;
    let child = f.conversation("Ребёнок", Some(parent), 2).await;
    let grandchild = f.conversation("Внук", Some(child), 3).await;
    let sibling = f.conversation("Сосед", Some(parent), 2).await;
    let stranger = f.conversation("Посторонний", None, 0).await;
    let hidden = f.conversation("Спрятанный внук", Some(child), 3).await;
    for id in [f.root, parent, child, grandchild, sibling, stranger, hidden] {
        f.block(id, "семейный секрет").await;
    }
    sqlx::query("update conversations set trashed_at=now() where id=$1")
        .bind(hidden)
        .execute(&f.pool)
        .await
        .unwrap();
    let query = json!({"query": "семейный"});
    let page = f.mcp(child, query.clone()).await.unwrap();
    let mut want = vec![f.root, parent, child, grandchild];
    want.sort();
    assert_eq!(sessions(&page), want);
    assert_eq!(page["total"], 4);

    let page = f.mcp(f.root, query.clone()).await.unwrap();
    assert_eq!(sessions(&page).len(), 6);

    for outside in [sibling, stranger, hidden, Uuid::now_v7()] {
        let error = f
            .mcp(child, json!({"query": "семейный", "session_id": outside}))
            .await
            .unwrap_err();
        assert_eq!(error.code, ToolErrorCode::SessionNotFound, "{outside}");
    }
    let page = f
        .mcp(
            child,
            json!({"query": "семейный", "session_id": grandchild}),
        )
        .await
        .unwrap();
    assert_eq!(sessions(&page), vec![grandchild]);
    let text = page["text"].as_str().unwrap();
    assert!(text.starts_with("1 of 1 matches"), "{text}");
    assert!(text.contains(&format!("session_id {grandchild}")), "{text}");
    assert!(text.contains("block_id "), "{text}");
    assert!(
        text.contains("**секрет**") || text.contains("**семейный**"),
        "{text}"
    );
}
