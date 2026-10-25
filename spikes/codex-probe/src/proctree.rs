//! Снимок дерева процессов из `/proc` (Linux): потомки app-server и их судьба.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    /// Время старта в тиках с загрузки: вместе с pid отличает процесс от переиспользованного pid.
    pub start_ticks: u64,
    pub comm: String,
    pub cmdline: String,
}

/// Разбор `/proc/<pid>/stat`: имя в скобках может содержать пробелы и скобки.
pub fn parse_stat(stat: &str) -> Option<(i32, String, i32, i32, i32, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let pid: i32 = stat[..open].trim().parse().ok()?;
    let comm = stat[open + 1..close].to_string();
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // rest[0]=state, [1]=ppid, [2]=pgrp, [3]=session, ..., [19]=starttime
    let ppid = rest.get(1)?.parse().ok()?;
    let pgid = rest.get(2)?.parse().ok()?;
    let sid = rest.get(3)?.parse().ok()?;
    let start = rest.get(19)?.parse().ok()?;
    Some((pid, comm, ppid, pgid, sid, start))
}

pub fn snapshot() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((pid, comm, ppid, pgid, sid, start_ticks)) = parse_stat(&stat) else {
            continue;
        };
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                b.split(|&c| c == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push(ProcInfo {
            pid,
            ppid,
            pgid,
            sid,
            start_ticks,
            comm,
            cmdline: shorten(&cmdline),
        });
    }
    out
}

/// Длинные командные строки (аргументы app-server) сокращаются для evidence.
fn shorten(s: &str) -> String {
    const MAX: usize = 160;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut cut = MAX;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}

/// Все потомки `root` (включая его самого), по цепочке ppid.
pub fn descendants(all: &[ProcInfo], root: i32) -> Vec<ProcInfo> {
    let mut result: Vec<ProcInfo> = all.iter().filter(|p| p.pid == root).cloned().collect();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for p in all.iter().filter(|p| p.ppid == parent) {
            if !result.iter().any(|r| r.pid == p.pid) {
                result.push(p.clone());
                frontier.push(p.pid);
            }
        }
    }
    result
}

/// Жив ли ровно тот же процесс (pid + время старта).
pub fn still_alive(p: &ProcInfo) -> bool {
    std::fs::read_to_string(format!("/proc/{}/stat", p.pid))
        .ok()
        .and_then(|s| parse_stat(&s))
        .is_some_and(|(_, _, _, _, _, start)| start == p.start_ticks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_spaces_in_comm() {
        let stat =
            "1234 (my (odd) proc) S 1 1234 1234 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        let (pid, comm, ppid, pgid, sid, start) = parse_stat(stat).expect("parse");
        assert_eq!((pid, ppid, pgid, sid, start), (1234, 1, 1234, 1234, 98765));
        assert_eq!(comm, "my (odd) proc");
    }

    #[test]
    fn collects_transitive_descendants() {
        let mk = |pid, ppid| ProcInfo {
            pid,
            ppid,
            pgid: 0,
            sid: 0,
            start_ticks: 0,
            comm: String::new(),
            cmdline: String::new(),
        };
        let all = vec![mk(10, 1), mk(11, 10), mk(12, 11), mk(13, 1), mk(14, 12)];
        let mut d: Vec<i32> = descendants(&all, 10).iter().map(|p| p.pid).collect();
        d.sort();
        assert_eq!(d, vec![10, 11, 12, 14]);
    }

    #[test]
    fn current_process_is_alive() {
        let me = std::process::id() as i32;
        let snap = snapshot();
        let p = snap.iter().find(|p| p.pid == me).expect("self in /proc");
        assert!(still_alive(p));
    }
}
