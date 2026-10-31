//! Каталоги, окружение и процесс `opencode serve --stdio`.

use crate::config::OpenCodeConfig;
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

/// Ровно это наследуется от воркера: ключи провайдеров и служебные переменные отрезаны.
const INHERITED_ENV: [&str; 6] = ["PATH", "HOME", "USER", "LANG", "TERM", "XDG_CONFIG_HOME"];

/// Каталоги canterlot для OpenCode. База, журнал и файлы shell лежат здесь:
/// база владельца не годится, сервер на ней подхватит чужие исполнения.
#[derive(Clone, Debug)]
pub struct DataLayout {
    root: PathBuf,
}

#[derive(Debug)]
pub enum LayoutError {
    Refused,
    Io(std::io::Error),
}

impl DataLayout {
    /// Корень для запуска при старте воркера: `<data_dir>/opencode`.
    pub fn for_worker(cfg: &OpenCodeConfig, data_dir: &Path) -> Self {
        Self {
            root: cfg
                .data_root
                .clone()
                .unwrap_or_else(|| data_dir.join("opencode")),
        }
    }

    /// Корень для исполнения: каталог исполнения лежит в `<data_dir>/executions/<id>`.
    pub fn for_execution(cfg: &OpenCodeConfig, exec_dir: &Path) -> Self {
        let mut ancestors = exec_dir.ancestors().skip(1);
        let data_dir = match (ancestors.next(), ancestors.next()) {
            (Some(executions), Some(data))
                if executions
                    .file_name()
                    .is_some_and(|name| name == "executions") =>
            {
                data
            }
            _ => exec_dir,
        };
        Self::for_worker(cfg, data_dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn data_home(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn state_home(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn cache_home(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn create(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        for dir in [
            self.data_home(),
            self.state_home(),
            self.cache_home(),
            self.tmp(),
        ] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Ok(())
    }
}

fn env_value<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_os_str())
        .filter(|value| !value.is_empty())
}

// Симлинк не должен скрывать совпадение: путь сравнивается по самому длинному
// существующему предку.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    let mut tail = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut found) => {
                found.extend(tail.iter().rev());
                return Ok(found);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                    return Ok(path.to_owned());
                };
                tail.push(name);
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// База canterlot не может лежать в данных владельца, совпадать с ними или содержать их.
pub fn check_root(
    layout: &DataLayout,
    cfg: &OpenCodeConfig,
    parent: &[(OsString, OsString)],
) -> Result<(), LayoutError> {
    let mut refused: Vec<PathBuf> = cfg.refuse_paths.clone();
    if let Some(data) = env_value(parent, "XDG_DATA_HOME") {
        refused.push(Path::new(data).join("opencode"));
    }
    if let Some(home) = env_value(parent, "HOME") {
        refused.push(Path::new(home).join(".local/share/opencode"));
    }
    let root = resolved(&layout.root).map_err(LayoutError::Io)?;
    let database = resolved(&layout.data_home().join("opencode")).map_err(LayoutError::Io)?;
    for path in refused {
        let path = resolved(&path).map_err(LayoutError::Io)?;
        if root.starts_with(&path) || database.starts_with(&path) || path.starts_with(&database) {
            return Err(LayoutError::Refused);
        }
    }
    Ok(())
}

/// Общая часть окружения сервера и проверки версии: `--version` тоже заводит каталоги и журнал.
pub fn base_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
    layout: &DataLayout,
    tmp: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = parent
        .filter(|(key, _)| INHERITED_ENV.iter().any(|name| key == name))
        .collect();
    env.extend([
        ("XDG_DATA_HOME".into(), layout.data_home().into_os_string()),
        (
            "XDG_STATE_HOME".into(),
            layout.state_home().into_os_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            layout.cache_home().into_os_string(),
        ),
        ("TMPDIR".into(), tmp.as_os_str().to_owned()),
        ("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()),
    ]);
    env
}
