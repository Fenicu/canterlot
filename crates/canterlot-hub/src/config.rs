use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Deserializer};

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
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HubTimers {
    #[serde(deserialize_with = "seconds")]
    pub handshake: Duration,
    #[serde(deserialize_with = "seconds")]
    pub degraded_after: Duration,
    #[serde(deserialize_with = "seconds")]
    pub unavailable_after: Duration,
    #[serde(deserialize_with = "seconds")]
    pub command_stored_receipt: Duration,
    #[serde(deserialize_with = "seconds")]
    pub dispatch_sweep: Duration,
}

impl Default for HubTimers {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(10),
            degraded_after: Duration::from_secs(15),
            unavailable_after: Duration::from_secs(30),
            command_stored_receipt: Duration::from_secs(5),
            dispatch_sweep: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config_io_error")]
    Io(#[from] std::io::Error),
    #[error("config_invalid")]
    Invalid,
}

impl HubConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let mut config: Self =
            toml::from_str(&fs::read_to_string(path)?).map_err(|_| ConfigError::Invalid)?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        for path in [
            &mut config.database_url_file,
            &mut config.tls_cert,
            &mut config.tls_key,
            &mut config.manifest,
        ] {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        Ok(config)
    }
}

fn seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_secs)
}
