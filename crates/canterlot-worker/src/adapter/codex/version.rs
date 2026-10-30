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

// Слово версии без `codex-cli`, с суффиксами semver.
fn version_token(output: &str) -> Result<&str, VersionError> {
    let text = output.strip_prefix("codex-cli").unwrap_or(output).trim();
    text.split_whitespace()
        .next()
        .ok_or(VersionError::Unparsable)
}

// Ядро `x.y.z` и признак предварительной сборки (`-…`); метаданные `+…` не влияют.
fn split_version(token: &str) -> Result<((u32, u32, u32), bool), VersionError> {
    let token = token.split_once('+').map_or(token, |(core, _)| core);
    let (core, pre) = match token.split_once('-') {
        Some((core, _)) => (core, true),
        None => (token, false),
    };
    let mut parts = core.split('.');
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
    Ok(((nums[0], nums[1], nums[2]), pre))
}

// Разбирает и `codex-cli 0.164.0`, и голое `0.164.0`, и `0.165.0-alpha.2`.
pub fn parse_version(output: &str) -> Result<(u32, u32, u32), VersionError> {
    split_version(version_token(output)?).map(|(core, _)| core)
}

pub async fn check(executable: &Path) -> Result<CliVersion, VersionError> {
    let out = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.env_clear()
            .envs(super::process::build_env(
                std::env::vars_os(),
                crate::adapter::InstructionMode::Owner,
                None,
            ))
            .arg("--version")
            .kill_on_drop(true);
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
    let token = version_token(&text)?;
    let (v, pre) = split_version(token)?;
    let label = if pre {
        token
            .split_once('+')
            .map_or(token, |(core, _)| core)
            .to_owned()
    } else {
        format!("{}.{}.{}", v.0, v.1, v.2)
    };
    // Предварительная сборка младше своего выпуска: x.y.z-pre < x.y.z.
    if v < MIN_SUPPORTED_VERSION || (pre && v == MIN_SUPPORTED_VERSION) {
        return Err(VersionError::BelowMinimum(label));
    }
    if !pre && VERIFIED_VERSIONS.contains(&v) {
        Ok(CliVersion::Verified(label))
    } else {
        Ok(CliVersion::UntestedNewer(label))
    }
}
