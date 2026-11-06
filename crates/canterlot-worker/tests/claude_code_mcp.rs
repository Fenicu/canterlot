//! canterlot MCP server tests (plan Task 4): relay, broker, approvals,
//! questions, canterlot_limits. Recorded B10 frames are matched byte for byte.

#![forbid(unsafe_code)]

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::adapter::canterlot_mcp::relay_hello;
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
        let (read, mut sink) = stream.into_split();
        sink.write_all(relay_hello(broker.relay_secret()).as_bytes())
            .await
            .expect("hello");
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

    fn socket_path(&self) -> PathBuf {
        self._dir.path().join("mcp.sock")
    }
}

type Peer = (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
);

async fn peer(path: &std::path::Path, secret: &str) -> Peer {
    let (read, mut sink) = UnixStream::connect(path)
        .await
        .expect("connect")
        .into_split();
    sink.write_all(relay_hello(secret).as_bytes())
        .await
        .expect("hello");
    (BufReader::new(read), sink)
}

async fn approve_on(sink: &mut tokio::net::unix::OwnedWriteHalf, id: u64, tool_use_id: &str) {
    write_line(
        sink,
        &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"approve",
            "arguments":{"tool_name":"Bash","input":{"command":"touch x"},"tool_use_id":tool_use_id},
            "_meta":{"claudecode/toolUseId":tool_use_id}}}),
    )
    .await;
}

/// Сервер закрыл соединение: чтение даёт EOF (или ошибку разрыва).
async fn closed_by_server(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> bool {
    let mut line = String::new();
    matches!(
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await,
        Ok(Ok(0) | Err(_))
    )
}

async fn opened(broker: &mut Broker) -> String {
    match tokio::time::timeout(Duration::from_secs(5), broker.next())
        .await
        .expect("broker event")
    {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    }
}

async fn quiet(broker: &mut Broker) -> bool {
    tokio::time::timeout(Duration::from_millis(200), broker.next())
        .await
        .is_err()
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
    assert_eq!(tools.len(), 22);
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
async fn permission_with_a_normalized_input_key_stays_a_permission() {
    let mut relay = RelayHarness::bind().await;
    let input = json!({"normalized": {"questions": []}, "path": "/tmp/x"});
    relay
        .call(
            json!(11),
            "approve",
            json!({"tool_name": "mcp__owner__tool", "input": input, "tool_use_id": "toolu_normalized"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened {
            request_id, kind, ..
        } => {
            assert_eq!(kind, "permission");
            request_id
        }
        other => panic!("expected Opened, got {other:?}"),
    };
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
    assert!(matches!(
        outcome,
        canterlot_worker::adapter::DecisionOutcome::Applied(_)
    ));
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer, json!({"behavior": "allow", "updatedInput": input}));
}

#[tokio::test]
async fn mismatched_decision_keeps_the_call_pending() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(12),
            "approve",
            json!({"tool_name": "Bash", "input": {"command": "touch x"}, "tool_use_id": "toolu_mismatch"}),
        )
        .await;
    let request_id = match relay.broker.next().await {
        BrokerEvent::Opened { request_id, .. } => request_id,
        other => panic!("expected Opened, got {other:?}"),
    };
    let wrong = relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Question {
                answers: BTreeMap::new(),
            },
        )
        .await;
    assert!(
        matches!(&wrong, canterlot_worker::adapter::DecisionOutcome::NotApplied(r) if r == "decision_mismatch"),
        "{wrong:?}"
    );
    let right = relay
        .broker
        .answer(
            &request_id,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: None,
            },
        )
        .await;
    assert!(matches!(
        right,
        canterlot_worker::adapter::DecisionOutcome::Applied(_)
    ));
    assert_eq!(relay.reply().await["id"], 12);
}

#[tokio::test]
async fn write_to_a_stalled_relay_is_bounded_and_keeps_the_call() {
    use canterlot_worker::adapter::DecisionOutcome;
    let mut h = RelayHarness::bind().await;
    // Ответ больше буфера сокета, а relay его не читает: CLI занят своим stdout.
    let input = json!({"content": "x".repeat(4 * 1024 * 1024)});
    h.call(
        json!(1),
        "approve",
        json!({"tool_name": "Write", "input": input, "tool_use_id": "toolu_stalled"}),
    )
    .await;
    let request = opened(&mut h.broker).await;
    let allow = DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    };
    let outcome = tokio::time::timeout(Duration::from_secs(15), h.broker.answer(&request, &allow))
        .await
        .expect("write to a stalled relay must not block the supervisor loop");
    assert!(
        matches!(outcome, DecisionOutcome::Undetermined),
        "{outcome:?}"
    );
    // Строка ушла частично: повторная запись обязана отказать, а не дописать за ней.
    let again = tokio::time::timeout(Duration::from_secs(1), h.broker.answer(&request, &allow))
        .await
        .expect("connection with a partial line stays closed");
    assert!(matches!(again, DecisionOutcome::Undetermined), "{again:?}");
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
async fn second_connection_while_the_relay_is_live_is_refused() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(1),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_live"}),
        )
        .await;
    let live = opened(&mut relay.broker).await;
    let (mut reader, mut sink) = peer(&relay.socket_path(), relay.broker.relay_secret()).await;
    approve_on(&mut sink, 2, "toolu_intruder").await;
    assert!(
        closed_by_server(&mut reader).await,
        "intruder is disconnected"
    );
    assert!(
        quiet(&mut relay.broker).await,
        "intruder opens nothing and cancels nothing"
    );
    let outcome = relay
        .broker
        .answer(
            &live,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
        .await;
    assert!(matches!(
        outcome,
        canterlot_worker::adapter::DecisionOutcome::Applied(_)
    ));
    assert_eq!(
        relay.reply().await["id"],
        1,
        "answer reaches the live relay"
    );
}

#[tokio::test]
async fn armed_relay_supersedes_and_each_connection_keeps_its_own_calls() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(1),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_old"}),
        )
        .await;
    let old = opened(&mut relay.broker).await;
    // Перезапуск CLI: адаптер взводит допуск, новый relay вытесняет прежний.
    relay.broker.arm();
    let (mut reader, mut sink) = peer(&relay.socket_path(), relay.broker.relay_secret()).await;
    approve_on(&mut sink, 7, "toolu_new").await;
    let new = opened(&mut relay.broker).await;
    assert_eq!(new, "toolu_new");
    // Вытесненный relay запросов владельцу больше не открывает.
    relay
        .call(
            json!(2),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_stale"}),
        )
        .await;
    assert_eq!(relay.reply().await["error"]["code"], -32602);
    assert!(quiet(&mut relay.broker).await);
    // Разрыв прежнего соединения отменяет только его вызов.
    let RelayHarness {
        mut broker,
        socket,
        sink: old_sink,
        _dir,
    } = relay;
    drop(old_sink);
    drop(socket);
    match tokio::time::timeout(Duration::from_secs(5), broker.next())
        .await
        .expect("cancel")
    {
        BrokerEvent::Cancelled { request_id } => assert_eq!(request_id, old),
        other => panic!("expected Cancelled, got {other:?}"),
    }
    assert!(quiet(&mut broker).await, "the new call survives");
    let outcome = broker
        .answer(
            &new,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: None,
            },
        )
        .await;
    assert!(matches!(
        outcome,
        canterlot_worker::adapter::DecisionOutcome::Applied(_)
    ));
    assert_eq!(
        read_json(&mut reader).await["id"],
        7,
        "answer goes to the asking relay"
    );
}

#[tokio::test]
async fn duplicate_tool_use_id_is_refused_and_keeps_the_first_call() {
    let mut relay = RelayHarness::bind().await;
    relay
        .call(
            json!(1),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_dup"}),
        )
        .await;
    let first = opened(&mut relay.broker).await;
    relay.call(json!(2), "approve", json!({"tool_name": "Bash", "input": {"command": "rm -rf /"}, "tool_use_id": "toolu_dup"})).await;
    let refused = relay.reply().await;
    assert_eq!(refused["id"], 2);
    assert_eq!(refused["error"]["code"], -32602);
    assert!(quiet(&mut relay.broker).await);
    relay
        .broker
        .answer(
            &first,
            &DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
        .await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 1);
    assert_eq!(
        reply["result"]["content"][0]["text"],
        "{\"behavior\":\"allow\",\"updatedInput\":{}}"
    );
}

#[tokio::test]
async fn oversized_line_closes_the_connection() {
    let mut relay = RelayHarness::bind().await;
    let big = vec![b'x'; canterlot_worker::adapter::canterlot_mcp::broker::MAX_MCP_LINE_BYTES + 16];
    let _ = relay.sink.write_all(&big).await;
    assert!(closed_by_server(&mut relay.socket).await);
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

#[tokio::test]
async fn limits_tool_reports_the_hub_text_or_no_data() {
    let mut relay = RelayHarness::bind().await;
    relay.call(json!(9), "canterlot_limits", json!({})).await;
    let call_id = match relay.broker.next().await {
        BrokerEvent::LimitsQuery { call_id } => call_id,
        other => panic!("expected LimitsQuery, got {other:?}"),
    };
    relay
        .broker
        .answer_agent_tool(
            call_id,
            &canterlot_worker::adapter::AgentCallResult::Ok(serde_json::json!({
                "text": "Claude API budget: five-hour window 50% used; weekly window 81% used."
            })),
        )
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"]
        .as_str()
        .expect("text block")
        .to_owned();
    assert!(text.contains("50"), "five-hour percent in {text}");
    assert!(text.contains("81"), "weekly percent in {text}");
    relay.call(json!(10), "canterlot_limits", json!({})).await;
    let call_id = match relay.broker.next().await {
        BrokerEvent::LimitsQuery { call_id } => call_id,
        other => panic!("expected LimitsQuery, got {other:?}"),
    };
    relay
        .broker
        .answer_agent_tool(
            call_id,
            &canterlot_worker::adapter::AgentCallResult::Ok(serde_json::json!({
                "text": "no limit data yet in this session"
            })),
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

/// Чужой процесс того же пользователя: подключается к сокету и сразу шлёт вызов.
async fn foreign(path: &std::path::Path, hello: &str) -> Peer {
    let (read, mut sink) = UnixStream::connect(path)
        .await
        .expect("connect")
        .into_split();
    sink.write_all(hello.as_bytes()).await.expect("hello");
    (BufReader::new(read), sink)
}

#[tokio::test]
async fn a_foreign_process_without_the_session_secret_is_dropped() {
    let mut relay = RelayHarness::bind().await;
    let path = relay.socket_path();
    relay
        .call(
            json!(1),
            "approve",
            json!({"tool_name": "Bash", "input": {}, "tool_use_id": "toolu_live"}),
        )
        .await;
    opened(&mut relay.broker).await;
    // Допуск взведён, как перед перезапуском CLI: чужой процесс не должен его занять.
    relay.broker.arm();
    for hello in [
        String::new(),
        relay_hello("not-the-secret"),
        "\n".to_owned(),
    ] {
        let (mut reader, mut sink) = foreign(&path, &hello).await;
        approve_on(&mut sink, 9, "toolu_foreign").await;
        assert!(
            closed_by_server(&mut reader).await,
            "dropped without the secret: {hello:?}"
        );
    }
    assert!(quiet(&mut relay.broker).await, "nothing was opened");
    let (_reader, mut sink) = peer(&path, relay.broker.relay_secret()).await;
    approve_on(&mut sink, 10, "toolu_own").await;
    assert_eq!(
        opened(&mut relay.broker).await,
        "toolu_own",
        "the armed slot is still free for the session's own relay"
    );
}

#[tokio::test]
async fn a_codex_socket_also_drops_a_foreign_process() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut broker = canterlot_worker::adapter::canterlot_mcp::Broker::bind_with(
        dir.path(),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
        "codex",
    )
    .await
    .expect("bind");
    let (mut reader, mut sink) = foreign(&dir.path().join("mcp.sock"), "").await;
    write_line(
        &mut sink,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"session_send",
            "arguments":{"session_id":"root","text":"do it"}}}),
    )
    .await;
    assert!(closed_by_server(&mut reader).await);
    assert!(quiet(&mut broker).await, "no agent call reaches the hub");
}

#[tokio::test]
async fn relay_sends_the_session_secret_first_when_given_one() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("mcp.sock");
    let listener = UnixListener::bind(&path).expect("listen");
    let _child = tokio::process::Command::new(worker_binary())
        .args(["mcp-relay", "--socket"])
        .arg(&path)
        .env("CANTERLOT_MCP_SECRET", "abc123")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn relay");
    let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .expect("accept")
        .expect("accept ok");
    let mut lines = BufReader::new(stream);
    let mut first = String::new();
    tokio::time::timeout(Duration::from_secs(10), lines.read_line(&mut first))
        .await
        .expect("hello in time")
        .expect("read");
    assert_eq!(first, relay_hello("abc123"));
}
