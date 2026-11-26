//! Удалённый запуск (`session_spawn {machine}`) и выключатель удалённой работы.
//!
//! Две машины в одном хабе: A (родитель) и B (`machine-b`), у каждой свой фальшивый сеанс
//! в реестре. Команды воркерам видны по таблице `commands`: у неё есть `machine_id`.

use std::{sync::Arc, time::Duration};

use axum::http::Method;
use canterlot_hub::{
    activity::Thresholds,
    config::{ActivityConfig, LimitsConfig, SchedulerConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions, http, machines,
    requests::{self, DecisionResult},
    scheduler::{ClaimedJob, JobKind, Scheduler},
    session::Registry,
    session_tools::{
        self, Caller, TOOLS, ToolContext, ToolError, ToolErrorCode as Code, jobs,
        settle::SettleConfig, spawn_queue,
    },
    testkit::{self, FakeSession, TestClient},
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const INSIDE: &str = "haiku";
const OUTSIDE: &str = "opus[1m]";
const B_ONLY: &str = "b-model";
const CAPABILITIES: [&str; 3] = ["pilot_v1", "instructions_v1", "session_tools_v1"];
const REMOTE: [&str; 4] = [
    "pilot_v1",
    "instructions_v1",
    "session_tools_v1",
    "remote_spawn_v1",
];

struct Fx {
    pool: PgPool,
    context: ToolContext,
    scheduler: Scheduler,
    owner: Uuid,
    a: Uuid,
    b: Uuid,
    parent: Uuid,
    execution: Uuid,
    _a: FakeSession,
    _b: Option<FakeSession>,
}

async fn catalog(pool: &PgPool, machine: Uuid, harness: &str, models: Value) {
    sqlx::query(
        "insert into machine_harnesses(machine_id,harness,provider,title,images,settings_midsession,
                                       permission_modes,models,settings_midsession_effort)
         values($1,$2,'claude',$2,true,true,'[\"owner\",\"ask\",\"bypass\",\"read_only\"]',$3,true)",
    )
    .bind(machine)
    .bind(harness)
    .bind(models)
    .execute(pool)
    .await
    .unwrap();
}

fn model(id: &str, efforts: &[&str]) -> Value {
    json!({"id": id, "title": id, "efforts": efforts})
}

fn remote_args() -> Value {
    json!({
        "machine": "machine-b",
        "harness": "claude_code",
        "model": INSIDE,
        "brief": "do it",
        "workdir": "/tmp/b/p",
    })
}

fn code(result: Result<Value, ToolError>) -> Code {
    result.expect_err("the call must be refused").code
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        Self::with(pool, true).await
    }

    async fn with(pool: PgPool, b_online: bool) -> Self {
        let seed = testkit::seed(&pool).await;
        let registry = Registry::new(Default::default());
        let session_a = testkit::fake_session(&registry, seed.machine_id, &CAPABILITIES, true);
        sqlx::query(
            "update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true,
               allowed_roots='[\"/tmp\"]',worktree_root='/data/worktrees' where id=$1",
        )
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
        let b = Uuid::now_v7();
        sqlx::query(
            "insert into machines(id,owner_id,name,availability,capabilities,limits_ok,allowed_roots,worktree_root)
             values($1,$2,'machine-b','available',array['pilot_v1'],true,'[\"/tmp/b\"]','/data/b-worktrees')",
        )
        .bind(b)
        .bind(seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
        catalog(
            &pool,
            seed.machine_id,
            "claude_code",
            json!([model(OUTSIDE, &["low", "high"]), model(INSIDE, &["none"])]),
        )
        .await;
        catalog(
            &pool,
            b,
            "claude_code",
            json!([
                model(INSIDE, &["none"]),
                model(B_ONLY, &["low"]),
                model(OUTSIDE, &["low", "high"])
            ]),
        )
        .await;
        sqlx::query(
            "update conversations set title='Родитель',model='small',effort='low',harness='claude_code',
               permission_mode='bypass',workdir='/tmp/w',memory_domain='personal' where id=$1",
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
            .bind(json!([{"harness":"claude_code","model":INSIDE}]))
            .execute(&pool)
            .await
            .unwrap();
        let b_session = b_online.then(|| testkit::fake_session(&registry, b, &REMOTE, true));
        let (dispatcher, _) = Dispatcher::channel();
        Self {
            context: ToolContext {
                memory: Default::default(),
                pool: pool.clone(),
                config: Arc::new(SessionToolsConfig {
                    spawn_stagger: Duration::ZERO,
                    ..Default::default()
                }),
                limits: Default::default(),
                settle: SettleConfig {
                    idle_quiet: Duration::from_secs(5),
                    idle_background_max: Duration::from_secs(15 * 60),
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
                cloud: None,
                registry: registry.clone(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            scheduler: Scheduler::new(pool.clone(), dispatcher, SchedulerConfig::default())
                .with_limits(registry, LimitsConfig::default()),
            pool,
            owner: seed.owner_id,
            a: seed.machine_id,
            b,
            parent: seed.conversation_id,
            execution: started.execution_id,
            _a: session_a,
            _b: b_session,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.parent,
            execution_id: self.execution,
            machine_id: self.a,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }

    async fn spawn(&self, args: Value) -> Result<Value, ToolError> {
        self.call("sessions.spawn", args).await
    }

    async fn switch(&self, on: bool) {
        sqlx::query("update machines set accepts_remote_work=$2 where id=$1")
            .bind(self.b)
            .bind(on)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn started(&self, args: Value) -> (Uuid, Value) {
        let answer = self.spawn(args).await.unwrap();
        assert_eq!(answer["status"], "started", "{answer}");
        (
            serde_json::from_value(answer["session_id"].clone()).unwrap(),
            answer,
        )
    }

    async fn children(&self) -> Vec<(Uuid, Uuid)> {
        sqlx::query_as(
            "select id,machine_id from conversations where parent_conversation_id=$1 order by created_at",
        )
        .bind(self.parent)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// Машины, которым адресованы команды запуска потомков.
    async fn start_machines(&self, child: Uuid) -> Vec<Uuid> {
        sqlx::query_scalar(
            "select machine_id from commands where conversation_id=$1 and kind='start_execution'",
        )
        .bind(child)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn notes(&self) -> Vec<String> {
        sqlx::query_scalar::<_, Value>(
            "select content from inputs where conversation_id=$1 order by input_seq",
        )
        .bind(self.parent)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|content| content[0]["text"].as_str().unwrap().to_owned())
        .collect()
    }

    /// Заметки очереди родителю: до доставки они лежат заданиями.
    async fn queue_notes(&self) -> Vec<String> {
        sqlx::query_scalar(
            "select payload->>'text' from scheduled_jobs
              where conversation_id=$1 and payload->>'action'='note'",
        )
        .bind(self.parent)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn tick(&self) {
        self.scheduler.tick(Utc::now()).await.unwrap();
    }

    /// Задание `spawn_dequeue` родителя: так очередь идёт у планировщика, но без ожидания срока.
    async fn dequeue_with(&self, registry: &Registry) {
        let job = ClaimedJob {
            id: Uuid::now_v7(),
            kind: JobKind::SessionTask,
            owner_id: self.owner,
            conversation_id: Some(self.parent),
            run_at: Utc::now(),
            payload: json!({"action": "spawn_dequeue"}),
            attempts: 0,
        };
        let runtime = spawn_queue::Runtime {
            registry: Some(registry),
            session_tools: &self.context.config,
            spawn_queue: &self.context.spawn_queue,
            guards: &self.context.guards,
            routing_enabled: self.context.routing_enabled,
            routing_stale: self.context.routing_stale,
            routing_queue_horizon: self.context.routing_queue_horizon,
        };
        let thresholds = Thresholds::from(&ActivityConfig::default());
        let mut tx = self.pool.begin().await.unwrap();
        jobs::run_in_tx(
            &mut tx,
            &job,
            Utc::now(),
            Uuid::now_v7(),
            Some(registry),
            &self.context.settle,
            &thresholds,
            Some(&runtime),
            None,
            &LimitsConfig::default(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn dequeue(&self) {
        self.dequeue_with(&self.context.registry).await;
    }

    async fn decide(&self, request: Uuid, behavior: PermissionBehavior) -> DecisionResult {
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
            "owner-key",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        result
    }
}

// ---------------------------------------------------------------------------
// Выключатель
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_switch_is_off_by_default_and_the_helper_reads_it() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    assert!(
        !machines::accepts_remote_work_for_share(&mut tx, f.b)
            .await
            .unwrap()
    );
    assert!(
        !machines::accepts_remote_work_for_share(&mut tx, Uuid::now_v7())
            .await
            .unwrap(),
        "нет машины: не принимает"
    );
    tx.commit().await.unwrap();
    f.switch(true).await;
    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    assert!(
        machines::accepts_remote_work_for_share(&mut tx, f.b)
            .await
            .unwrap()
    );
}

/// Допуск — одна точка: пока транзакция запуска держит строку машины `for share`, смена
/// выключателя (`for update`) ждёт её коммита, а после него видит выключатель сама.
#[tokio::test]
async fn switching_off_waits_for_a_launch_that_already_read_the_row() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut launch = canterlot_hub::db::begin(&f.pool).await.unwrap();
    assert!(
        machines::accepts_remote_work_for_share(&mut launch, f.b)
            .await
            .unwrap()
    );
    let pool = f.pool.clone();
    let (owner, machine) = (f.owner, f.b);
    let switch = tokio::spawn(async move {
        let mut tx = canterlot_hub::db::begin(&pool).await.unwrap();
        machines::set_accepts_remote_work(&mut tx, owner, machine, false)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    // Переключатель дошёл до блокировки строки и ждёт её: это видно в самой базе, без пауз.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity
                  where datname=current_database() and wait_event_type='Lock'",
            )
            .fetch_one(&f.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the switch waits on the machine row");
    assert!(!switch.is_finished(), "the switch must wait for the launch");
    launch.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), switch)
        .await
        .expect("the switch proceeds after the launch commits")
        .unwrap();
    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    assert!(
        !machines::accepts_remote_work_for_share(&mut tx, f.b)
            .await
            .unwrap()
    );
}

struct Api {
    client: TestClient,
    anonymous: TestClient,
    owner: Uuid,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let router = http::router(http::AppState::new(deps));
        Self {
            client: TestClient::in_process(router.clone()).as_owner(),
            anonymous: TestClient::in_process(router),
            owner,
            _dir: dir,
        }
    }

    async fn put(&self, machine: Uuid, accepts: Value) -> testkit::TestResponse {
        self.client
            .send(
                Method::PUT,
                &format!("/api/v1/machines/{machine}/remote-work"),
                Some(&Uuid::now_v7().to_string()),
                Some(json!({"accepts_remote_work": accepts})),
            )
            .await
    }
}

#[tokio::test]
async fn the_owner_flips_the_switch_over_http_and_only_the_owner_can() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool.clone()).await;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'machine-b')")
        .bind(machine)
        .bind(api.owner)
        .execute(&pool)
        .await
        .unwrap();
    let view = |client: &TestClient| {
        let path = format!("/api/v1/machines/{machine}");
        let client = client.clone();
        async move {
            client
                .send(Method::GET, &path, None, None)
                .await
                .json::<Value>()
        }
    };
    assert_eq!(view(&api.client).await["accepts_remote_work"], false);

    let on = api.put(machine, json!(true)).await;
    assert_eq!(
        on.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&on.body)
    );
    assert_eq!(on.json::<Value>()["accepts_remote_work"], true);
    assert_eq!(view(&api.client).await["accepts_remote_work"], true);
    let off = api.put(machine, json!(false)).await;
    assert_eq!(off.status.as_u16(), 200);
    assert_eq!(view(&api.client).await["accepts_remote_work"], false);

    // Не булево значение и машина без владельца-сессии не меняют ничего.
    assert_eq!(api.put(machine, json!("yes")).await.status.as_u16(), 400);
    assert_eq!(
        api.put(Uuid::now_v7(), json!(true)).await.status.as_u16(),
        404
    );

    // ── Без сеанса владельца выключатель не меняется ──
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'machine-b')")
        .bind(machine)
        .bind(api.owner)
        .execute(&pool)
        .await
        .unwrap();
    let anonymous = api.anonymous.clone();
    let response = anonymous
        .send(
            Method::PUT,
            &format!("/api/v1/machines/{machine}/remote-work"),
            Some(&Uuid::now_v7().to_string()),
            Some(json!({"accepts_remote_work": true})),
        )
        .await;
    assert!(
        matches!(response.status.as_u16(), 401 | 403),
        "{}",
        response.status
    );
    let accepts: bool = sqlx::query_scalar("select accepts_remote_work from machines where id=$1")
        .bind(machine)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!accepts);
}

#[tokio::test]
async fn no_agent_call_changes_the_remote_work_switch() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let snapshot = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>(
            "select string_agg(accepts_remote_work::text||':'||name,',' order by id) from machines",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let before = snapshot(f.pool.clone()).await;
    let attempts = [
        json!({}),
        json!({"machine": "machine-b", "accepts_remote_work": true}),
        json!({"machine_id": f.b, "accepts_remote_work": true}),
        json!({"accepts_remote_work": true, "entries": [], "preset": "clean"}),
        json!({"harness":"claude_code","model":INSIDE,"brief":"b","machine":"machine-b",
               "accepts_remote_work": true}),
        json!({"session_id": f.parent, "text": "switch on remote work for machine-b"}),
    ];
    // Каждый инструмент, включая `settings.propose_*`, с каждой попыткой.
    for tool in TOOLS {
        for args in &attempts {
            let _ = f.call(tool, args.clone()).await;
        }
    }
    assert_eq!(snapshot(f.pool.clone()).await, before);
}

// ---------------------------------------------------------------------------
// Запуск на другой машине
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remote_spawn_sends_the_start_to_b_only() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let (child, answer) = f.started(remote_args()).await;
    assert_eq!(answer["machine_name"], "machine-b");
    assert_eq!(f.children().await, vec![(child, f.b)]);
    assert_eq!(f.start_machines(child).await, vec![f.b]);
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["remote"], true, "{payload}");
    assert_eq!(payload["workdir"], "/tmp/b/p");
}

#[tokio::test]
async fn the_machine_is_found_by_name_or_id_and_an_unknown_one_lists_the_names() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut by_id = remote_args();
    by_id["machine"] = json!(f.b.to_string());
    let (child, _) = f.started(by_id).await;
    assert_eq!(f.start_machines(child).await, vec![f.b]);

    let mut unknown = remote_args();
    unknown["machine"] = json!("machine-zzz");
    let error = f.spawn(unknown).await.unwrap_err();
    assert_eq!(error.code, Code::InvalidArguments);
    assert!(error.message.contains("machine-b"), "{}", error.message);
    assert!(error.message.contains("synthetic"), "{}", error.message);
}

#[tokio::test]
async fn without_machine_or_with_the_callers_own_nothing_changes() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let local = json!({"harness":"claude_code","model":INSIDE,"brief":"x","workdir":"/tmp/w2"});
    let (plain, plain_answer) = f.started(local.clone()).await;
    let mut own_name = local.clone();
    own_name["machine"] = json!("synthetic");
    let (named, named_answer) = f.started(own_name).await;
    let mut own_id = local;
    own_id["machine"] = json!(f.a.to_string());
    let (by_id, _) = f.started(own_id).await;
    for child in [plain, named, by_id] {
        assert_eq!(f.start_machines(child).await, vec![f.a]);
        let payload: Value = sqlx::query_scalar(
            "select payload from commands where conversation_id=$1 and kind='start_execution'",
        )
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert!(payload.get("remote").is_none(), "{payload}");
    }
    // Ответ тот же, что без аргумента: имени машины в нём нет.
    assert_eq!(
        plain_answer.as_object().unwrap().keys().collect::<Vec<_>>(),
        named_answer.as_object().unwrap().keys().collect::<Vec<_>>()
    );
    assert!(plain_answer.get("machine_name").is_none());
}

#[tokio::test]
async fn remote_spawn_refuses_a_bad_workdir_domain_or_domainless_caller() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut none = remote_args();
    none.as_object_mut().unwrap().remove("workdir");
    assert_eq!(code(f.spawn(none).await), Code::WorkdirNotAllowed);
    let mut outside = remote_args();
    outside["workdir"] = json!("/tmp/w");
    assert_eq!(
        code(f.spawn(outside).await),
        Code::WorkdirNotAllowed,
        "/tmp/w лежит в корнях A, но не в корнях B"
    );
    assert!(f.children().await.is_empty());

    // ── Домен проверяется по рабочему каталогу цели ──
    sqlx::query(
        "update machines set capabilities=array['pilot_v1','memory_scope_v1'],
           memory_work_roots='[\"/tmp/b/work\"]' where id=$1",
    )
    .bind(f.b)
    .execute(&f.pool)
    .await
    .unwrap();
    let mut work = remote_args();
    work["workdir"] = json!("/tmp/b/work/p");
    assert_eq!(code(f.spawn(work).await), Code::DomainMismatch);
    // Домен родителя — personal: каталог B вне рабочих корней подходит.
    let (child, _) = f.started(remote_args()).await;
    let domain: Option<String> =
        sqlx::query_scalar("select memory_domain from conversations where id=$1")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(domain.as_deref(), Some("personal"));

    // ── Без домена у вызывающего удалённый запуск запрещён ──
    let children = f.children().await.len();
    sqlx::query("update conversations set memory_domain=null where id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(code(f.spawn(remote_args()).await), Code::DomainMismatch);
    assert_eq!(f.children().await.len(), children, "nothing was created");
}

#[tokio::test]
async fn a_remote_child_never_inherits_the_owner_mode_and_cannot_ask_for_it() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    sqlx::query("update conversations set permission_mode='owner' where id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    // Локально тот же харнесс наследует `owner`.
    let local = json!({"harness":"claude_code","model":INSIDE,"brief":"x","workdir":"/tmp/w2"});
    let (here, _) = f.started(local).await;
    let mode = |child: Uuid| {
        let pool = f.pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("select permission_mode from conversations where id=$1")
                .bind(child)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(mode(here).await, "owner");
    let (there, _) = f.started(remote_args()).await;
    assert_eq!(mode(there).await, "ask");
    let mut explicit = remote_args();
    explicit["permission_mode"] = json!("owner");
    assert_eq!(
        code(f.spawn(explicit).await),
        Code::PermissionModeTooWide,
        "явный owner на другой машине отказывается"
    );
}

#[tokio::test]
async fn the_catalog_comes_from_the_target_machine() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut remote = remote_args();
    remote["model"] = json!(B_ONLY);
    // `b-model` есть только у B; вне белого списка — карточка, но каталог B её допускает.
    let answer = f.spawn(remote).await.unwrap();
    assert_eq!(answer["status"], "pending_approval", "{answer}");
    let local = json!({"harness":"claude_code","model":B_ONLY,"brief":"x","workdir":"/tmp/w2"});
    assert_eq!(code(f.spawn(local).await), Code::ModelUnknown);
}

#[tokio::test]
async fn switch_off_refuses_a_fresh_remote_spawn_and_creates_nothing() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    assert_eq!(
        code(f.spawn(remote_args()).await),
        Code::RemoteWorkDisabled,
        "выключатель по умолчанию выключен"
    );
    assert!(f.children().await.is_empty());
    // Свои сессии машины выключатель не затрагивает: локальный запуск идёт.
    let local = json!({"harness":"claude_code","model":INSIDE,"brief":"x","workdir":"/tmp/w2"});
    f.started(local).await;
}

#[tokio::test]
async fn an_approved_remote_spawn_obeys_the_switch_and_starts_on_b() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut outside = remote_args();
    outside["model"] = json!(OUTSIDE);
    let answer = f.spawn(outside).await.unwrap();
    assert_eq!(answer["status"], "pending_approval", "{answer}");
    let request: Uuid = serde_json::from_value(answer["request_id"].clone()).unwrap();
    let stored: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
        .bind(request)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        stored["machine_id"],
        f.b.to_string(),
        "карточка хранит машину"
    );

    f.switch(false).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    assert!(
        f.children().await.is_empty(),
        "после выключения запуска нет"
    );
    let notes = f.notes().await;
    assert!(
        notes
            .iter()
            .any(|note| note.contains("remote_work_disabled")),
        "{notes:?}"
    );

    // ── Одобренная карточка при включённом выключателе стартует на B ──
    f.switch(true).await;
    let mut outside = remote_args();
    outside["model"] = json!(OUTSIDE);
    let answer = f.spawn(outside).await.unwrap();
    let request: Uuid = serde_json::from_value(answer["request_id"].clone()).unwrap();
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    let children = f.children().await;
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].1, f.b);
    assert_eq!(f.start_machines(children[0].0).await, vec![f.b]);
}

#[tokio::test]
async fn a_replayed_remote_call_returns_the_same_answer_and_starts_nothing_twice() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let caller = f.caller();
    let first = session_tools::call(&f.context, &caller, "sessions.spawn", remote_args())
        .await
        .unwrap();
    let again = session_tools::call(&f.context, &caller, "sessions.spawn", remote_args())
        .await
        .unwrap();
    assert_eq!(first["session_id"], again["session_id"]);
    assert_eq!(again["machine_name"], "machine-b");
    assert_eq!(f.children().await.len(), 1);
}

#[tokio::test]
async fn an_offline_target_queues_and_starts_after_it_returns() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::with(pool, false).await;
    f.switch(true).await;
    let answer = f.spawn(remote_args()).await.unwrap();
    assert_eq!(answer["status"], "queued", "{answer}");
    let (reason, spec): (String, Value) =
        sqlx::query_as("select reason,spec from spawn_queue where parent_id=$1")
            .bind(f.parent)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(reason, "machine_offline");
    assert_eq!(spec["args"]["machine_id"], f.b.to_string(), "{spec}");
    // Без очереди: отказ сразу.
    let mut no_queue = remote_args();
    no_queue["queue"] = json!(false);
    assert_eq!(code(f.spawn(no_queue).await), Code::MachineUnavailable);

    // ── Очередь переживает перезапуск хаба и запускает на B ──
    sqlx::query("delete from spawn_queue where parent_id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    f.spawn(remote_args()).await.unwrap();
    // «Перезапуск»: новый реестр с обеими машинами; очередь берётся только из базы.
    let registry = Registry::new(Default::default());
    let _a = testkit::fake_session(&registry, f.a, &CAPABILITIES, true);
    let _b = testkit::fake_session(&registry, f.b, &REMOTE, true);
    sqlx::query("update spawn_queue set not_before=now()-interval '1 second'")
        .execute(&f.pool)
        .await
        .unwrap();
    f.dequeue_with(&registry).await;
    let children = f.children().await;
    assert_eq!(children.len(), 1, "{:?}", f.queue_notes().await);
    assert_eq!(children[0].1, f.b);
    assert_eq!(f.start_machines(children[0].0).await, vec![f.b]);
}

#[tokio::test]
async fn a_queued_remote_spawn_is_refused_after_off_or_expiry() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::with(pool, false).await;
    f.switch(true).await;
    f.spawn(remote_args()).await.unwrap();
    f.switch(false).await;
    sqlx::query("update spawn_queue set not_before=now()-interval '1 second'")
        .execute(&f.pool)
        .await
        .unwrap();
    f.dequeue().await;
    let (state, result): (String, Option<Value>) =
        sqlx::query_as("select state,result from spawn_queue where parent_id=$1")
            .bind(f.parent)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(result.unwrap()["code"], "remote_work_disabled");
    assert!(f.children().await.is_empty());
    assert!(
        f.queue_notes()
            .await
            .iter()
            .any(|note| note.contains("remote_work_disabled")),
        "{:?}",
        f.queue_notes().await
    );

    // ── Выключение и включение обратно не оживляют ожидавший запуск ──
    sqlx::query("delete from spawn_queue where parent_id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    f.switch(true).await;
    f.spawn(remote_args()).await.unwrap();
    f.switch(false).await;
    f.switch(true).await;
    sqlx::query("update spawn_queue set not_before=now()-interval '1 second'")
        .execute(&f.pool)
        .await
        .unwrap();
    let registry = Registry::new(Default::default());
    let _a = testkit::fake_session(&registry, f.a, &CAPABILITIES, true);
    let _b = testkit::fake_session(&registry, f.b, &REMOTE, true);
    f.dequeue_with(&registry).await;
    let (state, result): (String, Option<Value>) =
        sqlx::query_as("select state,result from spawn_queue where parent_id=$1")
            .bind(f.parent)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed", "{result:?}");
    assert_eq!(result.unwrap()["code"], "remote_work_disabled");
    assert!(f.children().await.is_empty());

    // ── Срок ожидания истёк: элемент истекает с запиской родителю ──
    sqlx::query("delete from spawn_queue where parent_id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    f.switch(true).await;
    f.spawn(remote_args()).await.unwrap();
    sqlx::query("update spawn_queue set not_before=now()-interval '2 hours',expires_at=now()-interval '1 second'")
        .execute(&f.pool)
        .await
        .unwrap();
    f.dequeue().await;
    let state: String = sqlx::query_scalar("select state from spawn_queue where parent_id=$1")
        .bind(f.parent)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(state, "expired");
    assert!(
        f.queue_notes()
            .await
            .iter()
            .any(|note| note.contains("expired")),
        "{:?}",
        f.queue_notes().await
    );
}

#[tokio::test]
async fn an_approved_remote_spawn_does_not_revive_after_off_and_on() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let mut outside = remote_args();
    outside["model"] = json!(OUTSIDE);
    let answer = f.spawn(outside).await.unwrap();
    let request: Uuid = serde_json::from_value(answer["request_id"].clone()).unwrap();
    f.switch(false).await;
    f.switch(true).await;
    f.decide(request, PermissionBehavior::Allow).await;
    f.tick().await;
    assert!(f.children().await.is_empty(), "the old card must not start");
    assert!(
        f.notes()
            .await
            .iter()
            .any(|note| note.contains("remote_work_disabled")),
        "{:?}",
        f.notes().await
    );
    // Новая заявка после повторного включения проходит.
    f.started(remote_args()).await;
}

#[tokio::test]
async fn a_target_worker_without_remote_spawn_queues_instead_of_starting() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::with(pool, false).await;
    f.switch(true).await;
    // Воркер B подключён, но без `remote_spawn_v1`.
    let _old = testkit::fake_session(&f.context.registry, f.b, &CAPABILITIES, true);
    let answer = f.spawn(remote_args()).await.unwrap();
    assert_eq!(answer["status"], "queued", "{answer}");
    assert!(f.children().await.is_empty());
    let detail: String = sqlx::query_scalar("select detail->>'detail' from spawn_queue")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(detail.contains("remote_spawn_v1"), "{detail}");
    let mut no_queue = remote_args();
    no_queue["queue"] = json!(false);
    assert_eq!(code(f.spawn(no_queue).await), Code::CapabilityMissing);
}

#[tokio::test]
async fn an_isolated_remote_child_gets_its_worktree_from_the_target_machine() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    // Рабочая копия родителя на A не причём: источник и корень копий — у B.
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(f.parent)
        .bind(
            json!({"source_workdir":"/tmp/w","branch":"canterlot/parent","path":"/data/worktrees/p",
                     "base":"HEAD","state":"active"}),
        )
        .execute(&f.pool)
        .await
        .unwrap();
    let mut isolated = remote_args();
    isolated["isolated"] = json!(true);
    let (child, answer) = f.started(isolated).await;
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        payload["worktree"]["source_workdir"], "/tmp/b/p",
        "{payload}"
    );
    assert_eq!(payload["worktree"]["base"], "HEAD", "{payload}");
    assert!(
        payload["workdir"]
            .as_str()
            .unwrap()
            .starts_with("/data/b-worktrees/"),
        "{payload}"
    );
    assert_eq!(payload["remote"], true);
    assert_eq!(answer["machine_name"], "machine-b");
    assert!(answer["worktree"]["path"].is_string(), "{answer}");
    // Без корня рабочих копий у цели изоляция отказывается, как и у своей машины.
    sqlx::query("update machines set worktree_root=null where id=$1")
        .bind(f.b)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut again = remote_args();
    again["isolated"] = json!(true);
    assert_eq!(code(f.spawn(again).await), Code::CapabilityMissing);
}

#[tokio::test]
async fn sessions_list_names_the_machine_and_accept_stops_the_remote_child() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    let (child, _) = f.started(remote_args()).await;
    let list = f.call("sessions.list", json!({})).await.unwrap();
    let name_of = |id: Uuid| -> Value {
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id.to_string())
            .unwrap_or_else(|| panic!("{id} is not listed: {list}"))["machine_name"]
            .clone()
    };
    assert_eq!(name_of(child), "machine-b");
    assert_eq!(name_of(f.parent), "synthetic");

    // ── Принятие останавливает ребёнка на его собственной машине ──
    let mut isolated = remote_args();
    isolated["isolated"] = json!(true);
    let (child, _) = f.started(isolated).await;
    let accepted = f
        .call(
            "sessions.accept",
            json!({"session_id": child, "force": true}),
        )
        .await
        .unwrap();
    assert_eq!(accepted["status"], "stopping", "{accepted}");
    let stops: Vec<Uuid> = sqlx::query_scalar(
        "select machine_id from commands where conversation_id=$1 and kind='stop_execution'",
    )
    .bind(child)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(stops, vec![f.b]);
}

/// Политика с одним типом задачи; белый список разрешает модель, которая есть только у B.
async fn policy_with_b_only_model(f: &Fx) {
    sqlx::query("update owners set routing_policy=$2,spawn_whitelist=$3 where id=$1")
        .bind(f.owner)
        .bind(json!({"task_types": {"t": {"title": "Т", "candidates": [
            {"harness":"claude_code","model":B_ONLY,"effort":"low",
             "exclude_workdirs":["/tmp/b/secret"]}
        ]}}}))
        .bind(json!([
            {"harness":"claude_code","model":INSIDE},
            {"harness":"claude_code","model":B_ONLY},
        ]))
        .execute(&f.pool)
        .await
        .unwrap();
}

/// Предпросмотр на другую машину называет учётную запись, чьи цифры взял; свой запуск с одной
/// записью называет провайдера по-старому.
#[tokio::test]
async fn the_remote_explanation_names_the_account_whose_limits_it_used() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    policy_with_b_only_model(&f).await;
    sqlx::query(
        "update owners set routing_policy=routing_policy
           || '{\"providers\":{\"claude\":{\"max_weekly_percent\":90}}}'::jsonb where id=$1",
    )
    .bind(f.owner)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update machine_harnesses set account='b-claude' where machine_id=$1")
        .bind(f.b)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into provider_limits(provider,account,weekly_used_permille,weekly_resets_at,
                                     weekly_observed_at)
         values('claude','b-claude',990,now()+interval '3 days',now())",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    let error = f
        .call(
            "routing.preview",
            json!({"task_type": "t", "machine": "machine-b", "workdir": "/tmp/b/p"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::NoExecutorAvailable, "{}", error.message);
    let text = format!("{error:?}");
    assert!(text.contains("claude@b-claude weekly 99%"), "{text}");
}

#[tokio::test]
async fn route_preview_and_routed_spawn_see_the_target_machine() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    policy_with_b_only_model(&f).await;
    // Своя машина не знает модели `b-model`: исполнителя нет.
    let own = f
        .call("routing.preview", json!({"task_type": "t"}))
        .await
        .unwrap_err();
    assert_eq!(own.code, Code::NoExecutorAvailable, "{}", own.message);
    // У B она есть: предпросмотр и запуск выбирают её.
    let preview = f
        .call(
            "routing.preview",
            json!({"task_type": "t", "machine": "machine-b", "workdir": "/tmp/b/p"}),
        )
        .await
        .unwrap();
    assert_eq!(preview["decision"]["model"], B_ONLY, "{preview}");
    assert_eq!(preview["machine_name"], "machine-b");
    assert_eq!(preview["accepts_remote_work"], true);
    // Исключённый каталог считается по пути цели.
    let excluded = f
        .call(
            "routing.preview",
            json!({"task_type": "t", "machine": "machine-b", "workdir": "/tmp/b/secret/x"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        excluded.code,
        Code::NoExecutorAvailable,
        "{}",
        excluded.message
    );
    // Без каталога на другой машине и вне её корней предпросмотр отказывает, как запуск.
    for args in [
        json!({"task_type": "t", "machine": "machine-b"}),
        json!({"task_type": "t", "machine": "machine-b", "workdir": "/tmp/w"}),
    ] {
        let error = f.call("routing.preview", args).await.unwrap_err();
        assert_eq!(error.code, Code::WorkdirNotAllowed);
    }
    let (child, answer) = f
        .started(json!({
            "route": "t", "machine": "machine-b", "workdir": "/tmp/b/p", "brief": "go",
        }))
        .await;
    assert_eq!(answer["machine_name"], "machine-b");
    assert_eq!(f.start_machines(child).await, vec![f.b]);
    let model: String = sqlx::query_scalar("select model from conversations where id=$1")
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(model, B_ONLY);
}

/// Машина B становится раннером машины A: сессий на раннере нет, что бы он ни сообщал.
async fn turn_b_into_a_runner(f: &Fx) {
    sqlx::query(
        "update machines set role='runner', parent_machine_id=$2, root_dir='/srv/runner'
         where id=$1",
    )
    .bind(f.b)
    .bind(f.a)
    .execute(&f.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn session_spawn_and_route_preview_refuse_a_runner_machine() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.switch(true).await;
    policy_with_b_only_model(&f).await;
    // Пока B обычная машина, тот же вызов проходит.
    let preview = f
        .call(
            "routing.preview",
            json!({"task_type": "t", "machine": "machine-b", "workdir": "/tmp/b/p"}),
        )
        .await
        .unwrap();
    assert_eq!(preview["machine_name"], "machine-b");
    turn_b_into_a_runner(&f).await;

    for machine in ["machine-b".to_owned(), f.b.to_string()] {
        let mut args = remote_args();
        args["machine"] = json!(machine);
        assert_eq!(code(f.spawn(args).await), Code::MachineIsRunner);
        let error = f
            .call(
                "routing.preview",
                json!({"task_type": "t", "machine": machine, "workdir": "/tmp/b/p"}),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, Code::MachineIsRunner);
        // Маршрут с запуском: тот же отказ, ничего не создано.
        let routed = f
            .spawn(json!({
                "route": "t", "machine": machine, "workdir": "/tmp/b/p", "brief": "go",
            }))
            .await;
        assert_eq!(code(routed), Code::MachineIsRunner);
    }
    assert!(f.children().await.is_empty(), "no session was created");
    let commands: i64 = sqlx::query_scalar("select count(*) from commands where machine_id=$1")
        .bind(f.b)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(commands, 0, "nothing was sent to the runner");
}
