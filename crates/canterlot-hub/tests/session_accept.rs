use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::http::Method;
use canterlot_hub::{
    config::{LimitsConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions::{self, ExecutionError},
    http,
    ingest::{Ingest, JournalKind},
    model::ExecutionState,
    outbox::{CommandRow, OutboxError, to_proto_for_session},
    scheduler::Scheduler,
    session::Registry,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit::{self, FakeSession, TestClient, TestResponse},
};
use canterlot_proto::worker::v1::{
    JournalRecord, JournalRecords, RecordBody, WorktreeRemoved, record_body::Body,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sha2::Digest as _;
use sqlx::PgPool;
use uuid::Uuid;

const CAPABILITIES: [&str; 3] = ["pilot_v1", "instructions_v1", "session_tools_v1"];

struct Fx {
    pool: PgPool,
    registry: Registry,
    context: ToolContext,
    scheduler: Scheduler,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
    parent: Uuid,
    parent_execution: Uuid,
    child: Uuid,
    child_execution: Uuid,
    journal: Uuid,
    seq: AtomicU64,
    _session: FakeSession,
    _dir: tempfile::TempDir,
}

fn model(id: &str) -> Value {
    json!({"id": id, "title": id, "efforts": ["low", "high"]})
}

fn scheduler(pool: &PgPool, registry: &Registry) -> Scheduler {
    let (dispatcher, _) = Dispatcher::channel();
    Scheduler::new(pool.clone(), dispatcher, Default::default())
        .with_limits(registry.clone(), LimitsConfig::default())
}

fn context(pool: &PgPool, registry: &Registry) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
        },
        dispatcher: None,
        registry: registry.clone(),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        Self::with(pool, true).await
    }

    async fn with(pool: PgPool, isolated: bool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'synthetic')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
            .bind(machine)
            .bind(sha2::Sha256::digest(machine.as_bytes()).as_slice())
            .execute(&pool)
            .await
            .unwrap();
        let registry = Registry::new(Default::default());
        let session = testkit::fake_session(&registry, machine, &CAPABILITIES, true);
        sqlx::query(
            "update machines set availability='available',capabilities=array['pilot_v1'],
               allowed_roots='[\"/tmp\"]',worktree_root='/data/worktrees' where id=$1",
        )
        .bind(machine)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into machine_harnesses(machine_id,harness,provider,title,images,settings_midsession,
                                           permission_modes,models,settings_midsession_effort)
             values($1,'synthetic','claude','synthetic',true,true,
                    '[\"owner\",\"ask\",\"bypass\",\"read_only\"]',$2,true)",
        )
        .bind(machine)
        .bind(json!([model("small")]))
        .execute(&pool)
        .await
        .unwrap();
        let parent = testkit::seed_conversation(&pool, owner, machine).await;
        sqlx::query(
            "update conversations set title='Родитель',model='small',effort='low',
               permission_mode='bypass',workdir='/tmp/w' where id=$1",
        )
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
        let started = executions::start(&pool, owner, parent, Uuid::now_v7())
            .await
            .unwrap();
        sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
            .bind(owner)
            .bind(json!([{"harness":"synthetic","model":"small"}]))
            .execute(&pool)
            .await
            .unwrap();
        let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
        let mut fx = Self {
            context: context(&pool, &registry),
            scheduler: scheduler(&pool, &registry),
            client,
            owner,
            machine,
            parent,
            parent_execution: started.execution_id,
            child: Uuid::nil(),
            child_execution: Uuid::nil(),
            journal: Uuid::now_v7(),
            seq: AtomicU64::new(1),
            registry,
            pool,
            _session: session,
            _dir: dir,
        };
        let (child, execution) = fx.spawn_child(isolated).await;
        fx.child = child;
        fx.child_execution = execution;
        fx
    }

    fn caller(&self, conversation: Uuid, execution: Uuid) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: conversation,
            execution_id: execution,
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    fn parent_caller(&self) -> Caller {
        self.caller(self.parent, self.parent_execution)
    }

    async fn spawn_from(&self, caller: Caller, extra: Value) -> (Uuid, Uuid) {
        let mut args = json!({"harness":"synthetic","model":"small","brief":"Сделай задачу"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let answer = session_tools::call(&self.context, &caller, "sessions.spawn", args)
            .await
            .unwrap();
        assert_eq!(answer["status"], "started", "{answer}");
        let child: Uuid = serde_json::from_value(answer["session_id"].clone()).unwrap();
        let execution = sqlx::query_scalar("select id from executions where conversation_id=$1")
            .bind(child)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        (child, execution)
    }

    async fn spawn_child(&self, isolated: bool) -> (Uuid, Uuid) {
        self.spawn_from(self.parent_caller(), json!({"isolated": isolated}))
            .await
    }

    async fn state_of(&self, execution: Uuid, states: &[(ExecutionState, Option<&str>)]) {
        for (state, outcome) in states {
            executions::set_state(&self.pool, execution, *state, *outcome, Uuid::now_v7())
                .await
                .unwrap();
        }
    }

    async fn run_child(&self) {
        self.state_of(
            self.child_execution,
            &[
                (ExecutionState::Starting, None),
                (ExecutionState::Running, None),
            ],
        )
        .await;
    }

    async fn stop_child(&self) {
        self.state_of(
            self.child_execution,
            &[
                (ExecutionState::Stopping, None),
                (ExecutionState::Stopped, Some("user_stop")),
            ],
        )
        .await;
    }

    async fn stopped_child(&self) {
        self.run_child().await;
        self.stop_child().await;
    }

    async fn accept_as(
        &self,
        caller: Caller,
        session: Uuid,
        force: bool,
    ) -> Result<Value, ToolError> {
        session_tools::call(
            &self.context,
            &caller,
            "sessions.accept",
            json!({"session_id": session, "force": force}),
        )
        .await
    }

    async fn accept(&self, force: bool) -> Value {
        self.accept_as(self.parent_caller(), self.child, force)
            .await
            .unwrap()
    }

    async fn tick(&self) {
        self.scheduler.tick(Utc::now()).await.unwrap();
    }

    async fn tick_after(&self, delta: TimeDelta) {
        self.scheduler.tick(Utc::now() + delta).await.unwrap();
    }

    async fn worktree(&self) -> Value {
        sqlx::query_scalar::<_, Option<Value>>("select worktree from conversations where id=$1")
            .bind(self.child)
            .fetch_one(&self.pool)
            .await
            .unwrap()
            .unwrap_or(Value::Null)
    }

    async fn archived(&self) -> bool {
        sqlx::query_scalar("select archived_at is not null from conversations where id=$1")
            .bind(self.child)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn commands(&self, kind: &str) -> Vec<(Uuid, Value, String, Option<Uuid>)> {
        sqlx::query_as(
            "select id,payload,delivery_state,execution_id from commands
             where conversation_id=$1 and kind=$2 order by created_at,id",
        )
        .bind(self.child)
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn open_subscriptions(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from session_subscriptions
             where closed_at is null and (target_id=$1 or subscriber_id=$1)",
        )
        .bind(self.child)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn pending_cleanups(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from scheduled_jobs where kind='session_task' and state='pending'
               and conversation_id=$1 and payload->>'action'='cleanup'",
        )
        .bind(self.child)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn live_executions(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from executions where conversation_id=$1
               and state not in ('stopped','failed')",
        )
        .bind(self.child)
        .fetch_one(&self.pool)
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

    async fn last_note(&self) -> String {
        self.notes().await.pop().expect("a note for the parent")
    }

    async fn last_note_origin(&self) -> Value {
        sqlx::query_scalar::<_, Option<Value>>(
            "select origin from inputs where conversation_id=$1 order by input_seq desc limit 1",
        )
        .bind(self.parent)
        .fetch_one(&self.pool)
        .await
        .unwrap()
        .unwrap_or(Value::Null)
    }

    async fn report(&self, command: Uuid, outcome: &str, branch_kept: bool, detail: &str) {
        self.report_from(self.machine, command, outcome, branch_kept, detail)
            .await;
    }

    async fn report_from(
        &self,
        machine: Uuid,
        command: Uuid,
        outcome: &str,
        branch_kept: bool,
        detail: &str,
    ) {
        let attempt: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id=$1")
            .bind(command)
            .fetch_optional(&self.pool)
            .await
            .unwrap()
            .unwrap_or(0);
        self.report_attempt(
            machine,
            command,
            attempt as u32,
            outcome,
            branch_kept,
            detail,
        )
        .await;
    }

    async fn report_attempt(
        &self,
        machine: Uuid,
        command: Uuid,
        attempt: u32,
        outcome: &str,
        branch_kept: bool,
        detail: &str,
    ) {
        let body = Body::WorktreeRemoved(WorktreeRemoved {
            command_id: command.to_string(),
            dispatch_attempt: attempt,
            outcome: outcome.into(),
            detail: detail.into(),
            branch_kept,
        });
        let (journal, seq) = if machine == self.machine {
            (self.journal, self.seq.fetch_add(1, Ordering::Relaxed))
        } else {
            (Uuid::now_v7(), 1)
        };
        Ingest::new(self.pool.clone())
            .apply_batch(
                machine,
                sha2::Sha256::digest(machine.as_bytes()).as_slice(),
                JournalKind::Cj,
                &JournalRecords {
                    journal_id: journal.to_string(),
                    records: vec![JournalRecord {
                        seq,
                        body: Some(RecordBody { body: Some(body) }),
                    }],
                },
            )
            .await
            .unwrap();
    }

    /// Приём живой сессии доведён до команды воркеру.
    async fn removal_command(&self) -> (Uuid, Value) {
        let commands = self.commands("remove_worktree").await;
        let (id, payload, ..) = commands.last().expect("remove_worktree command").clone();
        (id, payload)
    }
}

fn code(result: Result<Value, ToolError>) -> Code {
    result.expect_err("tool error").code
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_stops_then_removes_then_archives(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    assert_eq!(fx.open_subscriptions().await, 1);

    let answer = fx.accept(false).await;
    assert_eq!(answer["status"], "stopping", "{answer}");
    assert!(answer["hint"].as_str().unwrap().contains("[canterlot]"));
    let stops = fx.commands("stop_execution").await;
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].1["force"], false);
    assert_eq!(fx.worktree().await["state"], "removing");
    assert_eq!(fx.open_subscriptions().await, 0);
    assert_eq!(fx.pending_cleanups().await, 1);

    // Пока процесс жив, уборка ждёт и ничего не отправляет воркеру.
    fx.tick().await;
    assert_eq!(fx.pending_cleanups().await, 1);
    assert!(fx.commands("remove_worktree").await.is_empty());

    fx.stop_child().await;
    fx.tick_after(TimeDelta::seconds(6)).await;
    let (command, payload) = fx.removal_command().await;
    let branch = fx.worktree().await["branch"].as_str().unwrap().to_owned();
    assert_eq!(
        payload,
        json!({
            "source_workdir": "/tmp/w",
            "path": format!("/data/worktrees/{}", fx.child),
            "branch": branch,
            "force": false,
        })
    );
    let attached: (Option<Uuid>, Option<i64>) =
        sqlx::query_as("select execution_id,execution_generation from commands where id=$1")
            .bind(command)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(attached, (Some(fx.child_execution), Some(1)));
    assert!(!fx.archived().await);

    fx.report(command, "removed", true, "").await;
    let worktree = fx.worktree().await;
    assert_eq!(worktree["state"], "removed");
    assert_eq!(worktree["branch_kept"], true);
    assert!(fx.archived().await);
    let state: String = sqlx::query_scalar("select delivery_state from commands where id=$1")
        .bind(command)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "completed");
    let updated: Value = sqlx::query_scalar(
        "select payload from events where kind='conversation_updated' and conversation_id=$1
         order by id desc limit 1",
    )
    .bind(fx.child)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(updated["worktree"]["state"], "removed");
    assert!(updated["archived"].as_bool().unwrap_or(false), "{updated}");

    fx.tick().await;
    let note = fx.last_note().await;
    assert!(note.contains(&branch), "{note}");
    assert!(note.contains("kept"), "{note}");
    assert!(
        !note.contains("/data/worktrees"),
        "no path in the note: {note}"
    );
    let origin = fx.last_note_origin().await;
    assert_eq!(origin["event"]["kind"], "accepted", "{origin}");
    assert_eq!(origin["event"]["session_id"], json!(fx.child));
    assert_eq!(origin["event"]["worktree"], "removed");
    assert_eq!(origin["event"]["branch"], branch);
    assert_eq!(origin["event"]["branch_kept"], true);
    assert_eq!(fx.open_subscriptions().await, 0);
    assert_eq!(fx.live_executions().await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_without_worktree_stops_and_archives(pool: PgPool) {
    let fx = Fx::with(pool, false).await;
    assert!(fx.worktree().await.is_null());
    fx.run_child().await;
    let answer = fx.accept(false).await;
    assert_eq!(answer["status"], "stopping", "{answer}");
    assert!(!fx.archived().await);
    fx.stop_child().await;
    fx.tick_after(TimeDelta::seconds(6)).await;
    assert!(fx.archived().await);
    assert!(fx.commands("remove_worktree").await.is_empty());
    assert_eq!(fx.pending_cleanups().await, 0);
    assert_eq!(fx.open_subscriptions().await, 0);
    fx.tick().await;
    assert!(fx.last_note().await.contains("archived"));
    let origin = fx.last_note_origin().await;
    assert_eq!(origin["event"]["kind"], "accepted", "{origin}");
    assert_eq!(origin["event"]["worktree"], "none");

    // Уже остановленную сессию без копии архивируют сразу, заданий не остаётся.
    let (other, execution) = fx.spawn_child(false).await;
    fx.state_of(
        execution,
        &[
            (ExecutionState::Starting, None),
            (ExecutionState::Running, None),
            (ExecutionState::Stopping, None),
            (ExecutionState::Stopped, Some("user_stop")),
        ],
    )
    .await;
    let answer = fx
        .accept_as(fx.parent_caller(), other, false)
        .await
        .unwrap();
    assert_eq!(answer["status"], "done", "{answer}");
    let archived: bool =
        sqlx::query_scalar("select archived_at is not null from conversations where id=$1")
            .bind(other)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert!(archived);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_parent_can_accept(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let child_caller = fx.caller(fx.child, fx.child_execution);
    let (grandchild, grandchild_execution) = fx
        .spawn_from(child_caller, json!({"workdir": "/tmp/w"}))
        .await;
    let (sibling, sibling_execution) = fx.spawn_child(false).await;

    assert_eq!(
        code(fx.accept_as(child_caller, fx.child, false).await),
        Code::NotParent
    );
    assert_eq!(
        code(
            fx.accept_as(fx.caller(grandchild, grandchild_execution), fx.child, false)
                .await
        ),
        Code::NotParent
    );
    assert_eq!(
        code(fx.accept_as(fx.parent_caller(), grandchild, false).await),
        Code::NotParent
    );
    assert_eq!(
        code(fx.accept_as(fx.parent_caller(), fx.parent, false).await),
        Code::NotDelegated
    );
    assert_eq!(
        code(
            fx.accept_as(fx.caller(sibling, sibling_execution), fx.child, false)
                .await
        ),
        Code::SessionNotFound
    );
    assert_eq!(
        code(
            fx.accept_as(fx.parent_caller(), Uuid::now_v7(), false)
                .await
        ),
        Code::SessionNotFound
    );

    // Отказы ничего не изменили.
    assert_eq!(fx.pending_cleanups().await, 0);
    assert!(fx.commands("stop_execution").await.is_empty());
    assert_eq!(fx.worktree().await["state"], "active");
    assert_eq!(fx.open_subscriptions().await, 2);
    assert_eq!(
        fx.accept(false).await["status"],
        "stopping",
        "the direct parent may accept"
    );
}

async fn set_mode(fx: &Fx, id: Uuid, mode: &str) {
    sqlx::query("update conversations set permission_mode=$2 where id=$1")
        .bind(id)
        .bind(mode)
        .execute(&fx.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn read_only_parent_cannot_stop_a_writing_child_or_discard_its_work(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    set_mode(&fx, fx.parent, "read_only").await;

    for force in [false, true] {
        assert_eq!(
            code(fx.accept_as(fx.parent_caller(), fx.child, force).await),
            Code::ReadOnlySession
        );
    }
    assert_eq!(fx.pending_cleanups().await, 0);
    assert_eq!(fx.worktree().await["state"], "active");

    sqlx::query(
        "update conversations set permission_mode='bypass',
           pending_settings=jsonb_build_object('permission_mode','read_only') where id=$1",
    )
    .bind(fx.parent)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        code(fx.accept_as(fx.parent_caller(), fx.child, true).await),
        Code::ReadOnlySession,
        "a narrowing that has not taken effect yet counts"
    );
    assert_eq!(fx.pending_cleanups().await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn read_only_parent_may_accept_a_read_only_child_but_never_force(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    set_mode(&fx, fx.parent, "read_only").await;
    set_mode(&fx, fx.child, "read_only").await;

    assert_eq!(
        code(fx.accept_as(fx.parent_caller(), fx.child, true).await),
        Code::ReadOnlySession,
        "the child may hold work from before it was narrowed"
    );
    assert_eq!(fx.pending_cleanups().await, 0);

    let answer = fx.accept(false).await;
    assert_eq!(answer["status"], "removing", "{answer}");
    fx.tick().await;
    let (_, payload) = fx.removal_command().await;
    assert_eq!(
        payload["force"], false,
        "the worker still refuses a dirty copy"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_tells_other_subscribers_instead_of_dropping_them(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let child_caller = fx.caller(fx.child, fx.child_execution);
    let (sibling, sibling_execution) = fx
        .spawn_from(child_caller, json!({"workdir": "/tmp/w"}))
        .await;
    let subscribed = session_tools::call(
        &fx.context,
        &fx.caller(sibling, sibling_execution),
        "sessions.subscribe",
        json!({"session_id": fx.child}),
    )
    .await
    .unwrap();
    assert_eq!(subscribed["subscribed"], true, "{subscribed}");
    // Итог, который подписчику уже поставлен в очередь, приём не отменяет.
    let queued = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,now(),$4)",
    )
    .bind(queued)
    .bind(fx.owner)
    .bind(sibling)
    .bind(json!({
        "action": "event", "target_id": fx.child, "state": "idle", "fingerprint": "f",
        "armed_at": Utc::now(), "chain_depth": 0,
    }))
    .execute(&fx.pool)
    .await
    .unwrap();

    fx.accept(false).await;
    let open: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        "select subscriber_id,closed_reason from session_subscriptions
         where target_id=$1 and closed_at is not null order by closed_at",
    )
    .bind(fx.child)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert!(open.iter().any(|row| row.0 == sibling), "{open:?}");
    assert_eq!(fx.open_subscriptions().await, 0);
    let state: String = sqlx::query_scalar("select state from scheduled_jobs where id=$1")
        .bind(queued)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "pending");

    fx.tick().await;
    let texts: Vec<String> = sqlx::query_scalar::<_, Value>(
        "select content from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(sibling)
    .fetch_all(&fx.pool)
    .await
    .unwrap()
    .into_iter()
    .map(|content| content[0]["text"].as_str().unwrap().to_owned())
    .collect();
    assert!(
        texts.iter().any(|text| text.contains("is now idle")),
        "the queued result still arrives: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("accepted") && text.contains("no further")),
        "the subscriber is told the session was accepted: {texts:?}"
    );
    let closed: Value = sqlx::query_scalar(
        "select origin from inputs where conversation_id=$1 and origin->'event'->>'closed'='true'",
    )
    .bind(sibling)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(closed["event"]["kind"], "accepted", "{closed}");
    assert_eq!(closed["event"]["session_id"], json!(fx.child));

    // Принявший родитель о собственном приёме отдельно не оповещается.
    assert!(
        fx.notes()
            .await
            .iter()
            .all(|text| !text.contains("no further events")),
    );
}

fn accept_path(id: Uuid) -> String {
    format!("/api/v1/conversations/{id}/accept")
}

async fn post(fx: &Fx, id: Uuid, key: &str, body: Value) -> TestResponse {
    fx.client
        .send(Method::POST, &accept_path(id), Some(key), Some(body))
        .await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_can_accept_over_http(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let first = post(&fx, fx.child, "accept-1", json!({})).await;
    assert_eq!(
        first.status,
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(first.json::<Value>(), json!({"state": "stopping"}));
    assert_eq!(fx.pending_cleanups().await, 1);
    assert_eq!(fx.commands("stop_execution").await.len(), 1);

    let replay = post(&fx, fx.child, "accept-1", json!({})).await;
    assert_eq!(replay.json::<Value>(), json!({"state": "stopping"}));
    assert_eq!(fx.pending_cleanups().await, 1);

    let conflict = post(&fx, fx.child, "accept-1", json!({"force": true})).await;
    assert_eq!(conflict.status, 409);
    assert_eq!(
        conflict.json::<Value>()["error"]["code"],
        "idempotency_conflict"
    );

    let root = post(&fx, fx.parent, "accept-root", json!({})).await;
    assert_eq!(root.status, 409);
    assert_eq!(root.json::<Value>()["error"]["code"], "not_delegated");

    let missing = post(&fx, Uuid::now_v7(), "accept-missing", json!({})).await;
    assert_eq!(missing.status, 404);

    let unknown = post(&fx, fx.child, "accept-unknown", json!({"surprise": 1})).await;
    assert!(unknown.status.as_u16() >= 400);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_accepts_a_child_whose_parent_is_gone(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    // Так оставляет deletion.rs: потомок без родителя, но всё ещё делегированный.
    sqlx::query("update conversations set parent_conversation_id=null where id=$1")
        .bind(fx.child)
        .execute(&fx.pool)
        .await
        .unwrap();
    let answer = post(&fx, fx.child, "orphan", json!({})).await;
    assert_eq!(
        answer.status,
        200,
        "{}",
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(answer.json::<Value>(), json!({"state": "removing"}));
    fx.tick().await;
    let (command, _) = fx.removal_command().await;
    fx.report(command, "removed", false, "").await;
    assert_eq!(fx.worktree().await["state"], "removed");
    assert!(fx.archived().await);
}

async fn delete(fx: &Fx, id: Uuid, key: &str) -> TestResponse {
    fx.client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{id}"),
            Some(key),
            None,
        )
        .await
}

async fn set_worktree_state(fx: &Fx, state: &str) {
    sqlx::query("update conversations set worktree=worktree||jsonb_build_object('state',$2::text) where id=$1")
        .bind(fx.child)
        .bind(state)
        .execute(&fx.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_session_keeps_waiting_for_its_worktree(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    for state in ["active", "dirty", "removing"] {
        set_worktree_state(&fx, state).await;
        let refused = delete(&fx, fx.child, &format!("delete-{state}")).await;
        assert_eq!(refused.status, 409, "{state}");
        assert_eq!(
            refused.json::<Value>()["error"]["code"],
            "worktree_not_removed",
            "{state}"
        );
    }
    assert_eq!(fx.worktree().await["state"], "removing");

    set_worktree_state(&fx, "removed").await;
    let done = delete(&fx, fx.child, "delete-removed").await;
    assert_eq!(done.status, 204, "{}", String::from_utf8_lossy(&done.body));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_does_not_wait_for_a_removal_that_failed(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    set_worktree_state(&fx, "failed").await;
    assert_eq!(delete(&fx, fx.child, "failed").await.status, 204);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_does_not_wait_for_a_machine_that_is_gone(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    sqlx::query("update machines set availability='unavailable' where id=$1")
        .bind(fx.machine)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(delete(&fx, fx.child, "gone").await.status, 204);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dirty_keeps_work_and_tells_parent(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    let answer = fx.accept(false).await;
    assert_eq!(answer["status"], "removing", "{answer}");
    fx.tick().await;
    let (command, payload) = fx.removal_command().await;
    assert_eq!(payload["force"], false);
    let executions_before: i64 =
        sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
            .bind(fx.child)
            .fetch_one(&fx.pool)
            .await
            .unwrap();

    fx.report(command, "dirty", false, "dirty").await;
    assert_eq!(fx.worktree().await["state"], "dirty");
    assert!(!fx.archived().await);
    assert_eq!(fx.live_executions().await, 0);
    let executions_after: i64 =
        sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
            .bind(fx.child)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(executions_before, executions_after);
    fx.tick().await;
    let note = fx.last_note().await;
    assert!(note.contains("force:true"), "{note}");
    assert!(note.contains("uncommitted"), "{note}");
    assert!(note.contains("commits on no branch"), "{note}");
    assert!(!note.contains("session_send to"), "{note}");
    assert!(note.contains("the owner has to resume"), "{note}");
    let origin = fx.last_note_origin().await;
    assert_eq!(origin["event"]["kind"], "accepted", "{origin}");
    assert_eq!(origin["event"]["worktree"], "dirty");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn force_removes_dirty_and_reports_branch(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    fx.tick().await;
    let (first, _) = fx.removal_command().await;
    fx.report(first, "dirty", false, "dirty").await;
    fx.tick().await;

    let answer = fx.accept(true).await;
    assert_eq!(answer["status"], "removing", "{answer}");
    assert_eq!(fx.worktree().await["state"], "removing");
    fx.tick().await;
    let (second, payload) = fx.removal_command().await;
    assert_ne!(first, second);
    assert_eq!(payload["force"], true);

    fx.report(second, "removed", true, "").await;
    let worktree = fx.worktree().await;
    assert_eq!(worktree["state"], "removed");
    assert_eq!(worktree["branch_kept"], true);
    assert!(fx.archived().await);
    fx.tick().await;
    let note = fx.last_note().await;
    assert!(
        note.contains(worktree["branch"].as_str().unwrap()),
        "{note}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_removal_is_reported_and_can_be_retried(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    fx.tick().await;
    let (command, _) = fx.removal_command().await;
    fx.report(command, "failed", false, "git_failed").await;
    let worktree = fx.worktree().await;
    assert_eq!(worktree["state"], "failed");
    assert_eq!(worktree["detail"], "git_failed");
    assert!(!fx.archived().await);
    fx.tick().await;
    assert!(fx.last_note().await.contains("git_failed"));
    let origin = fx.last_note_origin().await;
    assert_eq!(origin["event"]["kind"], "accepted", "{origin}");
    assert_eq!(origin["event"]["worktree"], "failed");
    assert_eq!(origin["event"]["detail"], "git_failed");

    assert_eq!(fx.accept(false).await["status"], "removing");
    fx.tick().await;
    assert_eq!(fx.commands("remove_worktree").await.len(), 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_survives_hub_restart(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    assert_eq!(fx.accept(false).await["status"], "stopping");
    fx.tick().await;
    drop(fx.scheduler);

    // Новый планировщик видит то же задание в базе, ничего не помнит и доводит дело до конца.
    let restarted = scheduler(&fx.pool, &fx.registry);
    let fx = Fx {
        scheduler: restarted,
        ..fx
    };
    fx.stop_child().await;
    fx.tick_after(TimeDelta::seconds(6)).await;
    let (command, _) = fx.removal_command().await;
    fx.report(command, "removed", false, "").await;
    assert!(fx.archived().await);
    assert_eq!(fx.worktree().await["state"], "removed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_is_idempotent(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    assert_eq!(fx.accept(false).await["status"], "stopping");
    assert_eq!(fx.accept(false).await["status"], "stopping");
    assert_eq!(fx.pending_cleanups().await, 1);
    assert_eq!(fx.commands("stop_execution").await.len(), 1);

    fx.stop_child().await;
    fx.tick_after(TimeDelta::seconds(6)).await;
    assert_eq!(fx.accept(false).await["status"], "removing");
    fx.tick_after(TimeDelta::seconds(7)).await;
    assert_eq!(fx.commands("remove_worktree").await.len(), 1);

    let (command, _) = fx.removal_command().await;
    fx.report(command, "removed", false, "").await;
    // Повтор того же итога заметок не множит.
    fx.report(command, "removed", false, "").await;
    fx.tick().await;
    let notes = fx.notes().await.len();
    assert_eq!(fx.accept(false).await["status"], "done");
    fx.tick().await;
    assert_eq!(fx.commands("remove_worktree").await.len(), 1);
    assert_eq!(fx.pending_cleanups().await, 0);
    assert_eq!(fx.notes().await.len(), notes);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn live_session_gets_one_forced_stop_after_ten_minutes(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    fx.accept(false).await;
    fx.tick_after(TimeDelta::minutes(9)).await;
    assert_eq!(fx.commands("stop_execution").await.len(), 1);

    fx.tick_after(TimeDelta::minutes(11)).await;
    fx.tick_after(TimeDelta::minutes(12)).await;
    let stops = fx.commands("stop_execution").await;
    let forced = stops.iter().filter(|stop| stop.1["force"] == true).count();
    assert_eq!((stops.len(), forced), (2, 1), "{stops:?}");
    assert_eq!(fx.pending_cleanups().await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stop_that_never_finishes_gives_up_after_a_day(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    fx.accept(false).await;
    fx.tick_after(TimeDelta::hours(25)).await;
    let worktree = fx.worktree().await;
    assert_eq!(worktree["state"], "failed");
    assert_eq!(worktree["detail"], "stop_timeout");
    assert_eq!(fx.pending_cleanups().await, 0);
    fx.tick_after(TimeDelta::hours(25)).await;
    assert!(fx.last_note().await.contains("stop_timeout"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_after_cleanup_is_refused(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    for stage in ["removing", "removed"] {
        if stage == "removed" {
            fx.tick().await;
            let (command, _) = fx.removal_command().await;
            fx.report(command, "removed", false, "").await;
        }
        let refused = executions::start(&fx.pool, fx.owner, fx.child, Uuid::now_v7()).await;
        assert!(
            matches!(refused, Err(ExecutionError::WorktreeRemoved)),
            "{stage}: {refused:?}"
        );
        let response = fx
            .client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{}/executions", fx.child),
                Some(&format!("resume-{stage}")),
                None,
            )
            .await;
        assert_eq!(response.status, 409, "{stage}");
        assert_eq!(
            response.json::<Value>()["error"]["code"],
            "worktree_removed"
        );
    }
    let launched: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(fx.child)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(launched, 1, "no second start was queued");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dirty_session_can_be_resumed_because_its_worktree_exists(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    fx.tick().await;
    let (command, _) = fx.removal_command().await;
    fx.report(command, "dirty", false, "dirty").await;
    executions::start(&fx.pool, fx.owner, fx.child, Uuid::now_v7())
        .await
        .unwrap();
    let start: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'
         order by created_at desc limit 1",
    )
    .bind(fx.child)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(start["worktree"]["branch"], fx.worktree().await["branch"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn worktree_removed_requires_capability_negotiated_command(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    fx.tick().await;
    let (command, _) = fx.removal_command().await;

    // Воркер без возможности команду не получает: она ждёт, а не теряется.
    let row: CommandRow = sqlx::query_as("select * from commands where id=$1")
        .bind(command)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let old = Registry::new(Default::default());
    let _old_session = testkit::fake_session(&old, fx.machine, &["pilot_v1"], true);
    assert!(matches!(
        to_proto_for_session(&row, &old),
        Err(OutboxError::CapabilityMissing)
    ));
    assert!(to_proto_for_session(&row, &fx.registry).is_ok());

    // Итог без такой команды ничего не меняет: чужой номер, чужая машина, не та попытка, не та команда.
    let stop = fx.commands("stop_execution").await;
    let foreign_command = stop.first().map(|stop| stop.0).unwrap_or_else(Uuid::now_v7);
    let other = testkit::seed(&fx.pool).await;
    fx.report(Uuid::now_v7(), "removed", true, "").await;
    fx.report(foreign_command, "removed", true, "").await;
    fx.report_from(other.machine_id, command, "removed", true, "")
        .await;
    fx.report_attempt(fx.machine, command, 7, "removed", true, "")
        .await;
    assert_eq!(fx.worktree().await["state"], "removing");
    assert!(!fx.archived().await);
    let state: String = sqlx::query_scalar("select delivery_state from commands where id=$1")
        .bind(command)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_ne!(state, "completed");

    fx.report(command, "removed", true, "").await;
    assert_eq!(fx.worktree().await["state"], "removed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rejected_removal_does_not_leave_the_copy_removing(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    fx.accept(false).await;
    fx.tick().await;
    let (command, _) = fx.removal_command().await;
    let body = Body::CommandRejected(canterlot_proto::worker::v1::CommandRejected {
        command_id: command.to_string(),
        dispatch_attempt: 0,
        reason: "unsupported_command".into(),
    });
    Ingest::new(fx.pool.clone())
        .apply_batch(
            fx.machine,
            sha2::Sha256::digest(fx.machine.as_bytes()).as_slice(),
            JournalKind::Cj,
            &JournalRecords {
                journal_id: fx.journal.to_string(),
                records: vec![JournalRecord {
                    seq: 1,
                    body: Some(RecordBody { body: Some(body) }),
                }],
            },
        )
        .await
        .unwrap();
    let worktree = fx.worktree().await;
    assert_eq!(worktree["state"], "failed");
    assert_eq!(worktree["detail"], "rejected");
    assert!(!fx.archived().await);
    assert_eq!(fx.commands("remove_worktree").await[0].2, "completed");
}

async fn control_tool(
    fx: &Fx,
    caller: Caller,
    tool: &str,
    target: Uuid,
) -> Result<Value, ToolError> {
    session_tools::call(&fx.context, &caller, tool, json!({"session_id": target})).await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn interrupt_stops_the_live_turn_once(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let first = control_tool(&fx, fx.parent_caller(), "sessions.interrupt", fx.child)
        .await
        .unwrap();
    assert_eq!(first["status"], "interrupted");
    let second = control_tool(&fx, fx.parent_caller(), "sessions.interrupt", fx.child)
        .await
        .unwrap();
    assert_eq!(second["status"], "interrupted");
    assert_eq!(second["command_id"], first["command_id"]);
    assert_eq!(fx.commands("interrupt").await.len(), 1);
    assert_eq!(fx.live_executions().await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn interrupt_reports_nothing_running(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.stopped_child().await;
    let answer = control_tool(&fx, fx.parent_caller(), "sessions.interrupt", fx.child)
        .await
        .unwrap();
    assert_eq!(answer["status"], "nothing_running");
    assert!(fx.commands("interrupt").await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stop_stops_the_process_and_keeps_the_worktree(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let answer = control_tool(&fx, fx.parent_caller(), "sessions.stop", fx.child)
        .await
        .unwrap();
    assert_eq!(answer["status"], "stopping");
    assert_eq!(answer["worktree_kept"], true);
    assert_eq!(fx.commands("stop_execution").await.len(), 1);
    let again = control_tool(&fx, fx.parent_caller(), "sessions.stop", fx.child)
        .await
        .unwrap();
    assert_eq!(again["status"], "already_stopping");
    assert_eq!(again["command_id"], answer["command_id"]);
    assert_eq!(fx.worktree().await["state"], "active");
    assert!(!fx.archived().await);
    assert!(fx.accept(false).await["status"].is_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_the_direct_parent_may_stop_and_owner_sessions_are_untouchable(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let elsewhere = testkit::seed_conversation(&fx.pool, fx.owner, fx.machine).await;
    sqlx::query("update conversations set parent_conversation_id=$2 where id=$1")
        .bind(fx.child)
        .bind(elsewhere)
        .execute(&fx.pool)
        .await
        .unwrap();
    let error = control_tool(&fx, fx.parent_caller(), "sessions.stop", fx.child)
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::NotParent);

    let root = testkit::seed_conversation(&fx.pool, fx.owner, fx.machine).await;
    let error = control_tool(&fx, fx.parent_caller(), "sessions.interrupt", root)
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::NotDelegated);
}

async fn pauses(fx: &Fx, id: Uuid) -> Value {
    sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
        .bind(id)
        .fetch_one(&fx.pool)
        .await
        .unwrap()
}

async fn send_to_child(fx: &Fx, text: &str) -> Value {
    session_tools::call(
        &fx.context,
        &fx.parent_caller(),
        "sessions.send",
        json!({"session_id": fx.child, "text": text}),
    )
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn after_an_interrupt_the_parents_next_message_is_delivered(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    let answer = control_tool(&fx, fx.parent_caller(), "sessions.interrupt", fx.child)
        .await
        .unwrap();
    assert_eq!(answer["status"], "interrupted");
    assert!(
        answer["hint"].as_str().unwrap().contains("session_send"),
        "{answer}"
    );
    let sent = send_to_child(&fx, "Продолжай с другим планом").await;
    assert_eq!(sent["target_paused"], false, "{sent}");
    assert_eq!(
        pauses(&fx, fx.child).await,
        json!([]),
        "the interrupt pause is lifted"
    );
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(Uuid::parse_str(sent["input_id"].as_str().unwrap()).unwrap())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_parent_never_lifts_the_owners_stop(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.run_child().await;
    control_tool(&fx, fx.parent_caller(), "sessions.interrupt", fx.child)
        .await
        .unwrap();
    // Владелец остановил ход после родителя: его стоп остаётся.
    canterlot_hub::delivery::control::request_stop(
        &fx.pool,
        fx.owner,
        fx.child,
        "owner-stop-1",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let sent = send_to_child(&fx, "Продолжай").await;
    assert_eq!(sent["target_paused"], true, "{sent}");
    assert!(
        sent["hint"].as_str().unwrap().contains("owner"),
        "the parent is told the message waits: {sent}"
    );
    assert_eq!(
        pauses(&fx, fx.child).await,
        json!([{"reason": "user_stop"}])
    );
}
