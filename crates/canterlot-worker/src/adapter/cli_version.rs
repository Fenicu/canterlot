//! Разбор вывода `--version` и политика версий, общие для адаптеров Codex и OpenCode.

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

pub type Triple = (u32, u32, u32);

/// Что считать версией CLI: подпись в выводе, минимум и проверенные выпуски.
pub struct VersionPolicy {
    pub prefix: &'static str,
    pub minimum: Triple,
    pub verified: &'static [Triple],
}

// Слово версии без подписи CLI, с суффиксами semver.
fn version_token<'a>(output: &'a str, prefix: &str) -> Result<&'a str, VersionError> {
    let text = output.strip_prefix(prefix).unwrap_or(output).trim();
    text.split_whitespace()
        .next()
        .ok_or(VersionError::Unparsable)
}

// Ядро `x.y.z` и признак предварительной сборки (`-…`); метаданные `+…` не влияют.
fn split_version(token: &str) -> Result<(Triple, bool), VersionError> {
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

impl VersionPolicy {
    pub fn parse(&self, output: &str) -> Result<Triple, VersionError> {
        split_version(version_token(output, self.prefix)?).map(|(core, _)| core)
    }

    /// Вывод `--version` в решение политики; метка для журнала без подписи CLI.
    pub fn classify(&self, output: &str) -> Result<CliVersion, VersionError> {
        let token = version_token(output, self.prefix)?;
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
        if v < self.minimum || (pre && v == self.minimum) {
            return Err(VersionError::BelowMinimum(label));
        }
        if !pre && self.verified.contains(&v) {
            Ok(CliVersion::Verified(label))
        } else {
            Ok(CliVersion::UntestedNewer(label))
        }
    }
}
