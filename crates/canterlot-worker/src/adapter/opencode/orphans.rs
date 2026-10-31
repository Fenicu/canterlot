//! Сироты. SIGKILL сервера оставляет его `shell` живыми: у каждого своя сессия и группа, а
//! `GET /api/shell` нового сервера пуст. Пока жив супервизор, их снимает дерево процессов.
//! Если умерли оба, остаётся этот журнал: новый запуск читает его до старта сервера.

use super::http::Http;
use crate::containment::start_ticks;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const LIST_WAIT: Duration = Duration::from_secs(5);
/// В `shell.created` идентификатора процесса нет, а в списке он появляется не сразу.
const ATTEMPTS: [Duration; 3] = [
    Duration::from_millis(0),
    Duration::from_millis(100),
    Duration::from_millis(500),
];

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct Entry {
    pid: u32,
    start_ticks: u64,
}

pub fn journal_path(root: &Path, session: &str) -> PathBuf {
    root.join("shells").join(format!("{session}.json"))
}

fn running(list: &Value, owners: &HashSet<String>) -> (Vec<u32>, bool) {
    let mut complete = true;
    let mut pids = Vec::new();
    let shells = list.get("data").and_then(Value::as_array);
    for shell in shells.into_iter().flatten() {
        let owned = shell
            .pointer("/metadata/sessionID")
            .and_then(Value::as_str)
            .is_some_and(|session| owners.contains(session));
        if !owned || shell.get("status").and_then(Value::as_str) != Some("running") {
            continue;
        }
        match shell
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
        {
            Some(pid) => pids.push(pid),
            None => complete = false,
        }
    }
    (pids, complete)
}

fn write(path: &Path, entries: &[Entry]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(entries)?)?;
    fs::rename(temporary, path)
}

/// Снимок работающих команд сессии. Устаревшая запись безвредна: процесс сверяется при чтении.
pub async fn record(http: Http, path: PathBuf, owners: HashSet<String>) {
    let mut pids = Vec::new();
    for delay in ATTEMPTS {
        tokio::time::sleep(delay).await;
        let Ok(list) = http.get("/api/shell", LIST_WAIT).await else {
            return;
        };
        let (found, complete) = running(&list, &owners);
        pids = found;
        if complete {
            break;
        }
    }
    let _ = tokio::task::spawn_blocking(move || {
        let entries: Vec<Entry> = pids
            .into_iter()
            .filter_map(|pid| {
                Some(Entry {
                    pid,
                    start_ticks: start_ticks(pid).ok()?,
                })
            })
            .collect();
        write(&path, &entries)
    })
    .await;
}

fn kill_group(entry: &Entry) -> bool {
    use rustix::process::{Pid, Signal, kill_process_group};
    if start_ticks(entry.pid).ok() != Some(entry.start_ticks) {
        return false;
    }
    i32::try_from(entry.pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| kill_process_group(pid, Signal::KILL).is_ok())
}

/// Снимает записанные команды прошлого сервера. Сверка времени старта не даёт задеть чужой
/// процесс с тем же номером. Возвращает, сколько снято.
pub fn sweep(path: &Path) -> io::Result<usize> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let entries: Vec<Entry> = serde_json::from_slice(&bytes).unwrap_or_default();
    let killed = entries.iter().filter(|entry| kill_group(entry)).count();
    fs::remove_file(path)?;
    Ok(killed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_running_shells_of_the_session_are_recorded() {
        let list = json!({"data": [
            {"status": "running", "pid": 10, "metadata": {"sessionID": "ses_1"}},
            {"status": "exited", "pid": 11, "metadata": {"sessionID": "ses_1"}},
            {"status": "running", "pid": 12, "metadata": {"sessionID": "ses_2"}},
        ]});
        let owners = HashSet::from(["ses_1".to_owned()]);
        assert_eq!(running(&list, &owners), (vec![10], true));
        let early = json!({"data": [{"status": "running", "metadata": {"sessionID": "ses_1"}}]});
        assert_eq!(running(&early, &owners), (Vec::new(), false));
    }

    #[test]
    fn foreign_process_with_the_same_pid_stays_alive() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = journal_path(dir.path(), "ses_1");
        let own = std::process::id();
        let entry = Entry {
            pid: own,
            start_ticks: start_ticks(own).unwrap() + 1,
        };
        write(&path, &[entry]).unwrap();
        assert_eq!(sweep(&path).unwrap(), 0);
        assert!(!path.exists());
        assert_eq!(sweep(&path).unwrap(), 0, "no journal");
    }
}
