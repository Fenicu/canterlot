//! Предзапусковая проверка расписания: `bash -c <команда>` в изолированной группе.
//!
//! Команда приходит от владельца, поэтому воркер не доверяет ей: каталог
//! канонизуется и ограничен разрешёнными корнями, наследуется только минимальное
//! окружение, оба потока читаются с ограничением по хвосту, а вся группа процессов
//! убивается и убирается по таймауту или при завершении проверки.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use canterlot_proto::worker::v1::{
    PrecheckError, PrecheckFinished, PrecheckResult, SchedulePrecheck, precheck_result,
};
use rustix::process::Signal;
use tokio::{io::AsyncReadExt, process::Command, sync::oneshot};

use crate::containment::{Containment, TreeContainment};

/// Сколько последних байтов вывода сохраняется в журнале запуска.
pub const OUTPUT_TAIL_BYTES: usize = 4096;
const TERMINATE_GRACE: Duration = Duration::from_millis(200);

/// Вся группа убивается, даже если future проверки брошена.
struct GroupGuard {
    pid: u32,
    armed: Arc<AtomicBool>,
}
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            let _ = rustix::process::kill_process_group(
                rustix::process::Pid::from_raw(self.pid as i32).expect("valid child pid"),
                Signal::KILL,
            );
        }
    }
}

pub fn error(reason: impl Into<String>) -> PrecheckResult {
    PrecheckResult {
        attempt_id: String::new(),
        outcome: Some(precheck_result::Outcome::Error(PrecheckError {
            reason: reason.into(),
        })),
    }
}

fn finished(attempt_id: &str, exit_code: i32, tail: Vec<u8>, timed_out: bool) -> PrecheckResult {
    PrecheckResult {
        attempt_id: attempt_id.to_owned(),
        outcome: Some(precheck_result::Outcome::Finished(PrecheckFinished {
            exit_code,
            output_tail: String::from_utf8_lossy(&tail).into_owned(),
            timed_out,
        })),
    }
}

/// Канонический каталог внутри разрешённых корней.
async fn canonical_workdir(workdir: &str, roots: Vec<PathBuf>) -> Result<PathBuf, &'static str> {
    let workdir = workdir.to_owned();
    tokio::task::spawn_blocking(move || {
        if workdir.is_empty() || !workdir.starts_with('/') {
            return Err("invalid_workdir");
        }
        let path = std::fs::canonicalize(Path::new(&workdir)).map_err(|_| "workdir_missing")?;
        let inside = roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .any(|root| path.starts_with(root));
        if inside {
            Ok(path)
        } else {
            Err("workdir_not_allowed")
        }
    })
    .await
    .map_err(|_| "workdir_probe_failed")?
}

async fn drain(mut stream: impl AsyncReadExt + Unpin + Send + 'static, ring: Arc<Mutex<Vec<u8>>>) {
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut ring = ring.lock().expect("output ring poisoned");
                ring.extend_from_slice(&buf[..n]);
                if ring.len() > OUTPUT_TAIL_BYTES {
                    let start = ring.len() - OUTPUT_TAIL_BYTES;
                    ring.drain(..start);
                }
            }
        }
    }
}

/// Запуск проверки с подменой таймера: `timeout_signal` нужен тестам, чтобы
/// проверить путь таймаута без реального ожидания времени.
pub async fn run_with(
    request: SchedulePrecheck,
    roots: Vec<PathBuf>,
    timeout: Duration,
    timeout_signal: Option<oneshot::Receiver<()>>,
) -> PrecheckResult {
    let attempt_id = request.attempt_id.clone();
    let workdir = match canonical_workdir(&request.workdir, roots).await {
        Ok(path) => path,
        Err(reason) => {
            let mut result = error(reason);
            result.attempt_id = attempt_id;
            return result;
        }
    };
    let containment = match TreeContainment::new() {
        Ok(containment) => containment,
        Err(_) => {
            let mut result = error("containment_unavailable");
            result.attempt_id = attempt_id;
            return result;
        }
    };
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&request.command)
        .current_dir(workdir)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env(
            "HOME",
            std::env::var_os("HOME").unwrap_or_else(|| "/".into()),
        )
        .env(
            "LANG",
            std::env::var_os("LANG").unwrap_or_else(|| "C.UTF-8".into()),
        )
        .env(
            "USER",
            std::env::var_os("USER").unwrap_or_else(|| "worker".into()),
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    containment.prepare_command(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let mut result = error("spawn_failed");
            result.attempt_id = attempt_id;
            return result;
        }
    };
    let pid = child.id().expect("spawned child has a pid");
    let armed = Arc::new(AtomicBool::new(true));
    let guard = GroupGuard {
        pid,
        armed: armed.clone(),
    };
    let ring: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let drains = [
        child.stdout.take().map(|out| {
            let ring = ring.clone();
            tokio::spawn(drain(out, ring))
        }),
        child.stderr.take().map(|err| {
            let ring = ring.clone();
            tokio::spawn(drain(err, ring))
        }),
    ];
    let wait = child.wait();
    let outcome = if let Some(signal) = timeout_signal {
        tokio::select! {
            status = wait => Ok(status),
            _ = signal => Err(()),
        }
    } else {
        match tokio::time::timeout(timeout, wait).await {
            Ok(status) => Ok(status),
            Err(_) => Err(()),
        }
    };
    let timed_out = outcome.is_err();
    if timed_out {
        let _ = containment.terminate(TERMINATE_GRACE).await;
    }
    let status = match outcome {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => {
            let mut result = error("wait_failed");
            result.attempt_id = attempt_id;
            return result;
        }
        Err(()) => None,
    };
    // Потоки closing по EOF после выхода группы: дать дренажам дочитать хвост,
    // иначе последняя запись может ещё не попасть в кольцо.
    for task in drains.into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }
    let tail = ring.lock().expect("output ring poisoned").clone();
    drop(guard);
    let _ = child.start_kill();
    let _ = child.wait().await;
    let _ = containment.reap_orphans();
    let code = status
        .and_then(|status| status.code())
        .unwrap_or(if timed_out { -1 } else { 1 });
    finished(&attempt_id, code, tail, timed_out)
}

pub async fn run(request: SchedulePrecheck, roots: Vec<PathBuf>) -> PrecheckResult {
    let timeout = Duration::from_secs(request.timeout_seconds.clamp(1, 300) as u64);
    run_with(request, roots, timeout, None).await
}

/// Записать хвост вывода вRing: отдельная функция для тестов границы UTF-8.
pub fn tail_bytes(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(OUTPUT_TAIL_BYTES);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}
