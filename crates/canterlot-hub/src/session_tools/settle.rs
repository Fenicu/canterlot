//! «Осевшая» сессия: состояние, о котором стоит сказать подписчику. Чистые правила без базы.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

use crate::{
    activity::{Activity, ActivityState, Evidence, QueueFacts, Thresholds},
    config::ActivityConfig,
    model::RunSlot,
};

/// Фоновая работа, молчащая дольше этого, больше не держит сессию.
const BACKGROUND_SILENCE: TimeDelta = TimeDelta::minutes(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    Idle,
    WaitingForUser,
    Stalled,
    QuotaPaused,
    Failed,
    Stopped,
}

impl Settled {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::WaitingForUser => "waiting_for_user",
            Self::Stalled => "stalled",
            Self::QuotaPaused => "quota_paused",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    /// Подписка закрывается: продолжение возможно только после нового ввода. Ввод, уже стоящий
    /// в очереди, и есть продолжение: он выполнится, когда владелец вернёт сессию.
    pub fn closes_subscription(self, queued: u32) -> bool {
        queued == 0 && matches!(self, Self::Idle | Self::Failed | Self::Stopped)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SettleFacts {
    pub now: DateTime<Utc>,
    pub since: DateTime<Utc>,
    pub last_turn_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SettleConfig {
    pub idle_quiet: Duration,
}

impl From<&ActivityConfig> for SettleConfig {
    fn from(config: &ActivityConfig) -> Self {
        Self {
            idle_quiet: config.idle_quiet,
        }
    }
}

impl From<&Thresholds> for SettleConfig {
    fn from(thresholds: &Thresholds) -> Self {
        Self {
            idle_quiet: thresholds.idle_quiet,
        }
    }
}

pub fn settled(
    a: &Activity,
    q: &QueueFacts,
    f: &SettleFacts,
    cfg: &SettleConfig,
) -> Option<Settled> {
    match a.state {
        ActivityState::Working => None,
        ActivityState::WaitingForUser => Some(Settled::WaitingForUser),
        ActivityState::Stalled => Some(Settled::Stalled),
        ActivityState::QuotaPaused => Some(Settled::QuotaPaused),
        ActivityState::Failed => Some(Settled::Failed),
        ActivityState::Stopped => Some(Settled::Stopped),
        // `sessions.wait` читает проекцию, которая отстаёт от очереди: занятый слот и есть работа.
        ActivityState::Idle if !matches!(q.slot, RunSlot::Free) => None,
        ActivityState::Idle if q.queued > 0 => {
            (!q.pause_reasons.is_empty()).then_some(Settled::WaitingForUser)
        }
        ActivityState::Idle => {
            let background = match a.evidence {
                Evidence::Idle {
                    background_work, ..
                } => background_work.unwrap_or(0),
                _ => 0,
            };
            let background_alive = f
                .last_event_at
                .is_some_and(|at| f.now - at < BACKGROUND_SILENCE);
            if background > 0 && background_alive {
                return None;
            }
            // Ход пробуждения начинается сразу за концом фона: от него тоже отсчитывается тишина.
            let quiet_from = [f.last_turn_at, f.last_event_at]
                .into_iter()
                .flatten()
                .fold(f.since, DateTime::max);
            // Время из будущего считается окном тишины: событие не уходит раньше срока.
            let quiet = (f.now - quiet_from)
                .to_std()
                .is_ok_and(|elapsed| elapsed >= cfg.idle_quiet);
            quiet.then_some(Settled::Idle)
        }
    }
}

/// База хранит микросекунды: точнее сравнивать нельзя, иначе отпечаток разойдётся с прочитанным.
pub fn fingerprint(
    state: Settled,
    since: DateTime<Utc>,
    last_turn_at: Option<DateTime<Utc>>,
) -> String {
    let at = |value: DateTime<Utc>| value.to_rfc3339_opts(SecondsFormat::Micros, true);
    format!(
        "{}|{}|{}",
        state.as_str(),
        at(since),
        last_turn_at.map(at).unwrap_or_default()
    )
}
