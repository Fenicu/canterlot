use std::{collections::BTreeSet, sync::Arc};

use axum::http::Method;
use canterlot_hub::{
    config::{LimitsConfig, SchedulerConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions, http,
    model::ExecutionState,
    requests::{self, DecisionResult},
    scheduler::Scheduler,
    session::Registry,
    session_tools::{
        self, Caller, TOOLS, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
        whitelist,
    },
    testkit::{self, FakeSession, TestClient},
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const OUTSIDE: &str = "opus[1m]";
const INSIDE: &str = "haiku";
const CAPABILITIES: [&str; 3] = ["pilot_v1", "instructions_v1", "session_tools_v1"];

struct Fx {
    pool: PgPool,
    context: ToolContext,
    scheduler: Scheduler,
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
        Self::with(pool, SessionToolsConfig::default(), &CAPABILITIES).await
    }

    async fn with(pool: PgPool, config: SessionToolsConfig, capabilities: &[&str]) -> Self {
        let seed = testkit::seed(&pool).await;
        let registry = Registry::new(Default::default());
        let session = testkit::fake_session(&registry, seed.machine_id, capabilities, true);
        sqlx::query(
            "update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true,
               allowed_roots='[\"/tmp\"]',worktree_root='/data/worktrees' where id=$1",
        )
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
        for (harness, provider, models) in [
            (
                "claude_code",
                "claude",
                json!([
                    model(OUTSIDE, &["low", "high"]),
                    model("sonnet[1m]", &["low", "medium", "high"]),
                    model(INSIDE, &["none"]),
                ]),
            ),
            (
                "codex",
                "codex",
                json!([
                    model("gpt-first", &["medium"]),
                    model("gpt-second", &["medium"]),
                ]),
            ),
            (
                "synthetic",
                "claude",
                json!([model("small", &["low", "high"])]),
            ),
        ] {
            catalog(&pool, seed.machine_id, harness, provider, models).await;
        }
        sqlx::query(
            "update conversations set title='Родитель \"главный\"',model='small',effort='low',
               permission_mode='bypass',workdir='/tmp/w' where id=$1",
        )
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
        let started = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
            .await
            .unwrap();
        sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
            .bind(seed.owner_id)
            .bind(json!([
                {"harness":"claude_code","model":INSIDE},
                {"harness":"codex","model":"default"},
                {"harness":"synthetic","model":"small"},
            ]))
            .execute(&pool)
            .await
            .unwrap();
        let (dispatcher, _) = Dispatcher::channel();
        Self {
            context: ToolContext {
                pool: pool.clone(),
                config: Arc::new(config),
                limits: Default::default(),
                settle: SettleConfig {
                    idle_quiet: std::time::Duration::from_secs(5),
                    nudge_enabled: true,
                },
                alarms_enabled: true,
                nudge_enabled: true,
                dispatcher: None,
                registry: registry.clone(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            scheduler: Scheduler::new(pool.clone(), dispatcher, Default::default())
                .with_limits(registry, LimitsConfig::default()),
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            parent: seed.conversation_id,
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

    async fn spawn(&self, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(), "sessions.spawn", args).await
    }

    async fn attachment(&self, name: &str, mime: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
             values($1,$2,$3,$4,$5,4,$6,'ready',clock_timestamp())",
        )
        .bind(id)
        .bind(self.owner)
        .bind(self.parent)
        .bind(name)
        .bind(mime)
        .bind("0".repeat(64))
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn pending(&self, args: Value) -> Uuid {
        let answer = self.spawn(args).await.unwrap();
        assert_eq!(answer["status"], "pending_approval", "{answer}");
        serde_json::from_value(answer["request_id"].clone()).unwrap()
    }

    async fn started(&self, args: Value) -> (Uuid, Value) {
        let answer = self.spawn(args).await.unwrap();
        assert_eq!(answer["status"], "started", "{answer}");
        (
            serde_json::from_value(answer["session_id"].clone()).unwrap(),
            answer,
        )
    }

    async fn decide_with(
        &self,
        request: Uuid,
        behavior: PermissionBehavior,
        key: &str,
    ) -> DecisionResult {
        let mut tx = self.pool.begin().await.unwrap();
        let result = requests::save_decision_in_tx(
            &mut tx,
            self.parent,
            request,
            1,
            &DecisionPayload::Permission {
                behavior,
                message: None,
            },
            key,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        result
    }

    async fn decide(&self, request: Uuid, behavior: PermissionBehavior) -> DecisionResult {
        self.decide_with(request, behavior, "owner-key").await
    }

    async fn tick(&self) {
        self.scheduler.tick(Utc::now()).await.unwrap();
    }

    async fn sessions(&self) -> i64 {
        sqlx::query_scalar("select count(*) from conversations where owner_id=$1")
            .bind(self.owner)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn child(&self) -> Option<Uuid> {
        sqlx::query_scalar("select id from conversations where parent_conversation_id=$1")
            .bind(self.parent)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
    }

    async fn request_state(&self, request: Uuid) -> String {
        sqlx::query_scalar("select state from requests where id=$1")
            .bind(request)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn closed_reason(&self, request: Uuid) -> Option<String> {
        sqlx::query_scalar(
            "select payload->>'reason' from events
             where kind='request_closed' and payload->>'request_id'=$1",
        )
        .bind(request.to_string())
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }

    async fn inputs(&self, conversation: Uuid) -> Vec<(String, Option<Value>)> {
        sqlx::query_as::<_, (Value, Option<Value>)>(
            "select content,origin from inputs where conversation_id=$1 order by input_seq",
        )
        .bind(conversation)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(content, origin)| (content[0]["text"].as_str().unwrap().to_owned(), origin))
        .collect()
    }

    async fn notes(&self) -> Vec<String> {
        self.inputs(self.parent)
            .await
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    async fn set_modes(&self, conversation: Uuid, mode: &str, pending: Value) {
        sqlx::query("update conversations set permission_mode=$2,pending_settings=$3 where id=$1")
            .bind(conversation)
            .bind(mode)
            .bind(pending)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn mode(&self, conversation: Uuid) -> String {
        sqlx::query_scalar("select permission_mode from conversations where id=$1")
            .bind(conversation)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn start_payload(&self, conversation: Uuid) -> Value {
        sqlx::query_scalar(
            "select payload from commands where conversation_id=$1 and kind='start_execution'",
        )
        .bind(conversation)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn stop_parent(&self) {
        executions::set_state(
            &self.pool,
            self.execution,
            ExecutionState::OwnerLost,
            None,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }
}

fn inside() -> Value {
    json!({"harness":"claude_code","model":INSIDE,"brief":"Сделай задачу"})
}

fn outside() -> Value {
    json!({"harness":"claude_code","model":OUTSIDE,"brief":"Сделай большую задачу"})
}

fn code(result: Result<Value, ToolError>) -> Code {
    result.expect_err("tool error").code
}

#[test]
fn default_whitelist_matches_decision() {
    let pairs: Vec<(String, String)> = whitelist::default_entries()
        .into_iter()
        .map(|entry| (entry.harness, entry.model))
        .collect();
    let expected = [
        ("claude_code", "sonnet[1m]"),
        ("claude_code", "haiku"),
        ("codex", "default"),
        ("opencode", "opencode-go/muse-spark-1.3-contributor"),
    ]
    .map(|(harness, model)| (harness.to_owned(), model.to_owned()));
    assert_eq!(pairs, expected);
}

struct Api {
    client: TestClient,
    owner: Uuid,
    pool: PgPool,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let router = http::router(http::AppState::new(deps));
        Self {
            client: TestClient::in_process(router).as_owner(),
            owner,
            pool,
            _dir: dir,
        }
    }

    async fn put(&self, key: &str, body: Value) -> testkit::TestResponse {
        self.client
            .send(
                Method::PUT,
                "/api/v1/spawn-whitelist",
                Some(key),
                Some(body),
            )
            .await
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn put_whitelist_validates_and_resets(pool: PgPool) {
    let api = Api::new(pool).await;
    let view: Value = api
        .client
        .send(Method::GET, "/api/v1/spawn-whitelist", None, None)
        .await
        .json();
    assert_eq!(view["is_default"], true);
    assert_eq!(view["entries"], view["default_entries"]);

    let entry = json!({"harness":"codex","model":"gpt"});
    let saved = api.put("save", json!({"entries":[entry]})).await;
    assert_eq!(saved.status, 200);
    let view: Value = saved.json();
    assert_eq!(view["is_default"], false);
    assert_eq!(view["entries"], json!([entry]));
    assert_eq!(view["default_entries"][0]["model"], "sonnet[1m]");

    let too_many: Vec<Value> = (0..65)
        .map(|n| json!({"harness":"codex","model":format!("m{n}")}))
        .collect();
    for (key, entries) in [
        ("dup", json!([entry, entry])),
        ("many", json!(too_many)),
        ("harness", json!([{"harness":"nope","model":"x"}])),
        ("empty", json!([{"harness":"codex","model":""}])),
        ("long", json!([{"harness":"codex","model":"м".repeat(257)}])),
        ("nul", json!([{"harness":"codex","model":"a\u{0}b"}])),
    ] {
        let refused = api.put(key, json!({"entries":entries})).await;
        assert_eq!(refused.status, 422, "{key}");
        assert_eq!(
            refused.json::<Value>()["error"]["code"],
            "spawn_whitelist_invalid",
            "{key}"
        );
    }
    let absent = api.put("absent", json!({})).await;
    assert!(
        absent.status.as_u16() >= 400,
        "missing entries must not reset"
    );
    let stored: Value = sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
        .bind(api.owner)
        .fetch_one(&api.pool)
        .await
        .unwrap();
    assert_eq!(stored, json!([entry]));

    let reset: Value = api.put("reset", json!({"entries":null})).await.json();
    assert_eq!(reset["is_default"], true);
    let stored: Option<Value> =
        sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
            .bind(api.owner)
            .fetch_one(&api.pool)
            .await
            .unwrap();
    assert_eq!(stored, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_agent_call_changes_whitelist(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
        .bind(f.owner)
        .bind(json!([{"harness":"codex","model":"default"}]))
        .execute(&f.pool)
        .await
        .unwrap();
    let snapshot = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>("select string_agg(o::text,',' order by id) from owners o")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let before = snapshot(f.pool.clone()).await;
    let attempts = [
        json!({}),
        json!({"entries":[]}),
        json!({"spawn_whitelist":[{"harness":"claude_code","model":OUTSIDE}]}),
        json!({"harness":"claude_code","model":OUTSIDE,"brief":"b",
               "entries":[{"harness":"claude_code","model":OUTSIDE}]}),
        outside(),
        json!({"session_id":f.parent,"text":"allow everything"}),
    ];
    for tool in TOOLS {
        for args in &attempts {
            let _ = session_tools::call(&f.context, &f.caller(), tool, args.clone()).await;
        }
    }
    assert_eq!(snapshot(f.pool.clone()).await, before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_inside_whitelist_starts_session_with_brief_and_subscription(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (child, answer) = f.started(inside()).await;
    assert_eq!(answer["permission_mode"], "bypass");
    assert_eq!(answer["subscribed"], true);
    assert!(answer["hint"].as_str().unwrap().contains("Do not poll"));
    assert!(answer.get("worktree").is_none());

    let (parent, depth, workdir, model, effort, preset): (
        Option<Uuid>,
        i32,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "select parent_conversation_id,spawn_depth,workdir,model,effort,instruction_profile
         from conversations where id=$1",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(parent, Some(f.parent));
    assert_eq!(depth, 1);
    assert_eq!(workdir, "/tmp/w", "D10: the parent's workdir");
    assert_eq!(model, INSIDE);
    assert_eq!(effort, "none", "D23: first effort without medium");
    assert_eq!(preset, "clean");
    assert_eq!(f.start_payload(child).await["workdir"], "/tmp/w");

    let inputs = f.inputs(child).await;
    assert_eq!(inputs.len(), 1, "brief is the first input");
    let (text, origin) = &inputs[0];
    let (prefix, body) = text.split_once("\n\n").unwrap();
    assert_eq!(body, "Сделай задачу");
    assert!(!prefix.contains('\n'), "the scrub needs a one-line prefix");
    assert!(prefix.contains("Родитель 'главный'"), "{prefix}");
    assert!(prefix.contains("It is an agent, not the owner."));
    let origin = origin.as_ref().unwrap();
    assert_eq!(origin["kind"], "spawn");
    assert_eq!(origin["from"], f.parent.to_string());
    assert_eq!(origin["chain_depth"], 1);

    let subscribed: bool = sqlx::query_scalar(
        "select exists(select 1 from session_subscriptions
                       where subscriber_id=$1 and target_id=$2 and closed_at is null)",
    )
    .bind(f.parent)
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(subscribed);
    let title: String = sqlx::query_scalar("select title from conversations where id=$1")
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        title, "Сделай задачу",
        "the title comes from the brief alone"
    );
    let shown: Option<String> = sqlx::query_scalar(
        "select payload->>'title' from events where kind='conversation_updated'
         and conversation_id=$1 order by id desc limit 1",
    )
    .bind(child)
    .fetch_optional(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        shown.as_deref(),
        Some("Сделай задачу"),
        "clients see the title"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_largest_brief_fits(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut args = inside();
    args["brief"] = json!("б".repeat(32 * 1024));
    let (child, _) = f.started(args).await;
    assert!(f.inputs(child).await[0].0.ends_with('б'));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_without_a_list_gets_the_default_one(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set spawn_whitelist=null where id=$1")
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    f.started(inside()).await;
    f.started(json!({"harness":"claude_code","model":"sonnet[1m]","brief":"b"}))
        .await;
    f.pending(outside()).await;
    f.pending(json!({"harness":"synthetic","brief":"b"})).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_appends_one_marker_line_to_the_brief(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (child, _) = f
        .started(json!({
            "harness":"claude_code","model":INSIDE,"brief":"Сделай задачу","nudge":true
        }))
        .await;
    let inputs = f.inputs(child).await;
    let text = &inputs[0].0;
    assert!(
        text.ends_with(
            "When you are completely finished, end your final message with a line `STATUS: DONE` \
         (or `STATUS: BLOCKED: <reason>` if you cannot continue)."
        ),
        "{text}"
    );
    assert_eq!(
        text.matches("When you are completely finished").count(),
        1,
        "the instruction is appended once"
    );
    let markers: Vec<String> =
        sqlx::query_scalar("select done_markers from nudge_rules where conversation_id=$1")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(markers, vec!["STATUS: DONE", "STATUS: BLOCKED"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_marker_line_uses_the_rules_markers_and_is_idempotent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (child, _) = f
        .started(json!({
            "harness":"claude_code","model":INSIDE,
            "brief":"Сделай задачу",
            "nudge":{"done_markers":["ГОТОВО","НЕ МОГУ"]}
        }))
        .await;
    let text = &f.inputs(child).await[0].0;
    assert!(
        text.ends_with(
            "When you are completely finished, end your final message with a line `ГОТОВО` \
         (or `НЕ МОГУ: <reason>` if you cannot continue)."
        ),
        "{text}"
    );

    // Бриф, уже кончающийся этой строкой, не получает вторую.
    let line = "When you are completely finished, end your final message with a line `STATUS: DONE` \
                (or `STATUS: BLOCKED: <reason>` if you cannot continue).";
    let (child, _) = f
        .started(json!({
            "harness":"claude_code","model":INSIDE,
            "brief":format!("Сделай задачу\n\n{line}"),
            "nudge":true
        }))
        .await;
    let text = &f.inputs(child).await[0].0;
    assert_eq!(
        text.matches("When you are completely finished").count(),
        1,
        "{text}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn without_nudge_the_brief_has_no_marker_line(pool: PgPool) {
    let f = Fx::new(pool).await;
    for nudge in [json!(false), json!(null)] {
        let (child, _) = f
            .started(json!({
                "harness":"claude_code","model":INSIDE,"brief":"Сделай задачу","nudge":nudge
            }))
            .await;
        let text = &f.inputs(child).await[0].0;
        assert!(!text.contains("STATUS: DONE"), "{text}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_without_subscription_stays_unsubscribed(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut args = inside();
    args["subscribe"] = json!(false);
    let (_, answer) = f.started(args).await;
    assert_eq!(answer["subscribed"], false);
    assert!(
        !answer["hint"]
            .as_str()
            .unwrap()
            .contains("You are subscribed")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_outside_whitelist_opens_card_and_creates_nothing(pool: PgPool) {
    let f = Fx::new(pool).await;
    let before = f.sessions().await;
    let request = f.pending(outside()).await;
    assert_eq!(f.sessions().await, before);

    let (kind, native, state, execution, seq, payload): (
        String,
        String,
        String,
        Uuid,
        Option<i64>,
        Value,
    ) = sqlx::query_as(
        "select kind,native_request_id,state,execution_id,created_seq,request_payload
         from requests where id=$1",
    )
    .bind(request)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(kind, "spawn_approval");
    assert!(native.starts_with("spawn:"), "{native}");
    assert_eq!(state, "open");
    assert_eq!(execution, f.execution);
    assert_eq!(payload["harness"], "claude_code");
    assert_eq!(payload["model"], OUTSIDE);
    assert_eq!(payload["effort"], "low");
    assert_eq!(payload["permission_mode"], "bypass", "resolved mode");
    assert_eq!(payload["brief"], "Сделай большую задачу");

    let opened: (i64, Value) = sqlx::query_as(
        "select scope_seq,payload from events where kind='request_opened'
         and conversation_id=$1",
    )
    .bind(f.parent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(seq, Some(opened.0), "the feed position of the card");
    assert_eq!(opened.1["kind"], "spawn_approval");
    assert_eq!(opened.1["request_id"], request.to_string());

    let holds: Value =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(f.parent)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(
        !holds.to_string().contains("awaiting_user_request"),
        "D14: the queue is not held: {holds}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn catalog_errors_open_no_card(pool: PgPool) {
    let f = Fx::new(pool).await;
    assert_eq!(
        code(f.spawn(json!({"harness":"nope","brief":"b"})).await),
        Code::HarnessUnknown
    );
    assert_eq!(
        code(
            f.spawn(json!({"harness":"claude_code","model":"nope","brief":"b"}))
                .await
        ),
        Code::ModelUnknown
    );
    assert_eq!(
        code(
            f.spawn(json!({"harness":"claude_code","model":OUTSIDE,"effort":"max","brief":"b"}))
                .await
        ),
        Code::EffortNotAllowed
    );
    let mut args = outside();
    args["preset"] = json!(Uuid::now_v7());
    assert_eq!(code(f.spawn(args).await), Code::ProfileUnknown);
    let mut args = outside();
    args["workdir"] = json!("/etc");
    assert_eq!(code(f.spawn(args).await), Code::WorkdirNotAllowed);
    assert_eq!(
        code(f.spawn(json!({"harness":"claude_code","brief":"  "})).await),
        Code::InvalidArguments
    );
    assert_eq!(
        code(
            f.spawn(json!({"harness":"claude_code","brief":"b","unknown":1}))
                .await
        ),
        Code::InvalidArguments
    );
    assert_eq!(
        code(
            f.spawn(json!({"harness":"claude_code","brief":"я".repeat(33 * 1024)}))
                .await
        ),
        Code::TextTooLarge
    );
    let opened: i64 = sqlx::query_scalar("select count(*) from requests")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(opened, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn allow_creates_session_and_notifies_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    assert!(matches!(
        f.decide(request, PermissionBehavior::Allow).await,
        DecisionResult::Committed { .. }
    ));
    assert_eq!(f.request_state(request).await, "decision_saved");
    assert!(f.child().await.is_none(), "the job creates it");

    f.tick().await;
    let child = f.child().await.expect("session exists after allow");
    assert_eq!(f.request_state(request).await, "decision_applied");
    assert_eq!(f.closed_reason(request).await.as_deref(), Some("applied"));
    assert_eq!(f.mode(child).await, "bypass");
    let inputs = f.inputs(child).await;
    assert!(inputs[0].0.ends_with("Сделай большую задачу"));
    assert_eq!(inputs[0].1.as_ref().unwrap()["kind"], "spawn");

    let notes = f.inputs(f.parent).await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].0.starts_with("[canterlot]"), "{}", notes[0].0);
    assert!(notes[0].0.contains(&child.to_string()));
    let origin = notes[0].1.as_ref().unwrap();
    assert_eq!(origin["kind"], "event");
    assert_eq!(origin["from"], child.to_string());
    assert_eq!(origin["event"]["kind"], "approval", "{origin}");
    assert_eq!(origin["event"]["decision"], "allowed");
    assert_eq!(origin["event"]["request_id"], json!(request));
    assert_eq!(origin["event"]["child_id"], json!(child));

    f.tick().await;
    assert_eq!(f.sessions().await, 2, "the job does not repeat");
    assert_eq!(f.inputs(f.parent).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deny_notifies_parent_and_creates_nothing(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.decide(request, PermissionBehavior::Deny).await;
    f.tick().await;
    assert!(f.child().await.is_none());
    assert_eq!(f.request_state(request).await, "decision_applied");
    assert_eq!(f.closed_reason(request).await.as_deref(), Some("applied"));
    let notes = f.inputs(f.parent).await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].0.contains("denied"), "{}", notes[0].0);
    let origin = notes[0].1.as_ref().unwrap();
    assert_eq!(origin["event"]["kind"], "approval", "{origin}");
    assert_eq!(origin["event"]["decision"], "denied");
    assert_eq!(origin["event"]["request_id"], json!(request));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_allow_closes_as_not_applied_and_notifies_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    sqlx::query(
        "update machine_harnesses set models='[]' where machine_id=$1 and harness='claude_code'",
    )
    .bind(f.machine)
    .execute(&f.pool)
    .await
    .unwrap();
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    assert!(f.child().await.is_none());
    assert_eq!(f.request_state(request).await, "decision_not_applied");
    assert_eq!(
        f.closed_reason(request).await.as_deref(),
        Some("model_unknown")
    );
    let notes = f.inputs(f.parent).await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].0.contains("model_unknown"), "{}", notes[0].0);
    let origin = notes[0].1.as_ref().unwrap();
    assert_eq!(origin["event"]["kind"], "approval", "{origin}");
    assert_eq!(origin["event"]["decision"], "failed");
    assert_eq!(origin["event"]["detail"], "model_unknown");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn allow_after_the_parent_narrowed_never_runs_wider(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.set_modes(f.parent, "ask", json!({})).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    assert!(f.child().await.is_none());
    assert_eq!(f.request_state(request).await, "decision_not_applied");
    assert_eq!(
        f.closed_reason(request).await.as_deref(),
        Some("permission_mode_too_wide")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn second_decision_loses(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.decide(request, PermissionBehavior::Deny).await;
    assert!(matches!(
        f.decide_with(request, PermissionBehavior::Deny, "owner-key")
            .await,
        DecisionResult::IdempotentReplay { .. }
    ));
    assert!(matches!(
        f.decide_with(request, PermissionBehavior::Allow, "other-key")
            .await,
        DecisionResult::Conflict { .. }
    ));
    f.tick().await;
    assert!(matches!(
        f.decide_with(request, PermissionBehavior::Allow, "late-key")
            .await,
        DecisionResult::Conflict { .. }
    ));
    f.tick().await;
    assert!(f.child().await.is_none(), "the first decision wins");
    let jobs: i64 =
        sqlx::query_scalar("select count(*) from scheduled_jobs where payload->>'action'='spawn'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(jobs, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approval_lapses_with_parent_execution(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.stop_parent().await;
    assert_eq!(f.request_state(request).await, "closed_without_decision");
    f.tick().await;
    let notes = f.inputs(f.parent).await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].0.contains("lapsed"), "{}", notes[0].0);
    assert!(notes[0].0.contains(&request.to_string()));
    let origin = notes[0].1.as_ref().unwrap();
    assert_eq!(origin["event"]["kind"], "approval", "{origin}");
    assert_eq!(origin["event"]["decision"], "lapsed");
    assert_eq!(origin["event"]["request_id"], json!(request));
    assert!(matches!(
        f.decide(request, PermissionBehavior::Allow).await,
        DecisionResult::AlreadyClosed { .. }
    ));
    f.tick().await;
    assert!(f.child().await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn saved_decision_survives_the_end_of_the_parent_execution(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.stop_parent().await;
    assert_eq!(f.request_state(request).await, "decision_saved");
    f.tick().await;
    assert!(f.child().await.is_some(), "the owner's word stands");
    assert_eq!(f.request_state(request).await, "decision_applied");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_deliver_decision_command_for_spawn(pool: PgPool) {
    let f = Fx::new(pool).await;
    let request = f.pending(outside()).await;
    f.decide(request, PermissionBehavior::Allow).await;
    let commands: i64 =
        sqlx::query_scalar("select count(*) from commands where kind='deliver_decision'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(commands, 0);
    let job: (String, Option<Uuid>, Value) = sqlx::query_as(
        "select kind,conversation_id,payload from scheduled_jobs where state='pending'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(job.0, "session_task");
    assert_eq!(job.1, Some(f.parent));
    assert_eq!(job.2["action"], "spawn");
    assert_eq!(job.2["request_id"], request.to_string());
    let decided: i64 =
        sqlx::query_scalar("select count(*) from events where kind='request_decided'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(decided, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn child_inherits_parent_mode(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.set_modes(f.parent, "ask", json!({})).await;
    let (child, answer) = f.started(inside()).await;
    assert_eq!(answer["permission_mode"], "ask");
    assert_eq!(f.mode(child).await, "ask");
    assert_eq!(f.start_payload(child).await["permission_mode"], "ask");

    f.set_modes(f.parent, "bypass", json!({"permission_mode":"read_only"}))
        .await;
    let (child, answer) = f.started(inside()).await;
    assert_eq!(
        answer["permission_mode"], "read_only",
        "D11: pending narrows"
    );
    assert_eq!(f.mode(child).await, "read_only");

    f.set_modes(f.parent, "read_only", json!({"permission_mode":"bypass"}))
        .await;
    let (_, answer) = f.started(inside()).await;
    assert_eq!(
        answer["permission_mode"], "read_only",
        "D11: effective narrows"
    );

    f.set_modes(f.parent, "owner", json!({"permission_mode":"ask"}))
        .await;
    let (_, answer) = f.started(inside()).await;
    assert_eq!(answer["permission_mode"], "ask");

    f.set_modes(f.parent, "", json!({})).await;
    let (_, answer) = f.started(json!({"harness":"synthetic","brief":"b"})).await;
    assert_eq!(
        answer["permission_mode"], "owner",
        "an empty mode runs as owner"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn requested_wider_mode_is_refused(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.set_modes(f.parent, "ask", json!({})).await;
    for wider in ["bypass", "owner"] {
        let mut args = inside();
        args["permission_mode"] = json!(wider);
        assert_eq!(
            code(f.spawn(args).await),
            Code::PermissionModeTooWide,
            "{wider}"
        );
    }
    let mut args = inside();
    args["permission_mode"] = json!("read_only");
    let (_, answer) = f.started(args).await;
    assert_eq!(answer["permission_mode"], "read_only");

    f.set_modes(f.parent, "read_only", json!({})).await;
    let mut args = inside();
    args["permission_mode"] = json!("ask");
    assert_eq!(code(f.spawn(args).await), Code::PermissionModeTooWide);

    let mut args = inside();
    args["permission_mode"] = json!("everything");
    assert_eq!(code(f.spawn(args).await), Code::InvalidArguments);

    f.set_modes(f.parent, "bypass", json!({})).await;
    let mut args = outside();
    args["permission_mode"] = json!("owner");
    assert_eq!(
        code(f.spawn(args).await),
        Code::PermissionModeTooWide,
        "the mode comes before the whitelist: no card"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_mode_across_harness_becomes_ask(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.set_modes(f.parent, "owner", json!({})).await;
    let (_, across) = f.started(inside()).await;
    assert_eq!(across["permission_mode"], "ask");

    let (_, same) = f.started(json!({"harness":"synthetic","brief":"b"})).await;
    assert_eq!(same["permission_mode"], "owner");

    let mut args = inside();
    args["permission_mode"] = json!("owner");
    assert_eq!(code(f.spawn(args).await), Code::PermissionModeTooWide);
    let mut args = inside();
    args["permission_mode"] = json!("bypass");
    assert_eq!(code(f.spawn(args).await), Code::PermissionModeTooWide);
    let args = json!({"harness":"synthetic","brief":"b","permission_mode":"bypass"});
    assert_eq!(
        code(f.spawn(args).await),
        Code::PermissionModeTooWide,
        "owner and bypass are not comparable"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn codex_default_resolves_to_first_catalog_model(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (first, _) = f.started(json!({"harness":"codex","brief":"b"})).await;
    let (named, _) = f
        .started(json!({"harness":"codex","model":"default","brief":"b"}))
        .await;
    for child in [first, named] {
        let model: String = sqlx::query_scalar("select model from conversations where id=$1")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(model, "gpt-first");
    }
    let second = f
        .spawn(json!({"harness":"codex","model":"gpt-second","brief":"b"}))
        .await
        .unwrap();
    assert_eq!(
        second["status"], "pending_approval",
        "`default` covers only the first model"
    );
    let first_model = f
        .spawn(json!({"harness":"claude_code","brief":"b"}))
        .await
        .unwrap();
    assert_eq!(first_model["status"], "pending_approval");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_depth_limit(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update conversations set spawn_depth=2 where id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    let (child, _) = f.started(inside()).await;
    let depth: i32 = sqlx::query_scalar("select spawn_depth from conversations where id=$1")
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(depth, 3);
    sqlx::query("update conversations set spawn_depth=3 where id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(code(f.spawn(inside()).await), Code::SpawnDepthExceeded);
    assert_eq!(
        code(f.spawn(outside()).await),
        Code::SpawnDepthExceeded,
        "depth comes first: no card"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_limits_count_children_and_cards(pool: PgPool) {
    let config = SessionToolsConfig {
        parent_spawns: 3,
        ..Default::default()
    };
    let f = Fx::with(pool, config, &CAPABILITIES).await;
    f.started(inside()).await;
    let denied = f.pending(outside()).await;
    f.decide(denied, PermissionBehavior::Deny).await;
    f.tick().await;
    let allowed = f.pending(outside()).await;
    f.decide(allowed, PermissionBehavior::Allow).await;
    f.tick().await;
    assert_eq!(f.request_state(allowed).await, "decision_applied");
    assert_eq!(code(f.spawn(inside()).await), Code::RateLimited);
    assert_eq!(code(f.spawn(outside()).await), Code::RateLimited);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_brief_stops_a_deep_chain(pool: PgPool) {
    let config = SessionToolsConfig {
        max_chain_depth: 1,
        ..Default::default()
    };
    let f = Fx::with(pool, config, &CAPABILITIES).await;
    let sender = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    sqlx::query("update conversations set model='small' where id=$1")
        .bind(sender)
        .execute(&f.pool)
        .await
        .unwrap();
    let caller = Caller {
        conversation_id: sender,
        ..f.caller()
    };
    session_tools::call(
        &f.context,
        &caller,
        "sessions.send",
        json!({"session_id":f.parent,"text":"сообщение"}),
    )
    .await
    .unwrap();
    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(code(f.spawn(inside()).await), Code::ChainDepthExceeded);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn transport_retry_starts_nothing_twice(pool: PgPool) {
    let f = Fx::new(pool).await;
    let caller = f.caller();
    let first = session_tools::call(&f.context, &caller, "sessions.spawn", inside())
        .await
        .unwrap();
    let again = session_tools::call(&f.context, &caller, "sessions.spawn", inside())
        .await
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(f.sessions().await, 2);

    let caller = f.caller();
    let card = session_tools::call(&f.context, &caller, "sessions.spawn", outside())
        .await
        .unwrap();
    let again = session_tools::call(&f.context, &caller, "sessions.spawn", outside())
        .await
        .unwrap();
    assert_eq!(card, again);
    let cards: i64 = sqlx::query_scalar("select count(*) from requests")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(cards, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn isolated_spawn_needs_capability(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig::default(),
        &["pilot_v1", "instructions_v1"],
    )
    .await;
    let mut args = inside();
    args["isolated"] = json!(true);
    assert_eq!(code(f.spawn(args).await), Code::CapabilityMissing);
    let mut args = outside();
    args["isolated"] = json!(true);
    assert_eq!(
        code(f.spawn(args).await),
        Code::CapabilityMissing,
        "no card"
    );
    assert_eq!(f.sessions().await, 1);
    f.started(inside()).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn isolated_spawn_needs_a_worktree_root(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update machines set worktree_root=null where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut args = inside();
    args["isolated"] = json!(true);
    assert_eq!(code(f.spawn(args).await), Code::CapabilityMissing);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn isolated_path_is_not_remembered(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut args = inside();
    args["isolated"] = json!(true);
    args["base"] = json!("main");
    let (child, answer) = f.started(args).await;

    let expected = format!("/data/worktrees/{child}");
    assert_eq!(answer["worktree"]["path"], expected.as_str());
    let branch = answer["worktree"]["branch"].as_str().unwrap().to_owned();
    assert!(branch.starts_with("canterlot/"), "{branch}");

    let (workdir, worktree): (String, Value) =
        sqlx::query_as("select workdir,worktree from conversations where id=$1")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(workdir, expected);
    assert_eq!(worktree["source_workdir"], "/tmp/w");
    assert_eq!(worktree["path"], expected.as_str());
    assert_eq!(worktree["branch"], branch.as_str());
    assert_eq!(worktree["base"], "main");
    assert_eq!(worktree["state"], "active");

    let start = f.start_payload(child).await;
    assert_eq!(start["workdir"], expected.as_str());
    assert_eq!(
        start["worktree"],
        json!({"source_workdir":"/tmp/w","branch":branch,"base":"main"})
    );
    let remembered: Vec<String> =
        sqlx::query_scalar("select workdir from machine_workdirs where machine_id=$1")
            .bind(f.machine)
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert!(remembered.is_empty(), "{remembered:?}");

    let text = &f.inputs(child).await[0].0;
    let (prefix, _) = text.split_once("\n\n").unwrap();
    assert!(prefix.contains(&format!("on branch {branch}")), "{prefix}");
    assert!(!prefix.contains('\n'));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn plain_spawn_is_remembered_like_any_session(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.started(inside()).await;
    let remembered: Vec<String> =
        sqlx::query_scalar("select workdir from machine_workdirs where machine_id=$1")
            .bind(f.machine)
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(remembered, ["/tmp/w"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn branches_of_quick_spawns_differ(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut branches = BTreeSet::new();
    for _ in 0..5 {
        let mut args = inside();
        args["isolated"] = json!(true);
        let (_, answer) = f.started(args).await;
        branches.insert(answer["worktree"]["branch"].as_str().unwrap().to_owned());
    }
    assert_eq!(branches.len(), 5, "{branches:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn isolated_parent_spawns_from_its_source_and_branch(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query(
        "update conversations set workdir='/data/worktrees/x',
           worktree='{\"source_workdir\":\"/tmp/w\",\"branch\":\"canterlot/aaaa\",\"path\":\"/data/worktrees/x\",\"base\":\"HEAD\",\"state\":\"active\"}'
         where id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    let mut args = inside();
    args["isolated"] = json!(true);
    let (child, _) = f.started(args).await;
    let start = f.start_payload(child).await;
    assert_eq!(start["worktree"]["source_workdir"], "/tmp/w");
    assert_eq!(start["worktree"]["base"], "canterlot/aaaa");
    assert_eq!(code(f.spawn(inside()).await), Code::WorkdirNotAllowed);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_error_in_a_worktree_names_both_choices(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query(
        "update conversations set workdir='/data/worktrees/x',
           worktree='{\"source_workdir\":\"/tmp/w\",\"branch\":\"canterlot/aaaa\",\"path\":\"/data/worktrees/x\",\"base\":\"HEAD\",\"state\":\"active\"}'
         where id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    let error = f.spawn(inside()).await.unwrap_err();
    assert_eq!(error.code, Code::WorkdirNotAllowed);
    assert_eq!(
        error.message,
        "This session works in a git worktree: pass \"isolated\": true for a new worktree, or \"workdir\": \"/data/worktrees/x\" to share it"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn card_answers_after_the_caller_stopped_are_refused(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.stop_parent().await;
    assert_eq!(code(f.spawn(outside()).await), Code::ConversationClosed);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_deleted_child_leaves_no_name_in_the_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut args = outside();
    args["title"] = json!("секретное название");
    args["subscribe"] = json!(false);
    let request = f.pending(args).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    let child = f.child().await.unwrap();
    let execution: Uuid = sqlx::query_scalar("select id from executions where conversation_id=$1")
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    executions::set_state(
        &f.pool,
        execution,
        ExecutionState::Failed,
        Some("start_rejected"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_, blobs) = testkit::test_blob_store(dir.path());
    canterlot_hub::deletion::delete(&f.pool, &*blobs, f.owner, child, "delete", Uuid::now_v7())
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar(
        "select (select count(*) from inputs i where i::text like '%'||$1||'%')
              + (select count(*) from scheduled_jobs j where j::text like '%'||$1||'%')",
    )
    .bind(child.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1, "the note stays as a neutral line");
    assert!(!notes[0].contains(&child.to_string()));
}

#[test]
fn spawn_spec_debug_hides_the_brief() {
    let spec: session_tools::spawn::SpawnSpec = serde_json::from_value(json!({
        "harness":"codex","model":"m","effort":"e","title":"секретное название","workdir":"/tmp/секрет",
        "isolated":false,"base":null,"preset":"clean","permission_mode":"ask",
        "brief":"секретный бриф","subscribe":true,"chain_depth":0
    }))
    .unwrap();
    let printed = format!("{spec:?}");
    assert!(!printed.contains("секрет"), "{printed}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_job_that_exhausts_attempts_closes_the_request(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Закрывающее задание не вставляется: задание запуска падает на каждой попытке.
    sqlx::query(
        "create function block_close() returns trigger language plpgsql as $$
         begin
           if new.payload->>'action' = 'spawn_close' then raise exception 'blocked'; end if;
           return new;
         end $$",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger block_close before insert on scheduled_jobs
         for each row execute function block_close()",
    )
    .execute(&fx.pool)
    .await
    .unwrap();
    let request = fx.pending(outside()).await;
    fx.decide(request, PermissionBehavior::Allow).await;
    assert_eq!(fx.request_state(request).await, "decision_saved");

    let (dispatcher, _) = Dispatcher::channel();
    let flaky = Scheduler::new(
        fx.pool.clone(),
        dispatcher,
        SchedulerConfig {
            max_attempts: 2,
            retry_queue_full: std::time::Duration::from_secs(1),
            ..Default::default()
        },
    )
    .with_limits(fx.context.registry.clone(), LimitsConfig::default());
    flaky.tick(Utc::now()).await.unwrap();
    assert_eq!(fx.request_state(request).await, "decision_saved");
    flaky
        .tick(Utc::now() + chrono::TimeDelta::hours(1))
        .await
        .unwrap();

    assert_eq!(fx.request_state(request).await, "decision_not_applied");
    assert_eq!(
        fx.closed_reason(request).await.as_deref(),
        Some("job_failed")
    );
    assert!(fx.child().await.is_none(), "nothing was started");
    fx.tick().await;
    let notes = fx.notes().await;
    assert!(
        notes.iter().any(|note| note.contains("could not start")),
        "{notes:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delegated_start_carries_the_isolation_flag_on_every_start(pool: PgPool) {
    let f = Fx::new(pool).await;
    // Владелец запустил родителя: у него флага нет.
    let root: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(f.parent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(root.get("isolate_mcp").is_none(), "{root}");

    let (child, _) = f.started(inside()).await;
    assert_eq!(f.start_payload(child).await["isolate_mcp"], true);

    // Возобновление: новое исполнение той же сессии, и после потери родителя.
    let execution: Uuid = sqlx::query_scalar("select id from executions where conversation_id=$1")
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    executions::set_state(
        &f.pool,
        execution,
        ExecutionState::Failed,
        Some("start_rejected"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sqlx::query("update conversations set parent_conversation_id=null where id=$1")
        .bind(child)
        .execute(&f.pool)
        .await
        .unwrap();
    executions::start(&f.pool, f.owner, child, Uuid::now_v7())
        .await
        .unwrap();
    let flags: Vec<Option<bool>> = sqlx::query_scalar(
        "select (payload->'isolate_mcp')::boolean from commands
         where conversation_id=$1 and kind='start_execution' order by execution_generation",
    )
    .bind(child)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(flags, vec![Some(true), Some(true)]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn brief_carries_the_callers_attachments(pool: PgPool) {
    let f = Fx::new(pool).await;
    let file = f.attachment("brief.pdf", "application/pdf").await;
    let (child, _) = f
        .started(json!({
            "harness": "synthetic", "model": "small", "brief": "Прочитай файл",
            "attachment_ids": [file],
        }))
        .await;
    let content: Value = sqlx::query_scalar(
        "select content from inputs where conversation_id=$1 and origin is not null",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let blocks = content.as_array().unwrap();
    assert!(
        blocks.iter().any(
            |block| block["type"] == "attachment" && block["attachment_id"] == file.to_string()
        )
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_attachment_is_refused_for_spawn(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
         values($1,$2,$3,'secret.pdf','application/pdf',4,$4,'ready',clock_timestamp())",
    )
    .bind(id)
    .bind(f.owner)
    .bind(other)
    .bind("0".repeat(64))
    .execute(&f.pool)
    .await
    .unwrap();
    let error = f
        .spawn(json!({
            "harness": "synthetic", "model": "small", "brief": "чужой файл",
            "attachment_ids": [id],
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::AttachmentNotAvailable);
    assert!(f.child().await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn codex_ask_from_an_ask_parent_needs_the_owner(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.set_modes(f.parent, "ask", json!({})).await;
    let before = f.sessions().await;
    let request = f
        .pending(json!({"harness":"codex","brief":"b","permission_mode":"ask"}))
        .await;
    assert_eq!(f.sessions().await, before, "whitelisted, but no child yet");
    let payload: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
        .bind(request)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(payload["permission_mode"], "ask");
    assert_eq!(payload["wider_than_parent"], true, "{payload}");

    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    let child = f.child().await.expect("the owner allowed it");
    assert_eq!(f.mode(child).await, "ask");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn codex_child_without_a_mode_gets_the_strictest_equivalent(pool: PgPool) {
    let f = Fx::new(pool).await;
    for parent in ["ask", "owner"] {
        f.set_modes(f.parent, parent, json!({})).await;
        let (child, answer) = f.started(json!({"harness":"codex","brief":"b"})).await;
        assert_eq!(answer["permission_mode"], "read_only", "{parent}");
        assert_eq!(f.mode(child).await, "read_only", "{parent}");
    }
    f.set_modes(f.parent, "bypass", json!({})).await;
    let (_, answer) = f.started(json!({"harness":"codex","brief":"b"})).await;
    assert_eq!(answer["permission_mode"], "bypass");
    let (_, answer) = f
        .started(json!({"harness":"codex","brief":"b","permission_mode":"ask"}))
        .await;
    assert_eq!(
        answer["permission_mode"], "ask",
        "a bypass parent needs no card"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approved_wider_child_is_rechecked_against_a_narrowed_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.set_modes(f.parent, "ask", json!({})).await;
    let request = f
        .pending(json!({"harness":"codex","brief":"b","permission_mode":"ask"}))
        .await;
    f.set_modes(f.parent, "read_only", json!({})).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    assert!(f.child().await.is_none());
    assert_eq!(
        f.closed_reason(request).await.as_deref(),
        Some("permission_mode_too_wide")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn child_cannot_start_with_the_owner_preset(pool: PgPool) {
    let f = Fx::new(pool).await;
    let mut args = inside();
    args["preset"] = json!("owner");
    assert_eq!(code(f.spawn(args).await), Code::ProfileUnknown);
    assert!(f.child().await.is_none());
}
