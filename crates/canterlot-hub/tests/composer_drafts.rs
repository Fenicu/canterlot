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

    async fn attachment(&self, conversation: Uuid, state: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into attachments(id,owner_id,conversation_id,name,size_bytes,sha256,state)
             values($1,$2,$3,'файл.txt',3,$4,$5)",
        )
        .bind(id)
        .bind(self.owner)
        .bind(conversation)
        .bind("a".repeat(64))
        .bind(state)
        .execute(&self.pool)
        .await
        .unwrap();
        id
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn put_get_round_trip_keeps_revision_and_updated_at(pool: PgPool) {
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

    // Устаревшая base_revision не мешает: полная замена текста, побеждает последняя запись.
    let second = f
        .put(
            f.conversation,
            json!({"text":"продолжил на компьютере","base_revision":first_revision - 1,"device_id":"desktop"}),
            200,
        )
        .await;
    assert_eq!(second["text"], "продолжил на компьютере");
    assert!(second["revision"].as_i64().unwrap() > first_revision);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn empty_text_deletes_the_row_and_emits_a_clear(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let saved = f
        .put(
            f.conversation,
            json!({"text":"удалю всё","base_revision":0,"device_id":"phone"}),
            200,
        )
        .await;
    let revision = saved["revision"].as_i64().unwrap();

    let cleared = f
        .put(
            f.conversation,
            json!({"text":"","base_revision":revision,"device_id":"desktop"}),
            200,
        )
        .await;
    assert_eq!(cleared["text"], "");
    assert!(cleared["revision"].as_i64().unwrap() > revision);
    assert!(f.stored_text().await.is_none());
    assert_eq!(f.get(f.conversation).await["text"], "");

    let events = f.draft_events().await;
    let last = events.last().unwrap();
    assert_eq!(last["text"], "");
    assert_eq!(last["device_id"], "desktop");
    assert_eq!(
        last["revision"].as_i64().unwrap(),
        cleared["revision"].as_i64().unwrap()
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn text_is_capped_at_64_kib(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let at_limit = "x".repeat(65_536);
    f.put(
        f.conversation,
        json!({"text":at_limit,"base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;

    let too_long = "x".repeat(65_537);
    let refused = f
        .put(
            f.conversation,
            json!({"text":too_long,"base_revision":1,"device_id":"phone"}),
            400,
        )
        .await;
    assert_eq!(refused["error"]["code"], "invalid_request");
    assert_eq!(f.stored_text().await.unwrap().len(), 65_536);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_foreign_conversation_is_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other_owner = Uuid::now_v7();
    let foreign = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(other_owner)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into conversations(id,owner_id,machine_id,harness,workdir) values($1,$2,$3,'synthetic','/tmp/other')")
        .bind(foreign)
        .bind(other_owner)
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();

    let response = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{foreign}/draft"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 404);

    let response = f
        .client
        .send(
            Method::PUT,
            &format!("/api/v1/conversations/{foreign}/draft"),
            Some(&Uuid::now_v7().to_string()),
            Some(json!({"text":"чужое","base_revision":0,"device_id":"phone"})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 404);
    let rows: i64 = sqlx::query_scalar("select count(*) from composer_drafts")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_and_event_carry_the_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let saved = f
        .put(
            f.conversation,
            json!({"text":"строка из снимка","base_revision":0,"device_id":"phone"}),
            200,
        )
        .await;

    let snapshot = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}/snapshot", f.conversation),
            None,
            None,
        )
        .await
        .json::<Value>();
    assert_eq!(snapshot["data"]["draft"]["text"], "строка из снимка");
    assert_eq!(snapshot["data"]["draft"]["revision"], saved["revision"]);

    let events = f.draft_events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["conversation_id"], f.conversation.to_string());
    assert_eq!(events[0]["text"], "строка из снимка");
    assert_eq!(events[0]["device_id"], "phone");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sending_an_input_clears_the_draft_in_the_same_transaction(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_refused_input_keeps_the_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.put(
        f.conversation,
        json!({"text":"не должно пропасть","base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;
    sqlx::query("update queue_state set closed=true where conversation_id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();

    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/inputs", f.conversation),
            Some("send-refused-keeps-draft"),
            Some(json!({"content":[{"type":"text","text":"не должно пропасть"}]})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(
        f.stored_text().await.as_deref(),
        Some("не должно пропасть")
    );
    assert!(f.draft_events().await.len() == 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn scheduling_from_the_composer_clears_the_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.put(
        f.conversation,
        json!({"text":"отправлю позже","base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;
    let run_at = (chrono::Utc::now() + chrono::TimeDelta::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/scheduled", f.conversation),
            Some("schedule-clears-draft"),
            Some(json!({
                "run_at":run_at,
                "content":[{"type":"text","text":"отправлю позже"}],
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
    assert_eq!(events.last().unwrap()["text"], "");
    assert_eq!(events.last().unwrap()["device_id"], "phone");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_the_conversation_cascades_to_its_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.put(
        f.conversation,
        json!({"text":"исчезнет вместе с сессией","base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;
    sqlx::query("delete from queue_state where conversation_id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("delete from conversations where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from composer_drafts")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_id_is_stored_with_the_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.put(
        f.conversation,
        json!({"text":"владелец","base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from composer_drafts where conversation_id=$1")
        .bind(f.conversation)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(owner, f.owner);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attachment_list_round_trips_and_empty_draft_deletes_the_row(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let first = f.attachment(f.conversation, "ready").await;
    let second = f.attachment(f.conversation, "ready").await;

    let saved = f
        .put(
            f.conversation,
            json!({"text":"","attachment_ids":[first, second],"base_revision":0,"device_id":"phone"}),
            200,
        )
        .await;
    assert_eq!(saved["text"], "");
    assert_eq!(
        saved["attachment_ids"],
        json!([first.to_string(), second.to_string()])
    );
    assert!(f.stored_text().await.is_some());

    let loaded = f.get(f.conversation).await;
    assert_eq!(
        loaded["attachment_ids"],
        json!([first.to_string(), second.to_string()])
    );

    let removed = f
        .put(
            f.conversation,
            json!({"text":"","attachment_ids":[second],"base_revision":saved["revision"],"device_id":"desktop"}),
            200,
        )
        .await;
    assert_eq!(removed["attachment_ids"], json!([second.to_string()]));

    let cleared = f
        .put(
            f.conversation,
            json!({"text":"","attachment_ids":[],"base_revision":removed["revision"],"device_id":"desktop"}),
            200,
        )
        .await;
    assert_eq!(cleared["attachment_ids"], json!([]));
    assert!(f.stored_text().await.is_none());
    assert_eq!(f.draft_events().await.last().unwrap()["attachment_ids"], json!([]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_or_not_ready_attachments_are_rejected(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let foreign = f.attachment(other, "ready").await;
    let uploading = f.attachment(f.conversation, "uploading").await;
    let missing = Uuid::now_v7();

    for id in [foreign, uploading, missing] {
        let refused = f
            .put(
                f.conversation,
                json!({"text":"","attachment_ids":[id],"base_revision":0,"device_id":"phone"}),
                400,
            )
            .await;
        assert_eq!(refused["error"]["code"], "invalid_request", "{id}");
    }
    assert!(f.stored_text().await.is_none());
    assert!(f.draft_events().await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sending_with_attachments_consumes_them_and_clears_the_draft(pool: PgPool) {
    let f = Fixture::new(pool).await;
    sqlx::query("update machines set capabilities='{pilot_v1}', limits_ok=true where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let attachment = f.attachment(f.conversation, "ready").await;
    f.put(
        f.conversation,
        json!({"text":"с файлом","attachment_ids":[attachment],"base_revision":0,"device_id":"phone"}),
        200,
    )
    .await;

    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/inputs", f.conversation),
            Some("send-with-attachment"),
            Some(json!({
                "content":[{"type":"text","text":"с файлом"}],
                "attachment_ids":[attachment],
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
    let input_id: Uuid = response.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let linked: i64 = sqlx::query_scalar(
        "select count(*) from input_attachments where input_id=$1 and attachment_id=$2",
    )
    .bind(input_id)
    .bind(attachment)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(linked, 1);
    assert!(f.stored_text().await.is_none());
    let last = f.draft_events().await.pop().unwrap();
    assert_eq!(last["text"], "");
    assert_eq!(last["attachment_ids"], json!([]));
}
