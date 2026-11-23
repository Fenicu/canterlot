//! Носитель готовит исходники, собирает результаты и убирает: `StageSource`, `CollectOutputs`,
//! `CleanupSource` против настоящего непривилегированного `sshd` и настоящих git-репозиториев.
//! Каждая команда идемпотентна: хаб повторяет их после рестарта воркера.

use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, SystemTime},
};

use canterlot_worker::carrier::{
    self, AssignedRunner,
    cloud::{Attachment, CloudConfig, CloudState},
    script,
    stage::{
        CleanupRequest, CollectRequest, Failure, OutputFile, RunnerTarget, StageRequest, Stager,
        StagerSettings, sweep_outputs,
    },
};
use tokio::sync::watch;
use uuid::Uuid;

use crate::sshd::{ALIAS, TestSshd};

const RUNNER_ID: Uuid = Uuid::from_u128(0x1111);

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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Source {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    first: String,
    second: String,
}

fn source_repo() -> Source {
    let dir = tempfile::Builder::new()
        .prefix("src-")
        .tempdir_in("/tmp")
        .unwrap();
    // Репозиторий лежит в подкаталоге: рядом с ним носитель создаст `.canterlot-jobs`.
    let repo = dir.path().join("project");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("file.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let first = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("file.txt"), "two\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "two"]);
    let second = git(&repo, &["rev-parse", "HEAD"]);
    Source {
        _dir: dir,
        repo,
        first,
        second,
    }
}

struct Fixture {
    sshd: TestSshd,
    root: PathBuf,
    outputs: tempfile::TempDir,
    stager: Stager,
    /// Каталог данных носителя у облачного подключения: `known_hosts` и записи живут там.
    _data: Option<tempfile::TempDir>,
}

fn fixture() -> Fixture {
    let sshd = TestSshd::start();
    let root = sshd.path().join("runner");
    fs::create_dir(&root).unwrap();
    let outputs = tempfile::Builder::new()
        .prefix("outputs-")
        .tempdir_in("/tmp")
        .unwrap();
    let target = RunnerTarget {
        alias: ALIAS.to_owned(),
        root: root.clone(),
        host: None,
    };
    let stager = Stager::new(StagerSettings {
        ssh_config: Some(sshd.ssh_config.clone()),
        outputs_root: outputs.path().to_path_buf(),
        targets: Arc::new(move |machine| (machine == RUNNER_ID).then(|| target.clone())),
        ssh_limit: Duration::from_secs(30),
        push_limit: Duration::from_secs(60),
        allowed_roots: vec![PathBuf::from("/tmp")],
    });
    Fixture {
        sshd,
        root,
        outputs,
        stager,
        _data: None,
    }
}

/// Носитель, ведущий облачный раннер: псевдонима нет, есть адрес и DNAT-порт вместо 22,
/// ключ `[runners].ssh_key` и закреплённый `known_hosts`. Список целей — тот же, что строит
/// `session.rs` из состояния облачных подключений.
fn cloud_fixture() -> (Fixture, Uuid) {
    let sshd = TestSshd::start();
    let root = sshd.path().join("runner");
    fs::create_dir(&root).unwrap();
    let outputs = tempfile::Builder::new()
        .prefix("outputs-")
        .tempdir_in("/tmp")
        .unwrap();
    let data = tempfile::Builder::new()
        .prefix("cloud-data-")
        .tempdir_in("/tmp")
        .unwrap();
    let machine = Uuid::now_v7();
    let cloud = Arc::new(CloudState::open(CloudConfig {
        data_dir: data.path().to_path_buf(),
        ssh_key: Some(sshd.client_key().to_path_buf()),
        user: sshd.user().to_owned(),
        ..CloudConfig::default()
    }));
    cloud
        .apply(Attachment {
            window_id: "window-1".into(),
            machine_id: machine,
            host: "127.0.0.1".into(),
            ssh_port: sshd.port(),
            host_key_public: sshd.host_key_public(),
            root_dir: root.clone(),
            max_running: 3,
            lease_slots: 2,
        })
        .unwrap();
    let (_assign, assigned) = watch::channel(vec![AssignedRunner {
        machine_id: machine,
        name: "cloud".into(),
        root_dir: root.clone(),
    }]);
    let stager = Stager::new(StagerSettings {
        ssh_config: None,
        outputs_root: outputs.path().to_path_buf(),
        targets: carrier::targets(Vec::new(), assigned, Some(cloud)),
        ssh_limit: Duration::from_secs(30),
        push_limit: Duration::from_secs(60),
        allowed_roots: vec![PathBuf::from("/tmp")],
    });
    (
        Fixture {
            sshd,
            root,
            outputs,
            stager,
            _data: Some(data),
        },
        machine,
    )
}

fn repo_key(repo: &Path) -> String {
    format!(
        "{}-0123456789ab",
        repo.file_name().unwrap().to_str().unwrap()
    )
}

fn runner_stage(fx: &Fixture, src: &Source, rev: &str, job: Uuid) -> StageRequest {
    StageRequest {
        job_id: job.to_string(),
        repo: src.repo.clone(),
        rev: rev.to_owned(),
        domain: "work".into(),
        target_machine_id: RUNNER_ID.to_string(),
        target_root_dir: fx.root.display().to_string(),
        workdir: fx
            .root
            .join("jobs/work")
            .join(job.to_string())
            .display()
            .to_string(),
        repo_key: repo_key(&src.repo),
    }
}

fn carrier_stage(src: &Source, rev: &str, job: Uuid) -> StageRequest {
    let parent = src.repo.parent().unwrap();
    StageRequest {
        job_id: job.to_string(),
        repo: src.repo.clone(),
        rev: rev.to_owned(),
        domain: "personal".into(),
        target_machine_id: Uuid::from_u128(0x2222).to_string(),
        target_root_dir: String::new(),
        workdir: parent
            .join(".canterlot-jobs/project")
            .join(job.to_string())
            .display()
            .to_string(),
        repo_key: repo_key(&src.repo),
    }
}

fn reason(failure: Failure) -> String {
    failure.reason
}

#[tokio::test]
async fn staging_on_a_runner_lands_the_right_sha_in_the_domain_dir_and_repeats_harmlessly() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = runner_stage(&fx, &src, &src.first, job);
    fx.stager.stage(&request).await.unwrap();
    let workdir = PathBuf::from(&request.workdir);
    assert_eq!(
        fs::read_to_string(workdir.join("file.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
    let bare = fx
        .root
        .join("repos/work")
        .join(format!("{}.git", request.repo_key));
    assert_eq!(
        git(&bare, &["rev-parse", &format!("refs/canterlot/{job}")]),
        src.first
    );
    // Исходный репозиторий не получил ни рабочих копий, ни ссылок.
    assert!(git(&src.repo, &["worktree", "list"]).lines().count() == 1);
    // Повтор: тот же ответ, рабочая копия цела.
    fx.stager.stage(&request).await.unwrap();
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
    // Другая задача на другом sha того же репозитория делит один bare-репозиторий.
    let other = Uuid::now_v7();
    let second = runner_stage(&fx, &src, &src.second, other);
    fx.stager.stage(&second).await.unwrap();
    assert_eq!(
        fs::read_to_string(Path::new(&second.workdir).join("file.txt")).unwrap(),
        "two\n"
    );
}

#[tokio::test]
async fn a_cloud_target_on_a_nonstandard_port_stages_collects_and_cleans() {
    let (fx, machine) = cloud_fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = StageRequest {
        job_id: job.to_string(),
        repo: src.repo.clone(),
        rev: src.first.clone(),
        domain: "work".into(),
        target_machine_id: machine.to_string(),
        target_root_dir: fx.root.display().to_string(),
        workdir: fx
            .root
            .join("jobs/work")
            .join(job.to_string())
            .display()
            .to_string(),
        repo_key: repo_key(&src.repo),
    };
    // Коммит уходит на облачный сервер по адресу и DNAT-порту, не по псевдониму из ssh_config.
    fx.stager.stage(&request).await.unwrap();
    let workdir = PathBuf::from(&request.workdir);
    assert_eq!(
        fs::read_to_string(workdir.join("file.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
    write_outputs(&workdir);
    let collect = collect_request(
        &request,
        job,
        machine,
        &fx.root.display().to_string(),
        &["out/a.txt"],
        1 << 20,
    );
    let outcome = fx.stager.collect(&collect).await.unwrap();
    assert_eq!(
        outcome.files,
        [OutputFile {
            path: "out/a.txt".into(),
            bytes: 4
        }]
    );
    let copied = fx.outputs.path().join(job.to_string()).join("out/a.txt");
    assert_eq!(fs::read_to_string(copied).unwrap(), "aaaa");
    let cleanup = CleanupRequest {
        job_id: request.job_id.clone(),
        target_machine_id: request.target_machine_id.clone(),
        target_root_dir: request.target_root_dir.clone(),
        workdir: request.workdir.clone(),
        repo_key: request.repo_key.clone(),
        domain: request.domain.clone(),
    };
    fx.stager.cleanup(&cleanup).await.unwrap();
    assert!(!workdir.exists());
}

#[tokio::test]
async fn a_repo_reached_through_a_symlink_or_outside_the_allowed_roots_is_refused() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    // Ссылка внутри разрешённого корня на настоящий репозиторий: путь не канонический, и
    // домен по тексту пути не говорит, где репозиторий на самом деле.
    let link = src.repo.parent().unwrap().join("alias");
    symlink(&src.repo, &link).unwrap();
    let on_runner = StageRequest {
        repo: link.clone(),
        ..runner_stage(&fx, &src, &src.first, job)
    };
    assert_eq!(
        reason(fx.stager.stage(&on_runner).await.unwrap_err()),
        "bad_repo"
    );
    let on_carrier = StageRequest {
        repo: link.clone(),
        workdir: link
            .parent()
            .unwrap()
            .join(".canterlot-jobs/alias")
            .join(job.to_string())
            .display()
            .to_string(),
        ..carrier_stage(&src, &src.first, job)
    };
    assert_eq!(
        reason(fx.stager.stage(&on_carrier).await.unwrap_err()),
        "bad_repo"
    );
    // Ссылка в середине пути.
    let dir_link = src
        .repo
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("via-{}", Uuid::now_v7().simple()));
    symlink(src.repo.parent().unwrap(), &dir_link).unwrap();
    let mid = StageRequest {
        repo: dir_link.join("project"),
        ..runner_stage(&fx, &src, &src.first, job)
    };
    assert_eq!(reason(fx.stager.stage(&mid).await.unwrap_err()), "bad_repo");
    fs::remove_file(&dir_link).unwrap();
    // Каноническая запись, но вне разрешённых корней.
    let strict = Stager::new(StagerSettings {
        ssh_config: Some(fx.sshd.ssh_config.clone()),
        outputs_root: fx.outputs.path().to_path_buf(),
        targets: Arc::new(|_| None),
        ssh_limit: Duration::from_secs(30),
        push_limit: Duration::from_secs(60),
        allowed_roots: vec![fx.root.clone()],
    });
    assert_eq!(
        reason(
            strict
                .stage(&runner_stage(&fx, &src, &src.first, job))
                .await
                .unwrap_err()
        ),
        "bad_repo"
    );
    // Ничего не создано на раннере: отказ раньше любого вызова git и ssh.
    assert!(!fx.root.join("repos").exists());
    assert!(!fx.root.join("jobs").exists());
    // Настоящий путь тем же складом работает.
    fx.stager
        .stage(&runner_stage(&fx, &src, &src.first, job))
        .await
        .unwrap();
}

#[tokio::test]
async fn staging_on_the_carrier_makes_a_worktree_next_to_the_repo() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = carrier_stage(&src, &src.first, job);
    fx.stager.stage(&request).await.unwrap();
    let workdir = PathBuf::from(&request.workdir);
    assert_eq!(
        fs::read_to_string(workdir.join("file.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
    fx.stager.stage(&request).await.unwrap();
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
}

#[tokio::test]
async fn a_rerun_after_an_interrupted_stage_completes() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = runner_stage(&fx, &src, &src.first, job);
    let workdir = PathBuf::from(&request.workdir);
    fx.stager.stage(&request).await.unwrap();
    // Воркер умер после регистрации рабочей копии: каталога нет, запись в bare осталась.
    let bare = fx
        .root
        .join("repos/work")
        .join(format!("{}.git", request.repo_key));
    fs::remove_dir_all(&workdir).unwrap();
    assert!(git(&bare, &["worktree", "list"]).contains(&request.workdir));
    fx.stager.stage(&request).await.unwrap();
    assert_eq!(git(&workdir, &["rev-parse", "HEAD"]), src.first);
    // Воркер умер на полпути создания каталога: пустой каталог без записи.
    let again = Uuid::now_v7();
    let partial = runner_stage(&fx, &src, &src.first, again);
    fs::create_dir_all(&partial.workdir).unwrap();
    fx.stager.stage(&partial).await.unwrap();
    assert_eq!(
        git(Path::new(&partial.workdir), &["rev-parse", "HEAD"]),
        src.first
    );
    // То же на носителе.
    let local = carrier_stage(&src, &src.second, Uuid::now_v7());
    fs::create_dir_all(&local.workdir).unwrap();
    fx.stager.stage(&local).await.unwrap();
    assert_eq!(
        git(Path::new(&local.workdir), &["rev-parse", "HEAD"]),
        src.second
    );
}

#[tokio::test]
async fn a_worktree_at_another_sha_is_replaced_not_trusted() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = runner_stage(&fx, &src, &src.first, job);
    fx.stager.stage(&request).await.unwrap();
    // Тот же job_id, но хаб прислал другой rev (повтор команды после исправления).
    let changed = StageRequest {
        rev: src.second.clone(),
        ..request.clone()
    };
    let result = fx.stager.stage(&changed).await;
    // Ссылка `refs/canterlot/<job>` уже указывает на первый sha, и перезаписать её нельзя:
    // команда отказывает, а не отдаёт чужие исходники.
    match result {
        Ok(()) => assert_eq!(
            git(Path::new(&request.workdir), &["rev-parse", "HEAD"]),
            src.second
        ),
        Err(failure) => assert!(!failure.reason.is_empty()),
    }
}

#[tokio::test]
async fn invalid_stage_requests_are_refused_before_any_command_runs() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let good = runner_stage(&fx, &src, &src.first, job);
    let cases: Vec<(&str, StageRequest)> = vec![
        (
            "bad_rev",
            StageRequest {
                rev: "HEAD".into(),
                ..good.clone()
            },
        ),
        (
            "bad_rev",
            StageRequest {
                rev: format!("{}; touch /tmp/pwned", &src.first[..20]),
                ..good.clone()
            },
        ),
        (
            "bad_job_id",
            StageRequest {
                job_id: "../x".into(),
                ..good.clone()
            },
        ),
        (
            "bad_domain",
            StageRequest {
                domain: "other".into(),
                ..good.clone()
            },
        ),
        (
            "bad_repo_key",
            StageRequest {
                repo_key: "a b".into(),
                ..good.clone()
            },
        ),
        (
            "bad_repo_key",
            StageRequest {
                repo_key: "../escape".into(),
                ..good.clone()
            },
        ),
        // Хаб сводит имя к [A-Za-z0-9._-] (`place::repo_key`): «+» и юникод до сюда не доходят.
        (
            "bad_repo_key",
            StageRequest {
                repo_key: "my+repo-0123456789ab".into(),
                ..good.clone()
            },
        ),
        (
            "bad_repo_key",
            StageRequest {
                repo_key: String::new(),
                ..good.clone()
            },
        ),
        (
            "bad_root",
            StageRequest {
                target_root_dir: "/srv/run ner".into(),
                ..good.clone()
            },
        ),
        (
            "bad_workdir",
            StageRequest {
                workdir: format!("{}/jobs/work/other", fx.root.display()),
                ..good.clone()
            },
        ),
        (
            "bad_workdir",
            StageRequest {
                workdir: format!("{}/jobs/personal/{job}", fx.root.display()),
                ..good.clone()
            },
        ),
        (
            "bad_workdir",
            StageRequest {
                workdir: format!("{}/jobs/work/../../x/{job}", fx.root.display()),
                ..good.clone()
            },
        ),
        (
            "bad_workdir",
            StageRequest {
                workdir: format!("/tmp/{job}"),
                ..good.clone()
            },
        ),
        (
            "bad_repo",
            StageRequest {
                repo: PathBuf::from("relative/repo"),
                ..good.clone()
            },
        ),
        (
            "unknown_target",
            StageRequest {
                target_machine_id: Uuid::from_u128(0x9999).to_string(),
                ..good.clone()
            },
        ),
        (
            "bad_machine_id",
            StageRequest {
                target_machine_id: "nope".into(),
                ..good.clone()
            },
        ),
        (
            "bad_root",
            StageRequest {
                target_root_dir: "/somewhere/else".into(),
                workdir: format!("/somewhere/else/jobs/work/{job}"),
                ..good.clone()
            },
        ),
    ];
    for (want, request) in cases {
        let failure = fx.stager.stage(&request).await.unwrap_err();
        assert_eq!(reason(failure), want, "{request:?}");
    }
    assert!(!Path::new("/tmp/pwned").exists());
    // Ничего не создано на раннере.
    assert!(!fx.root.join("repos").exists());
    // Carrier target: workdir вне `.canterlot-jobs` рядом с репозиторием.
    let carrier = carrier_stage(&src, &src.first, job);
    let outside = StageRequest {
        workdir: format!("/tmp/{job}"),
        ..carrier.clone()
    };
    assert_eq!(
        reason(fx.stager.stage(&outside).await.unwrap_err()),
        "bad_workdir"
    );
    let unknown = StageRequest {
        rev: "0".repeat(40),
        ..carrier
    };
    assert_eq!(
        reason(fx.stager.stage(&unknown).await.unwrap_err()),
        "unknown_rev"
    );
    let _ = script::valid_root(Path::new("/srv/runner"));
}

fn write_outputs(workdir: &Path) {
    fs::create_dir_all(workdir.join("out/sub")).unwrap();
    fs::write(workdir.join("out/a.txt"), "aaaa").unwrap();
    fs::write(workdir.join("out/sub/b.txt"), "bbbbbb").unwrap();
    fs::write(workdir.join("single.log"), "log\n").unwrap();
}

fn collect_request(
    request: &StageRequest,
    job: Uuid,
    machine: Uuid,
    root: &str,
    paths: &[&str],
    cap: u64,
) -> CollectRequest {
    CollectRequest {
        job_id: job.to_string(),
        target_machine_id: machine.to_string(),
        target_root_dir: root.to_owned(),
        workdir: request.workdir.clone(),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        cap_bytes: cap,
    }
}

async fn staged(fx: &Fixture, src: &Source, on_runner: bool) -> (StageRequest, Uuid, CollectBase) {
    let job = Uuid::now_v7();
    let request = if on_runner {
        runner_stage(fx, src, &src.first, job)
    } else {
        carrier_stage(src, &src.first, job)
    };
    fx.stager.stage(&request).await.unwrap();
    write_outputs(Path::new(&request.workdir));
    let base = if on_runner {
        CollectBase {
            machine: RUNNER_ID,
            root: fx.root.display().to_string(),
        }
    } else {
        CollectBase {
            machine: Uuid::from_u128(0x2222),
            root: String::new(),
        }
    };
    (request, job, base)
}

struct CollectBase {
    machine: Uuid,
    root: String,
}

#[tokio::test]
async fn collecting_copies_regular_files_and_lists_missing_paths() {
    for on_runner in [true, false] {
        let fx = fixture();
        let src = source_repo();
        let (request, job, base) = staged(&fx, &src, on_runner).await;
        let collect = collect_request(
            &request,
            job,
            base.machine,
            &base.root,
            &["out", "single.log", "nothing/here"],
            1 << 20,
        );
        let outcome = fx.stager.collect(&collect).await.unwrap();
        let dir = fx.outputs.path().join(job.to_string());
        assert_eq!(outcome.local_dir, dir, "runner={on_runner}");
        assert!(!outcome.truncated);
        assert_eq!(
            fs::read_to_string(dir.join("out/sub/b.txt")).unwrap(),
            "bbbbbb"
        );
        assert_eq!(fs::read_to_string(dir.join("single.log")).unwrap(), "log\n");
        let mut files: Vec<(String, u64)> = outcome
            .files
            .iter()
            .map(|file| (file.path.clone(), file.bytes))
            .collect();
        files.sort();
        assert_eq!(
            files,
            [
                ("out/a.txt".to_owned(), 4),
                ("out/sub/b.txt".to_owned(), 6),
                ("single.log".to_owned(), 4)
            ],
            "runner={on_runner}"
        );
        assert_eq!(outcome.missing, ["nothing/here"]);
        // Повтор даёт тот же итог.
        let again = fx.stager.collect(&collect).await.unwrap();
        assert_eq!(again.files.len(), 3);
    }
}

#[tokio::test]
async fn collecting_refuses_symlinks_and_special_files() {
    for on_runner in [true, false] {
        let fx = fixture();
        let src = source_repo();
        let (request, job, base) = staged(&fx, &src, on_runner).await;
        let workdir = PathBuf::from(&request.workdir);
        let secret = fx.sshd.path().join("secret.txt");
        fs::write(&secret, "do not copy").unwrap();
        symlink(&secret, workdir.join("out/link.txt")).unwrap();
        symlink(fx.sshd.path(), workdir.join("out/linkdir")).unwrap();
        symlink(&secret, workdir.join("top.txt")).unwrap();
        let fifo = workdir.join("out/pipe");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let collect = collect_request(
            &request,
            job,
            base.machine,
            &base.root,
            &["out", "top.txt", "out/linkdir/secret.txt"],
            1 << 20,
        );
        let outcome = fx.stager.collect(&collect).await.unwrap();
        let dir = fx.outputs.path().join(job.to_string());
        let mut got: Vec<&str> = outcome
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        got.sort();
        assert_eq!(got, ["out/a.txt", "out/sub/b.txt"], "runner={on_runner}");
        assert!(!dir.join("out/link.txt").exists());
        assert!(!dir.join("out/linkdir").exists());
        assert!(!dir.join("top.txt").exists());
        assert!(!dir.join("out/pipe").exists());
        assert!(!dir.join("out/linkdir/secret.txt").exists());
        assert!(
            outcome.skipped.iter().any(|path| path == "top.txt"),
            "{:?}",
            outcome.skipped
        );
        assert!(
            outcome
                .skipped
                .iter()
                .any(|path| path == "out/linkdir/secret.txt"),
            "{:?}",
            outcome.skipped
        );
    }
}

#[tokio::test]
async fn the_total_cap_truncates_in_path_order() {
    for on_runner in [true, false] {
        let fx = fixture();
        let src = source_repo();
        let (request, job, base) = staged(&fx, &src, on_runner).await;
        // a.txt (4) влезает, b.txt (6) уже нет: 4 + 6 > 8.
        let collect = collect_request(&request, job, base.machine, &base.root, &["out"], 8);
        let outcome = fx.stager.collect(&collect).await.unwrap();
        assert!(outcome.truncated, "runner={on_runner}");
        let got: Vec<&str> = outcome
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(got, ["out/a.txt"]);
        let dir = fx.outputs.path().join(job.to_string());
        assert!(dir.join("out/a.txt").exists());
        assert!(!dir.join("out/sub/b.txt").exists());
    }
}

#[tokio::test]
async fn invalid_collect_requests_are_refused() {
    let fx = fixture();
    let src = source_repo();
    let (request, job, base) = staged(&fx, &src, true).await;
    for paths in [
        vec!["../escape"],
        vec!["/etc/passwd"],
        vec!["out/../../x"],
        vec!["a b"],
        vec!["x;touch /tmp/pwned2"],
        vec![""],
        vec!["-rf"],
    ] {
        let collect = collect_request(&request, job, base.machine, &base.root, &paths, 100);
        assert_eq!(
            reason(fx.stager.collect(&collect).await.unwrap_err()),
            "bad_path",
            "{paths:?}"
        );
    }
    let outside = CollectRequest {
        workdir: format!("/tmp/{job}"),
        ..collect_request(&request, job, base.machine, &base.root, &["out"], 100)
    };
    assert_eq!(
        reason(fx.stager.collect(&outside).await.unwrap_err()),
        "bad_workdir"
    );
    assert!(!Path::new("/tmp/pwned2").exists());
}

#[tokio::test]
async fn cleanup_removes_only_that_worktree_and_ref_and_repeats() {
    for on_runner in [true, false] {
        let fx = fixture();
        let src = source_repo();
        let first_job = Uuid::now_v7();
        let second_job = Uuid::now_v7();
        let (a, b) = if on_runner {
            (
                runner_stage(&fx, &src, &src.first, first_job),
                runner_stage(&fx, &src, &src.second, second_job),
            )
        } else {
            (
                carrier_stage(&src, &src.first, first_job),
                carrier_stage(&src, &src.second, second_job),
            )
        };
        fx.stager.stage(&a).await.unwrap();
        fx.stager.stage(&b).await.unwrap();
        fs::write(Path::new(&a.workdir).join("untracked.bin"), "x").unwrap();
        let cleanup = CleanupRequest {
            job_id: a.job_id.clone(),
            target_machine_id: a.target_machine_id.clone(),
            target_root_dir: a.target_root_dir.clone(),
            workdir: a.workdir.clone(),
            repo_key: a.repo_key.clone(),
            domain: a.domain.clone(),
        };
        fx.stager.cleanup(&cleanup).await.unwrap();
        assert!(!Path::new(&a.workdir).exists(), "runner={on_runner}");
        assert!(Path::new(&b.workdir).join("file.txt").exists());
        let repo_dir = if on_runner {
            fx.root
                .join("repos/work")
                .join(format!("{}.git", a.repo_key))
        } else {
            src.repo.clone()
        };
        let refs = git(&repo_dir, &["for-each-ref", "refs/canterlot"]);
        assert!(!refs.contains(&first_job.to_string()), "{refs}");
        if on_runner {
            assert!(refs.contains(&second_job.to_string()), "{refs}");
        }
        // Второй вызов безвреден.
        fx.stager.cleanup(&cleanup).await.unwrap();
        assert!(Path::new(&b.workdir).join("file.txt").exists());
    }
}

fn has_object(repo: &Path, sha: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn cleanup_prunes_the_sources_of_finished_jobs_from_the_runner_repo() {
    let fx = fixture();
    let src = source_repo();
    let live = Uuid::now_v7();
    let done = Uuid::now_v7();
    let keep = runner_stage(&fx, &src, &src.second, live);
    let finished = runner_stage(&fx, &src, &src.first, done);
    let bare = fx
        .root
        .join("repos/work")
        .join(format!("{}.git", keep.repo_key));
    fx.stager.stage(&keep).await.unwrap();
    fx.stager.stage(&finished).await.unwrap();
    let cleanup = |request: &StageRequest| CleanupRequest {
        job_id: request.job_id.clone(),
        target_machine_id: request.target_machine_id.clone(),
        target_root_dir: request.target_root_dir.clone(),
        workdir: request.workdir.clone(),
        repo_key: request.repo_key.clone(),
        domain: request.domain.clone(),
    };
    // Пока рядом живёт другая задача того же репозитория, её объекты на месте.
    fx.stager.cleanup(&cleanup(&finished)).await.unwrap();
    assert!(has_object(&bare, &src.second));
    assert!(Path::new(&keep.workdir).join("file.txt").exists());
    // Последняя задача ушла: исходники не остаются на раннере, сам репозиторий цел.
    fx.stager.cleanup(&cleanup(&keep)).await.unwrap();
    assert!(
        !has_object(&bare, &src.second) && !has_object(&bare, &src.first),
        "the sources of finished jobs must not linger on the runner"
    );
    assert_eq!(git(&bare, &["rev-parse", "--is-bare-repository"]), "true");
    // Следующая задача снова ставится в тот же репозиторий.
    let again = runner_stage(&fx, &src, &src.second, Uuid::now_v7());
    fx.stager.stage(&again).await.unwrap();
    assert_eq!(
        fs::read_to_string(Path::new(&again.workdir).join("file.txt")).unwrap(),
        "two\n"
    );
}

#[tokio::test]
async fn cleanup_rejects_a_workdir_outside_the_job_tree_and_removes_no_files() {
    let fx = fixture();
    let src = source_repo();
    let job = Uuid::now_v7();
    let request = runner_stage(&fx, &src, &src.first, job);
    fx.stager.stage(&request).await.unwrap();
    let victim = fx.sshd.path().join("keep");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("file"), "x").unwrap();
    let cleanup = CleanupRequest {
        job_id: job.to_string(),
        target_machine_id: RUNNER_ID.to_string(),
        target_root_dir: fx.root.display().to_string(),
        workdir: victim.display().to_string(),
        repo_key: request.repo_key.clone(),
        domain: "work".into(),
    };
    assert_eq!(
        reason(fx.stager.cleanup(&cleanup).await.unwrap_err()),
        "bad_workdir"
    );
    assert!(victim.join("file").exists());
    assert!(Path::new(&request.workdir).exists());
}

#[test]
fn the_retention_sweep_removes_only_old_job_dirs() {
    let outputs = tempfile::Builder::new()
        .prefix("sweep-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = outputs.path();
    let old = root.join(Uuid::now_v7().to_string());
    let fresh = root.join(Uuid::now_v7().to_string());
    let foreign = root.join("not-a-job");
    let elsewhere = tempfile::Builder::new()
        .prefix("sweep-target-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::write(elsewhere.path().join("precious"), "x").unwrap();
    let linked = root.join(Uuid::now_v7().to_string());
    for dir in [&old, &fresh, &foreign] {
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/file"), "x").unwrap();
    }
    // Старый каталог содержит ссылку наружу: уборка не идёт по ней.
    symlink(elsewhere.path(), old.join("escape")).unwrap();
    // Ссылка на каталог вместо каталога задачи не трогается и не разыменовывается.
    symlink(elsewhere.path(), &linked).unwrap();
    // Условное «сейчас»: время изменения каталогов задаётся явно от него.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let aged = |path: &Path, age: Duration| {
        let file = fs::File::open(path).unwrap();
        file.set_modified(now - age).unwrap();
    };
    aged(&old, Duration::from_secs(10 * 24 * 3600));
    aged(&foreign, Duration::from_secs(10 * 24 * 3600));
    aged(&fresh, Duration::from_secs(3600));
    let removed = sweep_outputs(root, Duration::from_secs(7 * 24 * 3600), now);
    assert_eq!(removed, 1);
    assert!(!old.exists());
    assert!(fresh.join("nested/file").exists());
    assert!(foreign.join("nested/file").exists());
    assert!(elsewhere.path().join("precious").exists());
    assert!(
        fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // Повтор и несуществующий корень безвредны.
    assert_eq!(
        sweep_outputs(root, Duration::from_secs(7 * 24 * 3600), now),
        0
    );
    assert_eq!(
        sweep_outputs(&root.join("missing"), Duration::from_secs(1), now),
        0
    );
}
