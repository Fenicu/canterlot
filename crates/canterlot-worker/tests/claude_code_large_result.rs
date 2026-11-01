#![forbid(unsafe_code)]

//! Крупный результат инструмента Claude Code одним кадром `user`, без потоковых дельт:
//! путь от кадра двойника CLI через разбор, настоящий супервизор и приём хаба до текста,
//! который видит владелец. Пустой или недособранный блок здесь недопустим.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    delivery::submit::{self, ContentBlock, SubmitInput},
    executions, history, machines,
    testkit::{self, spawn_test_hub},
};
use canterlot_proto::limits::{
    BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES, MAX_EVENT_BATCH_BYTES,
    MAX_SJ_BATCH_RECORDS,
};
use canterlot_worker::{
    config::WorkerConfig,
    session::{SessionState, Worker},
    store::WorkerStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::{sync::watch, time::timeout};
use uuid::Uuid;

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/fake_exec.rs"]
mod fake_exec;
#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

// Двойник из `claude_code_e2e.rs`: кадры хода берёт из `turn<N>.json` рядом с собой.
const FAKE: &str = include_str!("support/fake_claude.py");

const WAIT: Duration = Duration::from_secs(90);
const PAGE_BYTES: i64 = 262_144;

async fn until(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// Ровно `len` байт: повтор `unit`, обрезка по границе символа и добивка ASCII.
fn exact(unit: &str, len: usize) -> String {
    let mut text = unit.repeat(len / unit.len() + 1);
    let mut end = len;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(&"x".repeat(len - text.len()));
    assert_eq!(text.len(), len);
    text
}

/// Ровно `len` байт, начиная с ASCII-байта: границы символов лежат на нечётных смещениях,
/// и кратная степени двойки граница дельты приходится внутрь символа.
fn offset_by_one(unit: char, len: usize) -> String {
    let mut text = String::from("x");
    while text.len() + unit.len_utf8() <= len {
        text.push(unit);
    }
    text.push_str(&"x".repeat(len - text.len()));
    assert_eq!(text.len(), len);
    text
}

fn cases() -> Vec<(String, String)> {
    let delta = MAX_BLOCK_DELTA_BYTES;
    let inline = BLOCK_FINAL_INLINE_MAX_BYTES;
    let mixed = "вывод команды: всё в порядке, код 0\n";
    let mut cases = Vec::new();
    for (label, len) in [
        ("ascii, delta limit - 1", delta - 1),
        ("ascii, delta limit", delta),
        ("ascii, delta limit + 1", delta + 1),
        ("ascii, inline limit - 1", inline - 1),
        ("ascii, inline limit", inline),
        ("ascii, inline limit + 1", inline + 1),
        ("ascii, event batch limit + 1", MAX_EVENT_BATCH_BYTES + 1),
    ] {
        cases.push((label.to_owned(), exact("a", len)));
    }
    cases.push((
        "cyrillic, inline limit - 1".into(),
        offset_by_one('я', inline - 1),
    ));
    cases.push((
        "cyrillic, inline limit + 1".into(),
        offset_by_one('я', inline + 1),
    ));
    cases.push((
        "cyrillic, delta boundary inside a character".into(),
        offset_by_one('я', 5 * delta),
    ));
    cases.push((
        "four-byte characters, delta boundary inside a character".into(),
        {
            let mut text = String::from("ab");
            while text.len() + 4 <= 5 * delta {
                text.push('😀');
            }
            text
        },
    ));
    cases.push(("200 KiB, mixed cyrillic".into(), exact(mixed, 200 * 1024)));
    // Больше пакета записей журнала: дельт больше `MAX_SJ_BATCH_RECORDS`.
    let records = MAX_SJ_BATCH_RECORDS + 64;
    cases.push((
        format!("{records} deltas of cyrillic"),
        exact(mixed, records * delta),
    ));
    cases
}

fn user_frame(index: usize, text: &str) -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": [{
            "type": "tool_result",
            "tool_use_id": format!("toolu_large_{index}"),
            "content": text,
            "is_error": false,
        }]},
        "parent_tool_use_id": null,
        "session_id": "00000000-0000-4000-8000-000000000000",
        "uuid": Uuid::now_v7().to_string(),
    })
}

fn worker_toml(root: &Path, work: &Path, port: u16, ids: [Uuid; 3], fake: &Path) -> String {
    let [machine, installation, restore] = ids;
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

[claude_code]
executable = "{fake}"
allow_paid_model_poll = false
"#,
        root = root.display(),
        work = work.display(),
        exe = env!("CARGO_BIN_EXE_canterlot-worker"),
        fake = fake.display(),
    )
}

/// Всё, что владелец получает из хаба по блоку: страницы чтения с сервера и сводка истории.
async fn owner_view(
    pool: &PgPool,
    owner: Uuid,
    conversation: Uuid,
    block: Uuid,
) -> (String, String, Option<String>) {
    let mut conn = pool.acquire().await.unwrap();
    let mut text = String::new();
    let mut offset = 0;
    let (state, checksum) = loop {
        let page =
            history::block_content(&mut conn, owner, conversation, block, offset, PAGE_BYTES)
                .await
                .unwrap()
                .expect("block of the owner");
        text.push_str(&page.content);
        match page.next_offset {
            Some(next) => offset = next,
            None => break (page.state, page.checksum),
        }
    };
    (text, state, checksum)
}

fn sha_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn large_tool_result_without_deltas_reaches_the_owner(pool: PgPool) {
    let _gate = e2e_gate::acquire().await;
    let timers = HubTimers {
        dispatch_sweep: Duration::from_millis(20),
        ..Default::default()
    };
    let hub = spawn_test_hub(pool.clone(), timers).await;
    let root = tempfile::tempdir().unwrap();
    let guard = ProcessGuard::new(root.path());
    let machine = machines::add(&pool, hub.installation.owner_id, "claude")
        .await
        .unwrap();
    let token_file = root.path().join("token");
    fs::write(&token_file, machine.token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(root.path().join("ca.pem"), &hub.pki.ca_pem).unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();

    let cases = cases();
    let tools = root.path().join("tools");
    fs::create_dir(&tools).unwrap();
    let fake = tools.join("fake-cli");
    fake_exec::write_executable(&fake, FAKE, 0o700);
    for (index, (_, text)) in cases.iter().enumerate() {
        let turn = json!([
            user_frame(index, text),
            {"type": "result", "subtype": "success", "is_error": false,
             "user_message_uuids": ["@input"]},
        ]);
        fs::write(
            tools.join(format!("turn{}.json", index + 1)),
            serde_json::to_vec(&turn).unwrap(),
        )
        .unwrap();
    }

    let config: WorkerConfig = toml::from_str(&worker_toml(
        root.path(),
        &work,
        hub.addr.port(),
        [
            machine.machine_id,
            hub.installation.installation_id,
            hub.installation.restore_generation,
        ],
        &fake,
    ))
    .unwrap();
    let owner = hub.installation.owner_id;
    let conversation = testkit::seed_conversation(&pool, owner, machine.machine_id).await;
    sqlx::query(
        "update conversations set workdir = $2, harness = 'claude_code', model = 'haiku',
         effort = 'low', permission_mode = 'bypass' where id = $1",
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
            "select count(*) from machine_harnesses where machine_id = $1
             and harness = 'claude_code'",
        )
        .bind(machine_id)
        .fetch_one(&pool)
        .await
        .unwrap()
            == 1
    })
    .await;

    let started = executions::start(&pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    let execution = started.execution_id;
    until("execution running", async || {
        sqlx::query_scalar::<_, String>("select state from executions where id = $1")
            .bind(execution)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "running"
    })
    .await;

    for (index, (label, _)) in cases.iter().enumerate() {
        submit::submit_input(
            &pool,
            SubmitInput {
                owner_id: owner,
                conversation_id: conversation,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: format!("ход {}", index + 1),
                }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        hub.dispatcher.trigger(conversation);
        let finished = timeout(Duration::from_secs(30), async {
            loop {
                let runs: i64 = sqlx::query_scalar(
                    "select count(*) from events where execution_id = $1 and kind = 'run_finished'",
                )
                .bind(execution)
                .fetch_one(&pool)
                .await
                .unwrap();
                if runs == index as i64 + 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if finished.is_err() {
            let kinds: Vec<String> = sqlx::query_scalar(
                "select kind from events where execution_id = $1 order by scope_seq",
            )
            .bind(execution)
            .fetch_all(&pool)
            .await
            .unwrap();
            panic!("no run_finished after {label}; events: {kinds:?}");
        }
    }

    let blocks_of_results: Vec<Uuid> = sqlx::query_scalar(
        "select block_id from blocks where execution_id = $1 and kind = 'tool_result'
         order by created_seq",
    )
    .bind(execution)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(blocks_of_results.len(), cases.len(), "one block per result");

    let mut problems = Vec::new();
    for ((label, expected), block) in cases.iter().zip(&blocks_of_results) {
        let (text, state, checksum) = owner_view(&pool, owner, conversation, *block).await;
        if state != "final" || text != *expected {
            problems.push(format!(
                "{label}: owner sees state={state}, {} of {} bytes{}",
                text.len(),
                expected.len(),
                if text.is_empty() {
                    " (empty block)"
                } else {
                    ""
                },
            ));
            continue;
        }
        if checksum.as_deref() != Some(sha_hex(expected).as_str()) {
            problems.push(format!(
                "{label}: checksum {checksum:?} does not match the text"
            ));
        }
        let diagnostics: Vec<String> = sqlx::query_scalar(
            "select kind from events where execution_id = $1 and payload->>'block_id' = $2
             and kind in ('block_checksum_mismatch', 'block_revision_gap', 'block_incomplete',
                          'block_conflicting_final')
             order by scope_seq",
        )
        .bind(execution)
        .bind(block.to_string())
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
        if !diagnostics.is_empty() {
            problems.push(format!("{label}: hub diagnostics {diagnostics:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "large tool results did not reach the owner whole:\n{}",
        problems.join("\n")
    );

    stop.send(true).unwrap();
    timeout(WAIT, join).await.unwrap().unwrap().unwrap();
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
    guard.cleanup();
}
