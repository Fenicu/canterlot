//! Подсказки рабочей папки: только каталоги, которые прошли бы проверку запуска.
//! Имена файлов и содержимое не читаются, пути в журналы не попадают.
use canterlot_proto::limits::{MAX_WORKDIR_BYTES, MAX_WORKDIR_SUGGESTIONS};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

const TIME_LIMIT: Duration = Duration::from_secs(2);
const MAX_SCANNED_ENTRIES: usize = 20_000;

type Lister = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Clone)]
pub struct WorkdirSuggester {
    lister: Lister,
    limit: Duration,
    listing: Arc<Semaphore>,
}

impl WorkdirSuggester {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self::with_lister(
            TIME_LIMIT,
            Arc::new(move |path| list_directories(path, &roots)),
        )
    }

    pub fn with_lister(limit: Duration, lister: Lister) -> Self {
        Self {
            lister,
            limit,
            listing: Arc::new(Semaphore::new(1)),
        }
    }

    /// Зависший диск держит один поток: пока он не вернулся, новых не будет.
    pub async fn suggest(&self, path: String) -> Vec<String> {
        let Ok(permit) = self.listing.clone().try_acquire_owned() else {
            return Vec::new();
        };
        let lister = self.lister.clone();
        let listing = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            lister(&path)
        });
        match tokio::time::timeout(self.limit, listing).await {
            Ok(Ok(found)) => found,
            _ => Vec::new(),
        }
    }
}

fn split(path: &str) -> Option<(&str, &str)> {
    if path.len() > MAX_WORKDIR_BYTES
        || !path.starts_with('/')
        || path.contains('\0')
        || path.split('/').any(|part| part == "..")
    {
        return None;
    }
    Some(path.split_at(path.rfind('/')? + 1))
}

fn inside(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub fn list_directories(path: &str, roots: &[PathBuf]) -> Vec<String> {
    let Some((parent, prefix)) = split(path) else {
        return Vec::new();
    };
    let roots: Vec<_> = roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect();
    let Ok(canonical) = fs::canonicalize(parent) else {
        return Vec::new();
    };
    // Предок корня читается, чтобы дойти до корня; посторонний каталог не читается вовсе.
    if !inside(&canonical, &roots) && !roots.iter().any(|root| root.starts_with(&canonical)) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&canonical) else {
        return Vec::new();
    };
    let hidden = prefix.starts_with('.');
    let mut names: Vec<String> = entries
        .take(MAX_SCANNED_ENTRIES)
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(prefix) && (hidden || !name.starts_with('.')))
        .collect();
    names.sort_unstable();
    names
        .into_iter()
        .filter(|name| {
            fs::canonicalize(canonical.join(name))
                .is_ok_and(|target| target.is_dir() && inside(&target, &roots))
        })
        .take(MAX_WORKDIR_SUGGESTIONS)
        .map(|name| format!("{parent}{name}"))
        .collect()
}
