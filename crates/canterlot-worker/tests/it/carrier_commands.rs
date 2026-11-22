//! Команды хаба носителю (`StageSource`, `CollectOutputs`, `CleanupSource`, `OwnerJobFinished`):
//! какие факты они пишут, как гасятся и что делают без согласованной возможности. Исполнитель
//! настоящий (git и `sshd`), приёмник фактов — запись в память.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    CleanupSource, CollectOutputs, Command, OwnerJobFinished, StageSource, command::Kind,
    record_body::Body,
};
use canterlot_worker::{
    carrier::{
        cloud::{CloudConfig, CloudState},
        commands::{CarrierCommands, CommandError, FactSink},
        stage::{RunnerTarget, Stager, StagerSettings},
    },
    owner_jobs::Store,
};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

use crate::sshd::{ALIAS, TestSshd};

const RUNNER_ID: Uuid = Uuid::from_u128(0x1111);
const LOCAL_ID: Uuid = Uuid::from_u128(0x2222);
const WAIT: Duration = Duration::from_secs(30);

/// Одна запись журнала: факты и погашенные ими команды `(command_id, dispatch_attempt)`.
type Write = (Vec<Body>, Vec<(String, u32)>);

#[derive(Default)]
struct Sink {
    written: Mutex<Vec<Write>>,
    written_notify: Notify,
    /// Сколько ближайших записей отказать (хранилище не ответило).
    fail: Mutex<u32>,
}

#[async_trait]
impl FactSink for Sink {
    async fn facts(&self, facts: Vec<Body>, terminal: Vec<(String, u32)>) -> Result<(), ()> {
        {
            let mut fail = self.fail.lock().unwrap();
            if *fail > 0 {
                *fail -= 1;
                return Err(());
            }
        }
        self.written.lock().unwrap().push((facts, terminal));
        self.written_notify.notify_waiters();
        Ok(())
    }
}

impl Sink {
    async fn next(&self, index: usize) -> Write {
        tokio::time::timeout(WAIT, async {
            loop {
                let notified = self.written_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(found) = self.written.lock().unwrap().get(index).cloned() {
                    return found;
                }
                notified.await;
            }
        })
        .await
        .expect("a fact was written in time")
    }

    fn count(&self) -> usize {
        self.written.lock().unwrap().len()
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Process::new("git")
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

struct Fixture {
    sshd: TestSshd,
    _src: tempfile::TempDir,
    _data: tempfile::TempDir,
    repo: PathBuf,
    sha: String,
    root: PathBuf,
    data: PathBuf,
    sink: Arc<Sink>,
    store: Arc<Store>,
    commands: Arc<CarrierCommands>,
    _stop: watch::Sender<bool>,
}

fn fixture() -> Fixture {
    let sshd = TestSshd::start();
    let src = tempfile::Builder::new()
        .prefix("cmd-src-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = src.path().join("project");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("file.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let root = sshd.path().join("runner");
    fs::create_dir(&root).unwrap();
    let data_dir = tempfile::Builder::new()
        .prefix("cmd-data-")
        .tempdir_in("/tmp")
        .unwrap();
    let data = data_dir.path().to_path_buf();
    let target = RunnerTarget {
        alias: ALIAS.to_owned(),
        root: root.clone(),
    };
    let stager = Stager::new(StagerSettings {
        ssh_config: Some(sshd.ssh_config.clone()),
        outputs_root: data.join("job-outputs"),
        targets: Arc::new(move |machine| (machine == RUNNER_ID).then(|| target.clone())),
        ssh_limit: Duration::from_secs(30),
        push_limit: Duration::from_secs(60),
        allowed_roots: vec![PathBuf::from("/tmp")],
    });
    let store = Store::open(&data).unwrap();
    let sink = Arc::new(Sink::default());
    let (stop, shutdown) = watch::channel(false);
    let cloud = Arc::new(CloudState::open(CloudConfig {
        data_dir: data.clone(),
        ..CloudConfig::default()
    }));
    let commands = CarrierCommands::new(stager, store.clone(), sink.clone(), cloud, shutdown);
    commands.set_enabled(true);
    Fixture {
        sshd,
        _src: src,
        _data: data_dir,
        repo,
        sha,
        root,
        data,
        sink,
        store,
        commands,
        _stop: stop,
    }
}

fn command(id: &str, kind: Kind) -> Command {
    Command {
        command_id: id.to_owned(),
        dispatch_attempt: 1,
        kind: Some(kind),
        ..Command::default()
    }
}

struct Place {
    machine: String,
    root: String,
    workdir: String,
    domain: &'static str,
}

fn runner_place(fx: &Fixture, job: Uuid) -> Place {
    Place {
        machine: RUNNER_ID.to_string(),
        root: fx.root.display().to_string(),
        workdir: fx
            .root
            .join("jobs/work")
            .join(job.to_string())
            .display()
            .to_string(),
        domain: "work",
    }
}

fn local_place(fx: &Fixture, job: Uuid) -> Place {
    Place {
        machine: LOCAL_ID.to_string(),
        root: String::new(),
        workdir: fx
            .repo
            .parent()
            .unwrap()
            .join(".canterlot-jobs/project")
            .join(job.to_string())
            .display()
            .to_string(),
        domain: "personal",
    }
}

fn stage(fx: &Fixture, job: Uuid, place: &Place, rev: &str) -> Command {
    command(
        &format!("stage-{job}"),
        Kind::StageSource(StageSource {
            job_id: job.to_string(),
            repo: fx.repo.display().to_string(),
            rev: rev.to_owned(),
            domain: place.domain.to_owned(),
            target_machine_id: place.machine.clone(),
            target_root_dir: place.root.clone(),
            workdir: place.workdir.clone(),
            repo_key: "project-0123456789ab".into(),
        }),
    )
}

fn cleanup(job: Uuid, place: &Place) -> Command {
    command(
        &format!("cleanup-{job}"),
        Kind::CleanupSource(CleanupSource {
            job_id: job.to_string(),
            target_machine_id: place.machine.clone(),
            target_root_dir: place.root.clone(),
            workdir: place.workdir.clone(),
            repo_key: "project-0123456789ab".into(),
            domain: place.domain.to_owned(),
        }),
    )
}

fn collect(job: Uuid, place: &Place, paths: &[&str], cap: u64) -> Command {
    command(
        &format!("collect-{job}"),
        Kind::CollectOutputs(CollectOutputs {
            job_id: job.to_string(),
            target_machine_id: place.machine.clone(),
            target_root_dir: place.root.clone(),
            workdir: place.workdir.clone(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            cap_bytes: cap,
        }),
    )
}

#[tokio::test]
async fn stage_collect_cleanup_write_their_facts_and_settle_the_command() {
    for on_runner in [true, false] {
        let fx = fixture();
        let job = Uuid::now_v7();
        let place = if on_runner {
            runner_place(&fx, job)
        } else {
            local_place(&fx, job)
        };
        let mut seen = fx.sink.count();
        // Подготовка.
        let staged = stage(&fx, job, &place, &fx.sha);
        fx.commands.handle(&staged).await.unwrap();
        let (facts, terminal) = fx.sink.next(seen).await;
        seen += 1;
        assert_eq!(terminal, [(format!("stage-{job}"), 1)]);
        assert!(
            matches!(&facts[..], [Body::SourceStaged(fact)] if fact.job_id == job.to_string()),
            "runner={on_runner}: {facts:?}"
        );
        assert_eq!(
            fs::read_to_string(Path::new(&place.workdir).join("file.txt")).unwrap(),
            "one\n"
        );
        // Сбор: файл, отсутствующий путь.
        fs::create_dir_all(Path::new(&place.workdir).join("out")).unwrap();
        fs::write(Path::new(&place.workdir).join("out/a.txt"), "aaaa").unwrap();
        fx.commands
            .handle(&collect(job, &place, &["out", "absent"], 1 << 20))
            .await
            .unwrap();
        let (facts, terminal) = fx.sink.next(seen).await;
        seen += 1;
        assert_eq!(terminal, [(format!("collect-{job}"), 1)]);
        let [Body::OutputsCollected(done)] = &facts[..] else {
            panic!("{facts:?}");
        };
        assert_eq!(
            done.local_dir,
            fx.data
                .join("job-outputs")
                .join(job.to_string())
                .display()
                .to_string()
        );
        assert_eq!(done.files.len(), 1);
        assert_eq!(
            (done.files[0].path.as_str(), done.files[0].bytes),
            ("out/a.txt", 4)
        );
        assert!(!done.truncated);
        // Пропавший путь остаётся пометкой рядом с результатами.
        let notes =
            fs::read_to_string(Path::new(&done.local_dir).join("COLLECT-NOTES.txt")).unwrap();
        assert!(notes.contains("missing: absent"), "{notes}");
        // Уборка.
        fx.commands.handle(&cleanup(job, &place)).await.unwrap();
        let (facts, terminal) = fx.sink.next(seen).await;
        assert_eq!(terminal, [(format!("cleanup-{job}"), 1)]);
        assert!(
            matches!(&facts[..], [Body::SourceCleaned(fact)] if fact.job_id == job.to_string())
        );
        assert!(!Path::new(&place.workdir).exists());
    }
}

#[tokio::test]
async fn failures_become_failure_facts_and_a_failed_cleanup_writes_no_fact() {
    let fx = fixture();
    let job = Uuid::now_v7();
    let place = local_place(&fx, job);
    // Неизвестный коммит: `SourceStageFailed` с причиной.
    fx.commands
        .handle(&stage(&fx, job, &place, &"0".repeat(40)))
        .await
        .unwrap();
    let (facts, terminal) = fx.sink.next(0).await;
    assert_eq!(terminal.len(), 1);
    let [Body::SourceStageFailed(failed)] = &facts[..] else {
        panic!("{facts:?}");
    };
    assert_eq!(failed.job_id, job.to_string());
    assert!(
        failed.reason.starts_with("unknown_rev"),
        "{}",
        failed.reason
    );
    // Сбор из рабочей копии, которой нет: `OutputsCollectFailed`.
    fx.commands
        .handle(&collect(job, &place, &["out"], 100))
        .await
        .unwrap();
    let (facts, _) = fx.sink.next(1).await;
    let [Body::OutputsCollectFailed(failed)] = &facts[..] else {
        panic!("{facts:?}");
    };
    assert_eq!(failed.job_id, job.to_string());
    // Негодная уборка: команда гасится, факта нет (хаб повторит сам).
    let bad = Place {
        workdir: "/tmp/not-a-job-dir".into(),
        ..local_place(&fx, job)
    };
    fx.commands.handle(&cleanup(job, &bad)).await.unwrap();
    let (facts, terminal) = fx.sink.next(2).await;
    assert!(facts.is_empty());
    assert_eq!(terminal, [(format!("cleanup-{job}"), 1)]);
}

#[tokio::test]
async fn without_the_negotiated_capability_commands_do_nothing() {
    let fx = fixture();
    fx.commands.set_enabled(false);
    let job = Uuid::now_v7();
    let place = local_place(&fx, job);
    let result = fx.commands.handle(&stage(&fx, job, &place, &fx.sha)).await;
    assert!(matches!(result, Err(CommandError::NotNegotiated)));
    assert!(!Path::new(&place.workdir).exists());
    assert_eq!(fx.sink.count(), 0);
    // Команды других видов этому обработчику не принадлежат.
    fx.commands.set_enabled(true);
    let other = command("x", Kind::CancelInput(Default::default()));
    assert!(matches!(
        fx.commands.handle(&other).await,
        Err(CommandError::Invalid)
    ));
}

#[tokio::test]
async fn a_command_whose_fact_was_not_written_replays_to_the_same_result() {
    let fx = fixture();
    let job = Uuid::now_v7();
    let place = local_place(&fx, job);
    let staged = stage(&fx, job, &place, &fx.sha);
    // Журнал не принял запись: команда остаётся непогашенной, работа уже сделана.
    *fx.sink.fail.lock().unwrap() = 1;
    fx.commands.handle(&staged).await.unwrap();
    for _ in 0..200 {
        if *fx.sink.fail.lock().unwrap() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(fx.sink.count(), 0);
    // Доигрывание после рестарта: та же команда, тот же ответ, повторная подготовка безвредна.
    loop {
        fx.commands.handle(&staged).await.unwrap();
        if tokio::time::timeout(Duration::from_millis(200), fx.sink.next(0))
            .await
            .is_ok()
        {
            break;
        }
    }
    let (facts, terminal) = fx.sink.next(0).await;
    assert!(matches!(&facts[..], [Body::SourceStaged(_)]));
    assert_eq!(terminal, [(format!("stage-{job}"), 1)]);
    assert_eq!(
        git(Path::new(&place.workdir), &["rev-parse", "HEAD"]),
        fx.sha
    );
}

#[tokio::test]
async fn owner_job_results_are_stored_and_the_command_settles() {
    let fx = fixture();
    let job = Uuid::now_v7();
    let done = command(
        "finished-1",
        Kind::OwnerJobFinished(OwnerJobFinished {
            job_id: job.to_string(),
            state: "failed".into(),
            exit_code: Some(3),
            tail: b"boom\n".to_vec(),
            outputs_dir: "/data/job-outputs/x".into(),
            outputs_note: "outputs not collected: unreachable".into(),
        }),
    );
    fx.commands.handle(&done).await.unwrap();
    let (facts, terminal) = fx.sink.next(0).await;
    assert!(facts.is_empty());
    assert_eq!(terminal, [("finished-1".to_owned(), 1)]);
    let found = fx.store.get(&job.to_string()).expect("result stored");
    assert_eq!((found.state.as_str(), found.exit_code), ("failed", Some(3)));
    assert_eq!(found.tail, "boom\n");
    assert_eq!(found.outputs_note, "outputs not collected: unreachable");
    // Повторная доставка той же команды безвредна.
    fx.commands.handle(&done).await.unwrap();
    let _ = fx.sink.next(1).await;
    assert_eq!(fx.store.get(&job.to_string()).unwrap().exit_code, Some(3));
    let _ = &fx.sshd;
}
