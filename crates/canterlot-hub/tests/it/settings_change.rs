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
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
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

fn routing() -> Value {
    json!({"task_types":{"review":{"title":"Проверка","candidates":[
        {"harness":"claude_code","model":"sonnet[1m]"}],"expected_minutes":30}}})
}

#[tokio::test]
async fn routing_proposal_allow_applies_with_events_and_note() {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn agent_cannot_decide_a_settings_proposal() {
    let pool = canterlot_hub::testkit::db().await;
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

async fn settings_requests(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from requests where kind='settings_change'")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn whitelist_args(model: &str) -> Value {
    json!({"entries": [{"harness":"claude_code","model":model}], "reason": "x", "base_revision": 0})
}

/// Зациклившийся агент предлагает одну и ту же настройку раз за разом, слегка меняя значение.
/// Пока владелец не решил первую карточку, вторая по той же настройке не заводится: ни
/// запроса, ни пуша.
#[tokio::test]
async fn a_setting_has_one_open_proposal_per_conversation() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let (first, _) = f
        .proposal("settings.propose_whitelist", whitelist_args("haiku"))
        .await;
    let error = f
        .call("settings.propose_whitelist", whitelist_args("sonnet"))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "rate_limited", "{error:?}");
    assert!(
        error.message.contains(&first.to_string()),
        "{}",
        error.message
    );
    assert_eq!(settings_requests(&f.pool).await, 1);

    // Другая настройка не занята.
    f.proposal(
        "settings.propose_routing",
        json!({"policy": routing(), "reason": "x", "base_revision": 0}),
    )
    .await;
    // Владелец решил: место свободно.
    assert_eq!(f.decide(first, "deny").await, 200);
    f.run_job_by_action("settings_change", Utc::now()).await;
    f.proposal("settings.propose_whitelist", whitelist_args("sonnet"))
        .await;
    assert_eq!(settings_requests(&f.pool).await, 3);
}

/// Текст пресета от агента проходит детектор секретов уже при предложении: карточка с
/// паролем владельцу не уходит.
#[tokio::test]
async fn a_preset_proposal_with_a_secret_is_refused() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let preset = f.new_preset().await;
    let error = f
        .call(
            "settings.propose_preset",
            json!({"id": preset.to_string(), "reason": "x", "base_revision": 1,
                   "text": "curl -u admin:S3cr3tPass! https://grafana.internal/api/health"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "invalid_arguments", "{error:?}");
    assert!(
        error.message.contains("looks_like_secret"),
        "{}",
        error.message
    );
    assert_eq!(settings_requests(&f.pool).await, 0);
}

/// Часовой предел карточек настроек на разговор: решённые карточки тоже в счёте.
#[tokio::test]
async fn settings_proposals_are_capped_per_hour() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    for n in 0..5 {
        let (request, _) = f
            .proposal(
                "settings.propose_whitelist",
                whitelist_args(&format!("model-{n}")),
            )
            .await;
        assert_eq!(f.decide(request, "deny").await, 200);
        f.run_job_by_action("settings_change", Utc::now()).await;
        assert_eq!(f.request_state(request).await, "declined");
        // Помощник задание не закрывает: убираем его, чтобы следующий круг взял своё.
        sqlx::query("delete from scheduled_jobs where payload->>'action'='settings_change'")
            .execute(&f.pool)
            .await
            .unwrap();
    }
    let error = f
        .call("settings.propose_whitelist", whitelist_args("one-more"))
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "rate_limited", "{error:?}");
    assert!(error.message.contains("hour"), "{}", error.message);
    assert_eq!(settings_requests(&f.pool).await, 5);

    // Час прошёл: счётчик пуст.
    sqlx::query("update requests set created_at=created_at - interval '61 minutes'")
        .execute(&f.pool)
        .await
        .unwrap();
    f.proposal("settings.propose_whitelist", whitelist_args("one-more"))
        .await;
}
