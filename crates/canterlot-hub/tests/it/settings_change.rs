use std::sync::Arc;

use axum::http::Method;
use canterlot_hub::{
    activity::Thresholds,
    config::{LimitsConfig, SessionToolsConfig},
    http::{self, AppState},
    scheduler::{ClaimedJob, JobKind},
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit::{self, TestClient},
};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
    router: axum::Router,
    client: TestClient,
    agent_client: TestClient,
    owner: Uuid,
    conversation: Uuid,
    execution: Uuid,
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
        let conversation = testkit::seed_conversation(&pool, owner, machine).await;
        let execution = Uuid::now_v7();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
        )
        .bind(execution)
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
        let router = http::router(AppState::new(deps));
        let context = ToolContext {
            memory: Default::default(),
            pool: pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            limits: Arc::new(LimitsConfig::default()),
            settle: SettleConfig {
                idle_quiet: std::time::Duration::from_secs(5),
                idle_background_max: std::time::Duration::from_secs(900),
                nudge_enabled: true,
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: std::time::Duration::from_secs(1800),
            routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: None,
            registry: canterlot_hub::session::Registry::new(Default::default()),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };
        Self {
            pool,
            context,
            router: router.clone(),
            client: TestClient::in_process(router.clone()).as_owner(),
            agent_client: TestClient::in_process(router)
                .with_header("Origin", testkit::TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "1"),
            owner,
            conversation,
            execution,
            _dir: dir,
        }
    }

    fn router(&self) -> axum::Router {
        self.router.clone()
    }

    fn spawn_job(&self, action: &str, request: Uuid) -> tokio::task::JoinHandle<bool> {
        let pool = self.pool.clone();
        let context = self.context.clone();
        let job = ClaimedJob {
            id: Uuid::now_v7(),
            kind: JobKind::SessionTask,
            owner_id: self.owner,
            conversation_id: Some(self.conversation),
            run_at: Utc::now(),
            payload: json!({"action": action, "request_id": request}),
            attempts: 0,
        };
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let outcome = session_tools::jobs::run_in_tx(
                &mut tx,
                &job,
                Utc::now(),
                Uuid::now_v7(),
                None,
                &context.settle,
                &Thresholds::from(&canterlot_hub::config::ActivityConfig::default()),
                None,
                None,
                &LimitsConfig::default(),
                std::time::Duration::from_secs(60),
            )
            .await;
            let ok = outcome.is_ok();
            if ok {
                tx.commit().await.unwrap();
            }
            ok
        })
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, session_tools::ToolError> {
        let caller = Caller {
            owner_id: self.owner,
            conversation_id: self.conversation,
            execution_id: self.execution,
            machine_id: Uuid::now_v7(),
            call_id: Uuid::now_v7(),
        };
        session_tools::call(&self.context, &caller, tool, args).await
    }

    async fn proposal(&self, tool: &str, args: Value) -> (Uuid, Value) {
        let answer = self.call(tool, args).await.unwrap_or_else(|error| {
            panic!(
                "{tool} refused: {} / {}",
                error.code.as_str(),
                error.message
            )
        });
        assert_eq!(answer["status"], "pending_approval", "{answer}");
        let request: Uuid = serde_json::from_value(answer["request_id"].clone()).unwrap();
        let payload: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
            .bind(request)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        (request, payload)
    }

    async fn decide(&self, request: Uuid, behavior: &str) -> u16 {
        let response = self
            .client
            .send(
                Method::POST,
                &format!(
                    "/api/v1/conversations/{}/requests/{request}/decision",
                    self.conversation
                ),
                Some(&format!("decision-{request}")),
                Some(json!({"kind":"permission","behavior":behavior,"message":null})),
            )
            .await;
        response.status.into()
    }

    async fn request_state(&self, request: Uuid) -> String {
        sqlx::query_scalar("select state from requests where id=$1")
            .bind(request)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Перед запуском задания нужен «занятый» its own job; scheduler в тесте не крутится.
    async fn run_job_by_action(&self, action: &str, now: chrono::DateTime<Utc>) {
        let row: Option<(Uuid, Value)> = sqlx::query_as(
            "select id,payload from scheduled_jobs where kind='session_task' and state='pending'
               and payload->>'action'=$1 order by id limit 1",
        )
        .bind(action)
        .fetch_optional(&self.pool)
        .await
        .unwrap();
        let Some((id, payload)) = row else {
            panic!("no pending {action} job");
        };
        let job = ClaimedJob {
            id,
            kind: JobKind::SessionTask,
            owner_id: self.owner,
            conversation_id: Some(self.conversation),
            run_at: now,
            payload,
            attempts: 0,
        };
        let mut tx = self.pool.begin().await.unwrap();
        let outcome = session_tools::jobs::run_in_tx(
            &mut tx,
            &job,
            now,
            Uuid::now_v7(),
            None,
            &self.context.settle,
            &Thresholds::from(&canterlot_hub::config::ActivityConfig::default()),
            None,
            None,
            &LimitsConfig::default(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
        let _ = outcome;
        tx.commit().await.unwrap();
    }

    async fn notes(&self) -> Vec<String> {
        sqlx::query_scalar(
            "select content::text from inputs where owner_id=$1 and content::text like '%[canterlot]%'",
        )
        .bind(self.owner)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn new_preset(&self) -> Uuid {
        let id = Uuid::now_v7();
        let mut tx = self.pool.begin().await.unwrap();
        sqlx::query(
            "insert into instruction_presets(id,owner_id,title,description,revision,origin,purpose)
             values($1,$2,'Проект','',$3,'agent','instructions')",
        )
        .bind(id)
        .bind(self.owner)
        .bind(1i64)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into instruction_preset_revisions(preset_id,revision,text,edited_by)
             values($1,1,'старый текст','agent')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }
}

/// Ждём, пока фоновые транзакции действительно встанут в очередь за блокировкой,
/// не пользуясь временем: тест не должен зависеть от скорости машины.
async fn wait_for_lock_waiters(pool: &PgPool, count: i64) {
    for _ in 0..20_000 {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity
              where datname=current_database() and wait_event_type='Lock' and state='active'
                and query not ilike '%pg_stat_activity%'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{count} lock waiters never appeared");
}

fn routing() -> Value {
    json!({"task_types":{"review":{"title":"Проверка","candidates":[
        {"harness":"claude_code","model":"sonnet[1m]"}],"expected_minutes":30}}})
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routing_proposal_allow_applies_with_events_and_note(pool: PgPool) {
    let f = Fx::new(pool).await;
    let before_revision: i64 =
        sqlx::query_scalar("select routing_policy_revision from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let (request, payload) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "нужна проверка", "base_revision": before_revision}),
        )
        .await;
    assert_eq!(payload["setting"], "routing");
    assert_eq!(payload["reason"], "нужна проверка");
    assert_eq!(payload["base_revision"], before_revision);
    let proposed: canterlot_hub::session_tools::routing::Policy =
        serde_json::from_value(routing()).unwrap();
    assert_eq!(payload["after"], serde_json::to_value(proposed).unwrap());
    assert!(
        payload.get("before").is_some(),
        "card needs the old value: {payload}"
    );
    let kind: String = sqlx::query_scalar("select kind from requests where id=$1")
        .bind(request)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(kind, "settings_change");
    // Карточка видна владельцу в снимке и в ленте.
    let opened: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id=$1 and kind='request_opened'
           and payload->>'request_id'=$2",
    )
    .bind(f.conversation)
    .bind(request.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(opened, 1);

    assert_eq!(f.decide(request, "allow").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;

    assert_eq!(f.request_state(request).await, "decision_applied");
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let expected: canterlot_hub::session_tools::routing::Policy =
        serde_json::from_value(routing()).unwrap();
    assert_eq!(stored, Some(serde_json::to_value(expected).unwrap()));
    let revision: i64 =
        sqlx::query_scalar("select routing_policy_revision from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(revision, before_revision + 1);
    for (event, owner_scope) in [
        ("request_decided", false),
        ("routing_policy_changed", true),
        ("request_closed", false),
    ] {
        let count: i64 = if owner_scope {
            sqlx::query_scalar("select count(*) from events where kind=$1")
                .bind(event)
                .fetch_one(&f.pool)
                .await
                .unwrap()
        } else {
            sqlx::query_scalar("select count(*) from events where conversation_id=$1 and kind=$2")
                .bind(f.conversation)
                .bind(event)
                .fetch_one(&f.pool)
                .await
                .unwrap()
        };
        assert_eq!(count, 1, "event {event}");
    }
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("allowed"), "{}", notes[0]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn whitelist_proposal_allow_applies_with_revision_and_event(pool: PgPool) {
    let f = Fx::new(pool).await;
    let revision: i64 =
        sqlx::query_scalar("select spawn_whitelist_revision from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let entries = json!([{"harness":"claude_code","model":"haiku"}]);
    let (request, payload) = f
        .proposal(
            "settings.propose_whitelist",
            json!({"entries": entries, "reason": "только haiku", "base_revision": revision}),
        )
        .await;
    assert_eq!(payload["setting"], "whitelist");
    assert_eq!(payload["after"], entries);
    assert_eq!(
        f.decide(request, "allow").await,
        200,
        "owner-authenticated decision"
    );
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "decision_applied");
    let stored: Option<Value> =
        sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(stored, Some(entries));
    let next: i64 = sqlx::query_scalar("select spawn_whitelist_revision from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(next, revision + 1);
    let count: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='spawn_whitelist_changed' and payload->>'revision'=$1",
    )
    .bind((revision + 1).to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn preset_proposal_allow_replaces_text_and_bumps_revision(pool: PgPool) {
    let f = Fx::new(pool).await;
    let preset = f.new_preset().await;
    let (request, payload) = f
        .proposal(
            "settings.propose_preset",
            json!({"id": preset.to_string(), "text": "новый текст", "reason": "уточнить", "base_revision": 1}),
        )
        .await;
    assert_eq!(payload["setting"], "preset");
    assert_eq!(payload["before"], "старый текст");
    assert_eq!(payload["after"], "новый текст");
    assert_eq!(payload["preset_id"], preset.to_string());
    assert_eq!(f.decide(request, "allow").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "decision_applied");
    let (revision, text): (i64, String) = sqlx::query_as(
        "select p.revision,r.text from instruction_presets p
           join instruction_preset_revisions r on r.preset_id=p.id and r.revision=p.revision
          where p.id=$1",
    )
    .bind(preset)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(revision, 2);
    assert_eq!(text, "новый текст");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='instruction_preset_changed'
           and payload->'preset'->>'id'=$1",
    )
    .bind(preset.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_base_revision_closes_as_setting_changed_and_applies_nothing(pool: PgPool) {
    let f = Fx::new(pool).await;
    let revision: i64 =
        sqlx::query_scalar("select routing_policy_revision from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "устарело", "base_revision": revision}),
        )
        .await;
    // Владелец правит настройку, пока карточка открыта.
    let before: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    f.client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("owner-edit"),
            Some(json!({"policy": null, "revision": revision})),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "setting_changed");
    let after: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "the stale proposal must not overwrite the owner edit"
    );
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("setting changed"), "{}", notes[0]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decline_closes_as_declined_and_applies_nothing(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_whitelist",
            json!({"entries": [], "reason": "пусто", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "deny").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "declined");
    let stored: Option<Value> =
        sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
            .bind(f.owner)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(stored, None);
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("declined"), "{}", notes[0]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn open_proposal_expires_after_twenty_four_hours(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "позже", "base_revision": 0}),
        )
        .await;
    let run_at: chrono::DateTime<Utc> = sqlx::query_scalar(
        "select run_at from scheduled_jobs where payload->>'action'='settings_expire'
           and payload->>'request_id'=$1",
    )
    .bind(request.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let created_at: chrono::DateTime<Utc> =
        sqlx::query_scalar("select created_at from requests where id=$1")
            .bind(request)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!((run_at - created_at - Duration::hours(24)).abs() < Duration::minutes(1));
    f.run_job_by_action("settings_expire", created_at + Duration::hours(25))
        .await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "expired");
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, None);
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("expired"), "{}", notes[0]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn validation_refusal_uses_the_settings_endpoint_rules(pool: PgPool) {
    let f = Fx::new(pool).await;
    let invalid_routing = json!({"task_types":{"BAD":{"title":"x","candidates":[]}}});
    let error = f
        .call(
            "settings.propose_routing",
            json!({"policy": invalid_routing, "reason": "x", "base_revision": 0}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
    let http = f
        .client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("invalid-routing"),
            Some(json!({"policy": invalid_routing, "revision": 0})),
        )
        .await;
    assert_eq!(http.status, 422, "{}", String::from_utf8_lossy(&http.body));

    let duplicates = json!([
        {"harness":"claude_code","model":"haiku"},
        {"harness":"claude_code","model":"haiku"}
    ]);
    let error = f
        .call(
            "settings.propose_whitelist",
            json!({"entries": duplicates, "reason": "x", "base_revision": 0}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
    let http = f
        .client
        .send(
            Method::PUT,
            "/api/v1/spawn-whitelist",
            Some("invalid-whitelist"),
            Some(json!({"entries": duplicates})),
        )
        .await;
    assert_eq!(http.status, 422, "{}", String::from_utf8_lossy(&http.body));

    let preset = f.new_preset().await;
    let error = f
        .call(
            "settings.propose_preset",
            json!({"id": preset.to_string(), "text": "   ", "reason": "x", "base_revision": 1}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn agent_cannot_decide_a_settings_proposal(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "x", "base_revision": 0}),
        )
        .await;
    let response = f
        .agent_client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/requests/{request}/decision",
                f.conversation
            ),
            Some("agent-decision"),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
        )
        .await;
    assert_ne!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(f.request_state(request).await, "open");
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn identical_value_is_refused_at_proposal_time(pool: PgPool) {
    let f = Fx::new(pool).await;
    for (tool, args) in [
        (
            "settings.propose_routing",
            json!({"policy": null, "reason": "x", "base_revision": 0}),
        ),
        (
            "settings.propose_whitelist",
            json!({"entries": null, "reason": "x", "base_revision": 0}),
        ),
    ] {
        let error = f.call(tool, args).await.unwrap_err();
        assert_eq!(
            error.code.as_str(),
            "invalid_arguments",
            "{tool}: {error:?}"
        );
        assert!(
            error.message.contains("identical"),
            "{tool}: {}",
            error.message
        );
    }
    let preset = f.new_preset().await;
    let error = f
        .call(
            "settings.propose_preset",
            json!({"id": preset.to_string(), "text": "старый текст", "reason": "x", "base_revision": 1}),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("identical"), "{}", error.message);
    let requests: i64 =
        sqlx::query_scalar("select count(*) from requests where kind='settings_change'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(requests, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn outcome_note_is_delivered_once_when_the_job_is_replayed(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "один раз", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    let notes = f.notes().await;
    assert_eq!(
        notes.len(),
        1,
        "a replayed durable job must not notify twice: {notes:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approval_job_takes_the_queue_before_the_request_row(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "порядок", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.conversation)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: f.owner,
        conversation_id: Some(f.conversation),
        run_at: Utc::now(),
        payload: json!({"action": "settings_change", "request_id": request}),
        attempts: 0,
    };
    let task = {
        let pool = f.pool.clone();
        let context = ToolContext {
            memory: Default::default(),
            pool: pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            limits: Arc::new(LimitsConfig::default()),
            settle: f.context.settle,
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: std::time::Duration::from_secs(1800),
            routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: None,
            registry: f.context.registry.clone(),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let outcome = session_tools::jobs::run_in_tx(
                &mut tx,
                &job,
                Utc::now(),
                Uuid::now_v7(),
                None,
                &context.settle,
                &Thresholds::from(&canterlot_hub::config::ActivityConfig::default()),
                None,
                None,
                &LimitsConfig::default(),
                std::time::Duration::from_secs(60),
            )
            .await;
            if outcome.is_ok() {
                tx.commit().await.unwrap();
            }
            outcome.is_ok()
        })
    };
    wait_for_lock_waiters(&f.pool, 1).await;
    let unlocked: Option<Uuid> =
        sqlx::query_scalar("select id from requests where id=$1 for update skip locked")
            .bind(request)
            .fetch_optional(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        unlocked,
        Some(request),
        "the job must wait on the queue, not hold the request row"
    );
    blocker.rollback().await.unwrap();
    assert!(task.await.unwrap());
    assert_eq!(f.request_state(request).await, "decision_applied");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_decisions_with_different_keys_do_not_deadlock(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "гонка", "base_revision": 0}),
        )
        .await;
    let decide = |key: &'static str| {
        let client = f.client.clone();
        let path = format!(
            "/api/v1/conversations/{}/requests/{request}/decision",
            f.conversation
        );
        async move {
            client
                .send(
                    Method::POST,
                    &path,
                    Some(key),
                    Some(json!({"kind":"permission","behavior":"allow","message":null})),
                )
                .await
                .status
        }
    };
    let (first, second) = tokio::join!(decide("race-one"), decide("race-two"));
    let statuses = [u16::from(first), u16::from(second)];
    assert_eq!(
        statuses.iter().filter(|status| **status == 200).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|status| **status == 409).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(f.request_state(request).await, "decision_saved");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn proposal_survives_execution_rollover(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "после хода", "base_revision": 0}),
        )
        .await;
    sqlx::query("update executions set state='stopped' where id=$1")
        .bind(f.execution)
        .execute(&f.pool)
        .await
        .unwrap();
    canterlot_hub::executions::start(&f.pool, f.owner, f.conversation, Uuid::now_v7())
        .await
        .unwrap();
    let snapshot = canterlot_hub::snapshot::conversation(&f.pool, f.owner, f.conversation, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(
        snapshot
            .data
            .open_requests
            .iter()
            .any(|item| item.id == request),
        "the proposal is conversation-scoped and stays in the snapshot"
    );
    let mut conn = f.pool.acquire().await.unwrap();
    let page = canterlot_hub::history::page(
        &mut conn,
        f.owner,
        f.conversation,
        canterlot_hub::history::HistoryCursor::Tail,
        100,
    )
    .await
    .unwrap()
    .unwrap();
    let state = page.items.iter().find_map(|item| match item {
        canterlot_hub::history::TimelineItem::Request(row) if row.id == request => {
            Some(row.state.clone())
        }
        _ => None,
    });
    assert_eq!(state.as_deref(), Some("open"));
    drop(conn);
    assert_eq!(f.decide(request, "allow").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.run_job_by_action("note", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "decision_applied");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trashing_the_proposer_closes_the_proposal_and_blocks_decisions(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "корзина", "base_revision": 0}),
        )
        .await;
    sqlx::query("update executions set state='stopped' where id=$1")
        .bind(f.execution)
        .execute(&f.pool)
        .await
        .unwrap();
    canterlot_hub::deletion::trash(
        &f.pool,
        f.owner,
        f.conversation,
        "trash-settings",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(f.request_state(request).await, "closed_without_decision");
    let response = f
        .client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/requests/{request}/decision",
                f.conversation
            ),
            Some("after-trash"),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
        )
        .await;
    assert_ne!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, None, "a trashed proposal must never be applied");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approval_job_skips_a_trashed_conversation(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "корзина", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    sqlx::query("update conversations set trashed_at=clock_timestamp() where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    f.run_job_by_action("settings_change", Utc::now()).await;
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, None);
    assert_eq!(f.request_state(request).await, "decision_saved");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_routing_edit_wins_the_race_with_an_approved_proposal(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "гонка", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.conversation)
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("select id from owners where id=$1 for update")
        .bind(f.owner)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let owner_policy = json!({"task_types":{"owner":{"title":"Владелец","candidates":[
        {"harness":"claude_code","model":"haiku"}],"expected_minutes":30}}});
    let edit = {
        let client = f.client.clone();
        let body = json!({"policy": owner_policy, "revision": 0});
        tokio::spawn(async move {
            client
                .send(
                    Method::PUT,
                    "/api/v1/routing-policy",
                    Some("owner-race"),
                    Some(body),
                )
                .await
                .status
        })
    };
    wait_for_lock_waiters(&f.pool, 1).await;
    let job = f.spawn_job("settings_change", request);
    wait_for_lock_waiters(&f.pool, 2).await;
    blocker.rollback().await.unwrap();
    assert_eq!(u16::from(edit.await.unwrap()), 200);
    assert!(job.await.unwrap());
    f.run_job_by_action("note", Utc::now()).await;
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let expected: canterlot_hub::session_tools::routing::Policy =
        serde_json::from_value(owner_policy).unwrap();
    assert_eq!(stored, Some(serde_json::to_value(expected).unwrap()));
    assert_eq!(f.request_state(request).await, "setting_changed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_whitelist_edit_wins_the_race_with_an_approved_proposal(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_whitelist",
            json!({"entries": [{"harness":"codex","model":"default"}], "reason": "гонка", "base_revision": 0}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.conversation)
        .execute(&mut *blocker)
        .await
        .unwrap();
    sqlx::query("select id from owners where id=$1 for update")
        .bind(f.owner)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let edit = {
        let client = f.client.clone();
        tokio::spawn(async move {
            client
                .send(
                    Method::PUT,
                    "/api/v1/spawn-whitelist",
                    Some("owner-race"),
                    Some(json!({"entries": [{"harness":"claude_code","model":"haiku"}]})),
                )
                .await
                .status
        })
    };
    wait_for_lock_waiters(&f.pool, 1).await;
    let job = f.spawn_job("settings_change", request);
    wait_for_lock_waiters(&f.pool, 2).await;
    blocker.rollback().await.unwrap();
    assert_eq!(u16::from(edit.await.unwrap()), 200);
    assert!(job.await.unwrap());
    f.run_job_by_action("note", Utc::now()).await;
    let stored: Value = sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, json!([{"harness":"claude_code","model":"haiku"}]));
    assert_eq!(f.request_state(request).await, "setting_changed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_preset_edit_wins_the_race_with_an_approved_proposal(pool: PgPool) {
    let f = Fx::new(pool).await;
    let preset = f.new_preset().await;
    let (request, _) = f
        .proposal(
            "settings.propose_preset",
            json!({"id": preset.to_string(), "text": "текст агента", "reason": "гонка", "base_revision": 1}),
        )
        .await;
    assert_eq!(f.decide(request, "allow").await, 200);
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select id from instruction_presets where id=$1 for update")
        .bind(preset)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let edit = {
        let client = f.client.clone();
        let path = format!("/api/v1/presets/{preset}");
        tokio::spawn(async move {
            client
                .send(
                    Method::PATCH,
                    &path,
                    Some("owner-race"),
                    Some(json!({"text": "текст владельца", "base_revision": 1})),
                )
                .await
                .status
        })
    };
    wait_for_lock_waiters(&f.pool, 1).await;
    let job = f.spawn_job("settings_change", request);
    wait_for_lock_waiters(&f.pool, 2).await;
    blocker.rollback().await.unwrap();
    assert_eq!(u16::from(edit.await.unwrap()), 200);
    assert!(job.await.unwrap());
    f.run_job_by_action("note", Utc::now()).await;
    let text: String = sqlx::query_scalar(
        "select r.text from instruction_presets p
           join instruction_preset_revisions r on r.preset_id=p.id and r.revision=p.revision
          where p.id=$1",
    )
    .bind(preset)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(text, "текст владельца");
    assert_eq!(f.request_state(request).await, "setting_changed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_approval_of_an_overdue_proposal_is_refused_without_a_scheduler(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "поздно", "base_revision": 0}),
        )
        .await;
    // Планировщик не запускался, но срок в базе уже прошёл.
    sqlx::query("update requests set expires_at=clock_timestamp()-interval '1 second' where id=$1")
        .bind(request)
        .execute(&f.pool)
        .await
        .unwrap();
    let response = f
        .client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/requests/{request}/decision",
                f.conversation
            ),
            Some("late-approval"),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
        )
        .await;
    assert_eq!(
        response.status,
        409,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let body = response.json::<Value>();
    assert_eq!(body["error"]["code"], "request_not_open");
    assert_eq!(body["error"]["details"]["reason"], "expired");
    let stored: Option<Value> = sqlx::query_scalar("select routing_policy from owners where id=$1")
        .bind(f.owner)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(stored, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approval_after_expiry_job_returns_conflict_not_internal_error(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "после срока", "base_revision": 0}),
        )
        .await;
    f.run_job_by_action("settings_expire", Utc::now()).await;
    assert_eq!(f.request_state(request).await, "expired");
    let response = f
        .client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/requests/{request}/decision",
                f.conversation
            ),
            Some("after-expiry"),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
        )
        .await;
    assert_eq!(
        response.status,
        409,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(
        response.json::<Value>()["error"]["code"],
        "request_not_open"
    );
    assert_eq!(
        response.json::<Value>()["error"]["details"]["reason"],
        "expired"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn empty_routing_policy_is_refused_like_on_the_settings_screen(pool: PgPool) {
    let f = Fx::new(pool).await;
    let error = f
        .call(
            "settings.propose_routing",
            json!({"policy": {"task_types": {}}, "reason": "пусто", "base_revision": 0}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
    assert!(error.message.contains("task type"), "{}", error.message);
    let response = f
        .client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("empty-policy"),
            Some(json!({"policy": {"task_types": {}}, "revision": 0})),
        )
        .await;
    assert_eq!(
        response.status,
        422,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn whitelist_proposal_requires_the_entries_field(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.client
        .send(
            Method::PUT,
            "/api/v1/spawn-whitelist",
            Some("custom-whitelist"),
            Some(json!({"entries": [{"harness": "claude_code", "model": "haiku"}]})),
        )
        .await;
    let error = f
        .call(
            "settings.propose_whitelist",
            json!({"reason": "сброс", "base_revision": 1}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
    assert!(error.message.contains("entries"), "{}", error.message);
    let requests: i64 =
        sqlx::query_scalar("select count(*) from requests where kind='settings_change'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(requests, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn worker_credential_cannot_decide_a_settings_proposal(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (request, _) = f
        .proposal(
            "settings.propose_routing",
            json!({"policy": routing(), "reason": "x", "base_revision": 0}),
        )
        .await;
    let worker = TestClient::in_process(f.router())
        .with_header("Origin", testkit::TEST_ORIGIN)
        .with_header("X-Canterlot-Client", "1")
        .with_header("Authorization", "Bearer worker-machine-token");
    let response = worker
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/requests/{request}/decision",
                f.conversation
            ),
            Some("worker-decision"),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
        )
        .await;
    assert_eq!(
        response.status,
        401,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(f.request_state(request).await, "open");
}

#[test]
fn openapi_documents_the_settings_change_payload() {
    let document = canterlot_hub::http::openapi::render();
    assert!(
        document.contains("SettingsChangePayload"),
        "the request payload must be documented in OpenAPI"
    );
}
