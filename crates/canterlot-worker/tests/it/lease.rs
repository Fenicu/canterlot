//! Аренда тяжёлых задач на настоящих процессах: сервер воркера на сокете во временном
//! каталоге и обёртки `canterlot-worker lease run`.

use std::{
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use canterlot_worker::containment::LEASE_SOCKET_ENV;
use canterlot_worker::lease::{
    self, Snapshot,
    server::{LeaseServer, ServerConfig},
};
use tokio::sync::watch;

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");
const WAIT: Duration = Duration::from_secs(20);

struct Fixture {
    root: tempfile::TempDir,
    stop: watch::Sender<bool>,
    server: Option<LeaseServer>,
    slots: u32,
    restore_grace: Duration,
}

impl Fixture {
    async fn new(slots: u32) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("data")).unwrap();
        std::fs::create_dir(root.path().join("runtime")).unwrap();
        let (stop, _) = watch::channel(false);
        let mut fixture = Self {
            root,
            stop,
            server: None,
            slots,
            restore_grace: lease::server::RESTORE_GRACE,
        };
        fixture.start().await;
        fixture
    }

    fn config(&self) -> ServerConfig {
        let mut config = ServerConfig::new(
            Some(&self.root.path().join("runtime")),
            &self.root.path().join("data"),
            self.slots,
        );
        config.restore_grace = self.restore_grace;
        config
    }

    fn socket(&self) -> PathBuf {
        self.config().socket
    }

    async fn start(&mut self) {
        self.server = Some(LeaseServer::start(self.config(), self.stop.subscribe()));
        let socket = self.socket();
        wait_for("socket", || {
            std::os::unix::net::UnixStream::connect(&socket).is_ok()
        })
        .await;
    }

    async fn restart(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.server.as_ref().unwrap().snapshot().borrow().clone()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn wrapper(&self, kind: &str, args: &[&str]) -> Command {
        let mut command = Command::new(WORKER);
        command
            .args(["lease", "run", "--kind", kind, "--label"])
            .arg(format!("test {kind}"))
            .arg("--")
            .args(args)
            .current_dir(self.root.path())
            .env(LEASE_SOCKET_ENV, self.socket())
            .env_remove(lease::HELD_ENV)
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("PW_WORKERS")
            .env_remove("CANTERLOT_EXECUTION_GROUP")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        command
    }

    /// Задача, которая под арендой пишет свои ограничения в `<name>.held` и ждёт `<name>.go`.
    fn job(&self, kind: &str, name: &str) -> Job {
        let held = self.path(&format!("{name}.held"));
        let go = self.path(&format!("{name}.go"));
        let script = format!(
            "echo \"$CANTERLOT_LEASE_HELD $CANTERLOT_TEST_THREADS $CARGO_BUILD_JOBS $PW_WORKERS $CANTERLOT_LEASE_BUDGET\" > {held:?}.tmp && mv {held:?}.tmp {held:?}; while [ ! -e {go:?} ]; do sleep 0.05; done"
        );
        let mut command = self.wrapper(kind, &["sh", "-c", &script]);
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        Job {
            child: command.spawn().unwrap(),
            held,
            go,
            reaped: false,
        }
    }

    async fn waiters(&self, count: usize) {
        let server = self.server.as_ref().unwrap();
        let mut snapshot = server.snapshot();
        tokio::time::timeout(WAIT, async {
            loop {
                if snapshot.borrow_and_update().waiters.len() == count {
                    return;
                }
                snapshot.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {count} waiters: {:?}", self.snapshot()));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

struct Job {
    child: std::process::Child,
    held: PathBuf,
    go: PathBuf,
    reaped: bool,
}

impl Job {
    fn granted(&self) -> bool {
        self.held.exists()
    }

    async fn wait_granted(&self) -> Vec<String> {
        let held = self.held.clone();
        wait_for("grant", || held.exists()).await;
        std::fs::read_to_string(&self.held)
            .unwrap()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    async fn finish(mut self) -> std::process::ExitStatus {
        std::fs::write(&self.go, b"").unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                return status;
            }
            assert!(started.elapsed() < WAIT, "job did not finish");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.id()).unwrap()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.go, b"");
        if self.reaped {
            return;
        }
        // Неубранный лидер держит номер группы: команда убитой обёртки уходит вместе с ней.
        if let Some(group) = rustix::process::Pid::from_raw(self.pid()) {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.child.wait();
    }
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Порядок FIFO, исключительность гейта и преимущество ожидающего гейта перед поздней
/// сборкой; ограничения ресурсов по формуле аренды.
#[tokio::test(flavor = "multi_thread")]
async fn two_builds_then_gate_respect_order_and_writer_preference() {
    let fixture = Fixture::new(2).await;
    let nproc = lease::nproc();
    let b1 = fixture.job("build", "b1");
    let b1_env = b1.wait_granted().await;
    let b2 = fixture.job("build", "b2");
    b2.wait_granted().await;
    let budget = lease::budget(nproc, 1, 2).to_string();
    let pw = lease::playwright_workers(nproc, 1, 2).to_string();
    assert_eq!(b1_env[1..], [budget.clone(), budget.clone(), pw, budget]);
    assert_ne!(b1_env[0], "unleased");

    let gate = fixture.job("gate", "gate");
    fixture.waiters(1).await;
    let b3 = fixture.job("build", "b3");
    fixture.waiters(2).await;
    let waiters: Vec<_> = fixture
        .snapshot()
        .waiters
        .iter()
        .map(|w| (w.kind.clone(), w.position))
        .collect();
    assert_eq!(
        waiters,
        [("gate".into(), Some(1)), ("build".into(), Some(2))]
    );

    // Освободился слот, но первым ждёт гейт: поздняя сборка его не обгоняет.
    assert!(b1.finish().await.success());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!gate.granted() && !b3.granted());
    assert_eq!(fixture.snapshot().holders.len(), 1);

    assert!(b2.finish().await.success());
    let gate_env = gate.wait_granted().await;
    let full = lease::budget(nproc, 2, 2).to_string();
    assert_eq!(gate_env[1], full);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!b3.granted(), "build started next to a gate");

    assert!(gate.finish().await.success());
    b3.wait_granted().await;
    assert!(b3.finish().await.success());
    wait_for("empty queue", || {
        let snapshot = fixture.snapshot();
        snapshot.holders.is_empty() && snapshot.waiters.is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_9_of_holder_wrapper_releases_its_slot() {
    let fixture = Fixture::new(1).await;
    let holder = fixture.job("build", "holder");
    holder.wait_granted().await;
    let next = fixture.job("build", "next");
    fixture.waiters(1).await;
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(holder.pid()).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    next.wait_granted().await;
    assert!(next.finish().await.success());
}

/// Рестарт воркера: живая аренда из `leases.json` держится, мёртвая снимается;
/// держатель переподключается и отпускает аренду своим выходом.
#[tokio::test(flavor = "multi_thread")]
async fn restart_keeps_live_grant_and_drops_dead_one() {
    let mut fixture = Fixture::new(2).await;
    let live = fixture.job("build", "live");
    live.wait_granted().await;
    let dead = fixture.job("build", "dead");
    dead.wait_granted().await;
    let persisted = std::fs::read_to_string(fixture.path("data/leases.json")).unwrap();
    assert_eq!(persisted.matches("\"lease_id\"").count(), 2, "{persisted}");

    fixture.restart().await;
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(dead.pid()).unwrap(),
        rustix::process::Signal::KILL,
    )
    .unwrap();
    wait_for("dead wrapper exit", || {
        !Path::new(&format!("/proc/{}/stat", dead.pid())).exists()
            || std::fs::read_to_string(format!("/proc/{}/stat", dead.pid()))
                .is_ok_and(|s| s.contains(") Z "))
    })
    .await;
    fixture.start().await;
    let holders = fixture.snapshot().holders;
    assert_eq!(holders.len(), 1, "{holders:?}");
    assert_eq!(holders[0].label, "test build");
    assert_eq!(u32::try_from(live.pid()).unwrap(), holders[0].wrapper_pid);

    let third = fixture.job("build", "third");
    third.wait_granted().await;
    let fourth = fixture.job("build", "fourth");
    fixture.waiters(1).await;
    assert!(live.finish().await.success());
    fourth.wait_granted().await;
    assert!(third.finish().await.success());
    assert!(fourth.finish().await.success());
}

/// Рестарт с непустой очередью: ожидающий гейт переподключается и не теряет места —
/// сборка, пришедшая в окно, не получает свободный слот раньше него.
#[tokio::test(flavor = "multi_thread")]
async fn restart_grace_keeps_the_queued_gate_ahead_of_a_new_build() {
    let mut fixture = Fixture::new(2).await;
    fixture.restore_grace = Duration::from_millis(1500);
    let holder = fixture.job("build", "holder");
    holder.wait_granted().await;
    let gate = fixture.job("gate", "queued-gate");
    fixture.waiters(1).await;

    fixture.restart().await;
    fixture.start().await;
    // Сборка просит слот раньше, чем гейт успел переподключиться: один слот свободен.
    let late = fixture.job("build", "late");
    fixture.waiters(2).await;
    let kinds: Vec<_> = fixture
        .snapshot()
        .waiters
        .iter()
        .map(|waiter| waiter.kind.clone())
        .collect();
    assert_eq!(kinds, ["gate", "build"], "gate lost its place");

    // Окно придержки прошло: поздняя сборка не обогнала гейт.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!late.granted(), "a new build overtook the restored queue");
    assert!(!gate.granted());

    assert!(holder.finish().await.success());
    gate.wait_granted().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!late.granted(), "build started next to a gate");
    assert!(gate.finish().await.success());
    late.wait_granted().await;
    assert!(late.finish().await.success());
}

fn output(mut command: Command) -> (std::process::ExitStatus, String, String) {
    let out = command.stdout(Stdio::piped()).output().unwrap();
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_call_inside_lease_execs_directly() {
    let fixture = Fixture::new(1).await;
    let mut command = fixture.wrapper(
        "gate",
        &["sh", "-c", "echo $$ $CANTERLOT_LEASE_HELD; exit 7"],
    );
    command.env(lease::HELD_ENV, "outer-lease");
    let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(status.code(), Some(7));
    // exec: тот же pid, окружение не тронуто, в очередь никто не встал.
    assert_eq!(stdout.trim(), format!("{pid} outer-lease"));
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains("already inside lease outer-lease"),
        "{stderr}"
    );
    assert!(fixture.snapshot().holders.is_empty());
}

fn unleased(socket: Option<&Path>) -> Command {
    let mut command = Command::new(WORKER);
    command
        .args(["lease", "run", "--kind", "build", "--", "sh", "-c"])
        .arg("echo held=$CANTERLOT_LEASE_HELD; exit 5")
        .env_remove(lease::HELD_ENV)
        .env_remove(LEASE_SOCKET_ENV)
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null());
    if let Some(socket) = socket {
        command.env(LEASE_SOCKET_ENV, socket);
    }
    command
}

fn assert_unleased(result: (std::process::ExitStatus, String, String), reason: &str) {
    let (status, stdout, stderr) = result;
    assert_eq!(status.code(), Some(5), "{stderr}");
    assert_eq!(stdout.trim(), "held=unleased");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("running without lease"), "{stderr}");
    assert!(stderr.contains(reason), "{reason}: {stderr}");
}

#[test]
fn without_worker_the_command_runs_unwrapped_with_one_line() {
    let root = tempfile::tempdir().unwrap();
    assert_unleased(output(unleased(None)), "no lease socket known");
    assert_unleased(
        output(unleased(Some(&root.path().join("missing.sock")))),
        "worker unreachable",
    );
    // Сокет без права подключения (EACCES) — та же недоступность.
    let socket = root.path().join("closed.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .unwrap();
    let result = output(unleased(Some(&socket)));
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    drop(listener);
    // root обходит права каталога: тогда подключение проходит и ответа нет, это уже не EACCES.
    if !rustix::process::geteuid().is_root() {
        assert_unleased(result, "worker unreachable");
    }
}

#[test]
fn refusing_socket_is_retried_then_unwrapped() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("lease.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    let started = Instant::now();
    assert_unleased(output(unleased(Some(&socket))), "worker unreachable");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(9) && elapsed < Duration::from_secs(20),
        "{elapsed:?}"
    );
}

#[test]
fn unanswered_worker_times_out_and_runs_unwrapped() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("lease.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(stream.try_clone().unwrap()),
            &mut line,
        )
        .unwrap();
        assert!(line.contains("\"op\":\"acquire\""), "{line}");
        let _ = wait.recv_timeout(Duration::from_secs(30));
        drop(stream);
    });
    let started = Instant::now();
    let mut command = unleased(Some(&socket));
    command.env("CANTERLOT_LEASE_REPLY_TIMEOUT_MS", "500");
    assert_unleased(output(command), "did not answer");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
    let _ = release.send(());
    server.join().unwrap();
}

#[test]
fn protocol_error_runs_unwrapped() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("lease.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(stream.try_clone().unwrap()),
            &mut line,
        )
        .unwrap();
        assert!(line.contains("\"op\":\"acquire\""), "{line}");
        std::io::Write::write_all(&mut stream, b"{\"v\":2,\"op\":\"granted\"}\n").unwrap();
    });
    assert_unleased(
        output(unleased(Some(&socket))),
        "unsupported protocol version 2",
    );
    server.join().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_codes_and_signals_pass_through() {
    let fixture = Fixture::new(1).await;
    let (status, _, stderr) = output(fixture.wrapper("build", &["sh", "-c", "exit 3"]));
    assert_eq!(status.code(), Some(3), "{stderr}");
    // Под арендой ничего лишнего в stderr: ни ожидания, ни выдачи.
    assert_eq!(stderr, "");
    let (status, _, _) = output(fixture.wrapper("build", &["sh", "-c", "kill -TERM $$"]));
    assert_eq!(status.signal(), Some(libc_signal::TERM), "{status:?}");
    let (status, _, stderr) = output(fixture.wrapper("build", &["/nonexistent/canterlot-command"]));
    assert_eq!(status.code(), Some(127), "{stderr}");
    let mut bad = Command::new(WORKER);
    bad.args(["lease", "run", "--kind", "exclusive", "--", "true"]);
    let (status, _, _) = output(bad);
    assert_eq!(status.code(), Some(2));
    let mut missing = Command::new(WORKER);
    missing.args(["lease", "run", "--kind", "build"]);
    let (status, _, _) = output(missing);
    assert_eq!(status.code(), Some(2));
    wait_for("released", || fixture.snapshot().holders.is_empty()).await;
}

mod libc_signal {
    pub const INT: i32 = 2;
    pub const TERM: i32 = 15;
}

/// Ctrl-C терминала достаётся всей группе: команда умирает, обёртка повторяет её сигнал
/// и не оставляет сироту. TERM только обёртке пересылается команде.
#[tokio::test(flavor = "multi_thread")]
async fn interrupt_to_group_and_term_to_wrapper_leave_no_orphan() {
    let fixture = Fixture::new(1).await;
    for (signal, to_group) in [(libc_signal::INT, true), (libc_signal::TERM, false)] {
        let pid_file = fixture.path(&format!("child-{signal}.pid"));
        let script = format!(
            "echo $$ > {pid_file:?}.tmp && mv {pid_file:?}.tmp {pid_file:?}; exec sleep 30"
        );
        let mut command = fixture.wrapper("build", &["sh", "-c", &script]);
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut wrapper = command.spawn().unwrap();
        let file = pid_file.clone();
        wait_for("child pid", || file.exists()).await;
        let child: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let wrapper_pid =
            rustix::process::Pid::from_raw(i32::try_from(wrapper.id()).unwrap()).unwrap();
        let sig = if signal == libc_signal::INT {
            rustix::process::Signal::INT
        } else {
            rustix::process::Signal::TERM
        };
        if to_group {
            rustix::process::kill_process_group(wrapper_pid, sig).unwrap();
        } else {
            rustix::process::kill_process(wrapper_pid, sig).unwrap();
        }
        let started = Instant::now();
        let status = loop {
            if let Some(status) = wrapper.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < WAIT, "wrapper did not exit on {signal}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(status.signal(), Some(signal), "{status:?}");
        assert!(
            !Path::new(&format!("/proc/{child}")).exists()
                || std::fs::read_to_string(format!("/proc/{child}/stat"))
                    .is_ok_and(|s| s.contains(") Z ")),
            "child {child} survived {signal}"
        );
    }
    wait_for("released", || fixture.snapshot().holders.is_empty()).await;
}

/// Унаследованный SIG_IGN (`nohup`, фоновое задание) переживает обёртку: иначе перехват
/// tokio вернул бы команде SIG_DFL, и HUP убил бы её вместе с закрытием терминала.
#[tokio::test(flavor = "multi_thread")]
async fn inherited_ignored_signal_stays_ignored() {
    let fixture = Fixture::new(1).await;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(
            "trap '' HUP; exec \"$0\" lease run --kind build -- \
             sh -c 'kill -HUP $$; echo survived'",
        )
        .arg(WORKER)
        .current_dir(fixture.root.path())
        .env(LEASE_SOCKET_ENV, fixture.socket())
        .env_remove(lease::HELD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (status, stdout, stderr) = output(command);
    assert_eq!(
        status.code(),
        Some(0),
        "signal {:?}: {stderr}",
        status.signal()
    );
    assert_eq!(stdout.trim(), "survived", "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_lists_holders_and_waiters() {
    let fixture = Fixture::new(1).await;
    let holder = fixture.job("gate", "status-holder");
    holder.wait_granted().await;
    let waiter = fixture.job("build", "status-waiter");
    fixture.waiters(1).await;
    let mut status = Command::new(WORKER);
    status
        .args(["lease", "status"])
        .env(LEASE_SOCKET_ENV, fixture.socket());
    let (exit, stdout, stderr) = output(status);
    assert!(exit.success(), "{stderr}");
    assert!(
        stdout.starts_with("slots 1/1\nholders:\n  held gate «test gate"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("pid {} [", holder.pid())),
        "{stdout}"
    );
    assert!(
        stdout.contains("waiters:\n  #1 build «test build"),
        "{stdout}"
    );
    let mut json = Command::new(WORKER);
    json.args(["lease", "status", "--json"])
        .env(LEASE_SOCKET_ENV, fixture.socket());
    let (_, stdout, _) = output(json);
    let parsed: Snapshot = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(parsed.holders.len(), 1);
    assert_eq!(parsed.waiters.len(), 1);
    assert!(!parsed.holders[0].state.is_empty());
    drop(waiter);
    drop(holder);
}

/// Воркер ушёл и не вернулся: команда держателя работает дальше, а её выход не ждёт
/// 30 с переподключения.
#[tokio::test(flavor = "multi_thread")]
async fn holder_exits_promptly_while_worker_is_down() {
    let mut fixture = Fixture::new(1).await;
    let job = fixture.job("build", "orphaned");
    job.wait_granted().await;
    fixture.restart().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    let status = job.finish().await;
    assert!(status.success(), "{status:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
