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
    pub instruction_profiles: Vec<ProfileConfig>,
    #[serde(default)]
    pub attachments: WorkerAttachmentsConfig,
    #[serde(default)]
    pub claude_code: Option<ClaudeCodeConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionConfig {
    pub allowed_workdir_roots: Vec<PathBuf>,
    pub supervisor_exe: Option<PathBuf>,
    #[serde(with = "humantime_serde")]
    pub spawn_timeout: Duration,
    pub supervisor: SupervisorTimers,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            allowed_workdir_roots: Vec::new(),
            supervisor_exe: None,
            spawn_timeout: Duration::from_secs(10),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigErrorCategory {
    Io(io::ErrorKind),
    Parse,
    HubUrl,
    TokenPermissions,
    UnsupportedTokenPermissions,
    PaidModelPollUnsupported,
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
        match self.category {
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
    let mut names = std::collections::BTreeSet::new();
    if config
        .instruction_profiles
        .iter()
        .any(|profile| profile.name == "owner" || !names.insert(&profile.name))
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
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            metrics_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    pub name: String,
    pub title: String,
    pub description: String,
    pub harnesses: Vec<String>,
    #[serde(default)]
    pub instructions_file: Option<PathBuf>,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeCodeConfig {
    #[serde(default = "default_claude_executable")]
    pub executable: PathBuf,
    #[serde(default)]
    pub claude_md_excludes: Vec<PathBuf>,
    pub oauth_poll_interval_s: u64,
    pub credentials_path: Option<PathBuf>,
    pub allow_paid_model_poll: bool,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("claude"),
            claude_md_excludes: Vec::new(),
            oauth_poll_interval_s: 1800,
            credentials_path: None,
            allow_paid_model_poll: false,
        }
    }
}

impl ClaudeCodeConfig {
    pub const MIN_OAUTH_POLL_INTERVAL_S: u64 = 900;

    pub fn oauth_poll_interval(&self) -> Duration {
        Duration::from_secs(
            self.oauth_poll_interval_s
                .max(Self::MIN_OAUTH_POLL_INTERVAL_S),
        )
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.credentials_path.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".claude/.credentials.json")
        })
    }
}

fn default_claude_executable() -> PathBuf {
    PathBuf::from("claude")
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
