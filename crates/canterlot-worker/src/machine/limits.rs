use canterlot_proto::worker::v1::LimitObservation;
use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

use crate::adapter::codex::{limits::observations_from_read_response, probe};
use crate::config::{ClaudeCodeConfig, CodexConfig};

pub const CODEX_IDLE_TICK_INTERVAL: Duration = Duration::from_secs(60);
const CODEX_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub type HubLimitsAge = Arc<dyn Fn(&str) -> Option<Instant> + Send + Sync>;

#[async_trait::async_trait]
pub trait LimitsSource: Send + Sync {
    /// Запрос хаба (`PollLimits`, проверка пробуждения «кранча»).
    async fn poll(&self, provider: &str) -> Vec<LimitObservation>;
    /// Тик опроса без живых сессий; источник со своим интервалом простоя переопределяет его.
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll(provider).await
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
/// первый тик через интервал после регистрации. Решение, опрашивать ли на самом деле,
/// остаётся за источником провайдера.
#[derive(Debug, Default)]
pub struct IdleTicker {
    entries: Vec<(String, Duration, Instant)>,
}

impl IdleTicker {
    pub fn register(&mut self, provider: &str, interval: Duration) {
        self.entries
            .push((provider.to_owned(), interval, Instant::now() + interval));
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

pub const CLAUDE_OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_VERSION: &str = "oauth-2025-04-20";
const OAUTH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(4 * 3600);
const HUB_POLL_DEBOUNCE: Duration = Duration::from_secs(60);
const MAX_CREDENTIALS_BYTES: u64 = 64 * 1024;
const MAX_USAGE_BYTES: usize = 256 * 1024;

/// Причины пустого опроса. Ни один вариант не несёт токен, тело ответа или путь.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthPollError {
    Credentials,
    Expired,
    Rejected(u16),
    RateLimited,
    Status(u16),
    Transport,
    Body,
}

impl fmt::Display for OAuthPollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credentials => f.write_str("claude oauth credentials unavailable"),
            Self::Expired => f.write_str("claude oauth token expired"),
            Self::Rejected(status) => write!(f, "claude oauth token rejected with status {status}"),
            Self::RateLimited => f.write_str("claude oauth usage endpoint rate limited"),
            Self::Status(status) => write!(f, "claude oauth usage failed with status {status}"),
            Self::Transport => f.write_str("claude oauth usage request failed"),
            Self::Body => f.write_str("claude oauth usage response invalid"),
        }
    }
}

impl std::error::Error for OAuthPollError {}

struct OAuthToken(String);

impl fmt::Debug for OAuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthToken(<redacted>)")
    }
}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct PollState {
    last_request: Option<Instant>,
    backoff_until: Option<Instant>,
    rate_limited: u32,
}

/// Окна Claude без живых сессий через OAuth-точку `usage`: без запроса к модели.
/// Токен читается из файла учётных данных Claude Code только на время запроса
/// и никогда не обновляется: обновление сменило бы refresh-токен владельца.
pub struct ClaudeOAuthLimitsSource {
    credentials_path: PathBuf,
    endpoint: String,
    interval: Duration,
    live: LiveLimits,
    clock: Clock,
    client: Option<reqwest::Client>,
    state: Mutex<PollState>,
}

impl fmt::Debug for ClaudeOAuthLimitsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeOAuthLimitsSource")
            .field("endpoint", &self.endpoint)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

fn oauth_client(proxy: bool) -> Option<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .timeout(OAUTH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("canterlot-worker/", env!("CARGO_PKG_VERSION")));
    let builder = if proxy { builder } else { builder.no_proxy() };
    builder.build().ok()
}

impl ClaudeOAuthLimitsSource {
    pub fn new(config: &ClaudeCodeConfig, live: LiveLimits) -> Self {
        Self {
            credentials_path: config.credentials_file(),
            endpoint: CLAUDE_OAUTH_USAGE_URL.into(),
            interval: config.oauth_poll_interval(),
            live,
            clock: Arc::new(Instant::now),
            client: oauth_client(true),
            state: Mutex::default(),
        }
    }

    /// Адрес локальной заглушки в тестах: прямое соединение без прокси окружения.
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.into();
        self.client = oauth_client(false);
        self
    }

    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Один запрос без учёта интервала и паузы; `poll` вызывает его не чаще расписания.
    pub async fn fetch(&self) -> Result<Vec<LimitObservation>, OAuthPollError> {
        let token = read_oauth_token(&self.credentials_path, super::now_ms()).await?;
        let client = self.client.as_ref().ok_or(OAuthPollError::Transport)?;
        let request = client
            .get(&self.endpoint)
            .bearer_auth(&token.0)
            .header("anthropic-version", OAUTH_VERSION);
        drop(token);
        let mut response = request
            .send()
            .await
            .map_err(|_| OAuthPollError::Transport)?;
        match response.status().as_u16() {
            200 => {}
            429 => return Err(OAuthPollError::RateLimited),
            status @ (401 | 403) => return Err(OAuthPollError::Rejected(status)),
            status => return Err(OAuthPollError::Status(status)),
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| OAuthPollError::Transport)?
        {
            if body.len() + chunk.len() > MAX_USAGE_BYTES {
                return Err(OAuthPollError::Body);
            }
            body.extend_from_slice(&chunk);
        }
        usage_observations(&body, super::now_ms())
    }
}

#[async_trait::async_trait]
impl LimitsSource for ClaudeOAuthLimitsSource {
    /// Запрос хаба: без интервала простоя, но с паузой после 429 и не чаще раза в 60 с.
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, false).await
    }

    /// Тик простоя: не чаще интервала и только без свежего показания живого исполнения.
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, true).await
    }
}

impl ClaudeOAuthLimitsSource {
    async fn poll_with(&self, provider: &str, idle: bool) -> Vec<LimitObservation> {
        if provider != "claude" {
            return Vec::new();
        }
        let now = (self.clock)();
        {
            let mut state = self.state.lock().expect("oauth poll state");
            let since = |at: Option<Instant>, gap: Duration| {
                at.is_some_and(|at| now.saturating_duration_since(at) < gap)
            };
            if state.backoff_until.is_some_and(|until| now < until)
                || since(state.last_request, HUB_POLL_DEBOUNCE)
                || (idle
                    && (since(state.last_request, self.interval)
                        || since(self.live.last(provider), self.interval)))
            {
                return Vec::new();
            }
            state.last_request = Some(now);
        }
        let result = self.fetch().await;
        let mut state = self.state.lock().expect("oauth poll state");
        match result {
            Ok(observations) => {
                state.rate_limited = 0;
                state.backoff_until = None;
                observations
            }
            Err(error) => {
                if error == OAuthPollError::RateLimited {
                    state.rate_limited = state.rate_limited.saturating_add(1);
                    let backoff = self
                        .interval
                        .saturating_mul(2_u32.saturating_pow(state.rate_limited))
                        .min(MAX_BACKOFF)
                        .max(self.interval);
                    state.backoff_until = Some(now + backoff);
                }
                if matches!(error, OAuthPollError::Expired | OAuthPollError::RateLimited) {
                    tracing::debug!(kind = "claude_oauth_poll_skipped", reason = %error, "claude limits poll skipped");
                } else {
                    tracing::warn!(kind = "claude_oauth_poll_failed", reason = %error, "claude limits poll failed");
                }
                Vec::new()
            }
        }
    }
}

async fn read_oauth_token(
    path: &std::path::Path,
    now_ms: u64,
) -> Result<OAuthToken, OAuthPollError> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| OAuthPollError::Credentials)?;
    let mut bytes = Vec::new();
    file.take(MAX_CREDENTIALS_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| OAuthPollError::Credentials)?;
    if bytes.len() as u64 > MAX_CREDENTIALS_BYTES {
        return Err(OAuthPollError::Credentials);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| OAuthPollError::Credentials)?;
    drop(bytes);
    let oauth = value
        .get("claudeAiOauth")
        .ok_or(OAuthPollError::Credentials)?;
    let token = oauth
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(OAuthPollError::Credentials)?;
    if let Some(expires) = oauth.get("expiresAt").and_then(serde_json::Value::as_u64) {
        // Claude Code пишет миллисекунды; секунды узнаются по порядку величины.
        let expires_ms = if expires < 100_000_000_000 {
            expires.saturating_mul(1000)
        } else {
            expires
        };
        if expires_ms <= now_ms {
            return Err(OAuthPollError::Expired);
        }
    }
    Ok(OAuthToken(token.to_owned()))
}

/// `five_hour` и `seven_day` ответа; недельные окна по моделям в пилоте не учитываются.
/// `utilization` здесь в процентах (в `rate_limit_event` потока — доля).
fn usage_observations(
    body: &[u8],
    observed_ms: u64,
) -> Result<Vec<LimitObservation>, OAuthPollError> {
    use canterlot_proto::worker::v1::LimitWindow;
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| OAuthPollError::Body)?;
    let rows: Vec<(LimitWindow, f64, u64)> = [
        ("five_hour", LimitWindow::FiveHour),
        ("seven_day", LimitWindow::Weekly),
    ]
    .into_iter()
    .filter_map(|(key, window)| {
        let row = value.get(key)?;
        let utilization = row
            .get("utilization")?
            .as_f64()
            .filter(|u| u.is_finite() && *u >= 0.0)?;
        let resets = row
            .get("resets_at")
            .and_then(serde_json::Value::as_str)
            .and_then(rfc3339_ms)
            .unwrap_or(0);
        Some((window, utilization, resets))
    })
    .collect();
    Ok(rows
        .into_iter()
        .map(|(window, utilization, resets)| LimitObservation {
            provider: "claude".into(),
            window: window as i32,
            used_permille: (utilization * 10.0).round().min(1000.0) as u32,
            resets_at_ms: resets,
            observed_machine_ms: observed_ms,
        })
        .collect())
}

fn rfc3339_ms(text: &str) -> Option<u64> {
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
}
