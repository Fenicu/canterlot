use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rustix::process::{self, Pid, Signal, WaitOptions};
use tokio::process::Command;

/// Метка группы в окружении CLI: её наследуют потомки, ушедшие в свои сессии, и она
/// переживает смерть сопровождения, в отличие от дерева родителей.
pub const GROUP_MARKER_ENV: &str = "CANTERLOT_EXECUTION_GROUP";
/// Последний снимок участников в каталоге исполнения; его наличие означает, что сопровождение
/// ставит метку. Старый релиз файла не пишет, и пустоту его группы доказать нельзя.
pub const GROUP_FILE: &str = "group.json";
/// Сокет аренды тяжёлых задач и бинарник воркера для исполнений: адаптеры чистят или
/// перечисляют окружение CLI, поэтому их, как и метку, ставит `prepare_command`.
pub const LEASE_SOCKET_ENV: &str = "CANTERLOT_LEASE_SOCKET";
pub const WORKER_BIN_ENV: &str = "CANTERLOT_WORKER_BIN";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Member {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Debug)]
pub enum Termination {
    Empty,
    Survivors(Vec<Member>),
}

#[derive(Debug, thiserror::Error)]
pub enum ContainmentError {
    #[error("process inspection failed")]
    Io(#[from] io::Error),
    #[error("process operation failed")]
    Process(#[from] rustix::io::Errno),
    #[error("invalid process stat")]
    InvalidStat,
    #[error("supervisor identity changed")]
    SupervisorChanged,
}

#[async_trait::async_trait]
pub trait Containment: Send + Sync {
    fn kind(&self) -> &'static str;
    fn prepare_command(&self, command: &mut Command);
    fn members(&self) -> Result<Vec<Member>, ContainmentError>;
    /// Живые участники без побочных действий: `members` закрепляет CLI и забирает зомби,
    /// поэтому фоновый опрос идёт сюда.
    fn observed(&self) -> Result<Vec<Member>, ContainmentError> {
        Ok(Vec::new())
    }
    fn populated(&self) -> Result<bool, ContainmentError>;
    fn signal_member(&self, member: &Member, signal: Signal) -> Result<bool, ContainmentError>;
    async fn terminate(&self, grace: Duration) -> Result<Termination, ContainmentError>;
    /// Прямой ребёнок как участник без опроса всего дерева: быстро вышедший CLI
    /// опрос застаёт уже зомби и исключает из участников, а чужая уборка статуса
    /// отнимает выход у `Child`. По умолчанию — поиск в опросе (старое поведение).
    fn direct_member(&self, pid: u32) -> Result<Member, ContainmentError> {
        self.members()?
            .into_iter()
            .find(|m| m.pid == pid)
            .ok_or(ContainmentError::InvalidStat)
    }
    /// Запомнить прямой CLI: его статус выхода читает `Child`, чужая уборка его отняла бы.
    fn register_cli(&self, _member: &Member) {}
    /// Убрать зомби усыновлённых процессов, не трогая детей самого супервизора.
    fn reap_orphans(&self) -> Result<(), ContainmentError> {
        Ok(())
    }
}

/// Одно исполнение на процесс; первый members после spawn фиксирует прямой CLI.
pub struct TreeContainment {
    supervisor: Member,
    cli: Mutex<Option<Member>>,
    prepared: AtomicBool,
    marker: Option<String>,
    /// Переменные аренды из окружения сопровождения (их ставит воркер при запуске).
    lease_env: Vec<(&'static str, std::ffi::OsString)>,
}

pub(crate) struct Stat {
    pub member: Member,
    pub parent: u32,
    group: u32,
    pub session: u32,
    pub zombie: bool,
}

/// Текст `stat` процесса. Имя в нём ядро режет по байтам, поэтому это не обязательно UTF-8.
pub(crate) fn read_stat(path: impl AsRef<std::path::Path>) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&fs::read(path)?).into_owned())
}

fn stat(pid: u32) -> Result<Stat, ContainmentError> {
    let text = read_stat(format!("/proc/{pid}/stat"))?;
    parse_stat(pid, &text)
}

fn parse_stat(pid: u32, text: &str) -> Result<Stat, ContainmentError> {
    // Завершающийся процесс иногда отдаёт пустой stat: считаем его исчезнувшим.
    if text.trim().is_empty() {
        return Err(io::Error::from(io::ErrorKind::NotFound).into());
    }
    let end = text.rfind(')').ok_or(ContainmentError::InvalidStat)?;
    let fields: Vec<_> = text[end + 1..].split_whitespace().collect();
    // После удаления pid ядро может отдать stat с pgrp = -1.
    if fields.get(2) == Some(&"-1") {
        return Err(io::Error::from(io::ErrorKind::NotFound).into());
    }
    let number = |index: usize| -> Result<u64, ContainmentError> {
        fields
            .get(index)
            .ok_or(ContainmentError::InvalidStat)?
            .parse()
            .map_err(|_| ContainmentError::InvalidStat)
    };
    Ok(Stat {
        member: Member {
            pid,
            start_ticks: number(19)?,
        },
        parent: u32::try_from(number(1)?).map_err(|_| ContainmentError::InvalidStat)?,
        group: u32::try_from(number(2)?).map_err(|_| ContainmentError::InvalidStat)?,
        session: u32::try_from(number(3)?).map_err(|_| ContainmentError::InvalidStat)?,
        zombie: matches!(fields.first(), Some(&"Z") | Some(&"X")),
    })
}

pub fn start_ticks(pid: u32) -> Result<u64, ContainmentError> {
    Ok(stat(pid)?.member.start_ticks)
}

/// Значение метки группы в окружении процесса. Время старта сверяется после чтения
/// `environ`: pid мог смениться между опознанием и чтением, тогда ответ — `None`.
/// Закрытый от чтения или исчезнувший процесс метки не показывает.
pub fn read_marker(pid: u32, start_ticks: u64) -> Option<String> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let prefix = format!("{GROUP_MARKER_ENV}=");
    let value = bytes
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
        .map(|value| String::from_utf8_lossy(value).into_owned());
    match stat(pid) {
        Ok(current) if current.member.start_ticks == start_ticks && !current.zombie => value,
        _ => None,
    }
}

/// Все процессы машины; исчезнувшие по ходу чтения пропускаются.
pub(crate) fn process_table() -> Result<HashMap<u32, Stat>, ContainmentError> {
    let mut all = HashMap::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        match stat(pid) {
            Ok(info) => {
                all.insert(pid, info);
            }
            Err(error) if disappeared(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(all)
}

/// Снимок заменяется целиком: смерть сопровождения посреди записи оставит прошлый.
pub fn write_group_file(exec_dir: &Path, members: &[Member]) -> io::Result<()> {
    let pairs: Vec<(u32, u64)> = members.iter().map(|m| (m.pid, m.start_ticks)).collect();
    let temporary = exec_dir.join(format!("{GROUP_FILE}.tmp"));
    fs::write(&temporary, serde_json::to_vec(&pairs)?)?;
    fs::rename(temporary, exec_dir.join(GROUP_FILE))
}

/// `None`, если сопровождение файла не писало.
pub(crate) fn read_group_file(exec_dir: &Path) -> io::Result<Option<Vec<Member>>> {
    let bytes = match fs::read(exec_dir.join(GROUP_FILE)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let pairs: Vec<(u32, u64)> = serde_json::from_slice(&bytes)?;
    Ok(Some(
        pairs
            .into_iter()
            .map(|(pid, start_ticks)| Member { pid, start_ticks })
            .collect(),
    ))
}

fn disappeared(error: &ContainmentError) -> bool {
    matches!(error, ContainmentError::Io(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()))
}

impl TreeContainment {
    pub fn new() -> Result<Self, ContainmentError> {
        Self::build(None)
    }

    /// Группа с меткой исполнения: её участники опознаются и после смерти сопровождения.
    pub fn marked(marker: String) -> Result<Self, ContainmentError> {
        Self::build(Some(marker))
    }

    fn build(marker: Option<String>) -> Result<Self, ContainmentError> {
        process::set_child_subreaper(Pid::from_raw(1))?;
        tracing::info!(containment = "tree", "process containment initialized");
        Ok(Self {
            supervisor: stat(std::process::id())?.member,
            cli: Mutex::new(None),
            prepared: AtomicBool::new(false),
            lease_env: if marker.is_some() {
                [LEASE_SOCKET_ENV, WORKER_BIN_ENV]
                    .into_iter()
                    .filter_map(|name| Some((name, std::env::var_os(name)?)))
                    .collect()
            } else {
                Vec::new()
            },
            marker,
        })
    }

    fn snapshot(&self) -> Result<HashMap<u32, Stat>, ContainmentError> {
        if stat(self.supervisor.pid)?.member != self.supervisor {
            return Err(ContainmentError::SupervisorChanged);
        }
        process_table()
    }

    /// Забрать статус зомби-ребёнка, если это всё ещё тот же процесс.
    fn reap(&self, member: &Member) -> Result<(), ContainmentError> {
        if let Some(pid) = i32::try_from(member.pid).ok().and_then(Pid::from_raw)
            && stat(member.pid).is_ok_and(|current| {
                current.member == *member && current.parent == self.supervisor.pid && current.zombie
            })
        {
            match process::waitpid(Some(pid), WaitOptions::NOHANG) {
                Ok(_) | Err(rustix::io::Errno::CHILD) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn belongs(&self, info: &Stat, all: &HashMap<u32, Stat>) -> bool {
        let mut current = info;
        let mut seen = HashSet::new();
        while seen.insert(current.member.pid) {
            let Some(parent) = all.get(&current.parent) else {
                return false;
            };
            if parent.member.start_ticks > current.member.start_ticks {
                return false;
            }
            if parent.member == self.supervisor {
                return info.member != self.supervisor;
            }
            current = parent;
        }
        false
    }

    pub fn signal_member(&self, member: &Member, signal: Signal) -> Result<bool, ContainmentError> {
        let all = self.snapshot()?;
        let Some(info) = all.get(&member.pid) else {
            return Ok(false);
        };
        if info.member != *member || info.zombie || !self.belongs(info, &all) {
            return Ok(false);
        }
        let current = match stat(member.pid) {
            Ok(info) => info,
            Err(error) if disappeared(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        if current.member != *member || current.parent != info.parent || current.zombie {
            return Ok(false);
        }
        let Some(pid) = i32::try_from(member.pid).ok().and_then(Pid::from_raw) else {
            return Ok(false);
        };
        match process::kill_process(pid, signal) {
            Ok(()) => Ok(true),
            Err(rustix::io::Errno::SRCH) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait::async_trait]
impl Containment for TreeContainment {
    fn kind(&self) -> &'static str {
        "tree"
    }

    fn prepare_command(&self, command: &mut Command) {
        let mut cli = self.cli.lock().expect("containment mutex poisoned");
        if cli.as_ref().is_some_and(|member| match stat(member.pid) {
            Ok(current) => current.member != *member,
            Err(error) => disappeared(&error),
        }) {
            *cli = None;
        }
        self.prepared.store(true, Ordering::Relaxed);
        command.process_group(0);
        if let Some(marker) = &self.marker {
            command.env(GROUP_MARKER_ENV, marker);
        }
        for (name, value) in &self.lease_env {
            command.env(name, value);
        }
    }

    fn members(&self) -> Result<Vec<Member>, ContainmentError> {
        let all = self.snapshot()?;
        let mut cli = self.cli.lock().expect("containment mutex poisoned");
        if cli.is_none() && self.prepared.load(Ordering::Relaxed) {
            *cli = all
                .values()
                .filter(|s| s.parent == self.supervisor.pid && s.group == s.member.pid)
                .min_by_key(|s| (s.member.start_ticks, s.member.pid))
                .map(|s| s.member.clone());
        }
        let mut members = Vec::new();
        for info in all.values().filter(|info| self.belongs(info, &all)) {
            if !info.zombie {
                members.push(info.member.clone());
                continue;
            }
            // Прямой CLI (лидер группы) остаётся за tokio::process::Child.
            if info.parent == self.supervisor.pid && cli.as_ref() != Some(&info.member) {
                self.reap(&info.member)?;
            }
        }
        members.sort_by_key(|m| m.pid);
        Ok(members)
    }

    fn observed(&self) -> Result<Vec<Member>, ContainmentError> {
        let all = self.snapshot()?;
        Ok(all
            .values()
            .filter(|info| !info.zombie && self.belongs(info, &all))
            .map(|info| info.member.clone())
            .collect())
    }

    /// Участник по pid прямого ребёнка без опроса дерева: зомби тоже подходит,
    /// его статус читает владеющий `Child`. Не наш или не лидер группы — `InvalidStat`.
    fn direct_member(&self, pid: u32) -> Result<Member, ContainmentError> {
        if stat(self.supervisor.pid)?.member != self.supervisor {
            return Err(ContainmentError::SupervisorChanged);
        }
        let info = stat(pid)?;
        if info.parent != self.supervisor.pid || info.group != pid {
            return Err(ContainmentError::InvalidStat);
        }
        Ok(info.member)
    }

    fn signal_member(&self, member: &Member, signal: Signal) -> Result<bool, ContainmentError> {
        TreeContainment::signal_member(self, member, signal)
    }

    fn populated(&self) -> Result<bool, ContainmentError> {
        Ok(!self.members()?.is_empty())
    }

    fn register_cli(&self, member: &Member) {
        *self.cli.lock().expect("containment mutex poisoned") = Some(member.clone());
    }

    fn reap_orphans(&self) -> Result<(), ContainmentError> {
        let all = self.snapshot()?;
        let own_group = all.get(&self.supervisor.pid).map(|info| info.group);
        let cli = self.cli.lock().expect("containment mutex poisoned").clone();
        // Проверка версии CLI живёт в группе супервизора, её статус читает `Command`.
        for info in all.values().filter(|info| {
            info.zombie
                && info.parent == self.supervisor.pid
                && Some(info.group) != own_group
                && cli.as_ref() != Some(&info.member)
        }) {
            self.reap(&info.member)?;
        }
        Ok(())
    }

    async fn terminate(&self, grace: Duration) -> Result<Termination, ContainmentError> {
        // Сигнал один на процесс: повторный SIGTERM многие программы считают
        // требованием выйти немедленно, а новые участники получают свой.
        let mut signalled = HashSet::new();
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let members = self.members()?;
            if members.is_empty() {
                return Ok(Termination::Empty);
            }
            for member in &members {
                if !signalled.contains(member) && self.signal_member(member, Signal::TERM)? {
                    signalled.insert(member.clone());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(
                Duration::from_millis(10)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
        let mut signalled = HashSet::new();
        let kill_deadline = tokio::time::Instant::now() + grace;
        loop {
            let members = self.members()?;
            if members.is_empty() {
                return Ok(Termination::Empty);
            }
            for member in &members {
                if !signalled.contains(member) && self.signal_member(member, Signal::KILL)? {
                    signalled.insert(member.clone());
                }
            }
            if tokio::time::Instant::now() >= kill_deadline {
                return Ok(Termination::Survivors(members));
            }
            tokio::time::sleep(
                Duration::from_millis(10)
                    .min(kill_deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_without_process_group_is_a_disappeared_process() {
        let error = parse_stat(42, "42 (worker) R 0 -1 -1 0 0 0 0 0 0 0 0 0 0 0 0 1 0 123")
            .err()
            .unwrap();
        assert!(disappeared(&error));
        assert!(matches!(
            parse_stat(42, "invalid"),
            Err(ContainmentError::InvalidStat)
        ));
    }

    #[test]
    fn process_name_cut_inside_a_character_does_not_fail_inspection() {
        // Ядро режет comm до 15 байт, здесь это середина кириллической буквы.
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("проверка-сборки");
        std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
        let mut child = std::process::Command::new(&link).arg("30").spawn().unwrap();
        let inspected = stat(child.id()).map(|info| info.member.pid);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(inspected.ok(), Some(child.id()));
    }

    #[test]
    fn disappearing_process_is_not_inspection_failure() {
        assert!(disappeared(
            &io::Error::from(io::ErrorKind::NotFound).into()
        ));
        assert!(disappeared(
            &io::Error::from_raw_os_error(rustix::io::Errno::SRCH.raw_os_error()).into()
        ));
        assert!(!disappeared(
            &io::Error::from(io::ErrorKind::PermissionDenied).into()
        ));
    }
}
