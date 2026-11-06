//! Режимы canterlot как правила сессии OpenCode. У агента `build` разрешено всё,
//! поэтому `ask` и `read_only` существуют только благодаря этим правилам.

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Owner,
    Ask,
    Bypass,
    ReadOnly,
}

/// Чтение через MCP canterlot и будильники: они касаются только своей очереди,
/// поэтому разрешены и в `ask`, и в `read_only`. Сохранение и применение пресета решает режим.
const READ_TOOLS: [&str; 11] = [
    "canterlot_canterlot_limits",
    "canterlot_canterlot_presets_list",
    "canterlot_sessions_list",
    "canterlot_session_inspect",
    // Подписка и ожидание ничего не меняют у другой сессии; права на них проверяет хаб.
    "canterlot_session_subscribe",
    "canterlot_session_unsubscribe",
    "canterlot_session_wait",
    "canterlot_alarm_set",
    "canterlot_alarm_cancel",
    "canterlot_alarm_list",
    "canterlot_route_preview",
];

fn rule(action: &str, effect: &str) -> Value {
    json!({"action": action, "resource": "*", "effect": effect})
}

fn with_read_tools(mut rules: Vec<Value>) -> Vec<Value> {
    rules.extend(READ_TOOLS.iter().map(|tool| rule(tool, "allow")));
    rules
}

impl PermissionMode {
    pub fn parse(mode: &str) -> Option<Self> {
        match mode {
            "owner" => Some(Self::Owner),
            "ask" => Some(Self::Ask),
            "bypass" => Some(Self::Bypass),
            "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }

    /// `None` у режима владельца: действуют правила его конфигурации.
    pub fn ruleset(self) -> Option<Value> {
        let rules = match self {
            Self::Owner => return None,
            Self::Bypass => vec![rule("*", "allow")],
            Self::Ask => with_read_tools(vec![
                rule("*", "ask"),
                rule("question", "allow"),
                rule("todowrite", "allow"),
            ]),
            // Список задан явно: всё, чего в нём нет, исчезает из инструментов модели.
            Self::ReadOnly => with_read_tools(vec![
                rule("*", "deny"),
                rule("read", "allow"),
                rule("glob", "allow"),
                rule("grep", "allow"),
                rule("question", "allow"),
                rule("subagent", "allow"),
                rule("todowrite", "allow"),
            ]),
        };
        Some(Value::Array(rules))
    }

    /// Правила для `PATCH`: `null` сервер игнорирует, снять их можно только пустым списком.
    pub fn session_rules(self) -> Value {
        self.ruleset().unwrap_or_else(|| Value::Array(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(mode: PermissionMode, tool: &str) -> bool {
        mode.ruleset()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["action"] == tool && rule["effect"] == "allow")
    }

    #[test]
    fn route_preview_is_allowed_in_ask_and_read_only() {
        for mode in [PermissionMode::Ask, PermissionMode::ReadOnly] {
            assert!(allowed(mode, "canterlot_route_preview"), "{mode:?}");
        }
    }

    fn effect(mode: PermissionMode, tool: &str) -> Option<String> {
        // Действует последнее совпавшее правило.
        mode.ruleset()?
            .as_array()?
            .iter()
            .rev()
            .find(|rule| rule["action"] == tool || rule["action"] == "*")
            .and_then(|rule| rule["effect"].as_str())
            .map(str::to_owned)
    }

    #[test]
    fn spawn_cancel_is_a_card_in_ask_and_forbidden_in_read_only() {
        assert_eq!(
            effect(PermissionMode::Ask, "canterlot_spawn_cancel").as_deref(),
            Some("ask")
        );
        assert_eq!(
            effect(PermissionMode::ReadOnly, "canterlot_spawn_cancel").as_deref(),
            Some("deny")
        );
        assert_eq!(
            effect(PermissionMode::Bypass, "canterlot_spawn_cancel").as_deref(),
            Some("allow")
        );
    }

    #[test]
    fn alarms_are_allowed_in_ask_and_read_only() {
        for mode in [PermissionMode::Ask, PermissionMode::ReadOnly] {
            for tool in [
                "canterlot_alarm_set",
                "canterlot_alarm_cancel",
                "canterlot_alarm_list",
            ] {
                assert!(allowed(mode, tool), "{mode:?} {tool}");
            }
        }
    }
}
