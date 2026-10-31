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

fn rule(action: &str, effect: &str) -> Value {
    json!({"action": action, "resource": "*", "effect": effect})
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
            Self::Ask => vec![
                rule("*", "ask"),
                rule("question", "allow"),
                rule("canterlot_canterlot_limits", "allow"),
                rule("todowrite", "allow"),
            ],
            // Список задан явно: всё, чего в нём нет, исчезает из инструментов модели.
            Self::ReadOnly => vec![
                rule("*", "deny"),
                rule("read", "allow"),
                rule("glob", "allow"),
                rule("grep", "allow"),
                rule("question", "allow"),
                rule("subagent", "allow"),
                rule("todowrite", "allow"),
                rule("canterlot_canterlot_limits", "allow"),
            ],
        };
        Some(Value::Array(rules))
    }

    /// Правила для `PATCH`: `null` сервер игнорирует, снять их можно только пустым списком.
    pub fn session_rules(self) -> Value {
        self.ruleset().unwrap_or_else(|| Value::Array(Vec::new()))
    }
}
