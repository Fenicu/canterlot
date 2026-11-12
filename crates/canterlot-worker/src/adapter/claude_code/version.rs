use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

pub use crate::adapter::cli_version::{CliVersion, VersionError};
use crate::adapter::cli_version::{DEFAULT_PROBE_DEADLINE, cached_check};
use crate::adapter::codex::process::GroupKill;
use crate::adapter::{reaped, spawn_with_retry_io};

const OUTPUT_CAP: u64 = 64 * 1024;

pub const MIN_SUPPORTED_VERSION: (u32, u32, u32) = (2, 1, 311);
pub const VERIFIED_VERSIONS: &[(u32, u32, u32)] = &[(2, 1, 311)];

/// Первая версия, на которой `--prompt-suggestions` проверен вживую; старее флаг не передаётся.
pub const PROMPT_SUGGESTIONS_MIN_VERSION: (u32, u32, u32) = (2, 1, 317);

pub fn supports_prompt_suggestions(version: &CliVersion) -> bool {
    parse_version(version.label()).is_ok_and(|v| v >= PROMPT_SUGGESTIONS_MIN_VERSION)
}

pub fn parse_version(output: &str) -> Result<(u32, u32, u32), VersionError> {
    let token = output
        .split_whitespace()
        .next()
        .ok_or(VersionError::Unparsable)?;
    let mut parts = token.split('.');
    let a = parts
        .next()
        .ok_or(VersionError::Unparsable)?
        .parse::<u32>()
        .map_err(|_| VersionError::Unparsable)?;
    let b = parts
        .next()
        .ok_or(VersionError::Unparsable)?
        .parse::<u32>()
        .map_err(|_| VersionError::Unparsable)?;
    let c = parts
        .next()
        .ok_or(VersionError::Unparsable)?
        .parse::<u32>()
        .map_err(|_| VersionError::Unparsable)?;
    Ok((a, b, c))
}

pub async fn check(executable: &Path) -> Result<CliVersion, VersionError> {
    check_with_deadline(executable, DEFAULT_PROBE_DEADLINE).await
}

/// Проба старта: кэш по бинарнику и повтор только по таймауту.
pub async fn check_with_deadline(
    executable: &Path,
    first: Duration,
) -> Result<CliVersion, VersionError> {
    check_with_cache(executable, first, None).await
}

/// То же с общим на процессы файлом кэша.
pub async fn check_with_cache(
    executable: &Path,
    first: Duration,
    cache: Option<&Path>,
) -> Result<CliVersion, VersionError> {
    cached_check(executable, first, cache, |limit| {
        check_within(executable, limit)
    })
    .await
}

pub async fn check_within(executable: &Path, limit: Duration) -> Result<CliVersion, VersionError> {
    let mut child = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        async move { cmd.spawn() }
    })
    .await
    {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(VersionError::NotFound);
        }
        Err(_) => return Err(VersionError::Failed),
    };
    let mut group = GroupKill::of(&child);
    let mut stdout = child.stdout.take().ok_or(VersionError::Failed)?;
    let work = async {
        let mut text = Vec::new();
        (&mut stdout)
            .take(OUTPUT_CAP)
            .read_to_end(&mut text)
            .await?;
        // Обход сдерживания мог забрать зомби: вывод уже прочитан, судит он.
        let status = match child.wait().await {
            Ok(status) => Some(status),
            Err(error) if reaped(&error) => None,
            Err(error) => return Err(error),
        };
        Ok::<_, std::io::Error>((status, text))
    };
    let (status, output) = match timeout(limit, work).await {
        Ok(Ok(out)) => out,
        Ok(Err(_)) => return Err(VersionError::Failed),
        Err(_) => return Err(VersionError::Timeout),
    };
    group.disarm();
    if status.is_some_and(|status| !status.success()) {
        return Err(VersionError::Failed);
    }
    let text = String::from_utf8_lossy(&output);
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
