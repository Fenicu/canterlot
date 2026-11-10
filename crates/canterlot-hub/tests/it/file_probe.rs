//! Условие `file` и файл отчёта автопинка: проба пути идёт по управляющему каналу,
//! её ответ решает будильник и прогресс толчка. Воркер здесь поддельный: проверяется
//! хаб — корреляция, срок, отложение задания и тексты.

use crate::support::task3 as support;

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{SchedulerConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions,
    model::ExecutionState,
    scheduler::Scheduler,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig, subs},
    testkit,
};
use canterlot_proto::worker::v1::{
    ControlUp, PathProbe, PathProbeExists, PathProbeMissing, PathProbeNotAllowed, control_down,
    control_up, path_probe,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// `ToolError` не реализует `std::error::Error`: в тестах он становится обычной ошибкой.
fn tool<T>(
    result: Result<T, session_tools::ToolError>,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    result.map_err(|error| Box::new(std::io::Error::other(format!("{error:?}"))) as Box<_>)
}

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    }
}

/// Поддельный воркер: отвечает на `ProbePath` по сценарию, умеет молчать.
struct Scripted {
    outcome: path_probe::Outcome,
    /// Часы воркера; `None` — текущее время теста.
    now_ms: Option<u64>,
}

struct FakeWorker {
    script: Arc<Mutex<VecDeque<Scripted>>>,
    seen: Arc<Mutex<Vec<String>>>,
    answered: Arc<AtomicUsize>,
    paused: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl FakeWorker {
    async fn connect(
        hub: &testkit::TestHub,
        seed: &testkit::Seed,
        capabilities: &[&str],
    ) -> FakeWorker {
        let client = connect_with(hub, seed, Uuid::now_v7(), capabilities, 1000).await;
        let tx = client.tx.clone();
        let mut stream = client.stream;
        let script: Arc<Mutex<VecDeque<Scripted>>> = Arc::default();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let answered = Arc::new(AtomicUsize::new(0));
        let paused = Arc::new(AtomicBool::new(false));
        let task = {
            let script = script.clone();
            let seen = seen.clone();
            let answered = answered.clone();
            let paused = paused.clone();
            tokio::spawn(async move {
                while let Ok(Some(message)) = stream.message().await {
                    let Some(control_down::Msg::ProbePath(request)) = message.msg else {
                        continue;
                    };
                    seen.lock().unwrap().push(request.path.clone());
                    if paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    let scripted = script.lock().unwrap().pop_front().unwrap_or(Scripted {
                        outcome: path_probe::Outcome::Missing(PathProbeMissing {}),
                        now_ms: None,
                    });
                    let now_ms = scripted.now_ms.unwrap_or_else(|| {
                        u64::try_from(Utc::now().timestamp_millis()).unwrap_or(0)
                    });
                    answered.fetch_add(1, Ordering::SeqCst);
                    let _ = tx
                        .send(ControlUp {
                            msg: Some(control_up::Msg::PathProbe(PathProbe {
                                request_id: request.request_id,
                                outcome: Some(scripted.outcome),
                                now_ms,
                            })),
                        })
                        .await;
                }
            })
        };
        FakeWorker {
            script,
            seen,
            answered,
            paused,
            task,
        }
    }

    fn push(&self, outcome: path_probe::Outcome) {
        self.script.lock().unwrap().push_back(Scripted {
            outcome,
            now_ms: None,
        });
    }

    fn push_at(&self, outcome: path_probe::Outcome, now_ms: u64) {
        self.script.lock().unwrap().push_back(Scripted {
            outcome,
            now_ms: Some(now_ms),
        });
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    async fn wait(&self, count: usize) {
        tokio::time::timeout(WAIT, async {
            while self.answered.load(Ordering::SeqCst) < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("probe answered");
    }

    /// Оборвать сеанс: машина уходит в офлайн, проба получает `Unavailable`.
    fn disconnect(self) {
        self.task.abort();
    }
}

impl Drop for FakeWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn exists(size: u64, mtime: DateTime<Utc>, head: Option<&str>) -> path_probe::Outcome {
    path_probe::Outcome::Exists(PathProbeExists {
        size,
        mtime_ms: u64::try_from(mtime.timestamp_millis()).unwrap_or(0),
        head: head.unwrap_or_default().into(),
        head_omitted: String::new(),
    })
}

fn missing() -> path_probe::Outcome {
    path_probe::Outcome::Missing(PathProbeMissing {})
}

fn exists_raw(size: u64, mtime_ms: u64, head: Option<&str>) -> path_probe::Outcome {
    path_probe::Outcome::Exists(PathProbeExists {
        size,
        mtime_ms,
        head: head.unwrap_or_default().into(),
        head_omitted: String::new(),
    })
}

fn denied(reason: &str) -> path_probe::Outcome {
    path_probe::Outcome::NotAllowed(PathProbeNotAllowed {
        reason: reason.into(),
    })
}

struct Fx {
    pool: PgPool,
    hub: testkit::TestHub,
    seed: testkit::Seed,
    child: Uuid,
    execution: executions::StartedExecution,
}

impl Fx {
    async fn new(pool: PgPool) -> Fx {
        let seed = wire_seed(&pool).await;
        sqlx::query("update machines set availability='available' where id=$1")
            .bind(seed.machine_id)
            .execute(&pool)
            .await
            .unwrap();
        let child = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        sqlx::query(
            "update conversations set parent_conversation_id=$2,spawn_depth=1,title='worker' where id=$1",
        )
        .bind(child)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
        let execution = executions::start(&pool, seed.owner_id, child, Uuid::now_v7())
            .await
            .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        let hub = testkit::spawn_test_hub(pool.clone(), Default::default()).await;
        Fx {
            pool,
            hub,
            seed,
            child,
            execution,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.seed.owner_id,
            conversation_id: self.seed.conversation_id,
            execution_id: Uuid::now_v7(),
            machine_id: self.seed.machine_id,
            call_id: Uuid::now_v7(),
        }
    }

    fn context(&self) -> ToolContext {
        ToolContext {
            pool: self.pool.clone(),
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
            registry: self.hub.registry.clone(),
            shutdown: CancellationToken::new(),
        }
    }

    fn scheduler(&self) -> Scheduler {
        Scheduler::new(
            self.pool.clone(),
            Dispatcher::channel().0,
            SchedulerConfig::default(),
        )
        .with_limits(self.hub.registry.clone(), Default::default())
    }

    async fn arm_file(&self, when: Value) -> Result<Value, session_tools::ToolError> {
        session_tools::call(
            &self.context(),
            &self.caller(),
            "alarms.set",
            json!({"when": when, "key": "gate"}),
        )
        .await
    }

    async fn files_inputs(&self) -> Vec<(String, Value)> {
        sqlx::query_as(
            "select content->0->>'text',origin from inputs
             where conversation_id=$1 and origin->>'kind'='alarm' order by input_seq",
        )
        .bind(self.seed.conversation_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn pending_probe_jobs(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from scheduled_jobs
             where state='pending' and payload->>'action'='alarm_probe'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn stop(self) {
        let _ = self.hub.shutdown.send(());
        let _ = self.hub.join.await;
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn file_alarm_polls_and_fires_with_size_time_and_head(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let armed_at = Utc::now();
    worker.push(missing());
    let armed = tool(
        f.arm_file(json!({
            "kind": "file",
            "path": "/tmp/w/gate.rc",
            "min_bytes": 2,
            "read_head": true,
        }))
        .await,
    )?;
    assert_eq!(armed["state"], "armed");
    assert_eq!(worker.seen(), ["/tmp/w/gate.rc"]);
    assert_eq!(f.pending_probe_jobs().await, 1);

    // Первый опрос: файл появился, условие выполнено. Проба идёт отдельной задачей,
    // поэтому задание возвращается через секунду, а ответ забирает следующий тик.
    let written = armed_at + TimeDelta::seconds(20);
    worker.push(exists(2, written, Some("0\n")));
    let scheduler = f.scheduler();
    scheduler.tick(armed_at + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(armed_at + TimeDelta::seconds(32)).await?;
    let state: String = sqlx::query_scalar("select state from alarms where key='gate'")
        .fetch_one(&f.pool)
        .await?;
    assert_eq!(state, "fired");

    scheduler.tick(armed_at + TimeDelta::seconds(40)).await?;
    let inputs = f.files_inputs().await;
    assert_eq!(inputs.len(), 1);
    let text = &inputs[0].0;
    assert!(
        text.contains("Alarm \"gate\" fired: file /tmp/w/gate.rc appeared (2 bytes"),
        "{text}"
    );
    assert!(text.contains("written "), "{text}");
    assert!(text.contains("Head: \"0\\n\"."), "{text}");
    assert_eq!(inputs[0].1["event"]["alarm_path"], "/tmp/w/gate.rc");
    assert_eq!(inputs[0].1["event"]["alarm_size"], 2);

    // Второго срабатывания нет: задание доставки закрыто, будильник не взводится заново.
    scheduler.tick(armed_at + TimeDelta::seconds(80)).await?;
    assert_eq!(f.files_inputs().await.len(), 1);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fresh_file_is_not_rewritten_until_its_mtime_changes(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    let old = now - TimeDelta::minutes(30);
    worker.push(exists(2, old, None));
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;

    // Файл уже был и не менялся: `fresh` по умолчанию не даёт сработать.
    worker.push(exists(2, old, None));
    let scheduler = f.scheduler();
    scheduler.tick(now + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(now + TimeDelta::seconds(32)).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select state from alarms where key='gate'")
            .fetch_one(&f.pool)
            .await?,
        "armed"
    );

    // Перезапись после взвода: время изменения другое — условие выполнено.
    worker.push(exists(2, now + TimeDelta::seconds(40), None));
    scheduler.tick(now + TimeDelta::seconds(63)).await?;
    worker.wait(3).await;
    scheduler.tick(now + TimeDelta::seconds(64)).await?;
    scheduler.tick(now + TimeDelta::seconds(70)).await?;
    let inputs = f.files_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].0.contains("appeared (2 bytes"), "{}", inputs[0].0);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fresh_compares_mtime_with_the_worker_clock(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    // Часы воркера далеко от часов хаба: граница взвода живёт на машине.
    let worker_now = 1_000_000_000_000u64;
    worker.push_at(missing(), worker_now);
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    let scheduler = f.scheduler();

    // Старый артефакт вернули после взвода: его время раньше границы.
    worker.push_at(exists_raw(2, worker_now - 60_000, None), worker_now + 1_000);
    scheduler.tick(Utc::now() + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(Utc::now() + TimeDelta::seconds(32)).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select state from alarms where key='gate'")
            .fetch_one(&f.pool)
            .await?,
        "armed",
        "an older artifact restored after arming is not fresh"
    );

    // Файл написан после взвода: время позже границы, срабатывает.
    worker.push_at(exists_raw(2, worker_now + 5_000, None), worker_now + 2_000);
    scheduler.tick(Utc::now() + TimeDelta::seconds(63)).await?;
    worker.wait(3).await;
    scheduler.tick(Utc::now() + TimeDelta::seconds(64)).await?;
    scheduler.tick(Utc::now() + TimeDelta::seconds(70)).await?;
    assert_eq!(f.files_inputs().await.len(), 1);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_file_at_the_arming_cutoff_is_not_fresh(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let cutoff = 1_500_000_000_000u64;
    // Файл существует ровно на границе: `fresh` требует строго позже.
    worker.push_at(exists_raw(2, cutoff, None), cutoff);
    let armed = tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    assert_eq!(armed["state"], "armed");

    let scheduler = f.scheduler();
    worker.push_at(exists_raw(2, cutoff + 1, None), cutoff + 1);
    scheduler.tick(Utc::now() + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(Utc::now() + TimeDelta::seconds(32)).await?;
    scheduler.tick(Utc::now() + TimeDelta::seconds(70)).await?;
    assert_eq!(f.files_inputs().await.len(), 1, "one millisecond is enough");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unknown_mtime_is_not_fresh(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    // `mtime_ms` вне `i64` не превращается в дату: неизвестное время не свежее.
    worker.push(exists_raw(2, u64::MAX, None));
    let scheduler = f.scheduler();
    scheduler.tick(Utc::now() + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(Utc::now() + TimeDelta::seconds(32)).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select state from alarms where key='gate'")
            .fetch_one(&f.pool)
            .await?,
        "armed"
    );
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancelling_a_file_alarm_cleans_up_its_probe_job(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    assert_eq!(f.pending_probe_jobs().await, 1);

    tool(
        session_tools::call(
            &f.context(),
            &f.caller(),
            "alarms.cancel",
            json!({"key": "gate"}),
        )
        .await,
    )?;
    assert_eq!(
        f.pending_probe_jobs().await,
        0,
        "the probe job is cancelled"
    );
    let seen = worker.seen().len();
    f.scheduler().tick(now + TimeDelta::seconds(31)).await?;
    assert_eq!(worker.seen().len(), seen, "a cancelled alarm is not probed");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delayed_reply_is_not_polled_again_on_every_tick(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    worker.pause();
    let seen_after_arming = worker.seen().len();

    let scheduler = f.scheduler();
    for second in [31, 32, 33, 34] {
        scheduler.tick(now + TimeDelta::seconds(second)).await?;
        assert_eq!(
            worker.seen().len(),
            seen_after_arming + 1,
            "one probe is in flight; tick {second} must not start another"
        );
        assert_eq!(f.pending_probe_jobs().await, 1);
    }
    worker.resume();
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn min_bytes_waits_for_a_larger_file(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({
            "kind": "file",
            "path": "/tmp/w/gate.rc",
            "min_bytes": 10,
            "fresh": false,
        }))
        .await,
    )?;

    worker.push(exists(5, now, None));
    let scheduler = f.scheduler();
    scheduler.tick(now + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(now + TimeDelta::seconds(32)).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("select state from alarms where key='gate'")
            .fetch_one(&f.pool)
            .await?,
        "armed"
    );

    worker.push(exists(12, now, None));
    scheduler.tick(now + TimeDelta::seconds(63)).await?;
    worker.wait(3).await;
    scheduler.tick(now + TimeDelta::seconds(64)).await?;
    scheduler.tick(now + TimeDelta::seconds(70)).await?;
    assert!(f.files_inputs().await[0].0.contains("appeared (12 bytes"));
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn arming_refuses_a_denied_path(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    worker.push(denied("denied"));
    let error = f
        .arm_file(json!({"kind": "file", "path": "/tmp/w/.env"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::PathNotAllowed);
    assert_eq!(f.pending_probe_jobs().await, 0);
    let count: i64 = sqlx::query_scalar("select count(*) from alarms")
        .fetch_one(&f.pool)
        .await?;
    assert_eq!(count, 0);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn arming_needs_path_probe_v1_and_sends_nothing_without_it(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1"]).await;
    let error = f
        .arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::CapabilityMissing);
    assert!(worker.seen().is_empty());
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_offline_machine_retries_and_the_deadline_names_it(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        session_tools::call(
            &f.context(),
            &f.caller(),
            "alarms.set",
            json!({
                "when": {"kind": "file", "path": "/tmp/w/gate.rc"},
                "deadline_seconds": 60,
                "key": "gate",
            }),
        )
        .await,
    )?;
    worker.disconnect();
    // Сеанс исчез: проба отвечает `Unavailable`, задание ждёт следующего опроса.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let scheduler = f.scheduler();
    scheduler.tick(now + TimeDelta::seconds(31)).await?;
    scheduler.tick(now + TimeDelta::seconds(32)).await?;
    let condition: Value = sqlx::query_scalar("select condition from alarms where key='gate'")
        .fetch_one(&f.pool)
        .await?;
    assert!(condition["unavailable_since"].is_string(), "{condition}");

    // Срок: сообщение называет машину и время, с которого её нет.
    scheduler.tick(now + TimeDelta::seconds(61)).await?;
    scheduler.tick(now + TimeDelta::seconds(70)).await?;
    let inputs = f.files_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(
        inputs[0]
            .0
            .contains("timed out after 1 min: machine unavailable since "),
        "{}",
        inputs[0].0
    );
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hung_probe_does_not_block_another_scheduler_job(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    // Второй будильник по времени: он должен сработать, пока проба висит.
    tool(
        session_tools::call(
            &f.context(),
            &f.caller(),
            "alarms.set",
            json!({"at": (now + TimeDelta::seconds(31)).to_rfc3339(), "key": "wake"}),
        )
        .await,
    )?;
    worker.pause();

    let started = std::time::Instant::now();
    let report = tokio::time::timeout(
        Duration::from_secs(3),
        f.scheduler().tick(now + TimeDelta::seconds(36)),
    )
    .await
    .expect("the scheduler tick must not wait for the probe deadline")
    .unwrap();
    assert!(
        started.elapsed() < canterlot_hub::session_tools::probe::RPC_DEADLINE,
        "tick took {:?}, which is not well before the RPC deadline",
        started.elapsed()
    );
    assert_eq!(report.fired, 1, "the time alarm is delivered");
    let probe_still_pending: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs where state='pending'
          and payload->>'action'='alarm_probe')",
    )
    .fetch_one(&f.pool)
    .await?;
    assert!(probe_still_pending);
    let fired: String = sqlx::query_scalar(
        "select content->0->>'text' from inputs where conversation_id=$1 and origin->>'kind'='alarm'",
    )
    .bind(f.seed.conversation_id)
    .fetch_one(&f.pool)
    .await?;
    assert!(fired.contains("fired at"), "{fired}");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_file_alarm_is_picked_up_by_a_new_scheduler(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;

    // «Перезапуск»: новый планировщик с пустым кэшем проб снова спрашивает машину.
    worker.push(exists(2, now + TimeDelta::seconds(20), None));
    let restarted = f.scheduler();
    restarted.tick(now + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    restarted.tick(now + TimeDelta::seconds(32)).await?;
    restarted.tick(now + TimeDelta::seconds(40)).await?;
    assert_eq!(f.files_inputs().await.len(), 1);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_file_alarm_coalesces_with_a_time_alarm(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    worker.push(missing());
    tool(
        f.arm_file(json!({"kind": "file", "path": "/tmp/w/gate.rc"}))
            .await,
    )?;
    tool(
        session_tools::call(
            &f.context(),
            &f.caller(),
            "alarms.set",
            json!({"at": (now + TimeDelta::seconds(31)).to_rfc3339(), "key": "wake"}),
        )
        .await,
    )?;

    let scheduler = f.scheduler();
    worker.push(exists(2, now + TimeDelta::seconds(20), None));
    scheduler.tick(now + TimeDelta::seconds(31)).await?;
    worker.wait(2).await;
    scheduler.tick(now + TimeDelta::seconds(32)).await?;
    // Задание времени доставляет оба сработавших будильника одним вводом.
    scheduler.tick(now + TimeDelta::seconds(36)).await?;
    let inputs = f.files_inputs().await;
    assert_eq!(inputs.len(), 1, "one input for both fired alarms");
    assert!(inputs[0].0.starts_with("[canterlot] 2 alarms fired."));
    assert!(inputs[0].0.contains("file /tmp/w/gate.rc appeared"));
    f.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Файл отчёта автопинка
// ---------------------------------------------------------------------------

/// Цель устоялась в `idle`: проекция снимается задним числом, как в `tests/nudge.rs`.
async fn prime_idle(pool: &PgPool, f: &Fx, now: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(f.child)
        .bind(now - TimeDelta::hours(1))
        .execute(pool)
        .await
        .unwrap();
    activity::refresh(
        pool,
        f.child,
        now - TimeDelta::hours(1),
        &thresholds(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

/// Один законченный ход без маркера готовности: ответ и факты git.
async fn answer_and_run(pool: &PgPool, f: &Fx, text: &str, at: DateTime<Utc>) {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,
             kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.seed.machine_id)
    .bind(f.child)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,
             input_command_id,agent,model,created_seq,created_at,updated_at)
         values($1,$2,$3,'text','final',1,$4,$5,'main','m',
             (select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),
             clock_timestamp()-interval '10 minutes',clock_timestamp()-interval '10 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(f.execution.execution_id)
    .bind(f.child)
    .bind(text)
    .bind(command)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,
             duration_ms,git)
         values($1,$2,$3,$4,$4,1000,$5)",
    )
    .bind(command)
    .bind(f.execution.execution_id)
    .bind(f.child)
    .bind(at)
    .bind(json!({
        "available": true,
        "head_before": "aaa",
        "head_after": "aaa",
        "dirty": false,
        "truncated": false,
        "commits": [],
        "files": []
    }))
    .execute(pool)
    .await
    .unwrap();
}

/// Отпечаток устоявшегося `idle`: задание толчка сверяет его с удержанным.
async fn idle_fingerprint(pool: &PgPool, child: Uuid, now: DateTime<Utc>) -> String {
    let mut tx = pool.begin().await.unwrap();
    let current = subs::settled_live_in_tx(
        &mut tx,
        child,
        now,
        &SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        &thresholds(),
    )
    .await
    .unwrap()
    .expect("the target is settled");
    assert_eq!(current.state.as_str(), "idle");
    tx.commit().await.unwrap();
    current.fingerprint
}

/// Правило в состоянии удержания и задание толчка на сейчас: перепроверка пройдёт.
async fn pending_nudge(
    pool: &PgPool,
    f: &Fx,
    report_file: Option<&str>,
    baseline: Value,
    consecutive: i32,
    at: DateTime<Utc>,
    fingerprint: &str,
) {
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,set_by_conversation,
             notify_conversation_id,enabled,idle_minutes,background_minutes,max_consecutive,
             max_total,done_markers,context_warn_percent,report_file,state,held_fingerprint,
             consecutive,total,progress_baseline)
         values($1,$2,'agent',$3,$3,true,3,10,3,20,$4,null,$5,'pending',$8,$6,1,$7)",
    )
    .bind(f.child)
    .bind(f.seed.owner_id)
    .bind(f.seed.conversation_id)
    .bind(vec![
        "STATUS: DONE".to_owned(),
        "STATUS: BLOCKED".to_owned(),
    ])
    .bind(report_file)
    .bind(consecutive)
    .bind(&baseline)
    .bind(fingerprint)
    .execute(pool)
    .await
    .unwrap();
    let job = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$5,$4)",
    )
    .bind(job)
    .bind(f.seed.owner_id)
    .bind(f.child)
    .bind(json!({"action": "nudge", "fingerprint": fingerprint}))
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

/// Отпечаток пустого списка изменённых файлов: так его считает продукт.
const EMPTY_FILES: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn baseline(size: i64, mtime: DateTime<Utc>) -> Value {
    // Проба отдаёт время с точностью до миллисекунды: база должна совпадать.
    let mtime = DateTime::from_timestamp_millis(mtime.timestamp_millis()).unwrap();
    json!({
        "head": "aaa",
        "dirty": false,
        "files": EMPTY_FILES,
        "report_size": size,
        "report_mtime": mtime.to_rfc3339(),
        "report_known": true,
        "run_id": null,
        "tools": 0,
        "working_ms": 0,
    })
}

async fn nudge_events(pool: &PgPool, child: Uuid) -> Vec<(String, Value)> {
    sqlx::query_as("select outcome,signals from nudge_events where conversation_id=$1 order by at")
        .bind(child)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn nudge_inputs(pool: &PgPool, child: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "select content->0->>'text' from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(child)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_growth_counts_as_progress_and_prevents_exhaustion(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    prime_idle(&f.pool, &f, now).await;
    answer_and_run(
        &f.pool,
        &f,
        "Still working on the parser.",
        now - TimeDelta::minutes(1),
    )
    .await;
    activity::refresh(&f.pool, f.child, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
    let at = now + TimeDelta::minutes(3);
    let fingerprint = idle_fingerprint(&f.pool, f.child, at + TimeDelta::seconds(1)).await;
    pending_nudge(
        &f.pool,
        &f,
        Some("/tmp/w/report.md"),
        baseline(10, now - TimeDelta::minutes(5)),
        3,
        at,
        &fingerprint,
    )
    .await;
    worker.push(exists(20, now, None));

    let scheduler = f.scheduler();
    scheduler.tick(at).await?;
    worker.wait(1).await;
    scheduler.tick(at + TimeDelta::seconds(1)).await?;

    let (state, consecutive): (String, i32) =
        sqlx::query_as("select state,consecutive from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&f.pool)
            .await?;
    assert_eq!(state, "watching");
    assert_eq!(
        consecutive, 1,
        "growth starts a new series instead of giving up"
    );
    assert_eq!(nudge_inputs(&f.pool, f.child).await.len(), 1);
    assert_eq!(nudge_events(&f.pool, f.child).await[0].0, "nudged");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unchanged_report_is_not_progress(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    prime_idle(&f.pool, &f, now).await;
    answer_and_run(
        &f.pool,
        &f,
        "Still working on the parser.",
        now - TimeDelta::minutes(1),
    )
    .await;
    activity::refresh(&f.pool, f.child, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
    let at = now + TimeDelta::minutes(3);
    let fingerprint = idle_fingerprint(&f.pool, f.child, at + TimeDelta::seconds(1)).await;
    let report_time = now - TimeDelta::minutes(5);
    // Все неотчётные поля — как в продукте: меняется только отчёт, и он тот же.
    pending_nudge(
        &f.pool,
        &f,
        Some("/tmp/w/report.md"),
        baseline(20, report_time),
        2,
        at,
        &fingerprint,
    )
    .await;
    worker.push(exists(20, report_time, None));

    let scheduler = f.scheduler();
    scheduler.tick(at).await?;
    worker.wait(1).await;
    scheduler.tick(at + TimeDelta::seconds(1)).await?;

    let (state, consecutive): (String, i32) =
        sqlx::query_as("select state,consecutive from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&f.pool)
            .await?;
    assert_eq!(state, "watching");
    assert_eq!(
        consecutive, 3,
        "an unchanged report must not look like progress"
    );
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_missing_report_is_named_in_the_text_and_signals(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    let now = Utc::now();
    prime_idle(&f.pool, &f, now).await;
    answer_and_run(
        &f.pool,
        &f,
        "Still working on the parser.",
        now - TimeDelta::minutes(1),
    )
    .await;
    activity::refresh(&f.pool, f.child, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
    let at = now + TimeDelta::minutes(3);
    let fingerprint = idle_fingerprint(&f.pool, f.child, at + TimeDelta::seconds(1)).await;
    pending_nudge(
        &f.pool,
        &f,
        Some("/tmp/w/report.md"),
        baseline(10, now - TimeDelta::minutes(5)),
        0,
        at,
        &fingerprint,
    )
    .await;
    worker.push(missing());

    let scheduler = f.scheduler();
    scheduler.tick(at).await?;
    worker.wait(1).await;
    scheduler.tick(at + TimeDelta::seconds(1)).await?;

    let text = nudge_inputs(&f.pool, f.child).await.remove(0);
    assert!(
        text.contains("The report file /tmp/w/report.md does not exist yet."),
        "{text}"
    );
    let signals = nudge_events(&f.pool, f.child).await.remove(0).1;
    assert_eq!(signals["report_missing"], true, "{signals}");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_probe_is_recorded_and_does_not_block_the_nudge(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    prime_idle(&f.pool, &f, now).await;
    answer_and_run(
        &f.pool,
        &f,
        "Still working on the parser.",
        now - TimeDelta::minutes(1),
    )
    .await;
    activity::refresh(&f.pool, f.child, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
    let at = now + TimeDelta::minutes(3);
    let fingerprint = idle_fingerprint(&f.pool, f.child, at + TimeDelta::seconds(1)).await;
    pending_nudge(
        &f.pool,
        &f,
        Some("/tmp/w/report.md"),
        baseline(10, now - TimeDelta::minutes(5)),
        0,
        at,
        &fingerprint,
    )
    .await;

    // Реестр пуст: сеанса нет, проба отвечает `Unavailable`; толчок всё равно уходит.
    let scheduler = Scheduler::new(
        f.pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig::default(),
    );
    scheduler.tick(at).await?;
    scheduler.tick(at + TimeDelta::seconds(1)).await?;

    let text = nudge_inputs(&f.pool, f.child).await.remove(0);
    assert!(text.contains("STATUS: DONE"), "{text}");
    assert!(!text.contains("does not exist yet"), "{text}");
    let signals = nudge_events(&f.pool, f.child).await.remove(0).1;
    assert_eq!(signals["report_probe_failed"], true, "{signals}");
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_set_resolves_the_report_path_and_refuses_a_denied_one(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1", "path_probe_v1"]).await;
    worker.push(missing());
    tool(
        session_tools::call(
            &f.context(),
            &f.caller(),
            "nudge.set",
            json!({"session_id": f.child, "report_file": "reports/out.md"}),
        )
        .await,
    )?;
    let stored: String =
        sqlx::query_scalar("select report_file from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&f.pool)
            .await?;
    assert_eq!(stored, "/tmp/w/reports/out.md");
    assert_eq!(worker.seen(), ["/tmp/w/reports/out.md"]);

    worker.push(denied("denied"));
    let error = session_tools::call(
        &f.context(),
        &f.caller(),
        "nudge.set",
        json!({"session_id": f.child, "report_file": "/tmp/w/.env"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::PathNotAllowed);
    f.stop().await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_set_needs_path_probe_v1_for_a_report(pool: PgPool) -> TestResult {
    let f = Fx::new(pool).await;
    let _worker = FakeWorker::connect(&f.hub, &f.seed, &["pilot_v1"]).await;
    let error = session_tools::call(
        &f.context(),
        &f.caller(),
        "nudge.set",
        json!({"session_id": f.child, "report_file": "/tmp/w/report.md"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::CapabilityMissing);
    f.stop().await;
    Ok(())
}
