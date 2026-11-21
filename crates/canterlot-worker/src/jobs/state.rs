//! Каталог состояния попытки: `<data_dir>/jobs/<job_id>/<attempt>/`.
//!
//! Каждая попытка — каталог с маленькими файлами, которые пишутся атомарно (временный файл
//! и переименование). Состояние читается с диска на каждом шаге, поэтому перезапуск воркера
//! ничего не теряет: по каталогу видно, как далеко ушла попытка.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Подкоманда воркера, которая ведёт одну попытку.
pub const SHIM_ARG: &str = "job-shim";

pub const SPEC: &str = "spec.json";
/// Номер и время старта процесса-лидера сессии; пишется до запуска команды.
pub const PID: &str = "pid";
/// Попытка встала в строгую очередь аренды.
pub const LEASE_WAIT: &str = "lease_wait";
/// Команда пользователя запущена: миллисекунды UNIX.
pub const STARTED: &str = "started";
/// Код выхода команды и время выхода; пишется через временный файл и переименование.
pub const EXIT_CODE: &str = "exit_code";
/// Отмена, сохранённая раньше всего прочего.
pub const CANCEL: &str = "cancel";
/// Воркер решил, что эта попытка не должна запускаться (прерванный запуск, отмена до `pid`).
pub const ABANDONED: &str = "abandoned";
/// Итог, записанный воркером перед фактом `JobFinished`.
pub const RESULT: &str = "result.json";
/// Факты, уже записанные в журнал, и загрузка журнала.
pub const EMITTED: &str = "emitted.json";
pub const UPLOADED: &str = "uploaded";
pub const LOG_GZ: &str = "log.gz";
pub const SHIM_LOG: &str = "shim.log";
/// Оболочка после выдачи аренды не признала рабочий каталог: команда не запускалась.
pub const WORKDIR_REFUSED: &str = "workdir_refused";
/// Замок запуска: под ним оболочка делает последнюю проверку отмены и создаёт процесс команды,
/// а воркер сохраняет отмену. Так сохранённая отмена либо видна до запуска, либо приходит
/// после того, как процесс уже есть и его группу можно убрать.
pub const START_LOCK: &str = "start.lock";

pub fn jobs_root(data_dir: &Path) -> PathBuf {
    data_dir.join("jobs")
}

/// Идентификатор попытки превращается в путь только через разбор UUID: ни `..`, ни
/// разделителей в нём быть не может.
pub fn canonical_job_id(raw: &str) -> Option<String> {
    Uuid::parse_str(raw).ok().map(|id| id.to_string())
}

pub fn job_dir(data_dir: &Path, job_id: &str) -> PathBuf {
    jobs_root(data_dir).join(job_id)
}

pub fn attempt_dir(data_dir: &Path, job_id: &str, attempt: u32) -> PathBuf {
    job_dir(data_dir, job_id).join(attempt.to_string())
}

/// Отмена, пришедшая раньше команды запуска: каталога попытки ещё нет.
pub fn early_cancel_path(data_dir: &Path, job_id: &str, attempt: u32) -> PathBuf {
    job_dir(data_dir, job_id).join(format!("cancel.{attempt}"))
}

pub fn now_ms() -> u64 {
    crate::machine::now_ms()
}

/// Что нужно оболочке попытки; воркер пишет это один раз после захвата каталога.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Spec {
    pub job_id: String,
    pub attempt: u32,
    pub command_id: String,
    pub dispatch_attempt: u32,
    pub workdir: String,
    /// Корни, внутри которых должен лежать рабочий каталог; оболочка проверяет его заново
    /// после выдачи аренды. Пустой список (описание без поля) отказывает любому каталогу.
    #[serde(default)]
    pub allowed_roots: Vec<PathBuf>,
    pub shell: String,
    /// Полное окружение команды, уже собранное воркером: оболочка берёт только его.
    pub env: Vec<(String, String)>,
    pub timeout_seconds: u32,
    /// `none`, `build` или `gate`.
    pub lease: String,
    pub lease_socket: Option<PathBuf>,
    pub log_max_bytes: u64,
    pub label: String,
}

/// Итог попытки: пишется до факта, поэтому перезапуск воркера не теряет решение.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Finished {
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub log_bytes: u64,
    pub truncated: bool,
    pub finished_at_ms: u64,
}

/// Какие факты уже в журнале. Запись идёт после добавления факта: при сбое между ними факт
/// повторится, а хаб применяет факты монотонно.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Emitted {
    #[serde(default)]
    pub dispatched: bool,
    #[serde(default)]
    pub waiting_lease: bool,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub finished: bool,
}

/// Запись файла целиком: временный файл, `sync`, переименование.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    if let Some(parent) = path.parent() {
        // Каталог синхронизируется ради долговечности переименования; сбой не фатален.
        let _ = File::open(parent).and_then(|dir| dir.sync_all());
    }
    Ok(())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write_atomic(path, &serde_json::to_vec(value).map_err(io::Error::other)?)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Метка без содержимого, созданная атомарно: повтор — не ошибка.
pub fn touch(path: &Path) -> io::Result<()> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// `pid` и время старта лидера сессии.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leader {
    pub pid: u32,
    pub start_ticks: u64,
}

pub fn write_leader(dir: &Path, leader: Leader) -> io::Result<()> {
    write_atomic(
        &dir.join(PID),
        format!("{} {}\n", leader.pid, leader.start_ticks).as_bytes(),
    )
}

pub fn read_leader(dir: &Path) -> Option<Leader> {
    let text = fs::read_to_string(dir.join(PID)).ok()?;
    let mut parts = text.split_whitespace();
    Some(Leader {
        pid: parts.next()?.parse().ok()?,
        start_ticks: parts.next()?.parse().ok()?,
    })
}

/// Код выхода и время, когда он записан.
pub fn read_exit(dir: &Path) -> Option<(i32, u64)> {
    let text = fs::read_to_string(dir.join(EXIT_CODE)).ok()?;
    let mut parts = text.split_whitespace();
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

pub fn read_started(dir: &Path) -> Option<u64> {
    fs::read_to_string(dir.join(STARTED))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Замок запуска попытки; снимается при закрытии файла.
pub struct StartLock {
    _file: File,
}

impl StartLock {
    /// Ждёт замок: его держат только на время проверки и создания процесса.
    pub fn acquire(dir: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(START_LOCK))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        Ok(Self { _file: file })
    }
}

/// Сохраняет отмену под замком запуска: к моменту возврата оболочка либо уже создала процесс
/// (группу убирает наблюдатель), либо увидит отмену в последней проверке и ничего не создаст.
pub fn persist_cancel(attempt: &Path) -> io::Result<()> {
    let _lock = StartLock::acquire(attempt)?;
    touch(&attempt.join(CANCEL))
}

pub fn cancel_requested(data_dir: &Path, job_id: &str, attempt: u32) -> bool {
    attempt_dir(data_dir, job_id, attempt).join(CANCEL).exists()
        || early_cancel_path(data_dir, job_id, attempt).exists()
}

/// Процесс жив и это тот же самый процесс: совпадают pid и время старта, он не зомби.
pub fn is_same_process(leader: Leader) -> bool {
    crate::containment::start_ticks(leader.pid).is_ok_and(|ticks| ticks == leader.start_ticks)
        && !is_zombie(leader.pid)
}

fn is_zombie(pid: u32) -> bool {
    crate::containment::read_stat(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| {
            text.rfind(')')
                .and_then(|end| text[end + 1..].split_whitespace().next().map(str::to_owned))
        })
        .is_some_and(|state| state == "Z" || state == "X")
}

/// Участники сессии лидера: сам лидер и всё, что он порождал и не вышло из сессии.
/// Пока лидер жив, это его сессия; после смерти лидера номер сессии остаётся за её
/// участниками, и пока они живы, ядро не отдаёт его другому процессу.
pub fn session_members(leader: Leader) -> Vec<u32> {
    let Ok(table) = crate::containment::process_table() else {
        return Vec::new();
    };
    let leader_alive = is_same_process(leader);
    table
        .iter()
        .filter(|(pid, stat)| {
            !stat.zombie
                && stat.session == leader.pid
                // Другой процесс с тем же номером, но иным временем старта — не наш лидер.
                && (**pid != leader.pid || leader_alive)
        })
        .map(|(pid, _)| *pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_become_paths_only_through_uuid_parsing() {
        let id = Uuid::now_v7().to_string();
        assert_eq!(canonical_job_id(&id), Some(id.clone()));
        assert_eq!(
            canonical_job_id(&id.to_uppercase()),
            Some(id),
            "case is normalised"
        );
        for bad in ["", "..", "../x", "a/b", "not-a-uuid", "/etc"] {
            assert_eq!(canonical_job_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn atomic_write_leaves_no_temporary_and_replaces_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["file"]);
    }

    #[test]
    fn touch_is_idempotent_and_leader_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("mark")).unwrap();
        touch(&dir.path().join("mark")).unwrap();
        let leader = Leader {
            pid: 7,
            start_ticks: 99,
        };
        write_leader(dir.path(), leader).unwrap();
        assert_eq!(read_leader(dir.path()), Some(leader));
        fs::write(dir.path().join(EXIT_CODE), "3 1234\n").unwrap();
        assert_eq!(read_exit(dir.path()), Some((3, 1234)));
        assert_eq!(read_exit(&dir.path().join("none")), None);
    }

    #[test]
    fn identity_needs_both_pid_and_start_time() {
        let own = std::process::id();
        let ticks = crate::containment::start_ticks(own).unwrap();
        assert!(is_same_process(Leader {
            pid: own,
            start_ticks: ticks
        }));
        assert!(!is_same_process(Leader {
            pid: own,
            start_ticks: ticks + 1
        }));
    }
}
