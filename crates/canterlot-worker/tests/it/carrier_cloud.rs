//! Подключение облачного раннера на носителе против настоящего `sshd` на случайном высоком
//! порту (вместо DNAT-порта роутера) и настоящего бинарника воркера. Хаб — тестовый,
//! токен раннера выдаётся подменой `RunnerControl`.

use std::{
    fs,
    path::PathBuf,
    process::Command as Process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    AttachRunner, Command as HubCommand, command, record_body::Body,
};
use canterlot_worker::{
    carrier::{
        self, AssignedRunner, CarrierSettings, Observation, RunnerControl, RunnerCredentials,
        SecretToken, Timings, TokenError,
        cloud::{AttachOutcome, CloudConfig, CloudState},
        commands::{CarrierCommands, CommandError, FactSink},
        stage::{RunnerTarget, Stager, StagerSettings},
    },
    owner_jobs::Store,
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    sshd::TestSshd,
    support::{Behavior, Observed, ProtocolHub},
};

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");
const WAIT: Duration = Duration::from_secs(40);

fn test_timings() -> Timings {
    Timings {
        liveness: Duration::from_millis(300),
        tunnel_settle: Duration::from_millis(400),
        backoff_min: Duration::from_millis(100),
        backoff_max: Duration::from_millis(600),
        observe_every: Duration::from_millis(500),
        token_retry_min: Duration::from_millis(500),
        token_retry_max: Duration::from_secs(2),
        ssh_limit: Duration::from_secs(20),
        upload_limit: Duration::from_secs(60),
    }
}

type Write = (Vec<Body>, Vec<(String, u32)>);

#[derive(Default)]
struct Sink {
    written: Mutex<Vec<Write>>,
    changed: Notify,
}

#[async_trait]
impl FactSink for Sink {
    async fn facts(&self, facts: Vec<Body>, terminal: Vec<(String, u32)>) -> Result<(), ()> {
        self.written.lock().unwrap().push((facts, terminal));
        self.changed.notify_waiters();
        Ok(())
    }
}

struct FakeControl {
    machine_id: Uuid,
    installation_id: Uuid,
    restore_generation: Uuid,
    token: String,
    requests: Mutex<Vec<(Uuid, bool)>>,
}

#[async_trait]
impl RunnerControl for FakeControl {
    async fn request_token(
        &self,
        machine_id: Uuid,
        reset_local_state: bool,
    ) -> Result<RunnerCredentials, TokenError> {
        self.requests
            .lock()
            .unwrap()
            .push((machine_id, reset_local_state));
        WAKE.notify_waiters();
        Ok(RunnerCredentials {
            machine_id: self.machine_id,
            installation_id: self.installation_id,
            restore_generation: self.restore_generation,
            token: SecretToken::new(self.token.clone()),
        })
    }

    async fn report_observation(&self, _observation: Observation) {
        WAKE.notify_waiters();
    }
}

#[derive(Default)]
struct HubLog {
    hellos: AtomicU32,
}

struct Fixture {
    sshd: TestSshd,
    hub: ProtocolHub,
    log: Arc<HubLog>,
    cloud: Arc<CloudState>,
    commands: Arc<CarrierCommands>,
    control: Arc<FakeControl>,
    assign: watch::Sender<Vec<AssignedRunner>>,
    stop: watch::Sender<bool>,
    join: Option<JoinHandle<()>>,
    drain: JoinHandle<()>,
    data: PathBuf,
    marker: PathBuf,
    egress: PathBuf,
    egress_port: u16,
    root: PathBuf,
    machine_id: Uuid,
    sink: Arc<Sink>,
    ready_timeout: Duration,
}

struct Options {
    assigned: bool,
    ready_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            assigned: true,
            ready_timeout: Duration::from_secs(4),
        }
    }
}

impl Fixture {
    async fn start(options: Options) -> Self {
        let token = format!("cloud-secret-{}", Uuid::now_v7());
        let behavior = Behavior {
            token: token.clone(),
            capabilities: vec![
                "exec_v1".into(),
                "pilot_v1".into(),
                "jobs_inplace_v1".into(),
            ],
            ..Behavior::default()
        };
        let (mut hub, store) = ProtocolHub::start(behavior.clone()).await;
        drop(store);
        let sshd = TestSshd::start();
        let data = tempfile::Builder::new()
            .prefix("cloud-data-")
            .tempdir_in("/tmp")
            .unwrap();
        let data = data.path().to_path_buf();
        let root = sshd.path().join("runner");
        let marker = sshd.path().join("ready");
        // Скрипт подготовки получает окружение выхода в сеть и проверяет, что проброс с
        // SOCKS уже поднят: файл указывает на порт обратного проброса.
        let egress_port = crate::support::free_port();
        let egress = sshd.path().join("egress.env");
        fs::write(&egress, format!("EGRESS_PORT={egress_port}\n")).unwrap();
        let machine_id = Uuid::now_v7();
        let control = Arc::new(FakeControl {
            machine_id,
            installation_id: hub.config.installation_id,
            restore_generation: hub.config.restore_generation,
            token: token.clone(),
            requests: Mutex::default(),
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
                        log.hellos.fetch_add(1, Ordering::SeqCst);
                        WAKE.notify_waiters();
                    }
                }
            }
        });
        let cloud_config = CloudConfig {
            data_dir: data.clone(),
            ssh_key: Some(sshd.client_key().to_path_buf()),
            user: sshd.user().to_owned(),
            ready_marker: marker.clone(),
            ready_timeout: options.ready_timeout,
            egress_env: egress.clone(),
            // `sshd` пускает обычным пользователем: боевой `sudo -n` в тесте не нужен.
            sudo: Some(String::new()),
        };
        let cloud = Arc::new(CloudState::open(cloud_config.clone()));
        let sink = Arc::new(Sink::default());
        let settings = CarrierSettings {
            hub_url: hub.config.hub_url.clone(),
            ca_cert: hub.config.ca_cert.clone(),
            ssh_config: None,
            worker_binary: Some(PathBuf::from(WORKER)),
            worker_sha256: None,
            entries: vec![],
            timings: test_timings(),
            egress_port,
            cloud: Some(cloud.clone()),
            facts: Some(sink.clone()),
        };
        let (assign, assigned_rx) = watch::channel(if options.assigned {
            vec![AssignedRunner {
                machine_id,
                name: "cloud".into(),
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
            shutdown.clone(),
        ));
        let stager = Stager::new(StagerSettings {
            ssh_config: Some(sshd.ssh_config.clone()),
            outputs_root: data.join("job-outputs"),
            targets: Arc::new(move |_| Option::<RunnerTarget>::None),
            ssh_limit: Duration::from_secs(20),
            push_limit: Duration::from_secs(60),
            allowed_roots: vec![PathBuf::from("/tmp")],
        });
        let store = Store::open(&data).unwrap();
        let commands = CarrierCommands::new(stager, store, sink.clone(), cloud.clone(), shutdown);
        commands.set_enabled(true);
        Self {
            sshd,
            hub,
            log,
            cloud,
            commands,
            control,
            assign,
            stop,
            join: Some(join),
            drain,
            data,
            marker,
            egress,
            egress_port,
            root,
            machine_id,
            sink,
            ready_timeout: options.ready_timeout,
        }
    }

    fn attach_command(&self, window: &str, key: String) -> HubCommand {
        self.attach_scripted(window, key, "")
    }

    fn attach_scripted(&self, window: &str, key: String, script: &str) -> HubCommand {
        HubCommand {
            command_id: window.to_owned(),
            dispatch_attempt: 1,
            kind: Some(command::Kind::AttachRunner(AttachRunner {
                window_id: window.to_owned(),
                machine_id: self.machine_id.to_string(),
                host: "127.0.0.1".into(),
                host_key_public: key,
                root_dir: self.root.display().to_string(),
                max_running: 3,
                lease_slots: 2,
                ssh_port: self.sshd.port() as u32,
                prepare_script: script.to_owned(),
            })),
            ..HubCommand::default()
        }
    }

    fn cloud_config(&self) -> CloudConfig {
        CloudConfig {
            data_dir: self.data.clone(),
            ssh_key: Some(self.sshd.client_key().to_path_buf()),
            user: self.sshd.user().to_owned(),
            ready_marker: self.marker.clone(),
            ready_timeout: self.ready_timeout,
            egress_env: self.egress.clone(),
            sudo: Some(String::new()),
        }
    }

    fn settings(&self, cloud: Arc<CloudState>) -> CarrierSettings {
        CarrierSettings {
            hub_url: self.hub.config.hub_url.clone(),
            ca_cert: self.hub.config.ca_cert.clone(),
            ssh_config: None,
            worker_binary: Some(PathBuf::from(WORKER)),
            worker_sha256: None,
            entries: vec![],
            timings: test_timings(),
            egress_port: self.egress_port,
            cloud: Some(cloud),
            facts: Some(self.sink.clone()),
        }
    }

    /// Новый носитель поверх того же каталога данных и того же сервера окна: так проверяется,
    /// что подготовка переживает рестарт.
    async fn restart(
        &mut self,
    ) -> (
        Arc<CloudState>,
        watch::Sender<Vec<AssignedRunner>>,
        watch::Sender<bool>,
        JoinHandle<()>,
    ) {
        self.shutdown_carrier().await;
        let cloud = Arc::new(CloudState::open(self.cloud_config()));
        let (assign, assigned) = watch::channel(Vec::new());
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(carrier::run(
            self.settings(cloud.clone()),
            self.control.clone(),
            assigned,
            shutdown,
        ));
        (cloud, assign, stop, join)
    }

    fn assign_runner(&self) -> Vec<AssignedRunner> {
        vec![AssignedRunner {
            machine_id: self.machine_id,
            name: "cloud".into(),
            root_dir: self.root.clone(),
        }]
    }

    /// Факты подготовки, которые носитель записал в приёмник.
    fn prepare_facts(&self) -> Vec<(String, String, i32, String)> {
        self.sink
            .written
            .lock()
            .unwrap()
            .iter()
            .flat_map(|(facts, _)| facts.clone())
            .filter_map(|body| match body {
                Body::RunnerPrepareFailed(fact) => Some((
                    fact.window_id,
                    fact.machine_id,
                    fact.exit_code,
                    fact.output_tail,
                )),
                _ => None,
            })
            .collect()
    }

    fn hello_count(&self) -> u32 {
        self.log.hellos.load(Ordering::SeqCst)
    }

    fn worker_pid(&self) -> Option<i32> {
        fs::read_to_string(self.root.join("state/worker.pid"))
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    fn tunnels(&self) -> usize {
        let needle = format!("{}/hub.sock", self.root.display());
        Process::new("pgrep")
            .args(["-fc", &needle])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0)
    }

    fn installed(&self) -> bool {
        self.root.join("bin/canterlot-worker").exists()
    }

    async fn shutdown_carrier(&mut self) {
        self.stop.send(true).unwrap();
        if let Some(join) = self.join.take() {
            tokio::time::timeout(Duration::from_secs(30), join)
                .await
                .expect("carrier stops")
                .unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(pid) = self.worker_pid() {
            let _ = Process::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        let needle = format!("{}/hub.sock", self.root.display());
        let _ = Process::new("pkill").args(["-f", &needle]).status();
        self.drain.abort();
        if let Some(join) = &self.join {
            join.abort();
        }
    }
}

static WAKE: Notify = Notify::const_new();

async fn until(what: &str, mut check: impl FnMut() -> bool) {
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    tokio::time::timeout(WAIT, async {
        loop {
            let notified = WAKE.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if check() {
                return;
            }
            tokio::select! {
                _ = notified => {}
                // Условие может исполниться без события носителя (например, после останова):
                // Короткий опрос не ждёт события, но и не спит фиксированное время.
                _ = poll.tick() => {}
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn wait_outcome(fx: &Fixture, code: &str) -> AttachOutcome {
    tokio::time::timeout(WAIT, async {
        loop {
            let notified = fx.cloud.changed().notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(found) = fx
                .cloud
                .outcomes()
                .iter()
                .find(|o| o.code() == code)
                .cloned()
            {
                return found;
            }
            notified.await;
        }
    })
    .await
    .expect("attach outcome in time")
}

fn write_marker(fx: &Fixture) {
    fs::write(&fx.marker, format!("ready-{}\n", Uuid::now_v7())).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_waits_for_the_marker_then_starts_the_worker_and_the_tunnel() {
    let fx = Fixture::start(Options::default()).await;
    let command = fx.attach_command("window-1", fx.sshd.host_key_public());
    let id = command.command_id.clone();
    // Маркер появляется после команды: отдельная задача пишет его, когда команда применена.
    let marker = fx.marker.clone();
    let late = Arc::new(Notify::const_new());
    let late_task = late.clone();
    tokio::spawn(async move {
        late_task.notified().await;
        fs::write(marker, "ready\n").unwrap();
    });
    fx.commands.handle(&command).await.unwrap();
    late.notify_one();
    let started = wait_outcome(&fx, "started").await;
    assert_eq!(
        started,
        AttachOutcome::Started {
            window_id: "window-1".into(),
            machine_id: fx.machine_id,
        }
    );
    until("the runner worker to connect", || fx.hello_count() >= 1).await;
    until("the tunnel to be up", || fx.tunnels() >= 1).await;
    assert!(fx.worker_pid().is_some_and(|pid| {
        Process::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    }));
    assert!(fx.installed());
    let (facts, terminal) = fx.sink.written.lock().unwrap().first().cloned().unwrap();
    assert!(facts.is_empty());
    assert_eq!(terminal, [(id, 1)]);
    until("the token to be requested", || {
        !fx.control.requests.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        fx.control.requests.lock().unwrap()[0],
        (fx.machine_id, true)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_host_key_is_rejected_before_anything_is_installed() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_command(
        "window-bad-key",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIWRONGKEYVALUE0000".into(),
    );
    fx.commands.handle(&command).await.unwrap();
    let failed = wait_outcome(&fx, "host_key_mismatch").await;
    assert_eq!(
        failed,
        AttachOutcome::Failed {
            window_id: "window-bad-key".into(),
            machine_id: fx.machine_id,
            code: "host_key_mismatch",
        }
    );
    assert!(
        !fx.installed(),
        "nothing may be installed on a key mismatch"
    );
    assert_eq!(fx.hello_count(), 0);
    assert_eq!(fx.tunnels(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_marker_times_out_within_the_deadline() {
    let fx = Fixture::start(Options::default()).await;
    let command = fx.attach_command("window-no-marker", fx.sshd.host_key_public());
    fx.commands.handle(&command).await.unwrap();
    let failed = tokio::time::timeout(
        fx.ready_timeout + Duration::from_secs(15),
        wait_outcome(&fx, "ready_timeout"),
    )
    .await
    .expect("ready_timeout is reported within the deadline");
    assert_eq!(failed.code(), "ready_timeout");
    assert!(!fx.installed());
    assert_eq!(fx.hello_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn replaying_the_command_keeps_one_worker_and_one_tunnel() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_command("window-replay", fx.sshd.host_key_public());
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "started").await;
    until("the first tunnel", || fx.tunnels() >= 1).await;
    until("the worker pid", || fx.worker_pid().is_some()).await;
    let pid = fx.worker_pid().unwrap();
    let tunnels = fx.tunnels();
    fx.commands.handle(&command).await.unwrap();
    let started: Vec<_> = fx
        .cloud
        .outcomes()
        .into_iter()
        .filter(|outcome| outcome.code() == "started")
        .collect();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(fx.worker_pid(), Some(pid));
    assert_eq!(fx.tunnels(), tunnels);
    assert_eq!(fx.cloud.attachments().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_assignment_stops_the_tunnel_and_forgets_the_attachment() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_command("window-detach", fx.sshd.host_key_public());
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "started").await;
    until("the tunnel before detach", || fx.tunnels() >= 1).await;
    fx.assign.send_replace(Vec::new());
    until("the tunnel to be stopped", || fx.tunnels() == 0).await;
    until("the attachment to be forgotten", || {
        fx.cloud.attachments().is_empty()
    })
    .await;
    assert!(!fx.root.join("state/worker.pid").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_resumes_the_attachment_only_while_assigned() {
    let fx = Fixture::start(Options {
        assigned: false,
        ..Options::default()
    })
    .await;
    write_marker(&fx);
    let command = fx.attach_command("window-restart", fx.sshd.host_key_public());
    fx.commands.handle(&command).await.unwrap();
    assert_eq!(fx.cloud.attachments().len(), 1);
    let mut fx = fx;
    fx.shutdown_carrier().await;

    // Полностью новый носитель читает состояние с диска.
    let cloud = Arc::new(CloudState::open(fx.cloud_config()));
    assert_eq!(cloud.attachments().len(), 1);
    let (assign, assigned_rx) = watch::channel(Vec::new());
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(carrier::run(
        fx.settings(cloud.clone()),
        fx.control.clone(),
        assigned_rx,
        shutdown,
    ));
    assert_eq!(cloud.specs().len(), 1);
    // Пока машина не поручена, раннер не поднимается.
    assert_eq!(
        cloud
            .outcomes()
            .iter()
            .filter(|o| o.code() == "started")
            .count(),
        0
    );
    let before = fx.hello_count();
    assign.send_replace(fx.assign_runner());
    until("the resumed runner to connect", || {
        fx.hello_count() > before
    })
    .await;
    assert!(
        cloud
            .outcomes()
            .iter()
            .any(|outcome| outcome.code() == "started")
    );
    let _ = stop.send(true);
    let _ = join.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_negotiated_capability_the_command_is_not_applied() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_command("window-no-cap", fx.sshd.host_key_public());
    fx.commands.set_enabled(false);
    let result = fx.commands.handle(&command).await;
    assert!(matches!(result, Err(CommandError::NotNegotiated)));
    assert!(fx.cloud.attachments().is_empty());
    assert_eq!(fx.hello_count(), 0);
    // Отказ не меняет состояние и при повторе после согласования команда применяется.
    fx.commands.set_enabled(true);
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "started").await;
}

/// Скрипт, который срабатывает только при живом обратном SOCKS: соединение на порт из
/// `egress.env` не пройдёт, если проброс ещё не поднят. След — файл на сервере окна.
fn tunnel_proof_script(effect: &std::path::Path) -> String {
    format!(
        "set -e\nexec 3<>/dev/tcp/127.0.0.1/$EGRESS_PORT\nexec 3>&-\necho prepared > {}\n",
        effect.display()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prepare_script_runs_after_the_tunnel_and_before_the_worker() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let effect = fx.sshd.path().join("prepared-by-script");
    let script = tunnel_proof_script(&effect);
    let command = fx.attach_scripted("window-prepare", fx.sshd.host_key_public(), &script);
    fx.commands.handle(&command).await.unwrap();
    // Успех подготовки — условие готовности окна и запуска воркера.
    wait_outcome(&fx, "started").await;
    until("the runner worker to connect", || fx.hello_count() >= 1).await;
    assert_eq!(fs::read_to_string(&effect).unwrap().trim(), "prepared");
    assert!(
        fx.worker_pid().is_some(),
        "the worker starts after the preparation"
    );
    assert!(fx.prepare_facts().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_prepare_script_reports_the_code_and_stops_the_worker() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_scripted(
        "window-prepare-fails",
        fx.sshd.host_key_public(),
        "echo boom; exit 3\n",
    );
    fx.commands.handle(&command).await.unwrap();
    let failed = wait_outcome(&fx, "prepare_failed").await;
    assert_eq!(failed.code(), "prepare_failed");
    until("the failure marker", || {
        fx.sshd.path().join("prepare-failed").exists()
    })
    .await;
    assert_eq!(fx.hello_count(), 0, "the worker never connects");
    assert!(fx.worker_pid().is_none(), "the worker never starts");
    assert!(!fx.installed(), "nothing is even installed");
    let facts = fx.prepare_facts();
    assert_eq!(facts.len(), 1, "{facts:?}");
    let (window, machine, code, tail) = &facts[0];
    assert_eq!(window, "window-prepare-fails");
    assert_eq!(machine, &fx.machine_id.to_string());
    assert_eq!(*code, 3);
    assert!(tail.contains("boom"), "{tail:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prepare_script_that_outlives_the_deadline_is_killed() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let command = fx.attach_scripted(
        "window-prepare-slow",
        fx.sshd.host_key_public(),
        "echo starting; sleep 60\n",
    );
    fx.commands.handle(&command).await.unwrap();
    let failed = tokio::time::timeout(
        fx.ready_timeout + Duration::from_secs(20),
        wait_outcome(&fx, "prepare_failed"),
    )
    .await
    .expect("the deadline ends the preparation");
    assert_eq!(failed.code(), "prepare_failed");
    assert_eq!(fx.hello_count(), 0, "the worker never connects");
    assert!(fx.worker_pid().is_none(), "the worker never starts");
    let facts = fx.prepare_facts();
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(facts[0].2, -1, "the deadline is reported as -1");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_success_does_not_rerun_the_script() {
    let mut fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let counter = fx.sshd.path().join("prepare-runs");
    let script = format!("echo run >> {}\n", counter.display());
    let command = fx.attach_scripted("window-prepared", fx.sshd.host_key_public(), &script);
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "started").await;
    until("the worker after the first attach", || {
        fx.hello_count() >= 1
    })
    .await;
    assert_eq!(fs::read_to_string(&counter).unwrap().lines().count(), 1);

    let before = fx.hello_count();
    let (cloud, assign, stop, join) = fx.restart().await;
    assign.send_replace(fx.assign_runner());
    until("the resumed runner to report ready", || {
        cloud
            .outcomes()
            .iter()
            .any(|outcome| outcome.code() == "started")
    })
    .await;
    until("the worker to reconnect", || fx.hello_count() > before).await;
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        1,
        "the script runs once per window"
    );
    let _ = stop.send(true);
    let _ = join.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_after_failure_starts_nothing() {
    let mut fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    let counter = fx.sshd.path().join("prepare-runs-again");
    let script = format!("echo run >> {}\nexit 4\n", counter.display());
    let command = fx.attach_scripted("window-failed", fx.sshd.host_key_public(), &script);
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "prepare_failed").await;
    assert_eq!(fs::read_to_string(&counter).unwrap().lines().count(), 1);

    let (cloud, assign, stop, join) = fx.restart().await;
    assign.send_replace(fx.assign_runner());
    until("the restarted carrier to refuse the window", || {
        cloud
            .outcomes()
            .iter()
            .any(|outcome| outcome.code() == "prepare_failed")
    })
    .await;
    assert_eq!(
        fs::read_to_string(&counter).unwrap().lines().count(),
        1,
        "the failed script never runs twice"
    );
    assert_eq!(fx.hello_count(), 0, "the worker never connects");
    assert!(fx.worker_pid().is_none(), "the worker never starts");
    let _ = stop.send(true);
    let _ = join.await;
}

/// Остался ли на хосте фоновый сон из подготовочного скрипта: у него своя строка команды,
/// поэтому чужие `sleep` не считаются.
fn hung_prepare_sleep() -> bool {
    Process::new("pgrep")
        .args(["-f", "^sleep 987"])
        .output()
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hung_prepare_script_is_killed_with_its_children() {
    let fx = Fixture::start(Options::default()).await;
    write_marker(&fx);
    // Фоновый сон игнорирует HUP: после обрыва сеанса sshd он пережил бы смерть оболочки,
    // и снять его можно только вместе со всей группой процессов скрипта.
    let command = fx.attach_scripted(
        "window-prepare-children",
        fx.sshd.host_key_public(),
        "bash -c 'trap \"\" HUP; exec sleep 987' & wait\n",
    );
    fx.commands.handle(&command).await.unwrap();
    wait_outcome(&fx, "prepare_failed").await;
    until("the hung child of the preparation to be gone", || {
        !hung_prepare_sleep()
    })
    .await;
}
