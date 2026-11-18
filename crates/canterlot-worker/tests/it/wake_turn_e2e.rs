#![forbid(unsafe_code)]

//! Ход, который Claude Code начал сам по уведомлению фоновой задачи, через настоящие хаб,
//! воркер и сопровождение. Кадры хода записаны с живого CLI 2.1.326 и проигрываются двойником.

use crate::fake_exec;
#[path = "support/pilot.rs"]
mod pilot;
#[path = "support/process_guard.rs"]
mod process_guard;

use std::{fs, path::PathBuf, time::Duration};

use canterlot_hub::config::ActivityConfig;
use canterlot_worker::config::ClaudeCodeConfig;
use pilot::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const QUIET: Duration = Duration::from_millis(300);

/// Кадры записи без начальных `system/init`: двойник присылает его сам перед каждым вводом.
fn recorded() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/fixtures/claude_code/background_wake_2_1_326.jsonl");
    let mut frames: Vec<Value> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["dir"] == "out")
        .map(|row| row["data"].clone())
        .collect();
    assert_eq!(frames[0]["subtype"], "init");
    frames.remove(0);
    // Запись сделана без хаба: ввод хаба CLI называет в `user_message_uuids`, как в проде.
    let mut results = frames.iter_mut().filter(|frame| frame["type"] == "result");
    results.next().unwrap()["user_message_uuids"] = json!(["@input"]);
    results.next().unwrap()["user_message_uuids"] = Value::Null;
    // Запись шла с пустым MCP, а воркер требует своего сервера в каждом `init`.
    for frame in frames.iter_mut().filter(|frame| frame["subtype"] == "init") {
        frame["mcp_servers"] = json!([{"name": "canterlot", "status": "connected"}]);
    }
    frames
}

/// Первый ход записи и пробуждающий ход после паузы: так CLI и идёт.
fn first_turn_then_wake(pause_ms: u64) -> Vec<Value> {
    let mut frames = recorded();
    let wake = frames
        .iter()
        .position(|frame| frame["subtype"] == "task_updated")
        .unwrap();
    frames.insert(wake, json!(format!("@sleep:{pause_ms}")));
    frames
}

fn options() -> PilotOptions {
    PilotOptions {
        activity: ActivityConfig {
            debounce: ms(50),
            tick: ms(100),
            idle_quiet: QUIET,
            ..Default::default()
        },
        ..Default::default()
    }
}

struct Bench {
    p: Pilot,
    _cli: tempfile::TempDir,
}

impl Bench {
    async fn new(pool: &PgPool, turns: Vec<Vec<Value>>) -> Self {
        let cli = tempfile::tempdir().unwrap();
        let executable = cli.path().join("fake-cli");
        fake_exec::write_executable(&executable, include_str!("support/fake_claude.py"), 0o700);
        for (n, turn) in turns.iter().enumerate() {
            fs::write(
                cli.path().join(format!("turn{}.json", n + 1)),
                serde_json::to_vec(turn).unwrap(),
            )
            .unwrap();
        }
        let mut options = options();
        options.claude_code = Some(ClaudeCodeConfig {
            executable,
            ..Default::default()
        });
        Self {
            p: Pilot::new(pool, options).await,
            _cli: cli,
        }
    }

    async fn session(&self) -> Uuid {
        let created = self
            .p
            .post(
                "/api/v1/conversations",
                Some(json!({
                    "machine_id": self.p.machine,
                    "harness": "claude_code",
                    "workdir": self.p.work.to_str().unwrap(),
                    "model": "haiku",
                    "effort": "low",
                    "permission_mode": "owner",
                    "start": true,
                })),
                201,
            )
            .await;
        let id = uuid(&created["conversation_id"]);
        until("execution running", async || {
            self.p.detail(id).await["execution"]["state"] == "running"
        })
        .await;
        id
    }

    async fn activity(&self, id: Uuid) -> (String, String) {
        let detail = self.p.detail(id).await;
        (
            detail["activity"]["state"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            detail["activity"]["evidence"]["phase"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
        )
    }

    /// Расход хода по записи хаба: пробуждающий ход дописывает свой к последней команде.
    async fn spent(&self, id: Uuid) -> i64 {
        sqlx::query_scalar(
            "select coalesce(sum(u.output_tokens),0)::bigint from run_model_usage u
             join commands c on c.id=u.input_command_id where c.conversation_id=$1",
        )
        .bind(id)
        .fetch_one(&self.p.pool)
        .await
        .unwrap()
    }

    /// Ход, чей итог унёс расход до `output`, кончился: сессия простаивает, ход не держит
    /// очередь, признак пробуждающего хода снят.
    async fn settles_idle(&self, id: Uuid, output: i64, what: &str) {
        until(what, async || self.spent(id).await >= output).await;
        until(what, async || self.activity(id).await.0 == "idle").await;
        let (state, phase) = self.activity(id).await;
        assert_eq!(state, "idle", "{what}: {state}/{phase}");
        let (slot, wake): (String, bool) = sqlx::query_as(
            "select q.run_slot, e.wake_turn from queue_state q
             join executions e on e.conversation_id=q.conversation_id where q.conversation_id=$1",
        )
        .bind(id)
        .fetch_one(&self.p.pool)
        .await
        .unwrap();
        assert_eq!(slot, "free", "{what}");
        assert!(!wake, "{what}: the wake flag is cleared");
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn wake_turn_of_cli_2_1_326_leaves_the_session_idle(pool: PgPool) {
    let b = Bench::new(&pool, vec![first_turn_then_wake(600)]).await;
    let id = b.session().await;
    let submitted = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&submitted["input_id"])).await;
    b.settles_idle(id, 170, "after the wake turn").await;
}

const MODEL: &str = "claude-haiku-5-5";

fn init() -> Value {
    json!({"type":"system","subtype":"init","session_id":"s","model":MODEL,"permissionMode":"default",
        "mcp_servers":[{"name":"canterlot","status":"connected"}]})
}

fn message_start(id: &str) -> Value {
    json!({"type":"stream_event","parent_tool_use_id":null,
        "event":{"type":"message_start","message":{"id":id,"model":MODEL,"usage":{}}}})
}

fn answer(id: &str, text: &str) -> Value {
    json!({"type":"assistant","parent_tool_use_id":null,"message":{"id":id,"model":MODEL,"type":"message",
        "role":"assistant","content":[{"type":"text","text":text}]}})
}

/// `output` — расход модели нарастающим итогом, как его считает CLI.
fn result(uuids: Value, origin: Option<&str>, output: u64) -> Value {
    let mut frame = json!({"type":"result","subtype":"success","is_error":false,"result":"ok",
        "user_message_uuids":uuids,"modelUsage":{MODEL:{"inputTokens":1,"outputTokens":output}}});
    if let Some(kind) = origin {
        frame["origin"] = json!({"kind":kind,"producer":"session-task","runId":"r1"});
    }
    frame
}

fn task_started() -> Value {
    json!({"type":"system","subtype":"background_tasks_changed","tasks":[
        {"task_id":"b1","run_id":"r1","task_type":"local_bash","description":"sleep"}]})
}

fn task_done() -> Vec<Value> {
    vec![
        json!({"type":"system","subtype":"task_notification","task_id":"b1","run_id":"r1","status":"completed","summary":"done"}),
        json!({"type":"system","subtype":"background_tasks_changed","tasks":[]}),
    ]
}

/// Ход владельца, фоновая задача и `n` пробуждающих ходов подряд.
fn streamed(wakes: usize, pause_ms: u64) -> Vec<Value> {
    let mut frames = vec![
        message_start("m1"),
        task_started(),
        answer("m1", "started"),
        result(json!(["@input"]), None, 10),
    ];
    frames.extend(task_done());
    for n in 0..wakes {
        frames.push(init());
        frames.push(message_start(&format!("w{n}")));
        frames.push(json!(format!("@sleep:{pause_ms}")));
        frames.push(answer(&format!("w{n}"), "finished"));
        frames.push(result(
            Value::Null,
            Some("task-notification"),
            10 + 5 * (n as u64 + 1),
        ));
    }
    frames
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn streamed_wake_turn_leaves_the_session_idle(pool: PgPool) {
    let b = Bench::new(&pool, vec![streamed(1, 400)]).await;
    let id = b.session().await;
    let submitted = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&submitted["input_id"])).await;
    b.settles_idle(id, 15, "after the streamed wake turn").await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn several_wake_turns_in_a_row_leave_the_session_idle(pool: PgPool) {
    let b = Bench::new(&pool, vec![streamed(3, 300)]).await;
    let id = b.session().await;
    let submitted = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&submitted["input_id"])).await;
    b.settles_idle(id, 25, "after three wake turns").await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn input_queued_during_a_wake_turn_is_delivered_after_it(pool: PgPool) {
    let second = vec![
        init(),
        message_start("m2"),
        answer("m2", "second"),
        result(json!(["@input"]), None, 20),
    ];
    let b = Bench::new(&pool, vec![streamed(1, 1500), second]).await;
    let id = b.session().await;
    let first = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&first["input_id"])).await;
    until("the wake turn is running", async || {
        b.activity(id).await.0 == "working"
    })
    .await;
    let queued = b.p.submit(id, "Второе сообщение", &[]).await;
    b.p.accepted(uuid(&queued["input_id"])).await;
    b.settles_idle(id, 20, "after the queued input").await;
}

/// Ход владельца кончается, задача завершается; сообщение, записанное следом, CLI берёт в ход
/// по уведомлению: у его `result` остаётся `origin` уведомления, а сообщение названо в
/// `user_message_uuids`.
fn notified() -> Vec<Value> {
    let mut frames = vec![
        message_start("m1"),
        task_started(),
        answer("m1", "started"),
        result(json!(["@input"]), None, 10),
        json!("@sleep:300"),
    ];
    frames.extend(task_done());
    frames
}

fn merged() -> Vec<Value> {
    vec![
        init(),
        message_start("w1"),
        answer("w1", "finished"),
        result(json!(["@input"]), Some("task-notification"), 15),
    ]
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn message_taken_into_a_wake_turn_ends_with_it(pool: PgPool) {
    let b = Bench::new(&pool, vec![notified(), merged()]).await;
    let id = b.session().await;
    let first = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&first["input_id"])).await;
    let second = b.p.submit(id, "Отчёт делегата", &[]).await;
    let second = uuid(&second["input_id"]);
    b.p.accepted(second).await;
    b.settles_idle(id, 15, "after the merged turn").await;
}

/// Следом за поглощённым сообщением уходит ещё одно: очередь не заперта.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn queue_keeps_moving_after_a_merged_wake_turn(pool: PgPool) {
    let third = vec![
        init(),
        message_start("m3"),
        answer("m3", "third"),
        result(json!(["@input"]), None, 30),
    ];
    let b = Bench::new(&pool, vec![notified(), merged(), third]).await;
    let id = b.session().await;
    let first = b.p.submit(id, "Запусти фоновую задачу", &[]).await;
    b.p.accepted(uuid(&first["input_id"])).await;
    let second = b.p.submit(id, "Отчёт делегата", &[]).await;
    let third = b.p.submit(id, "Третье", &[]).await;
    let third = uuid(&third["input_id"]);
    b.p.accepted(uuid(&second["input_id"])).await;
    b.p.accepted(third).await;
    b.settles_idle(id, 30, "after the queue drains").await;
}
