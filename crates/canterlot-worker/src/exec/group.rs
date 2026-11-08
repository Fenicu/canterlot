//! Группа исполнения после смерти сопровождения. Дерево родителей уже разорвано, поэтому
//! участник опознаётся по тому, что держит ядро или записало сопровождение: сессия, метка в
//! окружении, последний снимок участников, родство с любым из них. Ложная пустота хуже
//! ложной занятости: занятая группа всего лишь ждёт решения владельца, поэтому всякое
//! сомнение даёт не `Empty`.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::Path,
    time::Duration,
};

use canterlot_proto::worker::ipc::Ownership;
use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
use tokio::time::{Instant, sleep};

use crate::containment::{
    ContainmentError, GROUP_MARKER_ENV, Member, Stat, process_table, read_group_file, start_ticks,
};

/// Между проходами: процесс мог породить потомка и умереть, пока обход шёл мимо обоих.
const SECOND_PASS: Duration = Duration::from_millis(100);
const TERMINATE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Group {
    Empty,
    Occupied(Vec<Member>),
    Unverifiable,
}

fn marked(pid: u32, expected: &[u8]) -> io::Result<bool> {
    match fs::read(format!("/proc/{pid}/environ")) {
        Ok(bytes) => Ok(bytes
            .split(|byte| *byte == 0)
            .any(|entry| entry == expected)),
        // Чужие и закрытые от чтения процессы метку не показывают: их находит снимок.
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn descends(info: &Stat, members: &HashSet<u32>, all: &HashMap<u32, Stat>) -> bool {
    let mut current = info;
    let mut seen = HashSet::new();
    while seen.insert(current.member.pid) {
        let Some(parent) = all.get(&current.parent) else {
            return false;
        };
        if parent.member.start_ticks > current.member.start_ticks {
            return false;
        }
        if members.contains(&parent.member.pid) {
            return true;
        }
        current = parent;
    }
    false
}

fn list(record: &Ownership, snapshot: &[Member]) -> Result<Vec<Member>, ContainmentError> {
    let all = process_table()?;
    let expected = format!("{GROUP_MARKER_ENV}={}", record.execution_id).into_bytes();
    let supervisor = Member {
        pid: record.supervisor_pid,
        start_ticks: record.supervisor_start_ticks,
    };
    let cli = Member {
        pid: record.cli_pid,
        start_ticks: record.cli_start_ticks,
    };
    // Номер сессии занят, пока жив её участник; под другим лидером это уже чужая сессия.
    let session_ours = all
        .get(&supervisor.pid)
        .is_none_or(|leader| leader.member == supervisor);
    let own = std::process::id();
    let mut members = HashSet::new();
    for info in all.values().filter(|info| !info.zombie) {
        if info.member.pid == own {
            continue;
        }
        if info.member == supervisor
            || info.member == cli
            || (session_ours
                && info.session == supervisor.pid
                && info.member.start_ticks >= supervisor.start_ticks)
            || snapshot.contains(&info.member)
            || marked(info.member.pid, &expected)?
        {
            members.insert(info.member.pid);
        }
    }
    let mut found: Vec<Member> = all
        .values()
        .filter(|info| !info.zombie && info.member.pid != own)
        .filter(|info| members.contains(&info.member.pid) || descends(info, &members, &all))
        .map(|info| info.member.clone())
        .collect();
    found.sort_by_key(|member| member.pid);
    Ok(found)
}

/// Один проход. Запись без сопровождения или без снимка не доказывает пустоту, даже когда
/// найти нечего: такой группы могло не быть в поле зрения вовсе.
fn inspect(record: &Ownership) -> Group {
    if record.supervisor_pid == 0 {
        return Group::Unverifiable;
    }
    let snapshot = read_group_file(Path::new(&record.exec_dir));
    let members = list(
        record,
        snapshot
            .as_ref()
            .ok()
            .and_then(Option::as_deref)
            .unwrap_or(&[]),
    );
    match (members, snapshot) {
        (Ok(members), _) if !members.is_empty() => Group::Occupied(members),
        (Ok(_), Ok(Some(_))) => Group::Empty,
        _ => Group::Unverifiable,
    }
}

async fn inspect_blocking(record: &Ownership) -> Group {
    let record = record.clone();
    tokio::task::spawn_blocking(move || inspect(&record))
        .await
        .unwrap_or(Group::Unverifiable)
}

pub(super) async fn confirmed(record: &Ownership) -> Group {
    match inspect_blocking(record).await {
        Group::Empty => {}
        other => {
            tracing::debug!(execution_id = %record.execution_id, group = %describe(&other), "group pass: not empty");
            return other;
        }
    }
    sleep(SECOND_PASS).await;
    let second = inspect_blocking(record).await;
    tracing::debug!(execution_id = %record.execution_id, group = %describe(&second), "group second pass");
    second
}

/// Участники с их состоянием в `/proc` для отладочных строк.
fn describe(group: &Group) -> String {
    match group {
        Group::Occupied(members) => members
            .iter()
            .map(|member| format!("{}[{}]", member.pid, super::procfs::describe(member.pid)))
            .collect::<Vec<_>>()
            .join(", "),
        other => format!("{other:?}"),
    }
}

/// Дескриптор держит процесс, поэтому сигнал не уйдёт новому владельцу номера.
fn signal(member: &Member, signal: Signal) -> bool {
    let Some(pid) = i32::try_from(member.pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    let Ok(descriptor) = pidfd_open(pid, PidfdFlags::empty()) else {
        return false;
    };
    start_ticks(member.pid).is_ok_and(|ticks| ticks == member.start_ticks)
        && pidfd_send_signal(&descriptor, signal).is_ok()
}

/// Завершение остатка группы по решению владельца: один SIGTERM на процесс, затем SIGKILL.
pub(super) async fn terminate(record: &Ownership, grace: Duration) -> Group {
    for (kind, window) in [(Signal::TERM, grace), (Signal::KILL, grace)] {
        let deadline = Instant::now() + window;
        let mut signalled = HashSet::new();
        loop {
            let found = inspect_blocking(record).await;
            tracing::debug!(execution_id = %record.execution_id, signal = ?kind, group = %describe(&found), "terminate pass");
            let Group::Occupied(members) = found else {
                break;
            };
            for member in members {
                if !signalled.contains(&member) {
                    let sent = signal(&member, kind);
                    tracing::debug!(execution_id = %record.execution_id, pid = member.pid, signal = ?kind, sent, "terminate signal");
                    if sent {
                        signalled.insert(member);
                    }
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(TERMINATE_POLL).await;
        }
    }
    confirmed(record).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containment::write_group_file;
    use std::process::{Child, Command, Stdio};

    struct Fixture {
        dir: tempfile::TempDir,
        id: String,
        children: Vec<Child>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for child in &mut self.children {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn member(pid: u32) -> Member {
        Member {
            pid,
            start_ticks: start_ticks(pid).unwrap(),
        }
    }

    fn sleeper() -> Command {
        let mut command = Command::new("sleep");
        command.arg("30").stdin(Stdio::null());
        command
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            write_group_file(dir.path(), &[]).unwrap();
            Self {
                dir,
                id: uuid::Uuid::now_v7().to_string(),
                children: Vec::new(),
            }
        }

        /// Запись умершего сопровождения: настоящий завершившийся процесс.
        fn record(&mut self) -> Ownership {
            let mut child = Command::new("sleep").arg("30").spawn().unwrap();
            let dead = member(child.id());
            child.kill().unwrap();
            child.wait().unwrap();
            Ownership {
                execution_id: self.id.clone(),
                exec_dir: self.dir.path().to_string_lossy().into_owned(),
                supervisor_pid: dead.pid,
                supervisor_start_ticks: dead.start_ticks,
                ..Default::default()
            }
        }

        fn spawn(&mut self, command: &mut Command) -> Member {
            let child = command.spawn().unwrap();
            let found = member(child.id());
            self.children.push(child);
            found
        }

        fn marked(&mut self) -> Member {
            let marker = self.id.clone();
            self.spawn(sleeper().env(GROUP_MARKER_ENV, marker))
        }

        /// Свежий потомок виден сканеру не сразу: ждём ту же метку, которую читает `list`.
        async fn wait_observable(&self, survivor: &Member) {
            let expected = format!("{GROUP_MARKER_ENV}={}", self.id).into_bytes();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !marked(survivor.pid, &expected).is_ok_and(|found| found) {
                assert!(
                    Instant::now() < deadline,
                    "pid {} never showed its group marker",
                    survivor.pid
                );
                sleep(Duration::from_millis(10)).await;
            }
        }
    }

    fn occupied(group: Group) -> Vec<u32> {
        match group {
            Group::Occupied(members) => members.into_iter().map(|m| m.pid).collect(),
            other => panic!("expected occupied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dead_supervisor_with_nothing_left_is_empty() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        assert_eq!(confirmed(&record).await, Group::Empty);
    }

    #[tokio::test]
    async fn marked_process_outside_any_tree_keeps_the_group_occupied() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        let survivor = fixture.marked();
        assert_eq!(occupied(confirmed(&record).await), [survivor.pid]);
        let child = fixture.children.last_mut().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(confirmed(&record).await, Group::Empty);
    }

    #[tokio::test]
    async fn marker_of_another_execution_is_not_ours() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        fixture.spawn(sleeper().env(GROUP_MARKER_ENV, uuid::Uuid::now_v7().to_string()));
        assert_eq!(confirmed(&record).await, Group::Empty);
    }

    #[tokio::test]
    async fn snapshot_finds_a_process_that_lost_the_marker_but_not_a_reused_pid() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        let scrubbed = fixture.spawn(sleeper().env_remove(GROUP_MARKER_ENV));
        write_group_file(fixture.dir.path(), std::slice::from_ref(&scrubbed)).unwrap();
        assert_eq!(occupied(confirmed(&record).await), [scrubbed.pid]);
        let reused = Member {
            pid: scrubbed.pid,
            start_ticks: scrubbed.start_ticks + 1,
        };
        write_group_file(fixture.dir.path(), &[reused]).unwrap();
        assert_eq!(confirmed(&record).await, Group::Empty);
    }

    #[tokio::test]
    async fn child_of_a_member_belongs_even_without_a_marker_of_its_own() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        let marker = fixture.id.clone();
        let parent = fixture.spawn(
            Command::new("sh")
                .args(["-c", "env -u CANTERLOT_EXECUTION_GROUP sleep 30 & wait"])
                .env(GROUP_MARKER_ENV, marker),
        );
        let pids = loop {
            let pids = occupied(confirmed(&record).await);
            if pids.len() == 2 {
                break pids;
            }
            sleep(Duration::from_millis(20)).await;
        };
        assert!(pids.contains(&parent.pid));
        let guardian: Vec<_> = pids.into_iter().filter(|pid| *pid != parent.pid).collect();
        let _ = rustix::process::kill_process(
            Pid::from_raw(i32::try_from(guardian[0]).unwrap()).unwrap(),
            Signal::KILL,
        );
    }

    #[tokio::test]
    async fn orphan_in_the_session_of_a_dead_leader_keeps_the_group_occupied() {
        let mut fixture = Fixture::new();
        // Лидер сессии умирает первым, как сопровождение: потомок остаётся без предка и метки.
        let mut leader = Command::new("setsid")
            .args(["--wait", "sh", "-c", "sleep 30 & echo $! ; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(leader.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let orphan: u32 = line.trim().parse().unwrap();
        let session = process_table().unwrap()[&orphan].session;
        let dead = member(session);
        rustix::process::kill_process(
            Pid::from_raw(i32::try_from(session).unwrap()).unwrap(),
            Signal::KILL,
        )
        .unwrap();
        while process_table()
            .unwrap()
            .get(&session)
            .is_some_and(|info| !info.zombie)
        {
            sleep(Duration::from_millis(10)).await;
        }
        let mut record = fixture.record();
        record.supervisor_pid = dead.pid;
        record.supervisor_start_ticks = dead.start_ticks;
        let group = confirmed(&record).await;
        let _ = rustix::process::kill_process(
            Pid::from_raw(i32::try_from(orphan).unwrap()).unwrap(),
            Signal::KILL,
        );
        let _ = leader.wait();
        assert!(occupied(group).contains(&orphan));
    }

    #[tokio::test]
    async fn live_supervisor_is_never_empty() {
        let mut fixture = Fixture::new();
        let mut record = fixture.record();
        let supervisor = fixture.spawn(&mut sleeper());
        record.supervisor_pid = supervisor.pid;
        record.supervisor_start_ticks = supervisor.start_ticks;
        assert_eq!(occupied(confirmed(&record).await), [supervisor.pid]);
    }

    #[tokio::test]
    async fn session_number_under_another_leader_is_foreign() {
        let mut fixture = Fixture::new();
        let mut record = fixture.record();
        let foreign = fixture.spawn(Command::new("setsid").args(["--wait", "sleep", "30"]));
        let leader = process_table().unwrap()[&foreign.pid].session;
        let leader = member(leader);
        record.supervisor_pid = leader.pid;
        record.supervisor_start_ticks = leader.start_ticks + 1;
        assert_eq!(confirmed(&record).await, Group::Empty);
    }

    #[tokio::test]
    async fn no_snapshot_or_no_supervisor_proves_nothing() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        fs::remove_file(fixture.dir.path().join(crate::containment::GROUP_FILE)).unwrap();
        assert_eq!(confirmed(&record).await, Group::Unverifiable);
        let survivor = fixture.marked();
        fixture.wait_observable(&survivor).await;
        assert_eq!(occupied(confirmed(&record).await), [survivor.pid]);
        let spawning = Ownership {
            supervisor_pid: 0,
            ..record
        };
        assert_eq!(confirmed(&spawning).await, Group::Unverifiable);
    }

    #[tokio::test]
    async fn terminate_goes_from_term_to_kill_and_touches_only_the_group() {
        let mut fixture = Fixture::new();
        let record = fixture.record();
        let foreign = fixture.spawn(&mut sleeper());
        let marker = fixture.id.clone();
        fixture.spawn(
            Command::new("sh")
                .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
                .env(GROUP_MARKER_ENV, marker),
        );
        sleep(Duration::from_millis(50)).await;
        let group = terminate(&record, Duration::from_millis(200)).await;
        for child in fixture.children.iter_mut().skip(1) {
            let _ = child.try_wait();
        }
        assert_eq!(group, Group::Empty);
        assert_eq!(
            member(foreign.pid),
            foreign,
            "a foreign process stays alive"
        );
    }
}
