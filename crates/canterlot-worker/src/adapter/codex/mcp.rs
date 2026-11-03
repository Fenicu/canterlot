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

/// Секрет сокета для relay: `env` сервера canterlot. Значение видно в командной строке,
/// но читать её может только тот, кто и так читает окружение relay.
pub fn secret_override(secret: &str) -> [OsString; 2] {
    let mut env = toml::map::Map::new();
    env.insert(
        canterlot_mcp::RELAY_SECRET_ENV.to_owned(),
        toml::Value::String(secret.to_owned()),
    );
    [
        "-c".into(),
        format!("mcp_servers.canterlot.env={}", toml::Value::Table(env)).into(),
    ]
}

/// Действующий конфиг делегированной сессии (`config/read`): серверов нет, кроме canterlot, а
/// приложения аккаунта и плагины выключены. Сверка до `thread/start`, ничего не запуская.
pub fn isolated_config(response: &Value) -> bool {
    let config = &response["config"];
    let servers_ok = match config.get("mcp_servers") {
        None | Some(Value::Null) => true,
        Some(Value::Object(servers)) => servers.iter().all(|(name, server)| {
            name == SERVER_NAME || server.get("enabled").and_then(Value::as_bool) == Some(false)
        }),
        Some(_) => false,
    };
    let off = |feature: &str| config["features"][feature].as_bool() == Some(false);
    servers_ok && off("apps") && off("plugins")
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

#[cfg(test)]
mod isolation_tests {
    use super::*;
    use serde_json::json;

    fn config(servers: Value, apps: Value, plugins: Value) -> Value {
        json!({"config": {"mcp_servers": servers, "features": {"apps": apps, "plugins": plugins}}})
    }

    #[test]
    fn only_canterlot_with_apps_and_plugins_off_passes() {
        let canterlot = json!({"canterlot": {"command": "x", "enabled": true}});
        assert!(isolated_config(&config(
            canterlot.clone(),
            json!(false),
            json!(false)
        )));
        assert!(isolated_config(&config(
            json!({}),
            json!(false),
            json!(false)
        )));
        assert!(!isolated_config(&config(
            canterlot.clone(),
            json!(true),
            json!(false)
        )));
        assert!(!isolated_config(&config(
            canterlot.clone(),
            json!(false),
            json!(true)
        )));
        // Нет сведений - не доказано: закрыто.
        assert!(!isolated_config(&config(
            canterlot,
            Value::Null,
            json!(false)
        )));
        assert!(!isolated_config(&json!({"config": {"mcp_servers": {}}})));
        assert!(!isolated_config(&json!({})));
    }

    #[test]
    fn any_other_enabled_server_fails_the_start() {
        let other = json!({"canterlot": {"enabled": true}, "nanokvm": {"command": "x"}});
        assert!(!isolated_config(&config(other, json!(false), json!(false))));
        let off = json!({"canterlot": {"enabled": true}, "nanokvm": {"enabled": false}});
        assert!(isolated_config(&config(off, json!(false), json!(false))));
    }
}
