//! MCP-инструменты пресетов canterlot (план 2c, задача 5): список, сохранение,
//! применение к своей сессии — для Claude Code и Codex.

#![forbid(unsafe_code)]

use canterlot_worker::adapter::{
    AgentCallResult,
    canterlot_mcp::{Broker, BrokerEvent, mcp, relay_hello},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

fn tool_names(tools: &Value) -> Vec<&str> {
    tools
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect()
}

// У каждого харнесса свой бюджет в тексте: модель OpenCode не должна читать про Claude.
#[test]
fn opencode_gets_its_own_budget_and_instructions_without_approve() {
    use canterlot_worker::adapter::canterlot_mcp::ToolSet;
    let list = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        ToolSet::LimitsOnly,
        "opencode",
    );
    let tools = list.as_reply().expect("tools/list")["result"]["tools"].clone();
    let limits = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "canterlot_limits")
        .unwrap();
    let description = limits["description"].as_str().unwrap();
    assert!(description.contains("OpenCode API budget"), "{description}");
    assert!(!description.contains("Claude"), "{description}");
    let start = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
        ToolSet::LimitsOnly,
        "opencode",
    );
    let text = start.as_reply().expect("initialize")["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!text.contains("Claude"), "{text}");
    assert!(!text.contains("Codex"), "{text}");
    assert!(text.contains("canterlot_limits"), "{text}");
    assert!(text.contains("canterlot_presets_list"), "{text}");
    let snapshot = canterlot_proto::worker::v1::LimitsSnapshot {
        provider: "opencode".into(),
        current: vec![canterlot_proto::worker::v1::LimitObservation {
            window: canterlot_proto::worker::v1::LimitWindow::Weekly.into(),
            used_permille: 500,
            ..Default::default()
        }],
        ..Default::default()
    };
    let reply = mcp::limits_tool_text(&snapshot, "opencode");
    assert!(reply.starts_with("OpenCode API budget"), "{reply}");
}

#[test]
fn both_tool_sets_list_the_preset_tools() {
    for (tool_set, provider) in [
        (
            canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
            "claude",
        ),
        (
            canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
            "codex",
        ),
    ] {
        let list = mcp::handle_rpc_with_provider(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            tool_set,
            provider,
        );
        let tools = list.as_reply().expect("tools/list")["result"]["tools"].clone();
        let names = tool_names(&tools);
        assert!(names.contains(&"canterlot_limits"), "{names:?}");
        for mcp_name in [
            "canterlot_presets_list",
            "canterlot_preset_save",
            "canterlot_preset_apply",
        ] {
            assert!(names.contains(&mcp_name), "{provider}: {names:?}");
        }
        let list_tool = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "canterlot_presets_list")
            .unwrap();
        assert_eq!(list_tool["annotations"]["readOnlyHint"], true);
        assert!(
            list_tool["description"]
                .as_str()
                .unwrap()
                .contains("pass `id`"),
            "подсказка про чтение текста: {}",
            list_tool["description"]
        );
        let save = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "canterlot_preset_save")
            .unwrap();
        assert_eq!(
            save["inputSchema"]["required"],
            json!(["title", "text"]),
            "схема сохранения: {save}"
        );
        let apply = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "canterlot_preset_apply")
            .unwrap();
        assert_eq!(apply["inputSchema"]["required"], json!(["id"]));
    }
}

struct RelayHarness {
    broker: Broker,
    socket: BufReader<tokio::net::unix::OwnedReadHalf>,
    sink: tokio::net::unix::OwnedWriteHalf,
    _dir: tempfile::TempDir,
}

impl RelayHarness {
    async fn bind(
        tool_set: canterlot_worker::adapter::canterlot_mcp::ToolSet,
        provider: &str,
    ) -> Self {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let broker = Broker::bind_with(dir.path(), tool_set, provider)
            .await
            .expect("bind");
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

    async fn call(&mut self, id: Value, name: &str, arguments: Value) {
        let mut line = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
            .to_string();
        line.push('\n');
        self.sink.write_all(line.as_bytes()).await.expect("write");
        self.sink.flush().await.expect("flush");
    }

    async fn reply(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.socket.read_line(&mut line))
            .await
            .expect("read timeout")
            .expect("read");
        serde_json::from_str(&line).expect("json line")
    }

    /// Второе соединение с тем же сокетом.
    async fn peer(&self) -> Peer {
        let (read, mut sink) = UnixStream::connect(self._dir.path().join("mcp.sock"))
            .await
            .expect("connect")
            .into_split();
        sink.write_all(relay_hello(self.broker.relay_secret()).as_bytes())
            .await
            .expect("hello");
        (BufReader::new(read), sink)
    }
}

type Peer = (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
);

async fn send(sink: &mut tokio::net::unix::OwnedWriteHalf, value: Value) {
    let mut line = value.to_string();
    line.push('\n');
    sink.write_all(line.as_bytes()).await.expect("write");
    sink.flush().await.expect("flush");
}

async fn read_reply(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .expect("read timeout")
        .expect("read");
    serde_json::from_str(&line).expect("json line")
}

async fn agent_call(broker: &mut Broker) -> uuid::Uuid {
    match tokio::time::timeout(Duration::from_secs(10), broker.next())
        .await
        .expect("broker event")
    {
        BrokerEvent::AgentTool { call_id, .. } => call_id,
        other => panic!("ожидался AgentTool, получено {other:?}"),
    }
}

#[tokio::test]
async fn preset_tool_call_becomes_agent_call_and_result_is_text_content() {
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    )
    .await;
    relay
        .call(json!(7), "canterlot_presets_list", json!({}))
        .await;
    let (rpc_id, call_id, tool, arguments) = match relay.broker.next().await {
        BrokerEvent::AgentTool {
            rpc_id,
            call_id,
            tool,
            arguments,
        } => (rpc_id, call_id, tool, arguments),
        other => panic!("ожидался AgentTool, получено {other:?}"),
    };
    assert_eq!(rpc_id, json!(7));
    assert_eq!(tool, "presets.list");
    assert_eq!(arguments, json!({}));
    relay
        .broker
        .answer_agent_tool(
            call_id,
            &AgentCallResult::Ok(json!({"presets": [], "applied": {"id": "clean"}})),
        )
        .await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 7);
    assert!(reply.get("error").is_none(), "{reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    let parsed: Value = serde_json::from_str(text).expect("текст — JSON");
    assert_eq!(parsed["applied"]["id"], "clean");
}

#[tokio::test]
async fn hub_error_becomes_is_error_result_with_code() {
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
        "codex",
    )
    .await;
    relay
        .call(
            json!(8),
            "canterlot_preset_apply",
            json!({"id": "00000000-0000-7000-8000-000000000000"}),
        )
        .await;
    let call_id = match relay.broker.next().await {
        BrokerEvent::AgentTool { call_id, tool, .. } => {
            assert_eq!(tool, "presets.apply");
            call_id
        }
        other => panic!("ожидался AgentTool, получено {other:?}"),
    };
    relay
        .broker
        .answer_agent_tool(
            call_id,
            &AgentCallResult::Err {
                code: "preset_not_found".into(),
                message: "Пресет не найден".into(),
            },
        )
        .await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 8);
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["isError"], json!(true), "{reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert!(
        text.starts_with("preset_not_found: "),
        "код в тексте: {text}"
    );
    assert!(!text.contains("секрет"), "{text}");
}

#[tokio::test]
async fn cancelled_preset_call_ignores_late_result() {
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    )
    .await;
    relay
        .call(json!(9), "canterlot_presets_list", json!({}))
        .await;
    let call_id = match relay.broker.next().await {
        BrokerEvent::AgentTool { call_id, .. } => call_id,
        other => panic!("ожидался AgentTool, получено {other:?}"),
    };
    let mut line =
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9}})
            .to_string();
    line.push('\n');
    relay.sink.write_all(line.as_bytes()).await.expect("write");
    relay.sink.flush().await.expect("flush");
    tokio::time::sleep(Duration::from_millis(200)).await;
    relay
        .broker
        .answer_agent_tool(call_id, &AgentCallResult::Ok(json!({"presets": []})))
        .await;
    let mut buf = String::new();
    let read =
        tokio::time::timeout(Duration::from_millis(300), relay.socket.read_line(&mut buf)).await;
    assert!(read.is_err(), "поздний итог отброшен, ответа нет: {buf}");
}

#[tokio::test]
async fn connections_with_equal_rpc_ids_keep_their_own_preset_calls() {
    // Codex: соединений несколько, нумерация rpc id у каждого своя.
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
        "codex",
    )
    .await;
    relay
        .call(json!(3), "canterlot_presets_list", json!({}))
        .await;
    let first = agent_call(&mut relay.broker).await;
    let (mut reader, mut sink) = relay.peer().await;
    send(
        &mut sink,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"canterlot_presets_list","arguments":{}}}),
    )
    .await;
    let second = agent_call(&mut relay.broker).await;
    relay
        .broker
        .answer_agent_tool(first, &AgentCallResult::Ok(json!({"from": "first"})))
        .await;
    relay
        .broker
        .answer_agent_tool(second, &AgentCallResult::Ok(json!({"from": "second"})))
        .await;
    let text = relay.reply().await["result"]["content"][0]["text"].clone();
    assert!(text.as_str().unwrap().contains("first"), "{text}");
    let text = read_reply(&mut reader).await["result"]["content"][0]["text"].clone();
    assert!(text.as_str().unwrap().contains("second"), "{text}");
}

#[tokio::test]
async fn cancel_on_one_connection_keeps_the_preset_call_of_another() {
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
        "codex",
    )
    .await;
    relay
        .call(json!(3), "canterlot_presets_list", json!({}))
        .await;
    let first = agent_call(&mut relay.broker).await;
    let (_reader, mut sink) = relay.peer().await;
    send(
        &mut sink,
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}}),
    )
    .await;
    // Строки соединения читаются по порядку: событие вызова значит, что отмена разобрана.
    send(
        &mut sink,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"canterlot_presets_list","arguments":{}}}),
    )
    .await;
    agent_call(&mut relay.broker).await;
    relay
        .broker
        .answer_agent_tool(first, &AgentCallResult::Ok(json!({"presets": []})))
        .await;
    assert_eq!(
        relay.reply().await["id"],
        3,
        "чужая отмена вызов не снимает"
    );
}

#[tokio::test]
async fn superseded_relay_does_not_reach_presets() {
    let mut relay = RelayHarness::bind(
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    )
    .await;
    relay
        .call(json!(1), "canterlot_presets_list", json!({}))
        .await;
    agent_call(&mut relay.broker).await;
    // Перезапуск CLI: новый relay вытесняет прежний.
    relay.broker.arm();
    let (_reader, mut sink) = relay.peer().await;
    send(
        &mut sink,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"canterlot_presets_list","arguments":{}}}),
    )
    .await;
    agent_call(&mut relay.broker).await;
    relay
        .call(json!(2), "canterlot_preset_apply", json!({"id": "clean"}))
        .await;
    let reply = relay.reply().await;
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["error"]["code"], -32602, "{reply}");
    let quiet = tokio::time::timeout(Duration::from_millis(300), relay.broker.next()).await;
    assert!(quiet.is_err(), "вытесненный relay хаб не зовёт: {quiet:?}");
}

#[test]
fn invalid_arguments_are_rejected_before_the_hub() {
    // Не объект.
    let reply = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"canterlot_presets_list","arguments":[]}}),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    );
    assert_eq!(
        reply.as_error().expect("ошибка")["message"],
        "invalid_arguments"
    );
    // Неизвестное поле.
    let reply = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"canterlot_presets_list","arguments":{"unknown":"поле"}}}),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    );
    assert_eq!(
        reply.as_error().expect("ошибка")["message"],
        "invalid_arguments"
    );
    // Неверный тип.
    let reply = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"canterlot_preset_save","arguments":{"title":42,"text":"текст"}}}),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    );
    assert_eq!(
        reply.as_error().expect("ошибка")["message"],
        "invalid_arguments"
    );
    // Нет обязательных полей.
    let reply = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"canterlot_preset_apply","arguments":{}}}),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    );
    assert_eq!(
        reply.as_error().expect("ошибка")["message"],
        "invalid_arguments"
    );
    // Корректный вызов — не ошибка, а событие брокера (проверяется сокетом выше).
    let action = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
        "params":{"name":"canterlot_preset_apply","arguments":{"id":"clean"}}}),
        canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
        "claude",
    );
    assert!(action.as_reply().is_none(), "ошибки нет");
    assert!(action.as_error().is_none(), "ошибки нет");
}

#[test]
fn server_instructions_mention_presets_and_stay_within_15_lines() {
    let lines: Vec<&str> = mcp::SERVER_INSTRUCTIONS.lines().collect();
    assert!(
        (10..=15).contains(&lines.len()),
        "{}",
        mcp::SERVER_INSTRUCTIONS
    );
    assert!(
        mcp::SERVER_INSTRUCTIONS.contains("canterlot_presets_list"),
        "{}",
        mcp::SERVER_INSTRUCTIONS
    );
    assert!(
        mcp::SERVER_INSTRUCTIONS.contains("canterlot_preset_apply"),
        "{}",
        mcp::SERVER_INSTRUCTIONS
    );
}

#[tokio::test]
async fn claude_and_codex_adapters_forward_agent_call_results() {
    for (tool_set, provider) in [
        (
            canterlot_worker::adapter::canterlot_mcp::ToolSet::ApproveAndLimits,
            "claude",
        ),
        (
            canterlot_worker::adapter::canterlot_mcp::ToolSet::LimitsOnly,
            "codex",
        ),
    ] {
        let mut relay = RelayHarness::bind(tool_set, provider).await;
        relay
            .call(
                json!(11),
                "canterlot_preset_save",
                json!({"title": "Заметка", "text": "текст"}),
            )
            .await;
        let call_id = match relay.broker.next().await {
            BrokerEvent::AgentTool { call_id, tool, .. } => {
                assert_eq!(tool, "presets.save");
                call_id
            }
            other => panic!("{provider}: ожидался AgentTool, получено {other:?}"),
        };
        relay
            .broker
            .answer_agent_tool(
                call_id,
                &AgentCallResult::Ok(json!({"id": "preset-1", "revision": 2})),
            )
            .await;
        let reply = relay.reply().await;
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("text block");
        assert!(
            text.contains("Saved as revision 2"),
            "{provider}: подсказка сохранения: {text}"
        );
        assert!(
            text.contains("canterlot_preset_apply"),
            "{provider}: подсказка применения: {text}"
        );
    }
}
