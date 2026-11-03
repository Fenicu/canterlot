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

    use super::sweep_runtime;

    fn old(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(60);
        fs::File::open(path).unwrap().set_modified(old).unwrap();
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
