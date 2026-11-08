//! Процессы, пережившие свои исполнения, и сборки вне очереди.
//!
//! Утечка — процесс с меткой `CANTERLOT_EXECUTION_GROUP=<id>`, где `<id>` — отпущенная
//! запись принадлежности в хранилище этого воркера, и процесс не происходит ни от
//! одного живого участника исполнения. Незнакомые метки (тестовые супервизоры в
//! гейте) не считаются: о них воркер ничего не знает.
//!
//! Сборка вне очереди — процесс `cargo`, `rustc`, `cargo-nextest`, `rustdoc` или
//! `clippy-driver` без `CANTERLOT_LEASE_HELD` в окружении. Считаются только верхние
//! процессы: `cargo` со своими `rustc` — одна сборка.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::Path,
};

use canterlot_proto::{limits::MAX_WATCH_LEAKED, worker::v1::LeakedProcess};

use crate::containment::GROUP_MARKER_ENV;

pub const LEASE_HELD_ENV: &str = "CANTERLOT_LEASE_HELD";
pub const BUILD_COMMS: &[&str] = &[
    "cargo",
    "rustc",
    "cargo-nextest",
    "rustdoc",
    "clippy-driver",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub start_ticks: u64,
    pub comm: String,
    pub rss_bytes: u64,
    /// Окружение прочитано; чужой или закрытый процесс — `None`.
    pub env: Option<ProcEnv>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcEnv {
    pub marker: Option<String>,
    pub lease_held: bool,
}

/// Происходит ли процесс от живого участника исполнения. Предок, начавшийся позже
/// потомка, — уже другой процесс под старым номером.
fn descends_from_live(
    info: &ProcInfo,
    live: &HashSet<(u32, u64)>,
    all: &HashMap<u32, ProcInfo>,
) -> bool {
    let mut current = info;
    let mut seen = HashSet::new();
    while seen.insert(current.pid) {
        if live.contains(&(current.pid, current.start_ticks)) {
            return true;
        }
        let Some(parent) = all.get(&current.ppid) else {
            return false;
        };
        if parent.start_ticks > current.start_ticks {
            return false;
        }
        current = parent;
    }
    false
}

/// Утечки по правилу модуля, крупные по памяти первыми.
pub fn leaked(
    all: &HashMap<u32, ProcInfo>,
    released: &HashSet<String>,
    live: &HashSet<(u32, u64)>,
    uptime_ticks: u64,
    ticks_per_second: u64,
) -> Vec<LeakedProcess> {
    let mut rows: Vec<_> = all
        .values()
        .filter_map(|info| {
            let marker = info.env.as_ref()?.marker.as_ref()?;
            if !released.contains(marker) || descends_from_live(info, live, all) {
                return None;
            }
            Some(LeakedProcess {
                execution_id: marker.clone(),
                pid: info.pid,
                comm: info.comm.clone(),
                rss_bytes: info.rss_bytes,
                age_s: uptime_ticks.saturating_sub(info.start_ticks) / ticks_per_second.max(1),
            })
        })
        .collect();
    rows.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid)));
    rows.truncate(MAX_WATCH_LEAKED);
    rows
}

fn unleased_build(info: &ProcInfo) -> bool {
    BUILD_COMMS.contains(&info.comm.as_str())
        && info.env.as_ref().is_some_and(|env| !env.lease_held)
}

/// Сборки без аренды: верхние процессы сборки, чей родитель сам не сборка.
pub fn unleased_count(all: &HashMap<u32, ProcInfo>) -> u32 {
    let count = all
        .values()
        .filter(|info| unleased_build(info))
        .filter(|info| {
            !all.get(&info.ppid)
                .is_some_and(|parent| BUILD_COMMS.contains(&parent.comm.as_str()))
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn parse_stat(text: &str) -> Option<(String, u32, u64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_owned();
    let fields: Vec<_> = text.get(close + 1..)?.split_whitespace().collect();
    if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
        return None;
    }
    let ppid = fields.get(1)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    Some((comm, ppid, start))
}

fn parse_env(bytes: &[u8]) -> ProcEnv {
    let marker_prefix = format!("{GROUP_MARKER_ENV}=");
    let lease_prefix = format!("{LEASE_HELD_ENV}=");
    let mut env = ProcEnv::default();
    for entry in bytes.split(|byte| *byte == 0) {
        if let Some(value) = entry.strip_prefix(marker_prefix.as_bytes()) {
            env.marker = std::str::from_utf8(value).ok().map(str::to_owned);
        } else if entry
            .strip_prefix(lease_prefix.as_bytes())
            .is_some_and(|value| !value.is_empty())
        {
            env.lease_held = true;
        }
    }
    env
}

/// Один процесс: stat, окружение, снова stat — номер мог перейти к другому процессу,
/// пока читалось окружение.
fn read_proc(dir: &Path, pid: u32) -> Option<ProcInfo> {
    let (comm, ppid, start) = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
    let env = match fs::read(dir.join("environ")) {
        Ok(bytes) => Some(parse_env(&bytes)),
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(rustix::io::Errno::ACCESS.raw_os_error()) =>
        {
            None
        }
        Err(_) => return None,
    };
    let (_, _, again) = parse_stat(&fs::read_to_string(dir.join("stat")).ok()?)?;
    if again != start {
        return None;
    }
    let rss_bytes = fs::read_to_string(dir.join("statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(rustix::param::page_size() as u64);
    Some(ProcInfo {
        pid,
        ppid,
        start_ticks: start,
        comm,
        rss_bytes,
        env,
    })
}

pub fn process_table(proc_root: &Path) -> io::Result<HashMap<u32, ProcInfo>> {
    let own = std::process::id();
    let mut all = HashMap::new();
    for entry in fs::read_dir(proc_root)?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        if let Some(info) = read_proc(&entry.path(), pid) {
            all.insert(pid, info);
        }
    }
    Ok(all)
}

/// Время с загрузки в тиках часов: возраст процесса считается от его `starttime`.
pub fn uptime_ticks(proc_root: &Path) -> io::Result<u64> {
    let text = fs::read_to_string(proc_root.join("uptime"))?;
    let seconds: f64 = text
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "uptime"))?;
    Ok((seconds * rustix::param::clock_ticks_per_second() as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, start: u64, comm: &str, env: Option<ProcEnv>) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            start_ticks: start,
            comm: comm.into(),
            rss_bytes: u64::from(pid) * 1000,
            env,
        }
    }

    fn marked(id: &str) -> Option<ProcEnv> {
        Some(ProcEnv {
            marker: Some(id.into()),
            lease_held: false,
        })
    }

    fn table(rows: Vec<ProcInfo>) -> HashMap<u32, ProcInfo> {
        rows.into_iter().map(|p| (p.pid, p)).collect()
    }

    #[test]
    fn leak_needs_a_released_record_and_no_live_ancestor() {
        let released: HashSet<String> = ["old".to_owned()].into();
        // Живое исполнение: супервизор 10, CLI 11.
        let live: HashSet<(u32, u64)> = [(10, 100), (11, 101)].into();
        let all = table(vec![
            proc(1, 0, 1, "systemd", None),
            proc(10, 1, 100, "canterlot-worker", None),
            proc(11, 10, 101, "claude", marked("live")),
            // Отпущенное исполнение: процессы переподвешены к init.
            proc(20, 1, 200, "cargo", marked("old")),
            proc(21, 20, 201, "node", marked("old")),
            // Метка отпущенного исполнения, но потомок живого: тестовый супервизор
            // под гейтом живой сессии несёт настоящую метку родителя — не утечка.
            proc(30, 11, 300, "cargo", marked("old")),
            // Незнакомая метка (тестовый воркер): не утечка.
            proc(40, 1, 400, "sleep", marked("unknown")),
            // Ещё один переживший процесс того же исполнения.
            proc(50, 1, 500, "node", marked("old")),
            // Окружение не прочитано.
            proc(60, 1, 600, "cargo", None),
        ]);
        let rows = leaked(&all, &released, &live, 1000, 10);
        let pids: Vec<_> = rows.iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![50, 21, 20], "largest RSS first");
        let cargo = rows.iter().find(|r| r.pid == 20).unwrap();
        assert_eq!(cargo.execution_id, "old");
        assert_eq!(cargo.comm, "cargo");
        assert_eq!(cargo.age_s, 80);
    }

    #[test]
    fn reparented_pid_reuse_does_not_count_as_live_descent() {
        let released: HashSet<String> = ["old".to_owned()].into();
        let live: HashSet<(u32, u64)> = [(11, 101)].into();
        // Родитель с номером живого участника начался позже потомка: другой процесс.
        let all = table(vec![
            proc(11, 1, 900, "bash", None),
            proc(20, 11, 200, "cargo", marked("old")),
        ]);
        assert_eq!(leaked(&all, &released, &live, 1000, 10).len(), 1);
    }

    #[test]
    fn unleased_counts_top_build_processes_without_the_lease_marker() {
        let free = Some(ProcEnv::default());
        let held = Some(ProcEnv {
            marker: None,
            lease_held: true,
        });
        let all = table(vec![
            proc(1, 0, 1, "bash", free.clone()),
            // Сборка без аренды: cargo и его rustc — одна.
            proc(10, 1, 10, "cargo", free.clone()),
            proc(11, 10, 11, "rustc", free.clone()),
            proc(12, 10, 12, "rustc", free.clone()),
            // nextest под cargo — та же сборка.
            proc(13, 10, 13, "cargo-nextest", free.clone()),
            // Арендованная сборка.
            proc(20, 1, 20, "cargo", held.clone()),
            proc(21, 20, 21, "rustc", held),
            // Отдельный clippy без аренды.
            proc(30, 1, 30, "clippy-driver", free.clone()),
            // node не считается.
            proc(40, 1, 40, "node", free),
            // Окружение закрыто: не считаем.
            proc(50, 1, 50, "cargo", None),
        ]);
        assert_eq!(unleased_count(&all), 2);
    }

    #[test]
    fn env_parse_reads_marker_and_lease() {
        let env = parse_env(b"PATH=/bin\0CANTERLOT_EXECUTION_GROUP=abc\0CANTERLOT_LEASE_HELD=l1\0");
        assert_eq!(env.marker.as_deref(), Some("abc"));
        assert!(env.lease_held);
        assert!(!parse_env(b"CANTERLOT_LEASE_HELD=\0").lease_held);
    }

    #[test]
    fn live_process_table_sees_a_marked_child_as_leaked_only_without_live_parent() {
        let id = uuid::Uuid::now_v7().to_string();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env(GROUP_MARKER_ENV, &id)
            .env(LEASE_HELD_ENV, "lease-1")
            .spawn()
            .unwrap();
        let pid = child.id();
        let all = process_table(Path::new("/proc")).unwrap();
        let info = all.get(&pid).cloned().unwrap();
        assert_eq!(info.comm, "sleep");
        assert_eq!(
            info.env.as_ref().unwrap().marker.as_deref(),
            Some(id.as_str())
        );
        let released: HashSet<String> = [id.clone()].into();
        let ticks = rustix::param::clock_ticks_per_second();
        let uptime = uptime_ticks(Path::new("/proc")).unwrap();
        // Родитель — этот тест; он не участник исполнения: утечка.
        let rows = leaked(&all, &released, &HashSet::new(), uptime, ticks);
        assert!(rows.iter().any(|r| r.pid == pid && r.comm == "sleep"));
        // Тест объявлен живым участником: потомок не утечка.
        let me = all
            .get(&info.ppid)
            .map(|p| (p.pid, p.start_ticks))
            .unwrap_or((std::process::id(), 0));
        let mut live = HashSet::new();
        live.insert(me);
        let mut with_me = all.clone();
        if !with_me.contains_key(&me.0) {
            with_me.insert(
                me.0,
                ProcInfo {
                    pid: me.0,
                    ppid: 1,
                    start_ticks: me.1,
                    comm: "test".into(),
                    rss_bytes: 0,
                    env: None,
                },
            );
        }
        let rows = leaked(&with_me, &released, &live, uptime, ticks);
        assert!(!rows.iter().any(|r| r.pid == pid));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
