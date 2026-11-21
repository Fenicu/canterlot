//! Носитель в воркере против настоящего хаба: возможность `runners_v1`, допуск несущей,
//! токен раннера по запросу, список раннеров и наблюдения идут по настоящему потоку управления,
//! а воркер раннера на непривилегированном `sshd` приходит к хабу через проброс.

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use canterlot_hub::{
    config::HubTimers,
    machines::{self, RunnerSpec},
    session::Availability,
    testkit::spawn_test_hub,
};
use canterlot_worker::{
    config::{RunnerSsh, RunnersConfig, WorkerConfig, WorkerTimers},
    session::{SessionState, Worker},
    store::WorkerStore,
};
use sqlx::PgPool;
use tokio::{sync::watch, time::timeout};

use crate::sshd::{ALIAS, TestSshd};

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");

#[test]
fn runners_v1_is_advertised_only_by_a_worker_that_carries_runners() {
    let base = "hub_url = \"https://hub.example\"\nca_cert = \"/ca\"\ntoken_file = \"/token\"\n\
                machine_id = \"019a1b10-0000-7000-8000-000000000001\"\n\
                installation_id = \"019a1b10-0000-7000-8000-000000000002\"\n\
                restore_generation = \"019a1b10-0000-7000-8000-000000000003\"\n\
                data_dir = \"/data\"\n";
    let plain: WorkerConfig = toml::from_str(base).unwrap();
    let carrier: WorkerConfig = toml::from_str(&format!(
        "{base}[[runners.ssh]]\nmachine = \"runner-a\"\nssh = \"build-host\"\n"
    ))
    .unwrap();
    let has = |config: &WorkerConfig| {
        canterlot_worker::session::advertised_capabilities(config)
            .iter()
            .any(|cap| cap == "runners_v1")
    };
    assert!(!has(&plain));
    assert!(has(&carrier));
    // Сама сборка знает возможность: по ней выкладка сверяет воркер с выпуском.
    assert!(canterlot_worker::session::OPTIONAL_CAPABILITIES.contains(&"runners_v1"));
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn carrier_worker_brings_a_runner_online_through_the_real_hub(pool: PgPool) {
    let hub = spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_millis(25),
            ..Default::default()
        },
    )
    .await;
    let sshd = TestSshd::start();
    let root = sshd.path().join("runner-a");
    let owner = hub.installation.owner_id;
    let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
    machines::set_carrier(&pool, carrier.machine_id, true)
        .await
        .unwrap();
    let runner = machines::add_runner(
        &pool,
        owner,
        "runner-a",
        &RunnerSpec {
            parent: carrier.machine_id,
            root_dir: root.display().to_string(),
            rank: 20,
            labels: vec!["build-host".into()],
        },
    )
    .await
    .unwrap();
    let mut changes = hub.registry.subscribe_changes();

    // Воркер-носитель: обычная машина хаба с `[[runners.ssh]]`.
    let directory = tempfile::tempdir().unwrap();
    let token_file = directory.path().join("token");
    fs::write(&token_file, &carrier.token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let ca_cert = directory.path().join("ca.pem");
    fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
    let data_dir = directory.path().join("store");
    let store = WorkerStore::open(&data_dir).unwrap();
    let config = WorkerConfig {
        secrets: Default::default(),
        memory: Default::default(),
        jobs: Default::default(),
        machine: Default::default(),
        attachments: Default::default(),
        claude_code: None,
        codex: None,
        hub_url: format!("https://localhost:{}", hub.addr.port()),
        hub_dial: None,
        ca_cert,
        token_file,
        machine_id: carrier.machine_id,
        installation_id: hub.installation.installation_id,
        restore_generation: hub.installation.restore_generation,
        data_dir,
        timers: WorkerTimers {
            handshake: Duration::from_secs(2),
            heartbeat: Duration::from_millis(40),
            heartbeat_ack: Duration::from_millis(200),
            range_ack: Duration::from_millis(300),
            backoff_min: Duration::from_millis(200),
            backoff_max: Duration::from_millis(400),
            backoff_reset_after: Duration::from_millis(500),
            ..Default::default()
        },
        execution: Default::default(),
        opencode: None,
        legacy_ignored: None,
        runners: RunnersConfig {
            ssh_key: None,
            ssh_config: Some(sshd.ssh_config.clone()),
            worker_binary: Some(WORKER.into()),
            ssh: vec![RunnerSsh {
                machine: "runner-a".into(),
                ssh: ALIAS.into(),
                max_running: 2,
                lease_slots: 1,
            }],
        },
    };
    let runtime_dir = config.data_dir.join("runtime");
    let worker = Worker::new(config, store).with_runtime_dir(runtime_dir);
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));

    // Раннер приходит к хабу сам: список от хаба, токен ротацией, бинарник, конфиг, проброс.
    timeout(Duration::from_secs(40), async {
        loop {
            let (machine, availability) = changes.recv().await.unwrap();
            if machine == runner.machine_id && availability == Availability::Available {
                break;
            }
        }
    })
    .await
    .expect("the runner worker connects to the hub through the carrier");
    assert!(matches!(
        *state.borrow_and_update(),
        SessionState::Live | SessionState::ControlOnly
    ));
    assert_eq!(
        fs::read(root.join("bin/canterlot-worker")).unwrap(),
        fs::read(WORKER).unwrap()
    );
    // Токен, выданный ротацией, лежит на раннере и отличается от выданного при `machine add`.
    let token = fs::read_to_string(root.join("token")).unwrap();
    assert!(!token.is_empty() && token != runner.token);
    assert_eq!(
        fs::metadata(root.join("token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    // Наблюдения несущей дошли до хаба: строка есть и говорит «достижим».
    let (reachable, failures): (bool, i32) = sqlx::query_as(
        "select reachable, consecutive_failures from runner_observations where machine_id = $1",
    )
    .bind(runner.machine_id)
    .fetch_one(&pool)
    .await
    .expect("an observation reached the hub");
    assert!(reachable);
    assert_eq!(failures, 0);

    stop.send(true).unwrap();
    timeout(Duration::from_secs(30), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Воркер раннера остаётся жить после выхода носителя; убирает его тест.
    if let Some(pid) = fs::read_to_string(root.join("state/worker.pid"))
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status();
    }
    hub.shutdown.send(()).unwrap();
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
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

/// Задача владельца с `source` и `outputs` от `job run` до `OwnerJobFinished` через настоящий
/// хаб: размещение на раннере, подготовка коммита, запуск воркером раннера, сбор результатов,
/// уборка и итог в команде, которая ждала.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn an_owner_job_with_source_and_outputs_runs_on_the_runner_end_to_end(pool: PgPool) {
    use canterlot_worker::owner_jobs::client::{RunArgs, execute};
    let hub = spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_millis(25),
            ..Default::default()
        },
    )
    .await;
    let sshd = TestSshd::start();
    let root = sshd.path().join("runner-a");
    let owner = hub.installation.owner_id;
    let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
    machines::set_carrier(&pool, carrier.machine_id, true)
        .await
        .unwrap();
    let runner = machines::add_runner(
        &pool,
        owner,
        "runner-a",
        &RunnerSpec {
            parent: carrier.machine_id,
            root_dir: root.display().to_string(),
            rank: 20,
            labels: vec!["build-host".into()],
        },
    )
    .await
    .unwrap();
    let mut changes = hub.registry.subscribe_changes();

    let directory = tempfile::tempdir().unwrap();
    let token_file = directory.path().join("token");
    fs::write(&token_file, &carrier.token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let ca_cert = directory.path().join("ca.pem");
    fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
    let data_dir = directory.path().join("store");
    let store = WorkerStore::open(&data_dir).unwrap();
    let config = WorkerConfig {
        secrets: Default::default(),
        memory: Default::default(),
        jobs: Default::default(),
        machine: Default::default(),
        attachments: Default::default(),
        claude_code: None,
        codex: None,
        hub_url: format!("https://localhost:{}", hub.addr.port()),
        hub_dial: None,
        ca_cert,
        token_file,
        machine_id: carrier.machine_id,
        installation_id: hub.installation.installation_id,
        restore_generation: hub.installation.restore_generation,
        data_dir: data_dir.clone(),
        timers: WorkerTimers {
            handshake: Duration::from_secs(2),
            heartbeat: Duration::from_millis(40),
            heartbeat_ack: Duration::from_millis(200),
            range_ack: Duration::from_millis(300),
            backoff_min: Duration::from_millis(200),
            backoff_max: Duration::from_millis(400),
            backoff_reset_after: Duration::from_millis(500),
            ..Default::default()
        },
        execution: Default::default(),
        opencode: None,
        legacy_ignored: None,
        runners: RunnersConfig {
            ssh_key: None,
            ssh_config: Some(sshd.ssh_config.clone()),
            worker_binary: Some(WORKER.into()),
            ssh: vec![RunnerSsh {
                machine: "runner-a".into(),
                ssh: ALIAS.into(),
                max_running: 2,
                lease_slots: 1,
            }],
        },
    };
    let runtime_dir = config.data_dir.join("runtime");
    let socket = canterlot_worker::owner_jobs::socket_path(Some(&runtime_dir), &data_dir);
    let worker = Worker::new(config, store).with_runtime_dir(runtime_dir);
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    timeout(Duration::from_secs(40), async {
        loop {
            let (machine, availability) = changes.recv().await.unwrap();
            if machine == runner.machine_id && availability == Availability::Available {
                break;
            }
        }
    })
    .await
    .expect("the runner worker connects to the hub through the carrier");

    // Возможности раннера записываются отчётом машины чуть позже «на связи»: до этого хаб
    // не считает его годным для задач, и задача ушла бы на саму несущую.
    timeout(Duration::from_secs(40), async {
        loop {
            let capable: bool = sqlx::query_scalar(
                "select 'jobs_inplace_v1' = any(capabilities) from machines where id = $1",
            )
            .bind(runner.machine_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            if capable {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the runner reports its capabilities");

    // Репозиторий владельца на машине несущей.
    let source = tempfile::Builder::new()
        .prefix("ownerjob-src-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = source.path().join("project");
    fs::create_dir(&repo).unwrap();
    git_in(&repo, &["init", "-q"]);
    fs::write(repo.join("marker.txt"), "from the repo\n").unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "one"]);
    let sha = git_in(&repo, &["rev-parse", "HEAD"]);

    let run = |shell: &str| {
        let args = RunArgs {
            title: "end to end".into(),
            source: Some(format!("{}@{sha}", repo.display())),
            outputs: vec!["result".into()],
            wait: true,
            command: vec![shell.into()],
            ..RunArgs::default()
        };
        let socket = socket.clone();
        async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = timeout(
                Duration::from_secs(120),
                execute(
                    args,
                    Some(socket),
                    Duration::from_secs(30),
                    &mut out,
                    &mut err,
                ),
            )
            .await
            .expect("the owner job finished in time");
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }
    };

    let (code, out, err) = run(
        "mkdir -p result && cat marker.txt > result/marker.txt && git rev-parse HEAD > result/rev.txt && echo tail-line",
    )
    .await;
    let row: Option<String> = sqlx::query_scalar(
        "select concat_ws(' | ', state, outputs_note, convert_from(tail, 'UTF8'), workdir)
           from jobs where owner_job order by created_at desc limit 1",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}\njob row: {row:?}");
    assert!(out.contains("tail-line"), "{out}");
    let dir_line = err
        .lines()
        .find_map(|line| line.strip_prefix("canterlot job: outputs: "))
        .unwrap_or_else(|| panic!("no outputs line: {err}"));
    let dir = std::path::Path::new(dir_line);
    assert_eq!(dir.parent().unwrap(), data_dir.join("job-outputs"));
    assert_eq!(
        fs::read_to_string(dir.join("result/marker.txt")).unwrap(),
        "from the repo\n"
    );
    // Задача шла на раннере на ровно том коммите.
    assert_eq!(
        fs::read_to_string(dir.join("result/rev.txt"))
            .unwrap()
            .trim(),
        sha
    );
    let (machine, state): (Option<uuid::Uuid>, String) = sqlx::query_as(
        "select machine_id, state from jobs where owner_job order by created_at desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(machine, Some(runner.machine_id));
    assert_eq!(state, "succeeded");
    // Рабочая копия и ссылка на раннере убраны, исходный репозиторий не тронут.
    let leftovers = fs::read_dir(root.join("jobs/personal")).unwrap().count();
    assert_eq!(leftovers, 0, "the runner worktree was removed");
    assert_eq!(git_in(&repo, &["worktree", "list"]).lines().count(), 1);

    // Код выхода команды доходит до `job run --wait`.
    let (code, _, err) = run("exit 3").await;
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("failed"), "{err}");

    stop.send(true).unwrap();
    timeout(Duration::from_secs(30), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Some(pid) = fs::read_to_string(root.join("state/worker.pid"))
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status();
    }
    hub.shutdown.send(()).unwrap();
}
