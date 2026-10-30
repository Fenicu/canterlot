//! Тесты задачи 2 плана 2c: пресеты инструкций в хабе.

use axum::http::Method;
use canterlot_hub::{
    events,
    http::{self, AppState},
    presets::{self, Actor, NewPreset, PresetError, PresetPatch, PresetRef, SEED_EXECUTOR_EN},
    snapshot, testkit,
    testkit::TestClient,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            owner,
            machine,
            _dir: dir,
        }
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

    async fn create(&self, key: &str, title: &str, text: &str) -> Value {
        let response = self
            .request(
                Method::POST,
                "/api/v1/presets",
                Some(key),
                Some(json!({"title": title, "text": text})),
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

fn schema() -> Value {
    serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap()
}

fn validator(name: &str) -> jsonschema::Validator {
    let mut s = schema();
    s["$ref"] = json!(format!("#/$defs/{name}"));
    s.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&s).unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_starts_with_builtins_then_custom_by_title(pool: PgPool) {
    let f = Fx::new(pool).await;
    let first: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    // Посев init уже создал «Исполнитель (английский)».
    assert_eq!(first[0]["id"], "clean");
    assert_eq!(first[0]["kind"], "clean");
    assert_eq!(first[0]["title"], "Без инструкций");
    assert!(first[0].get("text").is_none());
    assert!(first[0]["revision"].is_null());
    assert_eq!(first[1]["id"], "owner");
    assert_eq!(first[1]["kind"], "owner");
    assert_eq!(first[1]["title"], "Как у владельца");
    f.create("list_beta", "Бета", "текст беты").await;
    f.create("list_alpha", "альфа", "текст альфы").await;
    let list: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    let titles: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Без инструкций",
            "Как у владельца",
            "альфа",
            "Бета",
            "Исполнитель (английский)"
        ]
    );
    for preset in list.as_array().unwrap() {
        assert!(preset.get("text").is_none(), "в списке нет текста");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn builtins_cannot_be_updated_or_deleted(pool: PgPool) {
    let f = Fx::new(pool).await;
    for id in ["clean", "owner"] {
        let patch = f
            .request(
                Method::PATCH,
                &format!("/api/v1/presets/{id}"),
                Some(&format!("builtin_patch_{id}")),
                Some(json!({"title": "Другое", "base_revision": 1})),
            )
            .await;
        assert_eq!(patch.status.as_u16(), 409);
        assert_eq!(Fx::error_code(&patch), "preset_builtin");
        let delete = f
            .request(
                Method::DELETE,
                &format!("/api/v1/presets/{id}"),
                Some(&format!("builtin_delete_{id}")),
                None,
            )
            .await;
        assert_eq!(delete.status.as_u16(), 409);
        assert_eq!(Fx::error_code(&delete), "preset_builtin");
        let get: Value = f
            .request(Method::GET, &format!("/api/v1/presets/{id}"), None, None)
            .await
            .json();
        assert!(get["text"].is_null(), "у встроенного нет текста");
    }
    let missing = f
        .request(
            Method::GET,
            &format!("/api/v1/presets/{}", Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(missing.status.as_u16(), 404);
    assert_eq!(Fx::error_code(&missing), "preset_not_found");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_validates_title_text_and_limits(pool: PgPool) {
    let f = Fx::new(pool).await;
    // Пустой текст.
    let empty = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("valid_empty"),
            Some(json!({"title": "Пустой", "text": "   "})),
        )
        .await;
    assert_eq!(empty.status.as_u16(), 422);
    assert_eq!(Fx::error_code(&empty), "preset_invalid");
    assert_eq!(empty.json::<Value>()["error"]["details"]["field"], "text");
    // Текст больше 64 КиБ.
    let big = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("valid_big"),
            Some(json!({"title": "Большой", "text": "я".repeat(65_536 + 1)})),
        )
        .await;
    assert_eq!(big.status.as_u16(), 422);
    assert_eq!(Fx::error_code(&big), "preset_invalid");
    // Название как у встроенного, в другом регистре.
    let taken = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("valid_taken"),
            Some(json!({"title": "без инструкций", "text": "текст"})),
        )
        .await;
    assert_eq!(taken.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&taken), "preset_title_taken");
    // Пустое название и длинное описание.
    let bad_title = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("valid_bad_title"),
            Some(json!({"title": "  ", "text": "текст"})),
        )
        .await;
    assert_eq!(bad_title.status.as_u16(), 422);
    assert_eq!(
        bad_title.json::<Value>()["error"]["details"]["field"],
        "title"
    );
    let bad_description = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("valid_bad_description"),
            Some(json!({"title": "Описание", "description": "о".repeat(501), "text": "текст"})),
        )
        .await;
    assert_eq!(bad_description.status.as_u16(), 422);
    assert_eq!(
        bad_description.json::<Value>()["error"]["details"]["field"],
        "description"
    );
    // Посев уже занимает один из 100: ещё 99 — можно, сотый новый — нет.
    for i in 0..99 {
        f.create(&format!("limit_{i}"), &format!("Пресет {i}"), "текст")
            .await;
    }
    let over = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("limit_over"),
            Some(json!({"title": "Сто первый", "text": "текст"})),
        )
        .await;
    assert_eq!(over.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&over), "preset_limit_reached");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn update_requires_current_base_revision_and_bumps_revision(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("bump", "Версии", "первый").await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["revision"], 1);
    let stale = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_stale"),
            Some(json!({"title": "Другой", "base_revision": 999})),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&stale), "preset_revision_conflict");
    assert_eq!(
        stale.json::<Value>()["error"]["details"]["current_revision"],
        1
    );
    let updated: Value = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_ok"),
            Some(json!({"title": "Версии 2", "text": "второй", "base_revision": 1})),
        )
        .await
        .json();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["text"], "второй");
    let again = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_again"),
            Some(json!({"text": "третий", "base_revision": 1})),
        )
        .await;
    assert_eq!(again.status.as_u16(), 409);
    assert_eq!(
        again.json::<Value>()["error"]["details"]["current_revision"],
        2
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_revision_keeps_its_text(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = presets::create(
        &f.pool,
        f.owner,
        "rev_create",
        NewPreset {
            title: "Ревизии".into(),
            description: None,
            text: "первый".into(),
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let id: Uuid = created.summary.id.parse().unwrap();
    let reference = PresetRef::Custom(id);
    presets::update(
        &f.pool,
        f.owner,
        id,
        "rev_update_1",
        PresetPatch {
            title: None,
            description: None,
            text: Some("второй".into()),
            base_revision: 1,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    presets::update(
        &f.pool,
        f.owner,
        id,
        "rev_update_2",
        PresetPatch {
            title: Some("Ревизии 2".into()),
            description: None,
            text: Some("третий".into()),
            base_revision: 2,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let first = presets::resolve_in_tx(&mut conn, f.owner, &reference, Some(1))
        .await
        .unwrap();
    assert_eq!(first.text.as_deref(), Some("первый"));
    assert_eq!(first.revision, Some(1));
    let second = presets::resolve_in_tx(&mut conn, f.owner, &reference, Some(2))
        .await
        .unwrap();
    assert_eq!(second.text.as_deref(), Some("второй"));
    let current = presets::resolve_in_tx(&mut conn, f.owner, &reference, None)
        .await
        .unwrap();
    assert_eq!(current.text.as_deref(), Some("третий"));
    assert_eq!(current.revision, Some(3));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_is_soft_hides_from_list_and_keeps_revisions(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("soft", "Мягкий", "текст копии").await;
    let id = created["id"].as_str().unwrap();
    let delete = f
        .request(
            Method::DELETE,
            &format!("/api/v1/presets/{id}"),
            Some("soft_delete"),
            None,
        )
        .await;
    assert_eq!(delete.status.as_u16(), 204);
    let get = f
        .request(Method::GET, &format!("/api/v1/presets/{id}"), None, None)
        .await;
    assert_eq!(get.status.as_u16(), 404);
    assert_eq!(Fx::error_code(&get), "preset_not_found");
    let list: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    assert!(
        list.as_array().unwrap().iter().all(|p| p["id"] != id),
        "удалённого нет в списке"
    );
    let patch = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("soft_patch"),
            Some(json!({"title": "Новое", "base_revision": 1})),
        )
        .await;
    assert_eq!(patch.status.as_u16(), 404);
    let deleted: bool =
        sqlx::query_scalar("select deleted_at is not null from instruction_presets where id=$1")
            .bind(id.parse::<Uuid>().unwrap())
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(deleted);
    // Снимок запуска (задача 3) работает и для удалённого пресета.
    let mut conn = f.pool.acquire().await.unwrap();
    let resolved = presets::resolve_in_tx(
        &mut conn,
        f.owner,
        &PresetRef::Custom(id.parse().unwrap()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(resolved.text.as_deref(), Some("текст копии"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn agent_actor_is_recorded_as_origin_and_last_edit(pool: PgPool) {
    let f = Fx::new(pool).await;
    let conversation = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let created = presets::create(
        &f.pool,
        f.owner,
        "agent_origin",
        NewPreset {
            title: "Агентский".into(),
            description: None,
            text: "текст".into(),
        },
        Actor::Agent {
            conversation_id: conversation,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(created.summary.created.by, "agent");
    assert_eq!(created.summary.created.conversation_id, Some(conversation));
    assert!(created.summary.last_edit.is_none());
    let id: Uuid = created.summary.id.parse().unwrap();
    let updated = presets::update(
        &f.pool,
        f.owner,
        id,
        "agent_origin_update",
        PresetPatch {
            title: None,
            description: Some("правка владельца".into()),
            text: None,
            base_revision: 1,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(updated.summary.revision, Some(2));
    assert_eq!(updated.summary.created.by, "agent");
    let last = updated.summary.last_edit.unwrap();
    assert_eq!(last.by, "owner");
    assert!(last.conversation_id.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn used_by_counts_effective_and_pending_references(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("used", "Используемый", "текст").await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["used_by"], 0);
    let effective = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    sqlx::query(
        "update conversations set instruction_profile=$1,instruction_preset_revision=1 where id=$2",
    )
    .bind(id)
    .bind(effective)
    .execute(&f.pool)
    .await
    .unwrap();
    let pending = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    sqlx::query("update conversations set pending_settings=jsonb_build_object('instruction_profile',$1) where id=$2")
        .bind(id)
        .bind(pending)
        .execute(&f.pool)
        .await
        .unwrap();
    let archived = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    sqlx::query("update conversations set instruction_profile=$1,instruction_preset_revision=1, archived_at=clock_timestamp() where id=$2")
        .bind(id)
        .bind(archived)
        .execute(&f.pool)
        .await
        .unwrap();
    let detail: Value = f
        .request(Method::GET, &format!("/api/v1/presets/{id}"), None, None)
        .await
        .json();
    assert_eq!(detail["used_by"], 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn preset_events_and_snapshot_carry_no_text(pool: PgPool) {
    let f = Fx::new(pool).await;
    let canary = "CANARY-PRESET-2C-TASK2-absence-check";
    let created = f.create("event_text", "Событийный", canary).await;
    let id = created["id"].as_str().unwrap();
    f.request(
        Method::PATCH,
        &format!("/api/v1/presets/{id}"),
        Some("event_text_update"),
        Some(json!({"text": "CANARY-PRESET-2C-TASK2-второй", "base_revision": 1})),
    )
    .await;
    // Снимок до удаления несёт сводку без текста.
    let snapshot = snapshot::conversations(&f.pool, f.owner).await.unwrap();
    let text = serde_json::to_string(&snapshot).unwrap();
    assert!(
        !text.contains("CANARY-PRESET-2C-TASK2"),
        "в снимке нет текста"
    );
    let listed = snapshot
        .data
        .presets
        .iter()
        .find(|p| p.id == id)
        .expect("снимок несёт сводки пресетов");
    assert_eq!(listed.revision, Some(2));
    assert_eq!(
        listed.text_bytes,
        Some("CANARY-PRESET-2C-TASK2-второй".len() as i64)
    );
    f.request(
        Method::DELETE,
        &format!("/api/v1/presets/{id}"),
        Some("event_text_delete"),
        None,
    )
    .await;
    let rows: Vec<(String, Value)> =
        sqlx::query_as("select kind, payload from events where scope=$1 order by scope_seq")
            .bind(format!("owner:{}:conversations", f.owner))
            .fetch_all(&f.pool)
            .await
            .unwrap();
    let preset_rows: Vec<_> = rows
        .iter()
        .filter(|(kind, _)| kind == "instruction_preset_changed")
        .collect();
    assert_eq!(preset_rows.len(), 3);
    for (i, (kind, payload)) in rows.iter().enumerate() {
        if kind != "instruction_preset_changed" {
            continue;
        }
        let stored = events::StoredEvent {
            id: Uuid::now_v7(),
            scope: format!("owner:{}:conversations", f.owner),
            scope_seq: (i + 1) as i64,
            kind: kind.clone(),
            conversation_id: None,
            execution_id: None,
            payload: payload.clone(),
            correlation_id: Uuid::now_v7(),
            committed_at: chrono::Utc::now(),
        };
        let frame = serde_json::to_value(&events::project_for_client(vec![stored])[0]).unwrap();
        assert!(validator("instruction_preset_changed").is_valid(&frame));
        let text = serde_json::to_string(&frame).unwrap();
        assert!(
            !text.contains("CANARY-PRESET-2C-TASK2"),
            "в событии нет текста"
        );
        assert_eq!(frame["payload"]["deleted"], json!(i == 2));
    }
    let after = snapshot::conversations(&f.pool, f.owner).await.unwrap();
    let after_text = serde_json::to_string(&after).unwrap();
    assert!(
        !after_text.contains("CANARY-PRESET-2C-TASK2"),
        "в снимке нет текста"
    );
    assert!(
        after.data.presets.iter().all(|p| p.id != id),
        "удалённого нет в снимке"
    );
    // Текст доступен только в детальном чтении и не попадает в Debug.
    let mut conn = f.pool.acquire().await.unwrap();
    let detail = presets::get(&mut conn, f.owner, &PresetRef::Custom(id.parse().unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(detail, PresetError::NotFound));
    let live = f.create("event_debug", "Отладочный", canary).await;
    let live_id: Uuid = live["id"].as_str().unwrap().parse().unwrap();
    let detail = presets::get(&mut conn, f.owner, &PresetRef::Custom(live_id))
        .await
        .unwrap();
    assert_eq!(detail.text.as_deref(), Some(canary));
    assert!(!format!("{detail:?}").contains(canary));
    let resolved = presets::resolve_in_tx(&mut conn, f.owner, &PresetRef::Custom(live_id), None)
        .await
        .unwrap();
    assert!(!format!("{resolved:?}").contains(canary));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idempotent_create_replays_and_conflicts_on_other_body(pool: PgPool) {
    let f = Fx::new(pool).await;
    let body = Some(json!({"title": "Идемпотентный", "text": "текст"}));
    let first = f
        .request(Method::POST, "/api/v1/presets", Some("idem"), body.clone())
        .await;
    assert_eq!(first.status.as_u16(), 201);
    let second = f
        .request(Method::POST, "/api/v1/presets", Some("idem"), body)
        .await;
    assert_eq!(second.status.as_u16(), 201);
    assert_eq!(first.body, second.body, "повтор возвращает тот же ответ");
    let conflict = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("idem"),
            Some(json!({"title": "Другой", "text": "текст"})),
        )
        .await;
    assert_eq!(conflict.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&conflict), "idempotency_conflict");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn seed_preset_is_created_once_per_owner_and_not_recreated_after_delete(pool: PgPool) {
    let f = Fx::new(pool).await;
    // Владелец из init: посев уже создан.
    let list: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    let seed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["title"] == SEED_EXECUTOR_EN.title)
        .cloned()
        .expect("посев исполнителя в списке");
    assert_eq!(seed["kind"], "custom");
    assert_eq!(seed["revision"], 1);
    assert_eq!(seed["description"], SEED_EXECUTOR_EN.description);
    assert_eq!(seed["created"]["by"], "owner");
    let id = seed["id"].as_str().unwrap();
    let detail: Value = f
        .request(Method::GET, &format!("/api/v1/presets/{id}"), None, None)
        .await
        .json();
    assert_eq!(detail["text"], SEED_EXECUTOR_EN.text);
    // Повторный посев не дублирует.
    let mut conn = f.pool.acquire().await.unwrap();
    presets::seed_in_tx(&mut conn, f.owner).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets where owner_id=$1 and lower(title)=lower($2)",
    )
    .bind(f.owner)
    .bind(SEED_EXECUTOR_EN.title)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    // Удаление посева не приводит к его воссозданию.
    f.request(
        Method::DELETE,
        &format!("/api/v1/presets/{id}"),
        Some("seed_delete"),
        None,
    )
    .await;
    presets::seed_in_tx(&mut conn, f.owner).await.unwrap();
    let alive: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets where owner_id=$1 and deleted_at is null and lower(title)=lower($2)",
    )
    .bind(f.owner)
    .bind(SEED_EXECUTOR_EN.title)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(alive, 0);
    // С var marker: существующий владелец без метки получает посев на старте.
    let other: Uuid = sqlx::query_scalar("insert into owners(id) values($1) returning id")
        .bind(Uuid::now_v7())
        .fetch_one(&f.pool)
        .await
        .unwrap();
    presets::seed_all_missing(&f.pool).await.unwrap();
    let seeded: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets where owner_id=$1 and deleted_at is null and lower(title)=lower($2)",
    )
    .bind(other)
    .bind(SEED_EXECUTOR_EN.title)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(seeded, 1);
    presets::seed_all_missing(&f.pool).await.unwrap();
    let again: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets where owner_id=$1 and lower(title)=lower($2)",
    )
    .bind(other)
    .bind(SEED_EXECUTOR_EN.title)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(again, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_clears_pending_references_and_keeps_effective_snapshot(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("keep-copy", "Временный", "текст копии").await;
    let id = created["id"].as_str().unwrap();
    // Сессия применяет пресет: эффективная ссылка закреплена за ревизией 1.
    let conv: Uuid = sqlx::query_scalar("insert into conversations(id,owner_id,machine_id,harness,workdir,model,effort,permission_mode,instruction_profile,instruction_preset_revision) values($1,$2,$3,'synthetic','/tmp/w','synthetic-small','low','ask',$4,1) returning id")
        .bind(Uuid::now_v7())
        .bind(f.owner)
        .bind(f.machine)
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    sqlx::query("update conversations set pending_settings=$2 where id=$1")
        .bind(conv)
        .bind(json!({"instruction_profile": id}))
        .execute(&f.pool)
        .await
        .unwrap();
    let deleted = f
        .request(
            Method::DELETE,
            &format!("/api/v1/presets/{id}"),
            Some("clear-pending"),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    // Висячая ссылка снята с видимой отменой, эффективная копия на месте.
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(conv)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(pending.as_object().unwrap().is_empty());
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{conv}")).fetch_one(&f.pool).await.unwrap();
    assert_eq!(event["rejected"]["keys"], json!(["instruction_profile"]));
    assert_eq!(event["rejected"]["reason"], "preset_deleted");
    // Эффективная ссылка и закрепление не тронуты; в представлении — удалённый.
    let row: (String, Option<i64>) = sqlx::query_as(
        "select instruction_profile, instruction_preset_revision from conversations where id=$1",
    )
    .bind(conv)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(row.0, id);
    assert_eq!(row.1, Some(1));
    let applied = canterlot_hub::presets::applied_in_tx(
        &mut f.pool.acquire().await.unwrap(),
        f.owner,
        id,
        Some(1),
    )
    .await
    .unwrap();
    assert!(applied.deleted);
    assert_eq!(applied.revision, Some(1));
    assert!(applied.current_revision.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_in_description_or_text_is_invalid_not_internal(pool: PgPool) {
    let f = Fx::new(pool).await;
    for (key, body, field) in [
        (
            "nul_description",
            json!({"title": "Нуль", "description": "до\u{0}после", "text": "текст"}),
            "description",
        ),
        (
            "nul_text",
            json!({"title": "Нуль", "text": "до\u{0}после"}),
            "text",
        ),
    ] {
        let response = f
            .request(Method::POST, "/api/v1/presets", Some(key), Some(body))
            .await;
        assert_eq!(
            response.status.as_u16(),
            422,
            "{field}: {}",
            String::from_utf8_lossy(&response.body)
        );
        assert_eq!(Fx::error_code(&response), "preset_invalid");
        assert_eq!(response.json::<Value>()["error"]["details"]["field"], field);
    }
    let created = f.create("nul_base", "Правка", "текст").await;
    let id = created["id"].as_str().unwrap();
    for (key, body, field) in [
        (
            "nul_patch_description",
            json!({"description": "\u{0}", "base_revision": 1}),
            "description",
        ),
        (
            "nul_patch_text",
            json!({"text": "a\u{0}b", "base_revision": 1}),
            "text",
        ),
    ] {
        let response = f
            .request(
                Method::PATCH,
                &format!("/api/v1/presets/{id}"),
                Some(key),
                Some(body),
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            422,
            "{field}: {}",
            String::from_utf8_lossy(&response.body)
        );
        assert_eq!(response.json::<Value>()["error"]["details"]["field"], field);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_with_pending_reference_takes_queue_before_list_counter(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("lock-order", "Блокировки", "текст").await;
    let id = created["id"].as_str().unwrap().to_owned();
    let conversation = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    sqlx::query("update conversations set pending_settings=$2 where id=$1")
        .bind(conversation)
        .bind(json!({"instruction_profile": id}))
        .execute(&f.pool)
        .await
        .unwrap();
    // Пересчёт активности: очередь, строка разговора, затем счётчики событий.
    let mut refresh = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
        .bind(conversation)
        .execute(&mut *refresh)
        .await
        .unwrap();
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation)
        .execute(&mut *refresh)
        .await
        .unwrap();
    let delete = {
        let (pool, owner) = (f.pool.clone(), f.owner);
        let preset: Uuid = id.parse().unwrap();
        tokio::spawn(async move {
            presets::delete(&pool, owner, preset, "delete-lock", Uuid::now_v7())
                .await
                .map_err(|error| error.to_string())
        })
    };
    for _ in 0..300 {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        if delete.is_finished() || waiting > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let appended = events::append(
        &mut refresh,
        events::NewEvent {
            scope: events::Scope::Conversations { owner_id: f.owner },
            kind: "derived_changed",
            conversation_id: Some(conversation),
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| error.to_string());
    let committed = refresh.commit().await.map_err(|error| error.to_string());
    let deleted = delete.await.unwrap();
    assert_eq!((appended, committed, deleted), (Ok(()), Ok(()), Ok(())));
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(pending, json!({}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idempotency_fingerprints_preset_text_and_stores_no_text(pool: PgPool) {
    let f = Fx::new(pool).await;
    let created = f.create("text-key", "Отпечаток", "первый-секрет").await;
    assert_eq!(created["text"], "первый-секрет");
    // Тот же ключ и та же длина, другой текст: правка не должна потеряться молча.
    let other = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("text-key"),
            Some(json!({"title": "Отпечаток", "text": "второй-секрет"})),
        )
        .await;
    assert_eq!(
        other.status.as_u16(),
        409,
        "{}",
        String::from_utf8_lossy(&other.body)
    );
    assert_eq!(Fx::error_code(&other), "idempotency_conflict");
    let replay = f
        .request(
            Method::POST,
            "/api/v1/presets",
            Some("text-key"),
            Some(json!({"title": "Отпечаток", "text": "первый-секрет"})),
        )
        .await;
    assert_eq!(replay.status.as_u16(), 201);
    assert_eq!(replay.json::<Value>()["text"], "первый-секрет");
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/v1/presets/{id}");
    let patch = |text: &str| json!({"text": text, "base_revision": 1});
    let updated = f
        .request(
            Method::PATCH,
            &path,
            Some("patch-key"),
            Some(patch("новый-текст-А")),
        )
        .await;
    assert_eq!(updated.status.as_u16(), 200);
    let other = f
        .request(
            Method::PATCH,
            &path,
            Some("patch-key"),
            Some(patch("новый-текст-Б")),
        )
        .await;
    assert_eq!(other.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&other), "idempotency_conflict");
    let replay = f
        .request(
            Method::PATCH,
            &path,
            Some("patch-key"),
            Some(patch("новый-текст-А")),
        )
        .await;
    assert_eq!(replay.status.as_u16(), 200);
    assert_eq!(replay.json::<Value>()["text"], "новый-текст-А");
    let stored: Vec<String> = sqlx::query_scalar(
        "select response::text from idempotency_keys where operation in ('create_preset', 'update_preset')",
    )
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    for response in stored {
        assert!(
            !response.contains("секрет") && !response.contains("новый-текст"),
            "preset text kept in idempotency storage: {response}"
        );
    }
}
