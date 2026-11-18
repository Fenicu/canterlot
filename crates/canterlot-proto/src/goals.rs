//! Общая модель цели исполнения: адаптер → сопровождение → журнал → хаб → PWA.
//!
//! Адаптеры Codex и Claude Code сообщают одно и то же состояние, хаб хранит последнюю
//! запись по разговору и рассылает её владельцу. `updated_at_ms` — миллисекунды UNIX;
//! хаб переводит их в RFC 3339 для API и снимков.

use serde::{Deserialize, Serialize};

/// Возможность воркера и машины управлять целями. Без неё хаб отказывает командам.
pub const CAPABILITY_GOALS_V1: &str = "goals_v1";

/// Максимальная длина формулировки цели, байты UTF-8.
pub const MAX_OBJECTIVE_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "blocked" => Some(Self::Blocked),
            "usage_limited" | "usageLimited" => Some(Self::UsageLimited),
            "budget_limited" | "budgetLimited" => Some(Self::BudgetLimited),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    pub objective: String,
    pub status: GoalStatus,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub token_budget: Option<i64>,
    /// Ходы, потраченные на цель; Claude Code сообщает их число, Codex — нет.
    pub turns_used: Option<i64>,
    pub updated_at_ms: i64,
}

impl GoalState {
    /// Отсекает формулировку до безопасной длины: текст цели — данные владельца.
    pub fn bounded(mut self) -> Self {
        if self.objective.len() > MAX_OBJECTIVE_BYTES {
            let mut end = MAX_OBJECTIVE_BYTES;
            while end > 0 && !self.objective.is_char_boundary(end) {
                end -= 1;
            }
            self.objective.truncate(end);
        }
        self.tokens_used = self.tokens_used.clamp(0, i64::MAX);
        self.time_used_seconds = self.time_used_seconds.clamp(0, i64::MAX);
        self.turns_used = self.turns_used.map(|v| v.clamp(0, i64::MAX));
        self.updated_at_ms = self.updated_at_ms.clamp(0, i64::MAX);
        self
    }
}

/// Действие владельца с целью. `pause` и `resume` поддерживает Codex; Claude Code
/// отвечает `unsupported` на эти действия.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAction {
    Set,
    Clear,
    Pause,
    Resume,
}

impl GoalAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Clear => "clear",
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "set" => Some(Self::Set),
            "clear" => Some(Self::Clear),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            _ => None,
        }
    }
}
