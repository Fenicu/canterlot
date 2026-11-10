//! Крупные каталоги сборки на диске машины. Обход ищет кэши (`target` с
//! `CACHEDIR.TAG`, `node_modules`, `.svelte-kit`, `web/build*`) под корнем рабочих
//! копий и корнями `[machine].hotspot_roots`, не переходит на другие файловые
//! системы, не идёт по ссылкам, в `.git`, скрытые каталоги и `/proc /sys /dev /run`
//! и не спускается внутрь найденного кэша. Размер меряется по `st_blocks` в фоне,
//! результат кэшируется: отчёт сторожа берёт готовые числа.

use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use canterlot_proto::{limits::MAX_WATCH_HOTSPOTS, worker::v1::DiskHotspot};
use uuid::Uuid;

/// Системные каталоги, куда обход не заходит, даже если корень их накрывает.
const SKIPPED: &[&str] = &["/proc", "/sys", "/dev", "/run"];
/// Глубже кэши сборки не прячутся: `~/проекты/репозиторий/web/node_modules` — 4.
const MAX_DEPTH: usize = 6;
/// Размер старше этого перемеряется на коротком проходе.
pub const REMEASURE_AFTER: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Target,
    NodeModules,
    SvelteKit,
    WebBuild,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Target => "target",
            Kind::NodeModules => "node_modules",
            Kind::SvelteKit => "svelte_kit",
            Kind::WebBuild => "web_build",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub kind: Kind,
    pub dev: u64,
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub found: Vec<Found>,
    /// Обход остановлен сроком: список может быть неполным.
    pub truncated: bool,
}

/// Подпись файла `CACHEDIR.TAG`: каталог объявлен кэшем.
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Каталог `target` — настоящий кэш сборки cargo: рядом лежит `CACHEDIR.TAG`,
/// обычный файл (не ссылка), начинающийся с подписи. Имя `target` без него ничего
/// не значит: в нём могут лежать исходники репозитория.
pub(crate) fn is_cargo_target(target: &Path) -> bool {
    let tag = target.join("CACHEDIR.TAG");
    let Ok(meta) = fs::symlink_metadata(&tag) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    fs::read(&tag).is_ok_and(|bytes| bytes.starts_with(CACHEDIR_SIGNATURE))
}

fn classify(path: &Path, name: &str) -> Option<Kind> {
    match name {
        "target" => is_cargo_target(path).then_some(Kind::Target),
        "node_modules" => Some(Kind::NodeModules),
        ".svelte-kit" => Some(Kind::SvelteKit),
        _ if name.starts_with("build")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|parent| parent == "web") =>
        {
            Some(Kind::WebBuild)
        }
        _ => None,
    }
}

/// Ищет кэши под корнями до срока. Повторяющиеся и вложенные корни не дают дублей.
pub fn discover(roots: &[PathBuf], deadline: Instant) -> Discovery {
    let mut discovery = Discovery::default();
    let mut seen = HashSet::new();
    for root in roots {
        if SKIPPED.iter().any(|skip| root == Path::new(skip)) || root == Path::new("/") {
            continue;
        }
        // Сам корень может быть ссылкой (домашний каталог): ему верим, глубже — нет.
        let Ok(meta) = fs::metadata(root) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        walk(root, meta.dev(), 0, deadline, &mut seen, &mut discovery);
        if discovery.truncated {
            break;
        }
    }
    discovery
}

fn walk(
    dir: &Path,
    dev: u64,
    depth: usize,
    deadline: Instant,
    seen: &mut HashSet<(u64, u64)>,
    out: &mut Discovery,
) {
    if Instant::now() >= deadline {
        out.truncated = true;
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.truncated {
            return;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        // `DirEntry::metadata` не идёт по ссылке: ссылка не каталог.
        if !meta.is_dir() || meta.dev() != dev {
            continue;
        }
        if !seen.insert((meta.dev(), meta.ino())) {
            continue;
        }
        let path = entry.path();
        if SKIPPED.iter().any(|skip| path == Path::new(skip)) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(kind) = classify(&path, name) {
            out.found.push(Found { path, kind, dev });
            continue;
        }
        if name.starts_with('.') || depth + 1 >= MAX_DEPTH {
            continue;
        }
        walk(&path, dev, depth + 1, deadline, seen, out);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub bytes: u64,
    pub incremental_bytes: u64,
    /// Счёт остановлен сроком: размер — нижняя граница.
    pub partial: bool,
}

/// Занятое место по `st_blocks`, без ссылок и других файловых систем; жёсткая ссылка
/// считается один раз (cargo связывает бинарники `deps/` и `debug/`). Для `target`
/// отдельно — каталоги `*/incremental` и `lint/*/incremental`.
pub fn measure(path: &Path, kind: Kind, dev: u64, deadline: Instant) -> Size {
    let mut size = Size::default();
    let mut inodes = HashSet::new();
    let Ok(meta) = fs::symlink_metadata(path) else {
        return size;
    };
    if !meta.is_dir() || meta.dev() != dev {
        return size;
    }
    size.bytes = meta.blocks().saturating_mul(512);
    let target = kind == Kind::Target;
    measure_dir(
        path,
        dev,
        0,
        target,
        false,
        deadline,
        &mut inodes,
        &mut size,
    );
    size
}

#[allow(clippy::too_many_arguments)]
fn measure_dir(
    dir: &Path,
    dev: u64,
    depth: usize,
    target: bool,
    incremental: bool,
    deadline: Instant,
    inodes: &mut HashSet<(u64, u64)>,
    size: &mut Size,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if Instant::now() >= deadline {
            size.partial = true;
            return;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.dev() != dev || meta.file_type().is_symlink() {
            continue;
        }
        if meta.nlink() > 1 && !meta.is_dir() && !inodes.insert((meta.dev(), meta.ino())) {
            continue;
        }
        let bytes = meta.blocks().saturating_mul(512);
        size.bytes = size.bytes.saturating_add(bytes);
        // `target/<профиль>/incremental` на глубине 1, `target/lint/<профиль>/incremental` — 2.
        let name = entry.file_name();
        let starts_incremental = target
            && name == "incremental"
            && (depth == 1
                || (depth == 2
                    && dir
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|p| p == "lint")));
        let inside = incremental || starts_incremental;
        if inside {
            size.incremental_bytes = size.incremental_bytes.saturating_add(bytes);
        }
        if meta.is_dir() {
            measure_dir(
                &entry.path(),
                dev,
                depth + 1,
                target,
                inside,
                deadline,
                inodes,
                size,
            );
            if size.partial {
                return;
            }
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::symlink_metadata(path)
        .ok()
        .filter(|meta| !meta.file_type().is_symlink())
        .and_then(|meta| meta.modified().ok())
}

fn direct_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// Последняя запись сборки в `target` без полного обхода: каталоги, куда rustc и
/// cargo кладут новые файлы при каждой сборке (`<профиль>/{deps,.fingerprint,build,
/// incremental}`, то же под `lint/<профиль>`) и `nextest`.
pub fn target_newest(target: &Path) -> Option<SystemTime> {
    let mut profiles = Vec::new();
    for dir in direct_dirs(target) {
        if dir.file_name().is_some_and(|name| name == "lint") {
            profiles.extend(direct_dirs(&dir));
        } else {
            profiles.push(dir);
        }
    }
    let mut candidates = vec![target.join("nextest")];
    for profile in profiles {
        for leaf in ["deps", ".fingerprint", "build", "incremental"] {
            candidates.push(profile.join(leaf));
        }
    }
    candidates.iter().filter_map(|path| mtime(path)).max()
}

/// Последняя запись в остальные кэши: сам каталог и его прямые потомки.
pub fn shallow_newest(path: &Path) -> Option<SystemTime> {
    let mut newest = mtime(path);
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Some(time) = mtime(&entry.path()) {
                newest = Some(newest.map_or(time, |current| current.max(time)));
            }
        }
    }
    newest
}

fn ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[derive(Clone, Debug)]
struct Cached {
    kind: Kind,
    size: Size,
    newest: Option<SystemTime>,
    measured_at: SystemTime,
}

/// Кэш размеров между проходами.
pub struct HotspotCache {
    roots: Vec<PathBuf>,
    worktree_root: PathBuf,
    entries: HashMap<PathBuf, Cached>,
}

impl HotspotCache {
    pub fn new(worktree_root: PathBuf, hotspot_roots: Vec<PathBuf>) -> Self {
        let mut roots = vec![worktree_root.clone()];
        for root in hotspot_roots {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        Self {
            roots,
            worktree_root,
            entries: HashMap::new(),
        }
    }

    /// Один проход. `full` перемеряет всё; короткий проход меряет новые кэши и
    /// размеры старше `REMEASURE_AFTER`, у остальных обновляет только время записи.
    pub fn refresh(&mut self, full: bool, discover_budget: Duration, measure_budget: Duration) {
        let now = SystemTime::now();
        let discovery = discover(&self.roots, Instant::now() + discover_budget);
        let measure_deadline = Instant::now() + measure_budget;
        let mut next = HashMap::new();
        for found in discovery.found {
            let newest = match found.kind {
                Kind::Target => target_newest(&found.path),
                _ => shallow_newest(&found.path),
            };
            let previous = self.entries.remove(&found.path);
            let stale = previous.as_ref().is_none_or(|cached| {
                cached.size.partial
                    || now
                        .duration_since(cached.measured_at)
                        .is_ok_and(|age| age >= REMEASURE_AFTER)
            });
            let cached = if full || stale {
                Cached {
                    kind: found.kind,
                    size: measure(&found.path, found.kind, found.dev, measure_deadline),
                    newest,
                    measured_at: SystemTime::now(),
                }
            } else {
                let mut cached = previous.expect("fresh cache entry");
                cached.newest = newest;
                cached
            };
            next.insert(found.path, cached);
        }
        // Неполный обход не забывает кэши, до которых не дошёл.
        if discovery.truncated {
            for (path, cached) in self.entries.drain() {
                if path.exists() {
                    next.entry(path).or_insert(cached);
                }
            }
        }
        self.entries = next;
    }

    /// Крупнейшие кэши, не больше `MAX_WATCH_HOTSPOTS`.
    pub fn hotspots(&self) -> Vec<DiskHotspot> {
        let mut rows: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, cached)| cached.size.bytes > 0)
            .map(|(path, cached)| DiskHotspot {
                path: path.to_string_lossy().into_owned(),
                kind: cached.kind.as_str().to_owned(),
                bytes: cached.size.bytes,
                incremental_bytes: cached.size.incremental_bytes,
                newest_ms: cached.newest.map_or(0, ms),
                measured_at_ms: ms(cached.measured_at),
                conversation_id: self.conversation(path).unwrap_or_default(),
            })
            .collect();
        rows.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        rows.truncate(MAX_WATCH_HOTSPOTS);
        rows
    }

    fn conversation(&self, path: &Path) -> Option<String> {
        let rest = path.strip_prefix(&self.worktree_root).ok()?;
        let first = rest.components().next()?.as_os_str().to_str()?;
        Uuid::parse_str(first).ok().map(|id| id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(target: &Path) {
        fs::create_dir_all(target).unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn paths(discovery: &Discovery) -> Vec<String> {
        let mut out: Vec<_> = discovery
            .found
            .iter()
            .map(|f| format!("{}:{}", f.kind.as_str(), f.path.display()))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn discover_finds_caches_and_skips_links_git_hidden_and_untagged_target() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        tag(&r.join("proj/target"));
        fs::create_dir_all(r.join("proj/target/debug/node_modules")).unwrap();
        fs::create_dir_all(r.join("proj/web/node_modules/inner/node_modules")).unwrap();
        fs::create_dir_all(r.join("proj/web/.svelte-kit")).unwrap();
        fs::create_dir_all(r.join("proj/web/build-ui")).unwrap();
        fs::create_dir_all(r.join("proj/other/build")).unwrap();
        fs::create_dir_all(r.join("plain/target")).unwrap();
        tag(&r.join("proj/.git/target"));
        tag(&r.join(".hidden/target"));
        let outside = tempfile::tempdir().unwrap();
        tag(&outside.path().join("target"));
        std::os::unix::fs::symlink(outside.path(), r.join("linked")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("target"), r.join("proj/sub-target"))
            .unwrap();

        let found = discover(&[r.to_owned(), r.join("proj")], far());
        assert!(!found.truncated);
        let p = |s: &str| r.join(s).display().to_string();
        assert_eq!(
            paths(&found),
            {
                let mut v = vec![
                    format!("node_modules:{}", p("proj/web/node_modules")),
                    format!("svelte_kit:{}", p("proj/web/.svelte-kit")),
                    format!("target:{}", p("proj/target")),
                    format!("web_build:{}", p("proj/web/build-ui")),
                ];
                v.sort();
                v
            },
            "no descent into caches, no links, .git, hidden dirs, untagged target, duplicates"
        );
    }

    #[test]
    fn discover_refuses_filesystem_root_and_system_dirs() {
        let found = discover(
            &[
                PathBuf::from("/"),
                PathBuf::from("/proc"),
                PathBuf::from("/sys"),
            ],
            far(),
        );
        assert!(found.found.is_empty());
        assert!(!found.truncated);
    }

    #[test]
    fn discover_stays_on_one_filesystem() {
        // /dev/shm — tmpfs, отдельная файловая система от временного каталога на диске.
        let root = tempfile::tempdir().unwrap();
        let shm = Path::new("/dev/shm");
        if !shm.is_dir()
            || fs::metadata(shm).unwrap().dev() == fs::metadata(root.path()).unwrap().dev()
        {
            return;
        }
        let foreign = tempfile::tempdir_in(shm).unwrap();
        fs::create_dir_all(foreign.path().join("node_modules")).unwrap();
        // Ссылка не в счёт; другая ФС достижима только как точка монтирования, а её
        // устройство отличается — проверяем фильтр устройства прямым вызовом walk.
        let mut out = Discovery::default();
        let dev = fs::metadata(root.path()).unwrap().dev();
        walk(
            foreign.path().parent().unwrap(),
            dev,
            0,
            far(),
            &mut HashSet::new(),
            &mut out,
        );
        assert!(
            out.found.is_empty(),
            "directories of another device are skipped"
        );
    }

    #[test]
    fn discover_stops_at_the_deadline() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("a/node_modules")).unwrap();
        let found = discover(&[root.path().to_owned()], Instant::now());
        assert!(found.truncated);
        assert!(found.found.is_empty());
    }

    #[test]
    fn measure_counts_blocks_once_per_inode_and_splits_incremental() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        tag(&target);
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        fs::create_dir_all(target.join("debug/incremental/crate-1")).unwrap();
        fs::create_dir_all(target.join("lint/debug/incremental")).unwrap();
        let mb = vec![7u8; 1 << 20];
        fs::write(target.join("debug/deps/bin-0123456789abcdef"), &mb).unwrap();
        fs::hard_link(
            target.join("debug/deps/bin-0123456789abcdef"),
            target.join("debug/bin"),
        )
        .unwrap();
        fs::write(target.join("debug/incremental/crate-1/q.bin"), &mb).unwrap();
        fs::write(target.join("lint/debug/incremental/w.bin"), &mb).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("big"), vec![1u8; 4 << 20]).unwrap();
        std::os::unix::fs::symlink(outside.path(), target.join("debug/link")).unwrap();

        let dev = fs::metadata(&target).unwrap().dev();
        let size = measure(&target, Kind::Target, dev, far());
        assert!(!size.partial);
        let mib = 1u64 << 20;
        assert!(size.bytes >= 3 * mib && size.bytes < 4 * mib, "{size:?}");
        assert!(
            size.incremental_bytes >= 2 * mib && size.incremental_bytes < 3 * mib,
            "{size:?}"
        );
        let cut = measure(&target, Kind::Target, dev, Instant::now());
        assert!(cut.partial);
    }

    #[test]
    fn target_newest_reads_build_dirs_not_the_whole_tree() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        tag(&target);
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        fs::create_dir_all(target.join("lint/debug/.fingerprint")).unwrap();
        fs::create_dir_all(target.join("debug/examples/deep")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(10 * 3600);
        let older = SystemTime::now() - Duration::from_secs(20 * 3600);
        for dir in ["debug/deps", "debug", "lint", "lint/debug", ""] {
            fs::File::open(target.join(dir))
                .unwrap()
                .set_modified(older)
                .unwrap();
        }
        fs::File::open(target.join("lint/debug/.fingerprint"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        // Свежая запись глубоко в дереве вне каталогов сборки не делает target живым.
        fs::write(target.join("debug/examples/deep/new"), "x").unwrap();
        let newest = target_newest(&target).unwrap();
        let age = SystemTime::now().duration_since(newest).unwrap();
        assert!(age > Duration::from_secs(9 * 3600) && age < Duration::from_secs(11 * 3600));
    }

    #[test]
    fn cache_measures_new_entries_reuses_fresh_sizes_and_names_worktrees() {
        let data = tempfile::tempdir().unwrap();
        let worktrees = data.path().join("worktrees");
        let id = Uuid::now_v7();
        let target = worktrees.join(id.to_string()).join("target");
        tag(&target);
        fs::write(target.join("blob"), vec![1u8; 1 << 20]).unwrap();
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("p/node_modules")).unwrap();
        fs::write(home.path().join("p/node_modules/x"), vec![1u8; 2 << 20]).unwrap();

        let mut cache = HotspotCache::new(worktrees.clone(), vec![home.path().to_owned()]);
        cache.refresh(false, Duration::from_secs(20), Duration::from_secs(60));
        let first = cache.hotspots();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].kind, "node_modules", "largest first");
        assert_eq!(first[1].conversation_id, id.to_string());
        assert_eq!(first[0].conversation_id, "");

        // Короткий проход не перемеряет свежий размер: рост виден на полном.
        fs::write(target.join("blob2"), vec![1u8; 4 << 20]).unwrap();
        cache.refresh(false, Duration::from_secs(20), Duration::from_secs(60));
        let short = cache.hotspots();
        let wt = short.iter().find(|h| h.kind == "target").unwrap();
        assert_eq!(wt.bytes, first[1].bytes);
        cache.refresh(true, Duration::from_secs(20), Duration::from_secs(60));
        let full = cache.hotspots();
        assert_eq!(full[0].kind, "target");
        assert!(full[0].bytes > wt.bytes);

        // Удалённый кэш пропадает из отчёта.
        fs::remove_dir_all(home.path().join("p/node_modules")).unwrap();
        cache.refresh(false, Duration::from_secs(20), Duration::from_secs(60));
        assert_eq!(cache.hotspots().len(), 1);
    }
}
