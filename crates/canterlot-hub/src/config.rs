use std::{
    fmt, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

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
        ] {
            if path.is_relative() {
                *path = base.join(&*path);
            }
        }
        Ok(config)
    }
}
