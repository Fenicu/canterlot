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
        Ok(config)
    }
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
            || !(1025..=1048576).contains(&self.ws.queue_bytes)
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
    pub warn_pcts: Vec<u16>,
    #[serde(with = "humantime_serde")]
    pub stale_after: Duration,
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActivityConfig {
    #[serde(with = "humantime_serde")]
    pub stalled_generating: Duration,
    #[serde(with = "humantime_serde")]
    pub stalled_tool: Duration,
    #[serde(with = "humantime_serde")]
    pub tick: Duration,
    #[serde(with = "humantime_serde")]
    pub debounce: Duration,
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            stalled_generating: Duration::from_secs(300),
            stalled_tool: Duration::from_secs(1800),
            tick: Duration::from_secs(15),
            debounce: Duration::from_millis(250),
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttachmentsConfig {
    pub blob_dir: PathBuf,
    pub max_concurrent_uploads: usize,
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
            gc_grace: Duration::from_secs(86400),
            gc_interval: Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachinesConfig {
    pub disk_warn_pct: u8,
    #[serde(with = "humantime_serde")]
    pub metrics_stale_after: Duration,
}

impl Default for MachinesConfig {
    fn default() -> Self {
        Self {
            disk_warn_pct: 85,
            metrics_stale_after: Duration::from_secs(90),
        }
    }
}
