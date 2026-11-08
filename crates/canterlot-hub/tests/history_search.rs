//! Поиск по истории: `POST /api/v1/search` и инструмент `history.search`.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::http::Method;
use canterlot_hub::{
    config::SessionToolsConfig,
    deletion,
    delivery::{self, ContentBlock, SubmitInput},
    http::{self, AppState},
    search,
    session_tools::{self, Caller, ToolContext, ToolError, ToolErrorCode, settle::SettleConfig},
    testkit::{self, TestClient},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
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
            pool: pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            limits: Default::default(),
            settle: SettleConfig {
                idle_quiet: Duration::from_secs(5),
                nudge_enabled: true,
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: Duration::from_secs(1800),
            routing_queue_horizon: Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
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

    async fn http_status(&self, body: Value) -> (u16, Value) {
        let response = self
            .client
            .send(Method::POST, "/api/v1/search", None, Some(body))
            .await;
        (response.status.as_u16(), response.json())
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

    async fn delete(&self, id: Uuid) {
        let directory = tempfile::tempdir().unwrap();
        let (_, blobs) = testkit::test_blob_store(directory.path());
        deletion::delete(
            &self.pool,
            blobs.as_ref(),
            self.owner,
            id,
            &format!("delete-{id}"),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
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

fn plain(hit: &Value) -> String {
    hit["snippet"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| segment["text"].as_str().unwrap())
        .collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn russian_and_english_word_forms_match(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn filters_narrow_the_hits(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = f.conversation("Другая", None, 0).await;
    sqlx::query("update conversations set harness='codex' where id=$1")
        .bind(other)
        .execute(&f.pool)
        .await
        .unwrap();
    let now = Utc::now();
    let old = f
        .block_at(f.root, "старый отчёт", now - TimeDelta::days(10), 1, None)
        .await;
    let fresh = f.block_at(f.root, "свежий отчёт", now, 2, None).await;
    let elsewhere = f.block_at(other, "чужой отчёт", now, 1, None).await;
    let message = f.input(f.root, "пришли отчёт").await;

    let page = f
        .http(json!({"q": "отчёт", "conversation_id": other}))
        .await;
    assert_eq!(ids(&page, "block_id"), vec![elsewhere]);
    assert!(ids(&page, "input_id").is_empty());

    let since = (now - TimeDelta::days(1)).to_rfc3339();
    let page = f
        .http(json!({"q": "отчёт", "since": since, "kind": "answers"}))
        .await;
    let mut found = ids(&page, "block_id");
    found.sort();
    let mut want = vec![fresh, elsewhere];
    want.sort();
    assert_eq!(found, want);

    let until = (now - TimeDelta::days(1)).to_rfc3339();
    let page = f.http(json!({"q": "отчёт", "until": until})).await;
    assert_eq!(ids(&page, "block_id"), vec![old]);

    let page = f.http(json!({"q": "отчёт", "harness": "codex"})).await;
    assert_eq!(ids(&page, "block_id"), vec![elsewhere]);

    let page = f.http(json!({"q": "отчёт", "kind": "messages"})).await;
    assert!(ids(&page, "block_id").is_empty());
    assert_eq!(ids(&page, "input_id"), vec![message]);
    assert_eq!(page["hits"][0]["kind"], "message");
    assert_eq!(page["hits"][0]["origin_kind"], "owner");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pages_follow_the_cursor_newest_first(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let mut expected = Vec::new();
    for n in 0..5 {
        let at = now - TimeDelta::minutes(n);
        expected.push(
            f.block_at(f.root, &format!("релиз {n}"), at, 10 - n, None)
                .await,
        );
    }
    // Равное время: ответ раньше сообщения, внутри вида по ID.
    let tie = now - TimeDelta::minutes(10);
    let tied_block = f.block_at(f.root, "релиз ничья", tie, 1, None).await;
    let tied_input = f.input(f.root, "релиз ничья").await;
    sqlx::query("update inputs set created_at=$2 where id=$1")
        .bind(tied_input)
        .bind(tie)
        .execute(&f.pool)
        .await
        .unwrap();
    expected.push(tied_block);
    expected.push(tied_input);

    let mut seen = Vec::new();
    let mut cursor = Value::Null;
    let mut pages = 0;
    loop {
        let page = f
            .http(json!({"q": "релиз", "limit": 3, "cursor": cursor}))
            .await;
        assert_eq!(page["total"], 7);
        assert_eq!(page["total_capped"], false);
        for hit in page["hits"].as_array().unwrap() {
            let id = hit["block_id"]
                .as_str()
                .or(hit["input_id"].as_str())
                .unwrap();
            seen.push(id.parse::<Uuid>().unwrap());
        }
        pages += 1;
        cursor = page["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, expected);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn total_is_capped_at_two_hundred(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,content,agent,model)
         select gen_random_uuid(),e.id,$1,'text','final',1,n,'частый токен '||n,'main','small'
         from executions e, generate_series(1,205) n where e.conversation_id=$1",
    )
    .bind(f.root)
    .execute(&f.pool)
    .await
    .unwrap();
    let page = f.http(json!({"q": "токен"})).await;
    assert_eq!(page["total"], 200);
    assert_eq!(page["total_capped"], true);
    assert_eq!(page["hits"].as_array().unwrap().len(), 20);
    assert!(page["next_cursor"].is_string());
    let page = f.http(json!({"q": "токен", "limit": 50})).await;
    assert_eq!(page["hits"].as_array().unwrap().len(), 50);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snippet_is_segments_without_markers_or_raw_whitespace(pool: PgPool) {
    let f = Fx::new(pool).await;
    let filler = "обычные слова без совпадений ".repeat(40);
    f.block(
        f.root,
        &format!("{filler}\n\nЗдесь\u{E000}ловушка\u{E001} и   сборка\tупала\n{filler}"),
    )
    .await;
    let page = f.http(json!({"q": "сборка"})).await;
    let hit = &page["hits"][0];
    let segments = hit["snippet"].as_array().unwrap();
    let hits: Vec<&str> = segments
        .iter()
        .filter(|s| s["hit"] == true)
        .map(|s| s["text"].as_str().unwrap())
        .collect();
    assert_eq!(hits, vec!["сборка"], "{segments:?}");
    let text = plain(hit);
    assert!(text.contains("Здесьловушка и сборка упала"), "{text}");
    assert!(
        !text.contains(['\u{E000}', '\u{E001}', '\n', '\t']) && !text.contains("  "),
        "{text:?}"
    );
    assert!(text.chars().count() <= search::SNIPPET_MAX_CHARS);
    assert!(!text.contains('<'), "без HTML: {text}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hits_carry_origin_seq_jump_target_and_archive_flag(pool: PgPool) {
    let f = Fx::new(pool).await;
    let sender = f.conversation("Отправитель", None, 0).await;
    let from = json!(sender.to_string());
    let (owner, owner_seq) = f.input_with(f.root, "маркер владельца", None, true).await;
    let (message, _) = f
        .input_with(
            f.root,
            "маркер сообщения",
            Some(json!({"kind": "message", "from": from, "chain_depth": 1})),
            true,
        )
        .await;
    let (spawn, _) = f
        .input_with(
            f.root,
            "маркер брифа",
            Some(json!({"kind": "spawn", "from": from, "chain_depth": 1})),
            true,
        )
        .await;
    let (result, _) = f
        .input_with(
            f.root,
            "маркер итога",
            Some(json!({"kind": "event", "from": from, "chain_depth": 0})),
            true,
        )
        .await;
    for (text, origin, fed) in [
        (
            "маркер будильника",
            Some(json!({"kind": "alarm", "chain_depth": 0})),
            true,
        ),
        (
            "маркер пинка",
            Some(json!({"kind": "nudge", "chain_depth": 0})),
            true,
        ),
        (
            "маркер хаба",
            Some(json!({"kind": "event", "chain_depth": 0})),
            true,
        ),
        (
            "маркер вычищенного",
            Some(json!({"kind": "event", "chain_depth": 0, "sender_deleted": true})),
            true,
        ),
        ("маркер очереди", None, false),
    ] {
        f.input_with(f.root, text, origin, fed).await;
    }
    let page = f.http(json!({"q": "маркер", "kind": "messages"})).await;
    let mut got: Vec<(Uuid, String)> = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| {
            (
                hit["input_id"].as_str().unwrap().parse().unwrap(),
                hit["origin_kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    got.sort();
    let mut want = vec![
        (owner, "owner".to_owned()),
        (message, "message".to_owned()),
        (spawn, "spawn".to_owned()),
        (result, "result".to_owned()),
    ];
    want.sort();
    assert_eq!(got, want, "{page}");
    let owner_hit = page["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["input_id"] == json!(owner))
        .unwrap();
    assert_eq!(owner_hit["seq"], owner_seq);
    assert_eq!(owner_hit["jump_seq"], owner_seq);
    assert_eq!(owner_hit["block_id"], Value::Null);
    assert_eq!(owner_hit["conversation_title"], "Корень");
    assert_eq!(owner_hit["harness"], "synthetic");
    assert_eq!(owner_hit["archived"], false);

    // Вложенный блок субагента: настоящий ID и свой seq, переход к верхнему предку.
    let now = Utc::now();
    let top = f.block_at(f.root, "общий план", now, 40, None).await;
    let middle = f.block_at(f.root, "шаг плана", now, 41, Some(top)).await;
    let leaf = f
        .block_at(f.root, "глубокий ответ субагента", now, 42, Some(middle))
        .await;
    sqlx::query("update conversations set archived_at=now() where id=$1")
        .bind(f.root)
        .execute(&f.pool)
        .await
        .unwrap();
    let page = f.http(json!({"q": "субагента"})).await;
    let hit = &page["hits"][0];
    assert_eq!(hit["block_id"], json!(leaf));
    assert_eq!(hit["kind"], "answer");
    assert_eq!(hit["origin_kind"], Value::Null);
    assert_eq!(hit["input_id"], Value::Null);
    assert_eq!(
        (hit["seq"].clone(), hit["jump_seq"].clone()),
        (json!(42), json!(40))
    );
    assert_eq!(hit["archived"], true);
    let mcp = f.mcp(f.root, json!({"query": "субагента"})).await.unwrap();
    assert_eq!(mcp["hits"][0]["block_id"], json!(leaf));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trashed_and_deleted_sessions_are_not_found(pool: PgPool) {
    let f = Fx::new(pool).await;
    let trashed = f.conversation("В корзину", None, 0).await;
    let doomed = f.conversation("На удаление", None, 0).await;
    let parent = f.conversation("Родитель", None, 0).await;
    let delegate = f.conversation("Делегат", Some(parent), 1).await;
    for (id, token) in [(trashed, "корзинатокен"), (doomed, "удалениетокен")]
    {
        f.block(id, &format!("ответ {token}")).await;
        f.input(id, &format!("вопрос {token}")).await;
    }
    f.block(delegate, "делегаттокен в ленте делегата").await;
    f.input_with(
        parent,
        "[canterlot] Session result: итогтокен",
        Some(json!({"kind": "event", "from": delegate.to_string(), "chain_depth": 0})),
        true,
    )
    .await;
    // Сообщение от делегата сохраняет тело и после удаления отправителя: это ввод родителя.
    f.input_with(
        parent,
        "[canterlot] Message from session \"Делегат\".\n\nсообщениетокен",
        Some(json!({"kind": "message", "from": delegate.to_string(), "chain_depth": 1})),
        true,
    )
    .await;
    let count = async |token: &str| -> (i64, usize) {
        let page = f.http(json!({"q": token})).await;
        let mcp = f.mcp(f.root, json!({"query": token})).await.unwrap();
        (
            page["total"].as_i64().unwrap(),
            mcp["hits"].as_array().unwrap().len(),
        )
    };
    for token in ["корзинатокен", "удалениетокен"] {
        assert_eq!(count(token).await, (2, 2), "{token}");
    }
    assert_eq!(count("делегаттокен").await, (1, 1));
    assert_eq!(count("итогтокен").await, (1, 1));

    deletion::trash(&f.pool, f.owner, trashed, "trash-it", Uuid::now_v7())
        .await
        .unwrap();
    f.delete(doomed).await;
    f.delete(delegate).await;
    for token in ["корзинатокен", "удалениетокен", "делегаттокен", "итогтокен"]
    {
        assert_eq!(count(token).await, (0, 0), "{token}");
    }
    assert_eq!(count("сообщениетокен").await, (1, 1));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delegate_searches_only_its_family(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn invalid_requests_are_rejected(pool: PgPool) {
    let f = Fx::new(pool).await;
    let long = "я".repeat(201);
    for body in [
        json!({"q": ""}),
        json!({"q": "   "}),
        json!({"q": long}),
        json!({"q": "x", "limit": 0}),
        json!({"q": "x", "limit": 51}),
        json!({"q": "x", "cursor": "не курсор"}),
        json!({"q": "x", "kind": "tools"}),
        json!({"q": "x", "unknown": 1}),
        json!({"q": "x", "since": "2026-11-08T10:00:00Z", "until": "2026-11-08T09:00:00Z"}),
        json!({"q": "x", "since": "вчера"}),
        json!({}),
    ] {
        let (status, error) = f.http_status(body.clone()).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(error["error"]["code"], "invalid_request", "{body}");
    }
    assert_eq!(f.http(json!({"q": &"я".repeat(200)})).await["total"], 0);
    for args in [
        json!({"query": ""}),
        json!({"query": "x", "limit": 51}),
        json!({"query": "x", "extra": true}),
        json!({}),
    ] {
        let error = f.mcp(f.root, args.clone()).await.unwrap_err();
        assert_eq!(error.code, ToolErrorCode::InvalidArguments, "{args}");
    }
}

static BUFFER: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
struct Capture;
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        BUFFER
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        Capture
    }
}

/// Запрос без лексем — «ничего не найдено», не ошибка. Ни запрос, ни фрагменты, ни NOTICE
/// базы с текстом запроса в журнал не попадают даже при перехвате всех целей на TRACE.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stop_word_queries_find_nothing_and_logs_hold_no_text(pool: PgPool) {
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(Capture)
        .try_init();
    let f = Fx::new(pool).await;
    let secret = format!("журналсекрет{}", Uuid::now_v7().simple());
    f.block(f.root, &format!("ответ с {secret} внутри")).await;
    for query in ["и или", "-слово", "\"\""] {
        let page = f.http(json!({"q": query})).await;
        assert_eq!(page["total"], 0, "{query}");
        assert_eq!(page["hits"], json!([]), "{query}");
        assert_eq!(page["next_cursor"], Value::Null, "{query}");
        let mcp = f.mcp(f.root, json!({"query": query})).await.unwrap();
        assert!(
            mcp["text"]
                .as_str()
                .unwrap()
                .starts_with("No words to search for"),
            "{mcp}"
        );
    }
    let page = f.http(json!({"q": secret})).await;
    assert_eq!(page["total"], 1);
    let mcp = f.mcp(f.root, json!({"query": secret})).await.unwrap();
    assert_eq!(mcp["total"], 1);
    let logs = String::from_utf8(
        BUFFER
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert!(
        logs.contains("history search"),
        "поиск пишет только счётчики: {logs}"
    );
    for leak in [
        secret.as_str(),
        "журналсекрет",
        "или",
        "text-search query",
        "ответ с",
    ] {
        assert!(!logs.contains(leak), "{leak} в журнале: {logs}");
    }
}

/// Каждая форма запроса идёт по индексам GIN, и с общим планом тоже: документ и частичные
/// предикаты в запросе совпадают с индексами буква в букву.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn search_queries_use_the_gin_indexes(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = f.conversation("Другая", None, 0).await;
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,content,agent,model)
         select gen_random_uuid(),e.id,e.conversation_id,'text','final',1,n,'текст номер '||n,'main','small'
         from executions e, generate_series(1,500) n where e.conversation_id in ($1,$2)",
    )
    .bind(f.root)
    .bind(other)
    .execute(&f.pool)
    .await
    .unwrap();
    for n in 0..20 {
        f.input(f.root, &format!("сообщение номер {n}")).await;
    }
    sqlx::query("analyze").execute(&f.pool).await.unwrap();
    let now = Utc::now();
    let family = vec![f.root, other];
    struct Shape {
        name: &'static str,
        family: Option<Vec<Uuid>>,
        conversation: Option<Uuid>,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        blocks: bool,
        inputs: bool,
    }
    let shapes = [
        Shape {
            name: "owner",
            family: None,
            conversation: None,
            since: None,
            until: None,
            blocks: true,
            inputs: true,
        },
        Shape {
            name: "family",
            family: Some(family.clone()),
            conversation: None,
            since: None,
            until: None,
            blocks: true,
            inputs: true,
        },
        Shape {
            name: "session",
            family: None,
            conversation: Some(f.root),
            since: None,
            until: None,
            blocks: true,
            inputs: true,
        },
        Shape {
            name: "period",
            family: None,
            conversation: None,
            since: Some(now - TimeDelta::days(7)),
            until: Some(now),
            blocks: true,
            inputs: true,
        },
        Shape {
            name: "answers",
            family: None,
            conversation: None,
            since: None,
            until: None,
            blocks: true,
            inputs: false,
        },
        Shape {
            name: "messages",
            family: None,
            conversation: None,
            since: None,
            until: None,
            blocks: false,
            inputs: true,
        },
    ];
    let mut conn = f.pool.acquire().await.unwrap();
    sqlx::query("set enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    let check = |context: String, plan: &str, blocks: bool, inputs: bool| {
        if blocks {
            assert!(plan.contains("blocks_search"), "{context}\n{plan}");
        }
        if inputs {
            assert!(plan.contains("inputs_search"), "{context}\n{plan}");
        }
        assert!(!plan.contains("Seq Scan on blocks"), "{context}\n{plan}");
        assert!(!plan.contains("Seq Scan on inputs"), "{context}\n{plan}");
    };
    for (label, sql, page) in [
        ("page", search::PAGE_SQL.as_str(), true),
        ("total", search::TOTAL_SQL.as_str(), false),
    ] {
        // Общий план, как у подготовленного запроса после пятого вызова: без значений
        // параметров обе ветви и оба индекса на месте.
        for mode in ["auto", "force_generic_plan"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("set plan_cache_mode = {mode}")))
                .execute(&mut *conn)
                .await
                .unwrap();
            // Простой протокол: параметры остаются `$n` без значений.
            let plan = sqlx::raw_sql(sqlx::AssertSqlSafe(format!("explain (generic_plan) {sql}")))
                .fetch_all(&mut *conn)
                .await
                .unwrap()
                .iter()
                .map(|row| row.get::<String, _>(0))
                .collect::<Vec<_>>()
                .join("\n");
            check(format!("generic {mode} {label}"), &plan, true, true);
        }
        // План со значениями для каждой формы запроса.
        for shape in &shapes {
            let mut query =
                sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!("explain {sql}")))
                    .bind(f.owner)
                    .bind("номер")
                    .bind(shape.family.as_deref())
                    .bind(shape.conversation)
                    .bind(shape.since)
                    .bind(shape.until)
                    .bind(None::<&str>)
                    .bind(shape.blocks)
                    .bind(shape.inputs);
            query = if page {
                query
                    .bind(None::<DateTime<Utc>>)
                    .bind(None::<&str>)
                    .bind(None::<Uuid>)
                    .bind(21_i64)
            } else {
                query.bind(201_i64)
            };
            let plan = query.fetch_all(&mut *conn).await.unwrap().join("\n");
            check(
                format!("{} {label}", shape.name),
                &plan,
                shape.blocks,
                shape.inputs,
            );
        }
    }
}

/// `ts_headline` разбирает весь кусок документа заново: страница из 20 документов по 32 тысячи
/// символов, сплошь из совпадений, укладывается далеко в срок поиска.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn headlines_of_a_full_page_of_long_documents_stay_fast(pool: PgPool) {
    let f = Fx::new(pool).await;
    let document: String = "сборка тестов упала "
        .repeat(1700)
        .chars()
        .take(32768)
        .collect();
    for _ in 0..20 {
        f.block(f.root, &document).await;
    }
    let started = Instant::now();
    let page = f.http(json!({"q": "сборка тесты", "limit": 20})).await;
    let elapsed = started.elapsed();
    assert_eq!(page["hits"].as_array().unwrap().len(), 20);
    assert!(
        page["hits"][0]["snippet"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["hit"] == true)
    );
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}
