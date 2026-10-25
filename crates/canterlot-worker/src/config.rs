use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
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
            heartbeat: Duration::from_secs(5),
            heartbeat_ack: Duration::from_secs(5),
            range_ack: Duration::from_secs(10),
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(30),
            backoff_reset_after: Duration::from_secs(30),
        }
    }
}

pub enum ConfigError {
    // Старые варианты сохранены для совместимости; load и From отбрасывают исходные ошибки.
    Io(io::Error),
    Toml(toml::de::Error),
    InvalidHubUrl,
    TokenPermissions,
    UnsupportedTokenPermissions,
    FileIo {
        path: PathBuf,
        kind: io::ErrorKind,
    },
    Parse {
        path: PathBuf,
        location: Option<(usize, usize)>,
    },
    HubUrl {
        path: PathBuf,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "configuration I/O error: {}", error.kind()),
            Self::Toml(_) => f.write_str("invalid worker configuration"),
            Self::InvalidHubUrl => {
                f.write_str("hub URL must use HTTPS with a host and no credentials")
            }
            Self::TokenPermissions => {
                f.write_str("token file must have no group or other permissions")
            }
            Self::UnsupportedTokenPermissions => {
                f.write_str("token permission checks are not supported on this platform")
            }
            Self::FileIo { path, kind } => {
                write!(f, "configuration I/O error in {}: {kind}", path.display())
            }
            Self::Parse { path, location } => {
                write!(f, "invalid worker configuration in {}", path.display())?;
                if let Some((line, column)) = location {
                    write!(f, " at line {line}, column {column}")?;
                }
                Ok(())
            }
            Self::HubUrl { path } => write!(f, "{}: {}", path.display(), Self::InvalidHubUrl),
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
        Self::Io(error.kind().into())
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(_: toml::de::Error) -> Self {
        Self::Parse {
            path: PathBuf::new(),
            location: None,
        }
    }
}

fn file_io(path: &Path, error: io::Error) -> ConfigError {
    ConfigError::FileIo {
        path: path.to_path_buf(),
        kind: error.kind(),
    }
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
        ConfigError::Parse {
            path: path.to_path_buf(),
            location,
        }
    })?;
    let invalid_url = || ConfigError::HubUrl {
        path: path.to_path_buf(),
    };
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
            return Err(ConfigError::TokenPermissions);
        }
    }
    #[cfg(not(unix))]
    return Err(ConfigError::UnsupportedTokenPermissions);

    let mut token = String::new();
    file.read_to_string(&mut token)
        .map_err(|error| file_io(path, error))?;
    Ok(token.trim().to_owned())
}

fn milliseconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}
