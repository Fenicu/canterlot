//! Собственный CODEX_HOME canterlot: символические ссылки на записи дома
//! владельца без файлов инструкций, SQLite, блокировок и каталогов демонов,
//! плюс общий AGENTS.md canterlot. Файлы владельца никогда не открываются:
//! читаются только имена записей, создаются только ссылки.

use crate::config::CodexConfig;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Имена записей дома владельца, которые никогда не ссылаются.
pub const EXCLUDED_NAMES: &[&str] = &[
    "AGENTS.md",
    "AGENTS.override.md",
    "app-server-control",
    "app-server-daemon",
    "thread-writer-locks",
    "hapi-runtime-owners",
    "tmp",
    ".tmp",
    ".sqlite-maintenance.lock",
];

/// Суффиксы записей дома владельца, которые никогда не ссылаются.
pub const EXCLUDED_SUFFIXES: &[&str] = &[".sqlite", ".sqlite-wal", ".sqlite-shm"];

/// Политика временных каталогов для дома canterlot.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TempDirPolicy {
    /// Отказ: путь под временным каталогом — запуск невозможен.
    #[default]
    Refuse,
    /// Разрешить (только тесты).
    Allow,
}

const AGENTS_MD: &str = "AGENTS.md";
const LOCK_FILE: &str = ".canterlot-home.lock";

/// Дом canterlot: путь и дом владельца, чьи записи ссылаются.
#[derive(Clone, Debug)]
pub struct CodexHome {
    pub path: PathBuf,
    pub owner_home: PathBuf,
}

/// Отчёт связывания: сколько создано, перевязано, убрано и конфликты.
#[derive(Clone, Debug, Default)]
pub struct LinkReport {
    pub linked: usize,
    pub relinked: usize,
    pub removed: usize,
    pub conflicts: Vec<String>,
}

/// Ошибка дома: непригодный путь или сбой ввода-вывода.
#[derive(Debug)]
pub enum HomeError {
    Unusable,
    Io(std::io::Error),
}

fn excluded(name: &std::ffi::OsStr) -> bool {
    let bytes = name.as_bytes();
    if EXCLUDED_NAMES.iter().any(|n| bytes == n.as_bytes()) {
        return true;
    }
    EXCLUDED_SUFFIXES
        .iter()
        .any(|suffix| bytes.ends_with(suffix.as_bytes()))
}

/// Путь под временным каталогом: системный temp, /tmp или /var/tmp.
fn is_temp_path(path: &Path) -> bool {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    for base in [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
    ] {
        let base = std::fs::canonicalize(&base).unwrap_or(base);
        if canonical.starts_with(&base) {
            return true;
        }
    }
    false
}

impl CodexHome {
    /// Дом владельца из конфигурации (явный override) или окружения
    /// ($CODEX_HOME или $HOME/.codex).
    pub fn owner_home(cfg: &CodexConfig, env: &[(OsString, OsString)]) -> Option<PathBuf> {
        if let Some(explicit) = &cfg.owner_home {
            return Some(explicit.clone());
        }
        let get = |key: &str| {
            env.iter()
                .rev()
                .find(|(name, _)| name.to_string_lossy() == key)
                .map(|(_, value)| PathBuf::from(value))
        };
        get("CODEX_HOME").or_else(|| {
            get("HOME").map(|home| {
                if home.as_os_str().is_empty() {
                    PathBuf::from(".codex")
                } else {
                    home.join(".codex")
                }
            })
        })
    }

    /// Дом владельца из окружения ($CODEX_HOME или $HOME/.codex);
    /// путь — home_dir из конфигурации или <data_dir>/codex-home.
    pub fn resolve(
        cfg: &CodexConfig,
        exec_dir: &Path,
        env: &[(OsString, OsString)],
    ) -> Result<Self, HomeError> {
        let Some(owner_home) = Self::owner_home(cfg, env) else {
            return Err(HomeError::Unusable);
        };
        let path = match &cfg.home_dir {
            Some(dir) => dir.clone(),
            None => {
                // exec_dir — это <data>/executions/<id>: дом рядом с executions.
                let mut ancestors = exec_dir.ancestors();
                let _self = ancestors.next();
                let parent = ancestors.next();
                let grandparent = ancestors.next();
                match (parent, grandparent) {
                    (Some(executions), Some(data))
                        if executions.file_name().is_some_and(|n| n == "executions") =>
                    {
                        data.join("codex-home")
                    }
                    _ => exec_dir.join("codex-home"),
                }
            }
        };
        if path == owner_home {
            return Err(HomeError::Unusable);
        }
        Ok(Self { path, owner_home })
    }

    /// Связать записи дома владельца под flock; общий AGENTS.md перезаписывается,
    /// только если отличается. Никогда не заменяет настоящий файл и не открывает
    /// файлы владельца.
    pub fn prepare(&self, agents_md: &str, policy: TempDirPolicy) -> Result<LinkReport, HomeError> {
        if policy == TempDirPolicy::Refuse && is_temp_path(&self.path) {
            return Err(HomeError::Unusable);
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.path)
            .map_err(HomeError::Io)?;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o700))
            .map_err(HomeError::Io)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.join(LOCK_FILE))
            .map_err(HomeError::Io)?;
        std::fs::set_permissions(
            self.path.join(LOCK_FILE),
            std::fs::Permissions::from_mode(0o600),
        )
        .map_err(HomeError::Io)?;
        // Исключительная блокировка на всё связывание.
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(|error| {
            HomeError::Io(std::io::Error::from_raw_os_error(error.raw_os_error()))
        })?;
        let _unlock = UnlockOnDrop(lock);
        let mut report = LinkReport::default();
        // Только имена записей; содержимое файлов владельца не читается.
        let mut entries: Vec<OsString> = Vec::new();
        match std::fs::read_dir(&self.owner_home) {
            Ok(listing) => {
                for entry in listing {
                    let entry = entry.map_err(HomeError::Io)?;
                    entries.push(entry.file_name());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(HomeError::Io(error)),
        }
        for name in &entries {
            if excluded(name) {
                continue;
            }
            let dest = self.path.join(name);
            let target = self.owner_home.join(name);
            match std::fs::symlink_metadata(&dest) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::os::unix::fs::symlink(&target, &dest).map_err(HomeError::Io)?;
                    report.linked += 1;
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    let same = std::fs::read_link(&dest)
                        .map(|current| current == target)
                        .unwrap_or(false);
                    if !same {
                        std::fs::remove_file(&dest).map_err(HomeError::Io)?;
                        std::os::unix::fs::symlink(&target, &dest).map_err(HomeError::Io)?;
                        report.relinked += 1;
                    }
                }
                // Настоящий файл или каталог Codex: не трогать, отметить конфликт.
                Ok(_) => {
                    report.conflicts.push(name.to_string_lossy().into_owned());
                }
                Err(error) => return Err(HomeError::Io(error)),
            }
        }
        // Ссылки с исчезнувшей целью убираются.
        let mut current: Vec<OsString> = Vec::new();
        for entry in std::fs::read_dir(&self.path).map_err(HomeError::Io)? {
            let entry = entry.map_err(HomeError::Io)?;
            let name = entry.file_name();
            if name == AGENTS_MD || name == LOCK_FILE {
                continue;
            }
            let file_type = entry.file_type().map_err(HomeError::Io)?;
            if !file_type.is_symlink() {
                continue;
            }
            current.push(name);
        }
        for name in &current {
            let dest = self.path.join(name);
            // metadata идёт по ссылке: у висячей цели — ошибка.
            let vanished = std::fs::metadata(&dest).is_err();
            if vanished {
                std::fs::remove_file(&dest).map_err(HomeError::Io)?;
                report.removed += 1;
            }
        }
        // Общий AGENTS.md canterlot: атомарно и только при отличии.
        let agents_path = self.path.join(AGENTS_MD);
        let same = std::fs::read(&agents_path)
            .map(|bytes| bytes == agents_md.as_bytes())
            .unwrap_or(false);
        if !same {
            let tmp = self
                .path
                .join(format!("{AGENTS_MD}.tmp-{}", std::process::id()));
            std::fs::write(&tmp, agents_md.as_bytes()).map_err(HomeError::Io)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(HomeError::Io)?;
            std::fs::rename(&tmp, &agents_path).map_err(HomeError::Io)?;
        }
        Ok(report)
    }
}

/// Разблокировка flock при выходе из области.
struct UnlockOnDrop(std::fs::File);

impl Drop for UnlockOnDrop {
    fn drop(&mut self) {
        use std::os::unix::io::AsFd;
        let _ = rustix::fs::flock(self.0.as_fd(), rustix::fs::FlockOperation::Unlock);
    }
}
