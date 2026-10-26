use std::{fs, process::Command, time::Duration};

use canterlot_hub::{db, installation, machines, testkit::tls::test_pki};
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rotate_cli_preserves_identity_revokes_old_and_resets_only_explicitly(
    pool: PgPool,
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let installation = installation::init(&pool, &dir.path().join("installation.toml")).await?;
    let machine = machines::add(&pool, installation.owner_id, "rotation").await?;
    let config = dir.path().join("hub.toml");
    fs::write(
        dir.path().join("database-url"),
        pool.connect_options().to_url_lossy().as_str().as_bytes(),
    )?;
    fs::write(
        &config,
        "database_url_file = 'database-url'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n",
    )?;
    let journal = Uuid::now_v7();
    for reset in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_canterlot-hub"));
        command
            .args(["machine", "rotate-token", "--config"])
            .arg(&config)
            .args(["--machine-id", &machine.machine_id.to_string()]);
        if reset {
            command.arg("--reset-local-state");
        }
        let output = command.output()?;
        assert!(output.status.success(), "rotate-token failed");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout)?;
        let token = text
            .strip_prefix("token=")
            .and_then(|text| text.strip_suffix('\n'))
            .expect("token output");
        assert!(!token.contains('\n'));
        assert_eq!(
            machines::verify(&pool, token).await?,
            Some(machine.machine_id)
        );
        assert_eq!(machines::verify(&pool, &machine.token).await?, None);
        let cj: Option<Uuid> =
            sqlx::query_scalar("select cj_journal_id from machines where id = $1")
                .bind(machine.machine_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(cj, None);
        sqlx::query("update machines set cj_journal_id = $2 where id = $1")
            .bind(machine.machine_id)
            .bind(journal)
            .execute(&pool)
            .await?;
        if !reset {
            let output = Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
                .args(["machine", "rotate-token", "--config"])
                .arg(&config)
                .args(["--machine-id", &machine.machine_id.to_string()])
                .output()?;
            assert!(output.status.success());
            let cj: Option<Uuid> =
                sqlx::query_scalar("select cj_journal_id from machines where id = $1")
                    .bind(machine.machine_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(cj, Some(journal));
        }
    }
    let rows: (i64, i64) = sqlx::query_as("select count(*), count(*) filter (where revoked_at is null) from machine_credentials where machine_id = $1")
        .bind(machine.machine_id).fetch_one(&pool).await?;
    assert_eq!(rows, (4, 1));
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn serve_applies_pending_migrations_before_listening_and_logs_version(
    pool: PgPool,
) -> TestResult {
    let dir = tempfile::tempdir()?;
    let old = sqlx::migrate::Migrator {
        migrations: db::MIGRATOR
            .iter()
            .filter(|migration| migration.version <= 3)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await?;
    installation::init(&pool, &dir.path().join("installation.toml")).await?;
    fs::write(
        dir.path().join("database-url"),
        pool.connect_options().to_url_lossy().as_str().as_bytes(),
    )?;
    let pki = test_pki();
    fs::write(dir.path().join("cert.pem"), pki.server_cert_pem)?;
    fs::write(dir.path().join("key.pem"), pki.server_key_pem)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);
    let config = dir.path().join("hub.toml");
    fs::write(
        &config,
        format!(
            "database_url_file = 'database-url'\ngrpc_listen = '{addr}'\ntls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n"
        ),
    )?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
        .args(["serve", "--config"])
        .arg(config)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while tokio::net::TcpStream::connect(addr).await.is_err() {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let version: i64 =
        sqlx::query_scalar("select max(version) from _sqlx_migrations where success")
            .fetch_one(&pool)
            .await?;
    Command::new("kill")
        .args(["-TERM", &child.id().unwrap().to_string()])
        .output()?;
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await??;
    assert_eq!(
        version,
        db::MIGRATOR
            .iter()
            .map(|migration| migration.version)
            .max()
            .unwrap()
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let logs = String::from_utf8(output.stderr)?;
    assert!(logs.contains("migrations_applied"));
    assert!(logs.contains(&format!("version={version}")));
    Ok(())
}
