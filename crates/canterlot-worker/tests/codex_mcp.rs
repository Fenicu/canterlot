//! MCP canterlot для Codex (plan 2b Task 5): только canterlot_limits, launch -c, статус.

#![forbid(unsafe_code)]

use canterlot_proto::worker::v1::{LimitObservation, LimitWindow, LimitsSnapshot};
use canterlot_worker::adapter::codex::{
    mcp::{self, SERVER_INSTRUCTIONS, launch_overrides},
    thread::{ChangeNote, compose_developer_instructions, owner_defaults_from},
};
use canterlot_worker::adapter::{AdapterEvent, canterlot_mcp::ToolSet};
use serde_json::{Value, json};
use std::path::PathBuf;

#[test]
fn limits_only_tool_set_lists_only_canterlot_limits() {
    let list = canterlot_worker::adapter::canterlot_mcp::mcp::handle_rpc_with_provider(
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        ToolSet::LimitsOnly,
        "codex",
    );
    let tools = list.as_reply().expect("tools/list")["result"]["tools"]
        .as_array()
        .expect("tools");
    assert_eq!(tools.len(), 22);
    assert_eq!(tools[0]["name"], "canterlot_limits");
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"canterlot_presets_list"));
    assert!(names.contains(&"canterlot_preset_save"));
    assert!(names.contains(&"canterlot_preset_apply"));

    let claude = canterlot_worker::adapter::claude_code::mcp::handle_rpc(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }));
    let tools = claude.as_reply().expect("claude tools/list")["result"]["tools"]
        .as_array()
        .expect("tools");
    assert_eq!(tools.len(), 23);
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"approve"));
    assert!(names.contains(&"canterlot_limits"));
}

#[test]
fn launch_overrides_point_relay_at_exec_socket() {
    let td = tempfile::TempDir::new().expect("tempdir");
    let exec_dir = td.path().join("exec dir");
    let relay = PathBuf::from("/opt/bin/canterlot-worker");
    let parts: Vec<String> = launch_overrides(&relay, &exec_dir)
        .iter()
        .map(|o| o.to_string_lossy().into_owned())
        .collect();
    assert_eq!(parts[0], "-c");
    assert_eq!(parts[2], "-c");
    assert!(parts[1].starts_with("mcp_servers.canterlot.command="));
    assert!(parts[1].contains("canterlot-worker"));
    let sock = exec_dir.join("mcp.sock").to_string_lossy().into_owned();
    assert!(parts[3].contains("mcp-relay"));
    assert!(parts[3].contains(&sock));
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[4], "-c");
    assert_eq!(parts[5], "mcp_servers.canterlot.tool_timeout_sec=330");
}

#[test]
fn server_instructions_go_to_initialize_and_developer_instructions() {
    let init = mcp::handle_rpc(&json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {"protocolVersion": "2025-11-25"}
    }));
    let instructions = init.as_reply().expect("init")["result"]["instructions"]
        .as_str()
        .expect("instructions");
    assert_eq!(instructions, SERVER_INSTRUCTIONS);
    let lines: Vec<&str> = instructions.lines().collect();
    assert!((10..=15).contains(&lines.len()), "{instructions}");

    let owner = owner_defaults_from(&serde_json::json!({
        "config": {"developer_instructions": "Owner line."}, "origins": {},
    }));
    let dev = compose_developer_instructions(
        &canterlot_worker::adapter::InstructionSpec {
            mode: canterlot_worker::adapter::InstructionMode::Owner,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        },
        &owner,
        SERVER_INSTRUCTIONS,
        ChangeNote::None,
    );
    assert!(dev.starts_with("Owner line."));
    assert!(dev.ends_with(SERVER_INSTRUCTIONS));
}

#[test]
fn codex_instructions_do_not_describe_claude_tools() {
    assert!(!SERVER_INSTRUCTIONS.contains("Claude"));
    assert!(!SERVER_INSTRUCTIONS.contains("approve tool"));
    assert!(!SERVER_INSTRUCTIONS.contains("AskUserQuestion"));
    assert!(SERVER_INSTRUCTIONS.contains("canterlot_limits"));
    assert!(SERVER_INSTRUCTIONS.contains("canterlot_preset_apply"));
}

#[test]
fn failed_startup_status_is_an_optional_diagnostic() {
    let params = json!({"name": "canterlot", "status": "failed"});
    let event =
        mcp::startup_status_event("mcpServer/startupStatus/updated", &params).expect("diagnostic");
    match event {
        AdapterEvent::Diagnostic {
            kind,
            event_type,
            required,
            ..
        } => {
            assert_eq!(kind, "mcp_unavailable");
            assert_eq!(event_type, "mcp_unavailable");
            assert!(!required);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        mcp::startup_status_event(
            "mcpServer/startupStatus/updated",
            &json!({"name": "canterlot", "status": "ready"})
        )
        .is_none()
    );
    let fixture = include_str!("fixtures/codex/mcp_status.jsonl");
    for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
        let frame: Value = serde_json::from_str(line).expect("jsonl line");
        let params = frame.get("params").expect("params");
        let method = frame["method"].as_str().expect("method");
        match params["status"].as_str() {
            Some("failed") => assert!(mcp::startup_status_event(method, params).is_some()),
            Some("ready") => assert!(mcp::startup_status_event(method, params).is_none()),
            other => panic!("unexpected status {other:?}"),
        }
    }
}

fn observation(window: &str, used_permille: u32, resets_at_ms: u64) -> LimitObservation {
    LimitObservation {
        provider: "codex".into(),
        window: match window {
            "five_hour" => LimitWindow::FiveHour as i32,
            _ => LimitWindow::Weekly as i32,
        },
        used_permille,
        resets_at_ms,
        ..Default::default()
    }
}

#[test]
fn limits_tool_reports_codex_windows_or_no_data() {
    let text = mcp::limits_tool_text(
        &LimitsSnapshot {
            provider: "codex".into(),
            current: vec![
                observation("five_hour", 420, 1792919400000),
                observation("weekly", 810, 1793062800000),
            ],
            weekly_reserve_active: false,
        },
        "codex",
    );
    assert!(text.starts_with("Codex API budget:"));
    assert!(text.contains("42"));
    assert!(text.contains("81"));
    let empty = mcp::limits_tool_text(
        &LimitsSnapshot {
            provider: "codex".into(),
            ..Default::default()
        },
        "codex",
    );
    assert_eq!(empty, "no limit data yet in this session");
}

#[test]
fn limits_tool_omits_a_window_the_provider_does_not_have() {
    let text = mcp::limits_tool_text(
        &LimitsSnapshot {
            provider: "claude".into(),
            current: vec![observation("weekly", 540, 1793062800000)],
            weekly_reserve_active: false,
        },
        "claude",
    );
    assert!(text.contains("weekly window 54% used"), "{text}");
    assert!(!text.contains("five-hour"), "{text}");
}
