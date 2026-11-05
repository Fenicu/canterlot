//! Маршрутизация C1: запуск по политике, HTTP политики и предпросмотр.

use std::sync::Arc;

use axum::http::Method;
use canterlot_hub::{
    config::{LimitsConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    http,
    session::Registry,
    session_tools::{self, Caller, TOOLS, ToolContext, ToolErrorCode as Code, settle::SettleConfig},
    testkit::{self, FakeSession, TestClient},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const CAPABILITIES: [&str; 3] = ["pilot_v1", "instructions_v1", "session_tools_v1"];

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    parent: Uuid,
    execution: Uuid,
    _session: FakeSession,
}

fn model(id: &str, efforts: &[&str]) -> Value {
    json!({"id": id, "title": id, "efforts": efforts})
}

async fn catalog(pool: &PgPool, machine: Uuid, harness: &str, provider: &str, models: Value) {
    sqlx::query(
        "insert into machine_harnesses(machine_id,harness,provider,title,images,settings_midsession,
                                       permission_modes,models,settings_midsession_effort)
         values($1,$2,$3,$2,true,true,'[\"owner\",\"ask\",\"bypass\",\"read_only\"]',$4,true)",
    )
    .bind(machine)
    .bind(harness)
    .bind(provider)
    .bind(models)
    .execute(pool)
    .await
    .unwrap();
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        Self::with_seed(pool, seed.owner_id, seed.machine_id, seed.conversation_id).await
    }

    async fn for_owner(pool: PgPool, owner: Uuid) -> Self {
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'synthetic')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let conversation = testkit::seed_conversation(&pool, owner, machine).await;
        Self::with_seed(pool, owner, machine, conversation).await
    }

    async fn with_seed(pool: PgPool, owner: Uuid, machine: Uuid, conversation: Uuid) -> Self {
        let registry = Registry::new(Default::default());
        let session = testkit::fake_session(&registry, machine, &CAPABILITIES, true);
        for (harness, provider, models) in [
            (
                "opencode",
                "opencode",
                json!([
                    model("opencode-go/deepseek-v4.1-flash", &["low", "medium", "high"]),
                    model("opencode-go/muse-spark-1.3-contributor", &["low", "medium", "high"]),
                    model("opencode-go/beam-free", &["low", "medium"]),
                ]),
            ),
            (
                "claude_code",
                "claude",
                json!([model("opus[1m]", &["low", "medium", "high"])]),
            ),
            (
                "codex",
                "codex",
                json!([model("gpt-6.1-sol", &["low", "medium", "high"])]),
            ),
        ] {
            catalog(&pool, machine, harness, provider, models).await;
        }
        sqlx::query(
            "update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true,
               allowed_roots='[\"/tmp\"]',worktree_root='/data/worktrees' where id=$1",
        )
        .bind(machine)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "update conversations set title='Родитель',harness='opencode',model='opencode-go/muse-spark-1.3-contributor',
               effort='high',permission_mode='bypass',workdir='/tmp/w' where id=$1",
        )
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
        let started = canterlot_hub::executions::start(
            &pool,
            owner,
            conversation,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
            .bind(owner)
            .bind(json!([
                {"harness":"opencode","model":"opencode-go/deepseek-v4.1-flash"},
                {"harness":"opencode","model":"opencode-go/muse-spark-1.3-contributor"},
                {"harness":"opencode","model":"opencode-go/beam-free"},
                {"harness":"claude_code","model":"opus[1m]"},
                {"harness":"codex","model":"gpt-6.1-sol"},
            ]))
            .execute(&pool)
            .await
            .unwrap();
        let (dispatcher, _) = Dispatcher::channel();
        Self {
            context: ToolContext {
                pool: pool.clone(),
                config: Arc::new(SessionToolsConfig::default()),
                limits: Default::default(),
                settle: SettleConfig {
                    idle_quiet: std::time::Duration::from_secs(5),
                    nudge_enabled: true,
                },
                alarms_enabled: true,
                nudge_enabled: true,
                routing_enabled: true,
                routing_stale: std::time::Duration::from_secs(1800),
                dispatcher: None,
                registry: registry.clone(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            pool,
            owner,
            machine,
            parent: conversation,
            execution: started.execution_id,
            _session: session,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.parent,
            execution_id: self.execution,
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, session_tools::ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }
}

async fn child_of(pool: &PgPool, parent: Uuid, harness: &str, model: &str, state: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into conversations(id,owner_id,machine_id,harness,workdir,model,effort,parent_conversation_id,spawn_depth)
         values($1,(select owner_id from conversations where id=$2),$3,$4,'/tmp/w',$5,'high',$2,1)",
    )
    .bind(id)
    .bind(parent)
    .bind(sqlx::query_scalar::<_, Uuid>("select machine_id from conversations where id=$1").bind(parent).fetch_one(pool).await.unwrap())
    .bind(harness)
    .bind(model)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,queue_queued) values($1,$2,0)",
    )
    .bind(id)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_route_starts_resolved_session(pool: PgPool) {
    let f = Fx::new(pool).await;
    let answer = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"Почини падение"}),
        )
        .await
        .unwrap();
    assert_eq!(answer["status"], "started", "{answer}");
    let child: Uuid = serde_json::from_value(answer["session_id"].clone()).unwrap();
    let row: (String, String, String, Option<Value>) = sqlx::query_as(
        "select harness,model,effort,routing from conversations where id=$1",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(row.0, "opencode");
    assert_eq!(row.1, "opencode-go/deepseek-v4.1-flash");
    assert_eq!(row.2, "high");
    let routing = row.3.expect("routing saved");
    assert_eq!(routing["task_type"], "bugfix");
    assert_eq!(routing["harness"], "opencode");
    // Подсказка называет выбор одной строкой.
    assert!(
        answer["hint"]
            .as_str()
            .unwrap()
            .starts_with("Routed bugfix to opencode/opencode-go/deepseek-v4.1-flash (high);"),
        "{answer}"
    );
    assert_eq!(answer["routing"]["task_type"], "bugfix");
    // Автопинок кандидата и сторож взведены.
    let nudge: Option<i32> = sqlx::query_scalar(
        "select max_consecutive from nudge_rules where conversation_id=$1",
    )
    .bind(child)
    .fetch_optional(&f.pool)
    .await
    .unwrap();
    assert_eq!(nudge, Some(2));
    let watchdog: Option<String> = sqlx::query_scalar(
        "select key from alarms where conversation_id=$1 and state='armed'",
    )
    .bind(f.parent)
    .fetch_optional(&f.pool)
    .await
    .unwrap();
    assert!(
        watchdog.is_some_and(|key| key.starts_with("watchdog-")),
        "watchdog armed"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_route_and_harness_or_model_is_invalid(pool: PgPool) {
    let f = Fx::new(pool).await;
    for args in [
        json!({"route":{"task_type":"bugfix"},"harness":"opencode","brief":"b"}),
        json!({"route":{"task_type":"bugfix"},"model":"m","brief":"b"}),
        json!({"route":{"task_type":"bugfix"},"effort":"high","brief":"b"}),
        json!({"brief":"b"}),
    ] {
        let error = f.call("sessions.spawn", args).await.unwrap_err();
        assert_eq!(error.code, Code::InvalidArguments);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_unknown_task_type_lists_available(pool: PgPool) {
    let f = Fx::new(pool).await;
    let error = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"nope"},"brief":"b"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::TaskTypeUnknown);
    assert!(error.message.contains("bugfix"), "{}", error.message);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_route_reports_no_executor(pool: PgPool) {
    let f = Fx::new(pool).await;
    for provider in ["opencode", "claude", "codex"] {
        sqlx::query(
            "insert into provider_limits(provider,weekly_used_permille,weekly_observed_at)
             values($1,990,now()) on conflict(provider) do update
             set weekly_used_permille=990,weekly_observed_at=now()",
        )
        .bind(provider)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let error = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"b"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::NoExecutorAvailable, "{}", error.message);
    assert!(error.message.contains("weekly"), "{}", error.message);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_route_counts_only_live_delegates(pool: PgPool) {
    let f = Fx::new(pool).await;
    // Четыре живых делегата на DeepSeek: выбор уходит к Muse.
    for _ in 0..4 {
        child_of(
            &f.pool,
            f.parent,
            "opencode",
            "opencode-go/deepseek-v4.1-flash",
            "working",
        )
        .await;
    }
    // Один остановленный на DeepSeek: в подсчёт не входит.
    child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/deepseek-v4.1-flash",
        "stopped",
    )
    .await;
    let answer = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"b"}),
        )
        .await
        .unwrap();
    assert_eq!(answer["status"], "started", "{answer}");
    assert_eq!(
        answer["routing"]["model"],
        "opencode-go/muse-spark-1.3-contributor",
        "{answer}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_with_route_applies_explicit_overrides(pool: PgPool) {
    let f = Fx::new(pool).await;
    let preset_id = Uuid::now_v7();
    sqlx::query("insert into instruction_presets(id,owner_id,title,revision,origin) values($1,$2,'Свой',1,'owner')")
        .bind(preset_id)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into instruction_preset_revisions(preset_id,revision,text,edited_by) values($1,1,'Профиль', 'owner')")
        .bind(preset_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let answer = f
        .call(
            "sessions.spawn",
            json!({
                "route": {"task_type": "review", "level": "high"},
                "brief": "Посмотри",
                "title": "Обзор",
                "isolated": true,
                "permission_mode": "read_only",
                "preset": preset_id.to_string(),
                "nudge": false,
                "watchdog_minutes": 60,
            }),
        )
        .await
        .unwrap();
    assert_eq!(answer["status"], "started", "{answer}");
    let child: Uuid = serde_json::from_value(answer["session_id"].clone()).unwrap();
    let row: (String, Option<serde_json::Value>, String) = sqlx::query_as(
        "select permission_mode,worktree,instruction_profile from conversations where id=$1",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(row.0, "read_only");
    assert!(row.1.is_some(), "explicit isolated:true wins");
    assert_eq!(row.2, preset_id.to_string(), "explicit preset wins");
    let nudge: Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from nudge_rules where conversation_id=$1",
    )
    .bind(child)
    .fetch_optional(&f.pool)
    .await
    .unwrap();
    assert_eq!(nudge, None, "explicit nudge:false wins");
    let deadline: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "select deadline_at from alarms
          where conversation_id=$1 and key like 'watchdog-%%' and state='armed'",
    )
    .bind(f.parent)
    .fetch_optional(&f.pool)
    .await
    .unwrap()
    .flatten();
    let deadline = deadline.expect("watchdog armed from explicit minutes");
    let lead = (deadline - chrono::Utc::now()).num_minutes();
    assert!((50..=70).contains(&lead), "watchdog ~60 min, got {lead}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_agent_call_changes_routing_policy(pool: PgPool) {
    let f = Fx::new(pool).await;
    let snapshot = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>(
            "select string_agg(o::text,',' order by id) from owners o",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = snapshot(f.pool.clone()).await;
    let attempts = [
        json!({}),
        json!({"task_type":"bugfix"}),
        json!({"routing_policy":{"task_types":{}}}),
        json!({"policy":null,"revision":99}),
    ];
    for tool in TOOLS {
        for args in &attempts {
            let _ = session_tools::call(&f.context, &f.caller(), tool, args.clone()).await;
        }
    }
    assert_eq!(snapshot(f.pool.clone()).await, before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routing_policy_http_roundtrip(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
    let get = || client.send(Method::GET, "/api/v1/routing-policy", None, None);
    let first = get().await;
    assert_eq!(first.status, 200);
    let body = first.json::<Value>();
    assert_eq!(body["policy"], Value::Null);
    assert_eq!(body["revision"], 0);
    assert!(body["builtin"]["task_types"]["bugfix"].is_object());

    // Неверная политика отклоняется.
    let bad = client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("route_bad"),
            Some(json!({"policy":{"task_types":{"Bad Key":{"title":"","candidates":[]}}},"revision":0})),
        )
        .await;
    assert_eq!(bad.status, 422, "{}", bad.status);

    // Сохранение растёт ревизией и публикует событие.
    let policy = json!({"task_types":{
        "smoke": {"title": "Проверка", "candidates": [
            {"harness": "opencode", "model": "m", "watchdog_minutes": 30}
        ], "isolated": true}
    }});
    let saved = client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("route_save"),
            Some(json!({"policy": policy, "revision": 0})),
        )
        .await;
    assert_eq!(saved.status, 200, "{:?}", saved.body);
    let saved_body = saved.json::<Value>();
    assert_eq!(saved_body["revision"], 1);
    assert_eq!(saved_body["policy"]["task_types"]["smoke"]["title"], "Проверка");

    // Устаревшая ревизия даёт конфликт.
    let stale = client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("route_stale"),
            Some(json!({"policy": policy, "revision": 0})),
        )
        .await;
    assert_eq!(stale.status, 409, "{:?}", stale.body);

    // Сброс ко встроенной через null.
    let reset = client
        .send(
            Method::PUT,
            "/api/v1/routing-policy",
            Some("route_reset"),
            Some(json!({"policy": null, "revision": 1})),
        )
        .await;
    assert_eq!(reset.status, 200);
    assert_eq!(reset.json::<Value>()["policy"], Value::Null);

    let seen: bool = sqlx::query_scalar(
        "select exists(select 1 from events where kind='routing_policy_changed')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(seen, "routing_policy_changed emitted");
    let _ = owner;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routing_preview_http_matches_tool(pool: PgPool) {
    let (deps, _dir) = testkit::test_app_deps(
        pool.clone(),
        testkit::test_http_config(),
        None,
    )
    .await;
    let owner = deps.installation.owner_id;
    let f = Fx::for_owner(pool, owner).await;
    let http_client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
    let tool = f
        .call("routing.preview", json!({"task_type": "bugfix"}))
        .await
        .unwrap();
    let via_http = http_client
        .send(
            Method::POST,
            "/api/v1/routing-policy/preview",
            None,
            Some(json!({"task_type": "bugfix"})),
        )
        .await;
    assert_eq!(via_http.status, 200, "{:?}", via_http.body);
    let http_body = via_http.json::<Value>();
    assert_eq!(http_body["decision"], tool["decision"], "preview equals tool answer");
    assert_eq!(http_body["decision"]["harness"], "opencode");

    // Список типов без task_type.
    let list = f.call("routing.preview", json!({})).await.unwrap();
    assert!(list["task_types"].as_array().unwrap().iter().any(|t| t["key"] == "bugfix"));

    // Неизвестный тип называет доступные.
    let error = f
        .call("routing.preview", json!({"task_type": "nope"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::TaskTypeUnknown);

    let http_unknown = http_client
        .send(
            Method::POST,
            "/api/v1/routing-policy/preview",
            None,
            Some(json!({"task_type": "nope"})),
        )
        .await;
    assert_eq!(http_unknown.status, 404, "{:?}", http_unknown.body);
}
