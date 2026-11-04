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
    pub ca_cert: PathBuf,
    pub token_file: PathBuf,
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub timers: WorkerTimers,
    #[serde(default)]
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
    pub supervisor: SupervisorTimers,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            allowed_workdir_roots: Vec::new(),
            supervisor_exe: None,
            spawn_timeout: Duration::from_secs(10),
            build_jobs: 6,
            supervisor: SupervisorTimers::default(),
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
    pub stop_grace: Duration,
    #[serde(with = "humantime_serde")]
    pub stop_retry: Duration,
}

impl Default for SupervisorTimers {
    fn default() -> Self {
        Self {
            attach_wait: Duration::from_secs(30),
            ready_deadline: Duration::from_secs(30),
            transmit_deadline: Duration::from_secs(10),
            stop_grace: Duration::from_secs(5),
            stop_retry: Duration::from_secs(2),
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
    TokenPermissions,
    UnsupportedTokenPermissions,
    PaidModelPollUnsupported,
    /// Ошибка `[opencode].provider_models`: провайдер и шаблон, если он виноват.
    OpenCodeProviderModels {
        provider: String,
        pattern: Option<String>,
    },
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
    let invalid_url = || ConfigError::new(path, ConfigErrorCategory::HubUrl);
    let url = Url::parse(&config.hub_url).map_err(|_| invalid_url())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_url());
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
}

impl MachineConfig {
    pub const MIN_LIMITS_POLL_INTERVAL: Duration = Duration::from_secs(900);

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
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("claude"),
            allow_paid_model_poll: false,
        }
    }
}

/// Пользовательский каталог Claude Code: `CLAUDE_CONFIG_DIR`, если задан
/// абсолютным путём (окружение воркера доходит до CLI), иначе `~/.claude`.
pub fn claude_config_dir(home: &std::path::Path, config_dir: Option<&std::ffi::OsStr>) -> PathBuf {
    config_dir
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| home.join(".claude"))
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
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("codex"),
            rate_limits_poll_interval_s: default_codex_rate_limits_poll_interval_s(),
            home_dir: None,
            owner_home: None,
            home_policy: crate::adapter::codex::home::TempDirPolicy::Refuse,
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
