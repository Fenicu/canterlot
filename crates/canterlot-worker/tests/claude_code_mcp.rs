//! canterlot MCP server tests (plan Task 4): relay, broker, approvals,
//! questions, canterlot_limits. Recorded B10 frames are matched byte for byte.

#![forbid(unsafe_code)]

use canterlot_proto::{
    requests::{AnswerValue, DecisionPayload, PermissionBehavior},
    worker::v1::{LimitObservation, LimitWindow, LimitsSnapshot},
};
use canterlot_worker::adapter::claude_code::{
    broker::{Broker, BrokerEvent},
    mcp,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

fn worker_binary() -> PathBuf {
    let test = std::env::current_exe().expect("test binary path");
    test.parent()
        .and_then(|dir| dir.parent())
        .expect("target dir")
        .join("canterlot-worker")
}

async fn write_line(writer: &mut (impl AsyncWriteExt + Unpin), value: &Value) {
    let mut line = value.to_string();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.expect("write");
    writer.flush().await.expect("flush");
}

async fn read_json(reader: &mut BufReader<impl tokio::io::AsyncReadExt + Unpin>) -> Value {
    let mut line = String::new();
    let bytes = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("read timeout")
        .expect("read");
    assert!(bytes > 0, "unexpected EOF");
    serde_json::from_str(&line).expect("json line")
}

struct RelayHarness {
    broker: Broker,
    socket: BufReader<tokio::net::unix::OwnedReadHalf>,
    sink: tokio::net::unix::OwnedWriteHalf,
    _dir: tempfile::TempDir,
}

impl RelayHarness {
    async fn bind() -> Self {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let broker = Broker::bind(dir.path()).await.expect("bind");
        let stream = UnixStream::connect(dir.path().join("mcp.sock"))
            .await
            .expect("connect");
        let (read, sink) = stream.into_split();
        Self {
            broker,
            socket: BufReader::new(read),
            sink,
            _dir: dir,
        }
    }

    /// Вызов, как его шлёт CLI: `_meta` несёт тот же `tool_use_id`.
    async fn call(&mut self, id: Value, name: &str, arguments: Value) {
        let meta = json!({"claudecode/toolUseId": arguments["tool_use_id"], "progressToken": id});
        self.call_raw(id, name, arguments, meta).await;
    }

    async fn call_raw(&mut self, id: Value, name: &str, arguments: Value, meta: Value) {
        write_line(
            &mut self.sink,
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments,"_meta":meta}}),
        )
        .await;
    }

    async fn reply(&mut self) -> Value {
        read_json(&mut self.socket).await
    }
}

#[test]
fn initialize_echoes_protocol_and_lists_approve_and_limits_tools() {
    let msg = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}});
    let reply = mcp::handle_rpc(&msg);
    let result = reply.as_reply().expect("initialize must be answered");
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(result["result"]["serverInfo"]["name"], "canterlot");
    let instructions = result["result"]["instructions"]
        .as_str()
        .expect("instructions text");
    let lines: Vec<&str> = instructions.lines().collect();
    assert!(
        (10..=15).contains(&lines.len()),
        "instructions is 10-15 lines"
    );
    let list = mcp::handle_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    let tools = list.as_reply().expect("tools/list answered")["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    assert_eq!(tools.len(), 2);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert!(names.contains(&"approve"), "{names:?}");
    assert!(names.contains(&"canterlot_limits"), "{names:?}");
    let approve = tools.iter().find(|tool| tool["name"] == "approve").unwrap();
    assert_eq!(
        approve["inputSchema"],
        json!({"type": "object", "additionalProperties": true})
    );
    let probe = mcp::handle_rpc(
        &json!({"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover"}),
    );
    assert_eq!(probe.as_error().expect("discover rejected")["code"], -32601);
}

#[tokio::test]
async fn relay_passes_lines_both_ways_and_writes_nothing_else() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("mcp.sock");
    let listener = UnixListener::bind(&path).expect("listen");
    let mut child = tokio::process::Command::new(worker_binary())
        .args(["mcp-relay", "--socket"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn relay");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut stdout = BufReader::new(stdout);
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("accept")
        .expect("accept ok");
    let (read, mut write) = stream.into_split();
    let mut socket_lines = BufReader::new(read);
    let init = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25"}});
    write_line(&mut stdin, &init).await;
    assert_eq!(read_json(&mut socket_lines).await, init);
    let reply = json!({"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-11-25"}});
    write_line(&mut write, &reply).await;
    let echoed = read_json(&mut stdout).await;
    assert_eq!(echoed, reply);
    let mut rest = String::new();
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("relay exits on stdin EOF")
        .expect("wait");
    assert!(status.success(), "clean EOF exit");
    use tokio::io::AsyncReadExt as _;
    stdout.read_to_string(&mut rest).await.expect("drain");
    for line in rest.lines() {
        let value: Value = serde_json::from_str(line).expect("stdout is JSON-RPC only");
        assert_eq!(value["jsonrpc"], "2.0", "no logs on stdout: {line}");
    }
    let missing = dir.path().join("absent.sock");
    let status = tokio::process::Command::new(worker_binary())
        .args(["mcp-relay", "--socket"])
        .arg(&missing)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("spawn");
    assert!(!status.success(), "missing socket is a non-zero exit");
}

#[tokio::test]
async fn permission_call_opens_request_and_allow_returns_original_input() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(2),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "touch probe.txt", "description": "Create probe.txt file"}, "tool_use_id": "toolu_01GkVeqnQwNz9hnzGsJfE8yM"}),
        )
        .await;
    let opened = relay.broker.next().await;
    let (request_id, kind, payload) = match opened {
        BrokerEvent::Opened {
            request_id,
            kind,
            payload,
        } => (request_id, kind, payload),
        other => panic!("expected Opened, got {other:?}"),
    };
    assert_eq!(kind, "permission");
    assert_eq!(request_id, "toolu_01GkVeqnQwNz9hnzGsJfE8yM");
    assert_eq!(payload["tool_name"], "Bash");
    assert_eq!(
        payload["input"],
        json!({"command": "touch probe.txt", "description": "Create probe.txt file"})
    );
    assert_eq!(payload["tool_use_id"], "toolu_01GkVeqnQwNz9hnzGsJfE8yM");
    let outcome = relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
        .await;
    let proof = match outcome {
        canterlot_worker::adapter::DecisionOutcome::Applied(proof) => proof,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(proof.proof_kind, "mcp_tool_result");
    assert_eq!(proof.payload["request_id"], request_id);
    assert_eq!(proof.payload["behavior"], "allow");
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 2);
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert_eq!(
        text,
        "{\"behavior\":\"allow\",\"updatedInput\":{\"command\":\"touch probe.txt\",\"description\":\"Create probe.txt file\"}}",
        "byte-identical with the recorded allow reply"
    );
}

#[test]
fn approve_is_only_a_prompt_handler_not_invited_for_direct_use() {
    let init = mcp::handle_rpc(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}));
    let instructions = init.as_reply().unwrap()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !instructions.contains("Use the approve tool"),
        "{instructions}"
    );
    assert!(instructions.contains("Never call the approve tool yourself"));
    let list = mcp::handle_rpc(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    let tools = list.as_reply().unwrap()["result"]["tools"].clone();
    let approve = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "approve")
        .unwrap()
        .clone();
    assert!(
        approve["description"]
            .as_str()
            .unwrap()
            .contains("Not for direct use")
    );
}

#[test]
fn approve_without_matching_meta_tool_use_id_is_an_error() {
    let call = |arguments: Value, meta: Value| {
        mcp::handle_rpc(
            &json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"approve","arguments":arguments,"_meta":meta}}),
        )
    };
    // Как шлёт CLI (живой прогон 2.1.311): id в аргументах и в `_meta` совпадают.
    let prompt = call(
        json!({"tool_name":"Bash","input":{},"tool_use_id":"toolu_01SyDBAXoSw8THczHraCSUWG"}),
        json!({"claudecode/toolUseId":"toolu_01SyDBAXoSw8THczHraCSUWG","progressToken":6}),
    );
    assert!(matches!(prompt, mcp::McpAction::ToolCall { .. }));
    for (arguments, meta) in [
        // Прямой вызов моделью: `_meta` несёт id самого вызова approve.
        (
            json!({"tool_name":"Bash","input":{},"tool_use_id":"made_up"}),
            json!({"claudecode/toolUseId":"toolu_direct"}),
        ),
        (
            json!({"tool_name":"Bash","input":{}}),
            json!({"claudecode/toolUseId":"toolu_direct"}),
        ),
        (
            json!({"tool_name":"Bash","input":{},"tool_use_id":"toolu_x"}),
            json!({}),
        ),
        (
            json!({"tool_name":"Bash","input":{},"tool_use_id":""}),
            json!({"claudecode/toolUseId":""}),
        ),
    ] {
        let reply = call(arguments.clone(), meta);
        let error = reply.as_error().unwrap_or_else(|| panic!("{arguments}"));
        assert_eq!(error["code"], -32602);
        assert_eq!(reply.as_reply().unwrap()["id"], 7);
    }
}

#[tokio::test]
async fn direct_approve_call_gets_an_error_reply_and_opens_nothing() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call_raw(
            json!(9),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "rm -rf /"}}),
            json!({"claudecode/toolUseId": "toolu_direct"}),
        )
        .await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 9);
    assert_eq!(reply["error"]["code"], -32602);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), relay.broker.next())
            .await
            .is_err(),
        "no request reaches the owner"
    );
}

#[tokio::test]
async fn deny_returns_message() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(3),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "touch second.txt"}, "tool_use_id": "toolu_deny"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    let outcome = relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: Some("nope".into()),
            },
        )
        .await;
    assert!(
        matches!(
            outcome,
            canterlot_worker::adapter::DecisionOutcome::Applied(_)
        ),
        "deny is Applied, got {outcome:?}"
    );
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert_eq!(text, "{\"behavior\":\"deny\",\"message\":\"nope\"}");
    relay
        .call(
            json!(4),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_deny_default"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: None,
            },
        )
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert_eq!(
        text,
        "{\"behavior\":\"deny\",\"message\":\"Denied by the owner\"}"
    );
}

#[tokio::test]
async fn question_payload_is_normalised_and_passes_valid_decision() {
    let arguments = json!({"tool_name": "AskUserQuestion", "tool_use_id": "toolu_01BRsHWr3KUCpL3tXWQKMYn6", "input": {"questions": [
        {"header": "Color", "multiSelect": false, "options": [{"description": "Red", "label": "Red"}, {"description": "Green", "label": "Green"}, {"description": "Blue", "label": "Blue"}], "question": "Which color do you prefer?"},
        {"header": "Fruits", "multiSelect": true, "options": [{"description": "Apple", "label": "Apple"}, {"description": "Banana", "label": "Banana"}, {"description": "Cherry", "label": "Cherry"}], "question": "Which fruits do you like?"},
        {"header": "City", "multiSelect": false, "options": [{"description": "Paris", "label": "Paris"}, {"description": "Tokyo", "label": "Tokyo"}], "question": "Which city should we use?"}
    ]}});
    let mut relay = RelayHarness::bind().await;
    relay.call(json!(2), "approve", arguments).await;
    let (request_id, kind, payload) = match relay.broker.next().await {
        BrokerEvent::Opened {
            request_id,
            kind,
            payload,
        } => (request_id, kind, payload),
        other => panic!("expected Opened, got {other:?}"),
    };
    assert_eq!(kind, "question");
    assert_eq!(request_id, "toolu_01BRsHWr3KUCpL3tXWQKMYn6");
    let questions = payload["questions"].as_array().expect("questions");
    assert_eq!(questions.len(), 3);
    assert_eq!(questions[0]["id"], "q0");
    assert_eq!(questions[0]["text"], "Which color do you prefer?");
    assert_eq!(questions[0]["header"], "Color");
    assert_eq!(questions[0]["multi_select"], false);
    assert_eq!(questions[0]["allow_custom"], true);
    assert_eq!(
        questions[1]["options"].as_array().expect("options").len(),
        3
    );
    assert_eq!(
        questions[2]["options"].as_array().expect("options").len(),
        2
    );
    assert_eq!(questions[2]["options"][1]["label"], "Tokyo");
    assert_eq!(questions[2]["options"][1]["description"], "Tokyo");
}

#[tokio::test]
async fn answers_go_back_keyed_by_question_text() {
    let arguments = json!({"tool_name": "AskUserQuestion", "tool_use_id": "toolu_01BRsHWr3KUCpL3tXWQKMYn6", "input": {"questions": [
        {"header": "Color", "multiSelect": false, "options": [{"description": "Red", "label": "Red"}, {"description": "Green", "label": "Green"}, {"description": "Blue", "label": "Blue"}], "question": "Which color do you prefer?"},
        {"header": "Fruits", "multiSelect": true, "options": [{"description": "Apple", "label": "Apple"}, {"description": "Banana", "label": "Banana"}, {"description": "Cherry", "label": "Cherry"}], "question": "Which fruits do you like?"},
        {"header": "City", "multiSelect": false, "options": [{"description": "Paris", "label": "Paris"}, {"description": "Tokyo", "label": "Tokyo"}], "question": "Which city should we use?"}
    ]}});
    let mut relay = RelayHarness::bind().await;
    relay.call(json!(2), "approve", arguments).await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    let mut answers = BTreeMap::new();
    answers.insert("q0".to_owned(), AnswerValue::Single("Green".to_owned()));
    answers.insert(
        "q1".to_owned(),
        AnswerValue::Multiple(vec!["Apple".to_owned(), "Cherry".to_owned()]),
    );
    answers.insert("q2".to_owned(), AnswerValue::Single("Reykjavik".to_owned()));
    relay
        .broker
        .answer(&request_id, &DecisionPayload::Question { answers })
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert_eq!(
        text,
        "{\"behavior\":\"allow\",\"updatedInput\":{\"answers\":{\"Which city should we use?\":\"Reykjavik\",\"Which color do you prefer?\":\"Green\",\"Which fruits do you like?\":[\"Apple\",\"Cherry\"]},\"questions\":[{\"header\":\"Color\",\"multiSelect\":false,\"options\":[{\"description\":\"Red\",\"label\":\"Red\"},{\"description\":\"Green\",\"label\":\"Green\"},{\"description\":\"Blue\",\"label\":\"Blue\"}],\"question\":\"Which color do you prefer?\"},{\"header\":\"Fruits\",\"multiSelect\":true,\"options\":[{\"description\":\"Apple\",\"label\":\"Apple\"},{\"description\":\"Banana\",\"label\":\"Banana\"},{\"description\":\"Cherry\",\"label\":\"Cherry\"}],\"question\":\"Which fruits do you like?\"},{\"header\":\"City\",\"multiSelect\":false,\"options\":[{\"description\":\"Paris\",\"label\":\"Paris\"},{\"description\":\"Tokyo\",\"label\":\"Tokyo\"}],\"question\":\"Which city should we use?\"}]}}",
        "answers keyed by question text, byte-identical with the recorded reply"
    );
}

#[tokio::test]
async fn cancelled_notification_closes_request_and_late_decision_is_not_applied() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(7),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "sleep 60"}, "tool_use_id": "toolu_cancel"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    assert_eq!(request_id, "toolu_cancel");
    write_line(
        &mut relay.sink,
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}),
    )
    .await;
    match relay.broker.next().await {
        BrokerEvent::Cancelled { request_id } => assert_eq!(request_id, "toolu_cancel"),
        other => panic!("expected Cancelled, got {other:?}"),
    }
    match relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
        .await
    {
        canterlot_worker::adapter::DecisionOutcome::NotApplied(reason) => {
            assert_eq!(reason, "request_not_pending")
        }
        other => panic!("late decision is not applied, got {other:?}"),
    }
}

#[tokio::test]
async fn pending_call_survives_hub_link_loss_and_is_denied_only_on_supervisor_shutdown() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(8),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "rm -rf /"}, "tool_use_id": "toolu_hang"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    let outcome = relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: None,
            },
        )
        .await;
    assert!(
        matches!(
            outcome,
            canterlot_worker::adapter::DecisionOutcome::Applied(_)
        ),
        "the pending call was still open, got {outcome:?}"
    );
    relay.reply().await;
    relay
        .call(
            json!(9),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_shutdown"}),
        )
        .await;
    let shutdown_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    relay.broker.shutdown().await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 9);
    assert_eq!(
        reply["result"]["content"][0]["text"],
        json!("{\"behavior\":\"deny\",\"message\":\"canterlot supervisor stopped\"}")
            .as_str()
            .unwrap(),
        "shutdown denies the pending call"
    );
    let outcome = relay
        .broker
        .answer(
            &shutdown_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
        .await;
    assert!(
        matches!(
            outcome,
            canterlot_worker::adapter::DecisionOutcome::NotApplied(_)
        ),
        "answered at shutdown, got {outcome:?}"
    );
}

fn observation(window: &str, used_permille: u32, resets_at_ms: u64) -> LimitObservation {
    LimitObservation {
        provider: "claude".into(),
        window: match window {
            "five_hour" => LimitWindow::FiveHour as i32,
            _ => LimitWindow::Weekly as i32,
        },
        used_permille,
        resets_at_ms,
        ..Default::default()
    }
}

#[tokio::test]
async fn limits_tool_reports_latest_observation_or_no_data() {
    let mut relay = RelayHarness::bind().await;
    relay.call(json!(9), "canterlot_limits", json!({})).await;
    match relay.broker.next().await {
        BrokerEvent::LimitsQuery { rpc_id } => assert_eq!(rpc_id, json!(9)),
        other => panic!("expected LimitsQuery, got {other:?}"),
    }
    relay
        .broker
        .answer_limits(
            json!(9),
            &LimitsSnapshot {
                provider: "claude".into(),
                current: vec![
                    observation("five_hour", 500, 1792919400000),
                    observation("weekly", 810, 1793062800000),
                ],
                weekly_reserve_active: false,
            },
        )
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert!(text.contains("50"), "five-hour percent in {text}");
    assert!(text.contains("81"), "weekly percent in {text}");
    relay.call(json!(10), "canterlot_limits", json!({})).await;
    match relay.broker.next().await {
        BrokerEvent::LimitsQuery { rpc_id } => assert_eq!(rpc_id, json!(10)),
        other => panic!("expected LimitsQuery, got {other:?}"),
    }
    relay
        .broker
        .answer_limits(
            json!(10),
            &LimitsSnapshot {
                provider: "claude".into(),
                ..Default::default()
            },
        )
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert_eq!(text, "no limit data yet in this session");
}

#[test]
fn tool_without_prompt_in_manual_mode_is_marked_cli_policy_and_not_in_bypass() {
    use canterlot_worker::adapter::claude_code::mcp::cli_policy_mark;
    use std::collections::HashSet;
    let approved: HashSet<String> = ["toolu_approved".to_owned()].into_iter().collect();
    assert_eq!(cli_policy_mark("manual", &approved, "toolu_approved"), None);
    assert_eq!(
        cli_policy_mark("manual", &approved, "toolu_fresh"),
        Some("cli_policy")
    );
    assert_eq!(
        cli_policy_mark("bypassPermissions", &approved, "toolu_fresh"),
        None,
        "in bypass everything is CLI-approved already"
    );
    assert_eq!(
        cli_policy_mark("default", &approved, "toolu_fresh"),
        Some("cli_policy")
    );
    assert_eq!(
        cli_policy_mark("plan", &approved, "toolu_fresh"),
        Some("cli_policy")
    );
}
