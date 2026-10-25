use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use url::Url;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
pub struct WorkerConfig {
    pub hub_url: String,
    pub ca_cert: PathBuf,
    pub token_file: PathBuf,
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub timers: WorkerTimers,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct WorkerTimers {
    #[serde(deserialize_with = "milliseconds")]
    pub handshake: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub http2_keepalive_interval: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub http2_keepalive_timeout: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub heartbeat: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub heartbeat_ack: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub range_ack: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub backoff_min: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub backoff_max: Duration,
    #[serde(deserialize_with = "milliseconds")]
    pub backoff_reset_after: Duration,
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

fn milliseconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}
