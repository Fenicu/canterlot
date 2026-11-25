//! Задачи на месте (`jobs_inplace_v1`): команда оболочки, которую хаб просит выполнить
//! в каталоге этой машины. Состояние живёт на диске, поэтому перезапуск воркера задачу не
//! трогает: воркер находит её по каталогу попытки и номеру процесса.
//!
//! Воркер занимает каталог попытки атомарным `mkdir`, пишет `spec.json` и запускает
//! оболочку `job-shim` ([`shim`]). Дальше за попыткой следит задача-наблюдатель: она читает
//! файлы каталога, пишет факты в журнал управления, шлёт куски живого вывода и в конце
//! записывает итог. Всё, что наблюдатель решает, он решает по файлам, а не по памяти, поэтому
//! после перезапуска новый наблюдатель продолжает с того же места.

pub mod log;
pub mod shim;
pub mod state;
pub mod workdir;

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use canterlot_proto::{
    limits::MAX_MEDIA_BYTES,
    worker::v1::{
        Command, JobDispatched, JobFinished, JobLease, JobLogChunk, JobOutcome, JobPhase,
        JobSnapshot, JobStarted, JobWaitingLease, JobsReport, MediaHeader, StartInPlaceJob,
        command::Kind, record_body::Body,
    },
};
use rustix::process::{Pid, Signal};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::config::JobsConfig;
use log::LogView;
use state::{
    ABANDONED, Emitted, Finished, LEASE_WAIT, Leader, Spec, UPLOADED, WORKDIR_REFUSED, attempt_dir,
    canonical_job_id, early_cancel_path, job_dir, jobs_root, now_ms, read_exit, read_json,
    read_leader, read_started, touch, write_json,
};

/// Последние байты вывода, которые уходят в факт итога.
const TAIL_BYTES: usize = 32 * 1024;
/// Один кусок живого вывода.
const CHUNK_BYTES: usize = 16 * 1024;
/// Сколько ждём `pid` от оболочки, которую сами запустили, прежде чем признать запуск сорванным.
const PID_DEADLINE: Duration = Duration::from_secs(15);
const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// Сколько попыток помещается в отчёт при подключении (сообщение не больше 1 МиБ).
const MAX_REPORT_JOBS: usize = 1000;
const MAX_ENV_VARS: usize = 64;
/// Единственные имена, которые задача берёт из `[jobs] env_file`: прокси для выхода в сеть.
/// Остальные строки файла не доходят до задачи.
const PROXY_ENV_NAMES: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("in-place jobs are not negotiated on this connection")]
    NotNegotiated,
    #[error("invalid in-place job command")]
    Invalid,
    #[error("job state I/O failed")]
    Io,
    #[error("job fact sink failed")]
    Sink,
}

impl From<io::Error> for JobError {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}

/// Куда менеджер пишет факты и откуда берёт канал загрузки. Реализацию даёт воркер
/// (журнал управления и загрузка медиа); тесты подставляют запись в память.
#[async_trait]
pub trait JobSink: Send + Sync {
    /// Факты одной записью и гашение команд, которые они закрывают.
    async fn facts(&self, facts: Vec<Body>, terminal: Vec<(String, u32)>) -> Result<(), JobError>;
    /// Загружает сжатый журнал попытки на хаб; возвращается после окончательного ответа.
    async fn upload_log(&self, header: MediaHeader, path: PathBuf) -> Result<(), JobError>;
}

/// Всё, что менеджеру нужно знать о воркере.
#[derive(Clone)]
pub struct JobsRuntime {
    pub data_dir: PathBuf,
    pub allowed_roots: Vec<PathBuf>,
    pub config: JobsConfig,
    /// Исполняемый файл воркера: он же оболочка попытки.
    pub shim_exe: PathBuf,
    pub lease_socket: Option<PathBuf>,
    /// Окружение воркера, из которого берутся `PATH`, `HOME`, `USER`, `LANG`, `TERM`, `XDG_*`.
    pub env: Vec<(String, String)>,
    /// Прокси из `[jobs] env_file`, прочитанные один раз при старте воркера: только они
    /// добавляются к окружению задачи, само окружение воркера не меняется.
    env_file_vars: Vec<(String, String)>,
    pub poll_interval: Duration,
    /// Сколько после TERM ждём, прежде чем убить группу.
    pub cancel_grace: Duration,
    pub chunk_interval: Duration,
}

impl JobsRuntime {
    pub fn new(
        data_dir: PathBuf,
        allowed_roots: Vec<PathBuf>,
        config: JobsConfig,
        shim_exe: PathBuf,
        lease_socket: Option<PathBuf>,
    ) -> Self {
        // Файл читается один раз на старте воркера: предупреждение тоже одно, а не на задачу.
        let env_file_vars = match &config.env_file {
            Some(path) => match read_env_file(path) {
                Ok(vars) => vars,
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        kind = ?error.kind(),
                        "jobs env_file is unreadable; jobs start without it"
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        Self {
            data_dir,
            allowed_roots,
            config,
            shim_exe,
            lease_socket,
            env: std::env::vars().collect(),
            env_file_vars,
            poll_interval: Duration::from_millis(250),
            cancel_grace: Duration::from_secs(3),
            chunk_interval: Duration::from_secs(1),
        }
    }

    /// Минимальное окружение команды: только названное воркером, без остального его окружения,
    /// плюс прокси из `[jobs] env_file`.
    fn base_env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = self
            .env
            .iter()
            .filter(|(name, _)| {
                matches!(name.as_str(), "PATH" | "HOME" | "USER" | "LANG" | "TERM")
                    || name.starts_with("XDG_")
            })
            .cloned()
            .collect();
        if !env.iter().any(|(name, _)| name == "PATH") {
            env.push(("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()));
        }
        env.extend(self.env_file_vars.iter().cloned());
        env
    }
}

/// Переменные прокси из файла `NAME=value`: пустые строки и комментарии пропускаются, всё,
/// чего нет в [`PROXY_ENV_NAMES`], отбрасывается.
fn read_env_file(path: &Path) -> io::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)?;
    let mut vars: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_suffix('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if !PROXY_ENV_NAMES.contains(&name) {
            continue;
        }
        vars.retain(|(existing, _)| existing != name);
        vars.push((name.to_owned(), value.to_owned()));
    }
    Ok(vars)
}

pub struct JobManager {
    rt: JobsRuntime,
    sink: Arc<dyn JobSink>,
    enabled: AtomicBool,
    tracked: Mutex<HashSet<(String, u32)>>,
    uploading: Mutex<HashSet<(String, u32)>>,
    chunks: broadcast::Sender<JobLogChunk>,
    shutdown: watch::Receiver<bool>,
    /// Будится после каждого шага наблюдателей, конца наблюдения и загрузки журнала.
    progress: Arc<tokio::sync::Notify>,
}

impl JobManager {
    pub fn new(
        rt: JobsRuntime,
        sink: Arc<dyn JobSink>,
        shutdown: watch::Receiver<bool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            rt,
            sink,
            enabled: AtomicBool::new(false),
            tracked: Mutex::default(),
            uploading: Mutex::default(),
            chunks: broadcast::channel(64).0,
            shutdown,
            progress: Arc::default(),
        })
    }

    /// Сигнал «что-то продвинулось»: ждущему не нужно опрашивать диск по таймеру, он
    /// перепроверяет своё условие после каждого шага наблюдателя.
    pub fn progress(&self) -> Arc<tokio::sync::Notify> {
        self.progress.clone()
    }

    /// Возможность `jobs_inplace_v1` согласована на текущем соединении. Без неё команды не
    /// принимаются и факты не пишутся; запущенные задачи идут дальше, итог ждёт соединения.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn max_running(&self) -> u32 {
        self.rt.config.max_running()
    }

    /// Непостоянные куски живого вывода; медленный получатель теряет старые.
    pub fn subscribe_chunks(&self) -> broadcast::Receiver<JobLogChunk> {
        self.chunks.subscribe()
    }

    /// Команда запуска. Каталог попытки занимается атомарным `mkdir`; уже занятый каталог —
    /// это повторная доставка или перезапуск: команда не запускается снова, а наблюдение
    /// продолжается с того места, где оно было.
    pub async fn start(self: &Arc<Self>, command: &Command) -> Result<(), JobError> {
        if !self.enabled() {
            return Err(JobError::NotNegotiated);
        }
        let Some(Kind::StartInPlaceJob(start)) = &command.kind else {
            return Err(JobError::Invalid);
        };
        let spec = self.spec_for(command, start)?;
        let dir = attempt_dir(&self.rt.data_dir, &spec.job_id, spec.attempt);
        let claim_dir = dir.clone();
        let claimed = tokio::task::spawn_blocking(move || -> io::Result<bool> {
            fs::create_dir_all(claim_dir.parent().expect("attempt has a job directory"))?;
            match fs::create_dir(&claim_dir) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            }
        })
        .await
        .map_err(|_| JobError::Io)??;
        let terminal = vec![(command.command_id.clone(), command.dispatch_attempt)];
        if !claimed {
            self.sink
                .facts(vec![], terminal)
                .await
                .map_err(|_| JobError::Sink)?;
            self.attach(spec.job_id, spec.attempt, false);
            return Ok(());
        }
        write_json(&dir.join(state::SPEC), &spec)?;
        // Команда погашается вместе с фактом «принято»: дальше за попыткой следят файлы.
        self.sink
            .facts(vec![dispatched_fact(&spec)], terminal)
            .await
            .map_err(|_| JobError::Sink)?;
        write_json(
            &dir.join(state::EMITTED),
            &Emitted {
                dispatched: true,
                ..Default::default()
            },
        )?;
        self.attach(spec.job_id, spec.attempt, true);
        Ok(())
    }

    fn spec_for(&self, command: &Command, start: &StartInPlaceJob) -> Result<Spec, JobError> {
        let job_id = canonical_job_id(&start.job_id).ok_or(JobError::Invalid)?;
        if start.attempt == 0 || start.shell.is_empty() || start.env.len() > MAX_ENV_VARS {
            return Err(JobError::Invalid);
        }
        let lease = match JobLease::try_from(start.lease).map_err(|_| JobError::Invalid)? {
            JobLease::None => "none",
            JobLease::Build => "build",
            JobLease::Gate => "gate",
        };
        let mut env = self.rt.base_env();
        for var in &start.env {
            if !valid_env_name(&var.name) {
                return Err(JobError::Invalid);
            }
            env.retain(|(name, _)| *name != var.name);
            env.push((var.name.clone(), var.value.clone()));
        }
        Ok(Spec {
            label: format!("job {}", job_id.chars().take(8).collect::<String>()),
            job_id,
            attempt: start.attempt,
            command_id: command.command_id.clone(),
            dispatch_attempt: command.dispatch_attempt,
            workdir: start.workdir.clone(),
            allowed_roots: self.rt.allowed_roots.clone(),
            shell: start.shell.clone(),
            env,
            timeout_seconds: start.timeout_seconds.clamp(1, 24 * 3600),
            lease: lease.into(),
            lease_socket: self.rt.lease_socket.clone(),
            log_max_bytes: self.rt.config.log_max_bytes(),
        })
    }

    /// Команда отмены. Отмена сначала сохраняется на диск, и только потом наблюдатель её
    /// исполняет, поэтому отмена до запуска запрещает запуск, а отмена после — убирает группу.
    pub async fn cancel(self: &Arc<Self>, command: &Command) -> Result<(), JobError> {
        if !self.enabled() {
            return Err(JobError::NotNegotiated);
        }
        let Some(Kind::CancelInPlaceJob(cancel)) = &command.kind else {
            return Err(JobError::Invalid);
        };
        let job_id = canonical_job_id(&cancel.job_id).ok_or(JobError::Invalid)?;
        let attempt = cancel.attempt;
        if attempt == 0 {
            return Err(JobError::Invalid);
        }
        let data_dir = self.rt.data_dir.clone();
        let marker_job = job_id.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let dir = attempt_dir(&data_dir, &marker_job, attempt);
            if dir.is_dir() {
                return state::persist_cancel(&dir);
            }
            // Команды запуска ещё нет: отметка рядом с будущим каталогом.
            fs::create_dir_all(job_dir(&data_dir, &marker_job))?;
            touch(&early_cancel_path(&data_dir, &marker_job, attempt))?;
            // Каталог мог появиться, пока писалась отметка: оболочка идёт под замком
            // запуска, поэтому отмена сохраняется ещё раз по правилам каталога.
            if dir.is_dir() {
                state::persist_cancel(&dir)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| JobError::Io)??;
        // Наблюдателя отмена не ставит: за попыткой уже следит тот, кто её запустил или
        // подобрал при старте, и он увидит отметку на следующем шаге. Новый наблюдатель
        // без каталога или с каталогом, который занимает запуск, принял бы работу за брошенную.
        self.sink
            .facts(
                vec![],
                vec![(command.command_id.clone(), command.dispatch_attempt)],
            )
            .await
            .map_err(|_| JobError::Sink)?;
        Ok(())
    }

    /// Подбирает всё, что осталось на диске: ставит наблюдателей на незавершённые попытки,
    /// на завершённые без записанного факта или загрузки журнала, и запускает уборку.
    pub async fn recover(self: &Arc<Self>) {
        let data_dir = self.rt.data_dir.clone();
        let found = tokio::task::spawn_blocking(move || list_attempts(&data_dir))
            .await
            .unwrap_or_default();
        for (job_id, attempt) in found {
            let dir = attempt_dir(&self.rt.data_dir, &job_id, attempt);
            let settled = read_json::<Finished>(&dir.join(state::RESULT)).is_some()
                && read_json::<Emitted>(&dir.join(state::EMITTED)).is_some_and(|e| e.finished)
                && dir.join(UPLOADED).exists();
            if !settled {
                self.attach(job_id, attempt, false);
            }
        }
        let manager = self.clone();
        let mut shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            loop {
                manager.sweep(now_ms()).await;
                tokio::select! {
                    _ = tokio::time::sleep(SWEEP_EVERY) => {}
                    _ = shutdown.wait_for(|stop| *stop) => return,
                }
            }
        });
    }

    /// Ставит наблюдателя на попытку, если его ещё нет. `fresh` — каталог только что занят
    /// этим вызовом: оболочки ещё не было, и её нужно запустить.
    fn attach(self: &Arc<Self>, job_id: String, attempt: u32, fresh: bool) {
        if !self
            .tracked
            .lock()
            .expect("tracked lock")
            .insert((job_id.clone(), attempt))
        {
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            let mut watcher = Watcher::new(manager.clone(), job_id.clone(), attempt, fresh);
            let mut shutdown = manager.shutdown.clone();
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let step = watcher.step().await;
                manager.progress.notify_waiters();
                if matches!(step, Step::Done) {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(manager.rt.poll_interval) => {}
                    _ = shutdown.wait_for(|stop| *stop) => break,
                }
            }
            manager
                .tracked
                .lock()
                .expect("tracked lock")
                .remove(&(job_id, attempt));
            manager.progress.notify_waiters();
        });
    }

    /// Отчёт при подключении: каждая попытка, которую воркер помнит, с последним состоянием.
    pub async fn report(&self) -> JobsReport {
        let data_dir = self.rt.data_dir.clone();
        let jobs = tokio::task::spawn_blocking(move || {
            cap_report(
                list_attempts(&data_dir)
                    .into_iter()
                    .filter_map(|(job_id, attempt)| snapshot(&data_dir, &job_id, attempt))
                    .collect(),
                MAX_REPORT_JOBS,
            )
        })
        .await
        .unwrap_or_default();
        JobsReport {
            jobs,
            observed_at_ms: now_ms(),
        }
    }

    /// Убирает каталоги завершённых попыток старше `state_retention`. Путь строится из
    /// разобранных идентификаторов и убирается по литеральному пути: ссылок не разыменовываем,
    /// рабочие каталоги и чужие файлы не трогаем. Возвращает число убранных попыток.
    pub async fn sweep(&self, now_ms: u64) -> usize {
        let data_dir = self.rt.data_dir.clone();
        let retention =
            u64::try_from(self.rt.config.state_retention.as_millis()).unwrap_or(u64::MAX);
        tokio::task::spawn_blocking(move || sweep_dirs(&data_dir, now_ms, retention))
            .await
            .unwrap_or(0)
    }

    fn tracked_count(&self) -> usize {
        self.tracked.lock().expect("tracked lock").len()
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with("CANTERLOT_LEASE_")
        && name != "CANTERLOT_NO_LEASE"
}

fn dispatched_fact(spec: &Spec) -> Body {
    Body::JobDispatched(JobDispatched {
        job_id: spec.job_id.clone(),
        attempt: spec.attempt,
        command_id: spec.command_id.clone(),
        dispatch_attempt: spec.dispatch_attempt,
    })
}

/// `(job_id, attempt)` всех каталогов попыток по возрастанию.
fn list_attempts(data_dir: &Path) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let Ok(jobs) = fs::read_dir(jobs_root(data_dir)) else {
        return found;
    };
    for job in jobs.flatten() {
        let Some(job_id) = job.file_name().to_str().and_then(canonical_job_id) else {
            continue;
        };
        if job.file_name().to_str() != Some(job_id.as_str())
            || !job.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let Ok(attempts) = fs::read_dir(job.path()) else {
            continue;
        };
        for attempt in attempts.flatten() {
            if let Some(number) = attempt
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .filter(|number| *number > 0)
                && attempt.file_type().is_ok_and(|kind| kind.is_dir())
            {
                found.push((job_id.clone(), number));
            }
        }
    }
    found.sort();
    found
}

fn snapshot(data_dir: &Path, job_id: &str, attempt: u32) -> Option<JobSnapshot> {
    let dir = attempt_dir(data_dir, job_id, attempt);
    let mut snapshot = JobSnapshot {
        job_id: job_id.into(),
        attempt,
        phase: JobPhase::Dispatched as i32,
        cancel_requested: state::cancel_requested(data_dir, job_id, attempt),
        ..Default::default()
    };
    if let Some(leader) = read_leader(&dir) {
        snapshot.pid = leader.pid;
        snapshot.start_time = leader.start_ticks;
    }
    if let Some(started) = read_started(&dir) {
        snapshot.phase = JobPhase::Running as i32;
        snapshot.started_at_ms = started;
    } else if dir.join(LEASE_WAIT).exists() {
        snapshot.phase = JobPhase::WaitingLease as i32;
    }
    if let Some(finished) = read_json::<Finished>(&dir.join(state::RESULT)) {
        snapshot.phase = JobPhase::Finished as i32;
        snapshot.outcome = outcome_code(&finished.outcome) as i32;
        snapshot.exit_code = finished.exit_code;
        snapshot.duration_ms = finished.duration_ms;
        snapshot.log_bytes = finished.log_bytes;
        snapshot.truncated = finished.truncated;
    }
    Some(snapshot)
}

/// Отчёт при подключении должен влезть в сообщение: незавершённые попытки идут первыми,
/// законченные добираются с самых свежих (идентификаторы задач упорядочены по времени).
fn cap_report(jobs: Vec<JobSnapshot>, max: usize) -> Vec<JobSnapshot> {
    let (finished, live): (Vec<_>, Vec<_>) = jobs
        .into_iter()
        .partition(|job| job.phase == JobPhase::Finished as i32);
    let room = max.saturating_sub(live.len());
    let skip = finished.len().saturating_sub(room);
    live.into_iter()
        .chain(finished.into_iter().skip(skip))
        .take(max)
        .collect()
}

fn outcome_name(outcome: JobOutcome) -> &'static str {
    match outcome {
        JobOutcome::Succeeded => "succeeded",
        JobOutcome::Failed => "failed",
        JobOutcome::TimedOut => "timed_out",
        JobOutcome::Canceled => "canceled",
        JobOutcome::LaunchInterrupted => "launch_interrupted",
        JobOutcome::WorkdirRefused => "workdir_refused",
        JobOutcome::Unspecified => "unspecified",
    }
}

fn outcome_code(name: &str) -> JobOutcome {
    [
        JobOutcome::Succeeded,
        JobOutcome::Failed,
        JobOutcome::TimedOut,
        JobOutcome::Canceled,
        JobOutcome::LaunchInterrupted,
        JobOutcome::WorkdirRefused,
    ]
    .into_iter()
    .find(|outcome| outcome_name(*outcome) == name)
    .unwrap_or(JobOutcome::Unspecified)
}

fn sweep_dirs(data_dir: &Path, now: u64, retention_ms: u64) -> usize {
    let mut removed = 0;
    for (job_id, attempt) in list_attempts(data_dir) {
        let dir = attempt_dir(data_dir, &job_id, attempt);
        let Some(finished) = read_json::<Finished>(&dir.join(state::RESULT)) else {
            continue;
        };
        let age = now.saturating_sub(finished.finished_at_ms);
        let reported = read_json::<Emitted>(&dir.join(state::EMITTED)).is_some_and(|e| e.finished);
        // Итог, который хаб так и не получил, живёт вдвое дольше: его ещё можно доставить.
        if age < retention_ms || (!reported && age < retention_ms.saturating_mul(2)) {
            continue;
        }
        // Путь собран из разобранных идентификаторов; ссылку вместо каталога не трогаем.
        if !fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir()) {
            continue;
        }
        if fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
            let _ = fs::remove_file(early_cancel_path(data_dir, &job_id, attempt));
            // Пустой каталог задачи уходит вместе с последней попыткой; непустой остаётся.
            let _ = fs::remove_dir(job_dir(data_dir, &job_id));
        }
    }
    removed
}

enum Step {
    Continue,
    Done,
}

/// Наблюдатель одной попытки. Всё состояние, от которого зависят решения, лежит на диске;
/// поля структуры — только то, что относится к этому процессу воркера.
struct Watcher {
    manager: Arc<JobManager>,
    job_id: String,
    attempt: u32,
    dir: PathBuf,
    /// Каталог занят этим процессом воркера: оболочку ещё нужно запустить.
    fresh: bool,
    child: Option<tokio::process::Child>,
    launched_at: Option<Instant>,
    terminated_at: Option<Instant>,
    sent_offset: u64,
    last_chunk: Instant,
}

impl Watcher {
    fn new(manager: Arc<JobManager>, job_id: String, attempt: u32, fresh: bool) -> Self {
        let dir = attempt_dir(&manager.rt.data_dir, &job_id, attempt);
        Self {
            manager,
            job_id,
            attempt,
            dir,
            fresh,
            child: None,
            launched_at: None,
            terminated_at: None,
            sent_offset: 0,
            last_chunk: Instant::now() - Duration::from_secs(3600),
        }
    }

    fn cancel_requested(&self) -> bool {
        state::cancel_requested(&self.manager.rt.data_dir, &self.job_id, self.attempt)
    }

    async fn step(&mut self) -> Step {
        if !self.dir.is_dir() {
            // Каталог убран уборкой: следить не за чем.
            return Step::Done;
        }
        if let Some(finished) = read_json::<Finished>(&self.dir.join(state::RESULT)) {
            return self.settle(&finished).await;
        }
        let Some(spec) = read_json::<Spec>(&self.dir.join(state::SPEC)) else {
            // Каталог занят, а описания нет: воркер умер между `mkdir` и записью.
            self.finish(JobOutcome::LaunchInterrupted, None, None).await;
            return Step::Continue;
        };
        let Some(leader) = read_leader(&self.dir) else {
            return self.before_pid(&spec).await;
        };
        if state::of_previous_boot(&self.dir) {
            return self.after_reboot(&spec, leader).await;
        }
        self.with_pid(&spec, leader).await
    }

    /// Попытка прошлой загрузки: её процессов нет, а номер лидера мог занять лидер чужой
    /// сессии. Сессию не ищем и сигналов не шлём, попытка закрывается сразу.
    async fn after_reboot(&mut self, spec: &Spec, leader: Leader) -> Step {
        self.reap_child();
        if self.manager.enabled() {
            self.emit_progress(spec, leader).await;
        }
        match read_exit(&self.dir) {
            Some((code, at)) => self.finish_by_exit(spec, code, at).await,
            None if self.cancel_requested() => self.finish(JobOutcome::Canceled, None, None).await,
            None => self.finish(JobOutcome::Failed, None, None).await,
        }
        Step::Continue
    }

    /// Оболочка ещё не записала `pid`: команда заведомо не запускалась.
    async fn before_pid(&mut self, spec: &Spec) -> Step {
        let cancel = self.cancel_requested();
        if self.child.is_none() && self.launched_at.is_none() {
            if cancel {
                self.finish(JobOutcome::Canceled, None, None).await;
            } else if self.fresh {
                match workdir::pin(&spec.workdir, &self.manager.rt.allowed_roots) {
                    Ok(_) => match self.launch() {
                        Ok(()) => {}
                        Err(_) => {
                            self.finish(JobOutcome::LaunchInterrupted, None, None).await;
                        }
                    },
                    Err(workdir::Refused) => {
                        self.finish(JobOutcome::WorkdirRefused, None, None).await;
                    }
                }
            } else {
                // Перезапуск воркера между занятием каталога и записью `pid`. Метка не даст
                // оболочке, если она всё же жива, запустить команду позже.
                let _ = touch(&self.dir.join(ABANDONED));
                if read_leader(&self.dir).is_none() {
                    self.finish(JobOutcome::LaunchInterrupted, None, None).await;
                }
            }
            return Step::Continue;
        }
        let exited = self
            .child
            .as_mut()
            .is_none_or(|child| child.try_wait().ok().flatten().is_some());
        let overdue = self
            .launched_at
            .is_some_and(|at| at.elapsed() > PID_DEADLINE);
        if cancel || overdue {
            let _ = touch(&self.dir.join(ABANDONED));
        }
        if exited || overdue {
            if read_leader(&self.dir).is_some() {
                return Step::Continue;
            }
            let outcome = if cancel {
                JobOutcome::Canceled
            } else {
                JobOutcome::LaunchInterrupted
            };
            self.finish(outcome, None, None).await;
        }
        Step::Continue
    }

    fn launch(&mut self) -> io::Result<()> {
        let rt = &self.manager.rt;
        let shim_log = fs::File::create(self.dir.join(state::SHIM_LOG))?;
        let path = rt
            .base_env()
            .into_iter()
            .find(|(name, _)| name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let child = tokio::process::Command::new(&rt.shim_exe)
            .arg(state::SHIM_ARG)
            .arg(&self.dir)
            .env_clear()
            .env("PATH", path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(shim_log)
            .kill_on_drop(false)
            .spawn()?;
        self.child = Some(child);
        self.launched_at = Some(Instant::now());
        Ok(())
    }

    async fn with_pid(&mut self, spec: &Spec, leader: Leader) -> Step {
        self.reap_child();
        if self.manager.enabled() {
            self.emit_progress(spec, leader).await;
        }
        if self.cancel_requested() {
            self.cancel_group(leader).await;
            return Step::Continue;
        }
        if let Some((code, at)) = read_exit(&self.dir) {
            self.finish_by_exit(spec, code, at).await;
            return Step::Continue;
        }
        if !state::is_same_process(leader) && state::session_members(leader).is_empty() {
            // Оболочка исчезла, не записав код выхода: убита снаружи или упала.
            self.finish(JobOutcome::Failed, None, None).await;
            return Step::Continue;
        }
        self.stream();
        Step::Continue
    }

    fn reap_child(&mut self) {
        if let Some(child) = &mut self.child
            && child.try_wait().ok().flatten().is_some()
        {
            self.child = None;
        }
    }

    /// Отмена идёт в два приёма: TERM всей сессии, а по истечении срока — KILL. Подтверждение
    /// выдаётся только когда в сессии никого не осталось.
    async fn cancel_group(&mut self, leader: Leader) {
        let members = state::session_members(leader);
        if members.is_empty() {
            match read_exit(&self.dir) {
                Some((code, at)) => {
                    if let Some(spec) = read_json::<Spec>(&self.dir.join(state::SPEC)) {
                        self.finish_by_exit(&spec, code, at).await;
                    }
                }
                None => self.finish(JobOutcome::Canceled, None, None).await,
            }
            return;
        }
        let signal = match self.terminated_at {
            None => {
                self.terminated_at = Some(Instant::now());
                Signal::TERM
            }
            Some(at) if at.elapsed() >= self.manager.rt.cancel_grace => Signal::KILL,
            Some(_) => return,
        };
        for pid in members {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = rustix::process::kill_process(pid, signal);
            }
        }
    }

    async fn finish_by_exit(&mut self, spec: &Spec, code: i32, finished_at: u64) {
        if read_started(&self.dir).is_none() && self.dir.join(WORKDIR_REFUSED).exists() {
            // Оболочка отказала каталогу после аренды: команда не запускалась.
            self.finish(JobOutcome::WorkdirRefused, None, Some(finished_at))
                .await;
            return;
        }
        let started = read_started(&self.dir).unwrap_or(finished_at);
        let duration = finished_at.saturating_sub(started);
        let timed_out =
            code == 124 || (code == 137 && duration >= u64::from(spec.timeout_seconds) * 1000);
        let outcome = if code == 0 {
            JobOutcome::Succeeded
        } else if timed_out {
            JobOutcome::TimedOut
        } else {
            JobOutcome::Failed
        };
        self.finish(outcome, Some(code), Some(finished_at)).await;
    }

    /// Записывает итог на диск; факт и загрузка журнала идут следующим шагом.
    async fn finish(&mut self, outcome: JobOutcome, exit_code: Option<i32>, at: Option<u64>) {
        self.stream_final();
        let view = LogView::open(&self.dir);
        let finished_at = at.unwrap_or_else(now_ms);
        let duration = read_started(&self.dir)
            .map(|started| finished_at.saturating_sub(started))
            .unwrap_or_default();
        let result = Finished {
            outcome: outcome_name(outcome).into(),
            exit_code,
            duration_ms: duration,
            log_bytes: view.total(),
            truncated: view.truncated(),
            finished_at_ms: finished_at,
        };
        if write_json(&self.dir.join(state::RESULT), &result).is_err() {
            tracing::warn!(
                kind = "job_result_write_failed",
                "job result was not stored"
            );
        }
    }

    /// Факты и загрузка журнала завершённой попытки. Попытка снимается с наблюдения, когда
    /// факт записан и загрузка либо сделана, либо идёт отдельной задачей.
    async fn settle(&mut self, finished: &Finished) -> Step {
        let emitted: Emitted = read_json(&self.dir.join(state::EMITTED)).unwrap_or_default();
        if !self.manager.enabled() {
            return Step::Continue;
        }
        if !emitted.finished {
            let Some(spec) = read_json::<Spec>(&self.dir.join(state::SPEC)) else {
                // Попытка без описания: хаб увидит её по отчёту, команды у неё нет.
                self.emit_one(dispatched_fact_for(&self.job_id, self.attempt), |e| {
                    e.dispatched = true;
                })
                .await;
                return self.emit_finished(finished).await;
            };
            let leader = read_leader(&self.dir).unwrap_or(Leader {
                pid: 0,
                start_ticks: 0,
            });
            self.emit_progress(&spec, leader).await;
            return self.emit_finished(finished).await;
        }
        if !self.dir.join(UPLOADED).exists() {
            self.upload();
        }
        Step::Done
    }

    async fn emit_finished(&mut self, finished: &Finished) -> Step {
        let view = LogView::open(&self.dir);
        let fact = Body::JobFinished(JobFinished {
            job_id: self.job_id.clone(),
            attempt: self.attempt,
            outcome: outcome_code(&finished.outcome) as i32,
            exit_code: finished.exit_code,
            duration_ms: finished.duration_ms,
            log_bytes: finished.log_bytes,
            tail: view.tail(TAIL_BYTES),
            truncated: finished.truncated,
        });
        if self.emit_one(fact, |e| e.finished = true).await {
            self.upload();
            return Step::Done;
        }
        Step::Continue
    }

    /// Факты хода попытки по порядку: принято, ждёт аренды, запущено. Уже записанные
    /// пропускаются, поэтому повтор после сбоя безопасен.
    async fn emit_progress(&mut self, spec: &Spec, leader: Leader) {
        let emitted: Emitted = read_json(&self.dir.join(state::EMITTED)).unwrap_or_default();
        if !emitted.dispatched
            && !self
                .emit_one(dispatched_fact(spec), |e| e.dispatched = true)
                .await
        {
            return;
        }
        if !emitted.waiting_lease && self.dir.join(LEASE_WAIT).exists() {
            let lease = match spec.lease.as_str() {
                "gate" => JobLease::Gate,
                "build" => JobLease::Build,
                _ => JobLease::None,
            };
            let fact = Body::JobWaitingLease(JobWaitingLease {
                job_id: spec.job_id.clone(),
                attempt: spec.attempt,
                lease: lease as i32,
            });
            if !self.emit_one(fact, |e| e.waiting_lease = true).await {
                return;
            }
        }
        if !emitted.started
            && let Some(started_at) = read_started(&self.dir)
        {
            let fact = Body::JobStarted(JobStarted {
                job_id: spec.job_id.clone(),
                attempt: spec.attempt,
                pid: leader.pid,
                start_time: leader.start_ticks,
                started_at_ms: started_at,
            });
            self.emit_one(fact, |e| e.started = true).await;
        }
    }

    /// Пишет факт и только потом отмечает его записанным. Сбой между ними даст повтор факта.
    async fn emit_one(&mut self, fact: Body, mark: impl FnOnce(&mut Emitted)) -> bool {
        if !self.manager.enabled() {
            return false;
        }
        if self.manager.sink.facts(vec![fact], vec![]).await.is_err() {
            return false;
        }
        let path = self.dir.join(state::EMITTED);
        let mut emitted: Emitted = read_json(&path).unwrap_or_default();
        mark(&mut emitted);
        write_json(&path, &emitted).is_ok()
    }

    fn upload(&self) {
        let key = (self.job_id.clone(), self.attempt);
        if !self
            .manager
            .uploading
            .lock()
            .expect("uploading lock")
            .insert(key.clone())
        {
            return;
        }
        let manager = self.manager.clone();
        let dir = self.dir.clone();
        let job_id = self.job_id.clone();
        let attempt = self.attempt;
        tokio::spawn(async move {
            upload_log(&manager, &dir, &job_id, attempt).await;
            manager
                .uploading
                .lock()
                .expect("uploading lock")
                .remove(&key);
            manager.progress.notify_waiters();
        });
    }

    /// Живой вывод: не чаще раза в `chunk_interval` и не больше одного куска за раз.
    fn stream(&mut self) {
        if self.last_chunk.elapsed() < self.manager.rt.chunk_interval {
            return;
        }
        self.last_chunk = Instant::now();
        self.send_chunk();
    }

    fn stream_final(&mut self) {
        for _ in 0..4 {
            if !self.send_chunk() {
                break;
            }
        }
    }

    fn send_chunk(&mut self) -> bool {
        let view = LogView::open(&self.dir);
        let (at, data) = view.read_from(self.sent_offset, CHUNK_BYTES);
        if data.is_empty() {
            return false;
        }
        self.sent_offset = at + data.len() as u64;
        // Без получателей кусок не нужен: вывод остаётся в журнале.
        let _ = self.manager.chunks.send(JobLogChunk {
            job_id: self.job_id.clone(),
            attempt: self.attempt,
            offset: at,
            data,
        });
        true
    }
}

fn dispatched_fact_for(job_id: &str, attempt: u32) -> Body {
    Body::JobDispatched(JobDispatched {
        job_id: job_id.into(),
        attempt,
        command_id: String::new(),
        dispatch_attempt: 0,
    })
}

/// Сжимает журнал и грузит его через общий путь медиа. Идентификатор вложения выводится из
/// попытки, поэтому повтор после сбоя не плодит копий.
async fn upload_log(manager: &Arc<JobManager>, dir: &Path, job_id: &str, attempt: u32) {
    let dir = dir.to_path_buf();
    let prepared = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || compress_log(&dir)).await
    };
    let Ok(Ok(Some((path, size, sha256)))) = prepared else {
        if matches!(prepared, Ok(Ok(None))) {
            // Крупнее предела вложения: журнал остаётся на воркере.
            let _ = touch(&dir.join(UPLOADED));
        }
        return;
    };
    let name: String = job_id.chars().take(8).collect();
    let header = MediaHeader {
        media_id: Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("canterlot-job-log:{job_id}:{attempt}").as_bytes(),
        )
        .to_string(),
        mime: "application/gzip".into(),
        size,
        sha256,
        name: format!("job-{name}-{attempt}.log.gz"),
        job_id: job_id.into(),
        job_attempt: attempt,
        ..Default::default()
    };
    if manager.sink.upload_log(header, path).await.is_ok() {
        let _ = touch(&dir.join(UPLOADED));
    }
}

/// `None`, когда сжатый журнал больше предела вложения.
fn compress_log(dir: &Path) -> io::Result<Option<(PathBuf, u64, String)>> {
    use std::io::Write;
    let target = dir.join(state::LOG_GZ);
    let temporary = dir.join(".log.gz.tmp");
    {
        let file = fs::File::create(&temporary)?;
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        LogView::open(dir).assemble(&mut encoder)?;
        encoder.flush()?;
        encoder.finish()?.sync_all()?;
    }
    fs::rename(&temporary, &target)?;
    let size = fs::metadata(&target)?.len();
    if size > MAX_MEDIA_BYTES {
        let _ = fs::remove_file(&target);
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(&target)?;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Some((target, size, sha256)))
}

impl JobManager {
    /// Сколько попыток сейчас под наблюдением (для проверок и отладочных строк).
    pub fn watching(&self) -> usize {
        self.tracked_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(index: u32, phase: JobPhase) -> JobSnapshot {
        JobSnapshot {
            job_id: format!("job-{index:04}"),
            attempt: 1,
            phase: phase as i32,
            ..Default::default()
        }
    }

    #[test]
    fn report_cap_keeps_every_live_attempt_then_the_newest_finished() {
        let mut jobs: Vec<_> = (0..10).map(|i| snap(i, JobPhase::Finished)).collect();
        jobs.insert(3, snap(100, JobPhase::Running));
        jobs.push(snap(101, JobPhase::WaitingLease));
        let capped = cap_report(jobs.clone(), 5);
        let ids: Vec<_> = capped.iter().map(|job| job.job_id.as_str()).collect();
        assert_eq!(
            ids,
            ["job-0100", "job-0101", "job-0007", "job-0008", "job-0009"]
        );
        assert_eq!(
            cap_report(jobs.clone(), 100).len(),
            12,
            "under the cap nothing is dropped"
        );
        // Незавершённых больше предела: отчёт всё равно не длиннее предела.
        assert_eq!(cap_report(jobs, 1).len(), 1);
    }
}
