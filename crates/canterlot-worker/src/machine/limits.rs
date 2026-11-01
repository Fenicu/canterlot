use canterlot_proto::worker::v1::{LimitAbsence, LimitObservation};
use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

use crate::adapter::codex::{limits::observations_from_read_response, probe};
use crate::config::CodexConfig;

const FIRST_TICK_MAX_DELAY: Duration = Duration::from_secs(60);
const CODEX_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub type HubLimitsAge = Arc<dyn Fn(&str) -> Option<Instant> + Send + Sync>;

/// Результат опроса: показания окон и окна, которых у провайдера нет.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimitsFetch {
    pub observations: Vec<LimitObservation>,
    pub absent: Vec<LimitAbsence>,
}

#[async_trait::async_trait]
pub trait LimitsSource: Send + Sync {
    /// Запрос хаба (`PollLimits`, проверка пробуждения «кранча»).
    async fn poll(&self, provider: &str) -> Vec<LimitObservation>;
    /// Тик опроса без живых сессий; источник со своим интервалом простоя переопределяет его.
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll(provider).await
    }
    /// Опрос вместе с отсутствующими окнами; источники без такой возможности отдают только показания.
    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        LimitsFetch {
            observations: if idle {
                self.poll_idle(provider).await
            } else {
                self.poll(provider).await
            },
            absent: Vec::new(),
        }
    }
}
pub struct NoLimitsSource;
#[async_trait::async_trait]
impl LimitsSource for NoLimitsSource {
    async fn poll(&self, _provider: &str) -> Vec<LimitObservation> {
        Vec::new()
    }
}

#[cfg(feature = "synthetic-adapter")]
pub struct FileLimitsSource {
    data_dir: std::path::PathBuf,
}
#[cfg(feature = "synthetic-adapter")]
impl FileLimitsSource {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self { data_dir }
    }
}
#[cfg(feature = "synthetic-adapter")]
#[async_trait::async_trait]
impl LimitsSource for FileLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        use canterlot_proto::worker::v1::LimitWindow;
        if !matches!(provider, "claude" | "codex") {
            return Vec::new();
        }
        let path = self
            .data_dir
            .join("synthetic-limits")
            .join(format!("{provider}.json"));
        let Ok(file) = tokio::fs::File::open(path).await else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        use tokio::io::AsyncReadExt;
        if file.take(65537).read_to_end(&mut bytes).await.is_err() || bytes.len() > 65536 {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Vec::new();
        };
        let at = super::now_ms();
        [
            ("five_hour", LimitWindow::FiveHour),
            ("weekly", LimitWindow::Weekly),
        ]
        .into_iter()
        .filter_map(|(key, window)| {
            let row = value.get(key)?;
            let used = row["used_permille"].as_u64().filter(|v| *v <= 1000)?;
            Some(LimitObservation {
                provider: provider.into(),
                window: window as i32,
                used_permille: used as u32,
                resets_at_ms: row["resets_at_ms"].as_u64().unwrap_or(0),
                observed_machine_ms: at,
            })
        })
        .collect()
    }
}

/// Время последнего показания лимитов, пришедшего от живого исполнения, по провайдеру.
/// Воркер отмечает его, пересылая записи `LimitObserved` из SJ.
#[derive(Clone, Default)]
pub struct LiveLimits(Arc<Mutex<HashMap<String, Instant>>>);

impl LiveLimits {
    pub fn note(&self, provider: &str, at: Instant) {
        let mut seen = self.0.lock().expect("live limits lock");
        let last = seen.entry(provider.to_owned()).or_insert(at);
        *last = (*last).max(at);
    }

    /// Возраст берётся из машинного времени записи: повторная пересылка старых записей
    /// не делает показание свежим.
    pub fn note_observation(&self, observation: &LimitObservation) {
        let age = super::now_ms().saturating_sub(observation.observed_machine_ms);
        if observation.observed_machine_ms > 0
            && let Some(at) = Instant::now().checked_sub(Duration::from_millis(age))
        {
            self.note(&observation.provider, at);
        }
    }

    pub fn last(&self, provider: &str) -> Option<Instant> {
        self.0
            .lock()
            .expect("live limits lock")
            .get(provider)
            .copied()
    }
}

/// Общий тикер опроса лимитов без живых сессий: каждый провайдер со своим интервалом,
/// первый тик не позже чем через минуту после регистрации: при перезапусках чаще
/// интервала иначе не дошло бы до первого опроса. Решение, опрашивать ли на самом деле,
/// остаётся за источником провайдера.
#[derive(Debug, Default)]
pub struct IdleTicker {
    entries: Vec<(String, Duration, Instant)>,
}

impl IdleTicker {
    pub fn register(&mut self, provider: &str, interval: Duration) {
        self.entries.push((
            provider.to_owned(),
            interval,
            Instant::now() + interval.min(FIRST_TICK_MAX_DELAY),
        ));
    }

    /// Ждёт ближайшего срока и возвращает провайдеров, чей срок наступил; без записей не завершается.
    pub async fn tick(&mut self) -> Vec<String> {
        let Some(due) = self.entries.iter().map(|(_, _, due)| *due).min() else {
            return std::future::pending().await;
        };
        tokio::time::sleep_until(due).await;
        let now = Instant::now();
        self.entries
            .iter_mut()
            .filter(|(_, _, due)| *due <= now)
            .map(|(provider, interval, due)| {
                *due = now + *interval;
                provider.clone()
            })
            .collect()
    }
}

const HUB_POLL_DEBOUNCE: Duration = Duration::from_secs(60);

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct PollState {
    last_request: Option<Instant>,
    backoff_until: Option<Instant>,
    rate_limited: u32,
}

pub(super) fn rfc3339_ms(text: &str) -> Option<u64> {
    use toml::value::{Datetime, Offset};
    let datetime: Datetime = text.parse().ok()?;
    let date = datetime.date?;
    let time = datetime.time?;
    let offset_minutes = match datetime.offset? {
        Offset::Z => 0,
        Offset::Custom { minutes } => i64::from(minutes),
    };
    let (year, month, day) = (
        i64::from(date.year),
        i64::from(date.month),
        i64::from(date.day),
    );
    // Дни от 1970-01-01 по пролептическому григорианскому календарю (Howard Hinnant).
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400
        + i64::from(time.hour) * 3600
        + i64::from(time.minute) * 60
        + i64::from(time.second.unwrap_or(0))
        - offset_minutes * 60;
    let millis = seconds
        .checked_mul(1000)?
        .checked_add(i64::from(time.nanosecond.unwrap_or(0) / 1_000_000))?;
    u64::try_from(millis).ok()
}

/// Опрос лимитов Codex без живой сессии: короткий `account/rateLimits/read` через probe.
pub struct CodexRateLimitsSource {
    executable: PathBuf,
    interval: Duration,
    live: LiveLimits,
    hub_age: HubLimitsAge,
    clock: Clock,
    state: Mutex<PollState>,
}

impl fmt::Debug for CodexRateLimitsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexRateLimitsSource")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl CodexRateLimitsSource {
    pub fn new(config: &CodexConfig, live: LiveLimits, hub_age: HubLimitsAge) -> Self {
        Self {
            executable: config.executable.clone(),
            interval: config.rate_limits_poll_interval(),
            live,
            hub_age,
            clock: Arc::new(Instant::now),
            state: Mutex::default(),
        }
    }

    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = executable;
        self
    }

    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub async fn fetch(&self) -> Result<Vec<LimitObservation>, probe::ProbeError> {
        let response = probe::probe(
            &self.executable,
            "account/rateLimits/read",
            serde_json::json!({}),
            CODEX_PROBE_TIMEOUT,
        )
        .await?;
        Ok(observations_from_read_response(&response, super::now_ms()))
    }
}

#[async_trait::async_trait]
impl LimitsSource for CodexRateLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, false).await
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, true).await
    }
}

impl CodexRateLimitsSource {
    async fn poll_with(&self, provider: &str, idle: bool) -> Vec<LimitObservation> {
        if provider != "codex" {
            return Vec::new();
        }
        let now = (self.clock)();
        {
            let mut state = self.state.lock().expect("codex poll state");
            let since = |at: Option<Instant>, gap: Duration| {
                at.is_some_and(|at| now.saturating_duration_since(at) < gap)
            };
            if state.backoff_until.is_some_and(|until| now < until)
                || since(state.last_request, HUB_POLL_DEBOUNCE)
                || (idle
                    && (since(state.last_request, self.interval)
                        || since(self.live.last(provider), self.interval)
                        || since((self.hub_age)(provider), self.interval)))
            {
                return Vec::new();
            }
            state.last_request = Some(now);
        }
        match self.fetch().await {
            Ok(observations) => {
                let mut state = self.state.lock().expect("codex poll state");
                state.rate_limited = 0;
                state.backoff_until = None;
                observations
            }
            Err(error) => {
                if error == probe::ProbeError::Timeout {
                    tracing::debug!(
                        kind = "codex_limits_probe_timeout",
                        "codex limits probe timed out"
                    );
                } else {
                    tracing::warn!(kind = "codex_limits_probe_failed", reason = ?error, "codex limits probe failed");
                }
                Vec::new()
            }
        }
    }
}

pub struct RoutedLimitsSource {
    routes: HashMap<String, Arc<dyn LimitsSource>>,
    default: Arc<dyn LimitsSource>,
}

impl RoutedLimitsSource {
    pub fn new(
        routes: HashMap<String, Arc<dyn LimitsSource>>,
        default: Arc<dyn LimitsSource>,
    ) -> Self {
        Self { routes, default }
    }
}

#[async_trait::async_trait]
impl LimitsSource for RoutedLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        let source = self.routes.get(provider).unwrap_or(&self.default).clone();
        source.poll(provider).await
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        let source = self.routes.get(provider).unwrap_or(&self.default).clone();
        source.poll_idle(provider).await
    }

    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        let source = self.routes.get(provider).unwrap_or(&self.default).clone();
        source.poll_report(provider, idle).await
    }
}
