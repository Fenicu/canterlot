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

/// Сколько последних байтов вывода сохраняется в журнале запуска.
pub const OUTPUT_TAIL_BYTES: usize = 4096;
const TERMINATE_GRACE: Duration = Duration::from_millis(200);

/// Область предпроверки: только её дерево (лидер + его группа).
/// В отличие от `TreeContainment::new()`, привязанного к PID воркера,
/// здесь членство — ровно группа запущенного `bash`, чужие процессы
/// воркера (живые CLI, пробы, другие предпроверки) не затрагиваются.
struct ScopedGroup {
    pgid: u32,
    leader_start_ticks: u64,
    armed: Arc<AtomicBool>,
}

impl ScopedGroup {
    fn pid_reused(&self) -> bool {
        if self.leader_start_ticks == 0 {
            return false;
        }
        match crate::containment::start_ticks(self.pgid) {
            Ok(ticks) => ticks != self.leader_start_ticks,
            Err(_) => false,
        }
    }

    fn signal_group(&self, signal: Signal) {
        if self.pid_reused() {
            return;
        }
        let Some(pid) = i32::try_from(self.pgid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return;
        };
        let _ = rustix::process::kill_process_group(pid, signal);
    }

    async fn terminate(&self, grace: Duration) {
        self.signal_group(Signal::TERM);
        tokio::time::sleep(grace).await;
        self.signal_group(Signal::KILL);
    }

    async fn kill_and_reap(&self) {
        self.signal_group(Signal::KILL);
        wait_group_gone_and_reap(self.pgid).await;
    }
}

/// Участники группы предпроверки по `pgrp`: лидер уже может быть перебран, а
/// фоновые потомки ещё сидят с тем же `pgid`.
struct GroupMember {
    pid: u32,
    parent: u32,
    zombie: bool,
}

fn group_members(pgid: u32) -> Vec<GroupMember> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(end) = text.rfind(')') else {
            continue;
        };
        let fields: Vec<&str> = text[end + 1..].split_whitespace().collect();
        // Поля после `)`: состояние, ppid, pgrp.
        if fields.get(2).and_then(|s| s.parse::<u32>().ok()) != Some(pgid) {
            continue;
        }
        out.push(GroupMember {
            pid,
            parent: fields
                .get(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0),
            zombie: fields
                .first()
                .is_some_and(|state| *state == "Z" || *state == "X"),
        });
    }
    out
}

/// Ждёт, пока в группе не останется живых, и снимает статус усыновлённых
/// потомков: когда этот процесс — ближайший subreaper, убитые потомки
/// `bash` остаются его зомби, и никто, кроме него, их не заберёт. Лидера
/// не трогаем: его статус ждёт владелец `Child`.
async fn wait_group_gone_and_reap(pgid: u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let me = std::process::id();
    loop {
        let mut live = false;
        for member in group_members(pgid) {
            if !member.zombie {
                live = true;
            } else if member.parent == me
                && member.pid != pgid
                && let Some(pid) = i32::try_from(member.pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::waitid(
                    rustix::process::WaitId::Pid(pid),
                    rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOHANG,
                );
            }
        }
        if !live || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Дренажи вывода убиваются и при обрыве future: брошенный `JoinHandle`
/// иначе продолжал бы жить в фоне, держа трубы открытыми.
struct DrainSet {
    tasks: Option<Vec<tokio::task::JoinHandle<()>>>,
}

impl DrainSet {
    /// Дочитать дренажи с отсрочкой, не вынимая дескрипторы из набора:
    /// отмена future посреди ожидания роняет набор, а `Drop` отменяет задачи.
    async fn join(&mut self, per_task: Duration) {
        if let Some(tasks) = self.tasks.as_mut() {
            for task in tasks.iter_mut() {
                let _ = tokio::time::timeout(per_task, &mut *task).await;
                if !task.is_finished() {
                    task.abort();
                }
            }
        }
        self.tasks = None;
    }
}

impl Drop for DrainSet {
    fn drop(&mut self) {
        if let Some(tasks) = self.tasks.as_mut() {
            for task in tasks {
                task.abort();
            }
        }
    }
}

/// Вся группа убивается, даже если future проверки брошена.
struct GroupGuard {
    scope: Option<ScopedGroup>,
}
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(scope) = self.scope.take()
            && scope.armed.swap(false, Ordering::AcqRel)
        {
            scope.signal_group(Signal::KILL);
            // Отмена future: синхронно ждать нельзя, но усыновлённых потомков
            // всё равно надо снять, поэтому уборку доводит отдельная задача.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let pgid = scope.pgid;
                runtime.spawn(async move { wait_group_gone_and_reap(pgid).await });
            }
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
            output_tail: truncate_tail_to_string(&tail),
            timed_out,
        })),
    }
}

/// Канонический каталог внутри разрешённых корней.
async fn canonical_workdir(workdir: &str, roots: &[PathBuf]) -> Result<PathBuf, &'static str> {
    let workdir = workdir.to_owned();
    let roots = roots.to_owned();
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
    let workdir = match canonical_workdir(&request.workdir, &roots).await {
        Ok(path) => path,
        Err(reason) => {
            let mut result = error(reason);
            result.attempt_id = attempt_id;
            return result;
        }
    };
    // Прибиваем проверенный каталог дескриптором: запуск идёт через
    // `/proc/self/fd/N`, поэтому подмена пути между проверкой и запуском
    // не уводит исполнение наружу. Дескриптор живёт до конца spawn.
    let dir_fd = match super::nofollow::open_dir(&workdir) {
        Ok(fd) => fd,
        Err(_) => {
            let mut result = error("workdir_not_allowed");
            result.attempt_id = attempt_id;
            return result;
        }
    };
    let fd_cwd = {
        use std::os::fd::AsRawFd;
        let raw = dir_fd.as_raw_fd();
        let target =
            std::fs::read_link(format!("/proc/self/fd/{raw}")).map_err(|_| "workdir_not_allowed");
        let target = match target {
            Ok(target) => target,
            Err(reason) => {
                let mut result = error(reason);
                result.attempt_id = attempt_id;
                return result;
            }
        };
        let inside = roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .any(|root| target.starts_with(&root));
        if !inside {
            let mut result = error("workdir_not_allowed");
            result.attempt_id = attempt_id;
            return result;
        }
        format!("/proc/self/fd/{raw}")
    };
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&request.command)
        .current_dir(fd_cwd)
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
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let mut result = error("spawn_failed");
            result.attempt_id = attempt_id;
            return result;
        }
    };
    let Some(pid) = child.id() else {
        let mut result = error("spawn_failed");
        result.attempt_id = attempt_id;
        return result;
    };
    let leader_start_ticks = crate::containment::start_ticks(pid).unwrap_or(0);
    let armed = Arc::new(AtomicBool::new(true));
    let mut guard = GroupGuard {
        scope: Some(ScopedGroup {
            pgid: pid,
            leader_start_ticks,
            armed: armed.clone(),
        }),
    };
    let scope = ScopedGroup {
        pgid: pid,
        leader_start_ticks,
        armed: armed.clone(),
    };
    let ring: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let mut drain_set = DrainSet {
        tasks: Some(
            [
                child.stdout.take().map(|out| {
                    let ring = ring.clone();
                    tokio::spawn(drain(out, ring))
                }),
                child.stderr.take().map(|err| {
                    let ring = ring.clone();
                    tokio::spawn(drain(err, ring))
                }),
            ]
            .into_iter()
            .flatten()
            .collect(),
        ),
    };
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
        scope.terminate(TERMINATE_GRACE).await;
    }
    // Вся группа прибивается на каждом пути выхода: вышедший `bash` уже
    // перебран, а его фоновые потомки ещё сидят с тем же `pgid`.
    scope.kill_and_reap().await;
    let status = match outcome {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => {
            // Тот же путь уборки, что и обычно: прибить группу, дочитать
            // дренажи с отменой, снять охрану, переждать ребёнка.
            drain_set.join(Duration::from_secs(1)).await;
            guard.scope = None;
            armed.store(false, Ordering::Release);
            let _ = child.start_kill();
            let _ = child.wait().await;
            let mut result = error("wait_failed");
            result.attempt_id = attempt_id;
            return result;
        }
        Err(()) => None,
    };
    // Потоки closing по EOF после выхода группы: дать дренажам дочитать хвост,
    // иначе последняя запись может ещё не попасть в кольцо. Отмена вместо
    // отсоединения: брошенный `JoinHandle` продолжал бы жить в фоне.
    drain_set.join(Duration::from_secs(1)).await;
    let tail = truncate_tail(&ring.lock().expect("output ring poisoned"));
    guard.scope = None;
    armed.store(false, Ordering::Release);
    let _ = child.start_kill();
    let _ = child.wait().await;
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
    truncate_tail_to_string(bytes)
}

/// Хвост не шире 4 КиБ в кодировке: режем по границе символа и держим
/// закодированный ответ в бюджете (`U+FFFD` расширяет потерю).
fn truncate_tail(bytes: &[u8]) -> Vec<u8> {
    let start = bytes.len().saturating_sub(OUTPUT_TAIL_BYTES);
    let mut start = start;
    while start < bytes.len() && bytes[start] & 0xC0 == 0x80 {
        start += 1;
    }
    bytes[start..].to_vec()
}

fn truncate_tail_to_string(bytes: &[u8]) -> String {
    let tail = truncate_tail(bytes);
    let mut text = String::from_utf8_lossy(&tail).into_owned();
    while text.len() > OUTPUT_TAIL_BYTES {
        // Убираем по первому символу, пока закодированный вид в бюджете.
        let next = text.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        text.drain(..next);
    }
    text
}
