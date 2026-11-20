//! Задачи на месте на настоящих процессах: оболочка попытки (`canterlot-worker job-shim`),
//! каталоги состояния во временных каталогах и временный HOME.
//!
//! Ожидания событийные: менеджер будит `progress` после каждого шага наблюдателя, сервер
//! аренды публикует снимок очереди, живой вывод приходит кусками. Таймеров и часов в тестах нет.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    CancelInPlaceJob, Command, JobEnvVar, JobLease, JobOutcome, JobPhase, MediaHeader,
    StartInPlaceJob, command::Kind, record_body::Body,
};
use canterlot_worker::{
    config::JobsConfig,
    containment::LEASE_SOCKET_ENV,
    jobs::{JobError, JobManager, JobSink, JobsRuntime},
    lease::server::{LeaseServer, ServerConfig},
};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");
const WAIT: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Recorder {
    facts: Mutex<Vec<Body>>,
    terminals: Mutex<Vec<(String, u32)>>,
    uploads: Mutex<Vec<(MediaHeader, Vec<u8>)>>,
}

#[async_trait]
impl JobSink for Recorder {
    async fn facts(&self, facts: Vec<Body>, terminal: Vec<(String, u32)>) -> Result<(), JobError> {
        self.facts.lock().unwrap().extend(facts);
        self.terminals.lock().unwrap().extend(terminal);
        Ok(())
    }

    async fn upload_log(&self, header: MediaHeader, path: PathBuf) -> Result<(), JobError> {
        let bytes = fs::read(path).map_err(|_| JobError::Io)?;
        self.uploads.lock().unwrap().push((header, bytes));
        Ok(())
    }
}

impl Recorder {
    fn facts(&self) -> Vec<Body> {
        self.facts.lock().unwrap().clone()
    }

    fn finished(&self, job_id: &str) -> Option<canterlot_proto::worker::v1::JobFinished> {
        self.facts().into_iter().find_map(|body| match body {
            Body::JobFinished(fact) if fact.job_id == job_id => Some(fact),
            _ => None,
        })
    }

    /// Названия фактов задачи по порядку записи.
    fn kinds(&self, job_id: &str) -> Vec<&'static str> {
        self.facts()
            .into_iter()
            .filter_map(|body| match body {
                Body::JobDispatched(fact) if fact.job_id == job_id => Some("dispatched"),
                Body::JobWaitingLease(fact) if fact.job_id == job_id => Some("waiting_lease"),
                Body::JobStarted(fact) if fact.job_id == job_id => Some("started"),
                Body::JobFinished(fact) if fact.job_id == job_id => Some("finished"),
                _ => None,
            })
            .collect()
    }

    fn uploaded(&self) -> bool {
        !self.uploads.lock().unwrap().is_empty()
    }
}

/// Ждёт условие, перепроверяя его после каждого события; без события дольше `WAIT` — отказ.
async fn until(progress: Arc<Notify>, what: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        loop {
            let notified = progress.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if check() {
                return;
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

struct Harness {
    data: tempfile::TempDir,
    work: tempfile::TempDir,
    home: tempfile::TempDir,
    sink: Arc<Recorder>,
    stop: watch::Sender<bool>,
    manager: Arc<JobManager>,
    lease_socket: Option<PathBuf>,
    config: JobsConfig,
}

fn runtime(
    data: &Path,
    work: &Path,
    home: &Path,
    config: &JobsConfig,
    lease_socket: Option<PathBuf>,
    shim: &str,
) -> JobsRuntime {
    let mut rt = JobsRuntime::new(
        data.to_path_buf(),
        vec![work.to_path_buf()],
        config.clone(),
        PathBuf::from(shim),
        lease_socket,
    );
    rt.env = vec![
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
        ("HOME".into(), home.display().to_string()),
        ("USER".into(), "tester".into()),
        ("LANG".into(), "C".into()),
        ("TERM".into(), "dumb".into()),
    ];
    rt.poll_interval = Duration::from_millis(40);
    rt.cancel_grace = Duration::from_millis(600);
    rt.chunk_interval = Duration::from_millis(100);
    rt
}

impl Harness {
    async fn new() -> Self {
        Self::with(JobsConfig::default(), None, WORKER).await
    }

    async fn with(config: JobsConfig, lease_socket: Option<PathBuf>, shim: &str) -> Self {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let (stop, shutdown) = watch::channel(false);
        let sink = Arc::new(Recorder::default());
        let manager = JobManager::new(
            runtime(
                data.path(),
                &work.path().canonicalize().unwrap(),
                home.path(),
                &config,
                lease_socket.clone(),
                shim,
            ),
            sink.clone(),
            shutdown,
        );
        manager.set_enabled(true);
        Self {
            data,
            work,
            home,
            sink,
            stop,
            manager,
            lease_socket,
            config,
        }
    }

    async fn wait(&self, what: &str, check: impl FnMut() -> bool) {
        until(self.manager.progress(), what, check).await;
    }

    fn work(&self) -> PathBuf {
        self.work.path().canonicalize().unwrap()
    }

    /// Новый менеджер над теми же каталогами: так выглядит перезапуск воркера.
    async fn restart(&mut self) {
        let _ = self.stop.send(true);
        let old = self.manager.clone();
        until(old.progress(), "old watchers to stop", || {
            old.watching() == 0
        })
        .await;
        let (stop, shutdown) = watch::channel(false);
        self.stop = stop;
        self.manager = JobManager::new(
            runtime(
                self.data.path(),
                &self.work(),
                self.home.path(),
                &self.config,
                self.lease_socket.clone(),
                WORKER,
            ),
            self.sink.clone(),
            shutdown,
        );
        self.manager.set_enabled(true);
    }

    fn start_command(&self, job_id: &str, shell: &str) -> StartInPlaceJob {
        StartInPlaceJob {
            job_id: job_id.into(),
            attempt: 1,
            workdir: self.work().display().to_string(),
            shell: shell.into(),
            env: vec![],
            timeout_seconds: 60,
            lease: JobLease::None as i32,
        }
    }

    fn envelope(kind: Kind) -> Command {
        Command {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            kind: Some(kind),
            ..Default::default()
        }
    }

    async fn start(&self, start: StartInPlaceJob) -> String {
        let job_id = start.job_id.clone();
        self.manager
            .start(&Self::envelope(Kind::StartInPlaceJob(start)))
            .await
            .unwrap();
        job_id
    }

    async fn run(&self, shell: &str) -> String {
        let id = Uuid::now_v7().to_string();
        self.start(self.start_command(&id, shell)).await
    }

    async fn cancel(&self, job_id: &str) {
        self.manager
            .cancel(&Self::envelope(Kind::CancelInPlaceJob(CancelInPlaceJob {
                job_id: job_id.into(),
                attempt: 1,
            })))
            .await
            .unwrap();
    }

    fn attempt_dir(&self, job_id: &str) -> PathBuf {
        self.data.path().join("jobs").join(job_id).join("1")
    }

    async fn finished(&self, job_id: &str) -> canterlot_proto::worker::v1::JobFinished {
        self.wait("JobFinished", || self.sink.finished(job_id).is_some())
            .await;
        self.sink.finished(job_id).unwrap()
    }

    /// Журнал загружен: после итога и после загрузки наблюдатель будит `progress`.
    async fn uploaded(&self) {
        self.wait("log upload", || self.sink.uploaded()).await;
    }

    /// Ждёт, пока команда напечатает `needle`: куски живого вывода идут по мере записи.
    async fn output_has(
        chunks: &mut tokio::sync::broadcast::Receiver<canterlot_proto::worker::v1::JobLogChunk>,
        needle: &str,
    ) {
        tokio::time::timeout(WAIT, async {
            let mut seen = String::new();
            loop {
                if let Ok(chunk) = chunks.recv().await {
                    seen.push_str(&String::from_utf8_lossy(&chunk.data));
                    if seen.contains(needle) {
                        return;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the command never printed {needle}"));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // Брошенные попытки не должны пережить тест.
        if let Ok(jobs) = fs::read_dir(self.data.path().join("jobs")) {
            for job in jobs.flatten() {
                for attempt in fs::read_dir(job.path()).into_iter().flatten().flatten() {
                    if let Ok(text) = fs::read_to_string(attempt.path().join("pid"))
                        && let Some(pid) = text
                            .split_whitespace()
                            .next()
                            .and_then(|pid| pid.parse::<i32>().ok())
                    {
                        kill_session(pid);
                    }
                }
            }
        }
    }
}

fn session_ids(session: i32) -> Vec<i32> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(end) = stat.rfind(')') else { continue };
        let fields: Vec<_> = stat[end + 1..].split_whitespace().collect();
        let zombie = fields.first() == Some(&"Z");
        if !zombie && fields.get(3).and_then(|sid| sid.parse::<i32>().ok()) == Some(session) {
            found.push(pid);
        }
    }
    found
}

fn kill_session(session: i32) {
    for pid in session_ids(session) {
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

fn gunzip(bytes: &[u8]) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tokio::test(flavor = "multi_thread")]
async fn success_reports_dispatched_started_finished_with_the_output_tail() {
    let harness = Harness::new().await;
    let id = harness.run("echo hello; echo from-stderr >&2").await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert_eq!(finished.exit_code, Some(0));
    assert_eq!(finished.tail, b"hello\nfrom-stderr\n");
    assert_eq!(finished.log_bytes, 18);
    assert!(!finished.truncated);
    assert_eq!(
        harness.sink.kinds(&id),
        ["dispatched", "started", "finished"]
    );
    // Команда запуска погашена вместе с фактом «принято», а не держится до конца задачи.
    assert_eq!(harness.sink.terminals.lock().unwrap().len(), 1);
    let started = harness
        .sink
        .facts()
        .into_iter()
        .find_map(|body| match body {
            Body::JobStarted(fact) => Some(fact),
            _ => None,
        });
    let started = started.unwrap();
    assert!(started.pid > 0 && started.start_time > 0, "{started:?}");
    // Журнал уходит вложением через существующий путь загрузки, сжатый.
    harness.uploaded().await;
    let uploads = harness.sink.uploads.lock().unwrap();
    assert_eq!(uploads[0].0.job_id, id);
    assert_eq!(uploads[0].0.job_attempt, 1);
    assert_eq!(uploads[0].0.mime, "application/gzip");
    assert_eq!(gunzip(&uploads[0].1), "hello\nfrom-stderr\n");
    assert_eq!(uploads[0].0.size as usize, uploads[0].1.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn failure_keeps_the_exit_code_and_timeout_wins_over_it() {
    let harness = Harness::new().await;
    let id = harness.run("echo bad >&2; exit 3").await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Failed as i32);
    assert_eq!(finished.exit_code, Some(3));
    assert_eq!(finished.tail, b"bad\n");

    let slow = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&slow, "echo begun; sleep 30");
    start.timeout_seconds = 1;
    harness.start(start).await;
    let finished = harness.finished(&slow).await;
    assert_eq!(finished.outcome, JobOutcome::TimedOut as i32);
    // Секунда лимита, а не тридцать секунд команды: `timeout` оборвал её сам.
    assert!(
        finished.duration_ms >= 900 && finished.duration_ms < 15_000,
        "{finished:?}"
    );
    assert_eq!(finished.tail, b"begun\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn command_runs_in_the_workdir_with_a_minimal_environment_plus_env() {
    let harness = Harness::new().await;
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, "pwd; echo \"$HOME|$USER|$LANG|$TERM|$MODE\"; env");
    start.env = vec![JobEnvVar {
        name: "MODE".into(),
        value: "deploy".into(),
    }];
    harness.start(start).await;
    let finished = harness.finished(&id).await;
    let text = String::from_utf8(finished.tail).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next().unwrap(), harness.work().display().to_string());
    assert_eq!(
        lines.next().unwrap(),
        format!("{}|tester|C|dumb|deploy", harness.home.path().display())
    );
    // Окружение воркера не просачивается: только заданное воркером и `env` задачи.
    for line in lines {
        let name = line.split('=').next().unwrap();
        assert!(
            matches!(
                name,
                "PATH"
                    | "HOME"
                    | "USER"
                    | "LANG"
                    | "TERM"
                    | "MODE"
                    | "PWD"
                    | "SHLVL"
                    | "_"
                    | "OLDPWD"
                    | "LOGNAME"
                    | "SHELL"
            ) || name.starts_with("XDG_"),
            "unexpected variable {name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lease_variables_cannot_be_smuggled_through_env() {
    let harness = Harness::new().await;
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, "true");
    start.env = vec![JobEnvVar {
        name: "CANTERLOT_LEASE_HELD".into(),
        value: "unleased".into(),
    }];
    let refused = harness
        .manager
        .start(&Harness::envelope(Kind::StartInPlaceJob(start)))
        .await;
    assert!(matches!(refused, Err(JobError::Invalid)));
    assert!(!harness.data.path().join("jobs").join(&id).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_before_start_prevents_the_command_from_ever_running() {
    let harness = Harness::new().await;
    let id = Uuid::now_v7().to_string();
    harness.cancel(&id).await;
    let marker = harness.work().join("ran");
    harness
        .start(harness.start_command(&id, &format!("touch {}", marker.display())))
        .await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Canceled as i32);
    assert_eq!(finished.exit_code, None);
    // Итог записан без запуска оболочки: ни `pid`, ни факта запуска.
    assert!(!harness.attempt_dir(&id).join("pid").exists());
    assert!(!marker.exists(), "a cancelled job must not start");
    assert!(!harness.sink.kinds(&id).contains(&"started"));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_the_run_is_confirmed_only_after_the_group_is_gone() {
    let harness = Harness::new().await;
    let mut chunks = harness.manager.subscribe_chunks();
    let pids = harness.work().join("pids");
    // Внуки в своей группе и в своей сессии: подтверждение не должно опередить их смерть.
    let id = harness
        .run(&format!(
            "sleep 300 & echo $! >> {pids}; setsid sleep 300 & echo $! >> {pids}; echo ready; wait",
            pids = pids.display()
        ))
        .await;
    Harness::output_has(&mut chunks, "ready").await;
    let leader = fs::read_to_string(harness.attempt_dir(&id).join("pid"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(!session_ids(leader).is_empty());
    let sleepers: Vec<i32> = fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect();
    assert_eq!(sleepers.len(), 2);
    harness.cancel(&id).await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Canceled as i32);
    // К моменту факта сессия пуста: фоновый потомок убит.
    assert_eq!(session_ids(leader), Vec::<i32>::new());
    let alive = |pid: i32| {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| !stat.contains(") Z") && !stat.contains(") X"))
    };
    assert!(!alive(sleepers[0]), "the background child is gone");
    // Процесс, ушедший в свою сессию, остаётся: границей служит сессия попытки.
    for pid in sleepers {
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn crash_between_claim_and_pid_becomes_launch_interrupted_and_never_runs() {
    // Воркер занял каталог и умер до записи `pid`: оболочка так и не запустилась.
    let harness = Harness::new().await;
    let id = Uuid::now_v7().to_string();
    fs::create_dir_all(harness.attempt_dir(&id)).unwrap();
    harness.manager.recover().await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::LaunchInterrupted as i32);
    assert_eq!(finished.exit_code, None);

    // Описание записано, а оболочки нет: тот же исход, команда не запускалась.
    let claimed = Harness::new().await;
    let marker = claimed.work().join("ran");
    let id = Uuid::now_v7().to_string();
    let dir = claimed.attempt_dir(&id);
    fs::create_dir_all(&dir).unwrap();
    let spec = serde_json::json!({
        "job_id": id, "attempt": 1, "command_id": "", "dispatch_attempt": 0,
        "workdir": claimed.work().display().to_string(),
        "shell": format!("touch {}", marker.display()),
        "env": [], "timeout_seconds": 60, "lease": "none", "lease_socket": null,
        "log_max_bytes": 1_048_576, "label": "job",
    });
    fs::write(dir.join("spec.json"), spec.to_string()).unwrap();
    claimed.manager.recover().await;
    let finished = claimed.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::LaunchInterrupted as i32);
    assert!(!marker.exists());

    // Оболочка стартовала, но не дошла до `pid`: тот же исход.
    let broken = Harness::with(JobsConfig::default(), None, "/bin/true").await;
    let marker = broken.work().join("ran");
    let id = broken.run(&format!("touch {}", marker.display())).await;
    let finished = broken.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::LaunchInterrupted as i32);
    assert!(!marker.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_after_pid_reattaches_without_relaunching() {
    let mut harness = Harness::new().await;
    let mut chunks = harness.manager.subscribe_chunks();
    let counter = harness.work().join("launches");
    let id = harness
        .run(&format!(
            "echo x >> {counter}; echo up; sleep 2; echo end",
            counter = counter.display()
        ))
        .await;
    Harness::output_has(&mut chunks, "up").await;
    harness
        .wait("started fact", || {
            harness.sink.kinds(&id).contains(&"started")
        })
        .await;
    let before = harness.sink.facts().len();

    harness.restart().await;
    // Хаб повторно присылает ту же команду, воркер заново проигрывает сохранённые.
    harness.manager.recover().await;
    harness
        .manager
        .start(&Harness::envelope(Kind::StartInPlaceJob(
            harness.start_command(&id, "echo SHOULD-NOT-RUN"),
        )))
        .await
        .unwrap();
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert_eq!(finished.tail, b"up\nend\n");
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        1,
        "the command ran exactly once"
    );
    assert!(harness.sink.facts().len() > before);
    // Факт запуска не дублируется после перезапуска.
    let started = harness
        .sink
        .kinds(&id)
        .into_iter()
        .filter(|kind| *kind == "started")
        .count();
    assert_eq!(started, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn workdir_must_canonicalize_to_itself_inside_the_allowed_roots() {
    let harness = Harness::new().await;
    let marker = harness.work().join("ran");
    let shell = format!("touch {}", marker.display());
    let sub = harness.work().join("sub");
    fs::create_dir(&sub).unwrap();
    let link = harness.work().join("link");
    std::os::unix::fs::symlink(&sub, &link).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_link = harness.work().join("escape");
    std::os::unix::fs::symlink(outside.path(), &outside_link).unwrap();

    for refused in [
        // символьная ссылка: канонический путь иной
        link.display().to_string(),
        // `..` и лишний слэш
        format!("{}/sub/../sub", harness.work().display()),
        format!("{}//sub", harness.work().display()),
        // за корнем
        outside.path().display().to_string(),
        outside_link.display().to_string(),
        // не абсолютный, нет такого, корень файловой системы
        "relative/dir".into(),
        harness.work().join("missing").display().to_string(),
        "/".into(),
    ] {
        let id = Uuid::now_v7().to_string();
        let mut start = harness.start_command(&id, &shell);
        start.workdir = refused.clone();
        harness.start(start).await;
        let finished = harness.finished(&id).await;
        assert_eq!(
            finished.outcome,
            JobOutcome::WorkdirRefused as i32,
            "{refused}"
        );
        assert_eq!(harness.sink.kinds(&id), ["dispatched", "finished"]);
        assert!(!harness.attempt_dir(&id).join("pid").exists());
    }
    assert!(!marker.exists(), "no refused job may run");

    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, "pwd");
    start.workdir = sub.display().to_string();
    harness.start(start).await;
    assert_eq!(
        harness.finished(&id).await.outcome,
        JobOutcome::Succeeded as i32
    );
}

/// Исполняемый файл-обёртка: тот же воркер, но оболочка попытки останавливается перед
/// созданием процесса команды (тестовый шов `CANTERLOT_JOB_SHIM_PAUSE`).
fn pausing_shim(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("pausing-shim.sh");
    fs::write(
        &path,
        format!("#!/bin/sh\nexport CANTERLOT_JOB_SHIM_PAUSE=1\nexec {WORKER} \"$@\"\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn workdir_swapped_for_a_symlink_while_waiting_for_the_lease_is_refused() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("data")).unwrap();
    fs::create_dir(root.path().join("runtime")).unwrap();
    let (stop, _) = watch::channel(false);
    let config = ServerConfig::new(
        Some(&root.path().join("runtime")),
        &root.path().join("data"),
        2,
    );
    let socket = config.socket.clone();
    let server = LeaseServer::start(config, stop.subscribe());
    let harness = Harness::with(JobsConfig::default(), Some(socket.clone()), WORKER).await;
    let mut gate = HeldLease::take("gate", &socket, root.path());
    snapshot_until(&server, "the gate to be held", |s| s.holders.len() == 1).await;

    // Каталог проходит проверку воркера, затем попытка встаёт в очередь за гейтом.
    let sub = harness.work().join("sub");
    let other = harness.work().join("other");
    fs::create_dir(&sub).unwrap();
    fs::create_dir(&other).unwrap();
    let marker = harness.work().join("ran");
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(
        &id,
        &format!("touch {}; touch ./ran-in-cwd", marker.display()),
    );
    start.workdir = sub.display().to_string();
    start.lease = JobLease::Build as i32;
    harness.start(start).await;
    snapshot_until(&server, "the job to queue behind the gate", |s| {
        s.waiters.len() == 1
    })
    .await;

    // Пока попытка ждёт, каталог подменяют ссылкой на другой каталог внутри того же корня.
    fs::remove_dir(&sub).unwrap();
    std::os::unix::fs::symlink(&other, &sub).unwrap();
    gate.release();

    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::WorkdirRefused as i32);
    assert_eq!(finished.exit_code, None);
    assert!(!marker.exists(), "the command must not run");
    assert!(!other.join("ran-in-cwd").exists());
    assert!(!harness.sink.kinds(&id).contains(&"started"));
    assert!(!harness.attempt_dir(&id).join("started").exists());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn workdir_swapped_after_the_check_does_not_move_the_command() {
    let shims = tempfile::tempdir().unwrap();
    let harness = Harness::with(JobsConfig::default(), None, &pausing_shim(shims.path())).await;
    let sub = harness.work().join("sub");
    let moved = harness.work().join("moved");
    let other = harness.work().join("other");
    fs::create_dir(&sub).unwrap();
    fs::create_dir(&other).unwrap();
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, "touch ./ran-in-cwd");
    start.workdir = sub.display().to_string();
    harness.start(start).await;

    // Оболочка проверила каталог и стоит перед созданием процесса: путь подменяют.
    let dir = harness.attempt_dir(&id);
    harness
        .wait("the shim to reach the start", || {
            dir.join("pause_reached").exists()
        })
        .await;
    fs::rename(&sub, &moved).unwrap();
    std::os::unix::fs::symlink(&other, &sub).unwrap();
    fs::write(dir.join("pause_release"), b"").unwrap();

    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert!(
        moved.join("ran-in-cwd").exists(),
        "the command runs in the directory that was checked"
    );
    assert!(
        !other.join("ran-in-cwd").exists(),
        "the swapped path must not be followed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_is_serialized_with_process_creation_and_leaves_no_survivor() {
    let shims = tempfile::tempdir().unwrap();
    let harness = Harness::with(JobsConfig::default(), None, &pausing_shim(shims.path())).await;
    let marker = harness.work().join("ran");
    let id = harness
        .run(&format!("touch {}; sleep 300", marker.display()))
        .await;
    let dir = harness.attempt_dir(&id);
    harness
        .wait("the shim to reach the start", || {
            dir.join("pause_reached").exists()
        })
        .await;

    // Оболочка прошла последнюю проверку отмены и стоит перед созданием процесса. Отмена не
    // может сохраниться в этом окне: иначе команда стартовала бы после сохранённой отмены.
    let manager = harness.manager.clone();
    let cancel_id = id.clone();
    let mut cancel = tokio::spawn(async move {
        manager
            .cancel(&Harness::envelope(Kind::CancelInPlaceJob(CancelInPlaceJob {
                job_id: cancel_id,
                attempt: 1,
            })))
            .await
    });
    let early = tokio::time::timeout(Duration::from_millis(300), &mut cancel).await;
    assert!(
        early.is_err(),
        "a cancel must not be persisted between the last check and the process creation"
    );
    assert!(!dir.join("cancel").exists());
    fs::write(dir.join("pause_release"), b"").unwrap();
    cancel.await.unwrap().unwrap();

    // Отмена сохранена после создания процесса: группа убрана, живых не осталось.
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Canceled as i32);
    let leader = fs::read_to_string(dir.join("pid"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(session_ids(leader), Vec::<i32>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn log_is_capped_on_disk_and_the_upload_has_head_marker_and_tail() {
    let config = JobsConfig {
        log_max_mib: 1,
        ..Default::default()
    };
    let harness = Harness::with(config, None, WORKER).await;
    let id = harness
        .run("echo HEAD-LINE; head -c 6000000 /dev/zero | tr '\\0' 'x'; echo; echo TAIL-LINE")
        .await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert!(finished.truncated);
    assert_eq!(finished.log_bytes, 10 + 6_000_000 + 1 + 10);
    assert!(finished.tail.ends_with(b"TAIL-LINE\n"));
    assert!(finished.tail.len() <= 32 * 1024);
    let on_disk: u64 = fs::read_dir(harness.attempt_dir(&id))
        .unwrap()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == "log.head" || name.starts_with("log.t.")
        })
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert!(on_disk <= 1024 * 1024, "{on_disk}");
    harness.uploaded().await;
    let text = gunzip(&harness.sink.uploads.lock().unwrap()[0].1);
    assert!(text.starts_with("HEAD-LINE\n"));
    assert!(text.contains("bytes omitted"));
    assert!(text.ends_with("TAIL-LINE\n"));
    assert!(text.len() < 1024 * 1024 + 100);
}

/// Держатель аренды: настоящая обёртка `lease run --strict`, которая ждёт слот сколько нужно,
/// поэтому сокет сервера может подняться позже неё.
struct HeldLease {
    child: std::process::Child,
    go: PathBuf,
}

impl HeldLease {
    fn take(kind: &str, socket: &Path, dir: &Path) -> Self {
        let go = dir.join("holder.go");
        let script = format!("while [ ! -e {go:?} ]; do sleep 0.05; done", go = go);
        let mut command = Process::new(WORKER);
        command
            .args([
                "lease", "run", "--kind", kind, "--strict", "--", "sh", "-c", &script,
            ])
            .env(LEASE_SOCKET_ENV, socket)
            .env_remove("CANTERLOT_LEASE_HELD")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self {
            child: command.spawn().unwrap(),
            go,
        }
    }

    fn release(&mut self) {
        fs::write(&self.go, b"").unwrap();
        let _ = self.child.wait();
    }
}

impl Drop for HeldLease {
    fn drop(&mut self) {
        let _ = fs::write(&self.go, b"");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn snapshot_until(
    server: &LeaseServer,
    what: &str,
    mut check: impl FnMut(&canterlot_worker::lease::Snapshot) -> bool,
) {
    let mut snapshot = server.snapshot();
    tokio::time::timeout(WAIT, snapshot.wait_for(|snapshot| check(snapshot)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_lease_waits_behind_a_held_gate_and_never_runs_unleased() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("data")).unwrap();
    fs::create_dir(root.path().join("runtime")).unwrap();
    let (stop, _) = watch::channel(false);
    let config = ServerConfig::new(
        Some(&root.path().join("runtime")),
        &root.path().join("data"),
        2,
    );
    let socket = config.socket.clone();
    let server = LeaseServer::start(config, stop.subscribe());

    let harness = Harness::with(JobsConfig::default(), Some(socket.clone()), WORKER).await;
    let mut gate = HeldLease::take("gate", &socket, root.path());
    snapshot_until(&server, "the gate to be held", |s| s.holders.len() == 1).await;

    let marker = harness.work().join("ran");
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, &format!("touch {}; echo leased", marker.display()));
    start.lease = JobLease::Build as i32;
    harness.start(start).await;

    // Попытка стоит в очереди сервера за гейтом: слот ей не выдан, команда не запущена,
    // запуска вне аренды нет.
    snapshot_until(&server, "the job to queue behind the gate", |s| {
        s.waiters.len() == 1
    })
    .await;
    harness
        .wait("waiting_lease fact", || {
            harness.sink.kinds(&id).contains(&"waiting_lease")
        })
        .await;
    assert!(!marker.exists(), "no unleased fallback behind a gate");
    assert!(!harness.sink.kinds(&id).contains(&"started"));
    assert!(harness.sink.finished(&id).is_none());
    assert_eq!(server.snapshot().borrow().holders.len(), 1);

    gate.release();
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert_eq!(finished.tail, b"leased\n");
    assert!(marker.exists());
    assert_eq!(
        harness.sink.kinds(&id),
        ["dispatched", "waiting_lease", "started", "finished"]
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_lease_without_a_worker_socket_keeps_waiting_instead_of_running() {
    let nowhere = tempfile::tempdir().unwrap();
    let harness = Harness::with(
        JobsConfig::default(),
        Some(nowhere.path().join("missing.sock")),
        WORKER,
    )
    .await;
    let marker = harness.work().join("ran");
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, &format!("touch {}", marker.display()));
    start.lease = JobLease::Gate as i32;
    harness.start(start).await;
    // Оболочка сама сообщает, что воркера нет и она продолжает ждать.
    let shim_log = harness.attempt_dir(&id).join("shim.log");
    harness
        .wait("the shim to report the missing socket", || {
            fs::read_to_string(&shim_log).is_ok_and(|log| log.contains("still waiting"))
        })
        .await;
    assert!(!marker.exists());
    assert_eq!(harness.sink.kinds(&id), ["dispatched", "waiting_lease"]);
    // Отмена в ожидании снимает попытку, не запустив команду.
    harness.cancel(&id).await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Canceled as i32);
    assert!(!marker.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn sweep_removes_only_the_expired_attempt_directory() {
    let harness = Harness::new().await;
    let old = harness.run("echo old").await;
    harness.finished(&old).await;
    harness.uploaded().await;
    let fresh = harness.run("echo fresh").await;
    harness.finished(&fresh).await;
    harness
        .wait("both logs uploaded", || {
            harness.sink.uploads.lock().unwrap().len() == 2
        })
        .await;
    // Соседняя незавершённая попытка той же задачи и посторонние файлы остаются.
    let sibling = harness.data.path().join("jobs").join(&old).join("2");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("pid"), "1 1\n").unwrap();
    let stranger = harness.data.path().join("jobs").join("stranger.txt");
    fs::write(&stranger, "keep").unwrap();
    let workfile = harness.work().join("data.txt");
    fs::write(&workfile, "keep").unwrap();

    // Срок ещё не вышел: ничего не убрано.
    let now = canterlot_worker::jobs::state::now_ms();
    assert_eq!(harness.manager.sweep(now).await, 0);
    assert!(harness.attempt_dir(&old).exists());

    // Через три дня и час: убирается только каталог завершённой попытки каждой задачи.
    let later = now + (3 * 24 + 1) * 3600 * 1000;
    assert_eq!(harness.manager.sweep(later).await, 2);
    assert!(!harness.attempt_dir(&old).exists());
    assert!(!harness.attempt_dir(&fresh).exists());
    assert!(sibling.exists(), "unfinished sibling attempt stays");
    assert!(
        harness.data.path().join("jobs").join(&old).exists(),
        "job directory with a live attempt stays"
    );
    assert!(
        !harness.data.path().join("jobs").join(&fresh).exists(),
        "an emptied job directory goes with its last attempt"
    );
    assert!(stranger.exists());
    assert!(workfile.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn facts_are_written_only_while_the_capability_is_negotiated() {
    let harness = Harness::new().await;
    harness.manager.set_enabled(false);
    // Без согласованной возможности команда не принимается и состояния не создаёт.
    let id = Uuid::now_v7().to_string();
    let refused = harness
        .manager
        .start(&Harness::envelope(Kind::StartInPlaceJob(
            harness.start_command(&id, "echo never"),
        )))
        .await;
    assert!(matches!(refused, Err(JobError::NotNegotiated)));
    assert!(harness.sink.facts().is_empty());
    assert!(!harness.data.path().join("jobs").join(&id).exists());

    // Задача, начатая на согласованном соединении, доживает разрыв без записи фактов:
    // итог ждёт нового согласованного соединения и отчёта.
    harness.manager.set_enabled(true);
    let mut chunks = harness.manager.subscribe_chunks();
    let live = harness.run("echo up; sleep 1; echo down").await;
    Harness::output_has(&mut chunks, "up").await;
    harness
        .wait("started fact", || {
            harness.sink.kinds(&live).contains(&"started")
        })
        .await;
    harness.manager.set_enabled(false);
    let seen = harness.sink.facts().len();
    // Задача заканчивается сама, пока соединения нет: итог лежит на диске, а не в журнале.
    harness
        .wait("the job to finish on disk", || {
            fs::read_to_string(harness.attempt_dir(&live).join("result.json")).is_ok()
        })
        .await;
    assert_eq!(
        harness.sink.facts().len(),
        seen,
        "no job facts on a connection without the capability"
    );
    harness.manager.set_enabled(true);
    let finished = harness.finished(&live).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_report_lists_every_known_job_with_its_last_state() {
    let harness = Harness::new().await;
    let done = harness.run("echo ok").await;
    harness.finished(&done).await;
    let failed = harness.run("exit 5").await;
    harness.finished(&failed).await;
    let running = harness.run("sleep 30").await;
    harness
        .wait("started fact", || {
            harness.sink.kinds(&running).contains(&"started")
        })
        .await;
    // Попытка, у которой воркер умер до запуска оболочки, в отчёте ещё «принята».
    let claimed = Uuid::now_v7().to_string();
    let dir = harness.data.path().join("jobs").join(&claimed).join("1");
    fs::create_dir_all(&dir).unwrap();

    let report = harness.manager.report().await;
    let find = |id: &str| report.jobs.iter().find(|job| job.job_id == id);
    let done = find(&done).unwrap();
    assert_eq!(done.phase, JobPhase::Finished as i32);
    assert_eq!(done.outcome, JobOutcome::Succeeded as i32);
    assert_eq!(done.exit_code, Some(0));
    assert_eq!(done.log_bytes, 3);
    let failed = find(&failed).unwrap();
    assert_eq!(failed.outcome, JobOutcome::Failed as i32);
    assert_eq!(failed.exit_code, Some(5));
    let running = find(&running).unwrap();
    assert_eq!(running.phase, JobPhase::Running as i32);
    assert!(running.pid > 0 && running.start_time > 0 && running.started_at_ms > 0);
    assert_eq!(running.outcome, JobOutcome::Unspecified as i32);
    let claimed = find(&claimed).unwrap();
    assert_eq!(claimed.phase, JobPhase::Dispatched as i32);
    assert_eq!(report.jobs.len(), 4);
    assert!(report.observed_at_ms > 0);
    harness.cancel(&running.job_id.clone()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_output_arrives_as_ordered_rate_limited_chunks() {
    let harness = Harness::new().await;
    let mut chunks = harness.manager.subscribe_chunks();
    let id = harness
        .run("for i in 1 2 3 4 5 6; do echo line-$i; sleep 0.2; done")
        .await;
    harness.finished(&id).await;
    let mut seen = Vec::new();
    let mut position = 0;
    while let Ok(chunk) = chunks.try_recv() {
        assert_eq!(chunk.job_id, id);
        assert_eq!(chunk.attempt, 1);
        assert!(chunk.offset >= position, "chunks never go backwards");
        assert!(chunk.data.len() <= 16 * 1024);
        position = chunk.offset + chunk.data.len() as u64;
        seen.extend(chunk.data);
    }
    let text = String::from_utf8(seen).unwrap();
    assert!(text.starts_with("line-1\n"), "{text}");
    assert!(text.contains("line-3\n"));
}
