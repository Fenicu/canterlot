//! Очередь запусков C2: постановка, задание `spawn_dequeue`, отмена, страж, сроки.

use std::sync::Arc;

use axum::http::Method;
use canterlot_hub::{
    activity::Thresholds,
    config::{ActivityConfig, LimitsConfig, SchedulerConfig, SessionToolsConfig},
    crunch::JobOutcome,
    dispatcher::Dispatcher,
    http,
    scheduler::{ClaimedJob, JobKind, Scheduler},
    session::Registry,
    session_tools::{
        self, Caller, TOOLS, ToolContext, ToolErrorCode as Code, jobs, settle::SettleConfig,
        spawn_queue,
    },
    testkit::{self, FakeSession, TestClient},
};
use chrono::{DateTime, Utc};
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
    session: Option<FakeSession>,
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
        let registry = Registry::new(Default::default());
        let session = testkit::fake_session(&registry, seed.machine_id, &CAPABILITIES, true);
        for (harness, provider, models) in [
            (
                "opencode",
                "opencode",
                json!([
                    model(
                        "opencode-go/deepseek-v4.1-flash",
                        &["low", "medium", "high"]
                    ),
                    model("opencode-go/beam-free", &["low", "medium"]),
                ]),
            ),
            (
                "claude_code",
                "claude",
                json!([model("opus[1m]", &["low", "medium", "high"])]),
            ),
        ] {
            catalog(&pool, seed.machine_id, harness, provider, models).await;
        }
        sqlx::query(
            "update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true,
               allowed_roots='[\"/tmp\"]',worktree_root='/data/worktrees' where id=$1",
        )
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "update conversations set title='Родитель',harness='opencode',model='opencode-go/beam-free',
               effort='medium',permission_mode='bypass',workdir='/tmp/w' where id=$1",
        )
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
        let started = canterlot_hub::executions::start(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
            .bind(seed.owner_id)
            .bind(json!([
                {"harness":"opencode","model":"opencode-go/deepseek-v4.1-flash"},
                {"harness":"opencode","model":"opencode-go/beam-free"},
                {"harness":"claude_code","model":"opus[1m]"},
            ]))
            .execute(&pool)
            .await
            .unwrap();
        let (_dispatcher, _) = Dispatcher::channel();
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
                routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
                spawn_queue: Default::default(),
                guards: Default::default(),
                dispatcher: None,
                registry,
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            parent: seed.conversation_id,
            execution: started.execution_id,
            session: Some(session),
        }
    }

    fn caller(&self) -> Caller {
        self.caller_with(Uuid::now_v7())
    }

    fn caller_with(&self, call_id: Uuid) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.parent,
            execution_id: self.execution,
            machine_id: self.machine,
            call_id,
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, session_tools::ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }

    async fn spawn_direct(&self) -> Result<Value, session_tools::ToolError> {
        self.call(
            "sessions.spawn",
            json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
        )
        .await
    }

    async fn runtime_call(
        &self,
        queue_id: Uuid,
        start_now: bool,
    ) -> Result<spawn_queue::StartNowOutcome, session_tools::ToolError> {
        let runtime = spawn_queue::Runtime {
            registry: Some(&self.context.registry),
            session_tools: &self.context.config,
            spawn_queue: &self.context.spawn_queue,
            guards: &self.context.guards,
            routing_enabled: self.context.routing_enabled,
            routing_stale: self.context.routing_stale,
            routing_queue_horizon: self.context.routing_queue_horizon,
        };
        let mut tx = self.pool.begin().await.unwrap();
        let result = if start_now {
            spawn_queue::start_now_in_tx(
                &mut tx,
                &runtime,
                self.owner,
                self.parent,
                queue_id,
                Uuid::now_v7(),
                Utc::now(),
            )
            .await
        } else {
            unreachable!()
        };
        tx.commit().await.unwrap();
        result
    }

    async fn run_dequeue(&self, queue_id: Uuid, now: DateTime<Utc>) -> JobOutcome {
        self.run_dequeue_for(self.parent, queue_id, now).await
    }

    async fn run_dequeue_for(
        &self,
        parent: Uuid,
        queue_id: Uuid,
        now: DateTime<Utc>,
    ) -> JobOutcome {
        let job = ClaimedJob {
            id: Uuid::now_v7(),
            kind: JobKind::SessionTask,
            owner_id: self.owner,
            conversation_id: Some(parent),
            run_at: now,
            payload: json!({"action":"spawn_dequeue","queue_id":queue_id}),
            attempts: 0,
        };
        let runtime = spawn_queue::Runtime {
            registry: Some(&self.context.registry),
            session_tools: &self.context.config,
            spawn_queue: &self.context.spawn_queue,
            guards: &self.context.guards,
            routing_enabled: self.context.routing_enabled,
            routing_stale: self.context.routing_stale,
            routing_queue_horizon: self.context.routing_queue_horizon,
        };
        let thresholds = Thresholds::from(&ActivityConfig::default());
        let mut tx = self.pool.begin().await.unwrap();
        let outcome = jobs::run_in_tx(
            &mut tx,
            &job,
            now,
            Uuid::now_v7(),
            Some(&self.context.registry),
            &self.context.settle,
            &thresholds,
            Some(&runtime),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        outcome
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
    sqlx::query("insert into queue_state(conversation_id) values($1) on conflict do nothing")
        .bind(id)
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

/// Десять запусков в окне частоты: одиннадцатый встаёт в очередь.
async fn fill_rate(pool: &PgPool, parent: Uuid) {
    for _ in 0..10 {
        child_of(pool, parent, "opencode", "opencode-go/beam-free", "idle").await;
    }
}

async fn queue_row(
    pool: &PgPool,
    id: Uuid,
) -> (String, String, Value, Option<Uuid>, DateTime<Utc>) {
    sqlx::query_as("select state,reason,detail,child_id,not_before from spawn_queue where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn make_due(pool: &PgPool, id: Uuid) {
    sqlx::query("update spawn_queue set not_before=now()-interval '1 second' where id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

async fn notes_for(pool: &PgPool, parent: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "select payload->>'text' from scheduled_jobs
          where conversation_id=$1 and payload->>'action'='note' and payload->>'text' like '%Queued spawn%'",
    )
    .bind(parent)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn low_disk(pool: &PgPool, machine: Uuid) {
    sqlx::query(
        "insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,
              mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions,observed_at)
         values($1,0,8,100,16000000000,12000000000,40,40,40,
           '[{\"mount\":\"/\",\"total_bytes\":100000000000,\"free_bytes\":12000000000}]'::jsonb,'[]',now())
         on conflict (machine_id) do update set machine_time_ms=0,cpu_cores=8,cpu_permille=100,
           mem_total_bytes=16000000000,mem_available_bytes=12000000000,load1_centi=40,load5_centi=40,load15_centi=40,
           disks='[{\"mount\":\"/\",\"total_bytes\":100000000000,\"free_bytes\":12000000000}]'::jsonb,
           executions='[]'::jsonb,observed_at=now()",
    )
    .bind(machine)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn eleventh_spawn_queues_and_the_job_starts_it_with_one_note(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    assert_eq!(answer["status"], "queued", "{answer}");
    assert_eq!(answer["reason"], "rate", "{answer}");
    assert_eq!(answer["position"], 1, "{answer}");
    assert!(
        answer["detail"]
            .as_str()
            .unwrap()
            .contains("10 spawns in the last hour"),
        "{answer}"
    );
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();

    // Окно частоты освободилось: задание запускает элемент.
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, queue_id).await;
    let outcome = f.run_dequeue(queue_id, Utc::now()).await;
    assert!(matches!(outcome, JobOutcome::Done(_)), "{outcome:?}");
    let (state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "started");
    let child = child.expect("child id");
    let (harness, model): (String, String) =
        sqlx::query_as("select harness,model from conversations where id=$1")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(harness, "opencode");
    assert_eq!(model, "opencode-go/beam-free");
    let notes = notes_for(&f.pool, f.parent).await;
    assert_eq!(notes.len(), 1, "exactly one note: {notes:?}");
    assert!(notes[0].contains("Queued spawn q-"), "{}", notes[0]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retried_call_id_returns_the_same_item(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let call_id = Uuid::now_v7();
    let first = session_tools::call(
        &f.context,
        &f.caller_with(call_id),
        "sessions.spawn",
        json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
    )
    .await
    .unwrap();
    let second = session_tools::call(
        &f.context,
        &f.caller_with(call_id),
        "sessions.spawn",
        json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
    )
    .await
    .unwrap();
    assert_eq!(first["status"], "queued", "{first}");
    assert_eq!(first["queue_id"], second["queue_id"]);
    assert_eq!(second["position"], 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_cancel_answers_three_ways_and_refuses_another_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();

    let foreign = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let foreign_exec = canterlot_hub::executions::start(&f.pool, f.owner, foreign, Uuid::now_v7())
        .await
        .unwrap();
    let foreign_caller = Caller {
        owner_id: f.owner,
        conversation_id: foreign,
        execution_id: foreign_exec.execution_id,
        machine_id: f.machine,
        call_id: Uuid::now_v7(),
    };
    let refused = session_tools::call(
        &f.context,
        &foreign_caller,
        "spawn_queue.cancel",
        json!({"queue_id": queue_id}),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, Code::NotParent);

    let cancelled = f
        .call("spawn_queue.cancel", json!({"queue_id": queue_id}))
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    let again = f
        .call("spawn_queue.cancel", json!({"queue_id": queue_id}))
        .await
        .unwrap();
    assert_eq!(again["status"], "already_cancelled");

    // Второй элемент встаёт в очередь, затем окно частоты освобождается.
    let second = f.spawn_direct().await.unwrap();
    assert_eq!(second["status"], "queued", "{second}");
    let started_id: Uuid = serde_json::from_value(second["queue_id"].clone()).unwrap();
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, started_id).await;
    f.run_dequeue(started_id, Utc::now()).await;
    let answer = f
        .call("spawn_queue.cancel", json!({"queue_id": started_id}))
        .await
        .unwrap();
    assert_eq!(answer["status"], "already_started", "{answer}");
    assert!(answer["session_id"].is_string(), "{answer}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_false_returns_the_old_errors(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let error = f
        .call(
            "sessions.spawn",
            json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b","queue":false}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::RateLimited);

    low_disk(&f.pool, f.machine).await;
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    let error = f
        .call(
            "sessions.spawn",
            json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b","queue":false}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::MachineBusy, "{}", error.message);

    sqlx::query("delete from machine_metrics where machine_id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_full_at_twenty(pool: PgPool) {
    let f = Fx::new(pool).await;
    for _ in 0..20 {
        sqlx::query(
            "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
             values($1,$2,$3,$4,'{}','rate',now())",
        )
        .bind(Uuid::now_v7())
        .bind(f.owner)
        .bind(f.parent)
        .bind(Uuid::now_v7())
        .execute(&f.pool)
        .await
        .unwrap();
    }
    fill_rate(&f.pool, f.parent).await;
    let error = f.spawn_direct().await.unwrap_err();
    assert_eq!(error.code, Code::SpawnQueueFull, "{}", error.message);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn guard_queues_on_low_disk_and_names_cleanup_candidates(pool: PgPool) {
    let f = Fx::new(pool).await;
    let cleanup = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/deepseek-v4.1-flash",
        "idle",
    )
    .await;
    sqlx::query("update conversations set title='T3',worktree=$2 where id=$1")
        .bind(cleanup)
        .bind(json!({"path":"/data/worktrees/t3","branch":"canterlot/t3","state":"active"}))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update conversation_derived set activity_state='idle',activity_since=now()-interval '3 hours' where conversation_id=$1")
        .bind(cleanup)
        .execute(&f.pool)
        .await
        .unwrap();
    low_disk(&f.pool, f.machine).await;
    let answer = f.spawn_direct().await.unwrap();
    assert_eq!(answer["status"], "queued", "{answer}");
    assert_eq!(answer["reason"], "resources", "{answer}");
    let detail = answer["detail"].as_str().unwrap();
    assert!(detail.contains("disk 12 GB free < 30 GB"), "{detail}");
    assert!(
        detail.contains("T3 idle 3 h: session_accept frees their worktrees and target dirs"),
        "{detail}"
    );

    let preview = f
        .call("routing.preview", json!({"task_type":"bugfix"}))
        .await
        .unwrap();
    assert_eq!(preview["guard"]["blocked"], true, "{preview}");
    assert_eq!(preview["guard"]["stale"], false, "{preview}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_now_bypasses_the_guard_but_not_the_whitelist(pool: PgPool) {
    let f = Fx::new(pool).await;
    low_disk(&f.pool, f.machine).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    let started = f.runtime_call(queue_id, true).await.unwrap();
    assert_eq!(started.status, "started", "{started:?}");
    assert!(started.session_id.is_some(), "{started:?}");

    // Второй элемент встаёт в очередь при низком диске, затем пара пропадает из
    // белого списка: «Запустить сейчас» не обходит список.
    let queued = f.spawn_direct().await.unwrap();
    assert_eq!(queued["status"], "queued", "{queued}");
    let queue_id: Uuid = serde_json::from_value(queued["queue_id"].clone()).unwrap();
    sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
        .bind(f.owner)
        .bind(json!([{"harness":"claude_code","model":"opus[1m]"}]))
        .execute(&f.pool)
        .await
        .unwrap();
    let refused = f.runtime_call(queue_id, true).await.unwrap();
    assert_eq!(refused.status, "failed", "{refused:?}");
    assert_eq!(refused.reason.as_deref(), Some("not_in_whitelist"));
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "failed");
    assert!(
        notes_for(&f.pool, f.parent)
            .await
            .iter()
            .any(|note| note.contains("not_in_whitelist")),
        "failure note"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn expiry_after_expires_at_across_a_restart(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    sqlx::query("update spawn_queue set expires_at=now()-interval '1 second',not_before=now()-interval '1 second' where id=$1")
        .bind(queue_id)
        .execute(&f.pool)
        .await
        .unwrap();
    // Новый запуск хаба: задание берётся из базы.
    f.run_dequeue(queue_id, Utc::now()).await;
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "expired");
    let notes = notes_for(&f.pool, f.parent).await;
    assert!(
        notes.iter().any(|note| note.contains("expired")),
        "{notes:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn abandon_fails_the_item_and_frees_the_next(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let first = f.spawn_direct().await.unwrap();
    let first_id: Uuid = serde_json::from_value(first["queue_id"].clone()).unwrap();
    let second = f.spawn_direct().await.unwrap();
    let second_id: Uuid = serde_json::from_value(second["queue_id"].clone()).unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    jobs::abandon_in_tx(
        &mut tx,
        f.owner,
        Some(f.parent),
        &json!({"action":"spawn_dequeue","queue_id":first_id}),
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, first_id).await;
    assert_eq!(state, "failed");
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, second_id).await;
    assert_eq!(state, "queued");

    // Следующий элемент запускается тем же путём, что и обычно.
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, second_id).await;
    let outcome = f.run_dequeue(second_id, Utc::now()).await;
    assert!(matches!(outcome, JobOutcome::Done(_)), "{outcome:?}");
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, second_id).await;
    assert_eq!(state, "started");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn parent_stop_cancels_items_with_a_note(pool: PgPool) {
    let f = Fx::new(pool).await;
    // Очередь ведёт делегированная сессия: её и останавливает прямой родитель.
    let target = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/beam-free",
        "working",
    )
    .await;
    canterlot_hub::executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,'{\"chain_depth\":1,\"args\":{\"brief\":\"b\"}}','rate',now())",
    )
    .bind(queue_id)
    .bind(f.owner)
    .bind(target)
    .bind(Uuid::now_v7())
    .execute(&f.pool)
    .await
    .unwrap();
    let stopped = f
        .call("sessions.stop", json!({"session_id": target}))
        .await
        .unwrap();
    assert_eq!(stopped["status"], "stopping", "{stopped}");
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "cancelled");
    let notes: Vec<String> = sqlx::query_scalar(
        "select payload->>'text' from scheduled_jobs
          where conversation_id=$1 and payload->>'action'='note'",
    )
    .bind(target)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert!(
        notes.iter().any(|note| note.contains("queued spawn")),
        "{notes:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn policy_change_while_waiting_is_recomputed(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/deepseek-v4.1-flash","effort":"high"}
            ]}},
            "max_parallel": {"opencode/opencode-go/deepseek-v4.1-flash": 1}
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    let busy = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/deepseek-v4.1-flash",
        "working",
    )
    .await;
    let queued = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"Почини"}),
        )
        .await
        .unwrap();
    assert_eq!(queued["status"], "queued", "{queued}");
    assert_eq!(queued["reason"], "concurrency", "{queued}");
    let queue_id: Uuid = serde_json::from_value(queued["queue_id"].clone()).unwrap();

    // Политика сменилась: первым идёт beam-free, а занятый исполнитель освободился.
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"},
                {"harness":"opencode","model":"opencode-go/deepseek-v4.1-flash","effort":"high"}
            ]}}
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update conversation_derived set activity_state='stopped' where conversation_id=$1",
    )
    .bind(busy)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, queue_id).await;
    f.run_dequeue(queue_id, Utc::now()).await;
    let (state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "started");
    let model: String = sqlx::query_scalar("select model from conversations where id=$1")
        .bind(child.unwrap())
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(model, "opencode-go/beam-free");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn two_spawns_racing_for_the_last_slot_start_one_and_queue_one(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"}
            ]}},
            "max_parallel": {"opencode/opencode-go/beam-free": 1}
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    let args = json!({"route":{"task_type":"bugfix"},"brief":"Почини"});
    let first_caller = f.caller();
    let second_caller = f.caller();
    let first = session_tools::call(&f.context, &first_caller, "sessions.spawn", args.clone());
    let second = session_tools::call(&f.context, &second_caller, "sessions.spawn", args);
    let (first, second) = tokio::join!(first, second);
    let statuses = [
        first.unwrap()["status"].clone(),
        second.unwrap()["status"].clone(),
    ];
    assert!(
        statuses.iter().any(|status| status == "started"),
        "{statuses:?}"
    );
    assert!(
        statuses.iter().any(|status| status == "queued"),
        "{statuses:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sessions_list_reports_waiting_spawns(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    assert_eq!(answer["status"], "queued");
    let list = f.call("sessions.list", json!({})).await.unwrap();
    assert_eq!(list["queued_spawns"]["count"], 1, "{list}");
    assert!(list["queued_spawns"]["nearest"].is_string(), "{list}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_tool_writes_policy_or_whitelist_or_modes(pool: PgPool) {
    let f = Fx::new(pool).await;
    let snapshot = |pool: PgPool| async move {
        sqlx::query_scalar::<_, String>("select string_agg(o::text,',' order by id) from owners o")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let before = snapshot(f.pool.clone()).await;
    for tool in TOOLS {
        for args in [
            json!({}),
            json!({"queue_id": Uuid::now_v7()}),
            json!({"policy":null,"revision":99}),
        ] {
            let _ = session_tools::call(&f.context, &f.caller(), tool, args).await;
        }
    }
    assert_eq!(snapshot(f.pool.clone()).await, before);
}

async fn queue_seed(pool: &PgPool, _machine: Uuid, parent: Uuid, owner: Uuid, state: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before,state,result)
         values($1,$2,$3,$4,'{\"chain_depth\":0,\"args\":{\"brief\":\"b\"}}','rate',now(),$5,$6)",
    )
    .bind(id)
    .bind(owner)
    .bind(parent)
    .bind(Uuid::now_v7())
    .bind(state)
    .bind(if state == "started" {
        json!({"code": null})
    } else {
        Value::Null
    })
    .execute(pool)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn http_lists_and_cancels_and_the_snapshot_carries_the_queue(pool: PgPool) {
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
    let parent = testkit::seed_conversation(&pool, owner, machine).await;
    let queue_id = queue_seed(&pool, machine, parent, owner, "queued").await;
    let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();

    let list = client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{parent}/spawn-queue"),
            None,
            None,
        )
        .await;
    assert_eq!(list.status, 200, "{:?}", list.body);
    assert_eq!(list.json::<Value>()["items"][0]["id"], queue_id.to_string());

    let snapshot = client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{parent}/snapshot?history_limit=10"),
            None,
            None,
        )
        .await;
    assert_eq!(snapshot.status, 200, "{:?}", snapshot.body);
    assert_eq!(
        snapshot.json::<Value>()["data"]["spawn_queue"][0]["id"],
        queue_id.to_string()
    );

    let path = format!("/api/v1/conversations/{parent}/spawn-queue/{queue_id}");
    let cancelled = client
        .send(Method::DELETE, &path, Some("cancel-queue"), None)
        .await;
    assert_eq!(cancelled.status, 200, "{:?}", cancelled.body);
    assert_eq!(cancelled.json::<Value>()["status"], "cancelled");
    // Повтор с тем же ключом отвечает сохранённым ответом.
    let again = client
        .send(Method::DELETE, &path, Some("cancel-queue"), None)
        .await;
    assert_eq!(again.status, 200);
    assert_eq!(again.json::<Value>()["status"], "cancelled");

    let started = queue_seed(&pool, machine, parent, owner, "started").await;
    let missing = client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}/spawn-queue/{started}"),
            Some("cancel-started"),
            None,
        )
        .await;
    assert_eq!(missing.status, 200);
    assert_eq!(missing.json::<Value>()["status"], "already_started");

    let unknown = client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{parent}/spawn-queue/{}/start-now",
                Uuid::now_v7()
            ),
            Some("start-unknown"),
            None,
        )
        .await;
    assert_eq!(unknown.status, 404, "{:?}", unknown.body);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trash_cancels_waiting_spawns(pool: PgPool) {
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
    let parent = testkit::seed_conversation(&pool, owner, machine).await;
    let queue_id = queue_seed(&pool, machine, parent, owner, "queued").await;
    let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
    let trashed = client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}"),
            Some("trash-queue"),
            None,
        )
        .await;
    assert_eq!(trashed.status, 204, "{:?}", trashed.body);
    let (state, _reason, _detail, _child, _not_before) = queue_row(&pool, queue_id).await;
    assert_eq!(state, "cancelled");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_blocked_head_does_not_hold_a_ready_later_item(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {
                "slow": {"title": "Медленный", "candidates": [
                    {"harness":"opencode","model":"opencode-go/deepseek-v4.1-flash","effort":"high"}
                ]},
                "fast": {"title": "Быстрый", "candidates": [
                    {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"}
                ]}
            },
            "providers": {"opencode": {"max_weekly_percent": 95}},
            "unlimited_models": ["opencode-go/beam-free"]
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into provider_limits(provider,weekly_used_permille,weekly_observed_at,weekly_resets_at)
         values('opencode',990,now(),now()+interval '4 hours') on conflict(provider) do update
         set weekly_used_permille=990,weekly_observed_at=now(),weekly_resets_at=now()+interval '4 hours'",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    let insert = |id: Uuid, route: &str, created: DateTime<Utc>| {
        let query = sqlx::query(
            "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before,created_at)
             values($1,$2,$3,$4,$5,'limits',now(),$6)",
        );
        (id, query, route.to_owned(), created)
    };
    let slow = Uuid::now_v7();
    let fast = Uuid::now_v7();
    for (id, query, route, created) in [
        insert(slow, "slow", Utc::now() - chrono::TimeDelta::minutes(2)),
        insert(fast, "fast", Utc::now() - chrono::TimeDelta::minutes(1)),
    ] {
        query
            .bind(id)
            .bind(f.owner)
            .bind(f.parent)
            .bind(Uuid::now_v7())
            .bind(json!({"chain_depth":0,"args":{"route":{"task_type":route},"brief":"b"}}))
            .bind(created)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    let outcome = f.run_dequeue(fast, Utc::now()).await;
    assert!(matches!(outcome, JobOutcome::Done(_)), "{outcome:?}");
    let (slow_state, _r, _d, _c, _n) = queue_row(&f.pool, slow).await;
    assert_eq!(slow_state, "queued", "blocked head stays queued");
    let (fast_state, _r, _d, child, _n) = queue_row(&f.pool, fast).await;
    assert_eq!(fast_state, "started");
    let model: String = sqlx::query_scalar("select model from conversations where id=$1")
        .bind(child.unwrap())
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(model, "opencode-go/beam-free");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_blocked_item_is_rescheduled_and_the_tick_returns(pool: PgPool) {
    let f = Fx::new(pool).await;
    low_disk(&f.pool, f.machine).await;
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,$5,'resources',now()-interval '1 second')",
    )
    .bind(queue_id)
    .bind(f.owner)
    .bind(f.parent)
    .bind(Uuid::now_v7())
    .bind(json!({"chain_depth":0,"args":{"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}}))
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into scheduled_jobs(id,owner_id,conversation_id,kind,run_at,payload)
         values($1,$2,$3,'session_task',now()-interval '1 second',$4)",
    )
    .bind(Uuid::now_v7())
    .bind(f.owner)
    .bind(f.parent)
    .bind(json!({"action":"spawn_dequeue","queue_id":queue_id}))
    .execute(&f.pool)
    .await
    .unwrap();
    let scheduler = Scheduler::new(
        f.pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig::default(),
    )
    .with_limits(f.context.registry.clone(), LimitsConfig::default())
    .with_session_tools((*f.context.config).clone())
    .with_orchestrator(&Default::default());
    let tick = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        scheduler.tick(Utc::now()),
    )
    .await;
    assert!(tick.is_ok(), "the scheduler tick must return, not spin");
    tick.unwrap().unwrap();
    let (state, not_before, attempts): (String, DateTime<Utc>, i32) =
        sqlx::query_as("select state,not_before,attempts from spawn_queue where id=$1")
            .bind(queue_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "queued");
    assert!(attempts >= 1);
    assert!(not_before > Utc::now() - chrono::TimeDelta::seconds(5));
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs
          where conversation_id=$1 and kind='session_task' and state='pending'
            and payload->>'action'='spawn_dequeue'",
    )
    .bind(f.parent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(pending <= 1, "bounded jobs, got {pending}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_queued_start_publishes_attributes_from_a_separate_job(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, queue_id).await;
    f.run_dequeue(queue_id, Utc::now()).await;
    let (_state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    let child = child.expect("child");
    // Обновление атрибутов идёт отдельным заданием: порядок счётчиков событий
    // не переворачивается внутри транзакции создания сессии.
    let payload: Option<Value> = sqlx::query_scalar(
        "select payload from scheduled_jobs
          where conversation_id=$1 and state='pending' and payload->>'action'='spawn_attributes'",
    )
    .bind(f.parent)
    .fetch_optional(&f.pool)
    .await
    .unwrap();
    let payload = payload.expect("attribute job");
    assert_eq!(payload["conversation_id"], child.to_string());
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: f.owner,
        conversation_id: Some(f.parent),
        run_at: Utc::now(),
        payload: payload.clone(),
        attempts: 0,
    };
    let thresholds = Thresholds::from(&ActivityConfig::default());
    let mut tx = f.pool.begin().await.unwrap();
    jobs::run_in_tx(
        &mut tx,
        &job,
        Utc::now(),
        Uuid::now_v7(),
        Some(&f.context.registry),
        &f.context.settle,
        &thresholds,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let updated: bool = sqlx::query_scalar(
        "select exists(select 1 from events
          where scope='conversation:'||$1::text and kind='conversation_updated')",
    )
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(updated, "attributes published after the separate job");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_now_expires_a_past_item(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    sqlx::query("update spawn_queue set expires_at=now()-interval '1 second' where id=$1")
        .bind(queue_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let outcome = f.runtime_call(queue_id, true).await.unwrap();
    assert_eq!(outcome.status, "expired", "{outcome:?}");
    let (state, _reason, _detail, _child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "expired");
    assert!(
        notes_for(&f.pool, f.parent)
            .await
            .iter()
            .any(|note| note.contains("expired")),
        "expiry note"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn http_start_now_commits_a_permanent_failure_and_its_note(pool: PgPool) {
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
    let parent = testkit::seed_conversation(&pool, owner, machine).await;
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,$5,'resources',now())",
    )
    .bind(queue_id)
    .bind(owner)
    .bind(parent)
    .bind(Uuid::now_v7())
    .bind(json!({"chain_depth":0,"args":{"harness":"nope","brief":"b"}}))
    .execute(&pool)
    .await
    .unwrap();
    // Воркер подключён: неизвестный харнесс — постоянный отказ, а не потеря связи.
    let registry = deps.registry.clone();
    let _session = testkit::fake_session(&registry, machine, &CAPABILITIES, true);
    let client = TestClient::in_process(http::router(http::AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{parent}/spawn-queue/{queue_id}/start-now");
    let first = client
        .send(Method::POST, &path, Some("start-fail"), None)
        .await;
    assert_eq!(first.status, 409, "{:?}", first.body);
    let (state, _reason, _detail, _child, _not_before) = queue_row(&pool, queue_id).await;
    assert_eq!(state, "failed", "the terminal transition is committed");
    let note: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs
          where conversation_id=$1 and payload->>'action'='note'
            and payload->>'text' like '%harness_unknown%')",
    )
    .bind(parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(note, "the failure note is committed");
    let replay = client
        .send(Method::POST, &path, Some("start-fail"), None)
        .await;
    assert_eq!(replay.status, 409);
    assert_eq!(
        replay.json::<Value>()["error"]["details"]["reason"],
        "harness_unknown"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_after_stop_is_refused_and_does_not_queue(pool: PgPool) {
    let f = Fx::new(pool).await;
    let target = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/beam-free",
        "working",
    )
    .await;
    let execution = canterlot_hub::executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    let stopped = f
        .call("sessions.stop", json!({"session_id": target}))
        .await
        .unwrap();
    assert_eq!(stopped["status"], "stopping", "{stopped}");
    let caller = Caller {
        owner_id: f.owner,
        conversation_id: target,
        execution_id: execution.execution_id,
        machine_id: f.machine,
        call_id: Uuid::now_v7(),
    };
    let error = session_tools::call(
        &f.context,
        &caller,
        "sessions.spawn",
        json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::ConversationClosed, "{}", error.message);
    let queued: i64 = sqlx::query_scalar("select count(*) from spawn_queue where parent_id=$1")
        .bind(target)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dequeue_cancels_items_of_a_stopped_parent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let target = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/beam-free",
        "working",
    )
    .await;
    canterlot_hub::executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    f.call("sessions.stop", json!({"session_id": target}))
        .await
        .unwrap();
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,$5,'rate',now())",
    )
    .bind(queue_id)
    .bind(f.owner)
    .bind(target)
    .bind(Uuid::now_v7())
    .bind(json!({"chain_depth":1,"args":{"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}}))
    .execute(&f.pool)
    .await
    .unwrap();
    f.run_dequeue_for(target, queue_id, Utc::now()).await;
    let (state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "cancelled");
    assert!(child.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_disconnected_machine_keeps_an_isolated_item_queued(pool: PgPool) {
    let mut f = Fx::new(pool).await;
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,$5,'resources',now()-interval '1 second')",
    )
    .bind(queue_id)
    .bind(f.owner)
    .bind(f.parent)
    .bind(Uuid::now_v7())
    .bind(json!({
        "chain_depth": 0,
        "args": {
            "harness": "opencode",
            "model": "opencode-go/beam-free",
            "brief": "b",
            "isolated": true
        }
    }))
    .execute(&f.pool)
    .await
    .unwrap();
    // Воркер отключился до первого тика после перезапуска хаба.
    f.session.take();
    f.run_dequeue(queue_id, Utc::now()).await;
    let (state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(
        state, "queued",
        "connectivity loss is not a permanent failure"
    );
    assert!(child.is_none());

    // Воркер вернулся: тот же элемент запускается.
    f.session = Some(testkit::fake_session(
        &f.context.registry,
        f.machine,
        &CAPABILITIES,
        true,
    ));
    make_due(&f.pool, queue_id).await;
    f.run_dequeue(queue_id, Utc::now()).await;
    let (state, _reason, _detail, child, _not_before) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "started");
    assert!(child.is_some());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_jobs_name_the_head_and_a_sweep_job_recovers_on_abandon(pool: PgPool) {
    let f = Fx::new(pool).await;
    let queue_id = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,$5,'rate',now()-interval '1 second')",
    )
    .bind(queue_id)
    .bind(f.owner)
    .bind(f.parent)
    .bind(Uuid::now_v7())
    .bind(json!({"chain_depth":0,"args":{"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}}))
    .execute(&f.pool)
    .await
    .unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    spawn_queue::wake_in_tx(&mut tx, f.owner, f.parent, Utc::now(), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let payload: Value = sqlx::query_scalar(
        "select payload from scheduled_jobs
          where conversation_id=$1 and state='pending' and payload->>'action'='spawn_dequeue'",
    )
    .bind(f.parent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["queue_id"], queue_id.to_string());

    // Задание-обход без queue_id исчерпало попытки: элемент остаётся, но задание
    // на него снова появляется.
    sqlx::query("delete from scheduled_jobs where conversation_id=$1")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    jobs::abandon_in_tx(
        &mut tx,
        f.owner,
        Some(f.parent),
        &json!({"action":"spawn_dequeue"}),
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (state, _r, _d, _c, _n) = queue_row(&f.pool, queue_id).await;
    assert_eq!(state, "queued");
    let recovered: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs
          where conversation_id=$1 and state='pending' and payload->>'action'='spawn_dequeue'
            and payload->>'queue_id'=$2)",
    )
    .bind(f.parent)
    .bind(queue_id.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(recovered, "a sweep job is recreated");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approval_cards_still_count_toward_the_hourly_limit(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
        .bind(f.owner)
        .bind(json!([{"harness":"opencode","model":"opencode-go/beam-free"}]))
        .execute(&f.pool)
        .await
        .unwrap();
    fill_rate(&f.pool, f.parent).await;
    let error = f
        .call(
            "sessions.spawn",
            json!({"harness":"claude_code","model":"opus[1m]","brief":"b"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::RateLimited, "{}", error.message);
    let cards: i64 = sqlx::query_scalar(
        "select count(*) from requests where conversation_id=$1 and kind='spawn_approval'",
    )
    .bind(f.parent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(cards, 0, "a rate-limited parent cannot grow cards");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stopped_delegate_with_a_queued_input_frees_its_slot(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"}
            ]}},
            "max_parallel": {"opencode/opencode-go/beam-free": 1}
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    let stopped = child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/beam-free",
        "stopped",
    )
    .await;
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,kind,payload,delivery_state)
         values($1,$2,$3,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(stopped)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at)
         values($1,$2,$3,1,'held-queued','[]','queued',$4,now())",
    )
    .bind(Uuid::now_v7())
    .bind(stopped)
    .bind(f.owner)
    .bind(command)
    .execute(&f.pool)
    .await
    .unwrap();
    let answer = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"b"}),
        )
        .await
        .unwrap();
    assert_eq!(
        answer["status"], "started",
        "a stopped session must not hold the slot: {answer}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_now_ignores_unknown_limits(pool: PgPool) {
    let f = Fx::new(pool).await;
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"}
            ]}},
            "unknown_limits": "skip"
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("delete from provider_limits")
        .execute(&f.pool)
        .await
        .unwrap();
    fill_rate(&f.pool, f.parent).await;
    let answer = f
        .call(
            "sessions.spawn",
            json!({"route":{"task_type":"bugfix"},"brief":"b"}),
        )
        .await
        .unwrap();
    assert_eq!(answer["status"], "queued", "{answer}");
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    let started = f.runtime_call(queue_id, true).await.unwrap();
    assert_eq!(started.status, "started", "{started:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn guard_uses_the_sample_time_not_the_arrival_time(pool: PgPool) {
    let f = Fx::new(pool).await;
    let old = Utc::now() - chrono::TimeDelta::minutes(10);
    let metrics = canterlot_proto::worker::v1::MachineMetrics {
        machine_time_ms: old.timestamp_millis() as u64,
        cpu_cores: 4,
        cpu_permille: 50,
        mem_total_bytes: 16_000_000_000,
        mem_available_bytes: 12_000_000_000,
        disks: vec![canterlot_proto::worker::v1::DiskUsage {
            label: "data".into(),
            total_bytes: 100_000_000_000,
            free_bytes: 90_000_000_000,
        }],
        load1_centi: 40,
        ..Default::default()
    };
    canterlot_hub::machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let verdict = canterlot_hub::session_tools::guard::check_in_tx(
        &mut conn,
        f.owner,
        f.machine,
        &Default::default(),
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(
        verdict.stale,
        "a delayed sample must not look fresh: {verdict:?}"
    );
    assert!(!verdict.blocked());
}

async fn stale_metrics(pool: &PgPool, owner: Uuid, machine: Uuid) {
    let old = Utc::now() - chrono::TimeDelta::minutes(10);
    let metrics = canterlot_proto::worker::v1::MachineMetrics {
        machine_time_ms: old.timestamp_millis() as u64,
        cpu_cores: 4,
        cpu_permille: 50,
        mem_total_bytes: 16_000_000_000,
        mem_available_bytes: 12_000_000_000,
        disks: vec![canterlot_proto::worker::v1::DiskUsage {
            label: "data".into(),
            total_bytes: 100_000_000_000,
            free_bytes: 12_000_000_000,
        }],
        load1_centi: 40,
        ..Default::default()
    };
    canterlot_hub::machine_info::apply_metrics(pool, owner, machine, &metrics, Uuid::now_v7())
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_start_answer_marks_stale_metrics_and_replays_identically(pool: PgPool) {
    let f = Fx::new(pool).await;
    stale_metrics(&f.pool, f.owner, f.machine).await;
    let call_id = Uuid::now_v7();
    let first = session_tools::call(
        &f.context,
        &f.caller_with(call_id),
        "sessions.spawn",
        json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
    )
    .await
    .unwrap();
    assert_eq!(first["status"], "started", "{first}");
    assert_eq!(first["machine_metrics"], "stale", "{first}");
    let replay = session_tools::call(
        &f.context,
        &f.caller_with(call_id),
        "sessions.spawn",
        json!({"harness":"opencode","model":"opencode-go/beam-free","brief":"b"}),
    )
    .await
    .unwrap();
    assert_eq!(replay, first, "transport replay stays byte-identical");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_queued_start_note_marks_stale_metrics(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    stale_metrics(&f.pool, f.owner, f.machine).await;
    let answer = f.spawn_direct().await.unwrap();
    let queue_id: Uuid = serde_json::from_value(answer["queue_id"].clone()).unwrap();
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    make_due(&f.pool, queue_id).await;
    f.run_dequeue(queue_id, Utc::now()).await;
    let notes = notes_for(&f.pool, f.parent).await;
    assert!(
        notes
            .iter()
            .any(|note| note.contains("Machine metrics are stale")),
        "{notes:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_views_name_the_future_session(pool: PgPool) {
    let f = Fx::new(pool).await;
    fill_rate(&f.pool, f.parent).await;
    let direct = f
        .call(
            "sessions.spawn",
            json!({
                "harness":"opencode","model":"opencode-go/beam-free","brief":"b",
                "title":"Прямой запуск"
            }),
        )
        .await
        .unwrap();
    let direct_id: Uuid = serde_json::from_value(direct["queue_id"].clone()).unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let views = spawn_queue::snapshot_views(&mut conn, f.parent)
        .await
        .unwrap();
    let view = views.iter().find(|view| view.id == direct_id).unwrap();
    assert_eq!(view.title.as_deref(), Some("Прямой запуск"));
    assert_eq!(view.harness.as_deref(), Some("opencode"));
    assert_eq!(view.model.as_deref(), Some("opencode-go/beam-free"));
    assert_eq!(view.task_type, None);
    assert_eq!(view.child_id, None);

    // Окно частоты свободно, но слот занят: маршрут даёт тип задачи и уровень.
    sqlx::query(
        "update conversations set created_at=now()-interval '61 minutes'
          where parent_conversation_id=$1",
    )
    .bind(f.parent)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.owner)
        .bind(json!({
            "task_types": {"bugfix": {"title": "Починка", "candidates": [
                {"harness":"opencode","model":"opencode-go/beam-free","effort":"medium"}
            ]}},
            "max_parallel": {"opencode/opencode-go/beam-free": 1}
        }))
        .execute(&f.pool)
        .await
        .unwrap();
    child_of(
        &f.pool,
        f.parent,
        "opencode",
        "opencode-go/beam-free",
        "working",
    )
    .await;
    let routed = f
        .call(
            "sessions.spawn",
            json!({
                "route":{"task_type":"bugfix","level":"high"},"brief":"b",
                "title":"Маршрут"
            }),
        )
        .await
        .unwrap();
    assert_eq!(routed["status"], "queued", "{routed}");
    let routed_id: Uuid = serde_json::from_value(routed["queue_id"].clone()).unwrap();
    let views = spawn_queue::snapshot_views(&mut conn, f.parent)
        .await
        .unwrap();
    let view = views.iter().find(|view| view.id == routed_id).unwrap();
    assert_eq!(view.title.as_deref(), Some("Маршрут"));
    assert_eq!(view.task_type.as_deref(), Some("bugfix"));
    assert_eq!(view.level.as_deref(), Some("high"));
    assert_eq!(view.harness, None, "routed pair is unknown until start");
    assert_eq!(view.model, None);
    assert_eq!(view.child_id, None);

    // Остановим занятого, запустим маршрутный элемент: child_id появляется.
    sqlx::query("update conversation_derived set activity_state='stopped' where conversation_id in (select id from conversations where parent_conversation_id=$1 and title is not null)")
        .bind(f.parent)
        .execute(&f.pool)
        .await
        .unwrap();
    make_due(&f.pool, routed_id).await;
    f.run_dequeue(routed_id, Utc::now()).await;
    let views = spawn_queue::snapshot_views(&mut conn, f.parent)
        .await
        .unwrap();
    let view = views.iter().find(|view| view.id == routed_id).unwrap();
    assert!(view.child_id.is_some(), "{view:?}");
}
