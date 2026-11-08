use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::adapter::cli_version::{CliVersion, Triple, VersionError, VersionPolicy};
use crate::adapter::codex::process::GroupKill;
use crate::adapter::{reaped, spawn_with_retry_io};

const OUTPUT_CAP: u64 = 64 * 1024;

// Ветка 1.x другая программа с другим протоколом, поддержки нет.
pub const MIN_SUPPORTED_VERSION: Triple = (2, 1, 4);
pub const VERIFIED_VERSIONS: &[Triple] = &[(2, 1, 4), (2, 2, 5), (2, 2, 6)];

const POLICY: VersionPolicy = VersionPolicy {
    prefix: "opencode v",
    minimum: MIN_SUPPORTED_VERSION,
    verified: VERIFIED_VERSIONS,
};

pub fn parse_version(output: &str) -> Result<Triple, VersionError> {
    POLICY.parse(output)
}

pub async fn check(
    executable: &Path,
    env: &[(OsString, OsString)],
) -> Result<CliVersion, VersionError> {
    check_within(executable, env, Duration::from_secs(5)).await
}

/// `opencode --version` даже без запроса заводит каталоги XDG и журнал, поэтому
/// окружение то же, что у сервера: данные владельца проверка не трогает.
pub async fn check_within(
    executable: &Path,
    env: &[(OsString, OsString)],
    limit: Duration,
) -> Result<CliVersion, VersionError> {
    let mut child = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
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
