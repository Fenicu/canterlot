#![forbid(unsafe_code)]

//! Весь боевой путь OpenCode через настоящего `Worker` и хаб: конфигурация из TOML,
//! `discover` и `configure_adapters` в `session.rs`, каталог машины, запуск супервизора
//! командой хаба и ход до записанных блоков.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    delivery::{
        control,
        submit::{self, ContentBlock, SubmitInput},
    },
    executions, machines,
    model::PauseReason,
    testkit::{self, TestHub, spawn_test_hub},
};
use canterlot_worker::{
    adapter::opencode::replay::{Script, Step},
    config::WorkerConfig,
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use serde_json::json;
use sqlx::PgPool;
use tokio::{sync::watch, time::timeout};
use uuid::Uuid;

#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

use crate::opencode;
use opencode::{Fake, find};

const WAIT: Duration = Duration::from_secs(60);

async fn until(what: &str, condition: impl AsyncFnMut() -> bool) {
    until_within(WAIT, what, condition).await;
}

async fn until_within(limit: Duration, what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(limit, async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

struct Paths<'a> {
    root: &'a Path,
    work: &'a Path,
    port: u16,
    executable: &'a Path,
}

fn worker_toml(paths: &Paths<'_>, machine: Uuid, installation: Uuid, restore: Uuid) -> String {
    format!(
        r#"
hub_url = "https://localhost:{port}"
ca_cert = "{root}/ca.pem"
token_file = "{root}/token"
machine_id = "{machine}"
installation_id = "{installation}"
restore_generation = "{restore}"
data_dir = "{root}/store"

[timers]
handshake = "5s"
heartbeat = "100ms"
heartbeat_ack = "5s"
range_ack = "3s"
backoff_min = "250ms"
backoff_max = "500ms"

[execution]
allowed_workdir_roots = ["{work}"]
supervisor_exe = "{exe}"
spawn_timeout = "5s"

[execution.supervisor]
stop_grace = "500ms"
stop_retry = "300ms"

[opencode]
executable = "{executable}"
providers = ["fake"]
"#,
        port = paths.port,
        root = paths.root.display(),
        work = paths.work.display(),
        exe = env!("CARGO_BIN_EXE_canterlot-worker"),
        executable = paths.executable.display(),
    )
}

/// Запись хода и следом период без ввода: так выглядит завершение фоновой работы.
fn with_autonomous_run(mut script: Script) -> Script {
    let session = script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()
                .and_then(|body| body.pointer("/data/id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .unwrap();
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.step.started", "data": {
                "sessionID": session, "assistantMessageID": "msg_auto", "agent": "build",
                "model": {"id": "fake-1", "providerID": "fake"}}}),
            json!({"type": "session.step.ended", "data": {
                "sessionID": session, "assistantMessageID": "msg_auto", "finish": "stop",
                "tokens": {"input": 7, "output": 2, "reasoning": 0,
                           "cache": {"read": 0, "write": 0}}}}),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": session}}),
        ],
    });
    script
}

/// Хаб и настоящий воркер с таблицей `[opencode]`, двойник вместо CLI.
struct Stack {
    pool: PgPool,
    hub: TestHub,
    _root: tempfile::TempDir,
    guard: ProcessGuard,
    fake: Fake,
    stop: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), WorkerError>>,
    owner: Uuid,
    conversation: Uuid,
    machine_id: Uuid,
    state: watch::Receiver<SessionState>,
}

impl Stack {
    async fn start(pool: PgPool, script: Script) -> Self {
        Self::start_with(pool, Fake::new(script), true, None).await
    }

    async fn start_with(
        pool: PgPool,
        fake: Fake,
        wait_for_catalog: bool,
        retry_delays: Option<Vec<Duration>>,
    ) -> Self {
        let timers = HubTimers {
            dispatch_sweep: Duration::from_millis(20),
            ..Default::default()
        };
        let hub = spawn_test_hub(pool.clone(), timers).await;
        let root = tempfile::tempdir().unwrap();
        let guard = ProcessGuard::new(root.path());
        let machine = machines::add(&pool, hub.installation.owner_id, "opencode")
            .await
            .unwrap();
        let token_file = root.path().join("token");
        fs::write(&token_file, machine.token).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(root.path().join("ca.pem"), &hub.pki.ca_pem).unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let config: WorkerConfig = toml::from_str(&worker_toml(
            &Paths {
                root: root.path(),
                work: &work,
                port: hub.addr.port(),
                executable: &fake.exe,
            },
            machine.machine_id,
            hub.installation.installation_id,
            hub.installation.restore_generation,
        ))
        .unwrap();
        let owner = hub.installation.owner_id;
        let conversation = testkit::seed_conversation(&pool, owner, machine.machine_id).await;
        sqlx::query(
            "update conversations set workdir = $2, harness = 'opencode', model = 'fake/fake-1',
             effort = 'high', permission_mode = 'bypass' where id = $1",
        )
        .bind(conversation)
        .bind(work.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let store = WorkerStore::open(&config.data_dir).unwrap();
        let machine_id = config.machine_id;
        let runtime_dir = config.data_dir.join("runtime");
        let mut worker = Worker::new(config, store).with_runtime_dir(runtime_dir);
        if let Some(delays) = retry_delays {
            worker = worker.with_catalog_retry_delays(delays);
        }
        let state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        until("worker live", async || {
            *state.borrow() == SessionState::Live && hub.registry.is_reconciled(machine_id)
        })
        .await;
        if wait_for_catalog {
            until("harness in the catalog", async || {
                sqlx::query_scalar::<_, i64>(
                    "select count(*) from machine_harnesses where machine_id = $1
                     and harness = 'opencode' and provider = 'opencode'",
                )
                .bind(machine_id)
                .fetch_one(&pool)
                .await
                .unwrap()
                    == 1
            })
            .await;
        }
        Self {
            pool,
            hub,
            _root: root,
            guard,
            fake,
            stop,
            join,
            owner,
            conversation,
            machine_id,
            state,
        }
    }

    async fn execution(&self) -> Uuid {
        let started = executions::start(&self.pool, self.owner, self.conversation, Uuid::now_v7())
            .await
            .unwrap();
        let execution = started.execution_id;
        until("execution running", async || {
            sqlx::query_scalar::<_, String>("select state from executions where id = $1")
                .bind(execution)
                .fetch_one(&self.pool)
                .await
                .unwrap()
                == "running"
        })
        .await;
        execution
    }

    async fn say(&self, text: &str) -> Uuid {
        let input = submit::submit_input(
            &self.pool,
            SubmitInput {
                owner_id: self.owner,
                conversation_id: self.conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text: text.into() }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap();
        self.hub.dispatcher.trigger(self.conversation);
        input.input_id
    }

    async fn finished_runs(&self, execution: Uuid, count: i64) {
        until("run finished", async || {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from events where execution_id = $1 and kind = 'run_finished'",
            )
            .bind(execution)
            .fetch_one(&self.pool)
            .await
            .unwrap()
                >= count
        })
        .await;
    }

    async fn pauses(&self) -> Vec<PauseReason> {
        let (pauses,): (sqlx::types::Json<Vec<PauseReason>>,) =
            sqlx::query_as("select pause_reasons from queue_state where conversation_id = $1")
                .bind(self.conversation)
                .fetch_one(&self.pool)
                .await
                .unwrap();
        pauses.0
    }

    async fn wait_paused(&self, wanted: impl Fn(&PauseReason) -> bool) -> PauseReason {
        let mut found = None;
        until("queue paused", async || {
            found = self.pauses().await.into_iter().find(&wanted);
            found.is_some()
        })
        .await;
        found.unwrap()
    }

    async fn close(self) {
        self.stop.send(true).unwrap();
        timeout(WAIT, self.join).await.unwrap().unwrap().unwrap();
        self.hub.shutdown.send(()).unwrap();
        timeout(WAIT, self.hub.join)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.guard.cleanup();
        self.fake.wait_gone().await;
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_with_the_table_reports_the_harness_and_runs_a_turn(pool: PgPool) {
    let stack = Stack::start(
        pool.clone(),
        with_autonomous_run(
            Script::from_capture("start_stream", None)
                .with_canterlot_mcp("connected")
                .with_recorded_models(),
        ),
    )
    .await;

    // Каталог дошёл до хаба: форма и проверка настроек берут модели отсюда.
    let (models, modes, midsession): (serde_json::Value, serde_json::Value, bool) = sqlx::query_as(
        "select models, permission_modes, settings_midsession from machine_harnesses
         where machine_id = $1 and harness = 'opencode'",
    )
    .bind(stack.machine_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(models[0]["id"], "fake/fake-1");
    assert_eq!(models[0]["efforts"], json!(["low", "medium", "high"]));
    assert_eq!(modes, json!(["owner", "ask", "bypass", "read_only"]));
    assert!(midsession);

    let execution = stack.execution().await;
    let native: Option<String> =
        sqlx::query_scalar("select native_conversation_id from executions where id = $1")
            .bind(execution)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(native.is_some_and(|id| id.starts_with("ses_")));

    let input = stack.say("Скажи привет").await;
    stack.finished_runs(execution, 1).await;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select kind, content from blocks where execution_id = $1 and state = 'final' order by kind",
    )
    .bind(execution)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("reasoning".to_owned(), "think more".to_owned()),
            ("text".to_owned(), "Hello world".to_owned()),
        ]
    );
    let accepted: String = sqlx::query_scalar("select state from inputs where id = $1")
        .bind(input)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(accepted, "accepted");
    let requests = stack.fake.requests();
    let prompt = find(&requests, "POST", "/prompt").unwrap();
    assert_eq!(prompt["body"]["text"], "Скажи привет");
    stack.close().await;
}

/// Ход с инструментом в режиме владельца: вызов виден сразу по готовности входа,
/// пометка политики — на результате, скорость честная.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn tool_turn_in_owner_mode_yields_a_speed(pool: PgPool) {
    use canterlot_hub::speed;
    let mut script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let session = script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()?
                .pointer("/data/id")?
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .unwrap();
    let prompt = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(prompt + 1);
    let message = "msg_speedtool2";
    let started = |suffix: &str| {
        json!({"type": suffix, "data": {
            "sessionID": session, "assistantMessageID": message,
            "agent": "build", "model": {"id": "deepseek-v4.1-flash", "providerID": "opencode-go"}}})
    };
    let frame = |suffix: &str, data: serde_json::Value| {
        let mut data = data;
        data["sessionID"] = session.clone().into();
        data["assistantMessageID"] = message.into();
        json!({"type": suffix, "data": data})
    };
    let call = "call_speedtool2";
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.inbox.enqueued", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur",
                "sessionID": session,
                "item": {"type": "user", "payload": {"text": "say hello"}}}}),
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.inbox.delivered", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur", "sessionID": session}}),
            started("session.step.started"),
            frame("session.reasoning.started", json!({"ordinal": 0})),
            frame(
                "session.tool.input.started",
                json!({"id": call, "name": "read"}),
            ),
        ],
    });
    script.steps.push(Step::Stall { ms: 400 });
    script.steps.push(Step::Emit {
        events: vec![
            frame(
                "session.reasoning.delta",
                json!({"ordinal": 0, "delta": "let me read"}),
            ),
            frame(
                "session.reasoning.ended",
                json!({"ordinal": 0, "text": "let me read"}),
            ),
            frame(
                "session.tool.input.ended",
                json!({"id": call, "text": "{\"path\":\"/tmp/notes.txt\"}"}),
            ),
            frame(
                "session.tool.called",
                json!({"id": call, "input": {"path": "/tmp/notes.txt"}}),
            ),
            frame(
                "session.tool.success",
                json!({"id": call, "content": [{"type": "text", "text": "beta"}],
                    "metadata": {"truncated": false, "exit": 0}}),
            ),
            frame("session.step.streamed", json!({})),
            frame(
                "session.step.ended",
                json!({"finish": "tool-calls", "rawFinish": "tool_calls", "cost": 0,
                    "tokens": {"input": 11540, "output": 47, "reasoning": 20,
                               "cache": {"read": 4480, "write": 0}}}),
            ),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": session}}),
        ],
    });
    let stack = Stack::start(pool.clone(), script).await;
    sqlx::query(
        "update conversations set permission_mode = 'owner', effort = 'high' where id = $1",
    )
    .bind(stack.conversation)
    .execute(&pool)
    .await
    .unwrap();
    let execution = stack.execution().await;
    stack.say("Прочитай файл").await;
    stack.finished_runs(execution, 1).await;

    let run: (Option<String>, bool, bool, Uuid) = sqlx::query_as(
        "select provider, exact, finished_at is not null, input_command_id from runs
         where execution_id = $1",
    )
    .bind(execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(run.0.as_deref(), Some("opencode"));
    assert!(run.1 && run.2);
    let command = run.3;

    let mut conn = pool.acquire().await.unwrap();
    let speed = speed::for_runs(&mut conn, &[command]).await.unwrap();
    let speed = speed.get(&command).cloned().unwrap_or_default();
    eprintln!("SPEED: {speed:?}");
    assert!(
        speed.iter().any(|s| s.tokens_per_second.is_some()),
        "tool turn in owner mode must yield tokens per second: {speed:?}"
    );
    stack.close().await;
}

/// Ход с инструментом в режиме по умолчанию: рассуждение и вызов дают честную генерацию.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn tool_turn_in_bypass_yields_a_speed(pool: PgPool) {
    use canterlot_hub::speed;
    let mut script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let session = script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()?
                .pointer("/data/id")?
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .unwrap();
    let prompt = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(prompt + 1);
    // Кадры как в записи `live/turn_tools_flash` (OpenCode 2.2.6): рассуждение,
    // вызов read, короткий ответ. `tool.called` приходит до готовности входа.
    let message = "msg_speedtool1";
    let started = |suffix: &str| {
        json!({"type": suffix, "data": {
            "sessionID": session, "assistantMessageID": message,
            "agent": "build", "model": {"id": "deepseek-v4.1-flash", "providerID": "opencode-go"}}})
    };
    let frame = |suffix: &str, data: serde_json::Value| {
        let mut data = data;
        data["sessionID"] = session.clone().into();
        data["assistantMessageID"] = message.into();
        json!({"type": suffix, "data": data})
    };
    let call = "call_speedtool1";
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.inbox.enqueued", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur",
                "sessionID": session,
                "item": {"type": "user", "payload": {"text": "say hello"}}}}),
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.inbox.delivered", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur", "sessionID": session}}),
            started("session.step.started"),
            frame("session.reasoning.started", json!({"ordinal": 0})),
            frame(
                "session.tool.input.started",
                json!({"id": call, "name": "read"}),
            ),
        ],
    });
    script.steps.push(Step::Stall { ms: 400 });
    script.steps.push(Step::Emit {
        events: vec![
            frame(
                "session.reasoning.delta",
                json!({"ordinal": 0, "delta": "let me read"}),
            ),
            frame(
                "session.reasoning.ended",
                json!({"ordinal": 0, "text": "let me read"}),
            ),
            frame(
                "session.tool.input.ended",
                json!({"id": call, "text": "{\"path\":\"/tmp/notes.txt\"}"}),
            ),
            frame(
                "session.tool.called",
                json!({"id": call, "input": {"path": "/tmp/notes.txt"}}),
            ),
            frame(
                "session.tool.success",
                json!({"id": call, "content": [{"type": "text", "text": "beta"}],
                    "metadata": {"truncated": false, "exit": 0}}),
            ),
            frame("session.step.streamed", json!({})),
            frame(
                "session.step.ended",
                json!({"finish": "tool-calls", "rawFinish": "tool_calls", "cost": 0,
                    "tokens": {"input": 11540, "output": 47, "reasoning": 20,
                               "cache": {"read": 4480, "write": 0}}}),
            ),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": session}}),
        ],
    });
    let stack = Stack::start(pool.clone(), script).await;
    let execution = stack.execution().await;
    stack.say("Прочитай файл").await;
    stack.finished_runs(execution, 1).await;

    let run: (Option<String>, bool, bool, Uuid) = sqlx::query_as(
        "select provider, exact, finished_at is not null, input_command_id from runs
         where execution_id = $1",
    )
    .bind(execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(run.0.as_deref(), Some("opencode"));
    assert!(run.1 && run.2);
    let command = run.3;

    let blocks: Vec<(String, String, String, bool)> = sqlx::query_as(
        "select kind, agent, model, finished_at is not null from blocks
         where input_command_id = $1 order by created_seq",
    )
    .bind(command)
    .fetch_all(&pool)
    .await
    .unwrap();
    eprintln!("BLOCKS: {blocks:?}");
    let usage: Vec<(String, String, i64)> = sqlx::query_as(
        "select model, agent, output_tokens from run_model_usage where input_command_id = $1",
    )
    .bind(command)
    .fetch_all(&pool)
    .await
    .unwrap();
    eprintln!("USAGE: {usage:?}");

    let mut conn = pool.acquire().await.unwrap();
    let speed = speed::for_runs(&mut conn, &[command]).await.unwrap();
    let speed = speed.get(&command).cloned().unwrap_or_default();
    eprintln!("SPEED: {speed:?}");
    assert!(
        speed.iter().any(|s| s.tokens_per_second.is_some()),
        "tool turn must yield tokens per second: {speed:?}"
    );
    stack.close().await;
}

/// Сбой пробы на старте не убирает харнесс до перезапуска: повтор доносит каталог до хаба.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn failed_startup_probe_is_retried_and_updates_the_catalog(pool: PgPool) {
    let script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let stack = Stack::start_with(
        pool.clone(),
        Fake::failing_first(script),
        false,
        Some(vec![Duration::from_secs(1)]),
    )
    .await;

    let harnesses = sqlx::query_scalar::<_, i64>(
        "select count(*) from machine_harnesses where machine_id = $1 and harness = 'opencode'",
    )
    .bind(stack.machine_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(harnesses, 0);
    assert_eq!(*stack.state.borrow(), SessionState::Live);

    until_within(
        Duration::from_secs(10),
        "opencode catalog after retry",
        async || {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from machine_harnesses where machine_id = $1
                 and harness = 'opencode' and provider = 'opencode'",
            )
            .bind(stack.machine_id)
            .fetch_one(&pool)
            .await
            .unwrap()
                == 1
        },
    )
    .await;

    assert_eq!(*stack.state.borrow(), SessionState::Live);
    let reports: i64 =
        sqlx::query_scalar("select count(*) from events where kind = 'machine_report_updated'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        reports >= 2,
        "catalog update did not reach the hub: {reports}"
    );
    stack.close().await;
}

/// Упор в лимит доходит до очереди: следующий ввод не уходит в тот же отказ.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn provider_quota_pauses_the_queue_with_the_reset_time(pool: PgPool) {
    let mut script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if event["type"] == "session.execution.succeeded" {
                event["type"] = "session.execution.failed".into();
                event["data"]["error"] = json!({
                    "type": "provider.quota", "message": "Go usage limit reached",
                    "status": 429, "retryAfterMs": 3_600_000});
            }
        }
    }
    let stack = Stack::start(pool, script).await;
    let execution = stack.execution().await;
    stack.say("x").await;
    stack.finished_runs(execution, 1).await;
    let pause = stack
        .wait_paused(|reason| matches!(reason, PauseReason::LimitReached { .. }))
        .await;
    let PauseReason::LimitReached {
        provider,
        window,
        resets_at,
    } = pause
    else {
        unreachable!();
    };
    assert_eq!(provider.as_deref(), Some("opencode"));
    assert_eq!(window.as_deref(), Some("five_hour"));
    assert!(resets_at.is_some_and(|at| at > chrono::Utc::now()));
    stack.close().await;
}

/// Весь путь застрявшей сессии: факт воркера, пауза, команда хаба, снятие паузы.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stuck_session_pauses_the_queue_until_compaction_finishes(pool: PgPool) {
    let script = Script::from_capture("stuck_invalid_request", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    let stack = Stack::start(pool.clone(), script).await;
    let execution = stack.execution().await;
    stack.say("one").await;
    stack.finished_runs(execution, 1).await;
    assert_eq!(stack.pauses().await, vec![]);
    stack.say("two").await;
    stack.finished_runs(execution, 2).await;
    stack
        .wait_paused(|reason| matches!(reason, PauseReason::SessionStuck { .. }))
        .await;
    control::request_compaction(
        &pool,
        stack.owner,
        stack.conversation,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    until("pause lifted", async || stack.pauses().await.is_empty()).await;
    stack.say("three").await;
    stack.finished_runs(execution, 3).await;
    let requests = stack.fake.requests();
    assert!(find(&requests, "POST", "/compact").is_some());
    let prompts = requests
        .iter()
        .filter(|request| {
            request["path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("/prompt")
        })
        .count();
    assert_eq!(prompts, 3);
    stack.close().await;
}

/// Обычный текстовый ход OpenCode даёт скорость: кадры как в записи
/// `live/turn_text_flash` (OpenCode 2.2.6), генерация разнесена паузой двойника,
/// чтобы время генерации было честным.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn plain_text_turn_yields_a_speed(pool: PgPool) {
    use canterlot_hub::speed;
    let model = "opencode-go/deepseek-v4.1-flash";
    let mut script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let session = script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()?
                .pointer("/data/id")?
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .unwrap();
    let prompt = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(prompt + 1);
    let message = "msg_speedtext1";
    let started = |suffix: &str| {
        json!({"type": suffix, "data": {
            "sessionID": session, "assistantMessageID": message,
            "agent": "build", "model": {"id": "deepseek-v4.1-flash", "providerID": "opencode-go"}}})
    };
    let frame = |suffix: &str, data: serde_json::Value| {
        let mut data = data;
        data["sessionID"] = session.clone().into();
        data["assistantMessageID"] = message.into();
        json!({"type": suffix, "data": data})
    };
    // Сервер ставит ввод в очередь и отдаёт его ходу: без этих кадров конец хода
    // выглядит чужим периодом и ход не закрывается. Идентификаторы записанные:
    // двойник подменяет их живыми из привязки ответа `/prompt`.
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.inbox.enqueued", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur",
                "sessionID": session,
                "item": {"type": "user", "payload": {"text": "say hello"}}}}),
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.inbox.delivered", "data": {
                "inboxID": "msg_18f9dc297001Uo9jYOKSsV0hur", "sessionID": session}}),
            started("session.step.started"),
            frame("session.text.started", json!({"ordinal": 0})),
        ],
    });
    // Честная генерация: между первым кадром текста и его концом 400 мс.
    script.steps.push(Step::Stall { ms: 400 });
    script.steps.push(Step::Emit {
        events: vec![
            frame(
                "session.text.delta",
                json!({"ordinal": 0, "delta": "hello "}),
            ),
            frame(
                "session.text.delta",
                json!({"ordinal": 0, "delta": "world"}),
            ),
            frame(
                "session.text.ended",
                json!({"ordinal": 0, "text": "hello world"}),
            ),
            frame("session.step.streamed", json!({})),
            frame(
                "session.step.ended",
                json!({"finish": "stop", "rawFinish": "stop", "cost": 0,
                    "tokens": {"input": 15196, "output": 300, "reasoning": 0,
                               "cache": {"read": 384, "write": 0}}}),
            ),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": session}}),
        ],
    });
    let stack = Stack::start(pool.clone(), script).await;
    let execution = stack.execution().await;
    stack.say("Скажи привет").await;
    stack.finished_runs(execution, 1).await;

    let run: (Option<String>, bool, bool, Uuid) = sqlx::query_as(
        "select provider, exact, finished_at is not null, input_command_id from runs
         where execution_id = $1",
    )
    .bind(execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(run.0.as_deref(), Some("opencode"), "provider хода");
    assert!(run.1, "ход шёл вживую");
    assert!(run.2, "ход завершён");
    let command = run.3;

    let blocks: Vec<(String, String, String, bool)> = sqlx::query_as(
        "select kind, agent, model, finished_at is not null from blocks
         where input_command_id = $1 order by created_seq",
    )
    .bind(command)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        blocks,
        [(
            "text".to_owned(),
            "orchestrator".to_owned(),
            model.to_owned(),
            true
        )],
        "текст основного агента с моделью шага",
    );
    let usage: Vec<(String, String, i64)> = sqlx::query_as(
        "select model, agent, output_tokens from run_model_usage where input_command_id = $1",
    )
    .bind(command)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        usage,
        [(model.to_owned(), "orchestrator".to_owned(), 300)],
        "выходные токены шага",
    );

    let mut conn = pool.acquire().await.unwrap();
    let speed = speed::for_runs(&mut conn, &[command]).await.unwrap();
    let speed = speed.get(&command).cloned().unwrap_or_default();
    assert_eq!(speed.len(), 1, "ход даёт одну скорость: {speed:?}");
    assert_eq!(speed[0].model, model);
    assert_eq!(speed[0].output_tokens, 300);
    assert!(
        speed[0].tokens_per_second.is_some_and(|tps| tps > 0.0),
        "токены в секунду: {speed:?}"
    );
    assert!(
        speed[0].first_token_ms.is_some(),
        "задержка первого токена: {speed:?}"
    );
    stack.close().await;
}
