//! Избранные сообщения разговора: хранение, HTTP API и событие `bookmark_changed`.

use axum::http::Method;
use canterlot_hub::{
    delivery::{self, ContentBlock, SubmitInput},
    events,
    http::{self, AppState},
    testkit::{self, TestClient, TestResponse},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
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
            machine,
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

    async fn conversation(&self, title: &str) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.machine).await;
        sqlx::query("update conversations set title=$2 where id=$1")
            .bind(id)
            .bind(title)
            .execute(&self.pool)
            .await
            .unwrap();
        Self::execution(&self.pool, id).await;
        id
    }

    async fn block(&self, conversation: Uuid, text: &str, seq: i64, parent: Option<Uuid>) -> Uuid {
        let execution: Uuid = sqlx::query_scalar(
            "select id from executions where conversation_id=$1 order by generation desc limit 1",
        )
        .bind(conversation)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,
                                content,agent,model,created_at,parent_block_id)
             values($1,$2,$3,'text','final',1,$4,$5,'main','synthetic-small',clock_timestamp(),$6)",
        )
        .bind(id)
        .bind(execution)
        .bind(conversation)
        .bind(seq)
        .bind(text)
        .bind(parent)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn tool_block(&self, conversation: Uuid, seq: i64) -> Uuid {
        let execution: Uuid = sqlx::query_scalar(
            "select id from executions where conversation_id=$1 order by generation desc limit 1",
        )
        .bind(conversation)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,
                                content,agent,model,created_at)
             values($1,$2,$3,'tool_call','final',1,$4,'', 'main','synthetic-small',clock_timestamp())",
        )
        .bind(id)
        .bind(execution)
        .bind(conversation)
        .bind(seq)
        .execute(&self.pool)
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

    async fn global(&self, query: &str) -> Value {
        let response = self
            .client
            .send(
                Method::GET,
                &format!("/api/v1/bookmarks{query}"),
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

    async fn events(&self) -> Vec<Value> {
        sqlx::query_scalar(
            "select payload from events where scope=$1 and kind='bookmark_changed' order by scope_seq",
        )
        .bind(format!("owner:{}:conversations", self.owner))
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_bookmark_add_is_idempotent_and_delete_is_idempotent(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_bookmark_excerpt_collapses_whitespace_and_cuts_on_a_character_boundary(
    pool: PgPool,
) {
    let f = Fixture::new(pool).await;
    let long = format!("  Первая\n\nстрока\t{}", "Ж".repeat(400));
    let block = f.block(f.conversation, &long, 5, None).await;

    let created = f.put(f.conversation, "block", block).await;
    assert_eq!(
        created.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&created.body)
    );
    let bookmark: Value = created.json();
    assert_eq!(bookmark["target_kind"], "block");
    assert_eq!(bookmark["target_id"], block.to_string());
    assert_eq!(bookmark["block_id"], block.to_string());
    assert!(bookmark["input_id"].is_null());
    assert_eq!(bookmark["kind"], "answer");
    assert_eq!(bookmark["role"], "agent");
    assert_eq!(bookmark["agent"], "main");
    assert_eq!(bookmark["harness"], "synthetic");
    assert_eq!(bookmark["model"], "synthetic-small");
    let excerpt = bookmark["excerpt"].as_str().unwrap();
    assert_eq!(excerpt.chars().count(), 300);
    assert!(excerpt.starts_with("Первая строка "), "{excerpt}");
    assert!(!excerpt.contains('\n') && !excerpt.contains('\t'));

    let tool = f.tool_block(f.conversation, 6).await;
    let bookmark: Value = f.put(f.conversation, "block", tool).await.json();
    assert_eq!(bookmark["excerpt"], "");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nested_foreign_and_unknown_targets_are_refused(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = f.conversation("Другая").await;
    let top = f.block(f.conversation, "верхний", 1, None).await;
    let nested = f.block(f.conversation, "вложенный", 2, Some(top)).await;
    let foreign_block = f.block(other, "чужой", 1, None).await;
    let foreign_input = f.input(other, "чужой ввод", 1).await;

    for target in [nested, foreign_block] {
        let response = f.put(f.conversation, "block", target).await;
        assert_eq!(response.status.as_u16(), 404, "{target}");
        assert_eq!(code(&response), "not_found");
    }
    let response = f.put(f.conversation, "input", foreign_input).await;
    assert_eq!(response.status.as_u16(), 404);
    assert_eq!(code(&response), "not_found");
    let response = f.put(f.conversation, "block", Uuid::now_v7()).await;
    assert_eq!(response.status.as_u16(), 404);

    let response = f.put(f.conversation, "bogus", top).await;
    assert_eq!(response.status.as_u16(), 422);
    assert_eq!(code(&response), "invalid_request");

    sqlx::query("update conversations set trashed_at=clock_timestamp() where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    let response = f.put(f.conversation, "block", top).await;
    assert_eq!(response.status.as_u16(), 404);
    sqlx::query("update conversations set trashed_at=null where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn conversation_list_follows_the_timeline_not_the_bookmarking_order(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let first = f.input(f.conversation, "первый", 10).await;
    let second = f.block(f.conversation, "второй", 20, None).await;
    let third = f.input(f.conversation, "третий", 30).await;

    for (kind, target) in [("input", third), ("block", second), ("input", first)] {
        assert_eq!(f.put(f.conversation, kind, target).await.status, 201);
    }
    let bookmarks = f.list(f.conversation).await;
    let targets: Vec<_> = bookmarks.iter().map(|b| b["target_id"].clone()).collect();
    assert_eq!(
        targets,
        vec![
            json!(first.to_string()),
            json!(second.to_string()),
            json!(third.to_string())
        ]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn global_list_is_newest_first_paginates_and_hides_trash(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let first_chat = f.conversation("Первый разговор").await;
    let second_chat = f.conversation("Второй разговор").await;
    let third_chat = f.conversation("Третий разговор").await;

    let one = f.block(first_chat, "один", 1, None).await;
    let two = f.block(second_chat, "два", 1, None).await;
    let three = f.input(third_chat, "три", 1).await;
    let mut ids = Vec::new();
    for (chat, kind, target) in [
        (first_chat, "block", one),
        (second_chat, "block", two),
        (third_chat, "input", three),
    ] {
        let response = f.put(chat, kind, target).await;
        assert_eq!(response.status, 201);
        ids.push(
            response.json::<Value>()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("update bookmarks set created_at=$2 where id=$1")
            .bind(id)
            .bind(Utc::now() + TimeDelta::seconds(index as i64))
            .execute(&f.pool)
            .await
            .unwrap();
    }

    let page = f.global("?limit=2").await;
    let items = page["bookmarks"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["target_id"], three.to_string());
    assert_eq!(items[0]["conversation_title"], "Третий разговор");
    assert_eq!(items[1]["target_id"], two.to_string());
    let cursor = page["next_cursor"].as_str().unwrap();
    let page = f.global(&format!("?limit=2&cursor={cursor}")).await;
    let items = page["bookmarks"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["target_id"], one.to_string());
    assert!(page["next_cursor"].is_null());

    sqlx::query("update conversations set trashed_at=clock_timestamp() where id=$1")
        .bind(second_chat)
        .execute(&f.pool)
        .await
        .unwrap();
    let listed = f.global("?limit=100").await;
    let targets: Vec<_> = listed["bookmarks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["target_id"].clone())
        .collect();
    assert_eq!(
        targets,
        vec![json!(three.to_string()), json!(one.to_string())]
    );

    sqlx::query("update conversations set trashed_at=null where id=$1")
        .bind(second_chat)
        .execute(&f.pool)
        .await
        .unwrap();
    let restored = f.global("?limit=100").await;
    assert_eq!(restored["bookmarks"].as_array().unwrap().len(), 3);

    let refused = f
        .client
        .send(Method::GET, "/api/v1/bookmarks?limit=0", None, None)
        .await;
    assert_eq!(refused.status.as_u16(), 400);
    assert_eq!(code(&refused), "invalid_request");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn missing_targets_are_omitted_and_removed_lazily(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let block = f.block(f.conversation, "исчезнет", 1, None).await;
    assert_eq!(f.put(f.conversation, "block", block).await.status, 201);
    sqlx::query("delete from blocks where block_id=$1")
        .bind(block)
        .execute(&f.pool)
        .await
        .unwrap();

    assert!(f.list(f.conversation).await.is_empty());
    let rows: i64 = sqlx::query_scalar("select count(*) from bookmarks")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purging_a_conversation_removes_its_bookmarks(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let input = f
        .input(f.conversation, "уйдёт вместе с разговором", 1)
        .await;
    assert_eq!(f.put(f.conversation, "input", input).await.status, 201);

    let trashed = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{}", f.conversation),
            Some("bookmark-trash"),
            None,
        )
        .await;
    assert_eq!(trashed.status.as_u16(), 204);
    let purged = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{}/trash", f.conversation),
            Some("bookmark-purge"),
            None,
        )
        .await;
    assert_eq!(
        purged.status.as_u16(),
        204,
        "{}",
        String::from_utf8_lossy(&purged.body)
    );
    let rows: i64 = sqlx::query_scalar("select count(*) from bookmarks")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn bookmark_changed_is_published_on_add_and_remove(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let input = f.input(f.conversation, "отмечу", 4).await;
    let created: Value = f.put(f.conversation, "input", input).await.json();
    let events = f.events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["conversation_id"], f.conversation.to_string());
    assert_eq!(events[0]["target_kind"], "input");
    assert_eq!(events[0]["target_id"], input.to_string());
    assert_eq!(events[0]["bookmarked"], true);
    assert_eq!(events[0]["bookmark"], created);

    assert_eq!(f.delete(f.conversation, "input", input).await.status, 204);
    let events = f.events().await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["bookmarked"], false);
    assert!(events[1].get("bookmark").is_none());

    assert_eq!(f.delete(f.conversation, "input", input).await.status, 204);
    assert_eq!(f.events().await.len(), 2, "no event when nothing changed");
}

#[sqlx::test(migrations = false)]
async fn migration_applies_on_top_of_0080(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let old = sqlx::migrate::Migrator {
        migrations: canterlot_hub::db::MIGRATOR
            .iter()
            .filter(|migration| migration.version <= 80)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await?;
    let absent: Option<String> = sqlx::query_scalar("select to_regclass('public.bookmarks')::text")
        .fetch_one(&pool)
        .await?;
    assert!(absent.is_none());

    canterlot_hub::db::MIGRATOR.run(&pool).await?;
    let table: Option<String> = sqlx::query_scalar("select to_regclass('public.bookmarks')::text")
        .fetch_one(&pool)
        .await?;
    assert_eq!(table.as_deref(), Some("bookmarks"));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn bookmark_changed_matches_the_published_schema(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let block = f.block(f.conversation, "текст ответа", 1, None).await;
    assert_eq!(f.put(f.conversation, "block", block).await.status, 201);
    assert_eq!(f.delete(f.conversation, "block", block).await.status, 204);

    let rows: Vec<events::StoredEvent> = sqlx::query_as(
        "select * from events where scope=$1 and kind='bookmark_changed' order by scope_seq",
    )
    .bind(format!("owner:{}:conversations", f.owner))
    .fetch_all(&f.pool)
    .await
    .unwrap();
    let projected = events::project_for_client(rows);
    assert_eq!(projected.len(), 2);
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/bookmark_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    let validator = jsonschema::validator_for(&schema).unwrap();
    for event in projected {
        let value = serde_json::to_value(&event).unwrap();
        assert!(
            validator.is_valid(&value),
            "invalid bookmark_changed: {:?}",
            validator
                .iter_errors(&value)
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        );
    }
}
