//! Задачи на месте на настоящих процессах: оболочка попытки (`canterlot-worker job-shim`),
//! каталоги состояния во временных каталогах и временный HOME.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
use tokio::sync::watch;
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

    fn work(&self) -> PathBuf {
        self.work.path().canonicalize().unwrap()
    }

    /// Новый менеджер над теми же каталогами: так выглядит перезапуск воркера.
    async fn restart(&mut self) {
        let _ = self.stop.send(true);
        tokio::time::sleep(Duration::from_millis(150)).await;
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
        wait_for("JobFinished", || self.sink.finished(job_id).is_some()).await;
        self.sink.finished(job_id).unwrap()
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

fn kill_session(session: i32) {
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
        if fields.get(3).and_then(|sid| sid.parse::<i32>().ok()) == Some(session)
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

fn session_members(session: i32) -> Vec<i32> {
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

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
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
    wait_for("log upload", || {
        !harness.sink.uploads.lock().unwrap().is_empty()
    })
    .await;
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!marker.exists(), "a cancelled job must not start");
    assert!(!harness.sink.kinds(&id).contains(&"started"));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_the_run_is_confirmed_only_after_the_group_is_gone() {
    let harness = Harness::new().await;
    let pids = harness.work().join("pids");
    // Внуки в своей группе и сессии процесса: confirm не должен опередить их смерть.
    let id = harness
        .run(&format!(
            "sleep 300 & echo $! >> {pids}; setsid sleep 300 & echo started; wait",
            pids = pids.display()
        ))
        .await;
    wait_for("started", || {
        harness.sink.kinds(&id).contains(&"started") && pids.exists()
    })
    .await;
    let leader = fs::read_to_string(harness.attempt_dir(&id).join("pid"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(!session_members(leader).is_empty());
    harness.cancel(&id).await;
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Canceled as i32);
    // К моменту факта группа уже исчезла.
    assert_eq!(session_members(leader), Vec::<i32>::new());
    let sleeper = fs::read_to_string(&pids)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert!(
        !Path::new(&format!("/proc/{sleeper}")).exists()
            || fs::read_to_string(format!("/proc/{sleeper}/stat")).is_ok_and(|s| s.contains(") Z")),
        "the background child is gone"
    );
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

    // Тот же исход, когда оболочка стартовала, но не дошла до `pid`.
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
    let counter = harness.work().join("launches");
    let id = harness
        .run(&format!(
            "echo x >> {counter}; sleep 2; echo end",
            counter = counter.display()
        ))
        .await;
    wait_for("started", || harness.sink.kinds(&id).contains(&"started")).await;
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
    assert_eq!(finished.tail, b"end\n");
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
        // не абсолютный, не каталог, нет такого
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
    wait_for("log upload", || {
        !harness.sink.uploads.lock().unwrap().is_empty()
    })
    .await;
    let text = gunzip(&harness.sink.uploads.lock().unwrap()[0].1);
    assert!(text.starts_with("HEAD-LINE\n"));
    assert!(text.contains("bytes omitted"));
    assert!(text.ends_with("TAIL-LINE\n"));
    assert!(text.len() < 1024 * 1024 + 100);
}

struct HeldLease {
    child: std::process::Child,
    go: PathBuf,
    held: PathBuf,
}

impl HeldLease {
    fn take(kind: &str, socket: &Path, dir: &Path) -> Self {
        let go = dir.join("holder.go");
        let held = dir.join("holder.held");
        let script = format!(
            "touch {held:?}; while [ ! -e {go:?} ]; do sleep 0.05; done",
            held = held,
            go = go
        );
        let mut command = Process::new(WORKER);
        command
            .args(["lease", "run", "--kind", kind, "--", "sh", "-c", &script])
            .env(LEASE_SOCKET_ENV, socket)
            .env_remove("CANTERLOT_LEASE_HELD")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self {
            child: command.spawn().unwrap(),
            go,
            held,
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
    wait_for("lease socket", || {
        std::os::unix::net::UnixStream::connect(&socket).is_ok()
    })
    .await;

    let harness = Harness::with(JobsConfig::default(), Some(socket.clone()), WORKER).await;
    let mut gate = HeldLease::take("gate", &socket, root.path());
    wait_for("gate held", || gate.held.exists()).await;

    let marker = harness.work().join("ran");
    let id = Uuid::now_v7().to_string();
    let mut start = harness.start_command(&id, &format!("touch {}; echo leased", marker.display()));
    start.lease = JobLease::Build as i32;
    harness.start(start).await;

    wait_for("waiting_lease fact", || {
        harness.sink.kinds(&id).contains(&"waiting_lease")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!marker.exists(), "no unleased fallback behind a gate");
    assert!(!harness.sink.kinds(&id).contains(&"started"));
    assert!(harness.sink.finished(&id).is_none());

    gate.release();
    let finished = harness.finished(&id).await;
    assert_eq!(finished.outcome, JobOutcome::Succeeded as i32);
    assert_eq!(finished.tail, b"leased\n");
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
    tokio::time::sleep(Duration::from_millis(1500)).await;
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
    wait_for("log upload", || {
        !harness.sink.uploads.lock().unwrap().is_empty()
    })
    .await;
    let fresh = harness.run("echo fresh").await;
    harness.finished(&fresh).await;
    // Соседняя незавершённая попытка той же задачи и посторонние файлы остаются.
    let sibling = harness.data.path().join("jobs").join(&old).join("2");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("pid"), "1 1\n").unwrap();
    let stranger = harness.data.path().join("jobs").join("stranger.txt");
    fs::write(&stranger, "keep").unwrap();
    let workfile = harness.work().join("data.txt");
    fs::write(&workfile, "keep").unwrap();

    // Срок ещё не вышел: ничего не убрано.
    assert_eq!(harness.manager.sweep(canterlot_worker_now()).await, 0);
    assert!(harness.attempt_dir(&old).exists());

    // Через три дня и час: убирается только каталог завершённой попытки каждой задачи.
    let later = canterlot_worker_now() + (3 * 24 + 1) * 3600 * 1000;
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

fn canterlot_worker_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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
    let live = harness.run("sleep 1; echo done").await;
    wait_for("started", || harness.sink.kinds(&live).contains(&"started")).await;
    harness.manager.set_enabled(false);
    let seen = harness.sink.facts().len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
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
    wait_for("started", || {
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
