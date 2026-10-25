use std::{fs, process::Command};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use canterlot_hub::{installation, machines};
use sha2::{Digest, Sha256};
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[sqlx::test(migrations = "./migrations")]
async fn machine_token_verifies_and_revokes(pool: PgPool) -> TestResult {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(&pool)
        .await?;
    let machine = machines::add(&pool, owner, "test-machine").await?;
    assert_eq!(machine.machine_id.get_version_num(), 7);
    assert_eq!(machine.token.len(), 43);
    assert!(
        URL_SAFE_NO_PAD
            .decode(&machine.token)
            .is_ok_and(|bytes| bytes.len() == 32)
    );
    assert_eq!(
        machines::verify(&pool, &machine.token).await?,
        Some(machine.machine_id)
    );
    assert_eq!(machines::verify(&pool, "wrong-token").await?, None);
    let (stored, revoked): (Vec<u8>, bool) = sqlx::query_as("select token_sha256, revoked_at is not null from machine_credentials where machine_id = $1").bind(machine.machine_id).fetch_one(&pool).await?;
    assert!(stored == Sha256::digest(machine.token.as_bytes()).as_slice());
    assert!(stored != machine.token.as_bytes());
    assert!(!revoked);
    let record: String = sqlx::query_scalar(
        "select row_to_json(c)::text from machine_credentials c where machine_id = $1",
    )
    .bind(machine.machine_id)
    .fetch_one(&pool)
    .await?;
    assert!(!record.contains(&machine.token));
    let row: (Uuid, String) = sqlx::query_as("select owner_id, name from machines where id = $1")
        .bind(machine.machine_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row, (owner, "test-machine".into()));
    assert!(machines::revoke(&pool, machine.machine_id).await?);
    assert_eq!(machines::verify(&pool, &machine.token).await?, None);
    assert!(!machines::revoke(&pool, machine.machine_id).await?);
    assert!(!machines::revoke(&pool, Uuid::now_v7()).await?);
    assert!(
        sqlx::query_scalar::<_, bool>(
            "select revoked_at is not null from machine_credentials where machine_id = $1"
        )
        .bind(machine.machine_id)
        .fetch_one(&pool)
        .await?
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn credentials_are_unique_and_failed_add_is_atomic(pool: PgPool) -> TestResult {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(&pool)
        .await?;
    let a = machines::add(&pool, owner, "a").await?;
    let b = machines::add(&pool, owner, "b").await?;
    assert!(a.token != b.token);
    assert_ne!(a.machine_id, b.machine_id);
    assert!(
        machines::add(&pool, Uuid::now_v7(), "missing owner")
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from machines")
            .fetch_one(&pool)
            .await?,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from machine_credentials")
            .fetch_one(&pool)
            .await?,
        2
    );
    machines::revoke(&pool, a.machine_id).await?;
    assert_eq!(machines::verify(&pool, &b.token).await?, Some(b.machine_id));
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn cli_initializes_adds_checks_and_revokes(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("hub.toml");
    let manifest = dir.path().join("installation.toml");
    let url = pool.connect_options().to_url_lossy().to_string();
    fs::write(dir.path().join("database-url"), &url)?;
    fs::write(
        &config,
        "database_url_file = 'database-url'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n",
    )?;
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
            .args(args)
            .arg("--config")
            .arg(&config)
            .output()
    };
    let initialized = run(&["init"])?;
    assert!(initialized.status.success());
    assert!(initialized.stdout.is_empty() && initialized.stderr.is_empty());
    let current = installation::check(&pool, &manifest).await?;
    let added = run(&["machine", "add", "--name", "cli-machine"])?;
    assert!(added.status.success());
    assert!(added.stderr.is_empty());
    let text = String::from_utf8(added.stdout)?;
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let machine_id = Uuid::parse_str(
        lines[0]
            .strip_prefix("machine_id=")
            .expect("machine id missing"),
    )?;
    let token = lines[1].strip_prefix("token=").expect("token missing");
    assert_eq!(machines::verify(&pool, token).await?, Some(machine_id));
    assert_eq!(text.matches(token).count(), 1);
    assert!(!text.contains(&url));
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("select owner_id from machines where id = $1")
            .bind(machine_id)
            .fetch_one(&pool)
            .await?,
        current.owner_id
    );
    let served = run(&["serve"])?;
    assert!(served.status.success());
    assert!(served.stdout.is_empty() && served.stderr.is_empty());
    let revoked = run(&["machine", "revoke", "--machine-id", &machine_id.to_string()])?;
    assert!(revoked.status.success());
    assert!(revoked.stdout.is_empty() && revoked.stderr.is_empty());
    assert_eq!(machines::verify(&pool, token).await?, None);
    let repeated = run(&["init"])?;
    assert!(!repeated.status.success());
    assert!(repeated.stdout.is_empty());
    assert!(repeated.stderr == b"already_initialized\n");
    Ok(())
}
