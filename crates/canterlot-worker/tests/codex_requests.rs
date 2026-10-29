//! Запросы сервера Codex (задача 4 плана 2b, шаг 1). Записанные кадры —
//! docs/stage0/evidence/codex/, сконструированные — tests/fixtures/codex/.

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::adapter::{
    AdapterEvent,
    codex::{
        requests::{Intake, Requests, Resolved, RouteCtx, check_decision, cli_policy_mark},
        rpc::RpcId,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn evidence_data(name: &str, line: usize) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/codex")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let wrapper: Value = serde_json::from_str(text.lines().nth(line - 1).unwrap()).unwrap();
    wrapper["data"].clone()
}

fn fixture_data(name: &str, line: usize) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let wrapper: Value = serde_json::from_str(text.lines().nth(line - 1).unwrap()).unwrap();
    wrapper["data"].clone()
}

fn rpc_id(data: &Value) -> RpcId {
    match &data["id"] {
        Value::Number(n) => RpcId::Int(n.as_i64().unwrap()),
        Value::String(s) => RpcId::Str(s.clone()),
        other => panic!("unexpected id {other}"),
    }
}

fn open(
    requests: &mut Requests,
    data: &Value,
    agent: &str,
    item: Option<&Value>,
) -> (String, String, Value) {
    match requests.on_server_request(
        rpc_id(data),
        data["method"].as_str().unwrap(),
        &data["params"],
        &RouteCtx { agent, item },
    ) {
        Intake::Open(AdapterEvent::RequestOpened {
            request_id,
            kind,
            payload,
        }) => (request_id, kind, payload),
        other => panic!("expected Open, got {other:?}"),
    }
}

fn allow() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    }
}

fn deny() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Deny,
        message: Some("no".into()),
    }
}

fn single_answer(id: &str, value: &str) -> DecisionPayload {
    let mut answers = BTreeMap::new();
    answers.insert(id.to_owned(), AnswerValue::Single(value.to_owned()));
    DecisionPayload::Question { answers }
}

#[test]
fn command_approval_opens_permission_with_exact_arguments() {
    let data = evidence_data("permissions", 27);
    let params = &data["params"];
    let mut requests = Requests::new();
    let (request_id, kind, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(kind, "permission");
    assert_eq!(request_id, "exec-f1ae0124-bac5-459a-b24b-449b4eefc113/0");
    assert_eq!(payload["tool_name"], "shell");
    assert_eq!(payload["command"], params["command"]);
    assert_eq!(payload["cwd"], params["cwd"]);
    assert_eq!(payload["reason"], params["reason"]);
    assert_eq!(payload["command_actions"], params["commandActions"]);
    assert_eq!(payload["available_decisions"], params["availableDecisions"]);
    assert_eq!(payload["tool_use_id"], params["itemId"]);
    assert_eq!(payload["agent"], "orchestrator");
    // Отказ без предложенного decline прервёт и ход: карточка предупреждает.
    assert_eq!(payload["deny_also_interrupts"], true);
    let input = &payload["input"];
    assert_eq!(input["command"], params["command"]);
    assert!(input.get("threadId").is_none());
    assert!(input.get("turnId").is_none());
    assert!(input.get("itemId").is_none());
}

#[test]
fn allow_answers_accept_and_resolved_is_the_proof() {
    let data = evidence_data("permissions", 27);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests.answer(&request_id, &allow()).unwrap();
    assert_eq!(line, evidence_data("permissions", 28));
    let text = serde_json::to_string(&line).unwrap();
    assert!(!text.contains("touch"), "proof carries no command text");
    assert_eq!(
        requests.on_resolved(&RpcId::Int(0)),
        Some(Resolved::Ours {
            request_id: request_id.clone()
        })
    );
    // Повторный ответ не применяется.
    assert_eq!(
        requests.answer(&request_id, &allow()),
        Err("request_not_pending")
    );
}

#[test]
fn deny_uses_decline_when_offered_else_cancel() {
    // В списке нет decline: отказ отменяет и ход.
    let data = evidence_data("permissions", 27);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests.answer(&request_id, &deny()).unwrap();
    assert_eq!(line["result"], json!({"decision": "cancel"}));
    // У правок списка нет: отказ — decline, ход продолжается.
    let change = evidence_data("permissions", 36);
    let started = evidence_data("permissions", 34);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(
        &mut requests,
        &change,
        "orchestrator",
        Some(&started["params"]["item"]),
    );
    let line = requests.answer(&request_id, &deny()).unwrap();
    assert_eq!(line["result"], json!({"decision": "decline"}));
    // Предложенный decline используется.
    let mut params = change["params"].clone();
    params["availableDecisions"] = json!(["accept", "decline", "cancel"]);
    let data =
        json!({"id": 2, "method": "item/commandExecution/requestApproval", "params": params});
    let mut requests = Requests::new();
    let (request_id, _, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(payload["deny_also_interrupts"], false);
    let line = requests.answer(&request_id, &deny()).unwrap();
    assert_eq!(line["result"], json!({"decision": "decline"}));
}

#[test]
fn file_change_payload_takes_changes_from_started_item() {
    let change = evidence_data("permissions", 36);
    let started = evidence_data("permissions", 34);
    let mut requests = Requests::new();
    let (request_id, kind, payload) = open(
        &mut requests,
        &change,
        "orchestrator",
        Some(&started["params"]["item"]),
    );
    assert_eq!(kind, "permission");
    assert_eq!(request_id, "exec-579f86a8-ac3e-4139-93c9-6a355bb3638e/1");
    assert_eq!(payload["tool_name"], "apply_patch");
    assert_eq!(payload["changes"], started["params"]["item"]["changes"]);
    assert_eq!(payload["changes"][0]["diff"], "hi\n");
    assert_eq!(payload["reason"], Value::Null);
    assert_eq!(payload["grant_root"], Value::Null);
    assert_eq!(payload["deny_also_interrupts"], false);
}

#[test]
fn question_payload_is_normalised_and_passes_valid_decision() {
    let data = evidence_data("question_default_mode", 28);
    let mut requests = Requests::new();
    let (_, kind, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(kind, "question");
    let question = &payload["questions"][0];
    assert_eq!(question["id"], "color_preference");
    assert_eq!(question["text"], "Which color do you prefer?");
    assert_eq!(question["header"], "Color");
    assert_eq!(
        question["options"],
        json!([
            {"label": "Red", "description": "Choose red."},
            {"label": "Blue", "description": "Choose blue."},
        ])
    );
    assert_eq!(question["multi_select"], false);
    assert_eq!(question["allow_custom"], true);
    assert_eq!(question["is_secret"], false);
    assert_eq!(
        payload["tool_use_id"],
        "call_108782f23f0041fea7465f4b4994a39a"
    );
    assert!(check_decision(
        "question",
        &payload,
        &single_answer("color_preference", "Blue")
    ));
    assert!(!check_decision(
        "question",
        &payload,
        &single_answer("no-such-question", "Blue")
    ));
    let data = evidence_data("question", 99);
    let mut requests = Requests::new();
    let (_, _, payload) = open(&mut requests, &data, "helper", None);
    assert_eq!(payload["questions"][0]["id"], "color");
    assert_eq!(payload["agent"], "helper");
    assert!(check_decision(
        "question",
        &payload,
        &single_answer("color", "Red")
    ));
}

#[test]
fn answer_bytes_match_recorded_response() {
    let data = evidence_data("question_default_mode", 28);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests
        .answer(&request_id, &single_answer("color_preference", "Blue"))
        .unwrap();
    assert_eq!(line, evidence_data("question_default_mode", 29));
}

#[test]
fn free_text_answer_when_other_is_allowed() {
    let data = fixture_data("question_free_text", 1);
    let mut requests = Requests::new();
    let (request_id, _, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(payload["questions"][0]["allow_custom"], true);
    assert_eq!(payload["questions"][0]["options"], Value::Null);
    let decision = single_answer("free", "anything at all");
    assert!(check_decision("question", &payload, &decision));
    let line = requests.answer(&request_id, &decision).unwrap();
    assert_eq!(
        line["result"],
        json!({"answers": {"free": {"answers": ["anything at all"]}}})
    );
    // Повторные и пустые id уходят в q<index>, ответы возвращаются по карте.
    let data = json!({
        "id": 11,
        "method": "item/tool/requestUserInput",
        "params": {
            "itemId": "call-dup",
            "threadId": "thread-main",
            "turnId": "turn-main",
            "questions": [
                {"id": "", "question": "A?", "options": null},
                {"id": "", "question": "B?", "options": null},
            ],
        },
    });
    let mut requests = Requests::new();
    let (request_id, _, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(payload["questions"][0]["id"], "q0");
    assert_eq!(payload["questions"][1]["id"], "q1");
    let mut answers = BTreeMap::new();
    answers.insert("q0".into(), AnswerValue::Single("a".into()));
    answers.insert("q1".into(), AnswerValue::Single("b".into()));
    let line = requests
        .answer(&request_id, &DecisionPayload::Question { answers })
        .unwrap();
    assert_eq!(line["result"]["answers"][""], json!({"answers": ["b"]}));
}

#[test]
fn resolved_without_our_answer_closes_and_late_decision_is_not_applied() {
    let data = evidence_data("interrupt", 70);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    assert!(requests.is_pending(&request_id));
    // Сервер снял запрос сам: задача 7 закроет карточку как cli_cancelled.
    assert_eq!(
        requests.on_resolved(&RpcId::Int(0)),
        Some(Resolved::Cleared {
            request_id: request_id.clone()
        })
    );
    assert!(!requests.is_pending(&request_id));
    assert_eq!(
        requests.answer(&request_id, &allow()),
        Err("request_not_pending")
    );
}

#[test]
fn decision_after_turn_end_is_undetermined() {
    let data = evidence_data("permissions", 27);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    requests.answer(&request_id, &allow()).unwrap();
    // Конец хода: хозяин уже закрыл запросы, серверу — ничего.
    requests.drop_all();
    assert_eq!(requests.on_resolved(&RpcId::Int(0)), None);
    assert_eq!(
        requests.answer(&request_id, &deny()),
        Err("request_not_pending")
    );
}

#[tokio::test]
async fn pending_request_has_no_timeout() {
    let data = evidence_data("permissions", 27);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(requests.is_pending(&request_id));
    let line = requests.answer(&request_id, &allow()).unwrap();
    assert_eq!(line["result"], json!({"decision": "accept"}));
}

#[test]
fn permissions_and_elicitation_requests_open_permission_cards() {
    let data = fixture_data("permissions_request", 1);
    let mut requests = Requests::new();
    let (request_id, kind, payload) = open(&mut requests, &data, "orchestrator", None);
    assert_eq!(kind, "permission");
    assert_eq!(request_id, "perm-1/6");
    assert_eq!(payload["tool_name"], "permissions");
    assert_eq!(payload["permissions"], json!({"sandbox": "read-only"}));
    assert_eq!(payload["reason"], "Need network?");
    assert_eq!(payload["cwd"], "/tmp/work");
    let line = requests.answer(&request_id, &allow()).unwrap();
    assert_eq!(
        line["result"],
        json!({"permissions": {"sandbox": "read-only"}, "scope": "turn"})
    );
    let data = fixture_data("permissions_request", 1);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests.answer(&request_id, &deny()).unwrap();
    assert_eq!(line["result"], json!({"permissions": {}, "scope": "turn"}));
    // Elicitation: form несёт пустой content, url — null.
    for (line_no, content) in [(1, json!({})), (2, Value::Null)] {
        let data = fixture_data("elicitation", line_no);
        let mut requests = Requests::new();
        let (request_id, kind, payload) = open(&mut requests, &data, "orchestrator", None);
        assert_eq!(kind, "permission");
        assert_eq!(payload["tool_name"], "mcp_elicitation");
        assert_eq!(payload["server"], "s");
        let line = requests.answer(&request_id, &allow()).unwrap();
        assert_eq!(
            line["result"],
            json!({"action": "accept", "content": content, "_meta": null})
        );
    }
    let data = fixture_data("elicitation", 1);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests.answer(&request_id, &deny()).unwrap();
    assert_eq!(
        line["result"],
        json!({"action": "decline", "content": null, "_meta": null})
    );
}

#[test]
fn unsupported_server_requests_get_an_error_reply() {
    let mut requests = Requests::new();
    for line_no in [1, 2] {
        let data = fixture_data("unknown_server_request", line_no);
        match requests.on_server_request(
            rpc_id(&data),
            data["method"].as_str().unwrap(),
            &data["params"],
            &RouteCtx {
                agent: "orchestrator",
                item: None,
            },
        ) {
            Intake::Reply(line) => {
                assert_eq!(line["error"]["code"], -32601);
                assert_eq!(line["id"], data["id"]);
            }
            other => panic!("expected error reply, got {other:?}"),
        }
    }
    let diagnostics = requests.drain_diagnostics();
    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics.iter().all(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "unsupported_server_request",
            required: false,
            ..
        }
    )));
    // Повтор того же метода новой диагностики не даёт.
    let data = fixture_data("unknown_server_request", 1);
    match requests.on_server_request(
        RpcId::Int(99),
        data["method"].as_str().unwrap(),
        &data["params"],
        &RouteCtx {
            agent: "orchestrator",
            item: None,
        },
    ) {
        Intake::Reply(_) => {}
        other => panic!("expected error reply, got {other:?}"),
    }
    assert!(requests.drain_diagnostics().is_empty());
}

#[test]
fn auto_run_items_are_marked_cli_policy_except_under_never() {
    // Два вызова без запросов под on-request + readOnly — с меткой.
    assert_eq!(
        cli_policy_mark(
            &json!("on-request"),
            &json!({"networkAccess": false, "type": "readOnly"})
        ),
        Some("cli_policy")
    );
    assert_eq!(
        cli_policy_mark(&json!("on-request"), &json!("read-only")),
        Some("cli_policy")
    );
    // Та же политика, но dangerFullAccess — без метки (там всё разрешено).
    assert_eq!(
        cli_policy_mark(&json!("on-request"), &json!({"type": "dangerFullAccess"})),
        None
    );
    assert_eq!(
        cli_policy_mark(&json!("on-request"), &json!("danger-full-access")),
        None
    );
    // Effective.approval_policy = "never" — без метки.
    assert_eq!(
        cli_policy_mark(&json!("never"), &json!({"type": "readOnly"})),
        None
    );
    // Запрошенный пункт (ev/permissions.jsonl:31) вверх по потоку исключается:
    // переводчик ставит approved_by_cli=false, метка не вызывается.
    let data = evidence_data("permissions", 31);
    assert_eq!(data["method"], "item/completed");
}

#[test]
fn payload_and_answers_never_appear_in_debug() {
    let data = evidence_data("permissions", 27);
    let mut requests = Requests::new();
    let (_, _, _) = open(&mut requests, &data, "orchestrator", None);
    let debug = format!("{requests:?}");
    assert!(!debug.contains("touch"), "{debug}");
    assert!(!debug.contains("May I run"), "{debug}");
    let data = evidence_data("question_default_mode", 28);
    let mut requests = Requests::new();
    let (request_id, _, _) = open(&mut requests, &data, "orchestrator", None);
    let line = requests
        .answer(&request_id, &single_answer("color_preference", "Blue"))
        .unwrap();
    let intake = Intake::Reply(line);
    let debug = format!("{intake:?}");
    assert!(!debug.contains("Blue"), "{debug}");
    let debug = format!(
        "{:?}",
        Resolved::Ours {
            request_id: "exec-1/0".into()
        }
    );
    assert!(debug.contains("exec-1/0"));
}
