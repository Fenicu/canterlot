//! «Осевшая» сессия: состояние, о котором стоит сказать подписчику. Чистые правила без базы.

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

use crate::{
    activity::{Activity, ActivityState, Evidence, OWNER_LOST, QueueFacts, Thresholds},
    config::ActivityConfig,
    model::RunSlot,
};

/// Фоновая работа, молчащая дольше этого, больше не держит сессию.
const BACKGROUND_SILENCE: TimeDelta = TimeDelta::minutes(30);

/// Порог `[activity].idle_background_max` по умолчанию.
pub const DEFAULT_IDLE_BACKGROUND_MAX: Duration = Duration::from_secs(15 * 60);

/// Вид события подписки «простаивает с фоном»: не итог, а отдельный сигнал.
pub const IDLE_BACKGROUND: &str = "idle_background";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    Idle,
    WaitingForUser,
    Stalled,
    QuotaPaused,
    Failed,
    /// Для подписчика это `failed`, но конца ещё нет: воркер может вернуть исполнение.
    OwnerLost,
    Stopped,
}

impl Settled {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::WaitingForUser => "waiting_for_user",
            Self::Stalled => "stalled",
            Self::QuotaPaused => "quota_paused",
            Self::Failed | Self::OwnerLost => "failed",
            Self::Stopped => "stopped",
        }
    }

    /// Выход из `owner_lost` в `failed` не меняет ни имя состояния, ни `since`:
    /// отпечаток различает их сам, иначе итог завершения не дошёл бы до подписчика.
    fn fingerprint_name(self) -> &'static str {
        match self {
            Self::OwnerLost => OWNER_LOST,
            other => other.as_str(),
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
    /// Начало текущей фазы исполнения: для `stalled` отпечаток держится на ней,
    /// чтобы дребезг состояния не повторял одно и то же подозрение.
    pub phase_since: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SettleConfig {
    pub idle_quiet: Duration,
    /// `[activity].idle_background_max`: сколько `idle` с работающим фоном ждёт до предупреждения.
    pub idle_background_max: Duration,
    /// Выключатель `[orchestrator.nudge]`: шлюз автопинка не удерживает итоги.
    pub nudge_enabled: bool,
}

impl Default for SettleConfig {
    fn default() -> Self {
        Self {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: DEFAULT_IDLE_BACKGROUND_MAX,
            nudge_enabled: true,
        }
    }
}

impl From<&ActivityConfig> for SettleConfig {
    fn from(config: &ActivityConfig) -> Self {
        Self {
            idle_quiet: config.idle_quiet,
            idle_background_max: config.idle_background_max,
            nudge_enabled: true,
        }
    }
}

impl From<&Thresholds> for SettleConfig {
    fn from(thresholds: &Thresholds) -> Self {
        Self {
            idle_quiet: thresholds.idle_quiet,
            idle_background_max: thresholds.idle_background_max,
            nudge_enabled: true,
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
        ActivityState::Failed => Some(match &a.evidence {
            Evidence::Failed { reason } if reason == OWNER_LOST => Settled::OwnerLost,
            _ => Settled::Failed,
        }),
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

/// Сессия ждёт ввода, а держит её от итога только работающая фоновая задача.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleBackground {
    pub jobs: u32,
    /// Начало эпизода простоя: позднее из начала `idle` и конца последнего хода.
    pub idle_since: DateTime<Utc>,
}

/// `idle` с живой фоновой работой не осел: агент, ждущий свою сборку, ещё не закончил.
/// Но фон не будит сессию Claude Code, и `tail -F` может не кончиться никогда. Если такой
/// простой длится дольше `idle_background_max`, подписчик должен об этом услышать —
/// отдельным сигналом, а не итогом. Время считается от начала простоя, а не от последнего
/// события: вывод фоновой задачи освежает события, но работой сессии не является.
pub fn idle_background(
    a: &Activity,
    q: &QueueFacts,
    f: &SettleFacts,
    cfg: &SettleConfig,
) -> Option<IdleBackground> {
    if a.state != ActivityState::Idle
        || !matches!(q.slot, RunSlot::Free)
        || q.queued > 0
        || settled(a, q, f, cfg).is_some()
    {
        return None;
    }
    let jobs = match a.evidence {
        Evidence::Idle {
            background_work: Some(jobs),
            ..
        } if jobs > 0 => jobs,
        _ => return None,
    };
    let idle_since = f.last_turn_at.map_or(f.since, |at| at.max(f.since));
    let long = (f.now - idle_since)
        .to_std()
        .is_ok_and(|elapsed| elapsed >= cfg.idle_background_max);
    long.then_some(IdleBackground { jobs, idle_since })
}

/// Один сигнал на эпизод простоя: новый ход меняет и `since`, и конец последнего хода.
/// Отпечаток не совпадает с отпечатком `idle`, поэтому настоящий итог потом дойдёт тоже.
pub fn idle_background_fingerprint(
    since: DateTime<Utc>,
    last_turn_at: Option<DateTime<Utc>>,
) -> String {
    let at = |value: DateTime<Utc>| value.to_rfc3339_opts(SecondsFormat::Micros, true);
    format!(
        "{IDLE_BACKGROUND}|{}|{}",
        at(since),
        last_turn_at.map(at).unwrap_or_default()
    )
}

/// База хранит микросекунды: точнее сравнивать нельзя, иначе отпечаток разойдётся с прочитанным.
/// У `stalled` опорное время — начало фазы, а не `since`: возврат из короткого пробуждения
/// в ту же фазу не должен считаться новым эпизодом.
pub fn fingerprint(
    state: Settled,
    since: DateTime<Utc>,
    last_turn_at: Option<DateTime<Utc>>,
    phase_since: Option<DateTime<Utc>>,
) -> String {
    let at = |value: DateTime<Utc>| value.to_rfc3339_opts(SecondsFormat::Micros, true);
    let mark = match state {
        Settled::Stalled => phase_since.unwrap_or(since),
        _ => since,
    };
    format!(
        "{}|{}|{}",
        state.fingerprint_name(),
        at(mark),
        last_turn_at.map(at).unwrap_or_default()
    )
}
