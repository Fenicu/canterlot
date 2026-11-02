use axum::http::Method;
use canterlot_hub::{
    executions,
    http::{self, AppState},
    session::{CAPABILITY_WORKDIR_SUGGEST_V1, MAX_SUGGESTS_PER_MACHINE, Registry},
    testkit::{self, TestClient},
};
use canterlot_proto::worker::v1::{SuggestWorkdirs, WorkdirSuggestions, control_down};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
    registry: canterlot_hub::session::Registry,
    _leases: Vec<canterlot_hub::session::TestLease>,
    _dir: tempfile::TempDir,
}
impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let registry = deps.registry.clone();
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')").bind(machine).bind(owner).execute(&pool).await.unwrap();
        testkit::seed_catalog(&pool, machine).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        let mut this = Self {
            pool,
            client,
            owner,
            machine,
            registry,
            _leases: Vec::new(),
            _dir: dir,
        };
        // Машина пилота по умолчанию знает снимок инструкций (план 2c, задача 3).
        this.caps(&["pilot_v1", "instructions_v1"]);
        this
    }

    fn caps(&mut self, capabilities: &[&str]) {
        self._leases.clear();
        let lease = self.registry.connect_for_tests(
            self.machine,
            &capabilities
                .iter()
                .map(|c| (*c).to_owned())
                .collect::<Vec<_>>(),
        );
        self._leases.push(lease);
    }

    fn body_with_profile(&self, profile: &str) -> Value {
        let mut body = self.body();
        body["instruction_profile"] = json!(profile);
        body
    }

    async fn patch_settings(&self, id: Uuid, body: Value) -> Value {
        self.request(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}/settings"),
            Some(body),
            200,
        )
        .await
    }

    async fn create_preset(&mut self, title: &str, text: &str) -> Value {
        let r = self
            .client
            .send(
                Method::POST,
                "/api/v1/presets",
                Some(&Uuid::now_v7().to_string()),
                Some(json!({"title": title, "text": text})),
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            201,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        r.json()
    }
    fn body(&self) -> Value {
        json!({"machine_id":self.machine,"harness":"synthetic","workdir":"/tmp/w","model":"synthetic-small","effort":"high"})
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>, status: u16) -> Value {
        let r = self
            .client
            .send(method, path, Some(&Uuid::now_v7().to_string()), body)
            .await;
        assert_eq!(
            r.status.as_u16(),
            status,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.headers["cache-control"], "no-store");
        if r.body.is_empty() {
            Value::Null
        } else {
            r.json()
        }
    }
    async fn create(&self, patch: Value) -> Uuid {
        let mut body = self.body();
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = self
            .request(Method::POST, "/api/v1/conversations", Some(body), 201)
            .await;
        r["conversation_id"].as_str().unwrap().parse().unwrap()
    }
    async fn get(&self, id: Uuid) -> Value {
        self.request(
            Method::GET,
            &format!("/api/v1/conversations/{id}"),
            None,
            200,
        )
        .await
    }
    async fn patch(&self, id: Uuid, body: Value) -> Value {
        self.request(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}"),
            Some(body),
            200,
        )
        .await
    }
    async fn list(&self, query: &str) -> Value {
        self.request(
            Method::GET,
            &format!("/api/v1/conversations{query}"),
            None,
            200,
        )
        .await
    }
    async fn group(&self, name: &str) -> Uuid {
        self.request(
            Method::POST,
            "/api/v1/groups",
            Some(json!({"name":name})),
            201,
        )
        .await["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }
    async fn events(&self, kind: &str) -> Vec<Value> {
        sqlx::query_scalar(
            "select payload from events where scope=$1 and kind=$2 order by scope_seq",
        )
        .bind(format!("owner:{}:conversations", self.owner))
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_validates_against_machine_catalog_with_distinct_codes(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for (field, value, code) in [
        ("machine_id", json!(Uuid::now_v7()), "machine_not_found"),
        ("harness", json!("missing"), "harness_unknown"),
        ("model", json!("missing"), "model_unknown"),
        ("effort", json!("missing"), "effort_not_allowed"),
        (
            "permission_mode",
            json!("missing"),
            "permission_mode_unknown",
        ),
        ("instruction_profile", json!("missing"), "profile_unknown"),
        ("workdir", json!("/etc"), "workdir_not_allowed"),
        ("group_ids", json!([Uuid::now_v7()]), "group_not_found"),
    ] {
        let mut b = f.body();
        b[field] = value;
        let r = f
            .client
            .send(
                Method::POST,
                "/api/v1/conversations",
                Some("validation"),
                Some(b),
            )
            .await;
        assert!(r.status.is_client_error());
        assert_eq!(r.json::<Value>()["error"]["code"], code);
    }
    sqlx::query("update machines set capabilities='{}' where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let r = f
        .request(Method::POST, "/api/v1/conversations", Some(f.body()), 409)
        .await;
    assert_eq!(r["error"]["code"], "capability_missing");
    testkit::seed_catalog(&f.pool, f.machine).await;
    sqlx::query("update machines set availability='unavailable' where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut b = f.body();
    b["start"] = json!(true);
    assert_eq!(
        f.request(Method::POST, "/api/v1/conversations", Some(b), 409)
            .await["error"]["code"],
        "machine_unavailable"
    );
    f.create(json!({})).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from idempotency_keys where key='validation'"
        )
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        0
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_is_idempotent_by_key(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let a = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some("create"),
            Some(f.body()),
        )
        .await;
    let b = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some("create"),
            Some(f.body()),
        )
        .await;
    assert_eq!(a.status, 201);
    assert_eq!(a.body, b.body);
    let mut changed = f.body();
    changed["title"] = json!("Другая");
    let c = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some("create"),
            Some(changed),
        )
        .await;
    assert_eq!(c.status, 409);
    assert_eq!(c.json::<Value>()["error"]["code"], "idempotency_conflict");
    assert_eq!(f.list("").await["items"].as_array().unwrap().len(), 1);
    assert_eq!(f.events("conversation_created").await.len(), 1);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_with_start_queues_start_execution_with_settings(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f
        .create(json!({"start":true,"permission_mode":"bypass"}))
        .await;
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    for (k, v) in [
        ("model", json!("synthetic-small")),
        ("effort", json!("high")),
        ("permission_mode", json!("bypass")),
    ] {
        assert_eq!(payload[k], v);
    }
    assert!(payload.get("instruction_profile").is_none());
    assert_eq!(payload["instructions"]["mode"], "clean");
    assert!(payload.get("capabilities").is_none());
    assert!(!f.get(id).await["execution"].is_null());
    let created = f.events("conversation_created").await;
    assert_eq!(created[0]["model"], "synthetic-small");
    assert_eq!(created[0]["counts"]["all"], 1);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_conversation_has_queue_state_and_no_live_execution_hold(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    let hold: Value =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(hold, json!([{"reason":"no_live_execution"}]));
    let d = f.get(id).await;
    assert_eq!(d["activity"]["state"], "idle");
    assert!(d["execution"].is_null());
    assert_eq!(d["settings"]["effective"]["permission_mode"], "bypass");
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_conversation_defaults_to_bypass_and_keeps_an_explicit_choice(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let default = f.create(json!({})).await;
    assert_eq!(f.get(default).await["permission_mode"], "bypass");
    sqlx::query("update machine_harnesses set permission_modes='[\"owner\",\"bypass\"]'")
        .execute(&f.pool)
        .await
        .unwrap();
    let chosen = f.create(json!({"permission_mode":"owner"})).await;
    assert_eq!(f.get(chosen).await["permission_mode"], "owner");
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_without_a_chosen_mode_starts_as_owner_and_keeps_its_stored_value(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    sqlx::query("update conversations set permission_mode='' where id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    f.request(
        Method::POST,
        &format!("/api/v1/conversations/{id}/executions"),
        None,
        201,
    )
    .await;
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["permission_mode"], "owner");
    let stored: String =
        sqlx::query_scalar("select permission_mode from conversations where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(stored, "");
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pin_color_archive_title_roundtrip_and_emit_list_events(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    let d = f
        .patch(id, json!({"title":"Название","color":"teal","pinned":true}))
        .await;
    assert_eq!(d["title"], "Название");
    assert_eq!(d["title_source"], "user");
    assert_eq!(d["color"], "teal");
    assert_eq!(d["pinned"], true);
    f.patch(id, json!({"color":null,"archived":true})).await;
    assert_eq!(f.get(id).await["color"], Value::Null);
    assert_eq!(f.get(id).await["archived"], true);
    f.patch(id, json!({"archived":false,"pinned":false})).await;
    assert_eq!(f.events("conversation_updated").await.len(), 3);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn conversation_updated_carries_attributes_and_counts_and_never_volatile_fields(
    pool: PgPool,
) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    f.patch(id, json!({"title":"Ручное"})).await;
    let p = &f.events("conversation_updated").await[0];
    assert_eq!(p["id"], id.to_string());
    assert_eq!(p["title"], "Ручное");
    assert_eq!(p["counts"]["all"], 1);
    for k in [
        "activity",
        "unread_count",
        "queue",
        "crunch",
        "last_activity_at",
        "model",
        "effort",
        "permission_mode",
        "settings",
    ] {
        assert!(p.get(k).is_none(), "{k}");
    }
    let chat: Value = sqlx::query_scalar(
        "select payload from events where scope=$1 and kind='conversation_updated'",
    )
    .bind(format!("conversation:{id}"))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(*p, chat);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn archived_conversations_leave_default_list_and_appear_in_archive(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    f.patch(id, json!({"pinned":true,"archived":true})).await;
    assert!(f.list("").await["items"].as_array().unwrap().is_empty());
    assert!(
        f.list("?filter=pinned").await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let archived = f.list("?filter=archived").await;
    assert_eq!(archived["items"][0]["id"], id.to_string());
    assert_eq!(archived["counts"]["archived"], 1);
    assert_eq!(archived["counts"]["all"], 0);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn archiving_with_active_crunch_or_schedule_answers_409_unless_forced(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"crunch":{"enabled":true}})).await;
    f.request(Method::POST,&format!("/api/v1/conversations/{id}/scheduled"),Some(json!({"run_at":Utc::now()+TimeDelta::hours(1),"content":[{"type":"text","text":"scheduled"}]})),201).await;
    let path = format!("/api/v1/conversations/{id}");
    let r = f
        .request(Method::PATCH, &path, Some(json!({"archived":true})), 409)
        .await;
    assert_eq!(r["error"]["code"], "archive_active");
    assert_eq!(
        r["error"]["details"]["reasons"],
        json!(["crunch", "scheduled"])
    );
    let d = f.patch(id, json!({"archived":true,"force":true})).await;
    assert_eq!(d["archived"], true);
    assert_eq!(d["crunch"]["enabled"], false);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from scheduled_jobs where state='pending'")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("select count(*) from events where scope=$1 and kind='scheduled_changed' and payload->>'state'='cancelled'").bind(format!("conversation:{id}")).fetch_one(&f.pool).await.unwrap(),1);
}

async fn set_phase(f: &Fixture, id: Uuid, phase: &str) {
    sqlx::query("update executions set state='running',phase=$2 where conversation_id=$1")
        .bind(id)
        .bind(phase)
        .execute(&f.pool)
        .await
        .unwrap();
}

async fn archive_refusal(f: &Fixture, id: Uuid) -> Value {
    let path = format!("/api/v1/conversations/{id}");
    let r = f
        .request(Method::PATCH, &path, Some(json!({"archived":true})), 409)
        .await;
    assert_eq!(r["error"]["code"], "archive_active");
    r["error"]["details"]["reasons"].clone()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_process_archives_without_a_warning_and_stays_alive(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"start":true})).await;
    set_phase(&f, id, "awaiting_input").await;
    let archived = f.patch(id, json!({"archived":true})).await;
    assert_eq!(archived["archived"], true);
    let state: String = sqlx::query_scalar("select state from executions where conversation_id=$1")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(state, "running");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_in_progress_still_asks_before_archiving(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for phase in [
        "generating",
        "running_tool",
        "context_maintenance",
        "awaiting_permission",
        "awaiting_answer",
    ] {
        let id = f.create(json!({"start":true})).await;
        set_phase(&f, id, phase).await;
        assert_eq!(
            archive_refusal(&f, id).await,
            json!(["live_execution"]),
            "{phase}"
        );
        f.patch(id, json!({"archived":true,"force":true})).await;
    }
    let held = f.create(json!({"start":true})).await;
    set_phase(&f, held, "awaiting_input").await;
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(held)
    .bind(Uuid::now_v7())
    .execute(&f.pool)
    .await
    .unwrap();
    assert_eq!(archive_refusal(&f, held).await, json!(["live_execution"]));
    let asked = f.create(json!({"start":true})).await;
    set_phase(&f, asked, "awaiting_input").await;
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload)
         select $1,$2,e.id,e.generation,'question','archive','open','{}' from executions e where e.conversation_id=$2",
    )
    .bind(Uuid::now_v7())
    .bind(asked)
    .execute(&f.pool)
    .await
    .unwrap();
    assert_eq!(archive_refusal(&f, asked).await, json!(["live_execution"]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_lost_owner_is_no_turn_and_a_repeated_archive_answers_the_same(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"start":true})).await;
    sqlx::query(
        "update executions set state='owner_lost',phase='generating' where conversation_id=$1",
    )
    .bind(id)
    .execute(&f.pool)
    .await
    .unwrap();
    let path = format!("/api/v1/conversations/{id}");
    let key = Uuid::now_v7().to_string();
    let body = json!({"archived":true});
    let first = f
        .client
        .send(Method::PATCH, &path, Some(&key), Some(body.clone()))
        .await;
    let second = f
        .client
        .send(Method::PATCH, &path, Some(&key), Some(body))
        .await;
    assert_eq!(first.status.as_u16(), 200);
    assert_eq!(second.status.as_u16(), 200);
    assert_eq!(first.body, second.body);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn title_search_is_case_insensitive_and_escapes_like_wildcards(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for title in ["Ёлка", "Сборка", "100%_done\\", "100xxdone"] {
        f.create(json!({"title":title})).await;
    }
    for (query, title) in [
        ("%D1%91%D0%BB%D0%BA%D0%B0", "Ёлка"),
        ("%D1%81%D0%B1%D0%BE%D1%80%D0%BA%D0%B0", "Сборка"),
        ("%25%5F", "100%_done\\"),
        ("%5C", "100%_done\\"),
    ] {
        let p = f.list(&format!("?q={query}")).await;
        assert_eq!(p["items"].as_array().unwrap().len(), 1);
        assert_eq!(p["items"][0]["title"], title);
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn groups_are_many_to_many_and_counted(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let a = f.group("А").await;
    let b = f.group("Б").await;
    let id = f.create(json!({"group_ids":[a,b]})).await;
    f.create(json!({"group_ids":[a]})).await;
    assert_eq!(f.get(id).await["groups"], json!([a, b]));
    let page = f.list(&format!("?filter=group:{a}")).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let counts = page["counts"]["groups"].as_array().unwrap();
    assert_eq!(
        counts.iter().find(|c| c["id"] == a.to_string()).unwrap()["count"],
        2
    );
    f.request(
        Method::PUT,
        &format!("/api/v1/conversations/{id}/groups"),
        Some(json!({"group_ids":[b]})),
        200,
    )
    .await;
    assert_eq!(
        f.list(&format!("?filter=group:{a}")).await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn group_names_are_unique_case_insensitively(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let a = f.group("Ёлка").await;
    let b = f.group("Другая").await;
    let r = f
        .request(
            Method::POST,
            "/api/v1/groups",
            Some(json!({"name":"ёлка"})),
            409,
        )
        .await;
    assert_eq!(r["error"]["code"], "group_name_taken");
    f.request(
        Method::PATCH,
        &format!("/api/v1/groups/{b}"),
        Some(json!({"name":"ёлка"})),
        409,
    )
    .await;
    f.request(
        Method::PATCH,
        &format!("/api/v1/groups/{a}"),
        Some(json!({"name":"Сборка","position":2})),
        200,
    )
    .await;
    let groups = f.request(Method::GET, "/api/v1/groups", None, 200).await;
    assert_eq!(groups[1]["name"], "Сборка");
    f.request(
        Method::POST,
        "/api/v1/groups",
        Some(json!({"name":""})),
        400,
    )
    .await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_group_keeps_its_conversations(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let g = f.group("Группа").await;
    let id = f.create(json!({"group_ids":[g]})).await;
    f.request(Method::DELETE, &format!("/api/v1/groups/{g}"), None, 204)
        .await;
    assert_eq!(f.get(id).await["groups"], json!([]));
    assert_eq!(f.list("").await["counts"]["all"], 1);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn group_deletion_emits_conversation_updated_for_former_members_with_counts(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let g = f.group("Группа").await;
    let ids = vec![
        f.create(json!({"group_ids":[g]})).await,
        f.create(json!({"group_ids":[g]})).await,
    ];
    f.request(Method::DELETE, &format!("/api/v1/groups/{g}"), None, 204)
        .await;
    let events = f.events("conversation_updated").await;
    assert_eq!(events.len(), 2);
    for (e, id) in events.iter().zip(ids) {
        assert_eq!(e["id"], id.to_string());
        assert_eq!(e["groups"], json!([]));
        assert_eq!(e["counts"]["groups"], json!([]));
        assert_eq!(e["counts"]["all"], 2);
    }
    assert_eq!(
        f.events("group_changed").await.last().unwrap()["counts"]["all"],
        2
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_pages_by_cursor_without_gaps(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut ids = Vec::new();
    for i in 0..9 {
        let id = f.create(json!({})).await;
        ids.push(id);
        sqlx::query("update conversations set last_activity_at=clock_timestamp()+$2 * interval '1 second' where id=$1").bind(id).bind(i).execute(&f.pool).await.unwrap();
    }
    let first = f.list("?limit=3").await;
    let mut seen: Vec<String> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_owned())
        .collect();
    sqlx::query(
        "update conversations set last_activity_at=clock_timestamp()-interval '1 day' where id=$1",
    )
    .bind(seen[0].parse::<Uuid>().unwrap())
    .execute(&f.pool)
    .await
    .unwrap();
    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
    while let Some(c) = cursor {
        let p = f.list(&format!("?limit=3&cursor={c}")).await;
        for item in p["items"].as_array().unwrap() {
            let id = item["id"].as_str().unwrap().to_owned();
            assert!(!seen.contains(&id));
            seen.push(id);
        }
        cursor = p["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(seen.len(), ids.len());
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_item_machine_is_id_and_name_only(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.create(json!({})).await;
    let m = f.list("").await["items"][0]["machine"].clone();
    assert_eq!(m, json!({"id":f.machine,"name":"Машина"}));
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_item_carries_working_time_without_owner_waits(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = f.create(json!({})).await;
    let execution = executions::start(&pool, f.owner, id, Uuid::now_v7())
        .await
        .unwrap();
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(id)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .execute(&pool)
    .await
    .unwrap();
    let finished = Utc::now();
    let started = finished - TimeDelta::minutes(10);
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms)
         values($1,$2,$3,$4,$5,600000)",
    )
    .bind(command)
    .bind(execution.execution_id)
    .bind(id)
    .bind(started)
    .bind(finished)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload,created_at,updated_at)
         values($1,$2,$3,$4,'permission','wait:1','decision_applied','{}',$5,$6)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .bind(started + TimeDelta::minutes(1))
    .bind(started + TimeDelta::minutes(6))
    .execute(&pool)
    .await
    .unwrap();

    let page = f.list("").await;
    let item = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.to_string())
        .expect("created conversation is listed");
    assert_eq!(item["working_ms"], json!(300_000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_conversation_ids_are_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let foreign = testkit::seed(&f.pool).await;
    let g = Uuid::now_v7();
    sqlx::query("insert into conversation_groups(id,owner_id,name) values($1,$2,'Чужая')")
        .bind(g)
        .bind(foreign.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
    for (method, suffix, body) in [
        (Method::GET, "", None),
        (Method::PATCH, "", Some(json!({"title":"чужая"}))),
        (Method::PUT, "/groups", Some(json!({"group_ids":[]}))),
        (Method::PUT, "/crunch", Some(json!({"enabled":true}))),
        (Method::POST, "/read", Some(json!({"seq":"100"}))),
        (Method::GET, "/scheduled", None),
        (
            Method::POST,
            "/scheduled",
            Some(
                json!({"run_at":Utc::now()+TimeDelta::hours(1),"content":[{"type":"text","text":"foreign"}]}),
            ),
        ),
    ] {
        let r = f
            .request(
                method,
                &format!("/api/v1/conversations/{}{suffix}", foreign.conversation_id),
                body,
                404,
            )
            .await;
        assert_eq!(r["error"]["code"], "not_found");
    }
    for path in [
        format!("/api/v1/machines/{}", foreign.machine_id),
        format!("/api/v1/machines/{}/workdirs/recent", foreign.machine_id),
        format!(
            "/api/v1/machines/{}/workdirs/suggest?path=/tmp",
            foreign.machine_id
        ),
    ] {
        f.request(Method::GET, &path, None, 404).await;
    }
    for (method, body) in [
        (Method::PATCH, Some(json!({"name":"другая"}))),
        (Method::DELETE, None),
    ] {
        f.request(method, &format!("/api/v1/groups/{g}"), body, 404)
            .await;
    }
    f.request(
        Method::GET,
        &format!("/api/v1/conversations?filter=group:{g}"),
        None,
        404,
    )
    .await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unread_marker_is_monotonic(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    for seq in ["20", "10", "30", "0"] {
        f.request(
            Method::POST,
            &format!("/api/v1/conversations/{id}/read"),
            Some(json!({"seq":seq})),
            204,
        )
        .await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select last_read_seq from conversations where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        30
    );
    f.request(
        Method::POST,
        &format!("/api/v1/conversations/{id}/read"),
        Some(json!({"seq":5})),
        400,
    )
    .await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn read_marker_on_one_device_updates_unread_on_another_via_derived_changed(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"start":true})).await;
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq) select $1,id,$2,'text','final',1,5 from executions where conversation_id=$2").bind(Uuid::now_v7()).bind(id).execute(&f.pool).await.unwrap();
    canterlot_hub::activity::refresh(
        &f.pool,
        id,
        Utc::now(),
        &canterlot_hub::activity::Thresholds::from(&Default::default()),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(f.get(id).await["unread_count"], 1);
    let other = f.client.clone();
    let r = other
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/read"),
            None,
            Some(json!({"seq":"5"})),
        )
        .await;
    assert_eq!(r.status, 204);
    assert_eq!(
        f.events("derived_changed").await.last().unwrap()["unread_count"],
        0
    );
    assert_eq!(f.get(id).await["unread_count"], 0);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sibling_with_common_prefix_is_not_under_the_root(pool: PgPool) {
    let f = Fixture::new(pool).await;
    sqlx::query("update machines set allowed_roots='[\"/home/a\"]' where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    for path in ["/home/ab", "/home/a/../b", "relative", "/home/a/\u{0}x"] {
        let mut b = f.body();
        b["workdir"] = json!(path);
        let r = f
            .request(Method::POST, "/api/v1/conversations", Some(b), 422)
            .await;
        assert_eq!(r["error"]["code"], "workdir_not_allowed");
    }
    f.create(json!({"workdir":"/home/a/sub"})).await;
}

/// Путь из псевдослучайных символов: сжатие индекса его не уменьшает.
fn incompressible_workdir(bytes: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut path = String::from("/tmp");
    while path.len() < bytes {
        let segment = path.len() % 200 == 4;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if segment && path.len() + 1 < bytes {
            path.push('/');
        } else {
            path.push(ALPHABET[(state % ALPHABET.len() as u64) as usize] as char);
        }
    }
    path
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_length_is_capped_with_a_validation_error(pool: PgPool) {
    use canterlot_hub::conversations::MAX_WORKDIR_BYTES;
    let f = Fixture::new(pool).await;
    for bytes in [3000, MAX_WORKDIR_BYTES + 1, 4096] {
        let mut b = f.body();
        b["workdir"] = json!(incompressible_workdir(bytes));
        let r = f
            .request(Method::POST, "/api/v1/conversations", Some(b), 422)
            .await;
        assert_eq!(r["error"]["code"], "workdir_not_allowed");
    }
    let longest = incompressible_workdir(MAX_WORKDIR_BYTES);
    assert_eq!(longest.len(), MAX_WORKDIR_BYTES);
    let id = f.create(json!({"workdir":longest})).await;
    assert_eq!(f.get(id).await["workdir"], longest);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recent_workdirs_shows_last_three_from_remembered_table(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for i in 0..23 {
        f.create(json!({"workdir":format!("/tmp/{i}")})).await;
    }
    // Пересоздание освежает метку: /tmp/4 снова самый свежий.
    let id = f.create(json!({"workdir":"/tmp/4"})).await;
    let r = f
        .request(
            Method::GET,
            &format!("/api/v1/machines/{}/workdirs/recent", f.machine),
            None,
            200,
        )
        .await;
    // Экран новой сессии показывает последние 3, свежие первыми.
    assert_eq!(r["workdirs"], json!(["/tmp/4", "/tmp/22", "/tmp/21"]));
    // Хранится не больше 20: самые старые вытеснены.
    let count: i64 = sqlx::query_scalar(
        "select count(*) from machine_workdirs where owner_id=$1 and machine_id=$2",
    )
    .bind(f.owner)
    .bind(f.machine)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 20);
    let pruned: i64 = sqlx::query_scalar(
        "select count(*) from machine_workdirs where owner_id=$1 and machine_id=$2
         and workdir in ('/tmp/0','/tmp/1','/tmp/2')",
    )
    .bind(f.owner)
    .bind(f.machine)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(pruned, 0);

    // Разговор чужой машины, записанный напрямую, не запоминается.
    let other_machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Другая')")
        .bind(other_machine)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    let other = testkit::seed_conversation(&f.pool, f.owner, other_machine).await;
    sqlx::query("update conversations set workdir='/other-machine' where id=$1")
        .bind(other)
        .execute(&f.pool)
        .await
        .unwrap();
    // Удалённый каталог всё равно в списке: хаб файловую систему воркера не проверяет.
    f.create(json!({"workdir":"/tmp/gone"})).await;
    let r = f
        .request(
            Method::GET,
            &format!("/api/v1/machines/{}/workdirs/recent", f.machine),
            None,
            200,
        )
        .await;
    assert_eq!(r["workdirs"], json!(["/tmp/gone", "/tmp/4", "/tmp/22"]));
    assert!(
        !r["workdirs"]
            .as_array()
            .unwrap()
            .contains(&json!("/other-machine"))
    );

    assert_eq!(f.get(id).await["workdir"], "/tmp/4");
    assert_eq!(f.list("").await["items"][0]["workdir"], "/tmp/0");
}
async fn metrics(f: &Fixture, free: u64, ms: u64) {
    use canterlot_proto::worker::v1::{DiskUsage, MachineMetrics};
    canterlot_hub::machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &MachineMetrics {
            machine_time_ms: ms,
            cpu_cores: 4,
            cpu_permille: 123,
            mem_total_bytes: 10000,
            mem_available_bytes: 5000,
            disks: vec![DiskUsage {
                label: "data".into(),
                total_bytes: 10000,
                free_bytes: free,
            }],
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machines_endpoint_lists_catalog_os_metrics_and_disk_warning(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"title":"Сессия"})).await;
    metrics(&f, 1510, 1).await;
    let path = format!("/api/v1/machines/{}", f.machine);
    let m = f.request(Method::GET, &path, None, 200).await;
    assert_eq!(m["os"], "linux");
    assert_eq!(m["capabilities"], json!(["pilot_v1"]));
    assert_eq!(m["allowed_roots"], json!(["/tmp"]));
    assert_eq!(
        m["harnesses"][0]["models"][0]["efforts"],
        json!(["low", "high"])
    );
    assert!(m.get("profiles").is_none());
    assert_eq!(m["metrics"]["cpu_permille"], 123);
    assert_eq!(m["sessions"][0]["conversation_id"], id.to_string());
    assert_eq!(m["sessions"][0]["title"], "Сессия");
    assert_eq!(m["sessions"][0]["activity_state"], "idle");
    assert_eq!(m["availability"], "available");
    assert_eq!(m["warnings"], json!([]));
    metrics(&f, 1500, 2).await;
    let m = f.request(Method::GET, &path, None, 200).await;
    assert_eq!(m["warnings"], json!(["disk_usage_high"]));
    assert_eq!(
        f.request(Method::GET, "/api/v1/machines", None, 200).await[0]["id"],
        f.machine.to_string()
    );
    assert!(m.get("availability_since").is_some());
    assert!(m.get("heartbeat_age_ms").is_some());
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machines_endpoint_flags_limits_mismatch_and_stale_metrics(pool: PgPool) {
    let f = Fixture::new(pool).await;
    metrics(&f, 2000, 1).await;
    sqlx::query("update machines set limits_ok=false where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update machine_metrics set observed_at=clock_timestamp()-interval '2 minutes'")
        .execute(&f.pool)
        .await
        .unwrap();
    let m = f
        .request(
            Method::GET,
            &format!("/api/v1/machines/{}", f.machine),
            None,
            200,
        )
        .await;
    assert_eq!(m["warnings"], json!(["metrics_stale", "limits_mismatch"]));
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machines_endpoint_shows_a_journal_stop_until_its_cursor_passes(pool: PgPool) {
    let f = Fixture::new(pool).await;
    metrics(&f, 2000, 1).await;
    let (cj, sj) = (Uuid::now_v7(), Uuid::now_v7());
    for (journal, kind) in [(cj, "cj"), (sj, "sj")] {
        sqlx::query(
            "insert into journal_stops(journal_id,machine_id,kind,seq,reason)
             values($1,$2,$3,5,'unknown_record')",
        )
        .bind(journal)
        .bind(f.machine)
        .bind(kind)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let path = format!("/api/v1/machines/{}", f.machine);
    let warnings = |m: Value| m["warnings"].clone();
    assert_eq!(
        warnings(f.request(Method::GET, &path, None, 200).await),
        json!(["cj_journal_stopped", "sj_journal_stopped"])
    );
    // Журнал удалённого разговора больше не придёт: стоять нечему.
    sqlx::query(
        "insert into journal_cursors(machine_id,journal_id,kind,committed_seq,closed,discarded)
         values($1,$2,'sj',1,true,true)",
    )
    .bind(f.machine)
    .bind(sj)
    .execute(&f.pool)
    .await
    .unwrap();
    // Курсор и строка стопа не в одной транзакции: сбой между ними не оставит ложного стопа.
    sqlx::query(
        "insert into journal_cursors(machine_id,journal_id,kind,committed_seq)
         values($1,$2,'cj',5)",
    )
    .bind(f.machine)
    .bind(cj)
    .execute(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        warnings(f.request(Method::GET, &path, None, 200).await),
        json!([])
    );
}
async fn limits_fixture(f: &Fixture) {
    let id = f.create(json!({"start":true})).await;
    sqlx::query("insert into provider_limits(provider,weekly_used_permille,weekly_observed_at,five_hour_used_permille,five_hour_observed_at,weekly_reserve_active) values('claude',954,clock_timestamp(),123,clock_timestamp(),true) on conflict(provider) do update set weekly_used_permille=excluded.weekly_used_permille,weekly_observed_at=excluded.weekly_observed_at,five_hour_used_permille=excluded.five_hour_used_permille,five_hour_observed_at=excluded.five_hour_observed_at,weekly_reserve_active=true").execute(&f.pool).await.unwrap();
    sqlx::query("insert into limit_snapshots(provider,window_kind,used_permille,observed_at,source,machine_id) values('claude','weekly',954,'2026-10-26T22:00:00Z','poll',$1),('claude','five_hour',123,'2026-10-26T22:00:00Z','poll',$1)").bind(f.machine).execute(&f.pool).await.unwrap();
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,provider,started_at,weekly_before_permille,weekly_after_permille) select id,execution_id,conversation_id,'claude','2026-10-26T22:00:00Z',950,954 from commands where conversation_id=$1").bind(id).execute(&f.pool).await.unwrap();
    sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) select input_command_id,'synthetic-small',agent,10,20,0,0 from runs cross join (values('main'),('helper')) a(agent)").execute(&f.pool).await.unwrap();
}
const HISTORY: &str = "/api/v1/limits/history?provider=claude&from=2026-10-26T00%3A00%3A00Z&to=2026-10-28T00%3A00%3A00Z&tz=Europe%2FMoscow";
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_endpoint_returns_current_reserve_flag_and_history_grouped_by_day_session_model(
    pool: PgPool,
) {
    let f = Fixture::new(pool).await;
    limits_fixture(&f).await;
    let current = f.request(Method::GET, "/api/v1/limits", None, 200).await;
    assert_eq!(current[0]["weekly_reserve_active"], true);
    assert_eq!(current[0]["weekly"]["used_permille"], 954);
    assert_eq!(current[0]["weekly"]["used_pct"], 95.4);
    let h = f.request(Method::GET, HISTORY, None, 200).await;
    assert_eq!(h["usage"][0]["day"], "2026-10-27");
    assert_eq!(h["usage"][0]["model"], "synthetic-small");
    assert_eq!(h["usage"][0]["input_tokens"], 10);
    assert_eq!(h["usage"][0]["output_tokens"], 20);
    assert_eq!(
        h["usage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["weekly_delta_permille"].as_i64().unwrap())
            .sum::<i64>(),
        4
    );
    assert!(h["usage"][0]["conversation_id"].is_string());
    f.request(Method::GET, &format!("{HISTORY}bad"), None, 400)
        .await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_history_has_five_hour_series_and_agent_breakdown(pool: PgPool) {
    let f = Fixture::new(pool).await;
    limits_fixture(&f).await;
    let h = f.request(Method::GET, HISTORY, None, 200).await;
    assert_eq!(h["series"][0]["five_hour"][0]["used_permille"], 123);
    assert_eq!(h["series"][0]["weekly"][0]["used_permille"], 954);
    let mut agents: Vec<_> = h["usage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["agent"].as_str().unwrap())
        .collect();
    agents.sort();
    assert_eq!(agents, vec!["helper", "main"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn forced_archive_skips_claimed_jobs_and_rolls_back_every_effect(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({"crunch":{"enabled":true}})).await;
    let message=f.request(Method::POST,&format!("/api/v1/conversations/{id}/scheduled"),Some(json!({"run_at":Utc::now()+TimeDelta::hours(1),"content":[{"type":"text","text":"scheduled"}]})),201).await;
    let job: Uuid = message["id"].as_str().unwrap().parse().unwrap();
    let mut claim = f.pool.begin().await.unwrap();
    sqlx::query("select id from scheduled_jobs where id=$1 for update")
        .bind(job)
        .fetch_one(&mut *claim)
        .await
        .unwrap();
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        f.client.send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}"),
            Some("archive-locked"),
            Some(json!({"archived":true,"force":true})),
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 409);
    assert_eq!(
        r.json::<Value>()["error"]["details"]["reasons"],
        json!(["scheduled"])
    );
    let d = f.get(id).await;
    assert_eq!(d["archived"], false);
    assert_eq!(d["crunch"]["enabled"], true);
    claim.commit().await.unwrap();
    let r = f
        .client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}"),
            Some("archive-locked"),
            Some(json!({"archived":true,"force":true})),
        )
        .await;
    assert_eq!(r.status, 200);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn all_task6a_mutations_replay_bytes_and_record_exactly_one_key(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    let group = f.group("Группа").await;
    for (method, path, body, conflict) in [
        (
            Method::PATCH,
            format!("/api/v1/conversations/{id}"),
            Some(json!({"color":null})),
            Some(json!({"color":"red"})),
        ),
        (
            Method::PUT,
            format!("/api/v1/conversations/{id}/groups"),
            Some(json!({"group_ids":[group]})),
            Some(json!({"group_ids":[]})),
        ),
        (
            Method::PUT,
            format!("/api/v1/conversations/{id}/crunch"),
            Some(json!({"enabled":true})),
            Some(json!({"enabled":false})),
        ),
        (
            Method::POST,
            "/api/v1/groups".into(),
            Some(json!({"name":"Новая"})),
            Some(json!({"name":"Ещё"})),
        ),
        (
            Method::PATCH,
            format!("/api/v1/groups/{group}"),
            Some(json!({"position":3})),
            Some(json!({"position":4})),
        ),
        (
            Method::DELETE,
            format!("/api/v1/groups/{group}"),
            None,
            None,
        ),
    ] {
        let key = Uuid::now_v7().to_string();
        let a = f
            .client
            .send(method.clone(), &path, Some(&key), body.clone())
            .await;
        assert!(a.status.is_success());
        let b = f.client.send(method.clone(), &path, Some(&key), body).await;
        assert_eq!(a.status, b.status);
        assert_eq!(a.body, b.body);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("select count(*) from idempotency_keys where key=$1")
                .bind(&key)
                .fetch_one(&f.pool)
                .await
                .unwrap(),
            1
        );
        if let Some(body) = conflict {
            let r = f.client.send(method, &path, Some(&key), Some(body)).await;
            assert_eq!(r.status, 409);
            assert_eq!(r.json::<Value>()["error"]["code"], "idempotency_conflict");
        }
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn projector_materialises_settings_and_crunch_without_attribute_event(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    f.request(
        Method::PUT,
        &format!("/api/v1/conversations/{id}/crunch"),
        Some(json!({"enabled":true})),
        200,
    )
    .await;
    canterlot_hub::activity::refresh(
        &f.pool,
        id,
        Utc::now(),
        &canterlot_hub::activity::Thresholds::from(&Default::default()),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let p = f.events("derived_changed").await.pop().unwrap();
    assert_eq!(
        p["settings"],
        json!({"model":"synthetic-small","effort":"high","permission_mode":"bypass"})
    );
    assert_eq!(p["crunch"]["enabled"], true);
    assert!(f.events("conversation_updated").await.is_empty());
    assert_eq!(f.list("").await["items"][0]["crunch"]["enabled"], true);
    assert_eq!(
        f.get(id).await["crunch"]["resume_text"],
        canterlot_hub::config::LimitsConfig::default().default_resume_text
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn missing_derived_rows_remain_listable_and_refresh_upserts(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    assert_eq!(f.get(id).await["activity"]["state"], "idle");
    assert_eq!(f.list("").await["items"][0]["id"], id.to_string());
    canterlot_hub::activity::refresh(
        &f.pool,
        id,
        Utc::now(),
        &canterlot_hub::activity::Thresholds::from(&Default::default()),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from conversation_derived where conversation_id=$1"
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        1
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_in_tx_preserves_resume_native_id_and_outbox_settings(pool: PgPool) {
    use canterlot_hub::{executions, outbox};
    use canterlot_proto::worker::v1::command;
    let f = Fixture::new(pool).await;
    let id = f
        .create(json!({"start":true,"permission_mode":"bypass"}))
        .await;
    let row = sqlx::query_as::<_, outbox::CommandRow>(
        "select * from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let message = outbox::to_proto(&row).unwrap();
    let Some(command::Kind::StartExecution(start)) = message.kind else {
        panic!("start")
    };
    assert_eq!(start.model, "synthetic-small");
    assert_eq!(start.effort, "high");
    assert_eq!(start.permission_mode, "bypass");
    let instructions = start
        .instructions
        .expect("start carries instructions snapshot");
    assert_eq!(instructions.mode, "clean");
    assert!(instructions.text.is_empty());
    assert!(start.capabilities.is_empty());
    sqlx::query("update executions set state='stopped',native_conversation_id='native-test' where conversation_id=$1").bind(id).execute(&f.pool).await.unwrap();
    testkit::seed_recorded_turn(&f.pool, id).await;
    let mut tx = f.pool.begin().await.unwrap();
    let started = executions::start_in_tx(&mut tx, f.owner, id, Uuid::now_v7())
        .await
        .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(started.command_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(payload["native_conversation_id"], "native-test");
    assert_eq!(started.generation, 2);
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from executions where conversation_id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_api_exposes_the_reported_home_and_an_old_worker_leaves_it_empty(pool: PgPool) {
    use canterlot_proto::worker::v1::MachineReport;
    let f = Fixture::new(pool).await;
    let path = format!("/api/v1/machines/{}", f.machine);
    assert_eq!(
        f.request(Method::GET, &path, None, 200).await["home_dir"],
        Value::Null
    );
    let mut report = MachineReport {
        home_dir: "/home/owner".into(),
        ..Default::default()
    };
    canterlot_hub::machine_info::apply_report(&f.pool, f.owner, f.machine, &report, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        f.request(Method::GET, &path, None, 200).await["home_dir"],
        "/home/owner"
    );
    let published: Value = sqlx::query_scalar(
        "select payload from events where kind='machine_report_updated' order by scope_seq desc limit 1",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(published["home_dir"], "/home/owner");
    report.home_dir.clear();
    canterlot_hub::machine_info::apply_report(&f.pool, f.owner, f.machine, &report, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        f.request(Method::GET, &path, None, 200).await["home_dir"],
        Value::Null
    );
}

fn suggested(session: &mut testkit::FakeSession) -> Option<SuggestWorkdirs> {
    match session.commands.try_recv().ok()?.msg {
        Some(control_down::Msg::SuggestWorkdirs(request)) => Some(request),
        other => panic!("{other:?}"),
    }
}

fn answer(machine: Uuid, directories: &[&str], request: SuggestWorkdirs, registry: &Registry) {
    registry.finish_suggest(
        machine,
        WorkdirSuggestions {
            request_id: request.request_id,
            directories: directories.iter().map(|d| (*d).to_owned()).collect(),
        },
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_suggestions_come_from_the_live_worker(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    f._leases.clear();
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_WORKDIR_SUGGEST_V1],
        true,
    );
    let (registry, machine) = (f.registry.clone(), f.machine);
    let worker = tokio::spawn(async move {
        let down = session.commands.recv().await.unwrap();
        let Some(control_down::Msg::SuggestWorkdirs(request)) = down.msg else {
            panic!("{down:?}")
        };
        assert_eq!(request.path, "/srv/can ary");
        answer(machine, &["/srv/can ary-one"], request, &registry);
        session
    });
    let body = f
        .request(
            Method::GET,
            &format!(
                "/api/v1/machines/{}/workdirs/suggest?path=%2Fsrv%2Fcan%20ary",
                f.machine
            ),
            None,
            200,
        )
        .await;
    assert_eq!(body, json!({"workdirs":["/srv/can ary-one"]}));
    worker.await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_suggestions_are_empty_without_a_capable_live_worker(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let path = format!("/api/v1/machines/{}/workdirs/suggest?path=/tmp/", f.machine);
    f._leases.clear();
    assert_eq!(
        f.request(Method::GET, &path, None, 200).await,
        json!({"workdirs":[]}),
        "offline machine"
    );
    let mut old = testkit::fake_session(&f.registry, f.machine, &["pilot_v1"], true);
    assert_eq!(
        f.request(Method::GET, &path, None, 200).await,
        json!({"workdirs":[]}),
        "worker without the capability"
    );
    assert!(suggested(&mut old).is_none(), "old worker gets no request");
    drop(old);
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_WORKDIR_SUGGEST_V1],
        true,
    );
    let long = "a".repeat(canterlot_proto::limits::MAX_WORKDIR_BYTES);
    assert_eq!(
        f.request(
            Method::GET,
            &format!(
                "/api/v1/machines/{}/workdirs/suggest?path=/{long}",
                f.machine
            ),
            None,
            200
        )
        .await,
        json!({"workdirs":[]}),
        "oversized path"
    );
    assert!(
        suggested(&mut session).is_none(),
        "oversized path stays here"
    );
    let missing = f
        .request(
            Method::GET,
            &format!("/api/v1/machines/{}/workdirs/suggest", f.machine),
            None,
            400,
        )
        .await;
    assert_eq!(missing["error"]["code"], "invalid_request");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn suggestion_wait_is_bounded_and_answers_match_the_asking_machine(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    f._leases.clear();
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_WORKDIR_SUGGEST_V1],
        true,
    );
    let wait = Duration::from_millis(40);
    assert!(
        f.registry
            .suggest_workdirs(f.machine, "/tmp/", wait)
            .await
            .is_empty(),
        "silent worker"
    );
    let late = suggested(&mut session).unwrap();
    answer(f.machine, &["/tmp/late"], late, &f.registry);

    let registry = f.registry.clone();
    let machine = f.machine;
    let asking =
        tokio::spawn(async move { registry.suggest_workdirs(machine, "/tmp/", wait).await });
    let request = session.commands.recv().await.unwrap();
    let Some(control_down::Msg::SuggestWorkdirs(request)) = request.msg else {
        panic!("{request:?}")
    };
    answer(Uuid::now_v7(), &["/tmp/foreign"], request, &f.registry);
    assert!(asking.await.unwrap().is_empty(), "foreign machine ignored");

    let many: Vec<_> = (0..MAX_SUGGESTS_PER_MACHINE)
        .map(|_| {
            let registry = f.registry.clone();
            let machine = f.machine;
            tokio::spawn(async move {
                registry
                    .suggest_workdirs(machine, "/tmp/", Duration::from_secs(30))
                    .await
            })
        })
        .collect();
    let mut requests = Vec::new();
    for _ in 0..MAX_SUGGESTS_PER_MACHINE {
        let down = session.commands.recv().await.unwrap();
        let Some(control_down::Msg::SuggestWorkdirs(request)) = down.msg else {
            panic!("{down:?}")
        };
        requests.push(request);
    }
    let over_bound = f
        .registry
        .suggest_workdirs(f.machine, "/tmp/", Duration::from_secs(30));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), over_bound)
            .await
            .unwrap()
            .is_empty(),
        "over the bound nothing waits"
    );
    for request in requests {
        answer(f.machine, &["/tmp/found"], request, &f.registry);
    }
    for task in many {
        assert_eq!(task.await.unwrap(), ["/tmp/found"]);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn catalog_changes_publish_new_harness_attributes_in_both_conversation_scopes(pool: PgPool) {
    use canterlot_proto::worker::v1::{HarnessInfo, MachineReport, ModelInfo};
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    let report = MachineReport {
        worker_version: "updated".into(),
        allowed_roots: vec!["/tmp".into()],
        os: "linux".into(),
        max_attachment_bytes: canterlot_proto::limits::MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: canterlot_proto::limits::MAX_ATTACHMENTS_PER_INPUT as u32,
        harnesses: vec![HarnessInfo {
            harness: "synthetic".into(),
            provider: "claude".into(),
            title: "Новое имя".into(),
            models: vec![ModelInfo {
                id: "synthetic-small".into(),
                title: "Модель".into(),
                efforts: vec!["high".into()],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    canterlot_hub::machine_info::apply_report(&f.pool, f.owner, f.machine, &report, Uuid::now_v7())
        .await
        .unwrap();
    let events = f.events("conversation_updated").await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["harness"]["title"], "Новое имя");
    assert_eq!(events[0]["counts"]["all"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "select count(*) from events where scope=$1 and kind='conversation_updated'"
        )
        .bind(format!("conversation:{id}"))
        .fetch_one(&f.pool)
        .await
        .unwrap(),
        1
    );
}

// Картинки и окно контекста у OpenCode свои у каждой модели: отчёт доходит до формы как есть.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_report_keeps_the_per_model_facts(pool: PgPool) {
    use canterlot_proto::worker::v1::{HarnessInfo, MachineReport, ModelInfo};
    let f = Fixture::new(pool).await;
    let report = MachineReport {
        worker_version: "updated".into(),
        allowed_roots: vec!["/tmp".into()],
        os: "linux".into(),
        max_attachment_bytes: canterlot_proto::limits::MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: canterlot_proto::limits::MAX_ATTACHMENTS_PER_INPUT as u32,
        harnesses: vec![HarnessInfo {
            harness: "opencode".into(),
            provider: "opencode".into(),
            title: "OpenCode".into(),
            images: true,
            models: vec![
                ModelInfo {
                    id: "opencode-go/text".into(),
                    title: "Текст".into(),
                    efforts: vec!["default".into()],
                    images: Some(false),
                    context_window: Some(262_144),
                },
                ModelInfo {
                    id: "opencode-go/old".into(),
                    title: "Без сведений".into(),
                    efforts: vec!["default".into()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }],
        ..Default::default()
    };
    canterlot_hub::machine_info::apply_report(&f.pool, f.owner, f.machine, &report, Uuid::now_v7())
        .await
        .unwrap();
    let path = format!("/api/v1/machines/{}", f.machine);
    let machine = f.request(Method::GET, &path, None, 200).await;
    let models = &machine["harnesses"][0]["models"];
    assert_eq!(models[0]["images"], false);
    assert_eq!(models[0]["context_window"], 262_144);
    assert!(models[1]["images"].is_null());
    assert!(models[1]["context_window"].is_null());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_defaults_to_clean_preset(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.create(json!({})).await;
    let detail = f.get(id).await;
    assert_eq!(
        detail["settings"]["effective"]["instruction_profile"],
        "clean"
    );
    let preset = &detail["settings"]["effective"]["instruction_preset"];
    assert_eq!(preset["id"], "clean");
    assert_eq!(preset["kind"], "clean");
    assert_eq!(preset["deleted"], false);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_with_custom_preset_pins_current_revision(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    f.caps(&["pilot_v1", "instructions_v1"]);
    let preset = f.create_preset("Исполнитель", "текст A").await;
    let id = f.create(json!({"instruction_profile": preset["id"]})).await;
    let detail = f.get(id).await;
    assert_eq!(
        detail["settings"]["effective"]["instruction_profile"],
        preset["id"]
    );
    let applied = &detail["settings"]["effective"]["instruction_preset"];
    assert_eq!(applied["id"], preset["id"]);
    assert_eq!(applied["kind"], "custom");
    assert_eq!(applied["revision"], 1);
    assert_eq!(applied["current_revision"], 1);
    assert_eq!(applied["deleted"], false);
    // Повторное применение того же пресета подтягивает новую версию только
    // через явную смену; закреплённая версия не меняется сама.
    f.patch_settings(id, json!({"instruction_profile": preset["id"]}))
        .await;
    let detail = f.get(id).await;
    assert_eq!(
        detail["settings"]["effective"]["instruction_preset"]["revision"],
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_with_deleted_or_foreign_preset_is_profile_unknown(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    f.caps(&["pilot_v1", "instructions_v1"]);
    let preset = f.create_preset("Временный", "текст").await;
    let id: String = preset["id"].as_str().unwrap().into();
    let gone = f
        .request(Method::DELETE, &format!("/api/v1/presets/{id}"), None, 204)
        .await;
    assert!(gone.is_null());
    for reference in [id.as_str(), &Uuid::now_v7().to_string()] {
        let r = f
            .client
            .send(
                Method::POST,
                "/api/v1/conversations",
                Some(&Uuid::now_v7().to_string()),
                Some(f.body_with_profile(reference)),
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            422,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.json::<Value>()["error"]["code"], "profile_unknown");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn non_owner_preset_needs_instructions_v1(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    let preset = f.create_preset("Исполнитель", "текст").await;
    let id: String = preset["id"].as_str().unwrap().into();
    // Без capability сессия со стартом отклоняется.
    f.caps(&["pilot_v1"]);
    let mut body = f.body_with_profile(&id);
    body["start"] = json!(true);
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some(&Uuid::now_v7().to_string()),
            Some(body),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        409,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json::<Value>()["error"]["code"], "capability_missing");
    // С capability — создаётся и стартует.
    f.caps(&["pilot_v1", "instructions_v1"]);
    let mut body = f.body_with_profile(&id);
    body["start"] = json!(true);
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some(&Uuid::now_v7().to_string()),
            Some(body),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_and_resume_check_instructions_v1_and_disconnect_is_unavailable(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    // Машина без сеанса: это недоступность, а не старый воркер.
    f._leases.clear();
    let mut body = f.body();
    body["start"] = json!(true);
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some(&Uuid::now_v7().to_string()),
            Some(body),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        409,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json::<Value>()["error"]["code"], "machine_unavailable");
    // Сеанс без instructions_v1: запуск и возобновление сессии «Без инструкций» отклоняются.
    f.caps(&["pilot_v1"]);
    let id = f.create(json!({})).await;
    for (path, body) in [("executions", None), ("resume", Some(json!({})))] {
        let r = f
            .client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{id}/{path}"),
                Some(&Uuid::now_v7().to_string()),
                body,
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            409,
            "{path}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.json::<Value>()["error"]["code"], "capability_missing");
    }
    f.caps(&["pilot_v1", "instructions_v1"]);
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/executions"),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_switching_preset_on_old_worker_shows_capability_hold(pool: PgPool) {
    let mut f = Fixture::new(pool).await;
    f.caps(&["pilot_v1"]);
    let id = f.create(json!({"instruction_profile": "owner"})).await;
    sqlx::query("update conversations set pending_settings='{\"instruction_profile\":\"clean\"}' where id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    let input = f
        .request(
            Method::POST,
            &format!("/api/v1/conversations/{id}/inputs"),
            Some(json!({"content":[{"type":"text","text":"дальше"}]})),
            201,
        )
        .await;
    let snapshot = f
        .request(
            Method::GET,
            &format!("/api/v1/conversations/{id}/snapshot"),
            None,
            200,
        )
        .await;
    assert!(
        snapshot["data"]["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!({"reason":"capability_missing","input_id":input["input_id"]})),
        "{}",
        snapshot["data"]["queue"]
    );
}
