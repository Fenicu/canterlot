use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::adapter::spawn_with_retry_io;

// Минимальная проверенная версия CLI.
pub const MIN_SUPPORTED_VERSION: (u32, u32, u32) = (0, 164, 0);
pub const VERIFIED_VERSIONS: &[(u32, u32, u32)] = &[(0, 164, 0)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliVersion {
    Verified(String),
    UntestedNewer(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    NotFound,
    Failed,
    Unparsable,
    BelowMinimum(String),
}

// Разбирает и `codex-cli 0.164.0`, и голое `0.164.0`.
pub fn parse_version(output: &str) -> Result<(u32, u32, u32), VersionError> {
    let text = output.strip_prefix("codex-cli").unwrap_or(output).trim();
    let token = text
        .split_whitespace()
        .next()
        .ok_or(VersionError::Unparsable)?;
    let mut parts = token.split('.');
    let mut nums = [0u32; 3];
    for slot in &mut nums {
        *slot = parts
            .next()
            .ok_or(VersionError::Unparsable)?
            .parse::<u32>()
            .map_err(|_| VersionError::Unparsable)?;
    }
    if parts.next().is_some() {
        return Err(VersionError::Unparsable);
    }
    Ok((nums[0], nums[1], nums[2]))
}

pub async fn check(executable: &Path) -> Result<CliVersion, VersionError> {
    let out = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.arg("--version").kill_on_drop(true);
        async move {
            match timeout(Duration::from_secs(5), cmd.output()).await {
                Ok(v) => v,
                Err(_) => Err(std::io::Error::other("timeout")),
            }
        }
    })
    .await
    {
        Ok(o) => o,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Err(VersionError::NotFound);
            }
            if err.to_string() == "timeout" {
                return Err(VersionError::Failed);
            }
            return Err(VersionError::Failed);
        }
    };
    if !out.status.success() {
        return Err(VersionError::Failed);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let v = parse_version(&text).map_err(|_| VersionError::Unparsable)?;
    let label = format!("{}.{}.{}", v.0, v.1, v.2);
    if v < MIN_SUPPORTED_VERSION {
        return Err(VersionError::BelowMinimum(label));
    }
    if VERIFIED_VERSIONS.contains(&v) {
        Ok(CliVersion::Verified(label))
    } else {
        Ok(CliVersion::UntestedNewer(label))
    }
}
