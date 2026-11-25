//! Раннеры как машины на стороне хаба (этап B1): создание строк `role = 'runner'`, несущая
//! машина и её допуск, наблюдения, доступность и отзыв.

use std::process::Command;

use canterlot_hub::{
    machines::{self, AddRunnerError, RunnerAvailability, RunnerFacts, RunnerRefusal, RunnerSpec},
    session::Registry,
    testkit,
};
use chrono::Utc;
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(crate) async fn owner(pool: &PgPool) -> Uuid {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    owner
}

fn spec(parent: Uuid) -> RunnerSpec {
    RunnerSpec {
        parent,
        root_dir: "/srv/runner".into(),
        rank: 20,
        labels: vec!["build-host".into(), "kernel-6.12".into()],
    }
}

pub(crate) struct Fleet {
    pub owner: Uuid,
    pub carrier: Uuid,
    pub runner: Uuid,
}

/// Несущая `carrier` (флаг включён) и её раннер `runner-a`.
pub(crate) async fn fleet(pool: &PgPool) -> Fleet {
    let owner = owner(pool).await;
    let carrier = machines::add(pool, owner, "carrier").await.unwrap();
    machines::set_carrier(pool, carrier.machine_id, true)
        .await
        .unwrap();
    let runner = machines::add_runner(pool, owner, "runner-a", &spec(carrier.machine_id))
        .await
        .unwrap();
    Fleet {
        owner,
        carrier: carrier.machine_id,
        runner: runner.machine_id,
    }
}

// ───────── Создание ─────────

#[tokio::test]
async fn add_runner_creates_a_runner_row_that_accepts_remote_work() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let owner = owner(&pool).await;
    let parent = machines::add(&pool, owner, "carrier").await?;
    let runner = machines::add_runner(&pool, owner, "runner-a", &spec(parent.machine_id)).await?;
    assert_eq!(
        machines::verify(&pool, &runner.token).await?,
        Some(runner.machine_id)
    );
    let row: (
        String,
        Option<Uuid>,
        Option<String>,
        i32,
        Vec<String>,
        bool,
        bool,
    ) = sqlx::query_as(
        "select role, parent_machine_id, root_dir, rank, labels, is_carrier, accepts_remote_work
             from machines where id = $1",
    )
    .bind(runner.machine_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (
            "runner".into(),
            Some(parent.machine_id),
            Some("/srv/runner".into()),
            20,
            vec!["build-host".into(), "kernel-6.12".into()],
            false,
            true
        )
    );
    // Обычная машина остаётся `worker`, без родителя и без несущей.
    let plain: (String, Option<Uuid>, bool, bool) = sqlx::query_as(
        "select role, parent_machine_id, is_carrier, accepts_remote_work from machines where id = $1",
    )
    .bind(parent.machine_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(plain, ("worker".into(), None, false, false));
    Ok(())
}

#[tokio::test]
async fn add_runner_validates_parent_and_root_dir() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let owner = owner(&pool).await;
    let parent = machines::add(&pool, owner, "carrier").await?;
    let runner = machines::add_runner(&pool, owner, "runner-a", &spec(parent.machine_id)).await?;

    // Родителя нет.
    let missing = machines::add_runner(&pool, owner, "r", &spec(Uuid::now_v7())).await;
    assert!(matches!(missing, Err(AddRunnerError::ParentNotFound)));
    // Родитель сам раннер.
    let nested = machines::add_runner(&pool, owner, "r", &spec(runner.machine_id)).await;
    assert!(matches!(nested, Err(AddRunnerError::ParentNotWorker)));
    // Корень: относительный, ненормализованный, пустой.
    for bad in [
        "relative/dir",
        "/srv/../etc",
        "/srv//runner",
        "/srv/./runner",
        "/srv/runner/",
        "",
        "/",
        // Знаки, которым нужны кавычки, и длиннее 90 байт (путь сокета не влезет в `sun_path`).
        "/srv/run ner",
        "/srv/ru$ner",
        &format!("/srv/{}", "a".repeat(86)),
    ] {
        let mut s = spec(parent.machine_id);
        s.root_dir = bad.into();
        let result = machines::add_runner(&pool, owner, "r", &s).await;
        assert!(
            matches!(result, Err(AddRunnerError::InvalidRootDir)),
            "{bad:?} must be refused"
        );
    }
    // Ровно 90 байт ещё проходит.
    let mut edge = spec(parent.machine_id);
    edge.root_dir = format!("/srv/{}", "a".repeat(85));
    assert_eq!(edge.root_dir.len(), 90);
    machines::add_runner(&pool, owner, "edge", &edge).await?;
    // Метки: пустая, с пробелом, слишком длинная.
    for bad in ["", "a b", &"x".repeat(65)] {
        let mut s = spec(parent.machine_id);
        s.labels = vec![bad.to_owned()];
        let result = machines::add_runner(&pool, owner, "r", &s).await;
        assert!(
            matches!(result, Err(AddRunnerError::InvalidLabels)),
            "{bad:?}"
        );
    }
    // Отозванный родитель.
    machines::revoke(&pool, parent.machine_id).await?;
    let revoked = machines::add_runner(&pool, owner, "r", &spec(parent.machine_id)).await;
    assert!(matches!(revoked, Err(AddRunnerError::ParentRevoked)));
    // Ни одной лишней строки: неудачные попытки откатились целиком.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from machines")
            .fetch_one(&pool)
            .await?,
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from machine_credentials")
            .fetch_one(&pool)
            .await?,
        3
    );
    Ok(())
}

#[tokio::test]
async fn the_runner_shape_is_enforced_by_the_database() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let owner = owner(&pool).await;
    let parent = machines::add(&pool, owner, "carrier").await?;
    let no_parent = sqlx::query("insert into machines (id, owner_id, name, role, root_dir) values ($1, $2, 'x', 'runner', '/srv/x')")
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(&pool)
        .await;
    assert!(no_parent.is_err(), "a runner needs a parent");
    let no_root = sqlx::query("insert into machines (id, owner_id, name, role, parent_machine_id) values ($1, $2, 'x', 'runner', $3)")
        .bind(Uuid::now_v7())
        .bind(owner)
        .bind(parent.machine_id)
        .execute(&pool)
        .await;
    assert!(no_root.is_err(), "a runner needs a root dir");
    let bad_role = sqlx::query(
        "insert into machines (id, owner_id, name, role) values ($1, $2, 'x', 'cloud')",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .execute(&pool)
    .await;
    assert!(bad_role.is_err(), "unknown role");
    Ok(())
}

#[tokio::test]
async fn set_carrier_marks_only_worker_machines() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    let is_carrier = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, bool>("select is_carrier from machines where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert!(is_carrier(f.carrier).await);
    machines::set_carrier(&pool, f.carrier, false).await?;
    assert!(!is_carrier(f.carrier).await);
    machines::set_carrier(&pool, f.carrier, true).await?;
    // Раннер не может быть несущей.
    assert!(matches!(
        machines::set_carrier(&pool, f.runner, true).await,
        Err(machines::SetCarrierError::IsRunner)
    ));
    assert!(!is_carrier(f.runner).await);
    assert!(matches!(
        machines::set_carrier(&pool, Uuid::now_v7(), true).await,
        Err(machines::SetCarrierError::NotFound)
    ));
    Ok(())
}

// ───────── Командная строка ─────────

#[sqlx::test(migrations = false)]
async fn cli_adds_runners_and_marks_carriers(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("hub.toml");
    let url = pool.connect_options().to_url_lossy().to_string();
    std::fs::write(dir.path().join("database-url"), &url)?;
    std::fs::write(
        &config,
        "database_url_file = 'database-url'\ngrpc_listen = '127.0.0.1:0'\ntls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n",
    )?;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
            .args(args)
            .arg("--config")
            .arg(&config)
            .output()
    };
    assert!(run(&["init"])?.status.success());
    let added = run(&["machine", "add", "--name", "carrier"])?;
    assert!(added.status.success());
    let text = String::from_utf8(added.stdout)?;
    let carrier = Uuid::parse_str(
        text.lines()
            .next()
            .unwrap()
            .strip_prefix("machine_id=")
            .unwrap(),
    )?;
    let carrier_text = carrier.to_string();

    let set = run(&["machine", "set-carrier", "--machine-id", &carrier_text])?;
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(set.stdout.is_empty() && set.stderr.is_empty());
    assert!(
        sqlx::query_scalar::<_, bool>("select is_carrier from machines where id = $1")
            .bind(carrier)
            .fetch_one(&pool)
            .await?
    );

    let runner = run(&[
        "machine",
        "add",
        "--name",
        "runner-a",
        "--role",
        "runner",
        "--parent",
        &carrier_text,
        "--root-dir",
        "/srv/runner",
        "--rank",
        "20",
        "--labels",
        "build-host,kernel-6.12",
    ])?;
    assert!(
        runner.status.success(),
        "{}",
        String::from_utf8_lossy(&runner.stderr)
    );
    assert!(runner.stderr.is_empty());
    let text = String::from_utf8(runner.stdout)?;
    assert_eq!(
        text.lines().count(),
        4,
        "the same four lines as a worker: {text}"
    );
    let runner_id = Uuid::parse_str(
        text.lines()
            .next()
            .unwrap()
            .strip_prefix("machine_id=")
            .unwrap(),
    )?;
    let row: (String, Option<Uuid>, Option<String>, i32, Vec<String>, bool) = sqlx::query_as(
        "select role, parent_machine_id, root_dir, rank, labels, accepts_remote_work from machines where id = $1",
    )
    .bind(runner_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (
            "runner".into(),
            Some(carrier),
            Some("/srv/runner".into()),
            20,
            vec!["build-host".into(), "kernel-6.12".into()],
            true
        )
    );

    // Флаги раннера без `--role runner` и недостающие флаги отказывают, ничего не создавая.
    let count = || async {
        sqlx::query_scalar::<_, i64>("select count(*) from machines")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let before = count().await;
    for args in [
        vec!["machine", "add", "--name", "x", "--role", "runner"],
        vec![
            "machine",
            "add",
            "--name",
            "x",
            "--role",
            "runner",
            "--parent",
            &carrier_text,
        ],
        vec![
            "machine",
            "add",
            "--name",
            "x",
            "--role",
            "runner",
            "--parent",
            &carrier_text,
            "--root-dir",
            "relative",
        ],
        vec![
            "machine",
            "add",
            "--name",
            "x",
            "--role",
            "runner",
            "--parent",
            &carrier_text,
            "--root-dir",
            "/srv/a/../b",
        ],
        vec![
            "machine",
            "add",
            "--name",
            "x",
            "--role",
            "runner",
            "--parent",
            &text.lines().next().unwrap()["machine_id=".len()..],
            "--root-dir",
            "/srv/x",
        ],
        vec!["machine", "add", "--name", "x", "--parent", &carrier_text],
        vec!["machine", "add", "--name", "x", "--rank", "3"],
    ] {
        let out = run(&args)?;
        assert!(!out.status.success(), "{args:?} must fail");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert_eq!(count().await, before);
    // Раннер не станет несущей.
    let refused = run(&[
        "machine",
        "set-carrier",
        "--machine-id",
        &runner_id.to_string(),
    ])?;
    assert!(!refused.status.success());
    assert_eq!(refused.stderr, b"machine_is_runner\n");
    // Снятие флага.
    let off = run(&[
        "machine",
        "set-carrier",
        "--machine-id",
        &carrier_text,
        "--off",
    ])?;
    assert!(off.status.success());
    assert!(
        !sqlx::query_scalar::<_, bool>("select is_carrier from machines where id = $1")
            .bind(carrier)
            .fetch_one(&pool)
            .await?
    );
    let missing = run(&[
        "machine",
        "set-carrier",
        "--machine-id",
        &Uuid::now_v7().to_string(),
    ])?;
    assert_eq!(missing.stderr, b"machine_not_found\n");
    Ok(())
}

// ───────── Допуск несущей ─────────

#[tokio::test]
async fn a_carrier_gets_a_token_for_its_runner_and_only_its_own() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    let old = sqlx::query_scalar::<_, Vec<u8>>(
        "select token_sha256 from machine_credentials where machine_id = $1 and revoked_at is null",
    )
    .bind(f.runner)
    .fetch_one(&pool)
    .await?;
    let token = machines::request_runner_token(&pool, f.carrier, f.runner, false).await?;
    assert_eq!(machines::verify(&pool, &token).await?, Some(f.runner));
    let now = sqlx::query_scalar::<_, Vec<u8>>(
        "select token_sha256 from machine_credentials where machine_id = $1 and revoked_at is null",
    )
    .bind(f.runner)
    .fetch_one(&pool)
    .await?;
    assert_ne!(old, now, "the token was rotated");

    // `reset_local_state` сбрасывает журнал так же, как `rotate-token`.
    sqlx::query("update machines set cj_journal_id = $2 where id = $1")
        .bind(f.runner)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await?;
    machines::request_runner_token(&pool, f.carrier, f.runner, true).await?;
    assert!(
        sqlx::query_scalar::<_, Option<Uuid>>("select cj_journal_id from machines where id = $1")
            .bind(f.runner)
            .fetch_one(&pool)
            .await?
            .is_none()
    );

    // Чужая несущая (флаг есть, но не родитель).
    let other = machines::add(&pool, f.owner, "other-carrier").await?;
    machines::set_carrier(&pool, other.machine_id, true).await?;
    assert_eq!(
        machines::request_runner_token(&pool, other.machine_id, f.runner, false).await,
        Err(RunnerRefusal::NotParent)
    );
    // Раннер не несущая: роль `runner` не пройдёт, даже если флаг записали в обход CLI.
    sqlx::query("update machines set is_carrier = true where id = $1")
        .bind(f.runner)
        .execute(&pool)
        .await?;
    assert_eq!(
        machines::request_runner_token(&pool, f.runner, f.runner, false).await,
        Err(RunnerRefusal::NotCarrier)
    );
    // Обычная машина без флага.
    let plain = machines::add(&pool, f.owner, "plain").await?;
    assert_eq!(
        machines::request_runner_token(&pool, plain.machine_id, f.runner, false).await,
        Err(RunnerRefusal::NotCarrier)
    );
    // Неизвестная машина и обычная машина вместо раннера.
    assert_eq!(
        machines::request_runner_token(&pool, f.carrier, Uuid::now_v7(), false).await,
        Err(RunnerRefusal::UnknownMachine)
    );
    assert_eq!(
        machines::request_runner_token(&pool, f.carrier, plain.machine_id, false).await,
        Err(RunnerRefusal::UnknownMachine)
    );
    // Снятый флаг лишает допуска.
    machines::set_carrier(&pool, f.carrier, false).await?;
    assert_eq!(
        machines::request_runner_token(&pool, f.carrier, f.runner, false).await,
        Err(RunnerRefusal::NotCarrier)
    );
    Ok(())
}

#[tokio::test]
async fn a_revoked_runner_gets_no_token() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    machines::revoke(&pool, f.runner).await?;
    assert_eq!(
        machines::request_runner_token(&pool, f.carrier, f.runner, false).await,
        Err(RunnerRefusal::Revoked)
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "select not exists(select 1 from machine_credentials where machine_id = $1 and revoked_at is null)"
        )
        .bind(f.runner)
        .fetch_one(&pool)
        .await?,
        "the refusal must not reactivate the runner"
    );
    Ok(())
}

#[tokio::test]
async fn observations_pass_the_same_gate_and_keep_the_last_one() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    let at = Utc::now();
    machines::record_runner_observation(&pool, f.carrier, f.runner, false, 4, at).await?;
    let row: (bool, i32, Uuid) = sqlx::query_as(
        "select reachable, consecutive_failures, carrier_id from runner_observations where machine_id = $1",
    )
    .bind(f.runner)
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (false, 4, f.carrier));
    machines::record_runner_observation(&pool, f.carrier, f.runner, true, 0, at).await?;
    let row: (bool, i32) = sqlx::query_as(
        "select reachable, consecutive_failures from runner_observations where machine_id = $1",
    )
    .bind(f.runner)
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (true, 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from runner_observations")
            .fetch_one(&pool)
            .await?,
        1
    );

    let plain = machines::add(&pool, f.owner, "plain").await?;
    let other = machines::add(&pool, f.owner, "other-carrier").await?;
    machines::set_carrier(&pool, other.machine_id, true).await?;
    for (caller, expected) in [
        (f.runner, RunnerRefusal::NotCarrier),
        (plain.machine_id, RunnerRefusal::NotCarrier),
        (other.machine_id, RunnerRefusal::NotParent),
    ] {
        assert_eq!(
            machines::record_runner_observation(&pool, caller, f.runner, false, 9, at).await,
            Err(expected)
        );
    }
    assert_eq!(
        machines::record_runner_observation(&pool, f.carrier, Uuid::now_v7(), false, 9, at).await,
        Err(RunnerRefusal::UnknownMachine)
    );
    // Отказы ничего не записали.
    let row: (bool, i32) = sqlx::query_as(
        "select reachable, consecutive_failures from runner_observations where machine_id = $1",
    )
    .bind(f.runner)
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (true, 0));
    Ok(())
}

#[tokio::test]
async fn runners_of_lists_only_live_children_of_the_carrier() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    let second = machines::add_runner(
        &pool,
        f.owner,
        "runner-b",
        &RunnerSpec {
            root_dir: "/srv/b".into(),
            ..spec(f.carrier)
        },
    )
    .await?;
    let other = machines::add(&pool, f.owner, "other").await?;
    machines::add_runner(&pool, f.owner, "foreign", &spec(other.machine_id)).await?;
    let list = machines::runners_of(&pool, f.carrier).await?;
    let names: Vec<_> = list
        .iter()
        .map(|r| (r.name.as_str(), r.root_dir.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![("runner-a", "/srv/runner"), ("runner-b", "/srv/b")]
    );
    machines::revoke(&pool, second.machine_id).await?;
    let list = machines::runners_of(&pool, f.carrier).await?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].machine_id, f.runner);
    Ok(())
}

// ───────── Доступность ─────────

fn facts(runner: bool, carrier: bool, observation: Option<(bool, u32, i64)>) -> RunnerFacts {
    let now = Utc::now();
    RunnerFacts {
        runner_connected: runner,
        carrier_connected: carrier,
        observation: observation.map(|(reachable, failures, age_secs)| {
            machines::RunnerObservationFact {
                reachable,
                consecutive_failures: failures,
                observed_at: now - chrono::TimeDelta::seconds(age_secs),
            }
        }),
        now,
    }
}

#[test]
fn availability_follows_the_spec_rule() {
    use RunnerAvailability::{Offline, Online, Unknown};
    // Воркер раннера на связи — online, что бы ни говорило наблюдение.
    assert_eq!(
        machines::runner_availability(&facts(true, true, None)),
        Online
    );
    assert_eq!(
        machines::runner_availability(&facts(true, false, Some((false, 9, 1)))),
        Online
    );
    // Отключён, но нет наблюдения, несущей или свежести — unknown.
    assert_eq!(
        machines::runner_availability(&facts(false, true, None)),
        Unknown
    );
    assert_eq!(
        machines::runner_availability(&facts(false, false, Some((false, 9, 1)))),
        Unknown
    );
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((false, 9, 181)))),
        Unknown
    );
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((false, 2, 1)))),
        Unknown
    );
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((true, 0, 1)))),
        Unknown
    );
    // Все условия вместе — offline; граница свежести 3 минуты, число отказов 3.
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((false, 3, 1)))),
        Offline
    );
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((false, 7, 179)))),
        Offline
    );
    assert_eq!(
        machines::runner_availability(&facts(false, true, Some((false, 7, 180)))),
        Unknown
    );
}

#[tokio::test]
async fn availability_reads_the_registry_and_the_stored_observation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fleet(&pool).await;
    let registry = Registry::new(Default::default());
    let at = |conn: &PgPool| {
        let conn = conn.clone();
        let registry = registry.clone();
        async move {
            let mut conn = conn.acquire().await.unwrap();
            machines::runner_availability_of(&mut conn, &registry, f.runner, Utc::now())
                .await
                .unwrap()
        }
    };
    // Ничего не известно.
    assert_eq!(at(&pool).await.0, RunnerAvailability::Unknown);
    assert!(at(&pool).await.1.is_none());

    // Несущая на связи, свежее наблюдение с тремя отказами: offline.
    let _carrier = testkit::fake_session(&registry, f.carrier, &["runners_v1"], true);
    machines::record_runner_observation(&pool, f.carrier, f.runner, false, 3, Utc::now()).await?;
    let (state, observed) = at(&pool).await;
    assert_eq!(state, RunnerAvailability::Offline);
    assert!(observed.is_some());

    // Наблюдение чужой несущей после смены родителя не считается.
    let other = machines::add(&pool, f.owner, "other").await?;
    machines::set_carrier(&pool, other.machine_id, true).await?;
    sqlx::query("update machines set parent_machine_id = $2 where id = $1")
        .bind(f.runner)
        .bind(other.machine_id)
        .execute(&pool)
        .await?;
    assert_eq!(at(&pool).await.0, RunnerAvailability::Unknown);
    sqlx::query("update machines set parent_machine_id = $2 where id = $1")
        .bind(f.runner)
        .bind(f.carrier)
        .execute(&pool)
        .await?;
    assert_eq!(at(&pool).await.0, RunnerAvailability::Offline);

    // Несущая отключилась: unknown.
    drop(_carrier);
    assert_eq!(at(&pool).await.0, RunnerAvailability::Unknown);
    let _carrier = testkit::fake_session(&registry, f.carrier, &["runners_v1"], true);
    assert_eq!(at(&pool).await.0, RunnerAvailability::Offline);

    // Воркер раннера подключился: online.
    let _runner = testkit::fake_session(&registry, f.runner, &[], true);
    assert_eq!(at(&pool).await.0, RunnerAvailability::Online);
    drop(_runner);

    // Устаревшее наблюдение: unknown.
    sqlx::query("update runner_observations set observed_at = now() - interval '4 minutes'")
        .execute(&pool)
        .await?;
    assert_eq!(at(&pool).await.0, RunnerAvailability::Unknown);
    Ok(())
}

// ───────── Отзыв ─────────

async fn seed_job(
    pool: &PgPool,
    owner: Uuid,
    machine: Uuid,
    conversation: Option<Uuid>,
    state: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into jobs (id, owner_id, conversation_id, domain, machine_id, remote, workdir, shell,
                           timeout_seconds, state, delivery)
         values ($1, $2, $3, 'personal', $4, true, '/srv/runner/jobs/personal/x', 'true', 600, $5,
                 case when $3::uuid is null then null else 'message' end)",
    )
    .bind(id)
    .bind(owner)
    .bind(conversation)
    .bind(machine)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn revoke_ends_in_place_jobs_as_lost_with_one_message_each() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let carrier = machines::add(&pool, seed.owner_id, "carrier").await?;
    let runner =
        machines::add_runner(&pool, seed.owner_id, "runner-a", &spec(carrier.machine_id)).await?;
    let c = Some(seed.conversation_id);
    let mut live = Vec::new();
    for state in ["queued", "dispatched", "waiting_lease", "running"] {
        live.push(seed_job(&pool, seed.owner_id, runner.machine_id, c, state).await);
    }
    let done = seed_job(&pool, seed.owner_id, runner.machine_id, c, "succeeded").await;
    let elsewhere = seed_job(&pool, seed.owner_id, seed.machine_id, c, "running").await;

    assert!(machines::revoke(&pool, runner.machine_id).await?);

    for id in &live {
        let (state, reason, finished): (String, Option<String>, bool) = sqlx::query_as(
            "select state, end_reason, finished_at is not null from jobs where id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            (state.as_str(), reason.as_deref(), finished),
            ("lost", Some("lost"), true)
        );
        let messages: i64 = sqlx::query_scalar(
            "select count(*) from scheduled_jobs where payload->>'action'='job_result' and payload->>'job_id'=$1",
        )
        .bind(id.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(messages, 1, "exactly one result message for {id}");
    }
    let state_of = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("select state from jobs where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        state_of(done).await,
        "succeeded",
        "a finished job is not touched"
    );
    assert_eq!(
        state_of(elsewhere).await,
        "running",
        "other machines are not touched"
    );

    // Повторный отзыв (и отзыв уже отозванной машины) ничего не удваивает.
    assert!(!machines::revoke(&pool, runner.machine_id).await?);
    let total: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where payload->>'action'='job_result'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(total, 4);
    Ok(())
}

#[tokio::test]
async fn revoke_of_an_ordinary_machine_also_ends_its_jobs() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let worker = machines::add(&pool, seed.owner_id, "build-host").await?;
    let queued = seed_job(
        &pool,
        seed.owner_id,
        worker.machine_id,
        Some(seed.conversation_id),
        "queued",
    )
    .await;
    machines::revoke(&pool, worker.machine_id).await?;
    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(queued)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "lost");
    Ok(())
}
