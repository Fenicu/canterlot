//! Носитель раннеров против настоящего непривилегированного `sshd` и настоящего бинарника
//! воркера. Хаб — тестовый (`ProtocolHub`): говорит по TLS на loopback, принимает токен
//! носителя и показывает, кто к нему пришёл. Обмен носителя с хабом — подмена `RunnerControl`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    Command as HubCommand, ControlDown, JobLease, StartInPlaceJob, command, control_down,
};
use canterlot_worker::{
    carrier::{
        self, AssignedRunner, CarrierSettings, Observation, RunnerControl, RunnerCredentials,
        SecretToken, Timings, TokenError,
    },
    config::RunnerSsh,
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    sshd::{ALIAS, TestSshd},
    support::{Behavior, Observed, ProtocolHub},
};

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");
const WAIT: Duration = Duration::from_secs(40);
const RUNNER: &str = "runner-a";

fn test_timings() -> Timings {
    Timings {
        liveness: Duration::from_millis(300),
        tunnel_settle: Duration::from_millis(400),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(800),
        observe_every: Duration::from_millis(500),
        token_retry_min: Duration::from_secs(2),
        token_retry_max: Duration::from_secs(4),
        ssh_limit: Duration::from_secs(20),
        upload_limit: Duration::from_secs(60),
    }
}

struct FakeControl {
    machine_id: Uuid,
    installation_id: Uuid,
    restore_generation: Uuid,
    token: String,
    requests: Mutex<Vec<RecordedRequest>>,
    observations: Mutex<Vec<Observation>>,
    refuse: Mutex<bool>,
}

struct RecordedRequest {
    machine_id: Uuid,
    reset: bool,
    at: std::time::Instant,
}

impl FakeControl {
    fn requests(&self) -> Vec<(Uuid, bool)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|recorded| (recorded.machine_id, recorded.reset))
            .collect()
    }

    fn request_times(&self) -> Vec<std::time::Instant> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|recorded| recorded.at)
            .collect()
    }

    fn observations(&self) -> Vec<Observation> {
        self.observations.lock().unwrap().clone()
    }
}

#[async_trait]
impl RunnerControl for FakeControl {
    async fn request_token(
        &self,
        machine_id: Uuid,
        reset_local_state: bool,
    ) -> Result<RunnerCredentials, TokenError> {
        self.requests.lock().unwrap().push(RecordedRequest {
            machine_id,
            reset: reset_local_state,
            at: std::time::Instant::now(),
        });
        WAKE.notify_waiters();
        if *self.refuse.lock().unwrap() {
            return Err(TokenError::Refused(carrier::TokenRefusal::Revoked));
        }
        Ok(RunnerCredentials {
            machine_id: self.machine_id,
            installation_id: self.installation_id,
            restore_generation: self.restore_generation,
            token: SecretToken::new(self.token.clone()),
        })
    }

    async fn report_observation(&self, observation: Observation) {
        self.observations.lock().unwrap().push(observation);
        WAKE.notify_waiters();
    }
}

/// Что тестовый хаб слышал от воркеров раннера.
#[derive(Default)]
struct HubLog {
    control_hellos: AtomicU32,
    machine_ids: Mutex<Vec<String>>,
}

struct Fixture {
    sshd: TestSshd,
    hub: ProtocolHub,
    log: Arc<HubLog>,
    root: PathBuf,
    control: Arc<FakeControl>,
    assign: watch::Sender<Vec<AssignedRunner>>,
    stop: watch::Sender<bool>,
    join: Option<JoinHandle<()>>,
    drain: JoinHandle<()>,
    job_pids: Mutex<Vec<i32>>,
}

struct Options {
    behavior: Behavior,
    worker_binary: Option<PathBuf>,
    assigned: bool,
    /// Хаб отказывает в токене с кодом `revoked`.
    refuse: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            behavior: Behavior::default(),
            worker_binary: None,
            assigned: true,
            refuse: false,
        }
    }
}

impl Fixture {
    async fn start(options: Options) -> Self {
        let Options {
            mut behavior,
            worker_binary,
            assigned,
            refuse,
        } = options;
        let token = format!("carrier-secret-{}", Uuid::now_v7());
        behavior.token = token.clone();
        behavior.capabilities = vec![
            "exec_v1".into(),
            "pilot_v1".into(),
            "jobs_inplace_v1".into(),
        ];
        let (mut hub, store) = ProtocolHub::start(behavior).await;
        drop(store);
        let sshd = TestSshd::start();
        let root = sshd.path().join(RUNNER);
        let control = Arc::new(FakeControl {
            machine_id: Uuid::now_v7(),
            installation_id: hub.config.installation_id,
            restore_generation: hub.config.restore_generation,
            token,
            requests: Mutex::default(),
            observations: Mutex::default(),
            refuse: Mutex::new(refuse),
        });
        let log = Arc::new(HubLog::default());
        let mut events = std::mem::replace(&mut hub.events, tokio::sync::mpsc::channel(1).1);
        let drain = tokio::spawn({
            let log = log.clone();
            async move {
                while let Some(event) = events.recv().await {
                    if let Observed::Hello(hello, _) = event
                        && hello.stream_kind == 1
                    {
                        log.machine_ids.lock().unwrap().push(hello.machine_id);
                        log.control_hellos.fetch_add(1, Ordering::SeqCst);
                        WAKE.notify_waiters();
                    }
                }
            }
        });
        let settings = CarrierSettings {
            hub_url: hub.config.hub_url.clone(),
            ca_cert: hub.config.ca_cert.clone(),
            ssh_config: Some(sshd.ssh_config.clone()),
            worker_binary: Some(worker_binary.unwrap_or_else(|| PathBuf::from(WORKER))),
            worker_sha256: None,
            entries: vec![RunnerSsh {
                machine: RUNNER.into(),
                ssh: ALIAS.into(),
                max_running: 3,
                lease_slots: 2,
            }],
            timings: test_timings(),
            egress_port: crate::support::free_port(),
            cloud: None,
            facts: None,
        };
        let (assign, assigned_rx) = watch::channel(if assigned {
            vec![AssignedRunner {
                machine_id: control.machine_id,
                name: RUNNER.into(),
                root_dir: root.clone(),
            }]
        } else {
            Vec::new()
        });
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(carrier::run(
            settings,
            control.clone(),
            assigned_rx,
            shutdown,
        ));
        Self {
            sshd,
            hub,
            log,
            root,
            control,
            assign,
            stop,
            join: Some(join),
            drain,
            job_pids: Mutex::default(),
        }
    }

    fn assigned(&self) -> Vec<AssignedRunner> {
        vec![AssignedRunner {
            machine_id: self.control.machine_id,
            name: RUNNER.into(),
            root_dir: self.root.clone(),
        }]
    }

    fn hellos(&self) -> u32 {
        self.log.control_hellos.load(Ordering::SeqCst)
    }

    async fn wait_hellos(&self, at_least: u32) {
        until("a worker hello at the hub", || self.hellos() >= at_least).await;
    }

    fn worker_pid(&self) -> Option<i32> {
        let text = fs::read_to_string(self.root.join("state/worker.pid")).ok()?;
        text.split_whitespace().next()?.parse().ok()
    }

    fn worker_alive(&self) -> bool {
        self.worker_pid().is_some_and(pid_alive)
    }

    /// Останавливает носитель; воркер раннера остаётся жить, как и задумано.
    async fn shutdown_carrier(&mut self) {
        self.stop.send(true).unwrap();
        if let Some(join) = self.join.take() {
            tokio::time::timeout(Duration::from_secs(30), join)
                .await
                .expect("carrier stops")
                .unwrap();
        }
    }

    async fn send_job(&self, id: &str, shell: &str) {
        let workdir = self.root.join("jobs/personal");
        self.hub
            .send(ControlDown {
                msg: Some(control_down::Msg::Command(HubCommand {
                    command_id: Uuid::now_v7().to_string(),
                    dispatch_attempt: 1,
                    kind: Some(command::Kind::StartInPlaceJob(StartInPlaceJob {
                        job_id: id.into(),
                        attempt: 1,
                        workdir: workdir.display().to_string(),
                        shell: shell.into(),
                        env: vec![],
                        timeout_seconds: 120,
                        lease: JobLease::None as i32,
                    })),
                    ..Default::default()
                })),
            })
            .await;
    }

    /// Запускает задачу `sleep` и ждёт, пока она запишет свой pid.
    async fn start_sleeping_job(&self) -> i32 {
        let marker = self.root.join("jobs/personal/job.pid");
        let shell = format!(
            "echo $$ > {}.tmp && mv {0}.tmp {0}; exec sleep 300",
            marker.display()
        );
        self.send_job(&Uuid::now_v7().to_string(), &shell).await;
        until("the job to record its pid", || marker.exists()).await;
        let pid = fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        self.job_pids.lock().unwrap().push(pid);
        pid
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Всё, что запустил этот тест на раннере: воркер и задачи по записанным pid.
        let mut pids: Vec<i32> = self.job_pids.lock().unwrap().clone();
        pids.extend(self.worker_pid());
        for pid in pids {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        let root = self.root.display().to_string();
        let _ = Command::new("pkill")
            .args(["-f", &format!("{root}/hub.sock")])
            .status();
        self.drain.abort();
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

/// Будит ожидающих при каждом событии носителя или хаба: наблюдение, запрос токена, `Hello`.
/// Носитель сам просыпается не реже `observe_every`, поэтому состояние раннера на диске
/// перепроверяется без таймеров в тестах.
static WAKE: Notify = Notify::const_new();

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    tokio::time::timeout(WAIT, async {
        loop {
            let notified = WAKE.notified();
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

fn pid_alive(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| !rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

/// Живые процессы `ssh -N`, держащие проброс этого раннера.
fn forward_pids(root: &Path) -> Vec<i32> {
    let pattern = format!("-N .*{}/hub.sock", root.display());
    let output = Command::new("pgrep")
        .args(["-f", "--", &pattern])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn forward_install_token_and_config_bring_a_runner_worker_to_the_hub() {
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    // Хаб увидел воркера под идентификатором из ответа на запрос токена.
    assert_eq!(
        fixture.log.machine_ids.lock().unwrap()[0],
        fixture.control.machine_id.to_string()
    );
    let root = &fixture.root;
    for dir in [
        "bin",
        "jobs/personal",
        "jobs/work",
        "repos/personal",
        "repos/work",
        "state",
    ] {
        assert!(root.join(dir).is_dir(), "{dir}");
    }
    assert_eq!(mode(&root.join("state")), 0o700);
    assert_eq!(mode(&root.join("bin/canterlot-worker")) & 0o111, 0o111);
    assert_eq!(
        fs::read(root.join("bin/canterlot-worker")).unwrap(),
        fs::read(WORKER).unwrap()
    );
    assert_eq!(
        fs::read(root.join("ca.pem")).unwrap(),
        fs::read(&fixture.hub.config.ca_cert).unwrap()
    );
    assert_eq!(mode(&root.join("token")), 0o600);
    let config: canterlot_worker::config::WorkerConfig =
        toml::from_str(&fs::read_to_string(root.join("worker.toml")).unwrap()).unwrap();
    assert_eq!(
        config.hub_dial.as_deref(),
        Some(format!("unix:{}/hub.sock", root.display()).as_str())
    );
    assert_eq!(config.machine_id, fixture.control.machine_id);
    assert_eq!(config.jobs.max_running, 3);
    assert_eq!(config.machine.lease_slots, 2);
    assert!(config.claude_code.is_none() && config.codex.is_none() && config.opencode.is_none());
    // Токен запрошен один раз и со сбросом: каталога состояния ещё не было.
    assert_eq!(
        fixture.control.requests(),
        vec![(fixture.control.machine_id, true)]
    );
    // Достижимость: после успешных попыток хаб слышит «достижим, ни одной неудачи».
    let last = fixture.control.observations().pop().expect("observation");
    assert!(last.reachable && last.consecutive_failures == 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_socket_left_on_the_runner_is_replaced_before_the_forward() {
    let fixture = Fixture::start(Options {
        assigned: false,
        ..Default::default()
    })
    .await;
    // Сокет прежнего проброса: файл остаётся, слушателя нет.
    fs::create_dir_all(&fixture.root).unwrap();
    drop(std::os::unix::net::UnixListener::bind(fixture.root.join("hub.sock")).unwrap());
    assert!(fixture.root.join("hub.sock").exists());
    fixture.assign.send(fixture.assigned()).unwrap();
    fixture.wait_hellos(1).await;
    assert!(!forward_pids(&fixture.root).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn updating_the_binary_restarts_the_worker_and_a_running_job_survives() {
    // Тот же бинарник с лишним хвостом: другой хэш, та же версия.
    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let newer = scratch.path().join("canterlot-worker");
    let mut bytes = fs::read(WORKER).unwrap();
    bytes.extend_from_slice(b"\0carrier-update-marker");
    fs::write(&newer, bytes).unwrap();
    fs::set_permissions(&newer, fs::Permissions::from_mode(0o755)).unwrap();

    let mut fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let job_pid = fixture.start_sleeping_job().await;
    let worker_before = fixture.worker_pid().unwrap();

    // Носитель с новым бинарником подхватывает раннер у прежнего.
    fixture.shutdown_carrier().await;
    assert!(pid_alive(job_pid) && pid_alive(worker_before));
    let (stop, shutdown) = watch::channel(false);
    let settings = CarrierSettings {
        hub_url: fixture.hub.config.hub_url.clone(),
        ca_cert: fixture.hub.config.ca_cert.clone(),
        ssh_config: Some(fixture.sshd.ssh_config.clone()),
        worker_binary: Some(newer.clone()),
        worker_sha256: None,
        entries: vec![RunnerSsh {
            machine: RUNNER.into(),
            ssh: ALIAS.into(),
            max_running: 3,
            lease_slots: 2,
        }],
        timings: test_timings(),
        egress_port: crate::support::free_port(),
        cloud: None,
        facts: None,
    };
    let (_assign, assigned) = watch::channel(fixture.assigned());
    let join = tokio::spawn(carrier::run(
        settings,
        fixture.control.clone(),
        assigned,
        shutdown,
    ));
    until("the runner binary to be replaced", || {
        fs::read(fixture.root.join("bin/canterlot-worker"))
            .map(|bytes| bytes.ends_with(b"carrier-update-marker"))
            .unwrap_or(false)
    })
    .await;
    until("a new worker process", || {
        fixture
            .worker_pid()
            .is_some_and(|pid| pid != worker_before && pid_alive(pid))
    })
    .await;
    assert!(!pid_alive(worker_before), "the old worker must be gone");
    assert!(pid_alive(job_pid), "the job must survive the update");
    fixture.wait_hellos(2).await;
    // Токен не запрашивался заново: обновление не сбрасывает учётные данные.
    assert_eq!(fixture.control.requests().len(), 1);
    stop.send(true).unwrap();
    join.await.unwrap();
}

// Один поток: подписчик журнала действует только на своём потоке, а задачи носителя должны быть на нём.
#[tokio::test(flavor = "current_thread")]
async fn a_cut_binary_upload_keeps_the_old_binary_and_the_next_round_retries() {
    let (writer, captured) = LogCapture::new();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(writer)
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    );
    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let newer = scratch.path().join("canterlot-worker");
    let mut bytes = fs::read(WORKER).unwrap();
    bytes.extend_from_slice(b"\0carrier-cut-marker");
    fs::write(&newer, &bytes).unwrap();
    fs::set_permissions(&newer, fs::Permissions::from_mode(0o755)).unwrap();

    let mut fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let original = fs::read(fixture.root.join("bin/canterlot-worker")).unwrap();
    let worker_before = fixture.worker_pid().unwrap();
    fixture.shutdown_carrier().await;

    // Носитель объявляет хэш не того, что пойдёт по ssh: на раннер дойдёт «обрубок».
    let (stop, shutdown) = watch::channel(false);
    let settings = CarrierSettings {
        hub_url: fixture.hub.config.hub_url.clone(),
        ca_cert: fixture.hub.config.ca_cert.clone(),
        ssh_config: Some(fixture.sshd.ssh_config.clone()),
        worker_binary: Some(newer.clone()),
        worker_sha256: Some("0".repeat(64)),
        entries: vec![RunnerSsh {
            machine: RUNNER.into(),
            ssh: ALIAS.into(),
            max_running: 3,
            lease_slots: 2,
        }],
        timings: test_timings(),
        egress_port: crate::support::free_port(),
        cloud: None,
        facts: None,
    };
    let (_assign, assigned) = watch::channel(fixture.assigned());
    let join = tokio::spawn(carrier::run(
        settings,
        fixture.control.clone(),
        assigned,
        shutdown,
    ));
    let failures = || {
        String::from_utf8_lossy(&captured.lock().unwrap())
            .matches("runner binary upload failed")
            .count()
    };
    // Повтор не ждёт переподключения туннеля: отправка сверяется на каждом круге присмотра
    // (раз в 300 мс). Прежний код сбрасывал `need_full` до отправки, и неудач было бы столько,
    // сколько раз поднимался туннель (два-три).
    until("the upload to be retried round after round", || {
        failures() >= 5
    })
    .await;
    assert_eq!(
        fs::read(fixture.root.join("bin/canterlot-worker")).unwrap(),
        original,
        "the old binary must stay in place"
    );
    assert_eq!(fixture.worker_pid(), Some(worker_before));
    assert!(pid_alive(worker_before), "the worker must not be restarted");
    stop.send(true).unwrap();
    join.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_worker_is_restarted_and_reconnects() {
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let before = fixture.worker_pid().unwrap();
    Command::new("kill")
        .args(["-KILL", &before.to_string()])
        .status()
        .unwrap();
    until("the worker to be restarted", || {
        fixture
            .worker_pid()
            .is_some_and(|pid| pid != before && pid_alive(pid))
    })
    .await;
    fixture.wait_hellos(2).await;
    assert_eq!(
        fixture.control.requests().len(),
        1,
        "a plain crash restarts the worker without a new token"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fatal_exit_asks_for_a_token_once_per_pause_and_does_not_spin() {
    // Хаб отказывает в доверии: воркер каждый раз уходит со словом `unauthenticated`.
    let fixture = Fixture::start(Options {
        behavior: Behavior {
            status: Some((tonic::Code::Unauthenticated, "credential_revoked")),
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    until("the first fatal word", || {
        fs::read_to_string(fixture.root.join("state/fatal"))
            .map(|word| word == "unauthenticated")
            .unwrap_or(false)
    })
    .await;
    // Контракт — не больше одного запроса за паузу: пауза не короче `token_retry_min`,
    // растёт к `token_retry_max`, а два живых круга подряд сбрасывают её к минимуму.
    // Считать круги присмотра нельзя: под нагрузкой воркер умирает медленно, успевает
    // побыть живым и сбросить паузу, и третий запрос законно приходит раньше максимума.
    // Без паузы запрос шёл бы каждый круг (около 300 мс), поэтому зазоры не короче
    // минимума с запасом отличают паузу от вращения при любой скорости кругов.
    until("the second token request", || {
        fixture.control.requests().len() >= 2
    })
    .await;
    let at_second = fixture.control.observations().len();
    until("the third token request", || {
        fixture.control.requests().len() >= 3
    })
    .await;
    let requests = fixture.control.requests();
    let times = fixture.control.request_times();
    assert!(
        requests.len() >= 3 && times.len() >= 3,
        "the carrier keeps asking after every pause, got {requests:?}"
    );
    let floor = test_timings().token_retry_min - Duration::from_secs(1);
    let gaps: Vec<Duration> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert!(
        gaps.iter().all(|gap| *gap >= floor),
        "token requests must follow the pause, gaps were {gaps:?}"
    );
    assert!(requests[0].1, "the first request resets the new state dir");
    assert!(
        requests[1..].iter().all(|(_, reset)| !reset),
        "a revoked credential needs a new token, not a state reset: {requests:?}"
    );
    // Присмотр не стоял: между вторым и третьим запросом круги шли.
    assert!(
        fixture.control.observations().len() >= at_second + 5,
        "supervision rounds must continue during the pause"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_restore_generation_resets_local_state_with_a_new_token() {
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let pid = fixture.worker_pid().unwrap();
    Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    until("the first worker to die", || !pid_alive(pid)).await;
    // Подкладываем слово воркера и метку в состоянии: после сброса метки быть не должно.
    let sentinel = fixture.root.join("state/sentinel");
    fs::write(&sentinel, "old").unwrap();
    fs::write(fixture.root.join("state/fatal"), "restore_generation").unwrap();
    let before = fixture.hellos();
    until("the state reset and a new worker", || {
        !sentinel.exists() && fixture.hellos() > before
    })
    .await;
    let requests = fixture.control.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[1].1, "restore_generation resets local state");
    assert!(!fixture.root.join("state/fatal").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_config_fatal_rewrites_the_config_without_a_token_request() {
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let pid = fixture.worker_pid().unwrap();
    Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .unwrap();
    until("the worker to die", || !pid_alive(pid)).await;
    let config = fixture.root.join("worker.toml");
    let good = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{good}\nbroken = true\n")).unwrap();
    fs::write(fixture.root.join("state/fatal"), "config").unwrap();
    let before = fixture.hellos();
    until("a restart on the regenerated config", || {
        fixture.hellos() > before
    })
    .await;
    assert_eq!(fs::read_to_string(&config).unwrap(), good);
    assert_eq!(fixture.control.requests().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ssh_drop_reconnects_and_the_job_keeps_running() {
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let job_pid = fixture.start_sleeping_job().await;
    let worker = fixture.worker_pid().unwrap();
    let first = forward_pids(&fixture.root);
    assert!(!first.is_empty());
    for pid in &first {
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap();
    }
    until("a new forward", || {
        let now = forward_pids(&fixture.root);
        !now.is_empty() && now.iter().all(|pid| !first.contains(pid))
    })
    .await;
    // Воркер раннера переподключается через новый проброс; прежний процесс и задача целы.
    fixture.wait_hellos(2).await;
    assert_eq!(fixture.worker_pid(), Some(worker));
    assert!(pid_alive(worker) && pid_alive(job_pid));
}

// Один поток: подписчик журнала действует только на своём потоке, а задачи носителя должны быть на нём.
#[tokio::test(flavor = "current_thread")]
async fn the_token_never_reaches_local_disk_or_logs() {
    let (writer, captured) = LogCapture::new();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(writer)
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish(),
    );
    let fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let token = fixture.control.token.clone();
    // Токен лежит только в файле раннера, с правами 0600.
    assert_eq!(
        fs::read_to_string(fixture.root.join("token")).unwrap(),
        token
    );
    let mut leaks = Vec::new();
    scan(
        fixture.sshd.path(),
        &fixture.root.join("token"),
        &token,
        &mut leaks,
    );
    assert!(leaks.is_empty(), "token found in {leaks:?}");
    let hub_dir = fixture.hub.config.ca_cert.parent().unwrap().to_path_buf();
    scan(&hub_dir, &hub_dir.join("token"), &token, &mut leaks);
    assert!(leaks.is_empty(), "token found in {leaks:?}");
    let logs = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    assert!(!logs.is_empty(), "the carrier logs something");
    assert!(!logs.contains(&token), "token is in the carrier log");
    let debug = format!(
        "{:?}",
        RunnerCredentials {
            machine_id: Uuid::nil(),
            installation_id: Uuid::nil(),
            restore_generation: Uuid::nil(),
            token: SecretToken::new(token.clone()),
        }
    );
    assert!(!debug.contains(&token));
}

#[derive(Clone)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (Self(buffer.clone()), buffer)
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Файлы под `dir`, кроме `allowed`, в которых встречается `needle`.
fn scan(dir: &Path, allowed: &Path, needle: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            scan(&path, allowed, needle, found);
        } else if kind.is_file()
            && path != allowed
            && fs::read(&path)
                .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
                .unwrap_or(false)
        {
            found.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn removal_stops_the_worker_and_the_forward_but_shutdown_leaves_them() {
    let mut fixture = Fixture::start(Options::default()).await;
    fixture.wait_hellos(1).await;
    let worker = fixture.worker_pid().unwrap();
    assert!(!forward_pids(&fixture.root).is_empty());
    // Выход носителя ничего не останавливает на раннере, кроме проброса.
    fixture.shutdown_carrier().await;
    assert!(pid_alive(worker), "worker survives the carrier exit");
    assert!(
        forward_pids(&fixture.root).is_empty(),
        "the forward stops with the carrier"
    );
    // Запуск заново и снятие раннера из назначения.
    let (stop, shutdown) = watch::channel(false);
    let settings = CarrierSettings {
        hub_url: fixture.hub.config.hub_url.clone(),
        ca_cert: fixture.hub.config.ca_cert.clone(),
        ssh_config: Some(fixture.sshd.ssh_config.clone()),
        worker_binary: Some(PathBuf::from(WORKER)),
        worker_sha256: None,
        entries: vec![RunnerSsh {
            machine: RUNNER.into(),
            ssh: ALIAS.into(),
            max_running: 3,
            lease_slots: 2,
        }],
        timings: test_timings(),
        egress_port: crate::support::free_port(),
        cloud: None,
        facts: None,
    };
    let (assign, assigned) = watch::channel(fixture.assigned());
    let join = tokio::spawn(carrier::run(
        settings,
        fixture.control.clone(),
        assigned,
        shutdown,
    ));
    until("the forward to return", || {
        !forward_pids(&fixture.root).is_empty()
    })
    .await;
    assign.send(Vec::new()).unwrap();
    until("the worker to stop on removal", || !pid_alive(worker)).await;
    // Остановка завершена, когда носитель вернул управление: снятие раннера стоит до выхода.
    stop.send(true).unwrap();
    join.await.unwrap();
    assert!(forward_pids(&fixture.root).is_empty());
    // Снятый раннер не запущен заново.
    assert!(!fixture.worker_alive());
}

#[tokio::test(flavor = "multi_thread")]
async fn only_runners_named_in_both_config_and_assignment_are_managed() {
    let fixture = Fixture::start(Options {
        assigned: false,
        ..Default::default()
    })
    .await;
    // Назначен раннер, которого нет в конфигурации носителя, а настроенного пока нет в назначении.
    let stranger = Uuid::now_v7();
    let mut assigned = vec![AssignedRunner {
        machine_id: stranger,
        name: "runner-b".into(),
        root_dir: fixture.sshd.path().join("runner-b"),
    }];
    fixture.assign.send(assigned.clone()).unwrap();
    // Контроль: когда назначен и настроенный раннер, носитель им занимается. К этому моменту
    // чужой раннер уже учтён, а следов от него нет.
    assigned.extend(fixture.assigned());
    fixture.assign.send(assigned).unwrap();
    fixture.wait_hellos(1).await;
    assert!(
        fixture
            .control
            .observations()
            .iter()
            .all(|o| o.machine_id == fixture.control.machine_id)
    );
    assert!(
        fixture
            .control
            .requests()
            .iter()
            .all(|(id, _)| *id == fixture.control.machine_id)
    );
    assert!(!fixture.sshd.path().join("runner-b").exists());
}

/// Носитель с раннером, до которого по SSH не достучаться (порт закрыт).
async fn start_dead_runner(
    timings: Timings,
) -> (
    tempfile::TempDir,
    Arc<FakeControl>,
    watch::Sender<bool>,
    JoinHandle<()>,
) {
    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let dead_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = scratch.path().join("ssh_config");
    fs::write(
        &config,
        format!("Host dead\n  HostName 127.0.0.1\n  Port {dead_port}\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n"),
    )
    .unwrap();
    let ca = scratch.path().join("ca.pem");
    fs::write(&ca, "ca").unwrap();
    let control = Arc::new(FakeControl {
        machine_id: Uuid::now_v7(),
        installation_id: Uuid::now_v7(),
        restore_generation: Uuid::now_v7(),
        token: "unused".into(),
        requests: Mutex::default(),
        observations: Mutex::default(),
        refuse: Mutex::new(false),
    });
    // Назначение живёт столько же, сколько носитель: источник не закрывается.
    let (assign, assigned) = watch::channel(vec![AssignedRunner {
        machine_id: control.machine_id,
        name: RUNNER.into(),
        root_dir: scratch.path().join("runner"),
    }]);
    std::mem::forget(assign);
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(carrier::run(
        CarrierSettings {
            hub_url: "https://hub.example:8443".into(),
            ca_cert: ca,
            ssh_config: Some(config),
            worker_binary: Some(PathBuf::from(WORKER)),
            worker_sha256: None,
            entries: vec![RunnerSsh {
                machine: RUNNER.into(),
                ssh: "dead".into(),
                max_running: 1,
                lease_slots: 1,
            }],
            timings,
            egress_port: crate::support::free_port(),
            cloud: None,
            facts: None,
        },
        control.clone(),
        assigned,
        shutdown,
    ));
    (scratch, control, stop, join)
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_runner_reports_growing_consecutive_failures() {
    let (_scratch, control, stop, join) = start_dead_runner(test_timings()).await;
    until("three consecutive failures", || {
        control
            .observations()
            .iter()
            .any(|observation| observation.consecutive_failures >= 3)
    })
    .await;
    let seen = control.observations();
    assert!(
        seen.iter()
            .all(|o| !o.reachable && o.machine_id == control.machine_id)
    );
    let counts: Vec<u32> = seen.iter().map(|o| o.consecutive_failures).collect();
    assert!(counts.windows(2).all(|w| w[0] <= w[1]), "{counts:?}");
    assert_eq!(counts[0], 1);
    stop.send(true).unwrap();
    join.await.unwrap();
}

/// Условие, которое сам носитель событием не объявляет (остановка проброса после отказа):
/// проверяется по тику, пока не выполнится.
async fn until_polled(what: &str, mut check: impl FnMut() -> bool) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tokio::time::timeout(WAIT, async {
        loop {
            tick.tick().await;
            if check() {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_token_parks_the_runner_until_the_next_list() {
    let fixture = Fixture::start(Options {
        refuse: true,
        ..Default::default()
    })
    .await;
    until("the first token request", || {
        !fixture.control.requests().is_empty()
    })
    .await;
    // Хаб отказал: проброс остановлен, воркер раннера не запускался, повторов вплотную нет.
    until_polled("the forward to stop", || {
        forward_pids(&fixture.root).is_empty()
    })
    .await;
    assert_eq!(fixture.control.requests().len(), 1);
    assert!(!fixture.root.join("worker.toml").exists());
    // Следующий список от хаба (его присылают при каждом подключении) пробует снова.
    fixture.assign.send(fixture.assigned()).unwrap();
    until("the second token request", || {
        fixture.control.requests().len() >= 2
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_observation_is_repeated_without_new_ssh_attempts() {
    // Круг присмотра и повтор проброса отложены на час: после двух первых неудач попыток SSH
    // больше нет, а наблюдения идут, потому что хабу нужна свежесть для «offline».
    let (_scratch, control, stop, join) = start_dead_runner(Timings {
        liveness: Duration::from_secs(3600),
        backoff_min: Duration::from_secs(3600),
        backoff_max: Duration::from_secs(3600),
        ..test_timings()
    })
    .await;
    until("repeated observations", || {
        control.observations().len() >= 5
    })
    .await;
    let seen = control.observations();
    let tail = &seen[seen.len() - 3..];
    assert!(tail.iter().all(|o| !o.reachable));
    assert!(
        tail.windows(2)
            .all(|w| w[0].consecutive_failures == w[1].consecutive_failures
                && w[0].observed_at < w[1].observed_at),
        "{tail:?}"
    );
    stop.send(true).unwrap();
    join.await.unwrap();
}
