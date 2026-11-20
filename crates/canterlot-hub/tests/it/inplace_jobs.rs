//! Задачи на месте (`job_run`) на стороне хаба: допуск, очередь машины, факты воркера,
//! единственная доставка итога, потеря и отчёт при подключении.
//!
//! Две машины в одном хабе: A (заказчик) и B (`machine-b`). Воркеры играются фактами,
//! которые тест применяет теми же функциями, что принимают их из журнала.

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    config::{LimitsConfig, SessionToolsConfig},
    executions,
    inplace_jobs::{self, State, facts, logs, sweep},
    session::Registry,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit::{self, FakeSession},
};
use canterlot_proto::worker::v1::{
    JobFinished, JobOutcome, JobPhase, JobSnapshot, JobStarted, JobWaitingLease, JobsReport,
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const CAPS: [&str; 1] = ["jobs_inplace_v1"];

struct Fx {
    pool: PgPool,
    context: ToolContext,
    registry: Registry,
    owner: Uuid,
    a: Uuid,
    b: Uuid,
    conversation: Uuid,
    execution: Uuid,
    _sessions: Vec<FakeSession>,
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
        let mut sessions = vec![testkit::fake_session(
            &registry,
            seed.machine_id,
            &CAPS,
            true,
        )];
        sqlx::query(
            "update machines set availability='available',capabilities=array['jobs_inplace_v1'],
               allowed_roots='[\"/tmp\"]',accepts_remote_work=false where id=$1",
        )
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
        let b = Uuid::now_v7();
        sqlx::query(
            "insert into machines(id,owner_id,name,availability,capabilities,allowed_roots,jobs_max_running)
             values($1,$2,'machine-b','available',array['jobs_inplace_v1'],'[\"/tmp\"]',2)",
        )
        .bind(b)
        .bind(seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "update conversations set title='Заказчик',harness='claude_code',model='m',
               permission_mode='bypass',workdir='/tmp/w',memory_domain='personal' where id=$1",
        )
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
        let started = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
            .await
            .unwrap();
        if b_online {
            sessions.push(testkit::fake_session(&registry, b, &CAPS, true));
        }
        Self {
            context: ToolContext {
                memory: Default::default(),
                pool: pool.clone(),
                config: Arc::new(SessionToolsConfig::default()),
                limits: Arc::<LimitsConfig>::default(),
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
                registry: registry.clone(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            registry,
            pool,
            owner: seed.owner_id,
            a: seed.machine_id,
            b,
            conversation: seed.conversation_id,
            execution: started.execution_id,
            _sessions: sessions,
        }
    }

    fn caller(&self) -> Caller {
        self.caller_of(self.conversation, self.execution)
    }

    fn caller_of(&self, conversation: Uuid, execution: Uuid) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: conversation,
            execution_id: execution,
            machine_id: self.a,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }

    /// Заявка без ожидания: задача получает доставку `message`.
    async fn run(&self, shell: &str) -> Value {
        self.call(
            "job.run",
            json!({"workdir": "/tmp/w", "shell": shell, "wait_seconds": 0}),
        )
        .await
        .unwrap()
    }

    async fn run_on_b(&self, shell: &str) -> Result<Value, ToolError> {
        self.call(
            "job.run",
            json!({"machine": "machine-b", "workdir": "/tmp/w", "shell": shell, "wait_seconds": 0}),
        )
        .await
    }

    async fn switch(&self, on: bool) {
        sqlx::query("update machines set accepts_remote_work=$2 where id=$1")
            .bind(self.b)
            .bind(on)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn job(&self, id: Uuid) -> inplace_jobs::Job {
        let mut conn = self.pool.acquire().await.unwrap();
        inplace_jobs::load(&mut conn, id).await.unwrap().unwrap()
    }

    async fn state(&self, id: Uuid) -> String {
        self.job(id).await.state
    }

    async fn messages(&self, id: Uuid) -> i64 {
        sqlx::query_scalar(
            "select count(*) from scheduled_jobs
              where payload->>'action'='job_result' and payload->>'job_id'=$1",
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn commands(&self, machine: Uuid, kind: &str) -> i64 {
        sqlx::query_scalar("select count(*) from commands where machine_id=$1 and kind=$2")
            .bind(machine)
            .bind(kind)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn apply<F>(&self, op: F)
    where
        F: for<'c> FnOnce(
                &'c mut sqlx::PgConnection,
            )
                -> futures_util::future::BoxFuture<'c, Result<(), sqlx::Error>>
            + Send,
    {
        canterlot_hub::db::in_tx(&self.pool, op).await.unwrap();
    }

    async fn started(&self, machine: Uuid, id: Uuid) {
        let fact = JobStarted {
            job_id: id.to_string(),
            attempt: 1,
            pid: 4242,
            start_time: 99,
            started_at_ms: Utc::now().timestamp_millis() as u64,
        };
        self.apply(move |conn| {
            Box::pin(
                async move { facts::apply_started(conn, machine, &fact, Uuid::now_v7()).await },
            )
        })
        .await;
    }

    async fn finished(&self, machine: Uuid, id: Uuid, outcome: JobOutcome, exit: Option<i32>) {
        let fact = JobFinished {
            job_id: id.to_string(),
            attempt: 1,
            outcome: outcome as i32,
            exit_code: exit,
            duration_ms: 1500,
            log_bytes: 12,
            tail: b"hello\nworld\n".to_vec(),
            truncated: false,
        };
        self.apply(move |conn| {
            Box::pin(
                async move { facts::apply_finished(conn, machine, &fact, Uuid::now_v7()).await },
            )
        })
        .await;
    }

    /// Единственная заявка в базе и её id: ждёт, пока вызов в фоне её запишет.
    async fn only_job(&self) -> Uuid {
        for _ in 0..250 {
            let ids: Vec<Uuid> = sqlx::query_scalar("select id from jobs")
                .fetch_all(&self.pool)
                .await
                .unwrap();
            if let [id] = ids.as_slice() {
                return *id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the job was not created");
    }

    async fn set_mode(&self, mode: &str, pending: Option<&str>) {
        sqlx::query(
            "update conversations set permission_mode=$2,
               pending_settings=case when $3::text is null then '{}'::jsonb
                                     else jsonb_build_object('permission_mode',$3::text) end
             where id=$1",
        )
        .bind(self.conversation)
        .bind(mode)
        .bind(pending)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Второй разговор в том же домене на той же машине.
    async fn other_conversation(&self) -> (Uuid, Uuid) {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.a).await;
        sqlx::query(
            "update conversations set harness='claude_code',permission_mode='bypass',
               memory_domain='personal',workdir='/tmp/w' where id=$1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        let started = executions::start(&self.pool, self.owner, id, Uuid::now_v7())
            .await
            .unwrap();
        (id, started.execution_id)
    }
}

fn job_id(value: &Value) -> Uuid {
    serde_json::from_value(value["job_id"].clone()).unwrap()
}

// ───────── Единственная доставка ─────────

#[sqlx::test(migrations = "./migrations")]
async fn inline_success_answers_the_call_and_sends_no_message(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let context = fx.context.clone();
    let caller = fx.caller();
    let call = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "job.run",
            json!({"workdir": "/tmp/w", "shell": "echo hi", "wait_seconds": 10}),
        )
        .await
    });
    let id = fx.only_job().await;
    // Команда ушла на машину заказчика: место зарезервировано при диспетчеризации.
    assert_eq!(fx.state(id).await, "dispatched");
    assert_eq!(fx.commands(fx.a, "start_inplace_job").await, 1);
    fx.started(fx.a, id).await;
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    let reply = call.await.unwrap().unwrap();
    assert_eq!(reply["state"], "succeeded", "{reply}");
    assert_eq!(reply["exit_code"], 0);
    assert_eq!(reply["output_tail"], "hello\nworld\n");
    assert_eq!(reply["output_bytes"], 12);
    assert_eq!(fx.job(id).await.delivery.as_deref(), Some("inline"));
    assert_eq!(fx.messages(id).await, 0, "inline never sends a message");
}

#[sqlx::test(migrations = "./migrations")]
async fn waiter_timeout_then_finish_sends_exactly_one_message(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let reply = fx
        .call(
            "job.run",
            json!({"workdir": "/tmp/w", "shell": "sleep 100", "wait_seconds": 1}),
        )
        .await
        .unwrap();
    assert_eq!(reply["state"], "dispatched", "{reply}");
    let id = job_id(&reply);
    assert_eq!(fx.job(id).await.delivery.as_deref(), Some("message"));
    assert_eq!(fx.messages(id).await, 0, "nothing to say before the end");
    fx.started(fx.a, id).await;
    fx.finished(fx.a, id, JobOutcome::Failed, Some(3)).await;
    assert_eq!(fx.messages(id).await, 1);
    // Повтор итога ничего не добавляет.
    fx.finished(fx.a, id, JobOutcome::Failed, Some(3)).await;
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn caller_worker_deadline_shorter_than_the_waiter_gives_one_message(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let context = fx.context.clone();
    let caller = fx.caller();
    let call = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "job.run",
            json!({"workdir": "/tmp/w", "shell": "make", "wait_seconds": 60}),
        )
        .await
    });
    let id = fx.only_job().await;
    // Воркер заказчика обрывает вызов раньше, чем ждущий хаба: обработчик отброшен.
    call.abort();
    let _ = call.await;
    for _ in 0..100 {
        if fx.job(id).await.delivery.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fx.job(id).await.delivery.as_deref(), Some("message"));
    fx.started(fx.a, id).await;
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn hub_restart_during_the_wait_turns_every_undelivered_job_into_a_message(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let context = fx.context.clone();
    let caller = fx.caller();
    let call = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "job.run",
            json!({"workdir": "/tmp/w", "shell": "make", "wait_seconds": 60}),
        )
        .await
    });
    let running = fx.only_job().await;
    // Хаб умирает вместе с ждущим вызовом; строка остаётся без выбранной доставки.
    call.abort();
    let _ = call.await;
    for _ in 0..100 {
        if fx.job(running).await.delivery.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sqlx::query("update jobs set delivery=null where id=$1")
        .bind(running)
        .execute(&fx.pool)
        .await
        .unwrap();
    // Вторая задача уже закончилась, пока ждущего не было.
    let done = job_id(&fx.run("true").await);
    sqlx::query("update jobs set delivery=null where id=$1")
        .bind(done)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.started(fx.a, done).await;
    fx.finished(fx.a, done, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.messages(done).await, 0);
    assert_eq!(sweep::on_start(&fx.pool).await.unwrap(), 2);
    assert_eq!(fx.job(running).await.delivery.as_deref(), Some("message"));
    assert_eq!(
        fx.messages(done).await,
        1,
        "terminal at start: queued at once"
    );
    assert_eq!(fx.messages(running).await, 0);
    fx.finished(fx.a, running, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.messages(running).await, 1);
    // Второй старт ничего не меняет.
    assert_eq!(sweep::on_start(&fx.pool).await.unwrap(), 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_terminal_state_racing_the_timeout_is_delivered_once(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Порядок 1: итог раньше срока ожидания — отвечает вызов, сообщения нет.
    let first = job_id(&fx.run("a").await);
    sqlx::query("update jobs set delivery=null where id=$1")
        .bind(first)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.finished(fx.a, first, JobOutcome::Succeeded, Some(0))
        .await;
    fx.apply(move |conn| {
        Box::pin(async move {
            assert!(inplace_jobs::claim_inline_in_tx(conn, first).await?);
            inplace_jobs::fallback_to_message_in_tx(conn, first).await
        })
    })
    .await;
    assert_eq!(fx.job(first).await.delivery.as_deref(), Some("inline"));
    assert_eq!(fx.messages(first).await, 0);
    // Порядок 2: срок раньше итога — сообщение, а ждущий вызов потом ничего не берёт.
    let second = job_id(&fx.run("b").await);
    fx.finished(fx.a, second, JobOutcome::Succeeded, Some(0))
        .await;
    fx.apply(move |conn| {
        Box::pin(async move {
            assert!(!inplace_jobs::claim_inline_in_tx(conn, second).await?);
            Ok(())
        })
    })
    .await;
    assert_eq!(fx.messages(second).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn replay_by_call_id_and_by_idempotency_key_returns_the_stored_job(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let caller = fx.caller();
    let args = json!({"workdir": "/tmp/w", "shell": "make", "wait_seconds": 0});
    let first = session_tools::call(&fx.context, &caller, "job.run", args.clone())
        .await
        .unwrap();
    // Тот же вызов транспорта.
    let again = session_tools::call(&fx.context, &caller, "job.run", args.clone())
        .await
        .unwrap();
    assert_eq!(job_id(&first), job_id(&again));
    // Другой вызов с тем же ключом.
    let keyed =
        json!({"workdir": "/tmp/w", "shell": "make", "wait_seconds": 0, "idempotency_key": "k1"});
    let one = fx.call("job.run", keyed.clone()).await.unwrap();
    let two = fx.call("job.run", keyed.clone()).await.unwrap();
    assert_eq!(job_id(&one), job_id(&two));
    let count: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    // Итог уже есть: повтор отдаёт сохранённый результат.
    let id = job_id(&one);
    fx.started(fx.a, id).await;
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    let replay = fx.call("job.run", keyed).await.unwrap();
    assert_eq!(replay["state"], "succeeded");
    assert_eq!(replay["exit_code"], 0);
    assert_eq!(replay["output_tail"], "hello\nworld\n");
    assert_eq!(
        fx.messages(id).await,
        1,
        "a replay is not a second delivery"
    );
}

// ───────── Допуск ─────────

#[sqlx::test(migrations = "./migrations")]
async fn only_bypass_may_run_commands(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let args = json!({"workdir": "/tmp/w", "shell": "true", "wait_seconds": 0});
    for (mode, pending) in [
        ("read_only", None),
        ("ask", None),
        ("owner", None),
        ("", None),
        ("mystery", None),
        // Узкий режим вступит со следующего хода и уже учитывается.
        ("bypass", Some("ask")),
        ("bypass", Some("read_only")),
    ] {
        fx.set_mode(mode, pending).await;
        assert_eq!(
            code(fx.call("job.run", args.clone()).await),
            Code::ModeTooNarrow,
            "{mode}/{pending:?}"
        );
    }
    // Codex `ask` силён как bypass, но команды ему всё равно отказаны.
    sqlx::query("update conversations set harness='codex' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.set_mode("ask", None).await;
    assert_eq!(
        code(fx.call("job.run", args.clone()).await),
        Code::ModeTooNarrow
    );
    // Боковая копия не запускает команд, даже в bypass.
    sqlx::query("update conversations set harness='claude_code' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.set_mode("bypass", None).await;
    let (side, side_execution) = fx.other_conversation().await;
    sqlx::query("update conversations set side_of=$2 where id=$1")
        .bind(side)
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    let denied = session_tools::call(
        &fx.context,
        &fx.caller_of(side, side_execution),
        "job.run",
        args.clone(),
    )
    .await;
    assert_eq!(code(denied), Code::ModeTooNarrow);
    fx.call("job.run", args).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn the_directory_must_belong_to_the_callers_domain(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Рабочий каталог по правилу по умолчанию: другой домен.
    let work = json!({"workdir": "/home/dev/work/p", "shell": "true", "wait_seconds": 0});
    assert_eq!(
        code(fx.call("job.run", work.clone()).await),
        Code::DomainMismatch
    );
    // Вызывающий без домена не запускает ничего.
    sqlx::query("update conversations set memory_domain=null where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"workdir": "/tmp/w", "shell": "true", "wait_seconds": 0})
            )
            .await
        ),
        Code::DomainMismatch
    );
    sqlx::query("update conversations set memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.call("job.run", work).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn another_machine_needs_the_owners_switch_and_the_capability(pool: PgPool) {
    let fx = Fx::new(pool).await;
    assert_eq!(code(fx.run_on_b("true").await), Code::RemoteWorkDisabled);
    fx.switch(true).await;
    let reply = fx.run_on_b("true").await.unwrap();
    assert_eq!(reply["machine"], "machine-b");
    assert_eq!(fx.commands(fx.b, "start_inplace_job").await, 1);
    assert_eq!(fx.commands(fx.a, "start_inplace_job").await, 0);
    sqlx::query("update machines set capabilities=array['pilot_v1'] where id=$1")
        .bind(fx.b)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(code(fx.run_on_b("true").await), Code::CapabilityMissing);
    sqlx::query("update machines set capabilities=array['pilot_v1'] where id=$1")
        .bind(fx.a)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"workdir": "/tmp/w", "shell": "x", "wait_seconds": 0})
            )
            .await
        ),
        Code::CapabilityMissing
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_queued_job_is_rechecked_at_dispatch_and_ends_canceled_with_the_reason(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.switch(true).await;
    // У B место на двоих: две занимают его, третья ждёт в очереди.
    let first = job_id(&fx.run_on_b("one").await.unwrap());
    let second = job_id(&fx.run_on_b("two").await.unwrap());
    let queued = job_id(&fx.run_on_b("three").await.unwrap());
    assert_eq!(fx.state(queued).await, "queued");
    // Выключатель погас, пока задача стояла в очереди: старт уже закоммиченных идёт,
    // ожидающие закрываются.
    fx.switch(false).await;
    fx.started(fx.b, first).await;
    fx.finished(fx.b, first, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(queued).await, "canceled");
    assert_eq!(
        fx.job(queued).await.end_reason.as_deref(),
        Some("remote_work_disabled")
    );
    assert_eq!(
        fx.state(second).await,
        "dispatched",
        "a committed start proceeds"
    );
    assert_eq!(fx.commands(fx.b, "start_inplace_job").await, 2);
    // Закрытая в очереди задача с выбранным сообщением доставляется как любая другая.
    assert_eq!(fx.messages(queued).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_narrowed_mode_closes_the_queued_job_at_dispatch(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.switch(true).await;
    let first = job_id(&fx.run_on_b("one").await.unwrap());
    let _second = job_id(&fx.run_on_b("two").await.unwrap());
    let narrowed = job_id(&fx.run_on_b("three").await.unwrap());
    assert_eq!(fx.state(narrowed).await, "queued");
    fx.set_mode("bypass", Some("ask")).await;
    fx.finished(fx.b, first, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(narrowed).await, "canceled");
    assert_eq!(
        fx.job(narrowed).await.end_reason.as_deref(),
        Some("mode_too_narrow")
    );
    // Сменился домен заказчика: то же самое.
    fx.set_mode("bypass", None).await;
    let _third = job_id(&fx.run_on_b("four").await.unwrap());
    let moved = job_id(&fx.run_on_b("five").await.unwrap());
    sqlx::query("update conversations set memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.finished(fx.b, _third, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(
        fx.job(moved).await.end_reason.as_deref(),
        Some("domain_mismatch")
    );
}

// ───────── Очередь и место ─────────

#[sqlx::test(migrations = "./migrations")]
async fn capacity_is_reserved_at_dispatch_and_the_queue_is_fifo(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.switch(true).await;
    let mut ids = Vec::new();
    for n in 0..4 {
        ids.push(job_id(&fx.run_on_b(&format!("job {n}")).await.unwrap()));
    }
    let states: Vec<String> = {
        let mut out = Vec::new();
        for id in &ids {
            out.push(fx.state(*id).await);
        }
        out
    };
    assert_eq!(states, ["dispatched", "dispatched", "queued", "queued"]);
    assert_eq!(fx.commands(fx.b, "start_inplace_job").await, 2);
    // Освободилось одно место: идёт третья, а не четвёртая.
    fx.started(fx.b, ids[0]).await;
    fx.finished(fx.b, ids[0], JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(ids[2]).await, "dispatched");
    assert_eq!(fx.state(ids[3]).await, "queued");
    fx.finished(fx.b, ids[1], JobOutcome::Failed, Some(1)).await;
    assert_eq!(fx.state(ids[3]).await, "dispatched");
    // Без числа в отчёте машина принимает четыре задачи.
    sqlx::query("update machines set jobs_max_running=null where id=$1")
        .bind(fx.b)
        .execute(&fx.pool)
        .await
        .unwrap();
    let mut more = Vec::new();
    for n in 0..4 {
        more.push(job_id(&fx.run_on_b(&format!("more {n}")).await.unwrap()));
    }
    let queued: i64 = sqlx::query_scalar("select count(*) from jobs where state='queued'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(queued, 2, "two of the new four fit beside the two running");
}

// ───────── Состояния и факты ─────────

#[sqlx::test(migrations = "./migrations")]
async fn facts_apply_forward_only(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("make").await);
    fx.started(fx.a, id).await;
    assert_eq!(fx.state(id).await, "running");
    let late = JobWaitingLease {
        job_id: id.to_string(),
        attempt: 1,
        lease: 1,
    };
    let a = fx.a;
    fx.apply(move |conn| {
        Box::pin(async move { facts::apply_waiting_lease(conn, a, &late, Uuid::now_v7()).await })
    })
    .await;
    assert_eq!(fx.state(id).await, "running", "an older state does not win");
    // Чужая машина и чужая попытка факты не меняют.
    fx.finished(fx.b, id, JobOutcome::Failed, Some(9)).await;
    assert_eq!(fx.state(id).await, "running");
    fx.finished(fx.a, id, JobOutcome::TimedOut, None).await;
    assert_eq!(fx.state(id).await, "timed_out");
    assert_eq!(fx.job(id).await.exit_code, None);
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    assert_eq!(
        fx.state(id).await,
        "timed_out",
        "a terminal row does not change"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn launch_interrupted_and_refused_workdir_end_failed_with_their_reason(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let a = job_id(&fx.run("one").await);
    fx.finished(fx.a, a, JobOutcome::LaunchInterrupted, None)
        .await;
    assert_eq!(fx.state(a).await, "failed");
    assert_eq!(
        fx.job(a).await.end_reason.as_deref(),
        Some("launch_interrupted")
    );
    let b = job_id(&fx.run("two").await);
    fx.finished(fx.a, b, JobOutcome::WorkdirRefused, None).await;
    assert_eq!(
        fx.job(b).await.end_reason.as_deref(),
        Some("workdir_refused")
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_report_on_connect_applies_forward(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let running = job_id(&fx.run("one").await);
    let finished = job_id(&fx.run("two").await);
    let a = fx.a;
    fx.apply(move |conn| {
        Box::pin(async move {
            let report = JobsReport {
                jobs: vec![
                    JobSnapshot {
                        job_id: running.to_string(),
                        attempt: 1,
                        phase: JobPhase::Running as i32,
                        pid: 7,
                        start_time: 8,
                        started_at_ms: 1,
                        ..Default::default()
                    },
                    JobSnapshot {
                        job_id: finished.to_string(),
                        attempt: 1,
                        phase: JobPhase::Finished as i32,
                        outcome: JobOutcome::Succeeded as i32,
                        exit_code: Some(0),
                        duration_ms: 10,
                        log_bytes: 3,
                        ..Default::default()
                    },
                ],
                observed_at_ms: 0,
            };
            facts::apply_report(conn, a, &report, Uuid::now_v7()).await
        })
    })
    .await;
    assert_eq!(fx.state(running).await, "running");
    assert_eq!(fx.state(finished).await, "succeeded");
    assert_eq!(fx.job(finished).await.exit_code, Some(0));
}

// ───────── Потеря ─────────

#[sqlx::test(migrations = "./migrations")]
async fn a_silent_machine_loses_the_job_and_a_late_finish_changes_nothing_else(pool: PgPool) {
    let fx = Fx::with(pool, false).await;
    fx.switch(true).await;
    let id = job_id(&fx.run_on_b("make").await.unwrap());
    fx.started(fx.b, id).await;
    // Машина B не подключена. Срок команды 30 минут, запас 10: пока он не вышел, задача идёт.
    let now = Utc::now();
    assert_eq!(
        sweep::lost_sweep(&fx.pool, &fx.registry, Duration::from_secs(600), now)
            .await
            .unwrap(),
        0
    );
    sqlx::query("update jobs set last_fact_at=now()-interval '2 hours' where id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        sweep::lost_sweep(&fx.pool, &fx.registry, Duration::from_secs(600), now)
            .await
            .unwrap(),
        1
    );
    let lost = fx.job(id).await;
    assert_eq!(lost.state, "lost");
    assert_eq!(lost.end_reason.as_deref(), Some("lost"));
    assert_eq!(fx.messages(id).await, 1, "lost is terminal and delivered");
    // Воркер вернулся и сообщает итог: он записан, остальное не тронуто.
    fx.finished(fx.b, id, JobOutcome::Succeeded, Some(0)).await;
    let after = fx.job(id).await;
    assert_eq!(after.state, "lost");
    assert_eq!(after.exit_code, None);
    assert_eq!(after.late_report.as_ref().unwrap()["state"], "succeeded");
    assert_eq!(after.late_report.as_ref().unwrap()["phase"], "finished");
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_lost_job_the_worker_reports_running_keeps_its_slot(pool: PgPool) {
    let fx = Fx::with(pool, false).await;
    fx.switch(true).await;
    let first = job_id(&fx.run_on_b("one").await.unwrap());
    let second = job_id(&fx.run_on_b("two").await.unwrap());
    let queued = job_id(&fx.run_on_b("three").await.unwrap());
    sqlx::query("update jobs set last_fact_at=now()-interval '2 hours' where id in ($1,$2)")
        .bind(first)
        .bind(second)
        .execute(&fx.pool)
        .await
        .unwrap();
    sweep::lost_sweep(&fx.pool, &fx.registry, Duration::from_secs(600), Utc::now())
        .await
        .unwrap();
    // Потеря освободила два места: третья ушла на машину.
    assert_eq!(fx.state(queued).await, "dispatched");
    // Первая оказалась живой: она снова занимает место, четвёртая ждёт.
    fx.started(fx.b, first).await;
    assert_eq!(fx.state(first).await, "lost");
    let fourth = job_id(&fx.run_on_b("four").await.unwrap());
    let fifth = job_id(&fx.run_on_b("five").await.unwrap());
    // Третья и «потерянная, но идущая» первая держат оба места.
    assert_eq!(
        fx.state(fourth).await,
        "queued",
        "the lost-but-running job holds a slot"
    );
    fx.finished(fx.b, queued, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(fourth).await, "dispatched");
    assert_eq!(fx.state(fifth).await, "queued");
}

// ───────── Отмена и доступ ─────────

#[sqlx::test(migrations = "./migrations")]
async fn cancel_ends_a_queued_job_at_once_and_asks_the_worker_for_a_running_one(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.switch(true).await;
    let running = job_id(&fx.run_on_b("one").await.unwrap());
    let _second = job_id(&fx.run_on_b("two").await.unwrap());
    let queued = job_id(&fx.run_on_b("three").await.unwrap());
    let reply = fx
        .call("job.cancel", json!({"job_id": queued}))
        .await
        .unwrap();
    assert_eq!(reply["state"], "canceled");
    assert_eq!(fx.commands(fx.b, "cancel_inplace_job").await, 0);
    fx.started(fx.b, running).await;
    let reply = fx
        .call("job.cancel", json!({"job_id": running}))
        .await
        .unwrap();
    assert_eq!(
        reply["state"], "running",
        "the state waits for the worker's word"
    );
    assert_eq!(reply["cancel_requested"], true);
    assert_eq!(fx.commands(fx.b, "cancel_inplace_job").await, 1);
    fx.call("job.cancel", json!({"job_id": running}))
        .await
        .unwrap();
    assert_eq!(
        fx.commands(fx.b, "cancel_inplace_job").await,
        1,
        "one cancel per attempt"
    );
    fx.finished(fx.b, running, JobOutcome::Canceled, None).await;
    assert_eq!(fx.state(running).await, "canceled");
}

#[sqlx::test(migrations = "./migrations")]
async fn another_conversation_cannot_read_list_or_cancel_the_job(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("secret").await);
    let (other, other_execution) = fx.other_conversation().await;
    let stranger = fx.caller_of(other, other_execution);
    for (tool, args) in [
        ("job.status", json!({"job_id": id})),
        ("job.log", json!({"job_id": id})),
        ("job.cancel", json!({"job_id": id})),
    ] {
        let denied = session_tools::call(&fx.context, &stranger, tool, args).await;
        assert_eq!(code(denied), Code::JobNotFound, "{tool}");
    }
    let listed = session_tools::call(&fx.context, &stranger, "jobs.list", json!({}))
        .await
        .unwrap();
    assert_eq!(listed["count"], 0);
    assert_eq!(
        fx.state(id).await,
        "dispatched",
        "the stranger's cancel did nothing"
    );
    // Своему разговору задача видна, а после смены его домена — нет.
    let own = fx.call("jobs.list", json!({})).await.unwrap();
    assert_eq!(own["count"], 1);
    sqlx::query("update conversations set memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        code(fx.call("job.status", json!({"job_id": id})).await),
        Code::JobNotFound
    );
    assert_eq!(fx.call("jobs.list", json!({})).await.unwrap()["count"], 0);
}

// ───────── Доставка сообщения ─────────

async fn deliver_job_result(fx: &Fx, id: Uuid) -> canterlot_hub::crunch::JobOutcome {
    use canterlot_hub::{
        activity::Thresholds,
        config::ActivityConfig,
        scheduler::{ClaimedJob, JobKind},
    };
    let claimed = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: fx.owner,
        // Задание поставлено заказчику; строка задачи могла потерять его позже.
        conversation_id: Some(fx.conversation),
        run_at: Utc::now(),
        payload: json!({"action": "job_result", "job_id": id}),
        attempts: 0,
    };
    let thresholds = Thresholds::from(&ActivityConfig::default());
    let mut tx = fx.pool.begin().await.unwrap();
    let outcome = session_tools::jobs::run_in_tx(
        &mut tx,
        &claimed,
        Utc::now(),
        Uuid::now_v7(),
        Some(&fx.registry),
        &fx.context.settle,
        &thresholds,
        None,
        None,
        &LimitsConfig::default(),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    outcome
}

async fn input_texts(fx: &Fx) -> Vec<String> {
    sqlx::query_scalar::<_, Value>(
        "select content from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(fx.conversation)
    .fetch_all(&fx.pool)
    .await
    .unwrap()
    .into_iter()
    .map(|content| content[0]["text"].as_str().unwrap().to_owned())
    .collect()
}

#[sqlx::test(migrations = "./migrations")]
async fn the_message_reaches_the_caller_once_with_an_eight_kib_tail(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("make").await);
    fx.started(fx.a, id).await;
    let big = vec![b'x'; 32 * 1024];
    let fact = JobFinished {
        job_id: id.to_string(),
        attempt: 1,
        outcome: JobOutcome::Failed as i32,
        exit_code: Some(2),
        duration_ms: 2500,
        log_bytes: 40_000,
        tail: big,
        truncated: false,
    };
    let a = fx.a;
    fx.apply(move |conn| {
        Box::pin(async move { facts::apply_finished(conn, a, &fact, Uuid::now_v7()).await })
    })
    .await;
    deliver_job_result(&fx, id).await;
    deliver_job_result(&fx, id).await;
    let texts = input_texts(&fx).await;
    assert_eq!(texts.len(), 1, "stable dedupe key: {texts:?}");
    let text = &texts[0];
    assert!(text.starts_with("[canterlot] Job "), "{text}");
    assert!(
        text.contains("failed") && text.contains("exit code 2"),
        "{text}"
    );
    assert!(
        text.len() < 9 * 1024,
        "the message tail is at most 8 KiB: {}",
        text.len()
    );
    assert!(text.contains("xxxx"));
}

#[sqlx::test(migrations = "./migrations")]
async fn an_archived_or_deleted_or_moved_caller_gets_no_message(pool: PgPool) {
    use canterlot_hub::crunch::JobOutcome as Outcome;
    let fx = Fx::new(pool).await;
    let archived = job_id(&fx.run("one").await);
    fx.finished(fx.a, archived, JobOutcome::Succeeded, Some(0))
        .await;
    sqlx::query("update conversations set archived_at=now() where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert!(matches!(
        deliver_job_result(&fx, archived).await,
        Outcome::Cancelled("caller_archived")
    ));
    sqlx::query("update conversations set archived_at=null,memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert!(matches!(
        deliver_job_result(&fx, archived).await,
        Outcome::Cancelled("domain_mismatch")
    ));
    assert!(input_texts(&fx).await.is_empty());
    // Удалённый заказчик: задача остаётся у владельца без заказчика.
    sqlx::query("update jobs set conversation_id=null where id=$1")
        .bind(archived)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert!(matches!(
        deliver_job_result(&fx, archived).await,
        Outcome::Cancelled("not_the_caller")
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn the_job_outlives_a_deleted_caller(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("deploy").await);
    sqlx::query("update jobs set conversation_id=null where id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.started(fx.a, id).await;
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    assert_eq!(fx.state(id).await, "succeeded");
    assert_eq!(fx.messages(id).await, 0, "nobody to tell");
}

// ───────── Журнал ─────────

#[sqlx::test(migrations = "./migrations")]
async fn the_log_is_the_hub_tail_while_running_and_the_attachment_after_the_end(pool: PgPool) {
    use canterlot_proto::worker::v1::{JobLogChunk, MediaHeader};
    use flate2::{Compression, write::GzEncoder};
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("make").await);
    fx.started(fx.a, id).await;
    let a = fx.a;
    for (offset, data) in [(0_u64, "alpha\n"), (6, "beta\n"), (11, "gamma\n")] {
        let chunk = JobLogChunk {
            job_id: id.to_string(),
            attempt: 1,
            offset,
            data: data.as_bytes().to_vec(),
        };
        fx.apply(move |conn| Box::pin(async move { facts::apply_chunk(conn, a, &chunk).await }))
            .await;
    }
    // Повтор и перекрытие куска не удваивают вывод.
    let overlap = JobLogChunk {
        job_id: id.to_string(),
        attempt: 1,
        offset: 6,
        data: b"beta\ngamma\ndelta\n".to_vec(),
    };
    fx.apply(move |conn| Box::pin(async move { facts::apply_chunk(conn, a, &overlap).await }))
        .await;
    let live = fx.call("job.log", json!({"job_id": id})).await.unwrap();
    assert_eq!(live["source"], "tail");
    assert_eq!(live["text"], "alpha\nbeta\ngamma\ndelta\n");
    let tail = fx
        .call("job.log", json!({"job_id": id, "offset": 6, "limit": 5}))
        .await
        .unwrap();
    assert_eq!(tail["text"], "beta\n");
    assert_eq!(tail["next_offset"], 11);
    // Итог, затем вложение: журнал читается из него по смещению и размеру.
    fx.finished(fx.a, id, JobOutcome::Succeeded, Some(0)).await;
    let directory = tempfile::tempdir().unwrap();
    let (_, store) = testkit::test_blob_store(directory.path());
    let full = "line one\nline two\nline three\n";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(full.as_bytes()).unwrap();
    let gz = encoder.finish().unwrap();
    let media = Uuid::now_v7();
    let header = logs::parse_header(&MediaHeader {
        media_id: media.to_string(),
        mime: "application/gzip".into(),
        size: gz.len() as u64,
        sha256: hex(&Sha256::digest(&gz)),
        job_id: id.to_string(),
        job_attempt: 1,
        ..Default::default()
    })
    .map_err(|_| ())
    .unwrap();
    let body = futures_util::stream::once(async move { Ok(bytes::Bytes::from(gz)) });
    logs::put(&fx.pool, &*store, fx.a, &header, Box::pin(body))
        .await
        .map_err(|error| format!("{error:?}"))
        .unwrap();
    // Чужая машина журнал не привязывает.
    let job = fx.job(id).await;
    assert_eq!(job.log_media_id, Some(media));
    let slice = logs::read(&*store, &job, Some(9), 8)
        .await
        .map_err(|_| ())
        .unwrap();
    assert_eq!(slice.source, "log");
    assert_eq!(slice.text, "line two");
    assert!(!slice.eof);
    let rest = logs::read(&*store, &job, Some(9), 1000)
        .await
        .map_err(|_| ())
        .unwrap();
    assert_eq!(rest.text, "line two\nline three\n");
    assert!(rest.eof);
    assert_eq!(rest.total_bytes, Some(full.len() as u64));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ───────── Команды и отказы ─────────

#[sqlx::test(migrations = "./migrations")]
async fn the_start_command_carries_the_job_and_needs_the_capability(pool: PgPool) {
    use canterlot_hub::outbox::{self, CommandRow};
    use canterlot_proto::worker::v1::command::Kind;
    let fx = Fx::new(pool).await;
    let reply = fx
        .call(
            "job.run",
            json!({
                "workdir": "/tmp/w", "shell": "echo $X", "wait_seconds": 0,
                "timeout_minutes": 5, "lease": "gate", "env": {"X": "1"},
            }),
        )
        .await
        .unwrap();
    let id = job_id(&reply);
    let row: CommandRow = sqlx::query_as("select * from commands where kind='start_inplace_job'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let command = outbox::to_proto(&row).unwrap();
    assert_eq!(command.execution_id, "");
    let Some(Kind::StartInPlaceJob(start)) = command.kind else {
        panic!("start command expected");
    };
    assert_eq!(start.job_id, id.to_string());
    assert_eq!(start.attempt, 1);
    assert_eq!(start.workdir, "/tmp/w");
    assert_eq!(start.shell, "echo $X");
    assert_eq!(start.timeout_seconds, 300);
    assert_eq!(
        start.lease,
        canterlot_proto::worker::v1::JobLease::Gate as i32
    );
    assert_eq!(start.env.len(), 1);
    assert_eq!(start.env[0].name, "X");
    // Сеанс без возможности команды не получает.
    let bare = Registry::new(Default::default());
    let _session = testkit::fake_session(&bare, fx.a, &["pilot_v1"], true);
    assert!(matches!(
        outbox::to_proto_for_session(&row, &bare),
        Err(outbox::OutboxError::CapabilityMissing)
    ));
    assert!(outbox::to_proto_for_session(&row, &fx.registry).is_ok());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_rejected_start_ends_the_job_failed_with_the_reason(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = job_id(&fx.run("make").await);
    let (command, attempt): (Uuid, i32) =
        sqlx::query_as("select id, dispatch_attempt from commands where kind='start_inplace_job'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    let a = fx.a;
    fx.apply(move |conn| {
        Box::pin(async move {
            canterlot_hub::ingest::apply_command_rejected_in_tx(
                conn,
                a,
                command,
                attempt,
                "capability_missing",
                Uuid::now_v7(),
            )
            .await
            .map_err(|_| sqlx::Error::Protocol("ingest".into()))
        })
    })
    .await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "failed");
    assert_eq!(job.end_reason.as_deref(), Some("capability_missing"));
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn arguments_are_checked_and_unknown_ones_refused(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for bad in [
        json!({"workdir": "/tmp/w", "shell": "x", "argv": ["a"]}),
        json!({"workdir": "rel", "shell": "x"}),
        json!({"workdir": "/tmp/w", "shell": " "}),
        json!({"workdir": "/tmp/w", "shell": "x", "wait_seconds": 61}),
        json!({"workdir": "/tmp/w", "shell": "x", "timeout_minutes": 0}),
        json!({"workdir": "/tmp/w", "shell": "x", "timeout_minutes": 1441}),
        json!({"workdir": "/tmp/w", "shell": "x", "lease": "other"}),
        json!({"workdir": "/tmp/w", "shell": "x", "env": {"1BAD": "v"}}),
    ] {
        assert_eq!(
            code(fx.call("job.run", bad.clone()).await),
            Code::InvalidArguments,
            "{bad}"
        );
    }
    let many: serde_json::Map<String, Value> =
        (0..33).map(|n| (format!("V{n}"), json!("v"))).collect();
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"workdir": "/tmp/w", "shell": "x", "env": many})
            )
            .await
        ),
        Code::InvalidArguments
    );
    let none: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(none, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn status_and_listing_report_machine_offline_and_state(pool: PgPool) {
    let fx = Fx::with(pool, false).await;
    fx.switch(true).await;
    let id = job_id(&fx.run_on_b("make").await.unwrap());
    let status = fx.call("job.status", json!({"job_id": id})).await.unwrap();
    assert_eq!(status["state"], "dispatched");
    assert_eq!(status["machine_offline"], true);
    let listed = fx
        .call(
            "jobs.list",
            json!({"machine": "machine-b", "state": "dispatched"}),
        )
        .await
        .unwrap();
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["jobs"][0]["machine_offline"], true);
    let empty = fx
        .call("jobs.list", json!({"state": "running"}))
        .await
        .unwrap();
    assert_eq!(empty["count"], 0);
    assert_eq!(
        code(fx.call("jobs.list", json!({"limit": 51})).await),
        Code::InvalidArguments
    );
    let _ = State::Queued;
}
