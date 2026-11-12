//! MCP canterlot для OpenCode: слой конфигурации и ожидание готовности.

use crate::adapter::canterlot_mcp;
use serde_json::{Value, json};
use std::path::Path;

pub const SERVER_NAME: &str = canterlot_mcp::SERVER_NAME;

/// Значение `OPENCODE_CONFIG_CONTENT`. `codemode: false` обязателен: иначе инструмент
/// доступен только изнутри `execute`. `relay_secret` уходит relay в окружении.
pub fn config_content(relay_exe: &Path, exec_dir: &Path, relay_secret: Option<&str>) -> String {
    let environment: serde_json::Map<String, Value> = relay_secret
        .map(|secret| (canterlot_mcp::RELAY_SECRET_ENV.to_owned(), secret.into()))
        .into_iter()
        .collect();
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
            "environment": environment,
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

/// Другие включённые серверы в ответе `GET /api/mcp`, кроме canterlot и `allowed` (оставленных
/// в копиях конфигурации): делегированная сессия их иметь не должна.
pub fn has_foreign_server(list: &Value, allowed: &[String]) -> bool {
    list.get("data")
        .and_then(Value::as_array)
        .is_some_and(|servers| {
            servers.iter().any(|server| {
                let name = server.get("name").and_then(Value::as_str);
                name != Some(SERVER_NAME)
                    && !name.is_some_and(|name| allowed.iter().any(|allowed| allowed == name))
                    && server.pointer("/status/status").and_then(Value::as_str) != Some("disabled")
            })
        })
}

/// Сам canterlot в списке: только такой ответ подтверждает, что список настоящий и уже собран.
/// Сразу после старта он бывает пуст, а `has_foreign_server` на незнакомой форме молчит.
pub fn lists_own_server(list: &Value) -> bool {
    list.get("data")
        .and_then(Value::as_array)
        .is_some_and(|servers| {
            servers
                .iter()
                .any(|server| server.get("name").and_then(Value::as_str) == Some(SERVER_NAME))
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
    fn only_a_list_with_canterlot_counts_as_proof() {
        let own = json!({"name": "canterlot", "status": {"status": "pending"}});
        assert!(lists_own_server(&json!({"data": [own]})));
        assert!(!lists_own_server(&json!({"data": []})));
        assert!(!lists_own_server(&json!({"data": {"canterlot": {}}})));
        assert!(!lists_own_server(&json!({"servers": [own]})));
        assert!(!lists_own_server(&json!([])));
    }

    fn has_foreign_server_none(list: &Value) -> bool {
        has_foreign_server(list, &[])
    }

    #[test]
    fn a_kept_delegate_server_is_not_foreign_but_anything_else_is() {
        let list = |entries: &[&str]| json!({"data": entries.iter().map(|name| json!({"name": name, "status": {"status": "connected"}})).collect::<Vec<_>>()});
        let kept = ["context7".to_owned()];
        assert!(!has_foreign_server(
            &list(&["canterlot", "context7"]),
            &kept
        ));
        assert!(has_foreign_server(&list(&["canterlot", "context7"]), &[]));
        assert!(has_foreign_server(
            &list(&["canterlot", "context7", "nanokvm"]),
            &kept
        ));
        // Запись без имени проверить нельзя: чужая.
        assert!(has_foreign_server(
            &json!({"data": [{"status": {"status": "connected"}}]}),
            &kept
        ));
    }

    #[test]
    fn foreign_enabled_servers_are_found_by_name() {
        let list = |entries: &[(&str, &str)]| json!({"data": entries.iter().map(|(name, status)| json!({"name": name, "status": {"status": status}})).collect::<Vec<_>>()});
        assert!(!has_foreign_server_none(&list(&[(
            "canterlot",
            "connected"
        )])));
        assert!(!has_foreign_server_none(&json!({"data": []})));
        assert!(has_foreign_server_none(&list(&[
            ("canterlot", "connected"),
            ("nanokvm", "pending")
        ])));
        assert!(has_foreign_server_none(&list(&[("nanokvm", "failed")])));
        assert!(!has_foreign_server_none(&list(&[
            ("canterlot", "connected"),
            ("nanokvm", "disabled")
        ])));
    }
}
