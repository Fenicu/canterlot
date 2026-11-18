use canterlot_proto::worker::v1::{
    LimitAbsence, LimitObservation, ProviderCredential, ProviderCredits,
};
use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

use super::codexbar::CodexBarLimitsSource;
use crate::adapter::codex::{
    limits::{credits_from_read_response, observations_from_read_response},
    probe,
};
use crate::config::CodexConfig;

const FIRST_TICK_MAX_DELAY: Duration = Duration::from_secs(60);
const CODEX_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Не запускать второй probe, если он уже был в этом же опросе (окна через запасной источник).
const SAME_POLL_GAP: Duration = Duration::from_secs(1);

pub type HubLimitsAge = Arc<dyn Fn(&str) -> Option<Instant> + Send + Sync>;

/// Результат опроса: показания окон и окна, которых у провайдера нет.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimitsFetch {
    pub observations: Vec<LimitObservation>,
    pub absent: Vec<LimitAbsence>,
    /// Конец входа провайдера, если источник его знает: у Claude его приносят сессии, не опрос.
    pub credentials: Vec<ProviderCredential>,
    /// Сбросы лимитов провайдера: их знает опрос Codex, сессии о них не сообщают.
    pub provider_credits: Vec<ProviderCredits>,
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
            credentials: Vec::new(),
            provider_credits: Vec::new(),
        }
    }
    /// Просьба владельца обновить сейчас (`PollLimits.owner_refresh`).
    async fn poll_refresh(&self, provider: &str) -> LimitsFetch {
        self.poll_report(provider, false).await
    }
    /// Через сколько источник сам ждёт повторного опроса после сбоя; `None` — обычный интервал.
    fn retry_in(&self, _provider: &str) -> Option<Duration> {
        None
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
        if !matches!(provider, "claude" | "codex" | "opencode") {
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
            ("monthly", LimitWindow::Monthly),
            ("weekly_fable", LimitWindow::WeeklyFable),
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
                ..Default::default()
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

    /// Назначить провайдеру более ранний срок: повтор после временного сбоя. Позже не сдвигает.
    pub fn reschedule(&mut self, provider: &str, after: Duration) {
        let due = Instant::now() + after;
        if let Some((_, _, slot)) = self
            .entries
            .iter_mut()
            .find(|(name, _, _)| name == provider)
        {
            *slot = (*slot).min(due);
        }
    }
}

const HUB_POLL_DEBOUNCE: Duration = Duration::from_secs(60);

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct PollState {
    last_request: Option<Instant>,
    backoff_until: Option<Instant>,
    rate_limited: u32,
    last_credits_fetch: Option<Instant>,
}

pub(crate) fn rfc3339_ms(text: &str) -> Option<u64> {
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

    pub async fn fetch(&self) -> Result<LimitsFetch, probe::ProbeError> {
        let response = probe::probe(
            &self.executable,
            "account/rateLimits/read",
            serde_json::json!({}),
            CODEX_PROBE_TIMEOUT,
        )
        .await?;
        let at = super::now_ms();
        Ok(LimitsFetch {
            observations: observations_from_read_response(&response, at),
            absent: Vec::new(),
            credentials: Vec::new(),
            // Сбросы видит только ответ чтения: уведомление их не несёт.
            provider_credits: credits_from_read_response(&response, at)
                .into_iter()
                .collect(),
        })
    }

    /// Сбросы берём коротким probe не чаще раза за интервал опроса лимитов. Свой счётчик
    /// независим от подавления окон по возрасту снимка и живым сессиям: когда codexbar
    /// исправен, он не вызывает запасной источник, и сбросы иначе никогда не дошли бы.
    pub async fn credits_if_due(&self, interval: Duration) -> Vec<ProviderCredits> {
        let now = (self.clock)();
        {
            let mut state = self.state.lock().expect("codex poll state");
            let since = |at: Option<Instant>, gap: Duration| {
                at.is_some_and(|at| now.saturating_duration_since(at) < gap)
            };
            if state.backoff_until.is_some_and(|until| now < until)
                || since(state.last_request, SAME_POLL_GAP)
                || since(state.last_credits_fetch, interval)
            {
                return Vec::new();
            }
            state.last_credits_fetch = Some(now);
        }
        match self.fetch().await {
            Ok(fetched) => {
                let mut state = self.state.lock().expect("codex poll state");
                state.rate_limited = 0;
                state.backoff_until = None;
                fetched.provider_credits
            }
            // При сбое молчим: окна от codexbar не трогаем, неизвестные сбросы не отправляем.
            Err(_) => Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl LimitsSource for CodexRateLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.report_with(provider, false).await.observations
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.report_with(provider, true).await.observations
    }

    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        self.report_with(provider, idle).await
    }
}

impl CodexRateLimitsSource {
    async fn report_with(&self, provider: &str, idle: bool) -> LimitsFetch {
        if provider != "codex" {
            return LimitsFetch::default();
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
                return LimitsFetch::default();
            }
            state.last_request = Some(now);
        }
        match self.fetch().await {
            Ok(fetched) => {
                let mut state = self.state.lock().expect("codex poll state");
                state.rate_limited = 0;
                state.backoff_until = None;
                fetched
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
                LimitsFetch::default()
            }
        }
    }
}

/// Маршрут Codex: окна остаются за CodexBar, сбросы приносит отдельный probe с паузой.
pub struct CodexLimitsSource {
    codexbar: CodexBarLimitsSource,
    probe: Arc<CodexRateLimitsSource>,
    credits_interval: Duration,
}

impl CodexLimitsSource {
    async fn complete(&self, provider: &str, mut fetched: LimitsFetch) -> LimitsFetch {
        if provider == "codex" && fetched.provider_credits.is_empty() {
            fetched.provider_credits = self.probe.credits_if_due(self.credits_interval).await;
        }
        fetched
    }
}

#[async_trait::async_trait]
impl LimitsSource for CodexLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        let observations = self.codexbar.poll(provider).await;
        self.complete(
            provider,
            LimitsFetch {
                observations,
                ..Default::default()
            },
        )
        .await
        .observations
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        let observations = self.codexbar.poll_idle(provider).await;
        self.complete(
            provider,
            LimitsFetch {
                observations,
                ..Default::default()
            },
        )
        .await
        .observations
    }

    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        self.complete(provider, self.codexbar.poll_report(provider, idle).await)
            .await
    }

    async fn poll_refresh(&self, provider: &str) -> LimitsFetch {
        self.complete(provider, self.codexbar.poll_refresh(provider).await)
            .await
    }

    fn retry_in(&self, provider: &str) -> Option<Duration> {
        self.codexbar.retry_in(provider)
    }
}

/// Сборка маршрута Codex, общая для воркера и тестов.
pub fn codex_limits_route(
    codex: &CodexConfig,
    codexbar_path: Option<PathBuf>,
    limits_interval: Duration,
    live: LiveLimits,
    hub_age: HubLimitsAge,
) -> Arc<dyn LimitsSource> {
    codex_limits_route_with_clock(
        codex,
        codexbar_path,
        limits_interval,
        live,
        hub_age,
        Instant::now,
    )
}

/// Тот же маршрут с подставными часами: тест двигает время, не останавливая Tokio.
pub fn codex_limits_route_with_clock(
    codex: &CodexConfig,
    codexbar_path: Option<PathBuf>,
    limits_interval: Duration,
    live: LiveLimits,
    hub_age: HubLimitsAge,
    clock: impl Fn() -> Instant + Send + Sync + 'static,
) -> Arc<dyn LimitsSource> {
    let clock: Arc<dyn Fn() -> Instant + Send + Sync + 'static> = Arc::new(clock);
    let probe = Arc::new(
        CodexRateLimitsSource::new(codex, live, hub_age).with_clock({
            let clock = clock.clone();
            move || clock()
        }),
    );
    Arc::new(CodexLimitsSource {
        codexbar: CodexBarLimitsSource::new(codexbar_path, limits_interval, Some(probe.clone()))
            .with_shared_clock(clock),
        probe,
        credits_interval: limits_interval,
    })
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

    async fn poll_refresh(&self, provider: &str) -> LimitsFetch {
        let source = self.routes.get(provider).unwrap_or(&self.default).clone();
        source.poll_refresh(provider).await
    }

    fn retry_in(&self, provider: &str) -> Option<Duration> {
        self.routes
            .get(provider)
            .unwrap_or(&self.default)
            .retry_in(provider)
    }
}

#[cfg(test)]
mod idle_ticker_tests {
    use super::IdleTicker;
    use std::time::Duration;

    impl IdleTicker {
        fn due_in(&self, provider: &str) -> Option<Duration> {
            let now = tokio::time::Instant::now();
            self.entries
                .iter()
                .find(|(name, _, _)| name == provider)
                .map(|(_, _, due)| due.saturating_duration_since(now))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_retry_moves_the_provider_tick_earlier_but_never_later() {
        let mut ticker = IdleTicker::default();
        ticker.register("claude", Duration::from_secs(900));
        assert_eq!(ticker.due_in("claude"), Some(Duration::from_secs(60)));

        tokio::time::advance(Duration::from_secs(50)).await;
        ticker.reschedule("claude", Duration::from_secs(5));
        assert_eq!(ticker.due_in("claude"), Some(Duration::from_secs(5)));

        ticker.reschedule("claude", Duration::from_secs(30));
        assert_eq!(
            ticker.due_in("claude"),
            Some(Duration::from_secs(5)),
            "a later retry does not postpone the tick"
        );

        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(ticker.tick().await, vec!["claude".to_owned()]);
    }
}
