//! Разбор вывода `--version`, политика версий и проба бинарника, общие для адаптеров.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Первый срок пробы воркера; повтор идёт с удвоенным сроком.
pub const DEFAULT_PROBE_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CliVersion {
    Verified(String),
    UntestedNewer(String),
}

impl CliVersion {
    /// Версия без подписи CLI: годится для имени закреплённой копии и журнала.
    pub fn label(&self) -> &str {
        match self {
            Self::Verified(label) | Self::UntestedNewer(label) => label,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    NotFound,
    Failed,
    /// Проба не уложилась в срок: это не признак сломанного CLI.
    Timeout,
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

/// Успешная проба помнится по самому бинарнику: путь, размер, mtime и inode.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ino: u64,
    dev: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    identity: Identity,
    version: CliVersion,
}

static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();

// `Command` сам ищет голое имя в PATH; для кэша нужен тот же найденный файл.
fn resolve(executable: &Path) -> Option<PathBuf> {
    let found = if executable.is_absolute() || executable.components().count() > 1 {
        executable.to_path_buf()
    } else {
        let path_env = std::env::var_os("PATH")?;
        std::env::split_paths(&path_env)
            .map(|dir| dir.join(executable))
            .find(|candidate| candidate.is_file())?
    };
    std::fs::metadata(&found).ok()?;
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

fn identity(executable: &Path) -> Option<(PathBuf, Identity)> {
    use std::os::unix::fs::MetadataExt;
    let path = resolve(executable)?;
    let meta = std::fs::metadata(&path).ok()?;
    Some((
        path,
        Identity {
            size: meta.len(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ino: meta.ino(),
            dev: meta.dev(),
        },
    ))
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached(executable: &Path) -> Option<CliVersion> {
    let (path, current) = identity(executable)?;
    let cache = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&path)
        .filter(|entry| entry.identity == current)
        .map(|entry| entry.version.clone())
}

fn remember(executable: &Path, version: CliVersion) {
    let Some((path, identity)) = identity(executable) else {
        return;
    };
    cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path, Cached { identity, version });
}

type DiskCache = HashMap<String, Cached>;

fn disk_read(path: &Path) -> DiskCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn disk_cached(cache: &Path, key: &Path, current: &Identity) -> Option<CliVersion> {
    let entries = disk_read(cache);
    let key = key.to_string_lossy();
    entries
        .get(key.as_ref())
        .filter(|entry| &entry.identity == current)
        .map(|entry| entry.version.clone())
}

fn disk_remember(cache: &Path, key: &Path, identity: &Identity, version: &CliVersion) {
    let mut entries = disk_read(cache);
    entries.insert(
        key.to_string_lossy().into_owned(),
        Cached {
            identity: identity.clone(),
            version: version.clone(),
        },
    );
    let Ok(bytes) = serde_json::to_vec(&entries) else {
        return;
    };
    let temporary = cache.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    if std::fs::write(&temporary, bytes).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::rename(&temporary, cache);
    }
}

/// Проба с кэшем и повтором только по таймауту: три попытки, повторы вдвое
/// дольше первой. Худшая добавка к старту — `4 × first` (20 с при умолчании).
/// `cache` — общий на процессы файл; без него кэш живёт только в памяти.
pub async fn cached_check<F, Fut>(
    executable: &Path,
    first: Duration,
    cache: Option<&Path>,
    probe: F,
) -> Result<CliVersion, VersionError>
where
    F: Fn(Duration) -> Fut,
    Fut: Future<Output = Result<CliVersion, VersionError>>,
{
    if let Some(version) = cached(executable) {
        return Ok(version);
    }
    let current = identity(executable);
    if let (Some(cache), Some((path, identity))) = (cache, &current)
        && let Some(version) = disk_cached(cache, path, identity)
    {
        remember(executable, version.clone());
        return Ok(version);
    }
    let retry = first.saturating_mul(2);
    for limit in [first, retry, retry] {
        match probe(limit).await {
            Ok(version) => {
                remember(executable, version.clone());
                if let (Some(cache), Some((path, identity))) = (cache, current) {
                    disk_remember(cache, &path, &identity, &version);
                }
                return Ok(version);
            }
            // Сломанный, отсутствующий или неразборчивый CLI повтор не лечит.
            Err(VersionError::Timeout) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(VersionError::Timeout)
}

#[cfg(test)]
mod disk_cache_tests {
    use super::*;

    #[test]
    fn disk_cache_matches_only_the_same_binary_identity() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".cli-versions.json");
        let key = Path::new("/usr/local/bin/claude");
        let identity = Identity {
            size: 10,
            mtime: 20,
            mtime_nsec: 30,
            ino: 40,
            dev: 50,
        };
        assert_eq!(disk_cached(&cache, key, &identity), None);
        disk_remember(
            &cache,
            key,
            &identity,
            &CliVersion::Verified("2.1.311".into()),
        );
        assert_eq!(
            disk_cached(&cache, key, &identity),
            Some(CliVersion::Verified("2.1.311".into()))
        );
        let changed = Identity {
            mtime: 21,
            ..identity
        };
        assert_eq!(disk_cached(&cache, key, &changed), None);
    }

    #[tokio::test]
    async fn disk_cache_avoids_the_probe_when_memory_is_cold() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("claude");
        std::fs::write(&exe, b"binary").unwrap();
        let (key, identity) = identity(&exe).unwrap();
        let cache = dir.path().join(".cli-versions.json");
        disk_remember(
            &cache,
            &key,
            &identity,
            &CliVersion::Verified("2.1.311".into()),
        );
        let result = cached_check(&exe, Duration::from_millis(50), Some(&cache), |_| async {
            Err(VersionError::Failed)
        })
        .await;
        assert_eq!(result, Ok(CliVersion::Verified("2.1.311".into())));
    }
}
