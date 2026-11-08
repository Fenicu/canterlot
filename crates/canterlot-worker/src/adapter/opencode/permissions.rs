//! Режимы canterlot как правила сессии OpenCode. У агента `build` разрешено всё,
//! поэтому `ask` и `read_only` существуют только благодаря этим правилам.

use serde_json::{Value, json};

use crate::adapter::canterlot_mcp::MODE_FREE_TOOLS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Owner,
    Ask,
    Bypass,
    ReadOnly,
}

/// Инструменты canterlot без вопроса в `ask` и `read_only`: общий список
/// [`MODE_FREE_TOOLS`] с префиксом сервера `canterlot_`.
fn read_tools() -> impl Iterator<Item = String> {
    MODE_FREE_TOOLS
        .iter()
        .map(|tool| format!("canterlot_{tool}"))
}

fn rule(action: &str, effect: &str) -> Value {
    json!({"action": action, "resource": "*", "effect": effect})
}

fn with_read_tools(mut rules: Vec<Value>) -> Vec<Value> {
    rules.extend(read_tools().map(|tool| rule(&tool, "allow")));
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

    #[test]
    fn own_plan_tools_are_allowed_and_adopt_follows_the_mode() {
        for mode in [PermissionMode::Ask, PermissionMode::ReadOnly] {
            for tool in [
                "canterlot_plan_list",
                "canterlot_plan_add",
                "canterlot_plan_update",
            ] {
                assert!(allowed(mode, tool), "{mode:?} {tool}");
            }
        }
        assert_eq!(
            effect(PermissionMode::Ask, "canterlot_plan_adopt").as_deref(),
            Some("ask")
        );
        assert_eq!(
            effect(PermissionMode::ReadOnly, "canterlot_plan_adopt").as_deref(),
            Some("deny")
        );
    }
}
