#![forbid(unsafe_code)]

//! Сквозной путь Antigravity через настоящие хаб, воркер и сопровождение: `fake-agy`
//! отдаёт кадры ACP, а решения, steer и прерывание идут через HTTP хаба.

use crate::antigravity;
#[path = "support/pilot.rs"]
mod pilot;
#[path = "support/process_guard.rs"]
mod process_guard;

use antigravity::Fake;
use canterlot_hub::testkit::Method;
use canterlot_worker::{
    adapter::antigravity::replay::{Script, Step},
    config::AntigravityConfig,
};
use pilot::{Pilot, PilotOptions, WAIT, ms, until, uuid};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::timeout;

const SESSION: &str = "d41d6945-1310-44ef-89e6-42705ee12e27";

fn init_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[0].clone()
}

fn new_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[2].clone()
}

fn prompt(recorded: i64) -> Step {
    Step::Expect {
        method: "session/prompt".into(),
        params: None,
        result: None,
        error: None,
        delay_ms: 0,
        recorded_id: Some(json!(recorded)),
    }
}

fn respond(recorded: i64, result: Value) -> Step {
    Step::Respond {
        recorded_id: json!(recorded),
        result: Some(result),
        error: None,
    }
}

fn end_turn(usage: Option<Value>) -> Value {
    let mut result = json!({"stopReason": "end_turn"});
    if let Some(usage) = usage {
        result["usage"] = usage;
    }
    result
}

fn update(update: Value) -> Step {
    Script::send(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": SESSION, "update": update},
    }))
}

fn chunk(kind: &str, text: &str) -> Step {
    update(json!({
        "sessionUpdate": kind,
        "content": {"type": "text", "text": text},
    }))
}

fn tool_call(status: &str) -> Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "call-e2e",
        "name": "run_command",
        "status": status,
        "kind": "execute",
        "title": "echo e2e",
        "rawInput": {"CommandLine": "echo e2e"},
        "rawOutput": {"output": "e2e\n", "exit_code": 0},
    })
}

fn main_script() -> Script {
    let started = update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-e2e",
        "name": "run_command",
        "status": "in_progress",
        "kind": "execute",
        "title": "echo e2e",
        "rawInput": {"CommandLine": "echo e2e"},
    }));
    let pending = update(tool_call("pending"));
    let completed = update(tool_call("completed"));
    let request = Script::send(json!({
        "jsonrpc": "2.0",
        "method": "session/request_permission",
        "id": "perm-e2e",
        "params": {
            "sessionId": SESSION,
            "toolCall": {
                "sessionUpdate": "tool_call",
                "toolCallId": "call-e2e",
                "name": "run_command",
                "status": "pending",
                "kind": "execute",
                "title": "echo e2e",
                "rawInput": {"CommandLine": "echo e2e"},
            },
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
            ]
        }
    }));
    Script {
        steps: vec![
            init_step(),
            new_step(),
            Script::expect("session/set_model", json!({})),
            Script::expect("session/set_config_option", json!({})),
            // Обычный текст.
            prompt(4),
            chunk("user_message_chunk", "первый ход"),
            chunk("agent_message_chunk", "готово"),
            respond(
                4,
                end_turn(Some(json!({
                    "inputTokens": 100,
                    "outputTokens": 7,
                    "cachedReadTokens": 0,
                    "cachedWriteTokens": 0,
                    "thoughtTokens": 2,
                    "totalTokens": 109
                }))),
            ),
            // Разрешение: запрос открывается, ответ приходит из API хаба.
            prompt(5),
            chunk("user_message_chunk", "нужен инструмент"),
            started,
            pending,
            request,
            Script::reply(json!("perm-e2e")),
            completed,
            chunk("agent_message_chunk", "после разрешения"),
            respond(5, end_turn(None)),
            // Steer: второй prompt вливается в ход, ответы приходят в обратном порядке.
            prompt(6),
            chunk("user_message_chunk", "долгий ход"),
            prompt(7),
            chunk("user_message_chunk", "влить сейчас"),
            respond(
                7,
                end_turn(Some(json!({
                    "inputTokens": 120,
                    "outputTokens": 9,
                    "totalTokens": 129
                }))),
            ),
            respond(6, end_turn(None)),
            // Прерывание: cancel и ответ cancelled.
            prompt(8),
            chunk("user_message_chunk", "останови"),
            Script::notification("session/cancel"),
            respond(8, json!({"stopReason": "cancelled"})),
        ],
        ..Default::default()
    }
}

fn pilot_options(fake: &Fake) -> PilotOptions {
    PilotOptions {
        antigravity: Some(AntigravityConfig {
            enabled: true,
            binary: Some(fake.exe.clone()),
            data_dir: None,
        }),
        ..Default::default()
    }
}

async fn run_finished(pool: &PgPool, command: uuid::Uuid) -> bool {
    sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from runs where input_command_id=$1 and finished_at is not null)",
    )
    .bind(command)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn antigravity_text_permission_steer_and_interrupt_through_hub_and_worker(pool: PgPool) {
    let fake = Fake::new(main_script());
    let p = Pilot::new(&pool, pilot_options(&fake)).await;
    let id = p
        .create_with(
            "antigravity",
            "gemini-3.8-flash-high",
            "high",
            Some("ask"),
            false,
        )
        .await;

    // Текст доходит до ленты.
    let first = p.submit(id, "первый ход", &[]).await;
    let first_command = uuid(&first["command_id"]);
    p.accepted(uuid(&first["input_id"])).await;
    p.answered(id, first_command, "готово").await;
    until("first run finished", async || {
        run_finished(&pool, first_command).await
    })
    .await;

    // Разрешение открывается и применяется по ответу из API хаба.
    let second = p.submit(id, "нужен инструмент", &[]).await;
    let second_command = uuid(&second["command_id"]);
    let mut request = Value::Null;
    until("permission request opened", async || {
        let snapshot = p.get(&format!("/api/v1/conversations/{id}/snapshot")).await;
        match snapshot["data"]["open_requests"]
            .as_array()
            .and_then(|requests| requests.first())
        {
            Some(open) => {
                request = open.clone();
                true
            }
            None => false,
        }
    })
    .await;
    assert_eq!(request["kind"], "permission");
    let request_id = uuid(&request["id"]);
    let decided = p
        .post(
            &format!("/api/v1/conversations/{id}/requests/{request_id}/decision"),
            Some(json!({"kind": "permission", "behavior": "allow", "message": null})),
            200,
        )
        .await;
    assert_eq!(decided["state"], "decision_saved");
    p.answered(id, second_command, "после разрешения").await;
    until("permission decision applied", async || {
        sqlx::query_scalar::<_, String>("select state from requests where id=$1")
            .bind(request_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "decision_applied"
    })
    .await;

    // Steer: второй ввод вливается в идущий ход через API.
    let third = p.submit(id, "долгий ход", &[]).await;
    let third_command = uuid(&third["command_id"]);
    p.accepted(uuid(&third["input_id"])).await;
    let steer = p.submit(id, "влить сейчас", &[]).await;
    let steer_input = uuid(&steer["input_id"]);
    let sent = p
        .call(
            Method::POST,
            &format!("/api/v1/conversations/{id}/inputs/{steer_input}/send-now"),
            None,
        )
        .await;
    assert_eq!(sent.status, 200, "{:?}", sent.body);
    until("steer accepted", async || {
        p.input_state(steer_input).await == "accepted"
    })
    .await;
    until("steered turn finished", async || {
        run_finished(&pool, third_command).await
    })
    .await;

    // Прерывание: ответ cancelled закрывает ход, процесс остаётся жив.
    let fourth = p.submit(id, "останови", &[]).await;
    let fourth_command = uuid(&fourth["command_id"]);
    p.accepted(uuid(&fourth["input_id"])).await;
    let stop = p
        .post(&format!("/api/v1/conversations/{id}/interrupt"), None, 200)
        .await;
    assert!(stop["requested"]["interrupt_command_id"].is_string());
    until("interrupted turn finished", async || {
        run_finished(&pool, fourth_command).await
    })
    .await;
    assert!(
        !fake.pids().is_empty(),
        "interrupt keeps the Antigravity process"
    );
    assert!(
        timeout(WAIT, async {
            loop {
                if p.detail(id).await["activity"]["state"] == "idle" {
                    break;
                }
                tokio::time::sleep(ms(20)).await;
            }
        })
        .await
        .is_ok(),
        "session returns to idle"
    );
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn antigravity_usage_limits_reach_the_hub(pool: PgPool) {
    let reset = "2026-11-04T18:55:00Z";
    let fake = Fake::new(Script {
        steps: vec![init_step(), new_step()],
        usage: Some(json!({
            "command": {"data": {"groups": [{
                "name": "Gemini Models",
                "buckets": [
                    {"window": "5h", "remaining_fraction": 0.75, "reset_time": reset},
                    {"window": "weekly", "remaining_fraction": 0.4, "reset_time": reset}
                ]
            }]}}
        })),
        ..Default::default()
    });
    let p = Pilot::new(&pool, pilot_options(&fake)).await;
    let machine = p.get(&format!("/api/v1/machines/{}", p.machine)).await;
    assert!(
        machine["harnesses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["id"] == "antigravity" && h["provider"] == "antigravity"),
        "{machine}"
    );
    p.post("/api/v1/limits/refresh", None, 202).await;
    until("antigravity limits in the hub", async || {
        p.get("/api/v1/limits")
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["provider"] == "antigravity" && l["five_hour"]["used_permille"] == 250)
    })
    .await;
    let limits = p.get("/api/v1/limits").await;
    let row = limits
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["provider"] == "antigravity")
        .unwrap();
    assert_eq!(row["weekly"]["used_permille"], 600);
    // `/usage` — отдельный процесс двойника, исполнение не запускалось.
    assert!(
        !fake
            .requests()
            .iter()
            .any(|entry| { entry["dir"] == "in" && entry["frame"]["method"] == "session/prompt" })
    );
}
