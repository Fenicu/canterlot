//! `canterlot-worker job run` против настоящего сокета воркера и подмены хаба.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use canterlot_worker::owner_jobs::{
    Finished, Hub, JobSpec, Refusal, Store,
    client::{RunArgs, execute},
    server::OwnerJobServer,
};
use tokio::sync::watch;
use uuid::Uuid;

const JOB_WAIT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct FakeHub {
    specs: Mutex<Vec<JobSpec>>,
    refuse: Mutex<Option<Refusal>>,
    issued: Mutex<Option<Uuid>>,
}

#[async_trait]
impl Hub for FakeHub {
    async fn submit(&self, spec_json: String) -> Result<Uuid, Refusal> {
        if let Some(refusal) = self.refuse.lock().unwrap().clone() {
            return Err(refusal);
        }
        self.specs
            .lock()
            .unwrap()
            .push(serde_json::from_str(&spec_json).unwrap());
        let id = Uuid::now_v7();
        *self.issued.lock().unwrap() = Some(id);
        Ok(id)
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    socket: PathBuf,
    store: Arc<Store>,
    hub: Arc<FakeHub>,
    stop: watch::Sender<bool>,
    server: Option<OwnerJobServer>,
}

impl Fixture {
    fn start() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ownerjobs-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("run/jobs.sock");
        let store = Store::open(&dir.path().join("data")).unwrap();
        let hub = Arc::new(FakeHub::default());
        let (stop, shutdown) = watch::channel(false);
        let server = Some(OwnerJobServer::start(
            socket.clone(),
            store.clone(),
            hub.clone(),
            shutdown,
        ));
        Self {
            dir,
            socket,
            store,
            hub,
            stop,
            server,
        }
    }

    /// Рестарт воркера: сокет и хранилище те же, процесс новый.
    fn restart(&mut self) {
        drop(self.server.take());
        self.store = Store::open(&self.dir.path().join("data")).unwrap();
        let shutdown = self.stop.subscribe();
        self.server = Some(OwnerJobServer::start(
            self.socket.clone(),
            self.store.clone(),
            self.hub.clone(),
            shutdown,
        ));
    }

    async fn run(&self, args: RunArgs) -> (i32, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = tokio::time::timeout(
            JOB_WAIT,
            execute(
                args,
                Some(self.socket.clone()),
                Duration::from_secs(15),
                &mut out,
                &mut err,
            ),
        )
        .await
        .expect("job run finished in time");
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

fn args(command: &[&str]) -> RunArgs {
    RunArgs {
        title: "gate".into(),
        workdir: Some("/tmp".into()),
        command: command.iter().map(OsString::from).collect(),
        ..RunArgs::default()
    }
}

fn finished(job_id: &str, state: &str, code: Option<i32>) -> Finished {
    Finished {
        job_id: job_id.to_owned(),
        state: state.into(),
        exit_code: code,
        tail: "last line\n".into(),
        outputs_dir: "/data/job-outputs/x".into(),
        outputs_note: "outputs not collected: unreachable".into(),
    }
}

async fn issued(hub: &FakeHub) -> String {
    for _ in 0..200 {
        if let Some(id) = *hub.issued.lock().unwrap() {
            return id.to_string();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the hub got no job");
}

#[tokio::test]
async fn submit_without_wait_prints_the_job_id_and_exits_zero() {
    let fx = Fixture::start();
    let mut a = args(&["echo", "hello world"]);
    a.labels = vec!["kernel-6.12".into()];
    a.outputs = vec!["out".into()];
    a.lease = Some("gate".into());
    a.expected_minutes = Some(40);
    a.timeout_minutes = Some(90);
    let (code, out, _err) = fx.run(a).await;
    assert_eq!(code, 0);
    let id = issued(&fx.hub).await;
    assert_eq!(out.trim(), id);
    let spec = fx.hub.specs.lock().unwrap()[0].clone();
    assert_eq!(spec.title, "gate");
    assert_eq!(spec.shell, "echo 'hello world'");
    assert_eq!(spec.labels, ["kernel-6.12"]);
    assert_eq!(spec.outputs, ["out"]);
    assert_eq!(spec.lease.as_deref(), Some("gate"));
    assert_eq!(spec.expected_minutes, Some(40));
    assert_eq!(spec.timeout_minutes, Some(90));
    assert_eq!(spec.workdir.as_deref(), Some("/tmp"));
    assert!(spec.source.is_none());
    // Один аргумент — это уже текст оболочки.
    let (_, _, _) = fx.run(args(&["cd /x && make 2>&1 | tail"])).await;
    assert_eq!(
        fx.hub.specs.lock().unwrap()[1].shell,
        "cd /x && make 2>&1 | tail"
    );
}

#[tokio::test]
async fn wait_returns_the_job_exit_code_whenever_the_result_arrives() {
    let fx = Fixture::start();
    let table = [
        ("succeeded", Some(0), 0),
        ("failed", Some(3), 3),
        ("failed", None, 1),
        ("lost", None, 1),
        ("timed_out", None, 124),
        ("canceled", None, 130),
    ];
    for (state, code, want) in table {
        // Итог приходит после того, как команда присоединилась.
        *fx.hub.issued.lock().unwrap() = None;
        let waiting = {
            let mut a = args(&["true"]);
            a.wait = true;
            fx.run(a)
        };
        let deliver = async {
            let id = issued(&fx.hub).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            fx.store.record(&finished(&id, state, code)).unwrap();
        };
        let ((got, out, err), ()) = tokio::join!(waiting, deliver);
        assert_eq!(got, want, "{state} {code:?}: {err}");
        assert_eq!(out, "last line\n");
        assert!(err.contains("outputs: /data/job-outputs/x"), "{err}");
        assert!(err.contains("outputs not collected: unreachable"), "{err}");
    }
}

#[tokio::test]
async fn wait_gets_a_result_that_arrived_before_the_command_attached() {
    let fx = Fixture::start();
    // Задача уже принята хабом, итог уже записан, а команда ещё не спрашивала.
    let id = Uuid::now_v7().to_string();
    fx.store.record(&finished(&id, "failed", Some(7))).unwrap();
    let mut stream = loop {
        match tokio::net::UnixStream::connect(&fx.socket).await {
            Ok(stream) => break stream,
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    stream
        .write_all(format!("{{\"v\":1,\"op\":\"wait\",\"job_id\":\"{id}\"}}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut line)
        .await
        .unwrap();
    assert!(
        line.contains("\"op\":\"finished\"") && line.contains("\"exit_code\":7"),
        "{line}"
    );
}

#[tokio::test]
async fn wait_survives_a_worker_restart() {
    let mut fx = Fixture::start();
    // Новая команда ждёт, воркер перезапускается, итог приходит уже после рестарта.
    let mut again = args(&["true"]);
    again.wait = true;
    let dir_data = fx.dir.path().join("data");
    let waiter = tokio::spawn({
        let socket = fx.socket.clone();
        async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = execute(
                again,
                Some(socket),
                Duration::from_secs(15),
                &mut out,
                &mut err,
            )
            .await;
            (code, String::from_utf8(err).unwrap())
        }
    });
    let second = issued(&fx.hub).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    fx.restart();
    tokio::time::sleep(Duration::from_millis(300)).await;
    fx.store
        .record(&finished(&second, "failed", Some(5)))
        .unwrap();
    let (code, err) = tokio::time::timeout(JOB_WAIT, waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(code, 5, "{err}");
    assert!(Path::new(&dir_data).join("owner-jobs").is_dir());
}

#[tokio::test]
async fn a_refusal_from_the_hub_exits_two_with_the_code() {
    let fx = Fixture::start();
    *fx.hub.refuse.lock().unwrap() =
        Some(Refusal::new("domain_mismatch", "repo is in another domain"));
    let (code, out, err) = fx.run(args(&["true"])).await;
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("domain_mismatch"), "{err}");
    // Негодное описание воркер отклоняет сам, до хаба.
    *fx.hub.refuse.lock().unwrap() = None;
    let mut bad = args(&["true"]);
    bad.title = String::new();
    let (code, _, err) = fx.run(bad).await;
    assert_eq!(code, 2);
    assert!(err.contains("bad_spec"), "{err}");
    assert!(fx.hub.specs.lock().unwrap().is_empty());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repo() -> (tempfile::TempDir, PathBuf, String) {
    let dir = tempfile::Builder::new()
        .prefix("ojrepo-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("project");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("a"), "1").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    (dir, repo, sha)
}

#[tokio::test]
async fn source_head_requires_a_clean_tree_and_resolves_to_a_full_sha() {
    let fx = Fixture::start();
    let (_guard, repo, sha) = repo();
    let source = |text: String| {
        let mut a = args(&["true"]);
        a.source = Some(text);
        a.workdir = None;
        a.outputs = vec!["result".into()];
        a
    };
    // Чистое дерево: HEAD превращается в полный sha, машина по умолчанию `auto`.
    let (code, _, err) = fx.run(source(format!("{}@HEAD", repo.display()))).await;
    assert_eq!(code, 0, "{err}");
    let spec = fx.hub.specs.lock().unwrap()[0].clone();
    let got = spec.source.unwrap();
    assert_eq!(got.rev, sha);
    assert_eq!(got.repo, repo.display().to_string());
    assert_eq!(spec.machine.as_deref(), Some("auto"));
    assert!(spec.workdir.is_none());
    assert_eq!(spec.outputs, ["result"]);
    // Полный sha принимается без проверки чистоты дерева.
    fs::write(repo.join("a"), "dirty").unwrap();
    let (code, _, err) = fx.run(source(format!("{}@{sha}", repo.display()))).await;
    assert_eq!(code, 0, "{err}");
    // Грязное дерево: отказ до хаба.
    let before = fx.hub.specs.lock().unwrap().len();
    let (code, out, err) = fx.run(source(format!("{}@HEAD", repo.display()))).await;
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("dirty"), "{err}");
    // Неотслеживаемый файл тоже делает дерево грязным.
    git(&repo, &["checkout", "-q", "--", "a"]);
    fs::write(repo.join("new.txt"), "x").unwrap();
    let (code, _, err) = fx.run(source(format!("{}@HEAD", repo.display()))).await;
    assert_eq!(code, 2, "{err}");
    // Короткий sha и произвольное имя ссылки не принимаются, неизвестный sha тоже.
    for bad in [&sha[..12], "main", &"0".repeat(40)] {
        let (code, _, err) = fx.run(source(format!("{}@{bad}", repo.display()))).await;
        assert_eq!(code, 2, "{bad}: {err}");
    }
    assert_eq!(fx.hub.specs.lock().unwrap().len(), before);
    // Не корень репозитория.
    let (code, _, err) = fx
        .run(source(format!("{}@{sha}", repo.join("sub").display())))
        .await;
    assert_eq!(code, 2, "{err}");
}

#[test]
fn the_binary_exposes_job_run_and_refuses_without_a_socket() {
    let binary = env!("CARGO_BIN_EXE_canterlot-worker");
    let output = Command::new(binary)
        .args([
            "job",
            "run",
            "--title",
            "t",
            "--workdir",
            "/tmp",
            "--",
            "true",
        ])
        .env("CANTERLOT_JOBS_SOCKET", "/nonexistent-dir/jobs.sock")
        .env_remove("XDG_RUNTIME_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not reachable"));
    let help = Command::new(binary)
        .args(["job", "run", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    for flag in [
        "--title", "--source", "--output", "--wait", "--lease", "--labels",
    ] {
        assert!(text.contains(flag), "{flag}");
    }
}
