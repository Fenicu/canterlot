//! Доска плана оркестратора: инструменты, автоматика по жизни исполнителя, приём,
//! удаление, снимок, события и порядок блокировок.

use std::{sync::Arc, time::Duration};

use axum::http::Method;
use canterlot_hub::{
    activity::{self, Thresholds},
    config::SessionToolsConfig,
    deletion, executions, http,
    model::ExecutionState,
    session_tools::{self, Caller, ToolContext, ToolErrorCode as Code, plan, settle::SettleConfig},
    snapshot,
    testkit::{self, TestClient},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        hung_rescue: canterlot_hub::config::HungRescue::Auto,
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    }
}

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    orchestrator: Uuid,
    child: Uuid,
    execution: executions::StartedExecution,
}

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
        memory: Default::default(),
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
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
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

async fn start(pool: &PgPool, owner: Uuid, conversation: Uuid) -> executions::StartedExecution {
    let execution = executions::start(pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

/// Разговор с личным доменом памяти: сессии одного домена видят друг друга.
async fn personal_conversation(pool: &PgPool, owner: Uuid, machine: Uuid) -> Uuid {
    let id = canterlot_hub::testkit::seed_conversation(pool, owner, machine).await;
    sqlx::query("update conversations set memory_domain='personal' where id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn child_of(pool: &PgPool, owner: Uuid, machine: Uuid, parent: Uuid) -> Uuid {
    let child = personal_conversation(pool, owner, machine).await;
    sqlx::query(
        "update conversations set parent_conversation_id=$2,spawn_depth=1,title='child' where id=$1",
    )
    .bind(child)
    .bind(parent)
    .execute(pool)
    .await
    .unwrap();
    child
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        sqlx::query("update machines set availability='available' where id=$1")
            .bind(seed.machine_id)
            .execute(&pool)
            .await
            .unwrap();
        let child = child_of(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
        sqlx::query("update conversations set memory_domain='personal' where id=any($1)")
            .bind([seed.conversation_id, child])
            .execute(&pool)
            .await
            .unwrap();
        let execution = start(&pool, seed.owner_id, child).await;
        Self {
            context: context(&pool),
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            orchestrator: seed.conversation_id,
            child,
            execution,
        }
    }

    fn caller_of(&self, conversation: Uuid) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: conversation,
            execution_id: Uuid::now_v7(),
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call_as(
        &self,
        conversation: Uuid,
        tool: &str,
        args: Value,
    ) -> Result<Value, session_tools::ToolError> {
        session_tools::call(&self.context, &self.caller_of(conversation), tool, args).await
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, session_tools::ToolError> {
        self.call_as(self.orchestrator, tool, args).await
    }

    async fn add(&self, titles: &[&str]) -> Value {
        let tasks: Vec<Value> = titles.iter().map(|t| json!({"title": t})).collect();
        self.call("plan.add", json!({"tasks": tasks}))
            .await
            .unwrap()
    }

    /// То же, что делает последним оператором `start_child`.
    async fn link(&self, number: i32, child: Uuid) {
        let mut tx = self.pool.begin().await.unwrap();
        let id = plan::resolve_for_spawn_in_tx(&mut tx, self.owner, self.orchestrator, number)
            .await
            .unwrap();
        let outcome = plan::link_child_in_tx(&mut tx, self.orchestrator, child, id, Uuid::now_v7())
            .await
            .unwrap();
        assert_eq!(outcome, plan::LinkOutcome::Linked { number });
        tx.commit().await.unwrap();
    }

    async fn task(&self, number: i32) -> (String, i64, Option<Uuid>) {
        self.task_of(self.orchestrator, number).await
    }

    async fn task_of(&self, orchestrator: Uuid, number: i32) -> (String, i64, Option<Uuid>) {
        sqlx::query_as(
            "select state,revision,executor_conversation_id from plan_tasks
             where orchestrator_conversation_id=$1 and number=$2",
        )
        .bind(orchestrator)
        .bind(number)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn state(&self, number: i32) -> String {
        self.task(number).await.0
    }

    async fn turn(&self, text: &str, at: DateTime<Utc>, git: Value) -> Uuid {
        let command = Uuid::now_v7();
        sqlx::query(
            "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
             values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
        )
        .bind(command)
        .bind(self.machine)
        .bind(self.child)
        .bind(self.execution.execution_id)
        .bind(self.execution.generation)
        .execute(&self.pool)
        .await
        .unwrap();
        let block = Uuid::now_v7();
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,agent,model,created_seq,created_at,updated_at)
             values($1,$2,$3,'text','final',1,$4,$5,'main','m',(select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),$6,$6)",
        )
        .bind(block)
        .bind(self.execution.execution_id)
        .bind(self.child)
        .bind(text)
        .bind(command)
        .bind(at)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms,git)
             values($1,$2,$3,$4,$4,1000,$5)",
        )
        .bind(command)
        .bind(self.execution.execution_id)
        .bind(self.child)
        .bind(at)
        .bind(git)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("update executions set last_event_at=$2 where id=$1")
            .bind(self.execution.execution_id)
            .bind(at)
            .execute(&self.pool)
            .await
            .unwrap();
        block
    }

    async fn events(&self) -> Vec<Value> {
        sqlx::query_scalar(
            "select payload from events where kind='plan_task_changed' order by committed_at,scope_seq",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn stop_child(&self) {
        executions::set_state(
            &self.pool,
            self.execution.execution_id,
            ExecutionState::Stopped,
            Some("machine_rebooted"),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }

    /// Приём доводится до конца: процесс исполнителя остановился, задание уборки архивирует
    /// сессию. Только после этого задача плана готова.
    async fn finish_accept(&self) {
        self.stop_child().await;
        canterlot_hub::scheduler::Scheduler::new(
            self.pool.clone(),
            canterlot_hub::dispatcher::Dispatcher::channel().0,
            Default::default(),
        )
        .tick(Utc::now())
        .await
        .unwrap();
    }
}

fn no_git() -> Value {
    json!({"available": false})
}

#[tokio::test]
async fn add_list_and_update_follow_the_rules_and_limits() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let added = f.add(&["  Парсер\nчисел ", "Тесты"]).await;
    assert_eq!(added["numbers"], json!([1, 2]));
    assert_eq!(added["tasks"][0]["title"], "Парсер чисел");
    assert_eq!(added["tasks"][0]["state"], "waiting");
    assert_eq!(added["tasks"][0]["note"], Value::Null);
    assert_eq!(f.add(&["Ещё"]).await["numbers"], json!([3]));

    for bad in [
        json!({"tasks": []}),
        json!({"tasks": vec![json!({"title": "x"}); 21]}),
        json!({"tasks": [{"title": "x".repeat(201)}]}),
        json!({"tasks": [{"title": " \n "}]}),
        json!({"tasks": [{"title": "x", "note": "n".repeat(2049)}]}),
        json!({"tasks": [{"title": "x", "extra": 1}]}),
    ] {
        let error = f.call("plan.add", bad.clone()).await.unwrap_err();
        assert_eq!(error.code, Code::InvalidArguments, "{bad}");
    }

    let missing = f
        .call("plan.update", json!({"number": 9, "state": "done"}))
        .await
        .unwrap_err();
    assert_eq!(missing.code, Code::PlanTaskNotFound);

    // Причина без состояния блокирует; снятая — возвращает в ожидание.
    let blocked = f
        .call(
            "plan.update",
            json!({"number": 1, "blocker": "нет доступа"}),
        )
        .await
        .unwrap();
    assert_eq!(blocked["task"]["state"], "blocked");
    assert_eq!(blocked["task"]["blocker"], "нет доступа");
    let cleared = f
        .call("plan.update", json!({"number": 1, "blocker": ""}))
        .await
        .unwrap();
    assert_eq!(cleared["task"]["state"], "waiting");
    assert_eq!(cleared["task"]["blocker"], Value::Null);
    // Причина с явным состоянием не блокирует.
    let explicit = f
        .call(
            "plan.update",
            json!({"number": 2, "blocker": "ждёт ревью", "state": "in_review"}),
        )
        .await
        .unwrap();
    assert_eq!(explicit["task"]["state"], "in_review");

    let findings = f
        .call(
            "plan.update",
            json!({"number": 3, "findings": ["a", "b"], "note": "детали", "title": "Новое"}),
        )
        .await
        .unwrap();
    assert_eq!(findings["task"]["findings"], json!(["a", "b"]));
    let replaced = f
        .call(
            "plan.update",
            json!({"number": 3, "findings": ["c"], "note": ""}),
        )
        .await
        .unwrap();
    assert_eq!(replaced["task"]["findings"], json!(["c"]));
    assert_eq!(replaced["task"]["note"], Value::Null);
    let revision = f.task(3).await.1;
    let same = f
        .call("plan.update", json!({"number": 3, "findings": ["c"]}))
        .await
        .unwrap();
    assert_eq!(same["status"], "unchanged");
    assert_eq!(
        f.task(3).await.1,
        revision,
        "no revision bump without a change"
    );
    for bad in [
        json!({"number": 3, "findings": vec!["f"; 21]}),
        json!({"number": 3, "findings": ["x".repeat(501)]}),
        json!({"number": 3, "blocker": "x".repeat(501)}),
        json!({"number": 3, "state": "finished"}),
        json!({"number": 3, "title": ""}),
    ] {
        let error = f.call("plan.update", bad.clone()).await.unwrap_err();
        assert_eq!(error.code, Code::InvalidArguments, "{bad}");
    }

    let list = f.call("plan.list", json!({})).await.unwrap();
    let tasks = list["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0]["number"], 1);
    assert_eq!(tasks[0]["commits"], json!([]));
    assert_eq!(tasks[0]["report_block_id"], Value::Null);
    assert_eq!(tasks[0]["report_seq"], Value::Null);
    let hint = list["hint"].as_str().unwrap();
    assert!(hint.contains("#2 [in_review] Тесты"), "{hint}");

    // Чужой план не виден: у потомка свой, пустой.
    let child_list = f.call_as(f.child, "plan.list", json!({})).await.unwrap();
    assert_eq!(child_list["tasks"], json!([]));
}

#[tokio::test]
async fn http_plan_and_snapshot_carry_the_contract() {
    let pool = canterlot_hub::testkit::db().await;
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'synthetic')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let root = personal_conversation(&pool, owner, machine).await;
    let delegate = child_of(&pool, owner, machine, root).await;
    let archived = personal_conversation(&pool, owner, machine).await;
    let f = Fx {
        context: context(&pool),
        pool: pool.clone(),
        owner,
        machine,
        orchestrator: root,
        child: delegate,
        execution: start(&pool, owner, delegate).await,
    };
    f.add(&["Парсер", "Старое"]).await;
    f.link(1, delegate).await;
    f.call("plan.update", json!({"number": 2, "state": "done"}))
        .await
        .unwrap();
    sqlx::query("update plan_tasks set updated_at=now()-interval '8 days' where number=2")
        .execute(&pool)
        .await
        .unwrap();
    f.call("plan.archive", json!({"numbers": [2]}))
        .await
        .unwrap();
    // План делегата и архивного оркестратора в снимок не попадают.
    f.call_as(
        delegate,
        "plan.add",
        json!({"tasks": [{"title": "личное"}]}),
    )
    .await
    .unwrap();
    f.call_as(archived, "plan.add", json!({"tasks": [{"title": "архив"}]}))
        .await
        .unwrap();
    sqlx::query("update conversations set archived_at=now() where id=$1")
        .bind(archived)
        .execute(&pool)
        .await
        .unwrap();
    let delegate_events = f
        .events()
        .await
        .iter()
        .filter(|event| event["task"]["orchestrator_id"] == json!(delegate))
        .count();
    assert_eq!(
        delegate_events, 0,
        "a delegate's plan stays out of the owner scope"
    );

    let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
    let response = client
        .send(
            Method::GET,
            &format!("/api/v1/plan?orchestrator={root}"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let body = response.json::<Value>();
    let task = &body["tasks"][0];
    for field in [
        "id",
        "orchestrator_id",
        "number",
        "title",
        "note",
        "blocker",
        "findings",
        "state",
        "executor_id",
        "attempts",
        "created_at",
        "updated_at",
        "revision",
        "archived",
        "commits",
        "commits_truncated",
        "files_changed",
        "report_block_id",
        "report_seq",
    ] {
        assert!(task.get(field).is_some(), "{field} missing in {task}");
    }
    assert_eq!(task["executor_id"], json!(delegate));
    assert_eq!(task["attempts"], 1);
    // HTTP-чтение владельца возвращает и архив: PWA показывает его по кнопке.
    assert_eq!(body["tasks"][1]["archived"], true);
    let missing = client
        .send(
            Method::GET,
            &format!("/api/v1/plan?orchestrator={}", Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(missing.status, 404);

    let snap = snapshot::conversations(&pool, owner).await.unwrap();
    let tasks = serde_json::to_value(&snap.data.plan_tasks).unwrap();
    let titles: Vec<&str> = tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["Парсер"], "{tasks}");
}

#[tokio::test]
async fn accept_marks_the_task_done() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.add(&["Парсер", "Тесты"]).await;
    f.link(1, f.child).await;
    f.call("plan.update", json!({"number": 1, "blocker": "ждём"}))
        .await
        .unwrap();
    f.call("sessions.accept", json!({"session_id": f.child}))
        .await
        .unwrap();
    assert_eq!(
        f.state(1).await,
        "blocked",
        "the executor is still stopping: nothing is accepted yet"
    );
    f.finish_accept().await;
    let (state, _, executor) = f.task(1).await;
    assert_eq!((state.as_str(), executor), ("done", Some(f.child)));
    assert_eq!(f.state(2).await, "waiting", "other tasks untouched");
    let revision = f.task(1).await.1;
    f.call("sessions.accept", json!({"session_id": f.child}))
        .await
        .unwrap();
    assert_eq!(
        f.task(1).await.1,
        revision,
        "a repeated accept changes nothing"
    );
}

#[tokio::test]
async fn adopt_moves_open_tasks_and_the_adopter_may_accept_their_executors() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.add(&["Парсер", "Тесты", "Доки"]).await;
    f.link(2, f.child).await;
    f.call("plan.update", json!({"number": 3, "state": "done"}))
        .await
        .unwrap();
    let fresh = personal_conversation(&f.pool, f.owner, f.machine).await;
    f.call_as(fresh, "plan.add", json!({"tasks": [{"title": "Своя"}]}))
        .await
        .unwrap();

    // Источник в корзине — главный случай передачи.
    sqlx::query("update conversations set trashed_at=now() where id=$1")
        .bind(f.orchestrator)
        .execute(&f.pool)
        .await
        .unwrap();
    let adopted = f
        .call_as(
            fresh,
            "plan.adopt",
            json!({"from_session_id": f.orchestrator}),
        )
        .await
        .unwrap();
    assert_eq!(
        adopted["moved"],
        json!([{"from": 1, "to": 2}, {"from": 2, "to": 3}])
    );
    assert_eq!(
        f.task_of(fresh, 3).await.2,
        Some(f.child),
        "keeps its executor"
    );
    assert_eq!(
        f.task_of(f.orchestrator, 3).await.0,
        "done",
        "closed tasks stay"
    );

    // Номера не переиспользуются ни у источника, ни у получателя.
    sqlx::query("update conversations set trashed_at=null where id=$1")
        .bind(f.orchestrator)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(f.add(&["Новая"]).await["numbers"], json!([4]));
    let mine = f
        .call_as(fresh, "plan.add", json!({"tasks": [{"title": "Ещё"}]}))
        .await
        .unwrap();
    assert_eq!(mine["numbers"], json!([4]));

    // Исполнитель перенятой задачи принимается новым оркестратором.
    f.call_as(fresh, "sessions.accept", json!({"session_id": f.child}))
        .await
        .unwrap();
    f.finish_accept().await;
    assert_eq!(f.task_of(fresh, 3).await.0, "done");
}

/// План работающего оркестратора перенять нельзя: иначе любой корневой агент домена получал
/// бы право принять с `force` чужих работающих делегатов и выбросить их незакоммиченную
/// работу. Передача идёт от сессии без живого процесса.
#[tokio::test]
async fn adopt_refuses_a_source_that_is_still_running() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    f.add(&["Парсер"]).await;
    f.link(1, f.child).await;
    let source = start(&f.pool, f.owner, f.orchestrator).await;
    let fresh = personal_conversation(&f.pool, f.owner, f.machine).await;
    let adopt = || {
        f.call_as(
            fresh,
            "plan.adopt",
            json!({"from_session_id": f.orchestrator}),
        )
    };

    let error = adopt().await.unwrap_err();
    assert_eq!(error.code, Code::InvalidArguments);
    assert!(error.message.contains("still running"), "{}", error.message);
    assert_eq!(f.task(1).await.2, Some(f.child), "nothing moved");
    let error = f
        .call_as(
            fresh,
            "sessions.accept",
            json!({"session_id": f.child, "force": true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::NotParent);

    // Процесс источника остановлен: план передаётся.
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("user_stop")),
    ] {
        executions::set_state(&f.pool, source.execution_id, state, outcome, Uuid::now_v7())
            .await
            .unwrap();
    }
    let adopted = adopt().await.unwrap();
    assert_eq!(adopted["moved"], json!([{"from": 1, "to": 1}]));
}

#[tokio::test]
async fn plan_writes_share_the_lock_order_with_accept_projector_and_delete() {
    let pool = canterlot_hub::testkit::db().await;
    // Хранилище вложений нужно удалению только как приёмник: одно на все круги, а не
    // новый каталог с синхронизацией на каждом круге, и в scratch_dir, а не на общем диске.
    let directory = testkit::scratch_dir().unwrap();
    let (_, blobs) = testkit::test_blob_store(directory.path());
    for round in 0..8 {
        let f = Fx::new(pool.clone()).await;
        f.add(&["Парсер", "Тесты"]).await;
        f.link(1, f.child).await;
        let other = personal_conversation(&f.pool, f.owner, f.machine).await;
        f.call_as(other, "plan.add", json!({"tasks": [{"title": "чужое"}]}))
            .await
            .unwrap();
        let now = Utc::now();
        f.turn(
            "Готово\nSTATUS: DONE",
            now - TimeDelta::minutes(5),
            no_git(),
        )
        .await;
        let accept = f.call("sessions.accept", json!({"session_id": f.child}));
        let limits = thresholds();
        let refresh = activity::refresh(&f.pool, f.child, now, &limits, Uuid::now_v7());
        let update = f.call("plan.update", json!({"number": 1, "findings": ["x"]}));
        let update2 = f.call("plan.update", json!({"number": 2, "state": "in_progress"}));
        let delete = deletion::delete(
            &f.pool,
            blobs.as_ref(),
            f.owner,
            other,
            "delete",
            Uuid::now_v7(),
        );
        let (a, r, u, u2, d) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(accept, refresh, update, update2, delete)
        })
        .await
        .expect("no deadlock");
        a.unwrap();
        r.unwrap();
        u.unwrap();
        u2.unwrap();
        d.unwrap();
        f.finish_accept().await;
        assert_eq!(f.state(1).await, "done", "round {round}");
    }
}
