use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use thiserror::Error;
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

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid worker configuration: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("hub URL must use HTTPS with a host and no credentials")]
    InvalidHubUrl,
    #[error("token file must have no group or other permissions")]
    TokenPermissions,
    #[error("token permission checks are not supported on this platform")]
    UnsupportedTokenPermissions,
}

pub fn load(path: &Path) -> Result<WorkerConfig, ConfigError> {
    let config: WorkerConfig = toml::from_str(&fs::read_to_string(path)?)?;
    let url = Url::parse(&config.hub_url).map_err(|_| ConfigError::InvalidHubUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ConfigError::InvalidHubUrl);
    }
    Ok(config)
}

pub fn read_token(path: &Path) -> Result<String, ConfigError> {
    let mut file = File::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(ConfigError::TokenPermissions);
        }
    }
    #[cfg(not(unix))]
    return Err(ConfigError::UnsupportedTokenPermissions);

    let mut token = String::new();
    file.read_to_string(&mut token)?;
    Ok(token.trim().to_owned())
}

fn milliseconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}
