//! MCP canterlot для Codex: флаги запуска, инструкции, статус сервера.

use std::{ffi::OsString, path::Path};

use crate::adapter::{AdapterEvent, canterlot_mcp};
use serde_json::Value;

pub use canterlot_mcp::{
    CODEX_SERVER_INSTRUCTIONS as SERVER_INSTRUCTIONS, ToolSet, limits_tool_text,
};

pub const SERVER_NAME: &str = canterlot_mcp::SERVER_NAME;

const TOOL_TIMEOUT_SEC: u32 = 330;

/// Шесть аргументов `-c mcp_servers.canterlot.*` для `codex app-server`.
/// Таймаут вызова 330 с покрывает `session_wait` (до 300 с) с запасом.
pub fn launch_overrides(relay_exe: &Path, exec_dir: &Path) -> [OsString; 6] {
    [
        "-c".into(),
        format!(
            "mcp_servers.canterlot.command={}",
            super::process::mcp_command_value(relay_exe)
        )
        .into(),
        "-c".into(),
        format!(
            "mcp_servers.canterlot.args={}",
            super::process::mcp_args_value(exec_dir)
        )
        .into(),
        "-c".into(),
        format!("mcp_servers.canterlot.tool_timeout_sec={TOOL_TIMEOUT_SEC}").into(),
    ]
}

/// Уведомление `mcpServer/startupStatus/updated`: сбой canterlot → необязательная диагностика.
pub fn startup_status_event(method: &str, params: &Value) -> Option<AdapterEvent> {
    if method != "mcpServer/startupStatus/updated" {
        return None;
    }
    if params.get("name").and_then(Value::as_str) != Some(SERVER_NAME) {
        return None;
    }
    if params.get("status").and_then(Value::as_str) == Some("failed") {
        return Some(AdapterEvent::Diagnostic {
            kind: "mcp_unavailable",
            event_type: "mcp_unavailable".into(),
            required: false,
            field_names: Vec::new(),
        });
    }
    None
}

pub fn handle_rpc(msg: &Value) -> canterlot_mcp::mcp::McpAction {
    canterlot_mcp::mcp::handle_rpc_with_provider(msg, ToolSet::LimitsOnly, "codex")
}
