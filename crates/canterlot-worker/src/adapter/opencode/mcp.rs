//! MCP canterlot для OpenCode: слой конфигурации и ожидание готовности.

use crate::adapter::canterlot_mcp;
use serde_json::{Value, json};
use std::path::Path;

pub const SERVER_NAME: &str = canterlot_mcp::SERVER_NAME;

/// Значение `OPENCODE_CONFIG_CONTENT`. `codemode: false` обязателен: иначе инструмент
/// доступен только изнутри `execute`.
pub fn config_content(relay_exe: &Path, exec_dir: &Path) -> String {
    json!({
        "update": "disable",
        "share": "disabled",
        "snapshots": false,
        "experimental": {"todo": true},
        "mcp": {"servers": {SERVER_NAME: {
            "type": "local",
            "command": [
                relay_exe.to_string_lossy(),
                "mcp-relay",
                "--socket",
                exec_dir.join("mcp.sock").to_string_lossy(),
            ],
            "environment": {},
            "codemode": false,
        }}},
    })
    .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpState {
    Pending,
    Connected,
    Failed,
}

/// Статус canterlot в ответе `GET /api/mcp`; сразу после старта списка может не быть.
pub fn state_of(list: &Value) -> McpState {
    let status = list
        .get("data")
        .and_then(Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("name").and_then(Value::as_str) == Some(SERVER_NAME))
        })
        .and_then(|server| server.pointer("/status/status"))
        .and_then(Value::as_str);
    match status {
        Some("connected") => McpState::Connected,
        Some("pending") | None => McpState::Pending,
        Some(_) => McpState::Failed,
    }
}

/// Другие включённые серверы в ответе `GET /api/mcp`: делегированная сессия их иметь не должна.
pub fn has_foreign_server(list: &Value) -> bool {
    list.get("data")
        .and_then(Value::as_array)
        .is_some_and(|servers| {
            servers.iter().any(|server| {
                server.get("name").and_then(Value::as_str) != Some(SERVER_NAME)
                    && server.pointer("/status/status").and_then(Value::as_str) != Some("disabled")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_of_canterlot_is_found_by_name() {
        let list = |name: &str, status: &str| json!({"data": [{"name": name, "status": {"status": status}}]});
        assert_eq!(
            state_of(&list("canterlot", "connected")),
            McpState::Connected
        );
        assert_eq!(state_of(&list("canterlot", "pending")), McpState::Pending);
        assert_eq!(state_of(&list("canterlot", "failed")), McpState::Failed);
        assert_eq!(state_of(&list("canterlot", "needs_auth")), McpState::Failed);
        assert_eq!(state_of(&list("other", "connected")), McpState::Pending);
        assert_eq!(state_of(&json!({"data": []})), McpState::Pending);
    }

    #[test]
    fn foreign_enabled_servers_are_found_by_name() {
        let list = |entries: &[(&str, &str)]| json!({"data": entries.iter().map(|(name, status)| json!({"name": name, "status": {"status": status}})).collect::<Vec<_>>()});
        assert!(!has_foreign_server(&list(&[("canterlot", "connected")])));
        assert!(!has_foreign_server(&json!({"data": []})));
        assert!(has_foreign_server(&list(&[
            ("canterlot", "connected"),
            ("nanokvm", "pending")
        ])));
        assert!(has_foreign_server(&list(&[("nanokvm", "failed")])));
        assert!(!has_foreign_server(&list(&[
            ("canterlot", "connected"),
            ("nanokvm", "disabled")
        ])));
    }
}
