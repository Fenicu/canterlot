//! Собственный CODEX_HOME canterlot: символические ссылки на записи дома
//! владельца без файлов инструкций, SQLite, блокировок и каталогов демонов,
//! плюс общий AGENTS.md canterlot. Файлы владельца никогда не открываются:
//! читаются только имена записей, создаются только ссылки. Исключение: изолированный дом
//! делегированной сессии (D24) читает `config.toml` владельца и кладёт его копию без
//! `mcp_servers`.

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
    "mcp-oauth-locks",
    "hapi-runtime-owners",
    "tmp",
    ".tmp",
    ".sqlite-maintenance.lock",
];

/// Суффиксы записей дома владельца, которые никогда не ссылаются.
pub const EXCLUDED_SUFFIXES: &[&str] =
    &[".sqlite", ".sqlite-wal", ".sqlite-shm", ".sqlite-journal"];

/// Блокировки владельца; своя `LOCK_FILE` живёт только в доме canterlot.
const LOCK_SUFFIX: &str = ".lock";

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
const CONFIG_TOML: &str = "config.toml";
const LOCK_FILE: &str = ".canterlot-home.lock";
const LINK_TMP_PREFIX: &str = ".canterlot-link-";

/// Вид дома canterlot. Изолированные дома служат делегированным сессиям (D24): конфиг
/// владельца в них не ссылка, а копия без `mcp_servers`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HomeKind {
    #[default]
    Shared,
    /// Свой `AGENTS.md` canterlot, как у общего дома.
    Isolated,
    /// «Как у владельца»: `AGENTS.md` владельца по ссылке, своего нет.
    IsolatedOwner,
}

impl HomeKind {
    pub fn is_isolated(self) -> bool {
        self != Self::Shared
    }
}

/// Дом canterlot: путь и дом владельца, чьи записи ссылаются.
#[derive(Clone, Debug)]
pub struct CodexHome {
    pub path: PathBuf,
    pub owner_home: PathBuf,
    pub kind: HomeKind,
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

fn excluded(name: &std::ffi::OsStr, kind: HomeKind) -> bool {
    let bytes = name.as_bytes();
    if kind.is_isolated() && bytes == CONFIG_TOML.as_bytes() {
        return true;
    }
    let instructions = bytes == AGENTS_MD.as_bytes() || bytes == b"AGENTS.override.md";
    if kind == HomeKind::IsolatedOwner && instructions {
        return false;
    }
    if EXCLUDED_NAMES.iter().any(|n| bytes == n.as_bytes()) {
        return true;
    }
    bytes.ends_with(LOCK_SUFFIX.as_bytes())
        || EXCLUDED_SUFFIXES
            .iter()
            .any(|suffix| bytes.ends_with(suffix.as_bytes()))
}

/// Ссылка убирается, только если имя исключено, это остаток перевязки или цели
/// точно нет: другие ошибки metadata (EACCES) о цели ничего не говорят.
fn removable_link(name: &std::ffi::OsStr, link: &Path, kind: HomeKind) -> bool {
    use std::io::ErrorKind;
    excluded(name, kind)
        || name.as_bytes().starts_with(LINK_TMP_PREFIX.as_bytes())
        || std::fs::metadata(link).is_err_and(|error| {
            matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
        })
}

/// Путь под временным каталогом: системный temp, /tmp или /var/tmp.
fn is_temp_path(path: &Path) -> bool {
    let canonical = canonical_form(path);
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

/// Канонический вид пути: сам путь, если существует, иначе ближайший
/// существующий предок плюс остаток. Алиасы-симлинки раскрываются.
fn canonical_form(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut existing = path;
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) if parent != existing => {
                if let Some(name) = existing.file_name() {
                    rest.push(name.to_os_string());
                }
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
    let mut out = std::fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf());
    for component in rest.into_iter().rev() {
        out.push(component);
    }
    out
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
        // Дом canterlot не должен совпадать с домом владельца, лежать внутри него,
        // содержать его или указывать на него через симлинк: сравниваются
        // канонические пути. Относительные пути запрещены с обеих сторон.
        if path.is_relative() || owner_home.is_relative() {
            return Err(HomeError::Unusable);
        }
        let home_canon = canonical_form(&path);
        let owner_canon = canonical_form(&owner_home);
        if home_canon == owner_canon
            || home_canon.starts_with(&owner_canon)
            || owner_canon.starts_with(&home_canon)
        {
            return Err(HomeError::Unusable);
        }
        Ok(Self {
            path,
            owner_home,
            kind: HomeKind::Shared,
        })
    }

    /// Отдельный дом для делегированной сессии рядом с общим: общий остаётся ссылкой на конфиг
    /// владельца и сессиям владельца не мешает.
    pub fn isolated(self, owner_instructions: bool) -> Self {
        let (suffix, kind) = if owner_instructions {
            ("-isolated-owner", HomeKind::IsolatedOwner)
        } else {
            ("-isolated", HomeKind::Isolated)
        };
        let mut name = self
            .path
            .file_name()
            .map(OsString::from)
            .unwrap_or_default();
        name.push(suffix);
        Self {
            path: self.path.with_file_name(name),
            kind,
            ..self
        }
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
            if excluded(name, self.kind) {
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
                        // Запущенные app-server блокировки не берут: имя не пропадает.
                        let tmp = self
                            .path
                            .join(format!("{LINK_TMP_PREFIX}{}", std::process::id()));
                        std::os::unix::fs::symlink(&target, &tmp).map_err(HomeError::Io)?;
                        std::fs::rename(&tmp, &dest).map_err(HomeError::Io)?;
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
        // Ссылки с исчезнувшей целью и на исключённые имена убираются.
        let mut current: Vec<OsString> = Vec::new();
        for entry in std::fs::read_dir(&self.path).map_err(HomeError::Io)? {
            let entry = entry.map_err(HomeError::Io)?;
            let name = entry.file_name();
            if (name == AGENTS_MD && self.kind != HomeKind::IsolatedOwner) || name == LOCK_FILE {
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
            if removable_link(name, &dest, self.kind) {
                std::fs::remove_file(&dest).map_err(HomeError::Io)?;
                report.removed += 1;
            }
        }
        // У «Как у владельца» инструкции владельца по ссылке, своего AGENTS.md нет.
        if self.kind != HomeKind::IsolatedOwner {
            self.write_atomic(AGENTS_MD, agents_md)?;
        }
        if self.kind.is_isolated() {
            self.write_atomic(CONFIG_TOML, &self.config_without_mcp()?)?;
        }
        Ok(report)
    }

    /// Конфиг владельца без серверов MCP (D24). Читается только он; не разобрался или не
    /// читается - дом непригоден, молча копировать целиком нельзя.
    fn config_without_mcp(&self) -> Result<String, HomeError> {
        let text = match std::fs::read_to_string(self.owner_home.join(CONFIG_TOML)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(HomeError::Io(error)),
        };
        let mut config: toml::Table = text.parse().map_err(|_| HomeError::Unusable)?;
        config.remove("mcp_servers");
        toml::to_string(&config).map_err(|_| HomeError::Unusable)
    }

    /// Файл дома canterlot: атомарно и только при отличии.
    fn write_atomic(&self, name: &str, text: &str) -> Result<(), HomeError> {
        let path = self.path.join(name);
        let same = std::fs::read(&path)
            .map(|bytes| bytes == text.as_bytes())
            .unwrap_or(false);
        if !same {
            let tmp = self.path.join(format!("{name}.tmp-{}", std::process::id()));
            std::fs::write(&tmp, text.as_bytes()).map_err(HomeError::Io)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(HomeError::Io)?;
            std::fs::rename(&tmp, &path).map_err(HomeError::Io)?;
        }
        Ok(())
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
