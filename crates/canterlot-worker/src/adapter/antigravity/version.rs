//! Проверенные `agy` 1.4.12 и 1.5.0: разбор `--version` и вердикт `agentInfo.version`.
//! Вопросы отвечаются только на 1.5.0: на 1.4.12 cancel + prompt ломает сессию.

use super::process::base_env;
use crate::adapter::cli_version::VersionPolicy;
use crate::adapter::{reaped, spawn_with_retry_io};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

pub use crate::adapter::cli_version::{CliVersion, VersionError};

const OUTPUT_CAP: u64 = 64 * 1024;

pub const MIN_SUPPORTED_VERSION: (u32, u32, u32) = (1, 4, 12);
pub const VERIFIED_VERSIONS: &[(u32, u32, u32)] = &[(1, 4, 12), (1, 5, 0)];

/// Вопросы `ask_question` отвечаются обычным prompt только с 1.5.0.
pub const QUESTIONS_SINCE: (u32, u32, u32) = (1, 5, 0);

const POLICY: VersionPolicy = VersionPolicy {
    prefix: "agy",
    minimum: MIN_SUPPORTED_VERSION,
    verified: VERIFIED_VERSIONS,
};

// `agy 1.4.12`, `1.4.12` и предварительные сборки `1.5.0-rc.1`.
pub fn parse_version(output: &str) -> Result<(u32, u32, u32), VersionError> {
    POLICY.parse(output)
}

/// Вердикт строки `agentInfo.version` без нового запуска.
pub fn classify_agent(version: &str) -> Result<CliVersion, VersionError> {
    POLICY.classify(version)
}

pub fn is_verified(version: &str) -> bool {
    matches!(classify_agent(version), Ok(CliVersion::Verified(_)))
}

pub fn supports_questions(version: &CliVersion) -> bool {
    matches!(parse_version(version.label()), Ok(found) if found >= QUESTIONS_SINCE)
}

/// 1.5.0+ кладёт мышление внутрь `outputTokens`; 1.4.12 отдаёт его отдельно.
pub fn thoughts_in_output(version: &CliVersion) -> bool {
    !matches!(parse_version(version.label()), Ok(found) if found < QUESTIONS_SINCE)
}

pub async fn check(executable: &Path) -> Result<CliVersion, VersionError> {
    check_within(executable, Duration::from_secs(5)).await
}

pub async fn check_within(executable: &Path, limit: Duration) -> Result<CliVersion, VersionError> {
    let mut child = match spawn_with_retry_io(|| {
        let mut cmd = Command::new(executable);
        cmd.env_clear()
            .envs(base_env(std::env::vars_os()))
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
    let mut stdout = child.stdout.take().ok_or(VersionError::Failed)?;
    let work = async {
        let mut text = Vec::new();
        (&mut stdout)
            .take(OUTPUT_CAP)
            .read_to_end(&mut text)
            .await?;
        let status = match child.wait().await {
            Ok(status) => Some(status),
            Err(error) if reaped(&error) => None,
            Err(error) => return Err(error),
        };
        Ok::<_, std::io::Error>((status, text))
    };
    let (status, output) = match timeout(limit, work).await {
        Ok(Ok(out)) => out,
        // Будущее с `child` и `stdout` брошено: `kill_on_drop` снимает процесс.
        _ => return Err(VersionError::Failed),
    };
    if status.is_some_and(|status| !status.success()) {
        return Err(VersionError::Failed);
    }
    POLICY.classify(&String::from_utf8_lossy(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_cli_prefix_and_bare_numbers() {
        assert_eq!(parse_version("agy 1.4.12\n"), Ok((1, 4, 12)));
        assert_eq!(parse_version("1.4.12"), Ok((1, 4, 12)));
        assert_eq!(parse_version("agy"), Err(VersionError::Unparsable));
    }

    #[test]
    fn only_the_probed_releases_are_verified() {
        assert!(is_verified("1.4.12"));
        assert!(is_verified("1.5.0"));
        assert!(!is_verified("1.5.1"));
        assert!(!is_verified("1.4.11"));
    }

    #[test]
    fn questions_need_1_5_0() {
        assert!(!supports_questions(&CliVersion::Verified("1.4.12".into())));
        assert!(supports_questions(&CliVersion::Verified("1.5.0".into())));
        assert!(supports_questions(&CliVersion::UntestedNewer(
            "1.5.1".into()
        )));
    }
}
