use std::{
    collections::BTreeMap,
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use ipnet::IpNet;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    pub database_url_file: PathBuf,
    pub grpc_listen: SocketAddr,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub manifest: PathBuf,
    #[serde(default)]
    pub timers: HubTimers,
    #[serde(default)]
    pub http: Option<HttpConfig>,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub activity: ActivityConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub attachments: AttachmentsConfig,
    #[serde(default)]
    pub machines: MachinesConfig,
    #[serde(default)]
    pub session_tools: SessionToolsConfig,
    #[serde(default)]
    pub sessions: SessionsConfig,
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HubTimers {
    #[serde(with = "humantime_serde")]
    pub handshake: Duration,
    #[serde(with = "humantime_serde")]
    pub degraded_after: Duration,
    #[serde(with = "humantime_serde")]
    pub unavailable_after: Duration,
    #[serde(with = "humantime_serde")]
    pub http2_keepalive_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub http2_keepalive_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub command_stored_receipt: Duration,
    #[serde(with = "humantime_serde")]
    pub dispatch_sweep: Duration,
    /// Срок итога «Отправить сейчас»: без итога вливание считается неподтверждённым.
    #[serde(with = "humantime_serde")]
    pub steer_outcome: Duration,
}

impl Default for HubTimers {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            degraded_after: Duration::from_secs(15),
            unavailable_after: Duration::from_secs(30),
            http2_keepalive_interval: Duration::from_secs(30),
            http2_keepalive_timeout: Duration::from_secs(10),
            command_stored_receipt: Duration::from_secs(5),
            dispatch_sweep: Duration::from_secs(5),
            steer_outcome: Duration::from_secs(120),
        }
    }
}

pub struct ConfigError {
    path: PathBuf,
    category: &'static str,
    location: Option<(usize, usize)>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.category, self.path.display())?;
        if let Some((line, column)) = self.location {
            write!(f, " (line {line}, column {column})")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ConfigError {}

impl HubConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|_| ConfigError {
            path: path.to_owned(),
            category: "config_io_error",
            location: None,
        })?;
        let mut config: Self = toml::from_str(&text).map_err(|error: toml::de::Error| {
            let offset = error.span().map_or(0, |span| span.start).min(text.len());
            let prefix = &text[..text.floor_char_boundary(offset)];
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            ConfigError {
                path: path.to_owned(),
                category: "config_invalid",
                location: Some((line, column)),
            }
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for path in [
            &mut config.database_url_file,
            &mut config.tls_cert,
            &mut config.tls_key,
            &mut config.manifest,
            &mut config.attachments.blob_dir,
        ] {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        if let Some(http) = &mut config.http {
            if let Some(secret) = &mut http.proxy_secret_file
                && secret.is_relative()
            {
                *secret = base.join(&*secret);
            }
            http.validate().map_err(|mut error| {
                error.path = path.to_owned();
                error
            })?;
        }
        if let Some(gotify) = &mut config.notify.gotify {
            if gotify.token_file.is_relative() {
                gotify.token_file = base.join(&gotify.token_file);
            }
            let origins = config
                .http
                .as_ref()
                .map(|http| http.allowed_origins.as_slice())
                .unwrap_or_default();
            gotify.resolve(path, origins)?;
        }
        Ok(config)
    }
}

/// Пуши владельцу. Без `[notify.gotify]` выключены.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifyConfig {
    pub gotify: Option<GotifyConfig>,
}

/// Ключ приложения Gotify. Значение не печатается ни в `Debug`, ни в журнал.
#[derive(Clone, Default)]
pub struct GotifyToken(String);

impl GotifyToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GotifyToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GotifyToken([redacted])")
    }
}

/// `[notify.gotify]`: сервер Gotify, файл с ключом приложения и выбор событий.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GotifyConfig {
    /// Адрес сервера Gotify, например `https://gotify.example.com`.
    pub url: String,
    /// Файл с ключом приложения. Читается один раз при старте.
    pub token_file: PathBuf,
    /// Адрес PWA для ссылки в пуше. По умолчанию первый из `[http].allowed_origins`.
    #[serde(default)]
    pub app_url: Option<String>,
    /// Часовой пояс времени в тексте пуша (IANA), по умолчанию `UTC`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Сессия ждёт владельца: вопрос, разрешение, план, одобрение запуска.
    #[serde(default = "enabled")]
    pub waiting: bool,
    /// Корневая сессия закончила ход.
    #[serde(default = "enabled")]
    pub turn_finished: bool,
    /// Корневая сессия упала, застряла, потеряла воркера или встала на паузу по лимиту.
    #[serde(default = "enabled")]
    pub failures: bool,
    /// Общий предел пушей в минуту.
    #[serde(default = "default_max_per_minute")]
    pub max_per_minute: u32,
    #[serde(skip)]
    pub token: GotifyToken,
}

fn default_timezone() -> String {
    "UTC".into()
}

fn enabled() -> bool {
    true
}

fn default_max_per_minute() -> u32 {
    20
}

impl GotifyConfig {
    /// Проверяет адреса, читает ключ и подставляет адрес PWA из `allowed_origins`.
    fn resolve(&mut self, config_path: &Path, origins: &[String]) -> Result<(), ConfigError> {
        let invalid = |category| ConfigError {
            path: config_path.to_owned(),
            category,
            location: None,
        };
        self.url = base_url(&self.url).ok_or_else(|| invalid("config_invalid_notify_url"))?;
        let app_url = self
            .app_url
            .as_deref()
            .or(origins.first().map(String::as_str))
            .ok_or_else(|| invalid("config_notify_app_url_missing"))?;
        self.app_url =
            Some(base_url(app_url).ok_or_else(|| invalid("config_invalid_notify_app_url"))?);
        if self.timezone.trim().is_empty() || self.max_per_minute == 0 {
            return Err(invalid("config_invalid_notify"));
        }
        let token_error = |category| ConfigError {
            path: self.token_file.clone(),
            category,
            location: None,
        };
        let bytes =
            fs::read(&self.token_file).map_err(|_| token_error("notify_token_file_unreadable"))?;
        let token = std::str::from_utf8(&bytes)
            .map(str::trim)
            .ok()
            .filter(|token| {
                !token.is_empty()
                    && token.len() <= 256
                    && token.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .ok_or_else(|| token_error("notify_token_file_invalid"))?;
        self.token = GotifyToken(token.to_owned());
        Ok(())
    }

    /// Адрес PWA без завершающей косой черты. После загрузки конфига задан всегда.
    pub fn app_url(&self) -> &str {
        self.app_url.as_deref().unwrap_or_default()
    }
}

/// Абсолютный адрес http(s) без запроса и фрагмента; завершающая косая черта снимается.
fn base_url(value: &str) -> Option<String> {
    let uri = absolute_http_uri(value.trim())?;
    if uri.query().is_some() || value.contains('#') {
        return None;
    }
    let path = uri.path().trim_end_matches('/');
    let authority = uri.authority()?;
    Some(format!("{}://{authority}{path}", uri.scheme_str()?))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    pub trusted_proxies: Vec<IpNet>,
    pub owner_login: String,
    pub user_header: String,
    pub allowed_origins: Vec<String>,
    pub max_json_body_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub header_read_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub body_read_timeout: Duration,
    pub max_connections: usize,
    pub ws: WsConfig,
    pub proxy_secret_file: Option<PathBuf>,
    pub logout_url: Option<String>,
    pub multi_process: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            trusted_proxies: Vec::new(),
            owner_login: String::new(),
            user_header: "Remote-User".into(),
            allowed_origins: Vec::new(),
            max_json_body_bytes: 524288,
            header_read_timeout: Duration::from_secs(10),
            body_read_timeout: Duration::from_secs(30),
            max_connections: 256,
            ws: WsConfig::default(),
            proxy_secret_file: None,
            logout_url: None,
            multi_process: false,
        }
    }
}

impl HttpConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = || ConfigError {
            path: PathBuf::new(),
            category: "config_invalid_http",
            location: None,
        };
        if self.trusted_proxies.is_empty()
            || self.owner_login.trim().is_empty()
            || self.allowed_origins.is_empty()
            || self.header_read_timeout.is_zero()
            || self.body_read_timeout.is_zero()
            || self.max_connections == 0
            || self.max_connections > tokio::sync::Semaphore::MAX_PERMITS
            || !(1..=16).contains(&self.ws.max_subscriptions)
            || !(2..=256).contains(&self.ws.queue_messages)
            || !(4096..=1048576).contains(&self.ws.queue_bytes)
            || self.ws.max_connection_age.is_zero()
            || self.ws.poll_interval.is_zero()
            || self.ws.stall_timeout.is_zero()
            || self.ws.delta_window.is_zero()
            || self.ws.delta_window > Duration::from_millis(25)
        {
            return Err(invalid());
        }
        for origin in &self.allowed_origins {
            let uri = absolute_http_uri(origin).ok_or_else(invalid)?;
            let scheme = uri.scheme_str().ok_or_else(invalid)?;
            let authority = uri.authority().ok_or_else(invalid)?;
            if origin != &format!("{scheme}://{authority}") {
                return Err(invalid());
            }
        }
        validate_user_header(&self.user_header).ok_or_else(invalid)?;
        if let Some(logout) = &self.logout_url {
            let uri = absolute_http_uri(logout).ok_or_else(invalid)?;
            if uri.scheme_str() != Some("https") {
                return Err(invalid());
            }
        }
        if let Some(path) = &self.proxy_secret_file {
            let size = fs::read(path).map_err(|_| invalid())?.len();
            if size < 32 {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

/// Имя заголовка личности: обычное имя, не из служебных и не из семейств `x-forwarded-*`, `x-canterlot-*`.
fn validate_user_header(name: &str) -> Option<()> {
    let name = axum::http::HeaderName::from_bytes(name.as_bytes()).ok()?;
    let name = name.as_str();
    const RESERVED: &[&str] = &[
        "host",
        "origin",
        "cookie",
        "authorization",
        "proxy-authorization",
        "forwarded",
        "x-real-ip",
        "true-client-ip",
        "cf-connecting-ip",
        "x-originating-ip",
        "content-type",
        "content-length",
        "transfer-encoding",
        "connection",
        "upgrade",
        "idempotency-key",
        "x-correlation-id",
    ];
    let forbidden = RESERVED.contains(&name)
        || name.starts_with("x-forwarded-")
        || name.starts_with("x-canterlot-")
        || name.starts_with("sec-");
    (!forbidden).then_some(())
}

fn absolute_http_uri(value: &str) -> Option<tonic::codegen::http::Uri> {
    let uri: tonic::codegen::http::Uri = value.parse().ok()?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return None;
    }
    let authority = uri.authority()?;
    if authority.as_str().contains('@') || authority.host().is_empty() {
        return None;
    }
    let host = authority.host();
    if host.starts_with('[') {
        host.strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .ok()?;
    } else if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    let suffix = authority.as_str().strip_prefix(host)?;
    if !suffix.is_empty() {
        suffix.strip_prefix(':')?.parse::<u16>().ok()?;
    }
    Some(uri)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WsConfig {
    #[serde(with = "humantime_serde")]
    pub max_connection_age: Duration,
    pub max_subscriptions: usize,
    pub queue_messages: usize,
    pub queue_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub delta_window: Duration,
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub stall_timeout: Duration,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            max_connection_age: Duration::from_secs(1800),
            max_subscriptions: 16,
            queue_messages: 256,
            queue_bytes: 1048576,
            delta_window: Duration::from_millis(25),
            poll_interval: Duration::from_secs(2),
            stall_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// Умолчание недельного резерва как расход, %: `100 - weekly_reserve_pct` — остаток.
    /// Владелец меняет действующее значение в PWA; файл задаёт только умолчание.
    pub weekly_reserve_pct: u16,
    #[serde(with = "humantime_serde")]
    pub snapshot_min_interval: Duration,
    pub history_days: u32,
    pub default_resume_text: String,
    #[serde(with = "humantime_serde")]
    pub reset_grace: Duration,
    #[serde(deserialize_with = "duration_vec")]
    pub verify_backoff: Vec<Duration>,
    #[serde(with = "humantime_serde")]
    pub poll_min_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub unknown_reset_retry: Duration,
    #[serde(deserialize_with = "duration_map")]
    pub poll_floor: BTreeMap<String, Duration>,
    pub reserve_exit_hysteresis_permille: u16,
    /// Устарело: раньше окрашивало остаток в PWA. Оставлено, чтобы старые hub.toml
    /// разбирались; действующие пороги владелец задаёт в настройках.
    pub warn_pcts: Vec<u16>,
    #[serde(with = "humantime_serde")]
    pub stale_after: Duration,
    /// Удержание снимается по часам, если столько после срока не пришло ни одного нового показания.
    #[serde(with = "humantime_serde")]
    pub unverified_wake_after: Duration,
}

impl fmt::Debug for LimitsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LimitsConfig")
            .field("default_resume_text_bytes", &self.default_resume_text.len())
            .finish_non_exhaustive()
    }
}

fn duration_vec<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Duration>, D::Error> {
    Vec::<humantime_serde::Serde<Duration>>::deserialize(deserializer).map(|values| {
        values
            .into_iter()
            .map(humantime_serde::Serde::into_inner)
            .collect()
    })
}

fn duration_map<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Duration>, D::Error> {
    BTreeMap::<String, humantime_serde::Serde<Duration>>::deserialize(deserializer).map(|values| {
        values
            .into_iter()
            .map(|(key, value)| (key, value.into_inner()))
            .collect()
    })
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            weekly_reserve_pct: 95,
            snapshot_min_interval: Duration::from_secs(600),
            history_days: 90,
            default_resume_text: "Лимиты восстановлены, продолжай с того места, где остановился."
                .into(),
            reset_grace: Duration::from_secs(20),
            verify_backoff: [30, 60, 120, 300].map(Duration::from_secs).to_vec(),
            poll_min_interval: Duration::from_secs(300),
            unknown_reset_retry: Duration::from_secs(1800),
            poll_floor: BTreeMap::from([
                ("claude".into(), Duration::from_secs(1800)),
                ("codex".into(), Duration::from_secs(300)),
            ]),
            reserve_exit_hysteresis_permille: 20,
            warn_pcts: vec![80, 90, 95],
            stale_after: Duration::from_secs(2100),
            unverified_wake_after: Duration::from_secs(1200),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActivityConfig {
    /// Молчание генерации: удалённая модель не тратит CPU, поэтому порог щедрый.
    #[serde(with = "humantime_serde")]
    pub stalled_generating: Duration,
    #[serde(with = "humantime_serde")]
    pub stalled_tool: Duration,
    #[serde(with = "humantime_serde")]
    pub tick: Duration,
    #[serde(with = "humantime_serde")]
    pub debounce: Duration,
    /// Сколько `idle` без фоновой работы держится спокойно, прежде чем сессия считается устоявшейся.
    #[serde(with = "humantime_serde")]
    pub idle_quiet: Duration,
    /// Сколько `idle` с работающей фоновой задачей держится, прежде чем подписчик услышит
    /// отдельное предупреждение: фон не будит сессию, сама она ничего не делает.
    #[serde(with = "humantime_serde")]
    pub idle_background_max: Duration,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            stalled_generating: Duration::from_secs(1800),
            stalled_tool: Duration::from_secs(1800),
            tick: Duration::from_secs(15),
            debounce: Duration::from_millis(250),
            idle_quiet: Duration::from_secs(5),
            idle_background_max: Duration::from_secs(15 * 60),
        }
    }
}

/// Повседневные настройки сессий владельца.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionsConfig {
    /// Сколько сессия лежит в корзине до окончательной уборки. `0` выключает уборку.
    #[serde(with = "humantime_serde")]
    pub trash_retention: Duration,
}

impl Default for SessionsConfig {
    fn default() -> Self {
        Self {
            trash_retention: Duration::from_secs(24 * 3600),
        }
    }
}

/// Пределы межсессионных инструментов: глубина и частота сообщений и запусков.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionToolsConfig {
    pub max_spawn_depth: u32,
    pub max_chain_depth: u32,
    pub pair_sends: u32,
    #[serde(with = "humantime_serde")]
    pub pair_window: Duration,
    pub sender_sends: u32,
    #[serde(with = "humantime_serde")]
    pub sender_window: Duration,
    pub parent_spawns: u32,
    #[serde(with = "humantime_serde")]
    pub spawn_window: Duration,
    /// Сколько детей одного родителя могут работать одновременно. `0` выключает.
    pub max_working_children: u32,
    /// Пауза между запусками детей одного родителя. `0` выключает.
    #[serde(with = "humantime_serde")]
    pub spawn_stagger: Duration,
    /// Срок хранения принятой делегации: через него уборка удаляет её из архива.
    /// `0` выключает автоматическое удаление.
    #[serde(with = "humantime_serde")]
    pub accepted_retention: Duration,
    /// Сколько машина должна пробыть без живого воркера, чтобы подписчики её незавершённых
    /// сессий услышали об этом один раз. Короткий простой молчит.
    #[serde(with = "humantime_serde")]
    pub machine_unavailable_after: Duration,
}

impl Default for SessionToolsConfig {
    fn default() -> Self {
        Self {
            max_spawn_depth: 3,
            max_chain_depth: 8,
            pair_sends: 10,
            pair_window: Duration::from_secs(600),
            sender_sends: 30,
            sender_window: Duration::from_secs(600),
            parent_spawns: 10,
            spawn_window: Duration::from_secs(3600),
            max_working_children: 6,
            spawn_stagger: Duration::from_secs(20),
            accepted_retention: Duration::from_secs(3 * 86400),
            machine_unavailable_after: Duration::from_secs(3 * 60),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    pub max_pending_per_conversation: usize,
    #[serde(with = "humantime_serde")]
    pub retry_queue_full: Duration,
    pub max_attempts: u32,
    #[serde(with = "humantime_serde")]
    pub idle_poll: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_pending_per_conversation: 50,
            retry_queue_full: Duration::from_secs(60),
            max_attempts: 20,
            idle_poll: Duration::from_secs(1),
        }
    }
}

/// Инструменты оркестратора: выключатели этапов, откат поведения без отката схемы.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestratorConfig {
    pub alarms: AlarmsConfig,
    pub nudge: NudgeConfig,
    pub routing: RoutingConfig,
    pub spawn_queue: SpawnQueueConfig,
    pub guards: GuardsConfig,
    /// Через сколько минут удержания аренды тяжёлой задачи получатели событий
    /// машины один раз слышат `lease_long_hold`.
    pub lease_warn_hold_minutes: u64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            alarms: Default::default(),
            nudge: Default::default(),
            routing: Default::default(),
            spawn_queue: Default::default(),
            guards: Default::default(),
            lease_warn_hold_minutes: 90,
        }
    }
}

impl OrchestratorConfig {
    pub fn lease_warn_hold(&self) -> Duration {
        Duration::from_secs(self.lease_warn_hold_minutes.max(1).saturating_mul(60))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NudgeConfig {
    pub enabled: bool,
}

impl Default for NudgeConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlarmsConfig {
    pub enabled: bool,
}

impl Default for AlarmsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Маршрутизация: выключатель, свежесть показаний лимитов и горизонт очереди.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub stale_limits: Duration,
    /// Дальше этого срока освобождение слота не ждут: запуск отвечает ошибкой.
    #[serde(with = "humantime_serde")]
    pub queue_horizon: Duration,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_limits: Duration::from_secs(30 * 60),
            queue_horizon: Duration::from_secs(6 * 3600),
        }
    }
}

/// Очередь запусков: срок жизни элемента, предел ожидающих и пауза повтора.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpawnQueueConfig {
    /// Сколько элемент ждёт запуска; после срока он истекает с заметкой.
    #[serde(with = "humantime_serde")]
    pub ttl: Duration,
    /// Больше этого числа ожидающих родитель не ставит: `spawn_queue_full`.
    pub max_waiting: i64,
    /// Через сколько повторять проверку занятости (параллельность, лимиты, страж).
    #[serde(with = "humantime_serde")]
    pub retry_delay: Duration,
}

impl Default for SpawnQueueConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(24 * 3600),
            max_waiting: 20,
            retry_delay: Duration::from_secs(60),
        }
    }
}

/// Страж машины перед запуском: пороги `[orchestrator.guards]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuardsConfig {
    pub min_free_disk_gb: f64,
    pub min_free_disk_percent: f64,
    pub max_load_per_core: f64,
    pub min_mem_available_gb: f64,
    /// Показания старше этого срока страж не включают.
    #[serde(with = "humantime_serde")]
    pub metrics_max_age: Duration,
}

impl Default for GuardsConfig {
    fn default() -> Self {
        Self {
            min_free_disk_gb: 30.0,
            min_free_disk_percent: 10.0,
            max_load_per_core: 1.5,
            min_mem_available_gb: 4.0,
            metrics_max_age: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttachmentsConfig {
    pub blob_dir: PathBuf,
    pub max_concurrent_uploads: usize,
    /// Суммарный объём медиа `PutMedia` одной машины; `0` (по умолчанию) выключает предел.
    pub max_media_bytes_per_machine: u64,
    #[serde(with = "humantime_serde")]
    pub gc_grace: Duration,
    #[serde(with = "humantime_serde")]
    pub gc_interval: Duration,
}

impl Default for AttachmentsConfig {
    fn default() -> Self {
        Self {
            blob_dir: "data/attachments".into(),
            max_concurrent_uploads: 4,
            max_media_bytes_per_machine: 0,
            gc_grace: Duration::from_secs(86400),
            gc_interval: Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachinesConfig {
    /// Свободного места меньше этого — предупреждение в PWA и событие подписчикам.
    pub disk_warn_free_gb: u64,
    /// Свободного места меньше этого — второй, отдельный уровень предупреждения.
    pub disk_critical_free_gb: u64,
    /// Устарело: прежний порог по доле занятого места. Принимается ради старых файлов.
    pub disk_warn_pct: u8,
    #[serde(with = "humantime_serde")]
    pub metrics_stale_after: Duration,
    /// Свободной памяти меньше этого три выборки подряд — эпизод `mem_low`. Ниже
    /// стража запуска (4 ГБ) намеренно: страж не пускает новые сессии раньше.
    pub mem_warn_gb: u64,
}

impl Default for MachinesConfig {
    fn default() -> Self {
        Self {
            disk_warn_free_gb: 30,
            disk_critical_free_gb: 10,
            disk_warn_pct: 85,
            metrics_stale_after: Duration::from_secs(90),
            mem_warn_gb: 3,
        }
    }
}

impl MachinesConfig {
    pub fn disk_warn_bytes(&self) -> u64 {
        self.disk_warn_free_gb.saturating_mul(1_000_000_000)
    }

    pub fn disk_critical_bytes(&self) -> u64 {
        self.disk_critical_free_gb.saturating_mul(1_000_000_000)
    }

    /// Возврат к обычному состоянию с запасом: иначе колебания у порога мигают.
    pub fn disk_recover_bytes(&self) -> u64 {
        self.disk_warn_bytes().saturating_add(5_000_000_000)
    }
}
