//! HTTP владельца для правила автопинка: чтение, пауза, снятие (B2).

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    presets::{self, Actor, NewPreset, PresetPurpose},
    testkit::{self, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
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
            pool,
            client,
            owner,
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_creates_pauses_resumes_and_clears_a_rule(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_rule_accepts_only_nudge_text_presets(pool: PgPool) {
    let f = Fx::new(pool).await;
    let path = format!("/api/v1/conversations/{}/nudge", f.session);
    let nudge = presets::create(
        &f.pool,
        f.owner,
        "http_nudge_preset",
        NewPreset {
            title: "HTTP толчок".into(),
            description: None,
            text: "[canterlot] {markers}".into(),
            purpose: PresetPurpose::Nudge,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let instruction = presets::create(
        &f.pool,
        f.owner,
        "http_instruction_preset",
        NewPreset {
            title: "HTTP инструкция".into(),
            description: None,
            text: "обычный текст".into(),
            purpose: PresetPurpose::Instructions,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();

    let bad = f
        .send(
            Method::PUT,
            &path,
            Some("nudge_bad_preset"),
            Some(json!({"text_preset": instruction.summary.id})),
        )
        .await;
    assert_eq!(bad.status.as_u16(), 422);
    assert_eq!(bad.json::<Value>()["error"]["code"], "profile_unknown");

    let ok = f
        .send(
            Method::PUT,
            &path,
            Some("nudge_ok_preset"),
            Some(json!({"text_preset": nudge.summary.id})),
        )
        .await;
    assert_eq!(ok.status.as_u16(), 200);
    let row: Option<String> =
        sqlx::query_scalar("select text_preset_id from nudge_rules where conversation_id=$1")
            .bind(f.session)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(row.as_deref(), Some(nudge.summary.id.as_str()));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_http_is_limited_to_the_owners_sessions(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = testkit::seed(&f.pool).await;
    let foreign = testkit::seed_conversation(&f.pool, other.owner_id, other.machine_id).await;
    let response = f
        .send(
            Method::PUT,
            &format!("/api/v1/conversations/{foreign}/nudge"),
            Some("nudge_foreign"),
            Some(json!({})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 404);
}
