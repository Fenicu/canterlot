use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::process::GroupKill;
pub use crate::adapter::cli_version::{CliVersion, VersionError};
use crate::adapter::{cli_version::VersionPolicy, reaped, spawn_with_retry_io};

const OUTPUT_CAP: u64 = 64 * 1024;

// Минимальная проверенная версия CLI.
pub const MIN_SUPPORTED_VERSION: (u32, u32, u32) = (0, 164, 0);
pub const VERIFIED_VERSIONS: &[(u32, u32, u32)] = &[(0, 164, 0), (0, 167, 0)];

const POLICY: VersionPolicy = VersionPolicy {
    prefix: "codex-cli",
    minimum: MIN_SUPPORTED_VERSION,
    verified: VERIFIED_VERSIONS,
};

// Разбирает и `codex-cli 0.164.0`, и голое `0.164.0`, и `0.165.0-alpha.2`.
pub fn parse_version(output: &str) -> Result<(u32, u32, u32), VersionError> {
    POLICY.parse(output)
}

pub async fn check(executable: &Path) -> Result<CliVersion, VersionError> {
    check_within(executable, Duration::from_secs(5)).await
}

pub async fn check_within(executable: &Path, limit: Duration) -> Result<CliVersion, VersionError> {
    let mut child = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.env_clear()
            .envs(super::process::build_env(
                std::env::vars_os(),
                crate::adapter::InstructionMode::Owner,
                None,
            ))
            .arg("--version")
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
    // Тайм-аут и ошибка чтения убивают всю группу, а не только обёртку.
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
        _ => return Err(VersionError::Failed),
    };
    group.disarm();
    if status.is_some_and(|status| !status.success()) {
        return Err(VersionError::Failed);
    }
    POLICY.classify(&String::from_utf8_lossy(&output))
}
