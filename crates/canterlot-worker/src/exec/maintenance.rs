//! Периодическая уборка состояния исполнений: сокеты, каталоги прерванного запуска
//! и забытые результаты сборки в рабочих копиях.

use std::{
    fs, io,
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use uuid::Uuid;

/// Каталог исполнения, оставшийся от прерванного запуска.
pub(crate) struct StaleExecution {
    pub id: Uuid,
    pub dir: PathBuf,
    pub socket: PathBuf,
}

/// Каталоги старше срока запуска: каталог мог остаться от прерванной подготовки.
pub(crate) fn stale_executions(
    data_dir: &Path,
    runtime_dir: Option<&Path>,
    spawn_timeout: Duration,
    now: SystemTime,
) -> io::Result<Vec<StaleExecution>> {
    let executions = data_dir.join("executions");
    let Ok(entries) = fs::read_dir(&executions) else {
        return Ok(Vec::new());
    };
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > spawn_timeout);
        if old {
            stale.push(StaleExecution {
                id,
                dir: entry.path(),
                socket: socket_path(runtime_dir, &executions, id),
            });
        }
    }
    Ok(stale)
}

/// Сокеты без каталога исполнения.
pub(crate) fn stale_sockets(
    data_dir: &Path,
    runtime_dir: Option<&Path>,
) -> io::Result<Vec<(Uuid, PathBuf)>> {
    let Some(runtime_dir) = runtime_dir else {
        return Ok(Vec::new());
    };
    let sockets = runtime_dir.join("canterlot");
    let Ok(entries) = fs::read_dir(&sockets) else {
        return Ok(Vec::new());
    };
    let executions = data_dir.join("executions");
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_socket() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(name) else {
            continue;
        };
        if executions.join(id.to_string()).exists() {
            continue;
        }
        stale.push((id, path));
    }
    Ok(stale)
}

/// Удаляет каталог, если его не занял запуск: возраст и сокеты сверяются снова.
pub(crate) fn remove_stale_execution(
    execution: &StaleExecution,
    spawn_timeout: Duration,
    now: SystemTime,
) -> io::Result<()> {
    let old = fs::symlink_metadata(&execution.dir)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > spawn_timeout);
    if !old || is_live_socket(&execution.socket) || is_live_socket(&execution.dir.join("ipc.sock"))
    {
        return Ok(());
    }
    match fs::remove_dir_all(&execution.dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Удаляет сокет, если каталог исполнения не появился и сокет не ожил.
pub(crate) fn remove_stale_socket(id: Uuid, path: &Path, executions: &Path) -> io::Result<()> {
    if executions.join(id.to_string()).exists() || is_live_socket(path) {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// В тестовой сборке уборка не имеет права трогать общий каталог сокетов.
pub(crate) fn ensure_test_runtime_allowed(runtime_dir: Option<&Path>) -> io::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        let Some(runtime_dir) = runtime_dir else {
            return Ok(());
        };
        let temp = std::path::absolute(std::env::temp_dir())?;
        let runtime = std::path::absolute(runtime_dir)?;
        if runtime.starts_with(&temp) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to sweep runtime outside {}: {}",
                temp.display(),
                runtime.display()
            ),
        ))
    }
    #[cfg(not(any(test, feature = "fault-injection")))]
    {
        let _ = runtime_dir;
        Ok(())
    }
}

fn socket_path(runtime_dir: Option<&Path>, executions: &Path, id: Uuid) -> PathBuf {
    runtime_dir.map_or_else(
        || executions.join(id.to_string()).join("ipc.sock"),
        |runtime| runtime.join("canterlot").join(format!("{id}.sock")),
    )
}

fn is_live_socket(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(_) => true,
        Err(error) => !matches!(
            error.kind(),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
        ),
    }
}

/// Рабочая копия со сборкой, которая не писала дольше наименьшего срока уборки.
pub(crate) struct BuildCandidate {
    pub conversation_id: String,
    pub path: PathBuf,
}

/// Насколько глубоко убирается сборка рабочей копии.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Grade {
    /// Живое, но простаивающее исполнение: `*/incremental`, `target/lint` и старые
    /// тестовые бинарники. Следующая сборка восстановит их сама.
    Idle,
    /// Исполнения нет: весь `target` и `node_modules/.cache`.
    Released,
}

/// Подкаталог корня рабочих копий для переименованного под удаление: та же файловая
/// система, имя не UUID, поэтому обход копий его не видит.
pub(crate) const TRASH_DIR: &str = ".gc";

pub(crate) fn trash_root(data_dir: &Path) -> PathBuf {
    data_dir.join("worktrees").join(TRASH_DIR)
}

/// Последняя запись сборки cargo; у каталога без `CACHEDIR.TAG` возраста нет.
pub(crate) fn build_newest(target: &Path) -> Option<SystemTime> {
    crate::machine::hotspots::is_cargo_target(target)
        .then(|| crate::machine::hotspots::target_newest(target))
        .flatten()
}

pub(crate) fn build_candidates(
    data_dir: &Path,
    now: SystemTime,
    min_idle: Duration,
) -> io::Result<Vec<BuildCandidate>> {
    let worktrees = data_dir.join("worktrees");
    let Ok(entries) = fs::read_dir(&worktrees) else {
        return Ok(Vec::new());
    };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if Uuid::parse_str(name).is_err() {
            continue;
        }
        let worktree = entry.path();
        let Some(target) = direct_dir(&worktree, "target")
            .filter(|target| crate::machine::hotspots::is_cargo_target(target))
        else {
            continue;
        };
        let Some(newest) = build_newest(&target) else {
            continue;
        };
        if !now.duration_since(newest).is_ok_and(|age| age > min_idle) {
            continue;
        }
        candidates.push(BuildCandidate {
            conversation_id: name.to_owned(),
            path: worktree,
        });
    }
    Ok(candidates)
}

/// Каталоги профилей: `target/<профиль>` и `target/lint/<профиль>`.
fn profiles(target: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(target) else {
        return out;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == "lint"
            && let Ok(inner) = fs::read_dir(&path)
        {
            out.extend(
                inner
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|e| e.path()),
            );
        }
        out.push(path);
    }
    out
}

/// Взятые на время уборки замки сборки cargo.
pub(crate) struct BuildLocks(#[allow(dead_code)] Vec<fs::File>);

/// Исключительный замок без ожидания на каждом замке cargo в `target`. Не взялся
/// хоть один — сборка идёт (или ждёт), копия занята: `None`. Пока замки у уборки,
/// новая сборка ждёт их и не увидит полуудалённый каталог.
pub(crate) fn lock_build(target: &Path) -> Option<BuildLocks> {
    use rustix::fs::{FlockOperation, flock};
    let mut held = Vec::new();
    for profile in profiles(target) {
        for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            let path = profile.join(name);
            let file = match fs::OpenOptions::new().read(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
                return None;
            }
            held.push(file);
        }
    }
    Some(BuildLocks(held))
}

/// Держит ли какой-нибудь процесс, кроме `skip` (сам воркер со взятыми замками),
/// исполняемый файл, рабочий каталог или открытый дескриптор под `target`: тестовый
/// бинарник из `deps/` или сборка с `--manifest-path` замок cargo могут уже отпустить.
pub(crate) fn target_in_use(target: &Path, proc_root: &Path, skip: u32) -> bool {
    let Ok(target) = fs::canonicalize(target) else {
        return false;
    };
    let under = |link: &Path| fs::read_link(link).is_ok_and(|path| path.starts_with(&target));
    let Ok(entries) = fs::read_dir(proc_root) else {
        // Не видно процессов — не видно и занятости: уборка не рискует.
        return true;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none_or(|pid| pid == skip)
        {
            continue;
        }
        let dir = entry.path();
        if under(&dir.join("cwd")) || under(&dir.join("exe")) {
            return true;
        }
        if let Ok(fds) = fs::read_dir(dir.join("fd")) {
            for fd in fds.flatten() {
                if under(&fd.path()) {
                    return true;
                }
            }
        }
    }
    false
}

/// Единицы сборки профиля по отпечаткам cargo: хэш из имени файла в `deps/` →
/// ключ единицы. Каталог `.fingerprint/<пакет>-<хэш>` хранит файл `<вид>-<цель>`
/// (`test-integration-test-lease`, `bin-canterlot-hub`, `test-lib-canterlot_hub`) и
/// его `.json` с хэшами цели, профиля, возможностей и флагов. Ключ — пакет, вид с целью
/// и эти поля без версии rustc и зависимостей: копии одного ключа отличаются только
/// устаревшими входами, разные ключи — разные текущие единицы с общим стеблем имени.
fn unit_keys(fingerprint: &Path) -> std::collections::HashMap<String, String> {
    let mut keys = std::collections::HashMap::new();
    let Ok(entries) = fs::read_dir(fingerprint) else {
        return keys;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((package, hash)) = name.to_str().and_then(|name| name.rsplit_once('-')) else {
            continue;
        };
        let Ok(files) = fs::read_dir(entry.path()) else {
            continue;
        };
        let units: Vec<String> = files
            .flatten()
            .filter_map(|file| {
                file.file_name()
                    .to_str()?
                    .strip_suffix(".json")
                    .map(str::to_owned)
            })
            .collect();
        // Одна единица на каталог; иначе соответствие бинарнику неоднозначно.
        let [unit] = units.as_slice() else {
            continue;
        };
        let Some(json) = fs::read(entry.path().join(format!("{unit}.json")))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            continue;
        };
        let fields = ["features", "target", "profile", "compile_kind", "rustflags"]
            .map(|field| json.get(field).map(ToString::to_string).unwrap_or_default());
        keys.insert(
            hash.to_owned(),
            format!("{package}\0{unit}\0{}", fields.join("\0")),
        );
    }
    keys
}

/// Старые копии тестовых бинарников в `deps/` (та же логика в `scripts/prune-target.sh`):
/// у каждой единицы сборки остаётся самая свежая копия, прочие уходят вместе с `.d` и
/// `.dwo`. Бинарник без отпечатка не трогается: его единица неизвестна.
fn stale_test_binaries(deps: &Path) -> Vec<PathBuf> {
    use std::{collections::HashMap, os::unix::fs::PermissionsExt};
    let Ok(entries) = fs::read_dir(deps) else {
        return Vec::new();
    };
    let keys = deps
        .parent()
        .map(|profile| unit_keys(&profile.join(".fingerprint")))
        .unwrap_or_default();
    let mut groups: HashMap<&str, Vec<(SystemTime, String)>> = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some((_, hash)) = name.rsplit_once('-') else {
            continue;
        };
        if hash.len() != 16
            || !hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            continue;
        }
        let Some(key) = keys.get(hash) else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 || meta.len() <= 1 << 20 {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        groups
            .entry(key.as_str())
            .or_default()
            .push((modified, name.to_owned()));
    }
    let mut stale = Vec::new();
    for mut names in groups.into_values() {
        names.sort();
        names.pop();
        for (_, name) in names {
            for suffix in ["", ".d", ".dwo"] {
                let path = deps.join(format!("{name}{suffix}"));
                if fs::symlink_metadata(&path).is_ok() {
                    stale.push(path);
                }
            }
        }
    }
    stale
}

/// Переименовывает убираемое в новый каталог корзины и возвращает его. Переименование
/// мгновенно, долгое удаление идёт потом, вне потока хранилища. Пустой результат —
/// убирать нечего.
pub(crate) fn stage_build(
    worktree: &Path,
    grade: Grade,
    trash_root: &Path,
) -> io::Result<Option<PathBuf>> {
    let Some(target) = direct_dir(worktree, "target") else {
        return Ok(None);
    };
    // Каталог мог измениться между перечислением и переименованием: без подписи
    // это не кэш cargo, а обычный каталог репозитория.
    if !crate::machine::hotspots::is_cargo_target(&target) {
        return Ok(None);
    }
    let mut doomed = Vec::new();
    match grade {
        Grade::Released => {
            doomed.push(target.clone());
            if let Some(modules) = direct_dir(worktree, "node_modules")
                && let Some(cache) = direct_dir(&modules, ".cache")
            {
                doomed.push(cache);
            }
        }
        Grade::Idle => {
            if let Some(lint) = direct_dir(&target, "lint") {
                doomed.push(lint);
            }
            for profile in profiles(&target) {
                if profile.starts_with(target.join("lint")) {
                    continue;
                }
                if let Some(incremental) = direct_dir(&profile, "incremental") {
                    doomed.push(incremental);
                }
                doomed.extend(stale_test_binaries(&profile.join("deps")));
            }
        }
    }
    if doomed.is_empty() {
        return Ok(None);
    }
    fs::create_dir_all(trash_root)?;
    let batch = trash_root.join(Uuid::now_v7().to_string());
    fs::create_dir(&batch)?;
    for (index, path) in doomed.iter().enumerate() {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match fs::rename(path, batch.join(format!("{index}-{name}"))) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(batch))
}

/// Удаляет содержимое корзины, включая остатки прерванных уборок.
pub(crate) fn purge_trash(trash_root: &Path) -> io::Result<()> {
    let Ok(entries) = fs::read_dir(trash_root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let result = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn direct_dir(parent: &Path, name: &str) -> Option<PathBuf> {
    let path = parent.join(name);
    let meta = fs::symlink_metadata(&path).ok()?;
    (meta.is_dir() && !meta.file_type().is_symlink()).then_some(path)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::net::UnixListener,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use canterlot_proto::worker::ipc::{Ownership, OwnershipPhase};
    use uuid::Uuid;

    use super::{
        Grade, TRASH_DIR, build_candidates, ensure_test_runtime_allowed, lock_build, purge_trash,
        remove_stale_execution, remove_stale_socket, stage_build, stale_executions, stale_sockets,
        target_in_use, trash_root,
    };

    fn old(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(8 * 3600);
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }

    /// Подпись настоящего кэша cargo: без неё `target` — обычный каталог.
    const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55";

    fn cargo_tag(target: &Path) {
        fs::write(target.join("CACHEDIR.TAG"), TAG).unwrap();
    }

    fn old_tree(path: &Path) {
        old(path);
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                old_tree(&entry.path());
            }
        }
    }

    fn socket(dir: &Path, id: Uuid) -> PathBuf {
        dir.join(format!("{id}.sock"))
    }

    fn record(id: Uuid, socket: &Path, exec_dir: &Path, phase: OwnershipPhase) -> Ownership {
        Ownership {
            execution_id: id.to_string(),
            generation: 1,
            exec_dir: exec_dir.to_string_lossy().into_owned(),
            ipc_socket: socket.to_string_lossy().into_owned(),
            phase: phase as i32,
            ..Default::default()
        }
    }

    #[test]
    fn gc_worktrees_removes_only_idle_build_output() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = root.path().join("data");
        let worktrees = data.join("worktrees");
        fs::create_dir_all(&worktrees).unwrap();

        let idle = Uuid::now_v7();
        let idle_tree = worktrees.join(idle.to_string());
        fs::create_dir_all(idle_tree.join("target/debug")).unwrap();
        cargo_tag(&idle_tree.join("target"));
        fs::create_dir_all(idle_tree.join("target/debug/deps")).unwrap();
        fs::create_dir_all(idle_tree.join("node_modules/.cache")).unwrap();
        fs::write(idle_tree.join("target/debug/old"), "old").unwrap();
        fs::write(idle_tree.join("target/debug/deps/dep"), "dep").unwrap();
        fs::write(idle_tree.join("node_modules/.cache/old"), "old").unwrap();
        fs::write(idle_tree.join("keep.txt"), "keep").unwrap();
        old_tree(&idle_tree.join("target"));
        old_tree(&idle_tree.join("node_modules/.cache"));
        old(&idle_tree);

        let live = Uuid::now_v7();
        let live_tree = worktrees.join(live.to_string());
        fs::create_dir_all(live_tree.join("target/debug/deps")).unwrap();
        cargo_tag(&live_tree.join("target"));
        fs::write(live_tree.join("target/debug/deps/old"), "old").unwrap();
        old_tree(&live_tree.join("target"));
        let live_conversation = live.to_string();

        let fresh = Uuid::now_v7();
        let fresh_tree = worktrees.join(fresh.to_string());
        fs::create_dir_all(fresh_tree.join("target/debug/deps")).unwrap();
        cargo_tag(&fresh_tree.join("target"));
        fs::write(fresh_tree.join("target/debug/deps/new"), "new").unwrap();

        let foreign = worktrees.join("not-a-conversation");
        fs::create_dir_all(foreign.join("target")).unwrap();
        fs::write(foreign.join("target/old"), "old").unwrap();
        old_tree(&foreign.join("target"));

        let outside = root.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "keep").unwrap();
        let linked = Uuid::now_v7();
        let linked_tree = worktrees.join(linked.to_string());
        fs::create_dir(&linked_tree).unwrap();
        std::os::unix::fs::symlink(&outside, linked_tree.join("target")).unwrap();

        let cache_link = Uuid::now_v7();
        let cache_tree = worktrees.join(cache_link.to_string());
        fs::create_dir_all(cache_tree.join("target/debug/deps")).unwrap();
        cargo_tag(&cache_tree.join("target"));
        fs::write(cache_tree.join("target/debug/deps/dep"), "dep").unwrap();
        old_tree(&cache_tree.join("target"));
        let cache_outside = root.path().join("cache-outside");
        fs::create_dir_all(&cache_outside).unwrap();
        std::os::unix::fs::symlink(&cache_outside, cache_tree.join("node_modules")).unwrap();

        let trash = trash_root(&data);
        let candidates =
            build_candidates(&data, SystemTime::now(), Duration::from_secs(6 * 3600)).unwrap();
        for candidate in candidates {
            if candidate.conversation_id == live_conversation {
                continue;
            }
            stage_build(&candidate.path, Grade::Released, &trash).unwrap();
        }
        assert!(
            build_candidates(&data, SystemTime::now(), Duration::from_secs(1))
                .unwrap()
                .iter()
                .all(|c| c.conversation_id != TRASH_DIR)
        );
        purge_trash(&trash).unwrap();

        assert!(!idle_tree.join("target").exists());
        assert!(!idle_tree.join("node_modules/.cache").exists());
        assert!(idle_tree.join("keep.txt").exists());
        assert!(live_tree.join("target/debug/deps/old").exists());
        assert!(fresh_tree.join("target/debug/deps/new").exists());
        assert!(foreign.join("target/old").exists());
        assert!(outside.join("keep").exists());
        assert!(linked_tree.join("target").is_symlink());
        assert!(!cache_tree.join("target").exists());
        assert!(cache_tree.join("node_modules").is_symlink());
        assert!(cache_outside.exists());
        assert_eq!(fs::read_dir(&trash).unwrap().count(), 0);
    }

    #[test]
    fn untagged_target_is_not_a_released_candidate() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = root.path().join("data");
        let worktree = data.join("worktrees").join(Uuid::now_v7().to_string());
        fs::create_dir_all(worktree.join("target/src")).unwrap();
        fs::write(worktree.join("target/src/main.rs"), "fn main() {}").unwrap();
        fs::write(worktree.join("target/Makefile"), "all:").unwrap();
        old_tree(&worktree.join("target"));
        old(&worktree);
        let trash = trash_root(&data);

        let candidates =
            build_candidates(&data, SystemTime::now(), Duration::from_secs(6 * 3600)).unwrap();
        assert!(
            candidates.is_empty(),
            "an untagged target is not a cargo build directory"
        );
        assert!(
            stage_build(&worktree, Grade::Released, &trash)
                .unwrap()
                .is_none()
        );
        assert!(worktree.join("target/src/main.rs").exists());
        assert!(worktree.join("target/Makefile").exists());
        assert!(!trash.exists());
    }

    #[test]
    fn untagged_target_keeps_idle_output() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let worktree = root.path().join("wt");
        let target = worktree.join("target");
        fs::create_dir_all(target.join("debug/incremental/crate-abc")).unwrap();
        fs::create_dir_all(target.join("lint/debug")).unwrap();
        fs::write(target.join("debug/incremental/crate-abc/q"), "q").unwrap();
        fs::write(target.join("lint/debug/out"), "out").unwrap();
        old_tree(&target);
        let trash = root.path().join("trash");

        assert!(
            stage_build(&worktree, Grade::Idle, &trash)
                .unwrap()
                .is_none()
        );
        assert!(target.join("debug/incremental/crate-abc/q").exists());
        assert!(target.join("lint/debug/out").exists());
        assert!(!trash.exists());
    }

    #[test]
    fn only_a_regular_cachedir_tag_with_the_signature_makes_target_a_candidate() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = root.path().join("data");
        let worktrees = data.join("worktrees");
        let trash = trash_root(&data);

        let wrong = worktrees.join(Uuid::now_v7().to_string());
        fs::create_dir_all(wrong.join("target/debug")).unwrap();
        fs::write(wrong.join("target/CACHEDIR.TAG"), "not a signature").unwrap();
        fs::write(wrong.join("target/debug/old"), "old").unwrap();
        old_tree(&wrong.join("target"));

        let valid = root.path().join("valid.tag");
        fs::write(&valid, TAG).unwrap();
        let linked = worktrees.join(Uuid::now_v7().to_string());
        fs::create_dir_all(linked.join("target/debug")).unwrap();
        std::os::unix::fs::symlink(&valid, linked.join("target/CACHEDIR.TAG")).unwrap();
        fs::write(linked.join("target/debug/old"), "old").unwrap();
        old_tree(&linked.join("target"));

        let candidates =
            build_candidates(&data, SystemTime::now(), Duration::from_secs(6 * 3600)).unwrap();
        assert!(candidates.is_empty());
        assert!(
            stage_build(&wrong, Grade::Released, &trash)
                .unwrap()
                .is_none()
        );
        assert!(
            stage_build(&linked, Grade::Released, &trash)
                .unwrap()
                .is_none()
        );
        assert!(wrong.join("target/debug/old").exists());
        assert!(linked.join("target/debug/old").exists());
        assert!(!trash.exists());
    }

    #[test]
    fn stage_build_rechecks_the_tag_before_renaming() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = root.path().join("data");
        let worktree = data.join("worktrees").join(Uuid::now_v7().to_string());
        let target = worktree.join("target");
        fs::create_dir_all(target.join("debug/deps")).unwrap();
        cargo_tag(&target);
        fs::write(target.join("debug/deps/old"), "old").unwrap();
        old_tree(&target);
        old(&worktree);
        let trash = trash_root(&data);

        let candidates =
            build_candidates(&data, SystemTime::now(), Duration::from_secs(6 * 3600)).unwrap();
        assert!(candidates.iter().any(|c| c.path == worktree));
        fs::remove_file(target.join("CACHEDIR.TAG")).unwrap();

        assert!(
            stage_build(&worktree, Grade::Released, &trash)
                .unwrap()
                .is_none()
        );
        assert!(target.join("debug/deps/old").exists());
        assert!(!trash.exists());
    }

    fn binary(path: &Path, age_hours: u64) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, vec![0u8; (1 << 20) + 1]).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        let at = SystemTime::now() - Duration::from_secs(age_hours * 3600);
        fs::File::open(path).unwrap().set_modified(at).unwrap();
    }

    #[test]
    fn idle_grade_keeps_target_but_drops_incremental_lint_and_stale_test_binaries() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let worktree = root.path().join("wt");
        let target = worktree.join("target");
        let deps = target.join("debug/deps");
        fs::create_dir_all(&deps).unwrap();
        cargo_tag(&target);
        fs::create_dir_all(target.join("debug/incremental/crate-abc")).unwrap();
        fs::create_dir_all(target.join("lint/debug/incremental")).unwrap();
        fs::write(target.join("debug/incremental/crate-abc/q"), "q").unwrap();
        binary(&deps.join("session-0123456789abcdef"), 5);
        fs::write(deps.join("session-0123456789abcdef.d"), "d").unwrap();
        binary(&deps.join("session-fedcba9876543210"), 1);
        let profile = target.join("debug");
        unit(
            &profile,
            "canterlot-worker",
            "0123456789abcdef",
            "test-integration-test-session",
            "[]",
        );
        unit(
            &profile,
            "canterlot-worker",
            "fedcba9876543210",
            "test-integration-test-session",
            "[]",
        );
        fs::write(deps.join("libdep-0123456789abcdef.rlib"), "lib").unwrap();
        binary(&target.join("debug/canterlot-worker"), 5);

        let trash = root.path().join("trash");
        let batch = stage_build(&worktree, Grade::Idle, &trash)
            .unwrap()
            .unwrap();
        assert!(!target.join("debug/incremental").exists());
        assert!(!target.join("lint").exists());
        assert!(!deps.join("session-0123456789abcdef").exists());
        assert!(!deps.join("session-0123456789abcdef.d").exists());
        assert!(
            deps.join("session-fedcba9876543210").exists(),
            "newest copy stays"
        );
        assert!(deps.join("libdep-0123456789abcdef.rlib").exists());
        assert!(target.join("debug/canterlot-worker").exists());
        assert_eq!(fs::read_dir(&batch).unwrap().count(), 4);
        purge_trash(&trash).unwrap();
        assert!(!batch.exists());
        // Повтор: убирать уже нечего.
        assert!(
            stage_build(&worktree, Grade::Idle, &trash)
                .unwrap()
                .is_none()
        );
    }

    /// Каталог отпечатка cargo для единицы сборки с хэшем `hash`.
    fn unit(profile: &Path, package: &str, hash: &str, name: &str, features: &str) {
        let dir = profile.join(format!(".fingerprint/{package}-{hash}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), "0011223344556677").unwrap();
        fs::write(dir.join(format!("dep-{name}")), "").unwrap();
        fs::write(dir.join("invoked.timestamp"), "").unwrap();
        let json = serde_json::json!({
            "rustc": 1, "features": features, "declared_features": "[]",
            "target": name.len(), "profile": 7, "path": 3, "deps": [], "local": [],
            "rustflags": [], "config": 9, "compile_kind": 0,
        });
        fs::write(dir.join(format!("{name}.json")), json.to_string()).unwrap();
    }

    #[test]
    fn stale_test_binaries_keep_every_current_unit_that_shares_a_stem() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let profile = root.path().join("debug");
        let deps = profile.join("deps");
        fs::create_dir_all(&deps).unwrap();
        // Интеграционный тест `version` в двух пакетах: оба текущие.
        binary(&deps.join("version-1111111111111111"), 5);
        unit(
            &profile,
            "canterlot-hub",
            "1111111111111111",
            "test-integration-test-version",
            "[]",
        );
        binary(&deps.join("version-2222222222222222"), 1);
        unit(
            &profile,
            "canterlot-worker",
            "2222222222222222",
            "test-integration-test-version",
            "[]",
        );
        // Один пакет: сам бинарник, тесты библиотеки и тесты бинарника под одним стеблем.
        binary(&deps.join("canterlot_hub-3333333333333333"), 4);
        unit(
            &profile,
            "canterlot-hub",
            "3333333333333333",
            "bin-canterlot-hub",
            "[]",
        );
        binary(&deps.join("canterlot_hub-4444444444444444"), 3);
        unit(
            &profile,
            "canterlot-hub",
            "4444444444444444",
            "test-lib-canterlot_hub",
            "[]",
        );
        binary(&deps.join("canterlot_hub-5555555555555555"), 2);
        unit(
            &profile,
            "canterlot-hub",
            "5555555555555555",
            "test-bin-canterlot-hub",
            "[]",
        );
        // Тот же тест с другим набором возможностей — тоже текущий.
        binary(&deps.join("exec-6666666666666666"), 6);
        unit(
            &profile,
            "canterlot-worker",
            "6666666666666666",
            "test-integration-test-exec",
            "[]",
        );
        binary(&deps.join("exec-7777777777777777"), 1);
        unit(
            &profile,
            "canterlot-worker",
            "7777777777777777",
            "test-integration-test-exec",
            "[\"fault-injection\"]",
        );
        // Старая копия той же единицы уходит, свежая остаётся.
        binary(&deps.join("lease-8888888888888888"), 9);
        unit(
            &profile,
            "canterlot-worker",
            "8888888888888888",
            "test-integration-test-lease",
            "[]",
        );
        binary(&deps.join("lease-9999999999999999"), 1);
        unit(
            &profile,
            "canterlot-worker",
            "9999999999999999",
            "test-integration-test-lease",
            "[]",
        );
        // Без отпечатка единица неизвестна: не трогается.
        binary(&deps.join("lease-aaaaaaaaaaaaaaaa"), 20);

        let stale = super::stale_test_binaries(&deps);
        assert_eq!(stale, vec![deps.join("lease-8888888888888888")]);
    }

    #[test]
    fn a_held_cargo_lock_or_a_process_inside_target_makes_the_build_busy() {
        use rustix::fs::{FlockOperation, flock};
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let target = root.path().join("wt/target");
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::create_dir_all(target.join("lint/debug")).unwrap();
        fs::write(target.join("debug/.cargo-lock"), "").unwrap();
        fs::write(target.join("lint/debug/.cargo-build-lock"), "").unwrap();

        let locks = lock_build(&target).expect("nobody builds");
        drop(locks);

        // Сборка началась во время уборки: её замок держит cargo.
        let cargo = fs::File::open(target.join("lint/debug/.cargo-build-lock")).unwrap();
        flock(&cargo, FlockOperation::LockShared).unwrap();
        assert!(lock_build(&target).is_none());
        drop(cargo);
        assert!(lock_build(&target).is_some());

        let own = std::process::id();
        let mine = fs::File::open(target.join("debug/.cargo-lock")).unwrap();
        assert!(
            !target_in_use(&target, Path::new("/proc"), own),
            "own fds are skipped"
        );
        assert!(target_in_use(&target, Path::new("/proc"), 0));
        drop(mine);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(target.join("debug"))
            .spawn()
            .unwrap();
        assert!(
            target_in_use(&target, Path::new("/proc"), own),
            "cwd counts"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(fs::File::open(target.join("debug/.cargo-lock")).unwrap())
            .spawn()
            .unwrap();
        assert!(
            target_in_use(&target, Path::new("/proc"), own),
            "an open fd counts"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!target_in_use(&target, Path::new("/proc"), own));
    }

    #[test]
    fn sweep_refuses_runtime_outside_the_temp_dir() {
        let error = ensure_test_runtime_allowed(Some(Path::new("/canterlot-worker-test-runtime")))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn sweep_removes_only_stale_runtime_state() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = root.path().join("data");
        let executions = data.join("executions");
        let runtime = root.path().join("runtime");
        let sockets = runtime.join("canterlot");
        fs::create_dir_all(&executions).unwrap();
        fs::create_dir_all(&sockets).unwrap();

        let stale_socket = Uuid::now_v7();
        drop(UnixListener::bind(socket(&sockets, stale_socket)).unwrap());

        let released_socket = Uuid::now_v7();
        drop(UnixListener::bind(socket(&sockets, released_socket)).unwrap());
        let released_exec = executions.join(released_socket.to_string());
        let released = record(
            released_socket,
            &socket(&sockets, released_socket),
            &released_exec,
            OwnershipPhase::Released,
        );

        let unreleased_socket = Uuid::now_v7();
        drop(UnixListener::bind(socket(&sockets, unreleased_socket)).unwrap());
        let unreleased_exec = executions.join(unreleased_socket.to_string());
        let unreleased = record(
            unreleased_socket,
            &socket(&sockets, unreleased_socket),
            &unreleased_exec,
            OwnershipPhase::Attached,
        );

        let existing_socket = Uuid::now_v7();
        drop(UnixListener::bind(socket(&sockets, existing_socket)).unwrap());
        let existing_exec = executions.join(existing_socket.to_string());
        fs::create_dir(&existing_exec).unwrap();

        let live_socket = Uuid::now_v7();
        let live_listener = UnixListener::bind(socket(&sockets, live_socket)).unwrap();

        let old_orphan = Uuid::now_v7();
        let old_orphan_dir = executions.join(old_orphan.to_string());
        fs::create_dir(&old_orphan_dir).unwrap();
        old(&old_orphan_dir);

        let fresh_orphan = Uuid::now_v7();
        let fresh_orphan_dir = executions.join(fresh_orphan.to_string());
        fs::create_dir(&fresh_orphan_dir).unwrap();

        let old_released = Uuid::now_v7();
        let old_released_dir = executions.join(old_released.to_string());
        fs::create_dir(&old_released_dir).unwrap();
        old(&old_released_dir);
        let released_dir_record = record(
            old_released,
            &socket(&sockets, old_released),
            &old_released_dir,
            OwnershipPhase::Released,
        );

        let old_live = Uuid::now_v7();
        let old_live_dir = executions.join(old_live.to_string());
        fs::create_dir(&old_live_dir).unwrap();
        old(&old_live_dir);
        let old_live_listener = UnixListener::bind(socket(&sockets, old_live)).unwrap();

        let old_inner_live = Uuid::now_v7();
        let old_inner_live_dir = executions.join(old_inner_live.to_string());
        fs::create_dir(&old_inner_live_dir).unwrap();
        let old_inner_listener = UnixListener::bind(old_inner_live_dir.join("ipc.sock")).unwrap();
        old(&old_inner_live_dir);

        let now = SystemTime::now();
        let timeout = Duration::from_secs(10);
        let records = [released, unreleased, released_dir_record];
        let executions_stale = stale_executions(&data, Some(&runtime), timeout, now).unwrap();
        let sockets_stale = stale_sockets(&data, Some(&runtime)).unwrap();
        for execution in executions_stale {
            if records
                .iter()
                .any(|record| record.execution_id == execution.id.to_string())
            {
                continue;
            }
            remove_stale_execution(&execution, timeout, now).unwrap();
        }
        for (id, path) in sockets_stale {
            let unreleased = records.iter().any(|record| {
                record.phase != OwnershipPhase::Released as i32
                    && Path::new(&record.ipc_socket) == path.as_path()
            });
            if unreleased {
                continue;
            }
            remove_stale_socket(id, &path, &executions).unwrap();
        }

        assert!(!socket(&sockets, stale_socket).exists());
        assert!(!socket(&sockets, released_socket).exists());
        assert!(socket(&sockets, unreleased_socket).exists());
        assert!(socket(&sockets, existing_socket).exists());
        assert!(socket(&sockets, live_socket).exists());
        assert!(!old_orphan_dir.exists());
        assert!(fresh_orphan_dir.exists());
        assert!(old_released_dir.exists());
        assert!(old_live_dir.exists());
        assert!(old_inner_live_dir.exists());

        drop(live_listener);
        drop(old_live_listener);
        drop(old_inner_listener);
    }
}
