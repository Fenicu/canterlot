//! Оболочка попытки: `canterlot-worker job-shim <каталог попытки>`.
//!
//! Воркер запускает её отдельным процессом; она создаёт свою сессию, поэтому не умирает
//! вместе с воркером, а вся её родня собрана в одной сессии и убирается разом. Порядок
//! важен для восстановления после сбоя:
//!
//! 1. `pid` с временем старта пишется **до** запуска команды: каталог без `pid` доказывает,
//!    что команда не запускалась;
//! 2. для аренды попытка встаёт в строгую очередь и до выдачи слота ничего не запускает;
//! 3. перед запуском проверяется, что воркер не передумал (отмена, брошенная попытка);
//! 4. команда идёт под `timeout`, вывод сливается в ограниченный журнал;
//! 5. `exit_code` пишется последним, через временный файл и переименование.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use super::{
    log::LogWriter,
    state::{
        self, ABANDONED, CANCEL, EXIT_CODE, LEASE_WAIT, Leader, STARTED, Spec, write_atomic,
        write_leader,
    },
};
use crate::lease::{Kind, client};

/// Код выхода оболочки, когда аренду не дали или команду нельзя было запустить.
const LEASE_REFUSED: i32 = 75;
const NOT_FOUND: i32 = 127;
/// Сколько после выхода команды ждём конец потока вывода: фоновый потомок мог унести
/// запись в канал и держать его открытым.
const DRAIN: Duration = Duration::from_secs(2);
/// Сколько `timeout` ждёт после TERM, прежде чем убить команду.
const KILL_AFTER: &str = "10s";

pub async fn run(dir: PathBuf) -> ExitCode {
    // Лидером сессии процесс обычно ещё не является; если уже стал, это не ошибка.
    let _ = rustix::process::setsid();
    let Some(spec) = state::read_json::<Spec>(&dir.join(state::SPEC)) else {
        return ExitCode::from(2);
    };
    let pid = std::process::id();
    let Ok(start_ticks) = crate::containment::start_ticks(pid) else {
        return ExitCode::from(2);
    };
    if write_leader(&dir, Leader { pid, start_ticks }).is_err() {
        return ExitCode::from(2);
    }
    if withdrawn(&dir, &spec) {
        return ExitCode::SUCCESS;
    }
    let lease = match lease_kind(&spec.lease) {
        None => None,
        Some(kind) => {
            let _ = state::touch(&dir.join(LEASE_WAIT));
            let Some(socket) = spec.lease_socket.clone() else {
                // Без сокета очереди строгая попытка не может ждать: запуск вне аренды запрещён.
                return finish(&dir, LEASE_REFUSED);
            };
            let dir_name = Path::new(&spec.workdir)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match client::acquire_strict(socket, kind, &spec.label, &dir_name).await {
                Ok(lease) => Some(lease),
                Err(_) => return finish(&dir, LEASE_REFUSED),
            }
        }
    };
    if withdrawn(&dir, &spec) {
        return ExitCode::SUCCESS;
    }
    let code = execute(&dir, &spec, lease.as_ref()).await;
    drop(lease);
    finish(&dir, code)
}

fn lease_kind(name: &str) -> Option<Kind> {
    match name {
        "build" => Some(Kind::Build),
        "gate" => Some(Kind::Gate),
        _ => None,
    }
}

/// Воркер отменил попытку или признал её брошенной: команда не запускается, `exit_code`
/// не пишется, итог ставит воркер.
fn withdrawn(dir: &Path, spec: &Spec) -> bool {
    dir.join(CANCEL).exists()
        || dir.join(ABANDONED).exists()
        || dir
            .parent()
            .is_some_and(|job| job.join(format!("cancel.{}", spec.attempt)).exists())
}

fn finish(dir: &Path, code: i32) -> ExitCode {
    let _ = write_atomic(
        &dir.join(EXIT_CODE),
        format!("{code} {}\n", state::now_ms()).as_bytes(),
    );
    ExitCode::SUCCESS
}

async fn execute(dir: &Path, spec: &Spec, lease: Option<&client::StrictLease>) -> i32 {
    let Ok((reader, writer)) = std::io::pipe() else {
        return NOT_FOUND;
    };
    let Ok(error_writer) = writer.try_clone() else {
        return NOT_FOUND;
    };
    let mut command = tokio::process::Command::new("timeout");
    command
        .arg(format!("--kill-after={KILL_AFTER}"))
        .arg(format!("{}s", spec.timeout_seconds.max(1)))
        .arg("bash")
        .arg("-lc")
        .arg(&spec.shell)
        .current_dir(&spec.workdir)
        .env_clear()
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::null())
        .stdout(writer)
        .stderr(error_writer);
    if let Some(lease) = lease {
        command.envs(lease.env());
    }
    // `timeout` ищется по PATH задачи, а не воркера.
    if let Some((_, path)) = spec.env.iter().find(|(name, _)| name == "PATH") {
        command.env("PATH", path);
    }
    let log = LogWriter::create(dir, spec.log_max_bytes);
    let _ = write_atomic(&dir.join(STARTED), state::now_ms().to_string().as_bytes());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return NOT_FOUND,
    };
    // Свои копии концов канала закрываются вместе с `command`: иначе конца потока не будет.
    drop(command);
    let pump = tokio::task::spawn_blocking(move || pump(reader, log));
    let status = child.wait().await;
    let _ = tokio::time::timeout(DRAIN, pump).await;
    match status {
        Ok(status) => {
            use std::os::unix::process::ExitStatusExt;
            status
                .code()
                .or_else(|| status.signal().map(|signal| 128 + signal))
                .unwrap_or(1)
        }
        Err(_) => 1,
    }
}

/// Читает общий канал stdout и stderr в журнал. Ошибка записи (диск) не останавливает
/// чтение: иначе команда зависла бы на заполненном канале.
fn pump(mut reader: std::io::PipeReader, log: std::io::Result<LogWriter>) {
    let mut log = log.ok();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if let Some(writer) = log.as_mut()
                    && writer.write(&buffer[..read]).is_err()
                {
                    log = None;
                }
            }
        }
    }
}
