use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use url::Url;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    pub hub_url: String,
    /// `unix:<абсолютный путь>`: соединения с хабом идут через этот сокет, а TLS проверяет
    /// имя сервера из `hub_url`. Так работает воркер раннера за SSH-пробросом носителя.
    #[serde(default)]
    pub hub_dial: Option<String>,
    pub ca_cert: PathBuf,
    pub token_file: PathBuf,
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub timers: WorkerTimers,
    #[serde(default, alias = "exec")]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub machine: MachineConfig,
    #[serde(default)]
    pub attachments: WorkerAttachmentsConfig,
    #[serde(default)]
    pub claude_code: Option<ClaudeCodeConfig>,
    #[serde(default)]
    pub codex: Option<CodexConfig>,
    #[serde(default)]
    pub opencode: Option<OpenCodeConfig>,
    /// Устаревший раздел: принимается и игнорируется для старых worker.toml.
    #[serde(rename = "antigravity", default)]
    pub legacy_ignored: Option<toml::Value>,
    #[serde(default)]
    pub secrets: SecretConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    /// Раннеры, которых этот воркер как носитель ведёт по SSH (`runners_v1`).
    #[serde(default)]
    pub runners: RunnersConfig,
}

/// `[runners]`: носитель ведёт перечисленные раннеры, если хаб тоже назвал их своими.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunnersConfig {
    /// Ключ для облачных раннеров; нужен только этапу C, статические ходят по `~/.ssh/config`.
    pub ssh_key: Option<PathBuf>,
    /// Только для тестов, в файле не читается (`skip`: ключ отвергается как неизвестный):
    /// файл `ssh_config`, который носитель передаёт в каждый `ssh` ключом `-F`.
    #[serde(skip)]
    pub ssh_config: Option<PathBuf>,
    /// Только для тестов, в файле не читается: бинарник, который носитель кладёт на раннер
    /// вместо собственного.
    #[serde(skip)]
    pub worker_binary: Option<PathBuf>,
    pub ssh: Vec<RunnerSsh>,
}

/// Одна запись `[[runners.ssh]]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerSsh {
    /// Имя машины-раннера в хабе; корень каталога приходит от хаба.
    pub machine: String,
    /// Псевдоним из `~/.ssh/config`.
    pub ssh: String,
    #[serde(default = "RunnerSsh::default_max_running")]
    pub max_running: u32,
    #[serde(default = "RunnerSsh::default_lease_slots")]
    pub lease_slots: u32,
}

impl RunnerSsh {
    fn default_max_running() -> u32 {
        4
    }

    fn default_lease_slots() -> u32 {
        2
    }

    /// Псевдоним уходит `ssh` отдельным словом: без пробелов и ведущего `-`.
    pub fn valid_alias(alias: &str) -> bool {
        !alias.is_empty()
            && !alias.starts_with('-')
            && alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
    }
}

/// Задачи на месте (`jobs_inplace_v1`): команды оболочки по просьбе хаба.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    /// Сколько задач на месте воркер принимает одновременно; уходит хабу в отчёте машины,
    /// лишнее хаб держит в своей очереди.
    pub max_running: u32,
    /// Предел журнала одной попытки на диске, МиБ: голова, отметка и скользящий хвост.
    pub log_max_mib: u64,
    /// Сколько каталог состояния законченной попытки живёт до уборки.
    #[serde(with = "humantime_serde")]
    pub state_retention: Duration,
    /// Сколько каталог собранных результатов задачи (`job-outputs/<job_id>`) и запись
    /// итога задачи владельца живут до уборки.
    #[serde(with = "humantime_serde")]
    pub output_retention: Duration,
    /// Абсолютный путь к файлу `NAME=value` с переменными прокси для задач; сам воркер
    /// этих переменных не получает. Носитель кладёт его только облачному раннеру.
    pub env_file: Option<PathBuf>,
}

impl JobsConfig {
    pub const MAX_RUNNING_CEILING: u32 = 64;
    pub const LOG_MIB_CEILING: u64 = 64;

    pub fn log_max_bytes(&self) -> u64 {
        self.log_max_mib.clamp(1, Self::LOG_MIB_CEILING) * 1024 * 1024
    }

    pub fn max_running(&self) -> u32 {
        self.max_running.clamp(1, Self::MAX_RUNNING_CEILING)
    }
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_running: 4,
            log_max_mib: 16,
            state_retention: Duration::from_secs(3 * 24 * 3600),
            output_retention: Duration::from_secs(7 * 24 * 3600),
            env_file: None,
        }
    }
}

impl WorkerConfig {
    /// Путь сокета из `hub_dial`; `None`, если ключа нет.
    pub fn hub_dial_path(&self) -> Result<Option<PathBuf>, ConfigErrorCategory> {
        let Some(dial) = &self.hub_dial else {
            return Ok(None);
        };
        let path = dial
            .strip_prefix("unix:")
            .map(Path::new)
            .filter(|path| {
                path.is_absolute()
                    && path.file_name().is_some()
                    && !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
            })
            .ok_or(ConfigErrorCategory::HubDial)?;
        Ok(Some(path.to_path_buf()))
    }

    /// Учётная запись лимитов провайдера этой машины (`claude`, `codex`, `opencode`);
    /// провайдер без таблицы и неизвестное имя дают `owner`.
    pub fn account_for_provider(&self, provider: &str) -> &str {
        let account = match provider {
            "claude" => self.claude_code.as_ref().map(|c| c.account.as_str()),
            "codex" => self.codex.as_ref().map(|c| c.account.as_str()),
            "opencode" => self.opencode.as_ref().map(|c| c.account.as_str()),
            _ => None,
        };
        account.unwrap_or(canterlot_proto::limits::OWNER_ACCOUNT)
    }

    /// То же по имени харнесса, включая синтетические.
    pub fn account_for_harness(&self, harness: &str) -> &str {
        match canterlot_proto::harness::provider_of(harness) {
            Some(provider) => self.account_for_provider(provider),
            None => canterlot_proto::limits::OWNER_ACCOUNT,
        }
    }
}

/// Общая память: какие корни считаются рабочими проектами (домен `work`).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    /// Каталоги рабочих проектов; `~` раскрывается в домашний каталог воркера.
    pub work_roots: Vec<String>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            work_roots: vec!["~/work".into()],
        }
    }
}

/// Файлы секретов: базовый каталог в tmpfs. Явный путь нужен тестам; боевой
/// воркер берёт `$XDG_RUNTIME_DIR/canterlot-secrets`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretConfig {
    pub base_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionConfig {
    pub allowed_workdir_roots: Vec<PathBuf>,
    pub supervisor_exe: Option<PathBuf>,
    #[serde(with = "humantime_serde")]
    pub spawn_timeout: Duration,
    /// Предел параллельной сборки для запускаемых сессий; не перекрывает уже заданный `CARGO_BUILD_JOBS`.
    pub build_jobs: u32,
    /// Приоритет (nice) процессов агентских CLI и их потомков; 0 выключает понижение.
    pub agent_nice: u32,
    pub supervisor: SupervisorTimers,
    /// Серверы MCP владельца, которые делегированная сессия получает рядом с canterlot (D24).
    /// Только серверы на чтение; пустой список — делегат видит один canterlot.
    pub delegate_mcp_servers: Vec<String>,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            allowed_workdir_roots: Vec::new(),
            supervisor_exe: None,
            spawn_timeout: Duration::from_secs(10),
            build_jobs: 6,
            agent_nice: 10,
            supervisor: SupervisorTimers::default(),
            delegate_mcp_servers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerTimers {
    #[serde(with = "humantime_serde")]
    pub handshake: Duration,
    #[serde(with = "humantime_serde")]
    pub http2_keepalive_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub http2_keepalive_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub heartbeat: Duration,
    #[serde(with = "humantime_serde")]
    pub heartbeat_ack: Duration,
    #[serde(with = "humantime_serde")]
    pub range_ack: Duration,
    #[serde(with = "humantime_serde")]
    pub backoff_min: Duration,
    #[serde(with = "humantime_serde")]
    pub backoff_max: Duration,
    #[serde(with = "humantime_serde")]
    pub backoff_reset_after: Duration,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SupervisorTimers {
    #[serde(with = "humantime_serde")]
    pub attach_wait: Duration,
    #[serde(with = "humantime_serde")]
    pub ready_deadline: Duration,
    #[serde(with = "humantime_serde")]
    pub transmit_deadline: Duration,
    #[serde(with = "humantime_serde")]
    pub cli_version_probe: Duration,
    #[serde(with = "humantime_serde")]
    pub stop_grace: Duration,
    #[serde(with = "humantime_serde")]
    pub stop_retry: Duration,
    /// Не читается из worker.toml: короткие значения задают только тесты, а в spec.toml
    /// таблица `[periods]` попадает, лишь когда отличается от боевой.
    #[serde(skip)]
    pub periods: SupervisorPeriods,
}

/// Внутренние периоды сопровождения. Боевые значения — `Default`; тесты укорачивают их,
/// чтобы не высиживать боевой период.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SupervisorPeriods {
    /// Период снимка участников группы: он находит потомков, потерявших метку группы.
    #[serde(with = "humantime_serde")]
    pub group_record: Duration,
    /// Как часто сопровождение проверяет свой сокет IPC и восстанавливает удалённый.
    #[serde(with = "humantime_serde")]
    pub socket_check: Duration,
}

impl Default for SupervisorPeriods {
    fn default() -> Self {
        Self {
            group_record: Duration::from_secs(5),
            socket_check: Duration::from_secs(5),
        }
    }
}

impl Default for SupervisorTimers {
    fn default() -> Self {
        Self {
            attach_wait: Duration::from_secs(30),
            // Claude Code ждёт каждый сервер MCP до 30 с (`MCP_TIMEOUT`), прежде чем прислать init.
            ready_deadline: Duration::from_secs(90),
            transmit_deadline: Duration::from_secs(10),
            cli_version_probe: Duration::from_secs(5),
            stop_grace: Duration::from_secs(5),
            stop_retry: Duration::from_secs(2),
            periods: SupervisorPeriods::default(),
        }
    }
}

impl Default for WorkerTimers {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            http2_keepalive_interval: Duration::from_secs(30),
            http2_keepalive_timeout: Duration::from_secs(10),
            heartbeat: Duration::from_secs(5),
            heartbeat_ack: Duration::from_secs(5),
            range_ack: Duration::from_secs(10),
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(30),
            backoff_reset_after: Duration::from_secs(30),
        }
    }
}

pub struct ConfigError {
    pub path: PathBuf,
    pub category: ConfigErrorCategory,
    pub location: Option<(usize, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigErrorCategory {
    Io(io::ErrorKind),
    Parse,
    HubUrl,
    /// `hub_dial` не вида `unix:<абсолютный путь без ..>`.
    HubDial,
    TokenPermissions,
    UnsupportedTokenPermissions,
    PaidModelPollUnsupported,
    /// Ошибка `[opencode].provider_models`: провайдер и шаблон, если он виноват.
    OpenCodeProviderModels {
        provider: String,
        pattern: Option<String>,
    },
    /// Негодное имя в `[execution].delegate_mcp_servers`.
    DelegateMcpServer {
        name: String,
    },
    /// Негодная метка `account` в таблице харнесса: нужны 1..40 знаков `[a-z0-9-]`.
    Account {
        harness: &'static str,
    },
    /// `[jobs].env_file` не абсолютный путь.
    JobsEnvFile,
}

impl ConfigError {
    fn new(path: &Path, category: ConfigErrorCategory) -> Self {
        Self {
            path: path.to_path_buf(),
            category,
            location: None,
        }
    }
}

impl ConfigError {
    /// Слово для `<data_dir>/fatal`: правка файла конфига исправит всё, кроме чтения диска и прав токена.
    pub fn fatal_reason(&self) -> Option<&'static str> {
        match self.category {
            ConfigErrorCategory::Io(_)
            | ConfigErrorCategory::TokenPermissions
            | ConfigErrorCategory::UnsupportedTokenPermissions => None,
            _ => Some(crate::fatal::CONFIG),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.category {
            ConfigErrorCategory::Io(kind) => {
                write!(
                    f,
                    "configuration I/O error in {}: {kind}",
                    self.path.display()
                )
            }
            ConfigErrorCategory::Parse => {
                write!(f, "invalid worker configuration in {}", self.path.display())?;
                if let Some((line, column)) = self.location {
                    write!(f, " at line {line}, column {column}")?;
                }
                Ok(())
            }
            ConfigErrorCategory::HubUrl => write!(
                f,
                "{}: hub URL must use HTTPS with a host and no credentials",
                self.path.display()
            ),
            ConfigErrorCategory::HubDial => write!(
                f,
                "{}: hub_dial must be unix:<absolute socket path>",
                self.path.display()
            ),
            ConfigErrorCategory::TokenPermissions => write!(
                f,
                "{}: token file must have no group or other permissions",
                self.path.display()
            ),
            ConfigErrorCategory::UnsupportedTokenPermissions => write!(
                f,
                "{}: token permission checks are not supported on this platform",
                self.path.display()
            ),
            ConfigErrorCategory::PaidModelPollUnsupported => write!(
                f,
                "{}: claude_code.allow_paid_model_poll is not supported in the pilot",
                self.path.display()
            ),
            ConfigErrorCategory::OpenCodeProviderModels { provider, pattern } => {
                write!(f, "{}: opencode.provider_models", self.path.display())?;
                match pattern {
                    Some(pattern) => write!(
                        f,
                        " has an invalid pattern {pattern:?} for provider {provider}"
                    ),
                    None => write!(
                        f,
                        " names provider {provider}, which is not listed in opencode.providers"
                    ),
                }
            }
            ConfigErrorCategory::DelegateMcpServer { name } => write!(
                f,
                "{}: execution.delegate_mcp_servers has an invalid server name {name:?}",
                self.path.display()
            ),
            ConfigErrorCategory::Account { harness } => write!(
                f,
                "{}: {harness}.account must be 1 to 40 characters of a-z, 0-9 and '-'",
                self.path.display()
            ),
            ConfigErrorCategory::JobsEnvFile => write!(
                f,
                "{}: jobs.env_file must be an absolute path",
                self.path.display()
            ),
        }
    }
}

impl fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        file_io(Path::new(""), error)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(_: toml::de::Error) -> Self {
        Self::new(Path::new(""), ConfigErrorCategory::Parse)
    }
}

fn file_io(path: &Path, error: io::Error) -> ConfigError {
    ConfigError::new(path, ConfigErrorCategory::Io(error.kind()))
}

pub fn load(path: &Path) -> Result<WorkerConfig, ConfigError> {
    let text = fs::read_to_string(path).map_err(|error| file_io(path, error))?;
    let config: WorkerConfig = toml::from_str(&text).map_err(|error: toml::de::Error| {
        let location = error
            .span()
            .and_then(|span| text.get(..span.start))
            .map(|prefix| {
                let line = prefix.bytes().filter(|&byte| byte == b'\n').count() + 1;
                let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
                (line, column)
            });
        ConfigError {
            location,
            ..ConfigError::new(path, ConfigErrorCategory::Parse)
        }
    })?;
    if config.legacy_ignored.is_some() {
        tracing::warn!("[antigravity] section ignored: Antigravity support was removed");
    }
    let invalid_url = || ConfigError::new(path, ConfigErrorCategory::HubUrl);
    let url = Url::parse(&config.hub_url).map_err(|_| invalid_url())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_url());
    }
    config
        .hub_dial_path()
        .map_err(|category| ConfigError::new(path, category))?;
    if config
        .jobs
        .env_file
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
    {
        return Err(ConfigError::new(path, ConfigErrorCategory::JobsEnvFile));
    }
    if config
        .runners
        .ssh
        .iter()
        .any(|runner| !RunnerSsh::valid_alias(&runner.ssh))
    {
        return Err(ConfigError::new(path, ConfigErrorCategory::Parse));
    }
    // Платный запрос к модели ради лимитов в пилоте не делается: явный отказ вместо тихого пути.
    if config
        .claude_code
        .as_ref()
        .is_some_and(|claude| claude.allow_paid_model_poll)
    {
        return Err(ConfigError::new(
            path,
            ConfigErrorCategory::PaidModelPollUnsupported,
        ));
    }
    for (harness, account) in [
        (
            "claude_code",
            config.claude_code.as_ref().map(|c| c.account.as_str()),
        ),
        ("codex", config.codex.as_ref().map(|c| c.account.as_str())),
        (
            "opencode",
            config.opencode.as_ref().map(|c| c.account.as_str()),
        ),
    ] {
        if account.is_some_and(|account| !canterlot_proto::limits::valid_account(account)) {
            return Err(ConfigError::new(
                path,
                ConfigErrorCategory::Account { harness },
            ));
        }
    }
    if let Some(name) = config
        .execution
        .delegate_mcp_servers
        .iter()
        .find(|name| !valid_delegate_mcp_name(name))
    {
        return Err(ConfigError::new(
            path,
            ConfigErrorCategory::DelegateMcpServer { name: name.clone() },
        ));
    }
    if let Some(opencode) = &config.opencode {
        for (provider, patterns) in &opencode.provider_models {
            if provider != crate::adapter::opencode::catalog::GO_PROVIDER
                && !opencode.providers.iter().any(|p| p == provider)
            {
                return Err(ConfigError::new(
                    path,
                    ConfigErrorCategory::OpenCodeProviderModels {
                        provider: provider.clone(),
                        pattern: None,
                    },
                ));
            }
            for pattern in patterns {
                if crate::adapter::opencode::catalog::pattern_parts(pattern).is_none() {
                    return Err(ConfigError::new(
                        path,
                        ConfigErrorCategory::OpenCodeProviderModels {
                            provider: provider.clone(),
                            pattern: Some(pattern.clone()),
                        },
                    ));
                }
            }
        }
    }
    Ok(config)
}

/// Имя сервера MCP владельца для делегата: непустое, `[A-Za-z0-9_-]`, не `canterlot`.
pub fn valid_delegate_mcp_name(name: &str) -> bool {
    !name.is_empty()
        && name != crate::adapter::canterlot_mcp::SERVER_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn read_token(path: &Path) -> Result<String, ConfigError> {
    let mut file = File::open(path).map_err(|error| file_io(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|error| file_io(path, error))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(ConfigError::new(
                path,
                ConfigErrorCategory::TokenPermissions,
            ));
        }
    }
    #[cfg(not(unix))]
    return Err(ConfigError::new(
        path,
        ConfigErrorCategory::UnsupportedTokenPermissions,
    ));

    let mut token = String::new();
    file.read_to_string(&mut token)
        .map_err(|error| file_io(path, error))?;
    Ok(token.trim().to_owned())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MachineConfig {
    #[serde(with = "humantime_serde")]
    pub metrics_interval: Duration,
    /// Путь к CodexBar; без него ищется `codexbar` в `PATH` и в `~/.local/bin`.
    pub codexbar_path: Option<PathBuf>,
    /// Как часто опрашивать лимиты через CodexBar, не меньше 15 минут: точка usage ограничена по частоте.
    #[serde(with = "humantime_serde")]
    pub limits_poll_interval: Duration,
    /// Ёмкость очереди тяжёлых задач: `build` занимает слот, `gate` — все.
    pub lease_slots: u32,
    /// Где сторож ищет крупные каталоги сборки кроме корня рабочих копий. Нет ключа —
    /// домашний каталог; `/` не обходится никогда.
    pub hotspot_roots: Option<Vec<PathBuf>>,
    /// Свободного места меньше этого — отчёт сторожа уходит хабу сразу, не дожидаясь
    /// пятиминутного. Совпадает с порогом хаба `[machines].disk_warn_free_gb`.
    pub disk_warn_free_gb: u64,
    /// Живая, но простаивающая рабочая копия теряет `incremental`, `target/lint` и
    /// старые тестовые бинарники, когда сборка не писала дольше этого.
    pub idle_cache_hours: u64,
    /// Рабочая копия без живого исполнения теряет весь `target` после стольких часов.
    pub released_target_hours: u64,
}

impl MachineConfig {
    pub const MIN_LIMITS_POLL_INTERVAL: Duration = Duration::from_secs(900);

    pub fn hotspot_roots(&self) -> Vec<PathBuf> {
        match &self.hotspot_roots {
            Some(roots) => roots.clone(),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .into_iter()
                .collect(),
        }
    }

    pub fn limits_poll_interval(&self) -> Duration {
        self.limits_poll_interval
            .max(Self::MIN_LIMITS_POLL_INTERVAL)
    }
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            metrics_interval: Duration::from_secs(30),
            codexbar_path: None,
            limits_poll_interval: Self::MIN_LIMITS_POLL_INTERVAL,
            lease_slots: 2,
            hotspot_roots: None,
            disk_warn_free_gb: 30,
            idle_cache_hours: 3,
            released_target_hours: 6,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeCodeConfig {
    #[serde(default = "default_claude_executable")]
    pub executable: PathBuf,
    // claude_md_excludes удалён планом 2c (задача 6): список исключений
    // вычисляется при каждом запуске (owner_instruction_excludes).
    pub allow_paid_model_poll: bool,
    /// Явный `~/.claude.json` владельца (тесты); иначе путь из окружения.
    #[serde(skip, default)]
    pub owner_user_config: Option<PathBuf>,
    /// Учётная запись, которой принадлежат лимиты харнесса.
    #[serde(default = "default_account", skip_serializing_if = "is_owner_account")]
    pub account: String,
    /// Сроки адаптера. Не читаются из worker.toml и не пишутся в спеку: короче задают тесты.
    #[serde(skip)]
    pub timers: ClaudeCodeTimers,
}

/// Сроки адаптера Claude Code; боевые значения — `Default`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCodeTimers {
    /// Ответ CLI на управляющий запрос (прерывание, смена настроек), после него — запасной путь.
    pub control: Duration,
    /// Перезапуск между ходами: выход старого CLI после EOF stdin, затем SIGTERM группе.
    pub restart_after_eof: Duration,
    /// Выход после SIGTERM при перезапуске, затем SIGKILL.
    pub restart_after_term: Duration,
}

impl Default for ClaudeCodeTimers {
    fn default() -> Self {
        Self {
            control: crate::adapter::claude_code::input::CONTROL_TIMEOUT,
            restart_after_eof: Duration::from_secs(5),
            restart_after_term: Duration::from_secs(2),
        }
    }
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("claude"),
            allow_paid_model_poll: false,
            owner_user_config: None,
            account: default_account(),
            timers: ClaudeCodeTimers::default(),
        }
    }
}

/// Учётная запись по умолчанию: лимиты единственной подписки владельца.
pub fn default_account() -> String {
    canterlot_proto::limits::OWNER_ACCOUNT.to_owned()
}

/// Спека сопровождения для `owner` остаётся прежней: ключ пишется только для другой метки.
fn is_owner_account(account: &String) -> bool {
    account == canterlot_proto::limits::OWNER_ACCOUNT
}

/// Пользовательский каталог Claude Code: `CLAUDE_CONFIG_DIR`, если задан
/// абсолютным путём (окружение воркера доходит до CLI), иначе `~/.claude`.
pub fn claude_config_dir(home: &std::path::Path, config_dir: Option<&std::ffi::OsStr>) -> PathBuf {
    config_dir
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".claude"))
}

/// Пользовательский файл Claude Code с серверами MCP (`~/.claude.json`): в
/// `CLAUDE_CONFIG_DIR`, если он задан абсолютным путём, иначе в домашнем каталоге.
pub fn claude_user_config_file(
    home: &std::path::Path,
    config_dir: Option<&std::ffi::OsStr>,
) -> PathBuf {
    config_dir
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.to_path_buf())
        .join(".claude.json")
}

fn default_claude_executable() -> PathBuf {
    PathBuf::from("claude")
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexConfig {
    #[serde(default = "default_codex_executable")]
    pub executable: PathBuf,
    #[serde(default = "default_codex_rate_limits_poll_interval_s")]
    pub rate_limits_poll_interval_s: u64,
    /// Корень собственного CODEX_HOME canterlot; по умолчанию <data_dir>/codex-home.
    #[serde(default)]
    pub home_dir: Option<PathBuf>,
    /// Явный корень дома владельца (тесты, флот без env); иначе $CODEX_HOME или $HOME/.codex.
    #[serde(skip, default)]
    pub owner_home: Option<PathBuf>,
    /// Политика временных каталогов для дома canterlot; тесты ставят Allow.
    #[serde(skip, default)]
    pub home_policy: crate::adapter::codex::home::TempDirPolicy,
    /// Учётная запись, которой принадлежат лимиты харнесса.
    #[serde(default = "default_account", skip_serializing_if = "is_owner_account")]
    pub account: String,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("codex"),
            rate_limits_poll_interval_s: default_codex_rate_limits_poll_interval_s(),
            home_dir: None,
            owner_home: None,
            home_policy: crate::adapter::codex::home::TempDirPolicy::Refuse,
            account: default_account(),
        }
    }
}

impl CodexConfig {
    pub const MIN_RATE_LIMITS_POLL_INTERVAL_S: u64 = 60;

    pub fn rate_limits_poll_interval(&self) -> Duration {
        Duration::from_secs(
            self.rate_limits_poll_interval_s
                .max(Self::MIN_RATE_LIMITS_POLL_INTERVAL_S),
        )
    }
}

fn default_codex_rate_limits_poll_interval_s() -> u64 {
    300
}

fn default_codex_executable() -> PathBuf {
    PathBuf::from("codex")
}

/// Таблица `[opencode]`: без неё харнесс не показывается и не запускается.
#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenCodeConfig {
    pub executable: PathBuf,
    /// Корень данных canterlot; по умолчанию <data_dir>/opencode. База владельца туда не попадает.
    pub data_root: Option<PathBuf>,
    /// Каталоги, в которых нельзя держать базу, сверх `opencode` в данных владельца.
    pub refuse_paths: Vec<PathBuf>,
    /// Провайдеры в каталоге моделей сверх `opencode-go`.
    pub providers: Vec<String>,
    /// Фильтр моделей по провайдеру: пустой список — ничего, отсутствие записи — все.
    pub provider_models: BTreeMap<String, Vec<String>>,
    /// Модель, которая идёт первой в каталоге; без неё порядок задаёт OpenCode.
    pub default_model: Option<String>,
    /// Каталог конфигурации владельца; по умолчанию `$XDG_CONFIG_HOME/opencode`.
    pub owner_config: Option<PathBuf>,
    /// Учётная запись, которой принадлежат лимиты харнесса.
    #[serde(skip_serializing_if = "is_owner_account")]
    pub account: String,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("opencode"),
            data_root: None,
            refuse_paths: Vec::new(),
            providers: Vec::new(),
            provider_models: BTreeMap::new(),
            default_model: None,
            owner_config: None,
            account: default_account(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerAttachmentsConfig {
    pub fetch_parallelism: usize,
    #[serde(with = "humantime_serde")]
    pub fetch_retry: Duration,
    pub fetch_attempts: u32,
    #[serde(with = "humantime_serde")]
    pub fetch_backoff_cap: Duration,
}

impl Default for WorkerAttachmentsConfig {
    fn default() -> Self {
        Self {
            fetch_parallelism: 2,
            fetch_retry: Duration::from_secs(2),
            fetch_attempts: 5,
            fetch_backoff_cap: Duration::from_secs(30),
        }
    }
}

impl WorkerAttachmentsConfig {
    pub(crate) fn valid(&self) -> bool {
        (1..=2).contains(&self.fetch_parallelism)
            && self.fetch_attempts > 0
            && !self.fetch_retry.is_zero()
            && !self.fetch_backoff_cap.is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Боевые сроки и периоды: тесты подставляют короткие, поэтому изменение боевого
    /// значения должно быть намеренным и менять эту проверку.
    #[test]
    fn prod_timer_defaults_are_pinned() {
        let worker = WorkerTimers::default();
        assert_eq!(
            [
                worker.handshake,
                worker.http2_keepalive_interval,
                worker.http2_keepalive_timeout,
                worker.heartbeat,
                worker.heartbeat_ack,
                worker.range_ack,
                worker.backoff_min,
                worker.backoff_max,
                worker.backoff_reset_after,
            ],
            [
                Duration::from_secs(10),
                Duration::from_secs(30),
                Duration::from_secs(10),
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_millis(250),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
        let supervisor = SupervisorTimers::default();
        assert_eq!(
            [
                supervisor.attach_wait,
                supervisor.ready_deadline,
                supervisor.transmit_deadline,
                supervisor.cli_version_probe,
                supervisor.stop_grace,
                supervisor.stop_retry,
                supervisor.periods.group_record,
                supervisor.periods.socket_check,
            ],
            [
                Duration::from_secs(30),
                Duration::from_secs(90),
                Duration::from_secs(10),
                Duration::from_secs(5),
                Duration::from_secs(5),
                Duration::from_secs(2),
                Duration::from_secs(5),
                Duration::from_secs(5),
            ]
        );
        assert_eq!(
            ClaudeCodeTimers::default(),
            ClaudeCodeTimers {
                control: Duration::from_secs(10),
                restart_after_eof: Duration::from_secs(5),
                restart_after_term: Duration::from_secs(2),
            }
        );
    }

    #[test]
    fn claude_timers_are_neither_read_nor_written() {
        let parsed: ClaudeCodeConfig = toml::from_str("executable = \"claude\"").unwrap();
        assert_eq!(parsed.timers, ClaudeCodeTimers::default());
        assert!(toml::from_str::<ClaudeCodeConfig>("[timers]\ncontrol = \"1s\"").is_err());
        let short = ClaudeCodeConfig {
            timers: ClaudeCodeTimers {
                control: Duration::from_millis(1),
                ..ClaudeCodeTimers::default()
            },
            ..ClaudeCodeConfig::default()
        };
        assert!(!toml::to_string(&short).unwrap().contains("timers"));
    }

    #[test]
    fn supervisor_periods_are_not_read_from_worker_toml() {
        let text = r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "00000000-0000-0000-0000-000000000001"
installation_id = "00000000-0000-0000-0000-000000000002"
restore_generation = "00000000-0000-0000-0000-000000000003"
data_dir = "/tmp/store"

[execution.supervisor.periods]
group_record = "1ms"
"#;
        assert!(toml::from_str::<WorkerConfig>(text).is_err());
    }

    #[test]
    fn ready_deadline_outlasts_the_cli_mcp_startup_timeout() {
        // Один зависший сервер MCP задерживает init Claude Code на 30 с.
        assert!(SupervisorTimers::default().ready_deadline >= Duration::from_secs(60));
    }

    #[test]
    fn jobs_section_defaults_and_bounds() {
        let base = r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "00000000-0000-0000-0000-000000000001"
installation_id = "00000000-0000-0000-0000-000000000002"
restore_generation = "00000000-0000-0000-0000-000000000003"
data_dir = "/tmp/store"
"#;
        let defaults: WorkerConfig = toml::from_str(base).unwrap();
        assert_eq!(defaults.jobs.max_running(), 4);
        assert_eq!(defaults.jobs.log_max_bytes(), 16 * 1024 * 1024);
        assert_eq!(
            defaults.jobs.state_retention,
            Duration::from_secs(3 * 24 * 3600)
        );
        assert_eq!(
            defaults.jobs.output_retention,
            Duration::from_secs(7 * 24 * 3600)
        );
        let tuned: WorkerConfig = toml::from_str(&format!(
            "{base}\n[jobs]\nmax_running = 2\nlog_max_mib = 1\nstate_retention = \"12h\"\noutput_retention = \"2d\"\n"
        ))
        .unwrap();
        assert_eq!(
            tuned.jobs.output_retention,
            Duration::from_secs(2 * 24 * 3600)
        );
        assert_eq!(tuned.jobs.max_running(), 2);
        assert_eq!(tuned.jobs.log_max_bytes(), 1024 * 1024);
        assert_eq!(tuned.jobs.state_retention, Duration::from_secs(12 * 3600));
        // Ноль и огромные значения не ломают воркер: границы подтягиваются.
        let wild: WorkerConfig = toml::from_str(&format!(
            "{base}\n[jobs]\nmax_running = 0\nlog_max_mib = 9999\n"
        ))
        .unwrap();
        assert_eq!(wild.jobs.max_running(), 1);
        assert_eq!(wild.jobs.log_max_bytes(), 64 * 1024 * 1024);
        assert!(
            toml::from_str::<WorkerConfig>(&format!("{base}\n[jobs]\nlost_grace = 1\n")).is_err()
        );
    }

    #[test]
    fn jobs_env_file_must_be_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.toml");
        let base = r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "00000000-0000-0000-0000-000000000001"
installation_id = "00000000-0000-0000-0000-000000000002"
restore_generation = "00000000-0000-0000-0000-000000000003"
data_dir = "/tmp/store"
"#;
        std::fs::write(
            &path,
            format!("{base}\n[jobs]\nenv_file = \"/etc/canterlot/egress.env\"\n"),
        )
        .unwrap();
        let config = load(&path).unwrap();
        assert_eq!(
            config.jobs.env_file.as_deref(),
            Some(Path::new("/etc/canterlot/egress.env"))
        );
        std::fs::write(
            &path,
            format!("{base}\n[jobs]\nenv_file = \"egress.env\"\n"),
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            matches!(error.category, ConfigErrorCategory::JobsEnvFile),
            "{error:?}"
        );
    }

    #[test]
    fn runners_test_hooks_cannot_be_set_from_a_production_config() {
        let base = r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "00000000-0000-0000-0000-000000000001"
installation_id = "00000000-0000-0000-0000-000000000002"
restore_generation = "00000000-0000-0000-0000-000000000003"
data_dir = "/tmp/store"
"#;
        let plain =
            format!("{base}\n[[runners.ssh]]\nmachine = \"runner-a\"\nssh = \"build-host\"\n");
        let config: WorkerConfig = toml::from_str(&plain).unwrap();
        let settings = crate::carrier::CarrierSettings::from_config(&config);
        assert!(settings.ssh_config.is_none() && settings.worker_binary.is_none());
        // Подмена ssh и бинарника раннера задаётся только полями структуры из тестов
        // (`#[serde(skip)]`), но не файлом: ключ в рабочем конфиге отвергается.
        for key in ["ssh_config", "worker_binary"] {
            let text = format!("{base}\n[runners]\n{key} = \"/tmp/x\"\n");
            assert!(
                toml::from_str::<WorkerConfig>(&text).is_err(),
                "[runners].{key} must be rejected"
            );
        }
    }

    #[test]
    fn legacy_antigravity_section_is_accepted_but_other_unknown_sections_are_rejected() {
        let base = r#"
hub_url = "https://localhost:1"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "00000000-0000-0000-0000-000000000001"
installation_id = "00000000-0000-0000-0000-000000000002"
restore_generation = "00000000-0000-0000-0000-000000000003"
data_dir = "/tmp/store"
"#;
        let legacy = format!(
            "{base}\n[antigravity]\nenabled = true\nbinary = \"/bin/false\"\nwhatever = 42\n"
        );
        let config: WorkerConfig = toml::from_str(&legacy).unwrap();
        assert!(config.legacy_ignored.is_some());
        let unknown = format!("{base}\n[unknown_section]\nkey = 1\n");
        assert!(toml::from_str::<WorkerConfig>(&unknown).is_err());
    }
}
