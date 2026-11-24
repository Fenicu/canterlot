//! Задачи на раннерах (этап B2) на стороне хаба: размещение (`machine: "auto"`), ожидание в
//! `waiting_runner`, подготовка и сбор несущей, проверка домена, отмена в каждом состоянии,
//! задачи владельца и `runners_list`.
//!
//! Несущая `carrier` (она же заказчик) и её раннеры. Воркеры и несущая играются фактами,
//! которые тест применяет теми же функциями, что принимают их из журнала.

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    config::{LimitsConfig, SessionToolsConfig},
    executions,
    inplace_jobs::{self, carrier, facts, owner, place, sweep},
    machines::{self, RunnerSpec},
    outbox::{self, CommandRow},
    session::Registry,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit::{self, FakeSession},
};
use canterlot_proto::worker::v1::{
    JobFinished, JobOutcome, JobStarted, OutputFile, OutputsCollectFailed, OutputsCollected,
    SourceCleaned, SourceStageFailed, SourceStaged, command::Kind,
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const CAPS: [&str; 1] = ["jobs_inplace_v1"];
const CARRIER_CAPS: [&str; 2] = ["jobs_inplace_v1", "runners_v1"];

struct Fx {
    pool: PgPool,
    context: ToolContext,
    registry: Registry,
    owner: Uuid,
    carrier: Uuid,
    conversation: Uuid,
    execution: Uuid,
    sessions: Vec<FakeSession>,
}

fn code(result: Result<Value, ToolError>) -> Code {
    result.expect_err("the call must be refused").code
}

fn job_id(value: &Value) -> Uuid {
    serde_json::from_value(value["job_id"].clone()).unwrap()
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        let registry = Registry::new(Default::default());
        let sessions = vec![testkit::fake_session(
            &registry,
            seed.machine_id,
            &CARRIER_CAPS,
            true,
        )];
        sqlx::query(
            "update machines set name='carrier', availability='available',
               capabilities=array['jobs_inplace_v1','runners_v1'], allowed_roots='[\"/tmp\"]',
               accepts_remote_work=false, jobs_max_running=2, is_carrier=true where id=$1",
        )
        .bind(seed.machine_id)
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
                cloud: None,
                registry: registry.clone(),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            registry,
            pool,
            owner: seed.owner_id,
            carrier: seed.machine_id,
            conversation: seed.conversation_id,
            execution: started.execution_id,
            sessions,
        }
    }

    /// Раннер несущей: ранг, метки, число мест; `online` — воркер подключён.
    async fn runner(
        &mut self,
        name: &str,
        rank: i32,
        labels: &[&str],
        max: i32,
        online: bool,
    ) -> Uuid {
        let added = machines::add_runner(
            &self.pool,
            self.owner,
            name,
            &RunnerSpec {
                parent: self.carrier,
                root_dir: format!("/srv/{name}"),
                rank,
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "update machines set capabilities=array['jobs_inplace_v1'], jobs_max_running=$2,
               availability='available' where id=$1",
        )
        .bind(added.machine_id)
        .bind(max)
        .execute(&self.pool)
        .await
        .unwrap();
        if online {
            self.connect(added.machine_id);
        }
        added.machine_id
    }

    fn connect(&mut self, machine: Uuid) {
        self.sessions
            .push(testkit::fake_session(&self.registry, machine, &CAPS, true));
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.conversation,
            execution_id: self.execution,
            machine_id: self.carrier,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }

    /// Заявка с `machine: "auto"` без ожидания; `extra` дополняет и заменяет поля.
    async fn auto(&self, extra: Value) -> Result<Value, ToolError> {
        let mut args = json!({
            "machine": "auto", "shell": "make test", "wait_seconds": 0,
            "source": {"repo": "/srv/src/app", "rev": REV},
        });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        self.call("job.run", args).await
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

    async fn command(&self, machine: Uuid, kind: &str) -> CommandRow {
        sqlx::query_as("select * from commands where machine_id=$1 and kind=$2 order by created_at desc limit 1")
            .bind(machine)
            .bind(kind)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn delivery_state(&self, command: Uuid) -> String {
        sqlx::query_scalar("select delivery_state from commands where id=$1")
            .bind(command)
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

    async fn staged(&self, from: Uuid, id: Uuid) {
        let fact = SourceStaged {
            job_id: id.to_string(),
            attempt: 1,
        };
        self.apply(move |conn| {
            Box::pin(async move {
                carrier::apply_source_staged(conn, from, &fact, Uuid::now_v7()).await
            })
        })
        .await;
    }

    async fn stage_failed(&self, id: Uuid, reason: &str) {
        let (from, fact) = (
            self.carrier,
            SourceStageFailed {
                job_id: id.to_string(),
                reason: reason.to_owned(),
                attempt: 1,
            },
        );
        self.apply(move |conn| {
            Box::pin(
                async move { carrier::apply_stage_failed(conn, from, &fact, Uuid::now_v7()).await },
            )
        })
        .await;
    }

    async fn collected(&self, id: Uuid, truncated: bool) {
        let (from, fact) = (
            self.carrier,
            OutputsCollected {
                job_id: id.to_string(),
                local_dir: format!("/var/outputs/{id}"),
                files: vec![OutputFile {
                    path: "out/report.txt".into(),
                    bytes: 42,
                }],
                truncated,
                attempt: 1,
            },
        );
        self.apply(move |conn| {
            Box::pin(async move {
                carrier::apply_outputs_collected(conn, from, &fact, Uuid::now_v7()).await
            })
        })
        .await;
    }

    async fn collect_failed(&self, id: Uuid, reason: &str) {
        let (from, fact) = (
            self.carrier,
            OutputsCollectFailed {
                job_id: id.to_string(),
                reason: reason.to_owned(),
                attempt: 1,
            },
        );
        self.apply(move |conn| {
            Box::pin(async move {
                carrier::apply_collect_failed(conn, from, &fact, Uuid::now_v7()).await
            })
        })
        .await;
    }

    async fn cleaned(&self, id: Uuid) {
        let (from, fact) = (
            self.carrier,
            SourceCleaned {
                job_id: id.to_string(),
                attempt: 1,
            },
        );
        self.apply(move |conn| {
            Box::pin(async move {
                carrier::apply_source_cleaned(conn, from, &fact, Uuid::now_v7()).await
            })
        })
        .await;
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

    /// Задача на машине, подготовка и запуск на ней закончены: `running`.
    async fn run_to_running(&self, machine: Uuid, id: Uuid) {
        self.staged(self.carrier, id).await;
        assert_eq!(self.state(id).await, "dispatched");
        self.started(machine, id).await;
        assert_eq!(self.state(id).await, "running");
    }

    async fn place(&self) -> usize {
        place::place_carrier(&self.pool, &self.registry, self.carrier)
            .await
            .unwrap()
    }
}

// ───────── Размещение ─────────

#[sqlx::test(migrations = "./migrations")]
async fn placement_takes_the_highest_rank_with_a_free_slot_and_the_carrier_last(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &["build-host"], 1, true).await;
    fx.runner("runner-b", 10, &["build-host"], 1, true).await;
    let first = fx.auto(json!({})).await.unwrap();
    assert_eq!(first["machine"], "runner-a", "{first}");
    assert_eq!(first["state"], "staging");
    // runner-a занят подготовкой: его место закреплено за первой задачей.
    let second = fx.auto(json!({})).await.unwrap();
    assert_eq!(second["machine"], "runner-b", "{second}");
    // Несущая — кандидат с рангом 0 и двумя местами.
    let third = fx.auto(json!({})).await.unwrap();
    assert_eq!(third["machine"], "carrier");
    let fourth = fx.auto(json!({})).await.unwrap();
    assert_eq!(fourth["machine"], "carrier");
    // Мест нигде нет: задача ждёт в хабе без машины.
    let waiting = fx.auto(json!({})).await.unwrap();
    assert_eq!(waiting["state"], "waiting_runner", "{waiting}");
    assert_eq!(waiting["machine"], Value::Null);
    assert_eq!(waiting["waiting_runner"], true);
    let job = fx.job(job_id(&waiting)).await;
    assert!(job.machine_id.is_none() && job.workdir.is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn placement_needs_every_label_and_skips_runners_without_them(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &["build-host"], 2, true).await;
    fx.runner("runner-b", 10, &["build-host", "kernel-6.12"], 1, true)
        .await;
    let reply = fx.auto(json!({"labels": ["kernel-6.12"]})).await.unwrap();
    assert_eq!(reply["machine"], "runner-b", "{reply}");
    // runner-b полон, остальные меток не имеют: ждать.
    let waiting = fx.auto(json!({"labels": ["kernel-6.12"]})).await.unwrap();
    assert_eq!(waiting["state"], "waiting_runner", "{waiting}");
    // Метки несущей берутся из её строки.
    sqlx::query("update machines set labels=array['kernel-6.12'] where id=$1")
        .bind(fx.carrier)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(fx.place().await, 1);
    assert_eq!(fx.state(job_id(&waiting)).await, "staging");
    assert_eq!(fx.job(job_id(&waiting)).await.machine_id, Some(fx.carrier));
}

#[sqlx::test(migrations = "./migrations")]
async fn equal_ranks_break_by_the_lower_machine_id(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let a = fx.runner("runner-a", 10, &[], 1, true).await;
    let b = fx.runner("runner-b", 10, &[], 1, true).await;
    let reply = fx.auto(json!({})).await.unwrap();
    let chosen = fx.job(job_id(&reply)).await.machine_id.unwrap();
    assert_eq!(chosen, a.min(b));
}

#[sqlx::test(migrations = "./migrations")]
async fn placement_kinds_restrict_where_a_job_may_go(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &[], 1, true).await;
    let local = fx.auto(json!({"placement": ["local"]})).await.unwrap();
    assert_eq!(local["machine"], "carrier", "{local}");
    let runner = fx.auto(json!({"placement": ["runner"]})).await.unwrap();
    assert_eq!(runner["machine"], "runner-a");
    // Раннер полон, а `local` не разрешён: ждёт, хотя у несущей есть место.
    let waiting = fx.auto(json!({"placement": ["runner"]})).await.unwrap();
    assert_eq!(waiting["state"], "waiting_runner", "{waiting}");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_paused_or_disconnected_runner_gets_no_new_binding(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let paused = fx.runner("runner-a", 30, &[], 2, true).await;
    fx.runner("runner-b", 20, &[], 2, false).await;
    sqlx::query("update machines set accepts_remote_work=false where id=$1")
        .bind(paused)
        .execute(&fx.pool)
        .await
        .unwrap();
    let reply = fx.auto(json!({"placement": ["runner"]})).await.unwrap();
    assert_eq!(reply["state"], "waiting_runner", "{reply}");
}

#[sqlx::test(migrations = "./migrations")]
async fn the_stage_command_names_the_target_and_the_workdir(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let on_runner = job_id(&fx.auto(json!({})).await.unwrap());
    let on_carrier = job_id(&fx.auto(json!({})).await.unwrap());
    let key = place::repo_key("/srv/src/app");
    assert!(
        key.starts_with("app-") && key.len() == "app-".len() + 12,
        "{key}"
    );
    assert_ne!(key, place::repo_key("/srv/other/app"));
    let job = fx.job(on_runner).await;
    assert_eq!(
        job.workdir.as_deref(),
        Some(format!("/srv/runner-a/jobs/personal/{on_runner}").as_str())
    );
    let local = fx.job(on_carrier).await;
    assert_eq!(
        local.workdir.as_deref(),
        Some(format!("/srv/src/.canterlot-jobs/app/{on_carrier}").as_str())
    );
    let rows: Vec<CommandRow> = sqlx::query_as(
        "select * from commands where machine_id=$1 and kind='stage_source' order by created_at",
    )
    .bind(fx.carrier)
    .fetch_all(&fx.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    let Some(Kind::StageSource(stage)) = outbox::to_proto(&rows[0]).unwrap().kind else {
        panic!("stage command expected");
    };
    assert_eq!(stage.job_id, on_runner.to_string());
    assert_eq!(stage.repo, "/srv/src/app");
    assert_eq!(stage.rev, REV);
    assert_eq!(stage.domain, "personal");
    assert_eq!(stage.target_machine_id, runner.to_string());
    assert_eq!(stage.target_root_dir, "/srv/runner-a");
    assert_eq!(stage.workdir, job.workdir.clone().unwrap());
    assert_eq!(stage.repo_key, key);
    let Some(Kind::StageSource(stage)) = outbox::to_proto(&rows[1]).unwrap().kind else {
        panic!("stage command expected");
    };
    // Цель — сама несущая: корень пуст.
    assert_eq!(stage.target_machine_id, fx.carrier.to_string());
    assert_eq!(stage.target_root_dir, "");
    // Команда несущей уходит только на соединении с `runners_v1`.
    let bare = Registry::new(Default::default());
    let _session = testkit::fake_session(&bare, fx.carrier, &["jobs_inplace_v1"], true);
    assert!(matches!(
        outbox::to_proto_for_session(&rows[0], &bare),
        Err(outbox::OutboxError::CapabilityMissing)
    ));
    assert!(outbox::to_proto_for_session(&rows[0], &fx.registry).is_ok());
}

// ───────── Ожидание и повторное размещение ─────────

#[sqlx::test(migrations = "./migrations")]
async fn a_slot_freeing_places_the_waiting_job(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let first = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    let second = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(fx.state(second).await, "waiting_runner");
    fx.run_to_running(runner, first).await;
    assert_eq!(fx.place().await, 0, "the only slot is still busy");
    // Раннер закончил: место свободно сразу, без сбора (результатов не просили).
    fx.finished(runner, first, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(first).await, "succeeded");
    assert_eq!(fx.place().await, 1);
    assert_eq!(fx.state(second).await, "staging");
    assert_eq!(fx.job(second).await.machine_id, Some(runner));
}

/// Освободившееся место будит службу расстановки: уведомление несёт несущую, и приходит оно
/// после фиксации (когда факт раннера применён в своей транзакции).
#[sqlx::test(migrations = "./migrations")]
async fn a_slot_freeing_wakes_the_placement_service_of_the_carrier(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    fx.run_to_running(runner, id).await;
    let mut listener = sqlx::postgres::PgListener::connect_with(&fx.pool)
        .await
        .unwrap();
    listener.listen("canterlot_place").await.unwrap();
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    let notification = tokio::time::timeout(Duration::from_secs(10), listener.recv())
        .await
        .expect("a notification")
        .unwrap();
    assert_eq!(notification.payload(), fx.carrier.to_string());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_runner_connecting_places_the_waiting_job(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, false).await;
    let waiting = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(fx.state(waiting).await, "waiting_runner");
    fx.connect(runner);
    let placed = sweep::place_for_machine(&fx.pool, &fx.registry, runner)
        .await
        .unwrap();
    assert_eq!(placed, 1);
    assert_eq!(fx.state(waiting).await, "staging");
}

#[sqlx::test(migrations = "./migrations")]
async fn turning_the_switch_on_places_the_waiting_job_and_the_sweep_is_a_safety_net(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    sqlx::query("update machines set accepts_remote_work=false where id=$1")
        .bind(runner)
        .execute(&fx.pool)
        .await
        .unwrap();
    let waiting = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(fx.state(waiting).await, "waiting_runner");
    sqlx::query("update machines set accepts_remote_work=true where id=$1")
        .bind(runner)
        .execute(&fx.pool)
        .await
        .unwrap();
    // Страховка: расстановка по таймеру находит несущую с ожидающими задачами сама.
    assert_eq!(sweep::place_sweep(&fx.pool, &fx.registry).await.unwrap(), 1);
    assert_eq!(fx.state(waiting).await, "staging");
}

#[sqlx::test(migrations = "./migrations")]
async fn two_hubs_placing_at_once_never_bind_one_slot_twice(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, false).await;
    // Два ожидающих задания, места нет: оба остаются в хабе.
    let jobs: Vec<Uuid> = {
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(job_id(
                &fx.auto(json!({"placement": ["runner"]})).await.unwrap(),
            ));
        }
        ids
    };
    // Второй хаб видит раннер на связи в своём реестре.
    let other = Registry::new(Default::default());
    let _other_session = testkit::fake_session(&other, runner, &CAPS, true);
    fx.connect(runner);
    for _ in 0..3 {
        let (a, b) = tokio::join!(
            place::place_carrier(&fx.pool, &fx.registry, fx.carrier),
            place::place_carrier(&fx.pool, &other, fx.carrier),
        );
        a.unwrap();
        b.unwrap();
    }
    let mut conn = fx.pool.acquire().await.unwrap();
    let occupied = inplace_jobs::occupied_in_tx(&mut conn, runner)
        .await
        .unwrap();
    assert_eq!(occupied, 1, "one slot is never bound twice");
    let mut staging = 0;
    for id in jobs {
        staging += i32::from(fx.state(id).await == "staging");
    }
    assert_eq!(staging, 1);
}

/// Заявка `auto` берёт допуск заказчика, а расстановка после него замки машин; диспетчер
/// машины берёт их в обратном порядке. Без общего порядка параллельные пути замыкают цикл, и
/// Postgres прерывает один из них: вызывающий видит `internal`. Замки кандидатов берутся до
/// допуска, и все вызовы проходят.
#[sqlx::test(migrations = "./migrations")]
async fn auto_submit_and_machine_dispatch_never_deadlock(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 5, &[], 2, true).await;
    let first = job_id(
        &fx.call("job.run", named("runner-a", json!({})))
            .await
            .unwrap(),
    );
    let second = job_id(
        &fx.call("job.run", named("runner-a", json!({})))
            .await
            .unwrap(),
    );
    sqlx::query("update machines set jobs_max_running=1 where id=$1")
        .bind(runner)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.staged(fx.carrier, first).await;
    fx.staged(fx.carrier, second).await;
    assert_eq!(fx.state(second).await, "queued");
    let fx = Arc::new(fx);
    let mut tasks = Vec::new();
    for round in 0..12 {
        let on = fx.clone();
        tasks.push(tokio::spawn(async move {
            on.auto(json!({"placement": ["runner"], "title": format!("auto {round}")}))
                .await
                .map(|_| ())
                .map_err(|error| format!("auto: {error:?}"))
        }));
        let pool = fx.pool.clone();
        tasks.push(tokio::spawn(async move {
            sweep::dispatch_machine(&pool, runner)
                .await
                .map_err(|error| format!("dispatch: {error}"))
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
}

// ───────── Отказы при заявке ─────────

#[sqlx::test(migrations = "./migrations")]
async fn auto_without_a_source_is_refused(pool: PgPool) {
    let fx = Fx::new(pool).await;
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"machine": "auto", "workdir": "/tmp/w", "shell": "true"})
            )
            .await
        ),
        Code::SourceRequired
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn labels_no_runner_has_are_refused_at_submit(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &["build-host"], 1, true).await;
    assert_eq!(
        code(fx.auto(json!({"labels": ["gpu"]})).await),
        Code::NoRunnerCanSatisfy
    );
    // Облако этапа C места пока не даёт: допустим один `cloud`.
    assert_eq!(
        code(fx.auto(json!({"placement": ["cloud"]})).await),
        Code::NoRunnerCanSatisfy
    );
    // Настроенный, но занятый или выключенный раннер — повод ждать, а не отказ.
    sqlx::query("update machines set accepts_remote_work=false where name='runner-a'")
        .execute(&fx.pool)
        .await
        .unwrap();
    let waiting = fx
        .auto(json!({"labels": ["build-host"], "placement": ["runner"]}))
        .await
        .unwrap();
    assert_eq!(waiting["state"], "waiting_runner", "{waiting}");
}

#[sqlx::test(migrations = "./migrations")]
async fn source_and_outputs_are_checked(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for bad in [
        json!({"source": {"repo": "/srv/src/app", "rev": "abc123"}}),
        json!({"source": {"repo": "/srv/src/app", "rev": "g".repeat(40)}}),
        json!({"source": {"repo": "src/app", "rev": REV}}),
        json!({"source": {"repo": "/srv/../etc", "rev": REV}}),
        json!({"outputs": ["../escape"]}),
        json!({"outputs": ["/abs/path"]}),
        // Несущая отказала бы им после запуска: хаб отказывает до него.
        json!({"outputs": ["test results"]}),
        json!({"outputs": ["out/$HOME"]}),
        json!({"outputs": ["out//log"]}),
        json!({"outputs": ["out/./log"]}),
        json!({"outputs": ["-rf"]}),
        json!({"outputs": ["x".repeat(513)]}),
        json!({"outputs": (0..33).map(|n| format!("out/{n}")).collect::<Vec<_>>()}),
        json!({"expected_minutes": 0}),
        json!({"placement": []}),
        json!({"placement": ["moon"]}),
    ] {
        assert_eq!(
            code(fx.auto(bad.clone()).await),
            Code::InvalidArguments,
            "{bad}"
        );
    }
    // Без `source` метки, результаты и место не принимаются, а `workdir` обязателен.
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"workdir": "/tmp/w", "shell": "true", "outputs": ["a"]})
            )
            .await
        ),
        Code::InvalidArguments
    );
    assert_eq!(
        code(fx.call("job.run", json!({"shell": "true"})).await),
        Code::InvalidArguments
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_named_runner_with_a_source_is_staged_without_choosing(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 5, &[], 1, true).await;
    fx.runner("runner-b", 50, &[], 1, true).await;
    let reply = fx
        .call(
            "job.run",
            json!({
                "machine": "runner-a", "shell": "true", "wait_seconds": 0,
                "source": {"repo": "/srv/src/app", "rev": REV},
            }),
        )
        .await
        .unwrap();
    assert_eq!(reply["machine"], "runner-a", "{reply}");
    let job = fx.job(job_id(&reply)).await;
    assert_eq!(job.state, "staging");
    assert_eq!(job.machine_id, Some(runner));
    assert!(job.remote);
    // Раннер без `source` требует `workdir` и допускается как раньше.
    assert_eq!(
        code(
            fx.call(
                "job.run",
                json!({"machine": "runner-a", "shell": "true", "wait_seconds": 0})
            )
            .await
        ),
        Code::InvalidArguments
    );
}

// ───────── Домен ─────────

#[sqlx::test(migrations = "./migrations")]
async fn the_source_must_lie_in_the_callers_memory_domain(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &[], 2, true).await;
    // Заказчик `personal`, репозиторий в рабочем корне: другой домен.
    assert_eq!(
        code(
            fx.auto(json!({"source": {"repo": "/home/dev/work/app", "rev": REV}}))
                .await
        ),
        Code::DomainMismatch
    );
    let ok = fx.auto(json!({})).await.unwrap();
    let job = fx.job(job_id(&ok)).await;
    assert_eq!(job.source_domain.as_deref(), Some("personal"));
    assert_eq!(job.domain, "personal");
    // Заказчик `work`: личный путь отказывается, рабочий проходит и ложится в `jobs/work`.
    sqlx::query("update conversations set memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(code(fx.auto(json!({})).await), Code::DomainMismatch);
    let work = fx
        .auto(json!({
            "source": {"repo": "/home/dev/work/app", "rev": REV},
            "placement": ["runner"],
        }))
        .await
        .unwrap();
    let id = job_id(&work);
    let job = fx.job(id).await;
    assert_eq!(job.source_domain.as_deref(), Some("work"));
    assert_eq!(
        job.workdir.as_deref(),
        Some(format!("/srv/runner-a/jobs/work/{id}").as_str())
    );
}

// ───────── Состояния, подготовка и сбор ─────────

#[sqlx::test(migrations = "./migrations")]
async fn the_result_waits_for_collection_and_goes_out_once(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out/report.txt", "out/log"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    assert_eq!(fx.state(id).await, "staging");
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 0);
    fx.staged(fx.carrier, id).await;
    // Подготовка закончена: задача ушла воркеру раннера.
    assert_eq!(fx.state(id).await, "dispatched");
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 1);
    fx.started(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    // Итог раннера сохранён, но задача не закончена: идёт сбор, сообщения ещё нет.
    let job = fx.job(id).await;
    assert_eq!(job.state, "collecting");
    assert_eq!(job.exit_code, Some(0));
    assert_eq!(job.outcome_state.as_deref(), Some("succeeded"));
    assert_eq!(fx.messages(id).await, 0);
    assert_eq!(fx.commands(fx.carrier, "collect_outputs").await, 1);
    assert_eq!(
        fx.commands(fx.carrier, "cleanup_source").await,
        0,
        "collect first"
    );
    let collect = fx.command(fx.carrier, "collect_outputs").await;
    let Some(Kind::CollectOutputs(collect)) = outbox::to_proto(&collect).unwrap().kind else {
        panic!("collect command expected");
    };
    assert_eq!(collect.paths, vec!["out/report.txt", "out/log"]);
    assert_eq!(collect.cap_bytes, 512 * 1024 * 1024);
    assert_eq!(collect.target_machine_id, runner.to_string());
    assert_eq!(collect.target_root_dir, "/srv/runner-a");
    fx.collected(id, false).await;
    // Сбор закончен: задача кончается сразу, очистка идёт следом и конец не держит.
    let job = fx.job(id).await;
    assert_eq!(job.state, "succeeded");
    assert_eq!(
        job.outputs_dir.as_deref(),
        Some(format!("/var/outputs/{id}").as_str())
    );
    assert!(job.outputs_note.is_none());
    assert!(job.cleaned_at.is_none());
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    assert_eq!(fx.messages(id).await, 1, "no waiting for the cleanup");
    fx.cleaned(id).await;
    assert!(fx.job(id).await.cleaned_at.is_some());
    assert_eq!(fx.messages(id).await, 1);
    // Повторные факты ничего не меняют и не множат сообщение.
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    fx.cleaned(id).await;
    fx.collected(id, false).await;
    assert_eq!(fx.state(id).await, "succeeded");
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn an_inline_waiter_sees_the_result_after_the_collection(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let context = fx.context.clone();
    let caller = fx.caller();
    let call = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "job.run",
            json!({
                "machine": "auto", "shell": "make", "wait_seconds": 20,
                "source": {"repo": "/srv/src/app", "rev": REV},
                "outputs": ["out"], "placement": ["runner"],
            }),
        )
        .await
    });
    let id = loop {
        let ids: Vec<Uuid> = sqlx::query_scalar("select id from jobs")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
        if let [id] = ids.as_slice() {
            break *id;
        }
        tokio::task::yield_now().await;
    };
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(id).await, "collecting");
    assert!(
        fx.job(id).await.delivery.is_none(),
        "the waiter has not taken a result yet"
    );
    // Ждущий вызов не ждёт очистки: подтверждения несущей нет, ответ всё равно приходит.
    fx.collected(id, false).await;
    let reply = call.await.unwrap().unwrap();
    assert_eq!(reply["state"], "succeeded", "{reply}");
    assert_eq!(reply["machine"], "runner-a");
    assert_eq!(reply["exit_code"], 0);
    assert_eq!(reply["outputs_dir"], format!("/var/outputs/{id}"));
    assert_eq!(fx.job(id).await.delivery.as_deref(), Some("inline"));
    assert_eq!(fx.messages(id).await, 0, "inline never sends a message");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_job_without_outputs_still_collects_to_clean_up(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Failed, Some(2)).await;
    // Собирать нечего: задача кончается сразу, очистка выдана.
    let job = fx.job(id).await;
    assert_eq!((job.state.as_str(), job.exit_code), ("failed", Some(2)));
    assert_eq!(fx.commands(fx.carrier, "collect_outputs").await, 0);
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    assert_eq!(fx.messages(id).await, 1);
    fx.cleaned(id).await;
    assert_eq!(fx.messages(id).await, 1);
    let cleanup = fx.command(fx.carrier, "cleanup_source").await;
    let Some(Kind::CleanupSource(cleanup)) = outbox::to_proto(&cleanup).unwrap().kind else {
        panic!("cleanup command expected");
    };
    assert_eq!(cleanup.job_id, id.to_string());
    assert_eq!(cleanup.domain, "personal");
    assert_eq!(cleanup.repo_key, place::repo_key("/srv/src/app"));
    assert_eq!(cleanup.target_root_dir, "/srv/runner-a");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_failed_staging_ends_the_job_failed_with_the_reason(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    fx.stage_failed(id, "no such commit").await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "failed");
    assert_eq!(
        job.end_reason.as_deref(),
        Some("staging_failed: no such commit")
    );
    assert_eq!(fx.messages(id).await, 1);
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 0);
    // Недоделанное убирается, место свободно.
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    let mut conn = fx.pool.acquire().await.unwrap();
    assert_eq!(
        inplace_jobs::occupied_in_tx(&mut conn, runner)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_failed_collection_keeps_the_runners_outcome_with_a_note(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    fx.collect_failed(id, "disk full").await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "succeeded");
    assert_eq!(job.exit_code, Some(0));
    assert_eq!(
        job.outputs_note.as_deref(),
        Some("outputs not collected: disk full")
    );
    assert!(job.outputs_dir.is_none());
    assert_eq!(fx.messages(id).await, 1);
    let status = fx.call("job.status", json!({"job_id": id})).await.unwrap();
    assert_eq!(status["outputs_note"], "outputs not collected: disk full");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_collection_timeout_finishes_with_the_runners_outcome_and_a_note(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Failed, Some(3)).await;
    // Срок не вышел: ничего не меняется.
    let now = Utc::now();
    assert_eq!(
        carrier::collect_timeout_sweep(&fx.pool, Duration::from_secs(600), now)
            .await
            .unwrap(),
        0
    );
    assert_eq!(fx.state(id).await, "collecting");
    let later = now + chrono::TimeDelta::minutes(11);
    assert_eq!(
        carrier::collect_timeout_sweep(&fx.pool, Duration::from_secs(600), later)
            .await
            .unwrap(),
        1
    );
    let job = fx.job(id).await;
    assert_eq!((job.state.as_str(), job.exit_code), ("failed", Some(3)));
    assert_eq!(
        job.outputs_note.as_deref(),
        Some("outputs not collected: timeout")
    );
    assert_eq!(fx.messages(id).await, 1);
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    // Опоздавшие факты несущей не воскрешают задачу и не множат сообщение.
    fx.collected(id, false).await;
    fx.cleaned(id).await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "failed");
    assert!(job.outputs_dir.is_none());
    assert!(job.cleaned_at.is_some());
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_cleanup_still_unconfirmed_after_the_job_ended_is_retried_up_to_a_bound(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    carrier::collect_timeout_sweep(
        &fx.pool,
        Duration::from_secs(1),
        Utc::now() + chrono::TimeDelta::minutes(1),
    )
    .await
    .unwrap();
    assert_eq!(fx.state(id).await, "succeeded");
    // Подтверждения нет: повторные выдачи, но не больше предела.
    for _ in 0..10 {
        carrier::cleanup_retry_sweep(
            &fx.pool,
            &fx.registry,
            Duration::from_secs(60),
            Utc::now() + chrono::TimeDelta::days(1),
        )
        .await
        .unwrap();
    }
    let sent = fx.commands(fx.carrier, "cleanup_source").await;
    assert_eq!(sent, i64::from(carrier::MAX_CLEANUP_ATTEMPTS));
    // Подтверждение закрывает очистку: больше выдач нет.
    fx.cleaned(id).await;
    carrier::cleanup_retry_sweep(
        &fx.pool,
        &fx.registry,
        Duration::from_secs(60),
        Utc::now() + chrono::TimeDelta::days(2),
    )
    .await
    .unwrap();
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, sent);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_staging_that_never_ends_fails_by_the_timeout(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(
        carrier::stage_timeout_sweep(&fx.pool, Duration::from_secs(900), Utc::now())
            .await
            .unwrap(),
        0
    );
    let later = Utc::now() + chrono::TimeDelta::minutes(16);
    assert_eq!(
        carrier::stage_timeout_sweep(&fx.pool, Duration::from_secs(900), later)
            .await
            .unwrap(),
        1
    );
    let job = fx.job(id).await;
    assert_eq!(job.state, "failed");
    assert_eq!(job.end_reason.as_deref(), Some("staging_failed: timeout"));
}

// ───────── Отмена ─────────

#[sqlx::test(migrations = "./migrations")]
async fn cancel_in_waiting_runner_ends_at_once_with_nothing_to_clean(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Несущая полна двумя задачами, раннеров нет: третья ждёт.
    fx.auto(json!({"placement": ["local"]})).await.unwrap();
    fx.auto(json!({"placement": ["local"]})).await.unwrap();
    let waiting = job_id(&fx.auto(json!({"placement": ["local"]})).await.unwrap());
    assert_eq!(fx.state(waiting).await, "waiting_runner");
    let before = fx.commands(fx.carrier, "cleanup_source").await;
    let reply = fx
        .call("job.cancel", json!({"job_id": waiting}))
        .await
        .unwrap();
    assert_eq!(reply["state"], "canceled", "{reply}");
    let job = fx.job(waiting).await;
    assert!(job.machine_id.is_none());
    assert_eq!(job.end_reason.as_deref(), Some("canceled_by_caller"));
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, before);
    assert_eq!(fx.messages(waiting).await, 1);
    // Расстановка такую задачу уже не берёт.
    assert_eq!(fx.place().await, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_in_staging_cleans_up_and_a_late_staged_fact_changes_nothing(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(fx.state(id).await, "staging");
    let reply = fx.call("job.cancel", json!({"job_id": id})).await.unwrap();
    assert_eq!(reply["state"], "canceled", "{reply}");
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    // Подготовка, которую несущая уже шла делать, закончилась позже: ничего не оживает.
    fx.staged(fx.carrier, id).await;
    assert_eq!(fx.state(id).await, "canceled");
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 0);
    let stage = fx.command(fx.carrier, "stage_source").await;
    assert_eq!(fx.delivery_state(stage.id).await, "completed");
    assert_eq!(fx.messages(id).await, 1);
}

/// Отмена во время подготовки: несущая успела убрать «пока ничего нет» и подтвердила, а её
/// задача подготовки закончила работу позже. Факт о готовности опоздавшей подготовки заново
/// просит очистку, иначе checkout и ссылка остаются на раннере навсегда.
#[sqlx::test(migrations = "./migrations")]
async fn a_late_staged_fact_for_a_finished_job_asks_for_the_cleanup_again(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    fx.call("job.cancel", json!({"job_id": id})).await.unwrap();
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    fx.cleaned(id).await;
    assert!(fx.job(id).await.cleaned_at.is_some());
    fx.staged(fx.carrier, id).await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "canceled");
    assert!(
        job.cleaned_at.is_none(),
        "the earlier cleanup no longer counts"
    );
    assert!(job.cleanup_sent_at.is_some());
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 2);
    // Подтверждение новой очистки закрывает дело; повтор факта о готовности лишнего не шлёт
    // после того, как очистка подтверждена.
    fx.cleaned(id).await;
    assert!(fx.job(id).await.cleaned_at.is_some());
    assert_eq!(fx.messages(id).await, 1);
}

/// Очистка не отвечает вовсе: задача уже закончена, сообщение и итог терминалу ушли,
/// а повтор очистки остаётся за службой по сроку.
#[sqlx::test(migrations = "./migrations")]
async fn a_cleanup_that_never_answers_does_not_delay_the_result(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(id).await, "collecting");
    fx.collect_failed(id, "ssh dropped").await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "succeeded");
    assert_eq!(
        job.outputs_note.as_deref(),
        Some("outputs not collected: ssh dropped")
    );
    assert_eq!(fx.messages(id).await, 1);
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    // Подтверждения нет сколько угодно: коллектор по сроку ничего не находит, повтор идёт.
    let later = Utc::now() + chrono::TimeDelta::hours(2);
    assert_eq!(
        carrier::collect_timeout_sweep(&fx.pool, Duration::from_secs(600), later)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        carrier::cleanup_retry_sweep(&fx.pool, &fx.registry, Duration::from_secs(600), later)
            .await
            .unwrap(),
        1
    );
    assert_eq!(fx.state(id).await, "succeeded");
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_of_a_queued_source_job_ends_at_once_and_cleans_up(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 1, &[], 2, true).await;
    let call = |extra: Value| {
        let mut args = json!({
            "machine": "runner-a", "shell": "true", "wait_seconds": 0,
            "source": {"repo": "/srv/src/app", "rev": REV},
        });
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };
    let first = job_id(&fx.call("job.run", call(json!({}))).await.unwrap());
    let second = job_id(&fx.call("job.run", call(json!({}))).await.unwrap());
    // Раннер при подготовке сообщил, что мест стало одно: вторая задача встанет в очередь.
    sqlx::query("update machines set jobs_max_running=1 where id=$1")
        .bind(runner)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.staged(fx.carrier, first).await;
    fx.staged(fx.carrier, second).await;
    assert_eq!(fx.state(first).await, "dispatched");
    assert_eq!(fx.state(second).await, "queued", "no free slot on runner-a");
    let reply = fx
        .call("job.cancel", json!({"job_id": second}))
        .await
        .unwrap();
    assert_eq!(reply["state"], "canceled", "{reply}");
    assert_eq!(fx.commands(runner, "cancel_inplace_job").await, 0);
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_of_a_running_job_asks_the_worker_and_then_cleans_up(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, id).await;
    let reply = fx.call("job.cancel", json!({"job_id": id})).await.unwrap();
    assert_eq!(reply["state"], "running");
    assert_eq!(reply["cancel_requested"], true);
    assert_eq!(fx.commands(runner, "cancel_inplace_job").await, 1);
    fx.finished(runner, id, JobOutcome::Canceled, None).await;
    // Отменённой команде собирать нечего: задача кончается сразу, остаётся очистка.
    assert_eq!(fx.state(id).await, "canceled");
    assert_eq!(fx.commands(fx.carrier, "collect_outputs").await, 0);
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
    fx.cleaned(id).await;
    assert_eq!(fx.state(id).await, "canceled");
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_in_collecting_is_ignored_and_the_job_ends_with_the_runners_outcome(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    let reply = fx.call("job.cancel", json!({"job_id": id})).await.unwrap();
    assert_eq!(reply["state"], "collecting", "{reply}");
    let job = fx.job(id).await;
    assert!(job.cancel_requested_at.is_none());
    fx.collected(id, false).await;
    assert_eq!(fx.state(id).await, "succeeded");
}

#[sqlx::test(migrations = "./migrations")]
async fn revoking_a_runner_ends_its_staging_jobs_lost(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    machines::revoke(&fx.pool, runner).await.unwrap();
    assert_eq!(fx.state(id).await, "lost");
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 1);
}

// ───────── Факты чужих машин ─────────

#[sqlx::test(migrations = "./migrations")]
async fn facts_from_anything_but_the_jobs_carrier_change_nothing(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let other = machines::add(&fx.pool, fx.owner, "other-carrier")
        .await
        .unwrap();
    machines::set_carrier(&fx.pool, other.machine_id, true)
        .await
        .unwrap();
    let plain = machines::add(&fx.pool, fx.owner, "plain").await.unwrap();
    let id = job_id(
        &fx.auto(json!({"outputs": ["out"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    // Сам раннер, другая несущая и обычная машина: ни один факт не принимается.
    for from in [runner, other.machine_id, plain.machine_id] {
        fx.staged(from, id).await;
        assert_eq!(fx.state(id).await, "staging", "{from}");
    }
    fx.staged(fx.carrier, id).await;
    fx.started(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(id).await, "collecting");
    // Чужой сбор результатов не заканчивает задачу.
    let fact = OutputsCollected {
        job_id: id.to_string(),
        local_dir: "/var/outputs/foreign".into(),
        files: vec![],
        truncated: false,
        attempt: 0,
    };
    let from = other.machine_id;
    fx.apply(move |conn| {
        Box::pin(async move {
            carrier::apply_outputs_collected(conn, from, &fact, Uuid::now_v7()).await
        })
    })
    .await;
    assert_eq!(fx.state(id).await, "collecting");
    assert!(fx.job(id).await.outputs_dir.is_none());
    // Флаг несущей, снятый владельцем, закрывает и своей несущей.
    machines::set_carrier(&fx.pool, fx.carrier, false)
        .await
        .unwrap();
    fx.collected(id, false).await;
    assert_eq!(fx.state(id).await, "collecting");
}

// ───────── Задачи владельца ─────────

fn owner_spec(extra: Value) -> String {
    let mut spec = json!({
        "shell": "make gate", "labels": ["kernel-6.12"],
        "source": {"repo": "/srv/src/app", "rev": REV},
        "outputs": ["result"],
    });
    for (key, value) in extra.as_object().unwrap() {
        spec[key] = value.clone();
    }
    spec.to_string()
}

#[sqlx::test(migrations = "./migrations")]
async fn an_owner_job_is_accepted_without_a_conversation_and_placed(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &["kernel-6.12"], 1, true).await;
    let id = owner::submit(&fx.pool, &fx.registry, fx.carrier, &owner_spec(json!({})))
        .await
        .unwrap();
    let job = fx.job(id).await;
    assert!(job.owner_job && job.conversation_id.is_none());
    assert_eq!(job.carrier_id, Some(fx.carrier));
    assert_eq!(job.domain, "personal");
    assert_eq!(job.source_domain.as_deref(), Some("personal"));
    assert_eq!(job.state, "staging");
    assert_eq!(job.machine_id, Some(runner));
    assert_eq!(job.outputs, vec!["result".to_owned()]);
    // Режим не проверяется: у задачи владельца нет разговора, а значит и сообщения.
    fx.staged(fx.carrier, id).await;
    fx.started(runner, id).await;
    fx.finished(runner, id, JobOutcome::Succeeded, Some(0))
        .await;
    fx.collected(id, false).await;
    fx.cleaned(id).await;
    assert_eq!(fx.state(id).await, "succeeded");
    assert_eq!(fx.messages(id).await, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn an_owner_job_without_a_free_runner_waits_and_is_placed_later(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &["kernel-6.12"], 1, false).await;
    let id = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"placement": ["runner"]})),
    )
    .await
    .unwrap();
    assert_eq!(fx.state(id).await, "waiting_runner");
    fx.connect(runner);
    assert_eq!(fx.place().await, 1);
    assert_eq!(fx.state(id).await, "staging");
}

#[sqlx::test(migrations = "./migrations")]
async fn owner_jobs_are_refused_with_a_code(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &["kernel-6.12"], 1, true).await;
    let plain = machines::add(&fx.pool, fx.owner, "plain").await.unwrap();
    // Только несущая подаёт.
    let refused = owner::submit(
        &fx.pool,
        &fx.registry,
        plain.machine_id,
        &owner_spec(json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, "not_carrier");
    let refused = owner::submit(
        &fx.pool,
        &fx.registry,
        Uuid::now_v7(),
        &owner_spec(json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, "not_carrier");
    for (spec, expected) in [
        ("not json".to_owned(), "invalid_arguments"),
        (owner_spec(json!({"extra": 1})), "invalid_arguments"),
        (owner_spec(json!({"shell": " "})), "invalid_arguments"),
        (
            owner_spec(json!({"source": {"repo": "/srv/src/app", "rev": "abc"}})),
            "invalid_arguments",
        ),
        (
            owner_spec(json!({"outputs": ["../x"]})),
            "invalid_arguments",
        ),
        (
            owner_spec(json!({"labels": ["gpu"]})),
            "no_runner_can_satisfy",
        ),
        (
            owner_spec(json!({"machine": "nowhere"})),
            "machine_unavailable",
        ),
    ] {
        let refused = owner::submit(&fx.pool, &fx.registry, fx.carrier, &spec)
            .await
            .unwrap_err();
        assert_eq!(refused.code, expected, "{spec}");
        assert!(!refused.message.is_empty());
    }
    let jobs: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0, "a refused job leaves no row");
}

#[sqlx::test(migrations = "./migrations")]
async fn the_owner_job_result_goes_to_the_submitting_terminal_and_survives_a_restart(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &["kernel-6.12"], 1, true).await;
    let id = owner::submit(&fx.pool, &fx.registry, fx.carrier, &owner_spec(json!({})))
        .await
        .unwrap();
    fx.staged(fx.carrier, id).await;
    fx.started(runner, id).await;
    fx.finished(runner, id, JobOutcome::Failed, Some(7)).await;
    assert_eq!(
        fx.commands(fx.carrier, "owner_job_finished").await,
        0,
        "not before the end"
    );
    fx.collected(id, false).await;
    assert_eq!(fx.commands(fx.carrier, "owner_job_finished").await, 1);
    fx.cleaned(id).await;
    // Повторный конец и повторные факты не плодят команд.
    fx.cleaned(id).await;
    assert_eq!(fx.commands(fx.carrier, "owner_job_finished").await, 1);
    // Команда лежит в базе: новый процесс хаба (пустой реестр) отдаёт её на новом соединении.
    let row = fx.command(fx.carrier, "owner_job_finished").await;
    assert_eq!(fx.delivery_state(row.id).await, "pending");
    let restarted = Registry::new(Default::default());
    let _session = testkit::fake_session(&restarted, fx.carrier, &CARRIER_CAPS, true);
    let Some(Kind::OwnerJobFinished(done)) =
        outbox::to_proto_for_session(&row, &restarted).unwrap().kind
    else {
        panic!("owner job result expected");
    };
    assert_eq!(done.job_id, id.to_string());
    assert_eq!(done.state, "failed");
    assert_eq!(done.exit_code, Some(7));
    assert_eq!(done.tail, b"hello\nworld\n");
    assert_eq!(done.outputs_dir, format!("/var/outputs/{id}"));
    assert_eq!(done.outputs_note, "");
    // Без `runners_v1` соединение команду не получает.
    let old = Registry::new(Default::default());
    let _old = testkit::fake_session(&old, fx.carrier, &CAPS, true);
    assert!(matches!(
        outbox::to_proto_for_session(&row, &old),
        Err(outbox::OutboxError::CapabilityMissing)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn an_owner_job_canceled_while_waiting_still_reports_to_the_terminal(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let id = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"labels": [], "placement": ["local"], "machine": "auto"})),
    )
    .await
    .unwrap();
    owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"labels": [], "placement": ["local"]})),
    )
    .await
    .unwrap();
    let waiting = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"labels": [], "placement": ["local"]})),
    )
    .await
    .unwrap();
    assert_eq!(fx.state(id).await, "staging");
    assert_eq!(fx.state(waiting).await, "waiting_runner");
    canterlot_hub::db::in_tx(&fx.pool, move |conn| {
        Box::pin(async move {
            inplace_jobs::cancel_in_tx(conn, waiting, "canceled_by_owner", Uuid::now_v7()).await?;
            Ok::<_, sqlx::Error>(())
        })
    })
    .await
    .unwrap();
    assert_eq!(fx.state(waiting).await, "canceled");
    assert_eq!(fx.commands(fx.carrier, "owner_job_finished").await, 1);
}

// ───────── Событие доступности раннера ─────────

async fn availability_events(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from events where kind='machine_availability_changed' order by scope_seq",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn availability_event_is_valid(payload: &Value) -> bool {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/machine_availability_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    let validator = jsonschema::validator_for(&schema).unwrap();
    validator.is_valid(&json!({
        "v": 1,
        "id": Uuid::now_v7(),
        "scope": "machines",
        "scope_seq": "3",
        "scope_seq_from": "3",
        "committed_at": "2026-11-21T12:00:00.000Z",
        "committed_at_from": "2026-11-21T12:00:00.000Z",
        "conversation_id": null,
        "execution_id": null,
        "correlation_id": Uuid::now_v7(),
        "kind": "machine_availability_changed",
        "payload": payload,
    }))
}

#[sqlx::test(migrations = "./migrations")]
async fn a_runner_reports_its_own_availability_vocabulary_and_a_worker_keeps_the_old_one(
    pool: PgPool,
) {
    let mut fx = Fx::new(pool).await;
    let online = fx.runner("runner-a", 20, &[], 1, true).await;
    let silent = fx.runner("runner-b", 10, &[], 1, false).await;
    sqlx::query("update machines set availability='unavailable', runner_availability=null")
        .execute(&fx.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        canterlot_hub::machine_info::project_availability(&fx.pool, &fx.registry, Uuid::now_v7())
            .await
            .unwrap();
    }
    let events = availability_events(&fx.pool).await;
    let of = |machine: Uuid| -> Vec<&Value> {
        events
            .iter()
            .filter(|event| event["machine_id"] == machine.to_string())
            .collect()
    };
    // Обычная машина: прежний словарь, без возраста наблюдения.
    assert_eq!(
        of(fx.carrier),
        [&json!({"machine_id": fx.carrier, "availability": "available"})]
    );
    // Раннеры: словарь раннера и ни одного события прежнего словаря.
    assert_eq!(
        of(online),
        [&json!({"machine_id": online, "availability": "online"})]
    );
    assert_eq!(
        of(silent),
        [&json!({"machine_id": silent, "availability": "unknown"})]
    );
    for event in &events {
        assert!(availability_event_is_valid(event), "{event}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn an_observation_changes_the_runners_availability_and_the_event_carries_its_age(
    pool: PgPool,
) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, false).await;
    let (pool, registry) = (fx.pool.clone(), fx.registry.clone());
    let project = move || {
        let (pool, registry) = (pool.clone(), registry.clone());
        async move {
            canterlot_hub::machine_info::project_availability(&pool, &registry, Uuid::now_v7())
                .await
                .unwrap();
        }
    };
    sqlx::query("update machines set availability='unavailable', runner_availability=null")
        .execute(&fx.pool)
        .await
        .unwrap();
    project().await;
    let count = |events: &Vec<Value>| {
        events
            .iter()
            .filter(|event| event["machine_id"] == runner.to_string())
            .count()
    };
    assert_eq!(count(&availability_events(&fx.pool).await), 1);
    // Три неудачи подряд при подключённой несущей: раннер `offline`, событие с возрастом.
    let observed = Utc::now() - chrono::TimeDelta::seconds(20);
    machines::record_runner_observation(&fx.pool, fx.carrier, runner, false, 3, observed)
        .await
        .unwrap();
    project().await;
    let events = availability_events(&fx.pool).await;
    let last = events.last().unwrap();
    assert_eq!(last["machine_id"], runner.to_string());
    assert_eq!(last["availability"], "offline", "{last}");
    let at = last["observed_at"].as_str().expect("observed_at");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .timestamp_millis(),
        observed.timestamp_millis()
    );
    assert!(availability_event_is_valid(last), "{last}");
    // Время хаба, с которого раннер `offline`, записано; повторный разбор его не сдвигает.
    let since = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>(
            "select runner_offline_since from machines where id=$1",
        )
        .bind(runner)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let offline_since = since(fx.pool.clone()).await.expect("offline_since is set");
    assert!(Utc::now() - offline_since < chrono::TimeDelta::seconds(30));
    // Повторный разбор без перемен ничего не добавляет.
    project().await;
    assert_eq!(availability_events(&fx.pool).await.len(), events.len());
    assert_eq!(since(fx.pool.clone()).await, Some(offline_since));
    // Воркер раннера подключился: `online`, возраст наблюдения остаётся в событии.
    fx.connect(runner);
    project().await;
    let events = availability_events(&fx.pool).await;
    assert_eq!(events.last().unwrap()["availability"], "online");
    assert_eq!(since(fx.pool.clone()).await, None, "cleared when online");
    // Наблюдение устарело (старше трёх минут): отключённый раннер снова `unknown`.
    fx.sessions.pop();
    sqlx::query("update runner_observations set observed_at = now() - interval '10 minutes'")
        .execute(&fx.pool)
        .await
        .unwrap();
    project().await;
    let events = availability_events(&fx.pool).await;
    assert_eq!(events.last().unwrap()["availability"], "unknown");
    assert_eq!(since(fx.pool.clone()).await, None, "cleared when unknown");
}

// ───────── Вид задачи и `runners_list` ─────────

#[sqlx::test(migrations = "./migrations")]
async fn the_job_view_shows_labels_source_and_outputs_without_the_local_path(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &["kernel-6.12"], 1, true).await;
    let id = job_id(
        &fx.auto(json!({
            "labels": ["kernel-6.12"], "expected_minutes": 45, "outputs": ["result"],
        }))
        .await
        .unwrap(),
    );
    let mut conn = fx.pool.acquire().await.unwrap();
    let view = inplace_jobs::view(&mut conn, &fx.job(id).await)
        .await
        .unwrap();
    assert_eq!(view.state, "staging");
    assert_eq!(view.machine_id, Some(runner));
    assert_eq!(view.labels, vec!["kernel-6.12".to_owned()]);
    assert_eq!(view.expected_minutes, 45);
    let source = view.source.expect("source view");
    assert_eq!(source.repo, "app");
    assert_eq!(source.rev, &REV[..12]);
    let text = serde_json::to_string(
        &inplace_jobs::view(&mut conn, &fx.job(id).await)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        !text.contains("/srv/src"),
        "the local path stays on the hub: {text}"
    );
    drop(conn);
    // Ожидающая задача: машины в виде нет.
    let waiting = job_id(
        &fx.auto(json!({"labels": ["kernel-6.12"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    let mut conn = fx.pool.acquire().await.unwrap();
    let view = inplace_jobs::view(&mut conn, &fx.job(waiting).await)
        .await
        .unwrap();
    assert_eq!(view.state, "waiting_runner");
    assert!(view.machine_id.is_none() && view.machine_name.is_none() && view.workdir.is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn runners_list_describes_the_carrier_and_its_runners(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 20, &["build-host"], 2, true).await;
    fx.runner("runner-b", 10, &["kernel-6.12"], 1, false).await;
    fx.auto(json!({"labels": ["build-host"], "placement": ["runner"]}))
        .await
        .unwrap();
    fx.auto(json!({"labels": ["kernel-6.12"], "placement": ["runner"]}))
        .await
        .unwrap();
    let reply = fx.call("runners.list", json!({})).await.unwrap();
    assert_eq!(reply["count"], 3, "{reply}");
    assert_eq!(reply["waiting_runner_jobs"], 1);
    let runners = reply["runners"].as_array().unwrap();
    let local = &runners[0];
    assert_eq!(local["name"], "carrier");
    assert_eq!(local["role"], "local");
    assert_eq!(local["availability"], "online");
    assert_eq!(local["rank"], 0);
    assert_eq!(local["slots_max"], 2);
    assert_eq!(local["slots_used"], 0);
    let a = runners.iter().find(|r| r["name"] == "runner-a").unwrap();
    assert_eq!(a["role"], "runner");
    assert_eq!(a["availability"], "online");
    assert_eq!(a["rank"], 20);
    assert_eq!(a["labels"], json!(["build-host"]));
    assert_eq!(a["slots_used"], 1);
    assert_eq!(a["slots_max"], 2);
    assert_eq!(a["paused"], false);
    assert!(a["observation_age_seconds"].is_null());
    let b = runners.iter().find(|r| r["name"] == "runner-b").unwrap();
    assert_eq!(b["availability"], "unknown");
    assert_eq!(b["slots_used"], 0);
    assert_eq!(
        (b["queued"].as_i64(), b["running"].as_i64()),
        (Some(0), Some(0))
    );
    // Аргументов нет.
    assert_eq!(
        code(fx.call("runners.list", json!({"machine": "x"})).await),
        Code::InvalidArguments
    );
}

// ───────── Исправления по ревью: метки названной цели, ожидание без конца ─────────

fn named(machine: &str, extra: Value) -> Value {
    let mut args = json!({
        "machine": machine, "shell": "true", "wait_seconds": 0,
        "source": {"repo": "/srv/src/app", "rev": REV},
    });
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}

#[sqlx::test(migrations = "./migrations")]
async fn a_named_target_must_carry_the_labels_of_the_job(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 5, &[], 2, true).await;
    fx.runner("runner-b", 5, &["kernel-6.12"], 2, true).await;
    // Раннер без метки отказывается, называй его хоть по имени, хоть по идентификатору.
    assert_eq!(
        code(
            fx.call(
                "job.run",
                named("runner-a", json!({"labels": ["kernel-6.12"]}))
            )
            .await
        ),
        Code::NoRunnerCanSatisfy
    );
    // Сама несущая тоже машина со своими метками.
    assert_eq!(
        code(
            fx.call(
                "job.run",
                named("carrier", json!({"labels": ["kernel-6.12"]}))
            )
            .await
        ),
        Code::NoRunnerCanSatisfy
    );
    let ok = fx
        .call(
            "job.run",
            named("runner-b", json!({"labels": ["kernel-6.12"]})),
        )
        .await
        .unwrap();
    assert_eq!(ok["machine"], "runner-b", "{ok}");
    // Задача владельца: то же правило и тот же код.
    let refused = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"machine": "runner-a"})),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code, "no_runner_can_satisfy");
    let id = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"machine": "runner-b"})),
    )
    .await
    .unwrap();
    assert_eq!(fx.job(id).await.state, "staging");
    // Отказ не оставляет строки: заняты только две принятые задачи.
    let jobs: i64 = sqlx::query_scalar("select count(*) from jobs")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_named_job_onto_a_full_runner_waits_for_that_runner(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 5, &[], 1, true).await;
    // Другой раннер свободен и ранг у него выше: названную задачу он не получит.
    let other = fx.runner("runner-b", 50, &[], 1, true).await;
    let first = job_id(
        &fx.call("job.run", named("runner-a", json!({})))
            .await
            .unwrap(),
    );
    assert_eq!(fx.state(first).await, "staging");
    let reply = fx
        .call("job.run", named("runner-a", json!({})))
        .await
        .unwrap();
    let second = job_id(&reply);
    assert_eq!(reply["state"], "waiting_runner", "{reply}");
    assert_eq!(reply["machine"], Value::Null);
    let job = fx.job(second).await;
    assert!(job.machine_id.is_none());
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 0);
    assert_eq!(
        fx.place().await,
        0,
        "runner-a is full, runner-b is not its stand-in"
    );
    assert_eq!(fx.job(second).await.state, "waiting_runner");
    assert_eq!(fx.commands(fx.carrier, "stage_source").await, 1);
    // Место освободилось: ждавшая задача идёт именно на названный раннер.
    fx.call("job.cancel", json!({"job_id": first}))
        .await
        .unwrap();
    assert_eq!(fx.place().await, 1);
    let job = fx.job(second).await;
    assert_eq!(job.state, "staging");
    assert_eq!(job.machine_id, Some(runner));
    assert_ne!(job.machine_id, Some(other));
    // То же для задачи владельца.
    let owned = owner::submit(
        &fx.pool,
        &fx.registry,
        fx.carrier,
        &owner_spec(json!({"machine": "runner-a", "labels": []})),
    )
    .await
    .unwrap();
    assert_eq!(fx.state(owned).await, "waiting_runner");
    // Отзыв названного раннера закрывает и ждущих его: ждать больше нечего.
    machines::revoke(&fx.pool, runner).await.unwrap();
    assert_eq!(fx.state(owned).await, "lost");
    assert_eq!(fx.state(second).await, "lost");
    assert_eq!(fx.messages(second).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn revoking_or_unmarking_the_carrier_ends_its_waiting_jobs_lost_once(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Несущая полна двумя задачами, третья и четвёртая ждут.
    let local = json!({"placement": ["local"]});
    fx.auto(local.clone()).await.unwrap();
    fx.auto(local.clone()).await.unwrap();
    let w1 = job_id(&fx.auto(local.clone()).await.unwrap());
    let w2 = job_id(&fx.auto(local).await.unwrap());
    assert_eq!(fx.state(w1).await, "waiting_runner");
    machines::set_carrier(&fx.pool, fx.carrier, false)
        .await
        .unwrap();
    for id in [w1, w2] {
        let job = fx.job(id).await;
        assert_eq!(job.state, "lost");
        assert_eq!(job.end_reason.as_deref(), Some("lost"));
        assert_eq!(fx.messages(id).await, 1);
    }
    // Повторный вызов и отзыв ничего не множат.
    machines::set_carrier(&fx.pool, fx.carrier, false)
        .await
        .unwrap();
    machines::revoke(&fx.pool, fx.carrier).await.unwrap();
    for id in [w1, w2] {
        assert_eq!(fx.messages(id).await, 1);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn revoking_the_carrier_ends_its_waiting_jobs_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let local = json!({"placement": ["local"]});
    fx.auto(local.clone()).await.unwrap();
    fx.auto(local.clone()).await.unwrap();
    let waiting = job_id(&fx.auto(local).await.unwrap());
    assert_eq!(fx.state(waiting).await, "waiting_runner");
    machines::revoke(&fx.pool, fx.carrier).await.unwrap();
    assert_eq!(fx.state(waiting).await, "lost");
    assert_eq!(fx.messages(waiting).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn an_owner_job_is_checked_for_outputs_before_it_runs(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 5, &["kernel-6.12"], 1, true).await;
    for bad in [
        json!({"outputs": ["test results"]}),
        json!({"outputs": ["a/../b"]}),
        json!({"outputs": ["/etc"]}),
    ] {
        let refused = owner::submit(&fx.pool, &fx.registry, fx.carrier, &owner_spec(bad.clone()))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "invalid_arguments", "{bad}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn a_repository_name_the_carrier_would_refuse_gets_a_safe_key(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let repo = "/srv/src/my app+v2 ё";
    let id = job_id(
        &fx.auto(json!({"source": {"repo": repo, "rev": REV}, "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    assert_eq!(fx.job(id).await.machine_id, Some(runner));
    let key = place::repo_key(repo);
    assert!(
        key.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
        "{key}"
    );
    assert!(key.starts_with("my-app-v2---"), "{key}");
    let stage = fx.command(fx.carrier, "stage_source").await;
    let Some(Kind::StageSource(stage)) = outbox::to_proto(&stage).unwrap().kind else {
        panic!("stage command expected");
    };
    assert_eq!(stage.repo_key, key);
    // Разные пути с одинаково сведённым именем ключей не делят.
    assert_ne!(place::repo_key("/a/x y"), place::repo_key("/b/x y"));
    // Имя из точек не бывает именем ключа.
    for odd in ["/srv/src/.", "/srv/src/..."] {
        assert!(place::repo_key(odd).starts_with("repo-"), "{odd}");
    }
}

/// Рабочий каталог на несущей лежит над репозиторием. Репозиторий, который сам рабочий корень,
/// дал бы каталог «личного» домена: на несущей такой задаче места нет.
#[sqlx::test(migrations = "./migrations")]
async fn the_workdir_on_the_carrier_never_leaves_the_domain_of_the_source(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 2, true).await;
    sqlx::query("update conversations set memory_domain='work' where id=$1")
        .bind(fx.conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    let root = json!({"repo": "/home/dev/work", "rev": REV});
    // Названная несущая: отказ с кодом домена.
    assert_eq!(
        code(
            fx.call("job.run", named("carrier", json!({"source": root.clone()})))
                .await
        ),
        Code::DomainMismatch
    );
    // Только на несущую: места нет вовсе.
    assert_eq!(
        code(
            fx.auto(json!({"source": root.clone(), "placement": ["local"]}))
                .await
        ),
        Code::NoRunnerCanSatisfy
    );
    // Раннер берёт: его каталог `jobs/work` не зависит от пути на несущей.
    let id = job_id(&fx.auto(json!({"source": root.clone()})).await.unwrap());
    assert_eq!(fx.job(id).await.machine_id, Some(runner));
    // Раннер занят полностью: задача ждёт раннер, а не уходит на несущую.
    let second = job_id(&fx.auto(json!({"source": root})).await.unwrap());
    let third = job_id(
        &fx.auto(json!({"source": {"repo": "/home/dev/work", "rev": REV}}))
            .await
            .unwrap(),
    );
    assert_eq!(fx.job(second).await.machine_id, Some(runner));
    assert_eq!(fx.state(third).await, "waiting_runner");
    // Обычный репозиторий внутри рабочего корня остаётся в домене и идёт на несущую.
    let inner = job_id(
        &fx.auto(json!({
            "source": {"repo": "/home/dev/work/app", "rev": REV},
            "placement": ["local"],
        }))
        .await
        .unwrap(),
    );
    let job = fx.job(inner).await;
    assert_eq!(job.machine_id, Some(fx.carrier));
    assert_eq!(
        job.workdir.as_deref(),
        Some(format!("/home/dev/work/.canterlot-jobs/app/{inner}").as_str())
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn a_job_waiting_for_a_runner_past_the_timeout_is_canceled_no_runner(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    fx.runner("runner-a", 5, &["kernel-6.12"], 1, false).await;
    let id = job_id(
        &fx.auto(json!({"labels": ["kernel-6.12"], "placement": ["runner"]}))
            .await
            .unwrap(),
    );
    assert_eq!(fx.state(id).await, "waiting_runner");
    let day = Duration::from_secs(24 * 3600);
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, day, Utc::now() + chrono::TimeDelta::hours(23))
            .await
            .unwrap(),
        0
    );
    assert_eq!(fx.state(id).await, "waiting_runner");
    // Ноль выключает срок.
    assert_eq!(
        sweep::waiting_timeout_sweep(
            &fx.pool,
            Duration::ZERO,
            Utc::now() + chrono::TimeDelta::days(30)
        )
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, day, Utc::now() + chrono::TimeDelta::hours(25))
            .await
            .unwrap(),
        1
    );
    let job = fx.job(id).await;
    assert_eq!(job.state, "canceled");
    assert_eq!(job.end_reason.as_deref(), Some("no_runner"));
    assert_eq!(fx.messages(id).await, 1);
    // Повтор ничего не меняет.
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, day, Utc::now() + chrono::TimeDelta::hours(26))
            .await
            .unwrap(),
        0
    );
    assert_eq!(fx.messages(id).await, 1);
}

// ───────── Один повтор после прерывания машины ─────────

use canterlot_hub::inplace_jobs::retry;
use canterlot_hub::machines::RevokeJobs;
use canterlot_proto::worker::v1::{JobPhase, JobSnapshot, JobsReport};

impl Fx {
    /// Отзыв прерванной машины: задачам, которым положен повтор, он остаётся.
    async fn preempt(&self, machine: Uuid) {
        self.apply(move |conn| {
            Box::pin(async move {
                machines::revoke_jobs_in_tx(conn, machine, RevokeJobs::RetryOnce)
                    .await
                    .map(|_| ())
            })
        })
        .await;
    }

    /// Прежняя машина подтверждена остановленной: ждущие повтора задачи выпускаются.
    async fn release(&self, machine: Uuid) -> Vec<Uuid> {
        canterlot_hub::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move { retry::release_retry_pending_in_tx(conn, machine).await })
        })
        .await
        .unwrap()
    }

    /// Машина с тем же идентификатором снова в строю: новый токен вместо отозванного.
    async fn reissue(&self, machine: Uuid) {
        machines::rotate_token(&self.pool, machine, false, true)
            .await
            .unwrap()
            .unwrap();
    }

    async fn staged_attempt(&self, id: Uuid, attempt: u32) {
        let (from, fact) = (
            self.carrier,
            SourceStaged {
                job_id: id.to_string(),
                attempt,
            },
        );
        self.apply(move |conn| {
            Box::pin(async move {
                carrier::apply_source_staged(conn, from, &fact, Uuid::now_v7()).await
            })
        })
        .await;
    }

    async fn finished_attempt(&self, machine: Uuid, id: Uuid, attempt: u32) {
        let fact = JobFinished {
            job_id: id.to_string(),
            attempt,
            outcome: JobOutcome::Succeeded as i32,
            exit_code: Some(0),
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

    async fn open_commands(&self, id: Uuid) -> i64 {
        sqlx::query_scalar(
            "select count(*) from commands
              where payload->>'job_id'=$1 and delivery_state<>'completed'",
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Задача на раннере (только `runner`) доведена до `running`.
    async fn running_on(&self, runner: Uuid) -> Uuid {
        let id = job_id(&self.auto(json!({"placement": ["runner"]})).await.unwrap());
        self.run_to_running(runner, id).await;
        id
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn a_preempted_machine_holds_its_staged_source_jobs_for_one_retry(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 3, true).await;
    let running = fx.running_on(runner).await;
    let staging = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    assert_eq!(fx.state(staging).await, "staging");
    fx.preempt(runner).await;
    for id in [running, staging] {
        let job = fx.job(id).await;
        assert_eq!(job.state, "retry_pending", "{id}");
        // Задача не закончена: ни итога, ни сообщения, ни очистки; попытка прежняя.
        assert_eq!((job.attempt, job.preemption_retries), (1, 0));
        assert!(job.finished_at.is_none() && job.end_reason.is_none());
        assert_eq!(job.machine_id, Some(runner));
        assert_eq!(fx.messages(id).await, 0);
        // Команды прежней попытки не дойдут до машины с тем же идентификатором.
        assert_eq!(fx.open_commands(id).await, 0);
    }
    assert_eq!(fx.commands(fx.carrier, "cleanup_source").await, 0);
    // Ждущая повтора не размещается и не теряется по сроку.
    assert_eq!(fx.place().await, 0);
    let later = Utc::now() + chrono::TimeDelta::days(3);
    assert_eq!(
        sweep::lost_sweep(&fx.pool, &fx.registry, Duration::ZERO, later)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        carrier::stage_timeout_sweep(&fx.pool, Duration::from_secs(60), later)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, Duration::from_secs(60), later)
            .await
            .unwrap(),
        0
    );
    assert_eq!(fx.state(running).await, "retry_pending");
    // Событие о задаче несёт новое состояние, и схема событий его знает.
    let state: String = sqlx::query_scalar(
        "select payload->'job'->>'state' from events
          where kind='job_changed' and payload->'job'->>'id'=$1 order by scope_seq desc limit 1",
    )
    .bind(running.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(state, "retry_pending");
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/JobView/properties/state");
    schema.as_object_mut().unwrap().remove("oneOf");
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&json!(state)));
    assert!(!validator.is_valid(&json!("no_such_state")));
    // Обычный повторный отзыв (уборка окна) повтора не отнимает.
    machines::revoke(&fx.pool, runner).await.unwrap();
    assert_eq!(fx.state(running).await, "retry_pending");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_preempted_job_with_a_cancel_requested_is_lost(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 2, true).await;
    let id = fx.running_on(runner).await;
    fx.call("job.cancel", json!({"job_id": id})).await.unwrap();
    assert_eq!(fx.state(id).await, "running");
    fx.preempt(runner).await;
    assert_eq!(fx.state(id).await, "lost");
    assert_eq!(fx.messages(id).await, 1);
    assert!(fx.release(runner).await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn a_preempted_job_with_a_finish_fact_is_lost(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 2, true).await;
    let id = fx.running_on(runner).await;
    // Снимок «закончена» с выводом: задача ещё не закрыта, но команда уже отработала.
    fx.apply(move |conn| {
        Box::pin(async move {
            let report = JobsReport {
                jobs: vec![JobSnapshot {
                    job_id: id.to_string(),
                    attempt: 1,
                    phase: JobPhase::Finished as i32,
                    outcome: JobOutcome::Succeeded as i32,
                    exit_code: Some(0),
                    duration_ms: 10,
                    log_bytes: 3,
                    ..Default::default()
                }],
                observed_at_ms: 0,
            };
            facts::apply_report(conn, runner, &report, Uuid::now_v7()).await
        })
    })
    .await;
    assert_eq!(fx.state(id).await, "running");
    fx.preempt(runner).await;
    assert_eq!(fx.state(id).await, "lost");
    // Итог раннера принят и идёт сбор: такую задачу отзыв не трогает вовсе.
    fx.reissue(runner).await;
    let collecting = job_id(
        &fx.auto(json!({"placement": ["runner"], "outputs": ["out"]}))
            .await
            .unwrap(),
    );
    fx.run_to_running(runner, collecting).await;
    fx.finished(runner, collecting, JobOutcome::Succeeded, Some(0))
        .await;
    assert_eq!(fx.state(collecting).await, "collecting");
    fx.preempt(runner).await;
    assert_eq!(fx.state(collecting).await, "collecting");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_job_without_a_staged_source_is_lost_with_its_preempted_machine(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 2, true).await;
    let id = fx.running_on(runner).await;
    // Команда в каталоге машины без checkout коммита: заново её подготовить нечем.
    sqlx::query(
        "update jobs set source_repo=null, source_rev=null, source_domain=null where id=$1",
    )
    .bind(id)
    .execute(&fx.pool)
    .await
    .unwrap();
    fx.preempt(runner).await;
    assert_eq!(fx.state(id).await, "lost");
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn release_returns_the_job_to_waiting_as_the_next_attempt(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 2, true).await;
    let id = fx.running_on(runner).await;
    let chunk = canterlot_proto::worker::v1::JobLogChunk {
        job_id: id.to_string(),
        attempt: 1,
        offset: 0,
        data: b"first attempt output".to_vec(),
    };
    fx.apply(move |conn| Box::pin(async move { facts::apply_chunk(conn, runner, &chunk).await }))
        .await;
    assert!(!fx.job(id).await.live_tail.is_empty());
    fx.preempt(runner).await;
    let before = Utc::now();
    assert_eq!(fx.release(runner).await, vec![id]);
    let job = fx.job(id).await;
    assert_eq!(job.state, "waiting_runner");
    assert_eq!((job.attempt, job.preemption_retries), (2, 1));
    assert!(
        job.placement_waiting_since
            .is_some_and(|since| since >= before)
    );
    // Место, факты и вывод прежней попытки сброшены.
    assert!(job.machine_id.is_none() && job.workdir.is_none());
    assert!(job.pid.is_none() && job.started_at.is_none() && job.dispatched_at.is_none());
    assert!(job.live_tail.is_empty() && job.live_offset == 0 && job.output_bytes == 0);
    assert!(job.finished_at.is_none() && fx.messages(id).await == 0);
    assert_eq!(fx.open_commands(id).await, 0);
    // Повторный выпуск ничего не делает.
    assert!(fx.release(runner).await.is_empty());
    assert_eq!(fx.job(id).await.attempt, 2);
    // Вторая потеря той же задачи: повтор уже был, она `lost`.
    fx.reissue(runner).await;
    assert_eq!(fx.place().await, 1);
    let job = fx.job(id).await;
    assert_eq!(job.state, "staging");
    assert_eq!(
        job.workdir.as_deref(),
        Some(&*format!("/srv/runner-a/jobs/personal/{id}-2"))
    );
    fx.preempt(runner).await;
    let job = fx.job(id).await;
    assert_eq!((job.state.as_str(), job.attempt), ("lost", 2));
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn late_facts_of_the_first_attempt_change_nothing_for_the_second(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = job_id(&fx.auto(json!({"placement": ["runner"]})).await.unwrap());
    // Машину прервали, пока несущая ещё готовила исходники первой попытки.
    assert_eq!(fx.state(id).await, "staging");
    fx.preempt(runner).await;
    // Пока задача ждёт повтора, факты прежней попытки её не двигают.
    fx.staged_attempt(id, 1).await;
    fx.finished_attempt(runner, id, 1).await;
    fx.stage_failed(id, "ssh: connection lost").await;
    assert_eq!(fx.state(id).await, "retry_pending");
    assert_eq!(fx.release(runner).await, vec![id]);
    // Вторая попытка размещена на машине с тем же идентификатором и готовится.
    fx.reissue(runner).await;
    assert_eq!(fx.place().await, 1);
    let job = fx.job(id).await;
    assert_eq!((job.state.as_str(), job.attempt), ("staging", 2));
    assert_eq!(job.machine_id, Some(runner));
    let stage = fx.command(fx.carrier, "stage_source").await;
    let Some(Kind::StageSource(stage)) = outbox::to_proto(&stage).unwrap().kind else {
        panic!("stage_source expected");
    };
    assert_eq!(stage.attempt, 2);
    assert!(
        stage.workdir.ends_with(&format!("/{id}-2")),
        "{}",
        stage.workdir
    );
    // Поздний успех подготовки первой попытки (и от несущей до повторов, с попыткой 0) не
    // отпускает вторую раньше, чем готова её рабочая копия.
    fx.staged_attempt(id, 1).await;
    fx.staged_attempt(id, 0).await;
    assert_eq!(fx.state(id).await, "staging");
    assert_eq!(fx.commands(runner, "start_inplace_job").await, 0);
    // Поздний сбой подготовки и поздний итог первой попытки её не заканчивают.
    fx.stage_failed(id, "ssh: connection lost").await;
    fx.finished_attempt(runner, id, 1).await;
    let job = fx.job(id).await;
    assert_eq!(job.state, "staging");
    assert!(job.exit_code.is_none() && job.finished_at.is_none());
    assert_eq!(fx.messages(id).await, 0);
    // Очистка, подтверждённая для первой попытки, не считается очисткой второй.
    let (from, fact) = (
        fx.carrier,
        SourceCleaned {
            job_id: id.to_string(),
            attempt: 1,
        },
    );
    fx.apply(move |conn| {
        Box::pin(
            async move { carrier::apply_source_cleaned(conn, from, &fact, Uuid::now_v7()).await },
        )
    })
    .await;
    assert!(fx.job(id).await.cleaned_at.is_none());
    // Свои факты второй попытки ведут её до конца.
    fx.staged_attempt(id, 2).await;
    assert_eq!(fx.state(id).await, "dispatched");
    let start = fx.command(runner, "start_inplace_job").await;
    assert_eq!(start.payload["attempt"], 2);
    fx.finished_attempt(runner, id, 1).await;
    assert_eq!(fx.state(id).await, "dispatched");
    fx.finished_attempt(runner, id, 2).await;
    let job = fx.job(id).await;
    assert_eq!((job.state.as_str(), job.exit_code), ("succeeded", Some(0)));
    assert_eq!(fx.messages(id).await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn a_released_job_waits_its_timeout_from_the_release(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 1, true).await;
    let id = fx.running_on(runner).await;
    // Подана двое суток назад и всё это время шла.
    sqlx::query("update jobs set created_at = created_at - interval '2 days' where id=$1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.preempt(runner).await;
    assert_eq!(fx.release(runner).await, vec![id]);
    let day = Duration::from_secs(24 * 3600);
    // Ближайший проход её не закрывает: срок идёт от возврата в очередь.
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, day, Utc::now() + chrono::TimeDelta::minutes(1))
            .await
            .unwrap(),
        0
    );
    assert_eq!(fx.state(id).await, "waiting_runner");
    assert_eq!(
        sweep::waiting_timeout_sweep(&fx.pool, day, Utc::now() + chrono::TimeDelta::hours(25))
            .await
            .unwrap(),
        1
    );
    let job = fx.job(id).await;
    assert_eq!(
        (job.state.as_str(), job.end_reason.as_deref()),
        ("canceled", Some("no_runner"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn pending_jobs_end_lost_when_there_is_no_retry_and_canceled_on_cancel(pool: PgPool) {
    let mut fx = Fx::new(pool).await;
    let runner = fx.runner("runner-a", 20, &[], 3, true).await;
    let lost = fx.running_on(runner).await;
    let canceled = fx.running_on(runner).await;
    fx.preempt(runner).await;
    // Отмена закрывает ждущую повтора сразу: повтора у неё не будет.
    let reply = fx
        .call("job.cancel", json!({"job_id": canceled}))
        .await
        .unwrap();
    assert_eq!(reply["state"], "canceled", "{reply}");
    let ended = canterlot_hub::db::in_tx(&fx.pool, move |conn| {
        Box::pin(async move { retry::lose_retry_pending_in_tx(conn, runner).await })
    })
    .await
    .unwrap();
    assert_eq!(ended, vec![lost]);
    let job = fx.job(lost).await;
    assert_eq!(
        (job.state.as_str(), job.end_reason.as_deref(), job.attempt),
        ("lost", Some("lost"), 1)
    );
    assert_eq!(fx.messages(lost).await, 1);
    assert_eq!(fx.state(canceled).await, "canceled");
    assert!(fx.release(runner).await.is_empty());
    // Несущая отозвана, пока задача ждала повтора: готовить исходники некому, задача `lost`.
    fx.reissue(runner).await;
    let orphan = fx.running_on(runner).await;
    fx.preempt(runner).await;
    assert_eq!(fx.state(orphan).await, "retry_pending");
    machines::revoke(&fx.pool, fx.carrier).await.unwrap();
    assert_eq!(fx.state(orphan).await, "lost");
}
