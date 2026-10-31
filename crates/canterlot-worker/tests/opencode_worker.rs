#![forbid(unsafe_code)]
#![cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]

//! Весь боевой путь OpenCode через настоящего `Worker` и хаб: конфигурация из TOML,
//! `discover` и `configure_adapters` в `session.rs`, каталог машины, запуск супервизора
//! командой хаба и ход до записанных блоков.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    delivery::submit::{self, ContentBlock, SubmitInput},
    executions, machines,
    testkit::{self, spawn_test_hub},
};
use canterlot_worker::{
    adapter::opencode::replay::{Script, Step},
    config::WorkerConfig,
    session::{SessionState, Worker},
    store::WorkerStore,
};
use serde_json::json;
use sqlx::PgPool;
use tokio::{sync::watch, time::timeout};
use uuid::Uuid;

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

#[path = "support/opencode.rs"]
mod opencode;
use opencode::{Fake, find};

const WAIT: Duration = Duration::from_secs(60);

async fn until(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
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

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_with_the_table_reports_the_harness_and_runs_a_turn(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
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
    let fake = Fake::new(with_autonomous_run(
        Script::from_capture("start_stream", None)
            .with_canterlot_mcp("connected")
            .with_recorded_models(),
    ));
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
    let worker = Worker::new(config, store);
    let state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    until("worker live", async || {
        *state.borrow() == SessionState::Live && hub.registry.is_reconciled(machine_id)
    })
    .await;
    until("harness in the catalog", async || {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from machine_harnesses where machine_id = $1 and harness = 'opencode'
             and provider = 'opencode'",
        )
        .bind(machine_id)
        .fetch_one(&pool)
        .await
        .unwrap()
            == 1
    })
    .await;

    // Каталог дошёл до хаба: форма и проверка настроек берут модели отсюда.
    let (models, modes, midsession): (serde_json::Value, serde_json::Value, bool) = sqlx::query_as(
        "select models, permission_modes, settings_midsession from machine_harnesses
         where machine_id = $1 and harness = 'opencode'",
    )
    .bind(machine_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(models[0]["id"], "fake/fake-1");
    assert_eq!(models[0]["efforts"], json!(["low", "medium", "high"]));
    assert_eq!(modes, json!(["owner", "ask", "bypass", "read_only"]));
    assert!(midsession);

    let started = executions::start(&pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    let execution = started.execution_id;
    let running = async || {
        sqlx::query_scalar::<_, String>("select state from executions where id = $1")
            .bind(execution)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "running"
    };
    until("execution running", running).await;
    let native: Option<String> =
        sqlx::query_scalar("select native_conversation_id from executions where id = $1")
            .bind(execution)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(native.is_some_and(|id| id.starts_with("ses_")));

    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: owner,
            conversation_id: conversation,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "Скажи привет".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    hub.dispatcher.trigger(conversation);
    until("run finished", async || {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from events where execution_id = $1 and kind = 'run_finished'",
        )
        .bind(execution)
        .fetch_one(&pool)
        .await
        .unwrap()
            == 1
    })
    .await;
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
        .bind(input.input_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(accepted, "accepted");
    let requests = fake.requests();
    let prompt = find(&requests, "POST", "/prompt").unwrap();
    assert_eq!(prompt["body"]["text"], "Скажи привет");

    stop.send(true).unwrap();
    timeout(WAIT, join).await.unwrap().unwrap().unwrap();
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
    guard.cleanup();
    fake.wait_gone().await;
}
