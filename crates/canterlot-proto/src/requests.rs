use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionPayload {
    Permission {
        behavior: PermissionBehavior,
        message: Option<String>,
    },
    Question {
        // Ответы привязаны к идентификаторам вопросов, а не к их тексту.
        answers: BTreeMap<String, AnswerValue>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum AnswerValue {
    Single(String),
    Multiple(Vec<String>),
}

impl fmt::Debug for DecisionPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission { behavior, .. } => f
                .debug_struct("Permission")
                .field("behavior", behavior)
                .field("message", &"[REDACTED]")
                .finish(),
            Self::Question { .. } => f
                .debug_struct("Question")
                .field("answers", &"[REDACTED]")
                .finish(),
        }
    }
}

impl fmt::Debug for AnswerValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
