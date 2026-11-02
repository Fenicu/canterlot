//! MCP-инструменты межсессионной работы: список, вызовы, подсказки, инструкции.

#![forbid(unsafe_code)]

use canterlot_worker::adapter::{
    AgentCallResult,
    canterlot_mcp::{Broker, BrokerEvent, ToolSet, mcp},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

const PROVIDERS: [(ToolSet, &str); 3] = [
    (ToolSet::ApproveAndLimits, "claude"),
    (ToolSet::LimitsOnly, "codex"),
    (ToolSet::LimitsOnly, "opencode"),
];

const SESSION_TOOLS: [(&str, &str); 10] = [
    ("sessions_list", "sessions.list"),
    ("session_inspect", "sessions.inspect"),
    ("session_send", "sessions.send"),
    ("session_spawn", "sessions.spawn"),
    ("session_subscribe", "sessions.subscribe"),
    ("session_unsubscribe", "sessions.unsubscribe"),
    ("session_wait", "sessions.wait"),
    ("session_accept", "sessions.accept"),
    ("session_interrupt", "sessions.interrupt"),
    ("session_stop", "sessions.stop"),
];

fn tools_of(tool_set: ToolSet, provider: &str) -> Vec<Value> {
    let list = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        tool_set,
        provider,
    );
    list.as_reply().expect("tools/list")["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone()
}

fn instructions_of(tool_set: ToolSet, provider: &str) -> String {
    let start = mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}),
        tool_set,
        provider,
    );
    start.as_reply().expect("initialize")["result"]["instructions"]
        .as_str()
        .expect("instructions")
        .to_owned()
}

#[test]
fn session_tools_listed_for_all_three_providers() {
    for (tool_set, provider) in PROVIDERS {
        let tools = tools_of(tool_set, provider);
        for (mcp_name, _) in SESSION_TOOLS {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == mcp_name)
                .unwrap_or_else(|| panic!("{provider}: нет {mcp_name}"));
            assert_eq!(tool["inputSchema"]["type"], "object", "{mcp_name}");
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                json!(false),
                "{provider}: {mcp_name}"
            );
            let description = tool["description"].as_str().expect("description");
            assert!(
                description.contains("Do not"),
                "{provider}: {mcp_name} не говорит, когда не звать: {description}"
            );
        }
        let expected = if tool_set == ToolSet::ApproveAndLimits {
            2 + 3 + 10 + 1
        } else {
            1 + 3 + 10 + 1
        };
        assert_eq!(tools.len(), expected, "{provider}");
        for required in [
            ("session_send", vec!["session_id", "text"]),
            ("session_spawn", vec!["harness", "brief"]),
            ("session_inspect", vec!["session_id"]),
            ("session_wait", vec!["session_id"]),
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == required.0)
                .unwrap();
            assert_eq!(
                tool["inputSchema"]["required"],
                json!(required.1),
                "{}",
                required.0
            );
        }
        let inspect = tools
            .iter()
            .find(|tool| tool["name"] == "session_inspect")
            .unwrap();
        for field in ["block_id", "offset"] {
            assert!(
                inspect["inputSchema"]["properties"].get(field).is_some(),
                "{provider}: {field}"
            );
        }
    }
}

#[test]
fn session_tool_calls_map_to_hub_names() {
    for (tool_set, provider) in PROVIDERS {
        for (mcp_name, hub_name) in SESSION_TOOLS {
            let arguments = json!({"session_id": "x", "незнакомое": 1});
            let action = mcp::handle_rpc_with_provider(
                &json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                    "params":{"name":mcp_name,"arguments":arguments}}),
                tool_set,
                provider,
            );
            match action {
                mcp::McpAction::ToolCall {
                    kind: mcp::ToolKind::Agent { tool },
                    arguments: forwarded,
                    ..
                } => {
                    assert_eq!(tool, hub_name, "{provider}: {mcp_name}");
                    assert_eq!(forwarded, arguments, "хаб проверяет аргументы сам");
                }
                other => panic!("{provider}: {mcp_name}: {other:?}"),
            }
        }
        // Не объект: хаб не зовём.
        let reply = mcp::handle_rpc_with_provider(
            &json!({"jsonrpc":"2.0","id":8,"method":"tools/call",
                "params":{"name":"session_send","arguments":"текст"}}),
            tool_set,
            provider,
        );
        assert_eq!(
            reply.as_error().expect("ошибка")["message"],
            "invalid_arguments"
        );
        // Без аргументов вызов проходит пустым объектом.
        let action = mcp::handle_rpc_with_provider(
            &json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"sessions_list"}}),
            tool_set,
            provider,
        );
        assert!(
            matches!(action, mcp::McpAction::ToolCall { ref arguments, .. } if arguments == &json!({})),
            "{action:?}"
        );
    }
}

#[test]
fn read_tools_are_read_only() {
    for (tool_set, provider) in PROVIDERS {
        let tools = tools_of(tool_set, provider);
        for (mcp_name, _) in SESSION_TOOLS {
            let tool = tools.iter().find(|tool| tool["name"] == mcp_name).unwrap();
            let read_only = matches!(mcp_name, "sessions_list" | "session_inspect");
            assert_eq!(
                tool["annotations"]["readOnlyHint"] == json!(true),
                read_only,
                "{provider}: {mcp_name}"
            );
        }
    }
}

#[test]
fn instructions_stay_short() {
    for (tool_set, provider) in PROVIDERS {
        let text = instructions_of(tool_set, provider);
        let lines = text.lines().count();
        assert!((10..=15).contains(&lines), "{provider}: {lines}\n{text}");
    }
    assert_eq!(mcp::SERVER_INSTRUCTIONS.lines().count(), 15);
}

#[test]
fn claude_instructions_keep_approve_and_codex_drops_it() {
    let claude = instructions_of(ToolSet::ApproveAndLimits, "claude");
    assert!(claude.contains("Never call the approve tool"), "{claude}");
    assert_eq!(claude, mcp::SERVER_INSTRUCTIONS);
    for provider in ["codex", "opencode"] {
        let text = instructions_of(ToolSet::LimitsOnly, provider);
        assert!(!text.contains("approve tool"), "{provider}: {text}");
        assert!(!text.contains("AskUserQuestion"), "{provider}: {text}");
        assert!(!text.contains("Claude"), "{provider}: {text}");
    }
    assert_eq!(
        instructions_of(ToolSet::LimitsOnly, "codex"),
        mcp::CODEX_SERVER_INSTRUCTIONS
    );
    for text in [
        mcp::SERVER_INSTRUCTIONS,
        mcp::CODEX_SERVER_INSTRUCTIONS,
        mcp::OPENCODE_SERVER_INSTRUCTIONS,
    ] {
        for needed in [
            "sessions_list",
            "session_inspect",
            "session_send",
            "session_spawn",
            "session_wait",
            "session_accept",
            "session_interrupt",
            "session_stop",
            "pending_approval",
            "not the owner",
        ] {
            assert!(text.contains(needed), "нет {needed}: {text}");
        }
    }
}

#[test]
fn instructions_name_prefixed_tools_for_opencode() {
    let text = instructions_of(ToolSet::LimitsOnly, "opencode");
    for name in [
        "canterlot_canterlot_limits",
        "canterlot_canterlot_presets_list",
        "canterlot_canterlot_preset_save",
        "canterlot_canterlot_preset_apply",
        "canterlot_sessions_list",
        "canterlot_session_inspect",
        "canterlot_session_send",
        "canterlot_session_spawn",
        "canterlot_session_wait",
        "canterlot_session_accept",
        "canterlot_session_interrupt",
        "canterlot_session_stop",
    ] {
        assert!(text.contains(name), "нет {name}: {text}");
    }
    // Голых имён без префикса сервера нет: модель вызовет только существующие.
    for bare in [
        "canterlot_limits",
        "canterlot_presets_list",
        "canterlot_preset_save",
        "canterlot_preset_apply",
    ] {
        assert!(
            !text.replace("canterlot_canterlot_", "").contains(bare),
            "голое имя {bare}: {text}"
        );
    }
    for (mcp_name, _) in SESSION_TOOLS {
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if word.ends_with(mcp_name) && word != mcp_name {
                assert_eq!(word, format!("canterlot_{mcp_name}"), "{text}");
            }
        }
    }
}

#[test]
fn tool_hint_is_appended() {
    let result = AgentCallResult::Ok(json!({
        "input_id": "i-1",
        "deduplicated": false,
        "subscribed": true,
        "hint": "Queued. You are subscribed: do not poll.",
    }));
    let (text, is_error) = mcp::agent_tool_text("sessions.send", &result);
    assert!(!is_error);
    assert!(
        text.ends_with("\nQueued. You are subscribed: do not poll."),
        "{text}"
    );
    assert_eq!(
        text.matches("Queued. You are subscribed").count(),
        1,
        "подсказка не дублируется в JSON: {text}"
    );
    let json_part = text
        .split_once("\nQueued")
        .expect("подсказка отдельной строкой")
        .0;
    let parsed: Value = serde_json::from_str(json_part).expect("JSON до подсказки");
    assert_eq!(parsed["input_id"], "i-1");
    assert!(parsed.get("hint").is_none());

    let plain = AgentCallResult::Ok(json!({"sessions": []}));
    let (text, _) = mcp::agent_tool_text("sessions.list", &plain);
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("JSON"),
        json!({"sessions": []})
    );

    let failed = AgentCallResult::Err {
        code: "rate_limited".into(),
        message: "too many sends".into(),
    };
    let (text, is_error) = mcp::agent_tool_text("sessions.send", &failed);
    assert!(is_error);
    assert_eq!(text, "rate_limited: too many sends");

    // Не строка в `hint` остаётся данными.
    let odd = AgentCallResult::Ok(json!({"hint": 5}));
    let (text, _) = mcp::agent_tool_text("sessions.wait", &odd);
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("JSON"),
        json!({"hint": 5})
    );
}

#[test]
fn no_tool_touches_the_whitelist() {
    for (mcp_name, hub_name) in mcp::AGENT_TOOLS {
        for name in [mcp_name, hub_name] {
            let lower = name.to_ascii_lowercase();
            assert!(
                !lower.contains("whitelist")
                    && !lower.contains("allowlist")
                    && !lower.contains("spawn_list"),
                "{name}"
            );
        }
    }
    for (tool_set, provider) in PROVIDERS {
        for tool in tools_of(tool_set, provider) {
            let schema = tool["inputSchema"].to_string().to_ascii_lowercase();
            assert!(!schema.contains("whitelist"), "{}: {schema}", tool["name"]);
        }
    }
    // Все имена агентских инструментов хаба — в пространствах presets и sessions.
    for (_, hub_name) in mcp::AGENT_TOOLS {
        assert!(
            hub_name.starts_with("presets.") || hub_name.starts_with("sessions."),
            "{hub_name}"
        );
    }
    assert_eq!(mcp::AGENT_TOOLS.len(), 13);
}

#[tokio::test]
async fn session_call_reaches_the_hub_and_returns_text_with_hint() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut broker = Broker::bind_with(dir.path(), ToolSet::LimitsOnly, "codex")
        .await
        .expect("bind");
    let stream = UnixStream::connect(dir.path().join("mcp.sock"))
        .await
        .expect("connect");
    let (read, mut sink) = stream.into_split();
    let mut socket = BufReader::new(read);
    let mut line = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
        "name":"session_send","arguments":{"session_id":"s","text":"привет"}}})
    .to_string();
    line.push('\n');
    sink.write_all(line.as_bytes()).await.expect("write");
    sink.flush().await.expect("flush");
    let (call_id, tool, arguments) =
        match tokio::time::timeout(Duration::from_secs(10), broker.next())
            .await
            .expect("событие брокера")
        {
            BrokerEvent::AgentTool {
                call_id,
                tool,
                arguments,
                ..
            } => (call_id, tool, arguments),
            other => panic!("ожидался AgentTool, получено {other:?}"),
        };
    assert_eq!(tool, "sessions.send");
    assert_eq!(arguments["session_id"], "s");
    broker
        .answer_agent_tool(
            call_id,
            &AgentCallResult::Ok(json!({"input_id": "i", "hint": "Queued."})),
        )
        .await;
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(10), socket.read_line(&mut reply))
        .await
        .expect("read timeout")
        .expect("read");
    let reply: Value = serde_json::from_str(&reply).expect("json line");
    assert_eq!(reply["id"], 5);
    assert!(reply["result"].get("isError").is_none(), "{reply}");
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.ends_with("\nQueued."), "{text}");
}

#[test]
fn share_file_is_listed_for_all_providers_with_a_strict_schema() {
    for (tool_set, provider) in PROVIDERS {
        let tools = tools_of(tool_set, provider);
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == "share_file")
            .unwrap_or_else(|| panic!("{provider}: нет share_file"));
        let description = tool["description"].as_str().expect("description");
        assert!(description.contains("owner"), "{description}");
        assert!(description.contains("50 MiB"), "{description}");
        assert!(description.contains("symlink"), "{description}");
        let required = tool["inputSchema"]["required"].as_array().unwrap();
        assert_eq!(required, &vec![json!("path")]);
        assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
        let instructions = instructions_of(tool_set, provider);
        let name = if provider == "opencode" {
            "canterlot_share_file"
        } else {
            "share_file"
        };
        assert!(instructions.contains(name), "{provider}: {instructions}");
    }
}

#[test]
fn share_file_arguments_are_checked_before_the_tool_runs() {
    assert!(mcp::valid_share_arguments(&json!({"path": "report.pdf"})));
    assert!(mcp::valid_share_arguments(
        &json!({"path": "a.png", "title": "Снимок", "note": "Смотри"})
    ));
    assert!(!mcp::valid_share_arguments(&json!({})));
    assert!(!mcp::valid_share_arguments(&json!({"path": 1})));
    assert!(!mcp::valid_share_arguments(
        &json!({"path": "a", "extra": true})
    ));
    assert!(!mcp::valid_share_arguments(
        &json!({"path": "a", "title": null})
    ));
}

#[test]
fn limits_report_falls_back_to_the_local_snapshot_on_an_old_hub() {
    use canterlot_worker::adapter::limits_fallback;
    let mut pending = std::collections::HashMap::new();
    let id = uuid::Uuid::now_v7();
    pending.insert(
        id,
        "Claude API budget: five-hour window 10% used.".to_owned(),
    );
    let local = limits_fallback(
        &mut pending,
        id,
        AgentCallResult::Err {
            code: "unknown_tool".into(),
            message: "old hub".into(),
        },
    );
    assert_eq!(
        local_value(&local),
        serde_json::json!({"text": "Claude API budget: five-hour window 10% used."})
    );
    let id = uuid::Uuid::now_v7();
    pending.insert(id, "local".to_owned());
    let report = limits_fallback(
        &mut pending,
        id,
        AgentCallResult::Ok(serde_json::json!({"text": "all providers"})),
    );
    assert_eq!(
        local_value(&report),
        serde_json::json!({"text": "all providers"})
    );
    assert!(pending.is_empty());
}

fn local_value(result: &AgentCallResult) -> Value {
    match result {
        AgentCallResult::Ok(value) => value.clone(),
        AgentCallResult::Err { .. } => panic!("expected ok"),
    }
}
