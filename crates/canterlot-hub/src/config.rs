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
    pub hibernate: crate::hibernate::HibernateConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub attachments: AttachmentsConfig,
    #[serde(default)]
    pub machines: MachinesConfig,
    #[serde(default)]
    pub session_tools: SessionToolsConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    #[serde(default)]
    pub sessions: SessionsConfig,
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub provider_status: ProviderStatusConfig,
    #[serde(default)]
    pub dictation: DictationConfig,
    /// `[runners.cloud]`: облачные окна раннеров. Нет таблицы — облако выключено.
    #[serde(default)]
    pub runners: RunnersConfig,
    /// Свои цены моделей: сильнее таблицы `pricing.rs`.
    #[serde(default)]
    pub pricing: BTreeMap<String, PriceConfig>,
    /// Что загрузка подменила в конфигурации. Подписчик журнала к этому моменту ещё не
    /// установлен, поэтому предупреждения выдаёт `log_startup_warnings` после него.
    #[serde(skip)]
    pub startup_warnings: Vec<StartupWarning>,
}

/// Предупреждение загрузки конфигурации, которое надо показать в журнале.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupWarning {
    /// `[activity].hung_rescue = "auto"` принято как `alert`.
    HungRescueAutoUnavailable,
}

/// `[pricing."<id>"]` в `hub.toml`: доллары за 1 млн токенов.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PriceConfig {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

impl PriceConfig {
    pub fn validate(&self) -> Result<(), String> {
        let all = [
            Some(self.input),
            Some(self.output),
            self.cache_read,
            self.cache_write,
        ];
        if all
            .iter()
            .flatten()
            .all(|v| v.is_finite() && *v >= 0.0 && *v <= 100_000.0)
        {
            Ok(())
        } else {
            Err("цены — конечные неотрицательные числа, доллары за 1 млн токенов".into())
        }
    }
}

/// `[dictation]`: живая диктовка в PWA через службу распознавания речи.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DictationConfig {
    pub enabled: bool,
    /// Сокет `canterlot-speech serve`; относительный путь считается от файла конфигурации.
    pub socket: PathBuf,
}

impl Default for DictationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            socket: PathBuf::from("/run/canterlot-speech/speech.sock"),
        }
    }
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
    /// Срок ответа на прикладную пробу после просроченной квитанции команды.
    #[serde(with = "humantime_serde")]
    pub app_probe_timeout: Duration,
    /// Не чаще одной прикладной пробы на машину за этот срок.
    #[serde(with = "humantime_serde")]
    pub app_probe_interval: Duration,
    /// Срок итога «Отправить сейчас»: без итога вливание считается неподтверждённым.
    #[serde(with = "humantime_serde")]
    pub steer_outcome: Duration,
    /// Сколько хаб ждёт квитанцию о файле секрета, прежде чем счесть доставку потерянной.
    #[serde(with = "humantime_serde")]
    pub secret_attempt: Duration,
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
            app_probe_timeout: Duration::from_secs(10),
            app_probe_interval: Duration::from_secs(30),
            steer_outcome: Duration::from_secs(120),
            secret_attempt: Duration::from_secs(60),
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
    /// Пишет предупреждения загрузки; вызывать после установки подписчика журнала.
    pub fn log_startup_warnings(&self) {
        for warning in &self.startup_warnings {
            match warning {
                StartupWarning::HungRescueAutoUnavailable => tracing::warn!(
                    kind = "hung_rescue_auto_unavailable",
                    configured = "auto",
                    effective = "alert",
                    "[activity].hung_rescue = \"auto\" is not available in this release; behaving as \"alert\""
                ),
            }
        }
    }
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
        if config.activity.hung_rescue == HungRescue::Auto {
            config
                .startup_warnings
                .push(StartupWarning::HungRescueAutoUnavailable);
            config.activity.hung_rescue = HungRescue::Alert;
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for path in [
            &mut config.database_url_file,
            &mut config.tls_cert,
            &mut config.tls_key,
            &mut config.manifest,
            &mut config.attachments.blob_dir,
            &mut config.auth.geoip.path,
            &mut config.dictation.socket,
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
            config
                .auth
                .resolve(&http.allowed_origins)
                .map_err(|mut error| {
                    error.path = path.to_owned();
                    error
                })?;
        }
        if config
            .pricing
            .values()
            .any(|price| price.validate().is_err())
        {
            return Err(ConfigError {
                path: path.to_owned(),
                category: "invalid [pricing] price",
                location: None,
            });
        }
        config.provider_status.validate().map_err(|mut error| {
            error.path = path.to_owned();
            error
        })?;
        if let Some(cloud) = &mut config.runners.cloud {
            if cloud.token_file.is_relative() {
                cloud.token_file = base.join(&cloud.token_file);
            }
            cloud.validate().map_err(|category| ConfigError {
                path: path.to_owned(),
                category,
                location: None,
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

/// `[runners]` в `hub.toml`. Сейчас здесь только облако; статические раннеры живут строками
/// `machines` и `worker.toml` несущей.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunnersConfig {
    pub cloud: Option<CloudConfig>,
}

/// Столько профилей допускает список: каждый цена и, возможно, вызов создания.
pub const MAX_CLOUD_PROFILES: usize = 8;

/// Один профиль сервера: конфигуратор провайдера и размеры.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudProfile {
    pub configurator_id: i64,
    pub cpu: u32,
    pub ram_gb: u32,
    pub disk_gb: u32,
    /// Зона доступности сервера в приватной сети (`msk-1` для ru-3 конфигураторов).
    pub availability_zone: String,
}

impl CloudProfile {
    pub fn ram_mb(&self) -> u64 {
        u64::from(self.ram_gb) * 1024
    }

    pub fn disk_mb(&self) -> u64 {
        u64::from(self.disk_gb) * 1024
    }
}

/// Корень облачного раннера на сервере: его создаёт cloud-init, владелец `runner`.
pub const CLOUD_ROOT_DIR: &str = "/srv/canterlot-runner";

/// `[runners.cloud]`: облачные окна раннеров. Токен провайдера лежит только в файле
/// `token_file` и нигде не показывается: ни в `Debug`, ни в журнале, ни в API.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudConfig {
    /// Провайдер; сейчас только `timeweb`.
    pub provider: String,
    pub token_file: PathBuf,
    /// Проект провайдера, в котором живут серверы раннеров. Сверка не трогает чужие проекты.
    pub project_id: i64,
    /// Упорядоченный список профилей сервера `[[runners.cloud.profile]]`: создание пробует их по
    /// очереди, пока Timeweb не примет один. Резерв считается по самому дорогому.
    pub profile: Vec<CloudProfile>,
    #[serde(default = "CloudConfig::default_os_id")]
    pub os_id: i64,
    /// Открытая половина `[runners].ssh_key` несущей: уходит в cloud-init и в ключи проекта.
    pub runner_public_key: String,
    /// Приватная сеть (VPC) провайдера, в которой создаются серверы окон. Идентификатор
    /// Timeweb (`network-…`) создаётся владельцем один раз, хаб его не создаёт.
    pub network_id: String,
    /// Роутер той же сети с NAT. Его публичный IP используется для подключения по SSH.
    pub router_id: String,
    /// Публичный IPv4 роутера: адрес подключения носителя к облачному раннеру.
    pub router_ip: String,
    /// Шлюз роутера внутри сети; cloud-init прописывает маршрут по умолчанию через него.
    pub gateway: String,
    /// Включающий диапазон локальных адресов внутри сети, например `10.0.0.10-10.0.0.60`.
    pub local_ip_pool: String,
    /// Включающий диапазон публичных портов DNAT, например `22100-22199`.
    pub dnat_port_range: String,
    /// Сколько сервер может оставаться `no_paid` с момента создания, прежде чем это
    /// считается определённым отказом провайдера.
    #[serde(
        default = "CloudConfig::default_no_paid_grace",
        with = "humantime_serde"
    )]
    pub no_paid_grace: Duration,
    /// Несущая, которая подключает облачный раннер.
    pub carrier_machine_id: uuid::Uuid,
    #[serde(default)]
    pub rank: i32,
    #[serde(default = "CloudConfig::default_labels")]
    pub labels: Vec<String>,
    #[serde(default = "CloudConfig::default_max_running")]
    pub max_running: u32,
    #[serde(default = "CloudConfig::default_lease_slots")]
    pub lease_slots: u32,
    /// От вызова создания до подключения воркера; позже сервер удаляется.
    #[serde(
        default = "CloudConfig::default_ready_timeout",
        with = "humantime_serde"
    )]
    pub ready_timeout: Duration,
    /// Сколько окно без работы живёт до удаления.
    #[serde(default = "CloudConfig::default_idle_minutes")]
    pub idle_minutes: u32,
    /// От жёсткого предела до отмены идущих задач.
    #[serde(default = "CloudConfig::default_drain_grace", with = "humantime_serde")]
    pub drain_grace: Duration,
    /// От отмены до того, как неподтверждённые задачи станут `lost`.
    #[serde(
        default = "CloudConfig::default_teardown_margin",
        with = "humantime_serde"
    )]
    pub teardown_margin: Duration,
    #[serde(
        default = "CloudConfig::default_reconcile_interval",
        with = "humantime_serde"
    )]
    pub reconcile_interval: Duration,
    /// Сколько ждать появления сервера в полных списках, прежде чем счесть создание
    /// неудавшимся.
    #[serde(
        default = "CloudConfig::default_create_unknown_window",
        with = "humantime_serde"
    )]
    pub create_unknown_window: Duration,
}

/// Разбор включающего диапазона `a-b` с одним и тем же правилом для адресов и портов.
fn inclusive_range<T: PartialOrd>(text: &str, parse: impl Fn(&str) -> Option<T>) -> Option<(T, T)> {
    let (first, last) = text.trim().split_once('-')?;
    let (first, last) = (parse(first)?, parse(last)?);
    (first <= last).then_some((first, last))
}

impl std::fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudConfig")
            .field("provider", &self.provider)
            .field("project_id", &self.project_id)
            .field("profile", &self.profile)
            .field("carrier_machine_id", &self.carrier_machine_id)
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

impl CloudConfig {
    fn default_os_id() -> i64 {
        109
    }
    fn default_labels() -> Vec<String> {
        vec!["cloud".to_owned(), "kernel-6.12".to_owned()]
    }
    fn default_max_running() -> u32 {
        4
    }
    fn default_lease_slots() -> u32 {
        2
    }
    fn default_ready_timeout() -> Duration {
        Duration::from_secs(15 * 60)
    }
    fn default_idle_minutes() -> u32 {
        15
    }
    fn default_drain_grace() -> Duration {
        Duration::from_secs(30 * 60)
    }
    fn default_teardown_margin() -> Duration {
        Duration::from_secs(30 * 60)
    }
    fn default_reconcile_interval() -> Duration {
        Duration::from_secs(5 * 60)
    }
    fn default_create_unknown_window() -> Duration {
        Duration::from_secs(10 * 60)
    }
    fn default_no_paid_grace() -> Duration {
        Duration::from_secs(3 * 60)
    }

    /// Всё, что видно по самой конфигурации, без базы и сети. Код отказа не содержит значений.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.provider != "timeweb" {
            return Err("invalid [runners.cloud] provider");
        }
        if self.project_id <= 0 || self.os_id <= 0 {
            return Err("invalid [runners.cloud] ids");
        }
        // Сеть и роутер создаются владельцем один раз. Идентификаторы непустые, адреса —
        // настоящие IPv4, диапазоны непустые и с пересечением портов 1024–65535. Значения в
        // сообщении об ошибке не попадают: наружу уходит только код.
        if self.network_id.trim().is_empty()
            || self.router_id.trim().is_empty()
            || self.router_ip().is_none()
            || self.gateway().is_none()
        {
            return Err("invalid [runners.cloud] network and router");
        }
        if self.local_ip_range().is_none() {
            return Err("invalid [runners.cloud] local_ip_pool");
        }
        if self.dnat_port_values().is_none() {
            return Err("invalid [runners.cloud] dnat_port_range");
        }
        if self.profile.is_empty() || self.profile.len() > MAX_CLOUD_PROFILES {
            return Err("invalid [runners.cloud] profile list");
        }
        for profile in &self.profile {
            if profile.configurator_id <= 0 {
                return Err("invalid [runners.cloud.profile] ids");
            }
            if profile.cpu == 0 || profile.ram_gb == 0 || profile.disk_gb == 0 {
                return Err("invalid [runners.cloud.profile] size");
            }
            if profile.availability_zone.trim().is_empty() {
                return Err("invalid [runners.cloud.profile] availability_zone");
            }
        }
        if !crate::cloud::cloud_init::validate_public_key(&self.runner_public_key) {
            return Err("invalid [runners.cloud] runner_public_key");
        }
        if self.labels.is_empty() || !crate::machines::valid_labels(&self.labels) {
            return Err("invalid [runners.cloud] labels");
        }
        if self.max_running == 0 || self.lease_slots == 0 || self.idle_minutes == 0 {
            return Err("invalid [runners.cloud] slots");
        }
        let minute = Duration::from_secs(60);
        if [
            self.ready_timeout,
            self.drain_grace,
            self.teardown_margin,
            self.reconcile_interval,
            self.create_unknown_window,
            self.no_paid_grace,
        ]
        .iter()
        .any(|timing| *timing < minute)
        {
            return Err("invalid [runners.cloud] timing");
        }
        Ok(())
    }

    /// Публичный адрес роутера как IPv4.
    pub fn router_ip(&self) -> Option<std::net::Ipv4Addr> {
        self.router_ip.trim().parse().ok()
    }

    /// Адрес шлюза в сети как IPv4.
    pub fn gateway(&self) -> Option<std::net::Ipv4Addr> {
        self.gateway.trim().parse().ok()
    }

    /// Включающий диапазон локальных адресов из пула окна.
    pub fn local_ip_range(&self) -> Option<(std::net::Ipv4Addr, std::net::Ipv4Addr)> {
        inclusive_range(&self.local_ip_pool, |text| text.trim().parse().ok())
    }

    /// Включающий диапазон портов DNAT; порты вне 1024–65535 не годятся.
    pub fn dnat_port_values(&self) -> Option<(u16, u16)> {
        let (first, last) = inclusive_range(&self.dnat_port_range, |text| text.trim().parse().ok())?;
        (first >= 1024).then_some((first, last))
    }

    pub fn drain_grace_minutes(&self) -> u64 {
        self.drain_grace.as_secs().div_ceil(60)
    }

    pub fn teardown_margin_minutes(&self) -> u64 {
        self.teardown_margin.as_secs().div_ceil(60)
    }
}

/// `[provider_status]`: опрос публичных страниц статуса провайдеров (statuspage).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderStatusConfig {
    pub enabled: bool,
    /// Период опроса; не меньше минуты.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub claude: Option<StatusPageOverride>,
    pub codex: Option<StatusPageOverride>,
}

/// Замена встроенного адреса страницы или списка компонентов провайдера.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusPageOverride {
    pub url: Option<String>,
    pub components: Option<Vec<String>>,
}

/// Страница статуса одного провайдера после подстановки умолчаний.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPage {
    pub provider: &'static str,
    pub url: String,
    pub components: Vec<String>,
}

impl Default for ProviderStatusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(180),
            claude: None,
            codex: None,
        }
    }
}

impl ProviderStatusConfig {
    pub const MIN_INTERVAL: Duration = Duration::from_secs(60);

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = || ConfigError {
            path: PathBuf::new(),
            category: "config_invalid_provider_status",
            location: None,
        };
        if self.interval < Self::MIN_INTERVAL {
            return Err(invalid());
        }
        for page in self.pages() {
            if absolute_http_uri(&page.url).is_none()
                || page.components.iter().any(|name| name.trim().is_empty())
            {
                return Err(invalid());
            }
        }
        Ok(())
    }

    /// Встроенные страницы Claude и Codex с заменами из конфига.
    pub fn pages(&self) -> Vec<StatusPage> {
        let page = |provider: &'static str,
                    url: &str,
                    components: &[&str],
                    custom: &Option<StatusPageOverride>| {
            let custom = custom.clone().unwrap_or_default();
            StatusPage {
                provider,
                url: custom.url.unwrap_or_else(|| url.to_owned()),
                components: custom
                    .components
                    .unwrap_or_else(|| components.iter().map(|name| (*name).to_owned()).collect()),
            }
        };
        vec![
            page(
                "claude",
                "https://status.claude.com/api/v2/summary.json",
                &["Claude Code", "Claude API (api.anthropic.com)"],
                &self.claude,
            ),
            page(
                "codex",
                "https://status.openai.com/api/v2/summary.json",
                &["Responses", "Login"],
                &self.codex,
            ),
        ]
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
    /// Устарел (ADR-0005): личность больше не берётся из заголовка. Имя только называет
    /// заголовок, приход которого помечается `X-Canterlot-Proxy-Leak`.
    pub user_header: Option<String>,
    pub allowed_origins: Vec<String>,
    pub max_json_body_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub header_read_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub body_read_timeout: Duration,
    pub max_connections: usize,
    pub ws: WsConfig,
    pub proxy_secret_file: Option<PathBuf>,
    /// Устарел (ADR-0005): читается, но ни на что не влияет.
    pub logout_url: Option<String>,
    pub multi_process: bool,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            trusted_proxies: Vec::new(),
            owner_login: String::new(),
            user_header: None,
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
            || self.ws.replay_limit == 0
            || self.ws.delta_window.is_zero()
            || self.ws.delta_window > Duration::from_millis(25)
            || self.ws.session_check_interval.is_zero()
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
        if let Some(path) = &self.proxy_secret_file {
            let size = fs::read(path).map_err(|_| invalid())?.len();
            if size < 32 {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

impl HttpConfig {
    /// Заданные в конфиге устаревшие ключи: по предупреждению `config_key_deprecated` на каждый.
    pub fn deprecated_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        if self.user_header.is_some() {
            keys.push("http.user_header");
        }
        if self.logout_url.is_some() {
            keys.push("http.logout_url");
        }
        keys
    }

    /// Имя заголовка старого прокси, приход которого считается утечкой. Неразборчивое
    /// имя не останавливает запуск: берётся `Remote-User`.
    pub fn leak_header(&self) -> axum::http::HeaderName {
        self.user_header
            .as_deref()
            .and_then(|name| axum::http::HeaderName::from_bytes(name.as_bytes()).ok())
            .unwrap_or_else(|| axum::http::HeaderName::from_static("remote-user"))
    }
}

/// `[auth]`: проверяющая сторона WebAuthn. По умолчанию `origin` — первый из
/// `[http].allowed_origins`, `rp_id` — его хост.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub rp_id: Option<String>,
    pub origin: Option<String>,
    pub geoip: GeoipConfig,
}

/// `[auth.geoip]`: файл DB-IP «IP to City Lite» (MMDB). Путь разрешается от каталога
/// конфигурации; без файла место сеанса неизвестно.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeoipConfig {
    pub path: PathBuf,
}

impl Default for GeoipConfig {
    fn default() -> Self {
        Self {
            path: "data/geoip/dbip-city-lite.mmdb".into(),
        }
    }
}

impl AuthConfig {
    /// Подставляет значения по умолчанию и проверяет: `origin` из `allowed_origins`,
    /// `rp_id` — хост `origin` или его родительский домен.
    pub fn resolve(&mut self, allowed_origins: &[String]) -> Result<(), ConfigError> {
        let invalid = || ConfigError {
            path: PathBuf::new(),
            category: "config_invalid_auth",
            location: None,
        };
        let origin = match &self.origin {
            Some(origin) => origin.clone(),
            None => allowed_origins.first().cloned().ok_or_else(invalid)?,
        };
        if !allowed_origins.contains(&origin) {
            return Err(invalid());
        }
        let host = absolute_http_uri(&origin)
            .and_then(|uri| uri.host().map(str::to_owned))
            .ok_or_else(invalid)?;
        let rp_id = match &self.rp_id {
            Some(rp_id) => {
                let rp_id = rp_id.trim().to_ascii_lowercase();
                let host = host.to_ascii_lowercase();
                let valid = !rp_id.is_empty()
                    && !rp_id.starts_with('.')
                    && rp_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                    && (host == rp_id || host.ends_with(&format!(".{rp_id}")));
                if !valid {
                    return Err(invalid());
                }
                rp_id
            }
            None => host,
        };
        self.origin = Some(origin);
        self.rp_id = Some(rp_id);
        Ok(())
    }

    /// Адрес PWA для ссылок входа. После загрузки конфига задан всегда.
    pub fn origin(&self) -> &str {
        self.origin.as_deref().unwrap_or_default()
    }

    pub fn rp_id(&self) -> &str {
        self.rp_id.as_deref().unwrap_or_default()
    }
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
    /// Сколько событий хвоста повторяется при подписке со старым курсором; больше — `resync_required`.
    pub replay_limit: usize,
    #[serde(with = "humantime_serde")]
    pub stall_timeout: Duration,
    /// Как часто открытый WebSocket перепроверяет свой сеанс в базе, даже без уведомлений.
    #[serde(with = "humantime_serde")]
    pub session_check_interval: Duration,
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
            replay_limit: 300,
            stall_timeout: Duration::from_secs(15),
            session_check_interval: Duration::from_secs(60),
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

/// Режим спасения зависшего хода (`[activity].hung_rescue`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HungRescue {
    /// Обнаружение, карточка, пуш и автоматические действия там, где свидетельства надёжны.
    Auto,
    /// Обнаружение, карточка и пуш; ни прерываний, ни вливаний без владельца.
    Alert,
    /// Verdict «завис в генерации» не выносится: остаётся обычный `stalled`.
    Off,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ActivityConfig {
    /// Молчание генерации: удалённая модель не тратит CPU, поэтому порог щедрый.
    #[serde(with = "humantime_serde")]
    pub stalled_generating: Duration,
    #[serde(with = "humantime_serde")]
    pub stalled_tool: Duration,
    /// Молчание генерации с живым, но незагруженным процессом и пустым деревом:
    /// ход считается зависшим, и его можно спасти. Инструменты не попадают под этот порог.
    #[serde(with = "humantime_serde")]
    pub hung_generating: Duration,
    /// Что хаб делает с зависшим ходом: `alert` (по умолчанию) показывает карточку и
    /// шлёт пуш, `off` не выносит verdict вовсе. `auto` в 1.40 не предлагается:
    /// значение принимается и ведёт себя как `alert` (см. [`Self::effective_hung_rescue`]).
    pub hung_rescue: HungRescue,
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

impl ActivityConfig {
    /// Режим, который действительно работает. Автоматические пути спасения остаются
    /// в коде, но из конфигурации недоступны, пока не закрыты пробелы безопасности.
    pub fn effective_hung_rescue(&self) -> HungRescue {
        match self.hung_rescue {
            HungRescue::Auto => HungRescue::Alert,
            other => other,
        }
    }
}

impl Default for ActivityConfig {
    fn default() -> Self {
        Self {
            stalled_generating: Duration::from_secs(1800),
            stalled_tool: Duration::from_secs(1800),
            hung_generating: Duration::from_secs(600),
            hung_rescue: HungRescue::Alert,
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

/// `[jobs]` в `hub.toml`: задачи на месте (`job_run`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    /// Запас после срока команды: машина, молчащая дольше `timeout + lost_grace`, теряет задачу.
    #[serde(with = "humantime_serde")]
    pub lost_grace: Duration,
    /// Сколько хранится полный журнал законченной задачи; потом вложение отпускается, а строка
    /// оставляет метаданные и хвост. `0` хранит журналы без срока.
    #[serde(with = "humantime_serde")]
    pub log_retention: Duration,
    /// Предел суммарного размера результатов задачи, которые несущая собирает с раннера, в
    /// байтах (`outputs`). По умолчанию 512 МиБ.
    pub outputs_cap: u64,
    /// Сколько задача ждёт сбора результатов и очистки после конца на раннере; потом она
    /// заканчивается итогом раннера с пометкой «outputs not collected».
    #[serde(with = "humantime_serde")]
    pub collect_timeout: Duration,
    /// Сколько несущая может готовить исходники: дольше задача заканчивается `failed`.
    #[serde(with = "humantime_serde")]
    pub stage_timeout: Duration,
    /// Сколько задача ждёт раннер (`waiting_runner`): дольше она заканчивается `canceled` с
    /// причиной `no_runner`. `0` ждёт без срока.
    #[serde(with = "humantime_serde")]
    pub waiting_timeout: Duration,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            lost_grace: Duration::from_secs(10 * 60),
            log_retention: Duration::from_secs(14 * 86400),
            outputs_cap: 512 * 1024 * 1024,
            collect_timeout: Duration::from_secs(10 * 60),
            stage_timeout: Duration::from_secs(15 * 60),
            waiting_timeout: Duration::from_secs(24 * 3600),
        }
    }
}

/// `[memory]` в `hub.toml`: пределы общей памяти агентов.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    /// Активных записей на домен.
    pub max_active_per_domain: u32,
    /// Записей, правок и архивирований агента в час на разговор.
    pub writes_per_hour: u32,
    /// Строк в ответе `memory_search`.
    pub search_limit: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_active_per_domain: 2000,
            writes_per_hour: 60,
            search_limit: 20,
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
