//! Периодическая уборка состояния исполнений: сокеты, каталоги прерванного запуска
//! и забытые результаты сборки в рабочих копиях.

use std::{
    collections::HashSet,
    fs, io,
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use canterlot_proto::worker::ipc::{Ownership, OwnershipPhase};
use uuid::Uuid;

/// Удаляет сокеты без каталога исполнения и каталоги, оставшиеся от прерванного запуска.
/// Не трогает ничего, на что указывает непогашенная запись принадлежности или живой сокет.
pub(crate) fn sweep_runtime(
    data_dir: &Path,
    runtime_dir: Option<&Path>,
    records: &[Ownership],
    spawn_timeout: Duration,
    now: SystemTime,
) -> io::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    guard_test_runtime(runtime_dir)?;
    let executions = data_dir.join("executions");
    let known: HashSet<Uuid> = records
        .iter()
        .filter_map(|record| Uuid::parse_str(&record.execution_id).ok())
        .collect();
    let unreleased_sockets: HashSet<PathBuf> = records
        .iter()
        .filter(|record| record.phase != OwnershipPhase::Released as i32)
        .map(|record| PathBuf::from(&record.ipc_socket))
        .collect();

    if let Ok(entries) = fs::read_dir(&executions) {
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
            if known.contains(&id) {
                continue;
            }
            let socket = socket_path(runtime_dir, &executions, id);
            if is_live_socket(&socket)
                || is_live_socket(&executions.join(id.to_string()).join("ipc.sock"))
            {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > spawn_timeout);
            if old {
                match fs::remove_dir_all(entry.path()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }

    let Some(runtime_dir) = runtime_dir else {
        return Ok(());
    };
    let sockets = runtime_dir.join("canterlot");
    let Ok(entries) = fs::read_dir(&sockets) else {
        return Ok(());
    };
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
        if executions.join(id.to_string()).exists()
            || unreleased_sockets.contains(&path)
            || is_live_socket(&path)
        {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// В тестовой сборке уборка не имеет права трогать общий каталог сокетов.
#[cfg(any(test, feature = "fault-injection"))]
fn guard_test_runtime(runtime_dir: Option<&Path>) -> io::Result<()> {
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

/// Рабочие копии, где сборка не менялась дольше `idle`.
pub(crate) struct IdleWorktree {
    pub conversation_id: String,
    pub path: PathBuf,
}

pub(crate) fn idle_worktrees(
    data_dir: &Path,
    now: SystemTime,
    idle: Duration,
) -> io::Result<Vec<IdleWorktree>> {
    let worktrees = data_dir.join("worktrees");
    let Ok(entries) = fs::read_dir(&worktrees) else {
        return Ok(Vec::new());
    };
    let mut idle_worktrees = Vec::new();
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
        let Some(target) = direct_dir(&worktree, "target") else {
            continue;
        };
        let Some(newest) = newest_mtime(&target)? else {
            continue;
        };
        if !now.duration_since(newest).is_ok_and(|age| age > idle) {
            continue;
        }
        idle_worktrees.push(IdleWorktree {
            conversation_id: name.to_owned(),
            path: worktree,
        });
    }
    Ok(idle_worktrees)
}

/// Убирает `target` и кэш `node_modules/.cache` в рабочей копии.
pub(crate) fn remove_build(worktree: &Path) -> io::Result<()> {
    let Some(target) = direct_dir(worktree, "target") else {
        return Ok(());
    };
    fs::remove_dir_all(&target)?;
    if let Some(modules) = direct_dir(worktree, "node_modules")
        && let Some(cache) = direct_dir(&modules, ".cache")
    {
        fs::remove_dir_all(cache)?;
    }
    Ok(())
}

fn direct_dir(parent: &Path, name: &str) -> Option<PathBuf> {
    let path = parent.join(name);
    let meta = fs::symlink_metadata(&path).ok()?;
    (meta.is_dir() && !meta.file_type().is_symlink()).then_some(path)
}

fn newest_mtime(path: &Path) -> io::Result<Option<SystemTime>> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(None);
    }
    let mut newest = meta.modified().ok();
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if let Ok(Some(time)) = newest_mtime(&entry.path()) {
                newest = Some(newest.map_or(time, |current| current.max(time)));
            }
        }
    }
    Ok(newest)
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

    use super::{idle_worktrees, remove_build, sweep_runtime};

    fn old(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(8 * 3600);
        fs::File::open(path).unwrap().set_modified(old).unwrap();
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
        fs::create_dir_all(idle_tree.join("node_modules/.cache")).unwrap();
        fs::write(idle_tree.join("target/debug/old"), "old").unwrap();
        fs::write(idle_tree.join("node_modules/.cache/old"), "old").unwrap();
        fs::write(idle_tree.join("keep.txt"), "keep").unwrap();
        old_tree(&idle_tree.join("target"));
        old_tree(&idle_tree.join("node_modules/.cache"));
        old(&idle_tree);

        let live = Uuid::now_v7();
        let live_tree = worktrees.join(live.to_string());
        fs::create_dir_all(live_tree.join("target")).unwrap();
        fs::write(live_tree.join("target/old"), "old").unwrap();
        old_tree(&live_tree.join("target"));
        let live_conversation = live.to_string();

        let fresh = Uuid::now_v7();
        let fresh_tree = worktrees.join(fresh.to_string());
        fs::create_dir_all(fresh_tree.join("target")).unwrap();
        fs::write(fresh_tree.join("target/new"), "new").unwrap();

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
        fs::create_dir_all(cache_tree.join("target")).unwrap();
        fs::write(cache_tree.join("target/old"), "old").unwrap();
        old_tree(&cache_tree.join("target"));
        let cache_outside = root.path().join("cache-outside");
        fs::create_dir_all(&cache_outside).unwrap();
        std::os::unix::fs::symlink(&cache_outside, cache_tree.join("node_modules")).unwrap();

        let idle_builds =
            idle_worktrees(&data, SystemTime::now(), Duration::from_secs(6 * 3600)).unwrap();
        for worktree in idle_builds {
            if worktree.conversation_id == live_conversation {
                continue;
            }
            remove_build(&worktree.path).unwrap();
        }

        assert!(!idle_tree.join("target").exists());
        assert!(!idle_tree.join("node_modules/.cache").exists());
        assert!(idle_tree.join("keep.txt").exists());
        assert!(live_tree.join("target/old").exists());
        assert!(fresh_tree.join("target/new").exists());
        assert!(foreign.join("target/old").exists());
        assert!(outside.join("keep").exists());
        assert!(linked_tree.join("target").is_symlink());
        assert!(!cache_tree.join("target").exists());
        assert!(cache_tree.join("node_modules").is_symlink());
        assert!(cache_outside.exists());
    }
    #[test]
    fn sweep_refuses_runtime_outside_the_temp_dir() {
        let root = tempfile::Builder::new()
            .prefix("cl-")
            .tempdir_in("/tmp")
            .unwrap();
        let error = sweep_runtime(
            &root.path().join("data"),
            Some(Path::new("/canterlot-worker-test-runtime")),
            &[],
            Duration::from_secs(10),
            SystemTime::now(),
        )
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

        sweep_runtime(
            &data,
            Some(&runtime),
            &[released, unreleased, released_dir_record],
            Duration::from_secs(10),
            SystemTime::now(),
        )
        .unwrap();

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
