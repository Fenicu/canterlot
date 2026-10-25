use std::{fs, path::Path, process::Command, time::Duration};

use canterlot_hub::{
    config::HubConfig,
    installation::{self, InstallationError},
};
use sqlx::{ConnectOptions, PgPool};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn write_manifest(path: &Path, installation_id: Uuid, generation: Uuid, restoring: bool) {
    fs::write(
        path,
        format!(
            "installation_id = \"{installation_id}\"\nrestore_generation = \"{generation}\"\nrestore_in_progress = {restoring}\n"
        ),
    )
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn init_creates_manifest_and_row(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("installation.toml");
    let installation = installation::init(&pool, &path).await?;
    let row: (Uuid, Uuid, Uuid) =
        sqlx::query_as("select installation_id, restore_generation, owner_id from installation")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        row,
        (
            installation.installation_id,
            installation.restore_generation,
            installation.owner_id
        )
    );
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(&path)?)?;
    assert_eq!(
        manifest["installation_id"].as_str(),
        Some(row.0.to_string().as_str())
    );
    assert_eq!(
        manifest["restore_generation"].as_str(),
        Some(row.1.to_string().as_str())
    );
    assert_eq!(manifest["restore_in_progress"].as_bool(), Some(false));
    assert_eq!(manifest.as_table().unwrap().len(), 3);
    for id in [row.0, row.1, row.2] {
        assert_eq!(id.get_version_num(), 7);
    }
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("select id from owners")
            .fetch_one(&pool)
            .await?,
        row.2
    );
    let checked = installation::check(&pool, &path).await?;
    assert_eq!(checked.installation_id, row.0);
    assert_eq!(checked.restore_generation, row.1);
    assert_eq!(checked.owner_id, row.2);
    assert_eq!(fs::read_dir(dir.path())?.count(), 1);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn init_twice_is_refused(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("installation.toml");
    installation::init(&pool, &path).await?;
    let original = fs::read(&path)?;
    assert!(matches!(
        installation::init(&pool, &path).await,
        Err(InstallationError::AlreadyInitialized)
    ));
    assert_eq!(fs::read(&path)?, original);
    fs::remove_file(&path)?;
    assert!(matches!(
        installation::init(&pool, &path).await,
        Err(InstallationError::AlreadyInitialized)
    ));
    assert!(!path.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from owners")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn init_refuses_existing_manifest_without_database_row(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("installation.toml");
    fs::write(&path, "existing administrative manifest")?;
    assert!(matches!(
        installation::init(&pool, &path).await,
        Err(InstallationError::AlreadyInitialized)
    ));
    assert_eq!(
        fs::read_to_string(&path)?,
        "existing administrative manifest"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from owners")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn check_detects_missing_and_mismatched_manifest(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("installation.toml");
    let current = installation::init(&pool, &path).await?;
    fs::remove_file(&path)?;
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::ManifestMissing)
    ));
    assert!(!path.exists());
    write_manifest(&path, current.installation_id, Uuid::now_v7(), false);
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::ManifestMismatch)
    ));
    write_manifest(&path, Uuid::now_v7(), current.restore_generation, false);
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::ManifestMismatch)
    ));
    write_manifest(
        &path,
        current.installation_id,
        current.restore_generation,
        true,
    );
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::RestoreInProgress)
    ));
    write_manifest(&path, current.installation_id, Uuid::now_v7(), true);
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::RestoreInProgress)
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn check_refuses_uninitialized_and_malformed_manifest(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("installation.toml");
    write_manifest(&path, Uuid::now_v7(), Uuid::now_v7(), false);
    assert!(matches!(
        installation::check(&pool, &path).await,
        Err(InstallationError::NotInitialized)
    ));
    fs::remove_file(&path)?;
    let current = installation::init(&pool, &path).await?;
    for invalid in [
        "not valid TOML".to_owned(),
        format!(
            "installation_id = \"{}\"\nrestore_generation = \"{}\"\n",
            current.installation_id, current.restore_generation
        ),
    ] {
        fs::write(&path, &invalid)?;
        assert!(matches!(
            installation::check(&pool, &path).await,
            Err(InstallationError::ManifestMismatch)
        ));
        assert_eq!(fs::read_to_string(&path)?, invalid);
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn init_rolls_back_when_manifest_cannot_be_written(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("absent/installation.toml");
    assert!(matches!(
        installation::init(&pool, &path).await,
        Err(InstallationError::Io(_))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from installation")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from owners")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert!(!path.exists());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_initializations_create_one_owner(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let first = dir.path().join("first.toml");
    let second = dir.path().join("second.toml");
    let (a, b) = tokio::join!(
        installation::init(&pool, &first),
        installation::init(&pool, &second)
    );
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(InstallationError::AlreadyInitialized))
            | (Err(InstallationError::AlreadyInitialized), Ok(_))
    ));
    assert_ne!(first.exists(), second.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from owners")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

fn config_text() -> &'static str {
    "database_url_file = 'database-url'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'tls/cert.pem'\ntls_key = 'tls/key.pem'\nmanifest = 'external/installation.toml'\n"
}

#[test]
fn config_loads_paths_defaults_and_timer_overrides() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("hub.toml");
    fs::write(&path, config_text())?;
    let config = HubConfig::load(&path)?;
    assert_eq!(config.database_url_file, dir.path().join("database-url"));
    assert_eq!(config.grpc_listen, "127.0.0.1:7443".parse()?);
    assert_eq!(config.tls_cert, dir.path().join("tls/cert.pem"));
    assert_eq!(config.tls_key, dir.path().join("tls/key.pem"));
    assert_eq!(
        config.manifest,
        dir.path().join("external/installation.toml")
    );
    assert_eq!(config.timers.handshake, Duration::from_secs(10));
    assert_eq!(config.timers.degraded_after, Duration::from_secs(15));
    assert_eq!(config.timers.unavailable_after, Duration::from_secs(30));
    assert_eq!(config.timers.command_stored_receipt, Duration::from_secs(5));
    assert_eq!(config.timers.dispatch_sweep, Duration::from_secs(5));
    assert_eq!(
        config.timers.http2_keepalive_interval,
        Duration::from_secs(30)
    );
    assert_eq!(
        config.timers.http2_keepalive_timeout,
        Duration::from_secs(10)
    );
    fs::write(
        &path,
        format!(
            "{}\n[timers]\nhandshake = 1\ndegraded_after = 2\nunavailable_after = 3\ncommand_stored_receipt = 4\ndispatch_sweep = 6\nhttp2_keepalive_interval = 31\nhttp2_keepalive_timeout = 11\n",
            config_text()
        ),
    )?;
    let timers = HubConfig::load(&path)?.timers;
    assert_eq!(timers.handshake, Duration::from_secs(1));
    assert_eq!(timers.degraded_after, Duration::from_secs(2));
    assert_eq!(timers.unavailable_after, Duration::from_secs(3));
    assert_eq!(timers.command_stored_receipt, Duration::from_secs(4));
    assert_eq!(timers.dispatch_sweep, Duration::from_secs(6));
    assert_eq!(timers.http2_keepalive_interval, Duration::from_secs(31));
    assert_eq!(timers.http2_keepalive_timeout, Duration::from_secs(11));
    fs::write(
        &path,
        format!("{}\n[timers]\nhandshake = 1\n", config_text()),
    )?;
    assert_eq!(
        HubConfig::load(&path)?.timers.dispatch_sweep,
        Duration::from_secs(5)
    );
    Ok(())
}

#[test]
fn config_refuses_inline_secrets_and_invalid_values() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("hub.toml");
    for extra in [
        "database_url = 'inline-secret'",
        "token = 'inline-secret'",
        "password = 'inline-secret'",
        "[timers]\nhandshake = -1",
    ] {
        fs::write(&path, format!("{}\n{extra}\n", config_text()))?;
        let error = HubConfig::load(&path).expect_err("invalid config accepted");
        assert!(!error.to_string().contains("inline-secret"));
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cli_serve_refuses_without_manifest(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("hub.toml");
    fs::write(&path, config_text())?;
    let url = pool.connect_options().to_url_lossy().to_string();
    fs::write(dir.path().join("database-url"), &url)?;
    let manifest = dir.path().join("external/installation.toml");
    fs::create_dir(dir.path().join("external"))?;
    installation::init(&pool, &manifest).await?;
    fs::remove_file(&manifest)?;
    let output = Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
        .args(["serve", "--config"])
        .arg(&path)
        .env("DATABASE_URL", "ignored-secret-environment")
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(stderr.trim(), "manifest_missing");
    assert!(!stderr.contains(&url));
    assert!(!stderr.contains("ignored-secret-environment"));
    assert!(!dir.path().join("external/installation.toml").exists());
    Ok(())
}

#[test]
fn cli_errors_do_not_disclose_config_or_database_file_contents() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("hub.toml");
    let database_file = dir.path().join("database-url");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
            .args(["serve", "--config"])
            .arg(&config)
            .output()
    };
    fs::write(
        &config,
        format!("{}\npassword = 'synthetic-secret'\n", config_text()),
    )?;
    let output = run()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.starts_with("config_invalid at "));
    assert!(stderr.contains(config.to_str().unwrap()));
    assert!(!stderr.contains("synthetic-secret"));
    fs::write(&config, config_text())?;
    for (contents, expected) in [
        ("", "database_url_invalid\n"),
        ("invalid URL with synthetic-secret", "db_error\n"),
    ] {
        fs::write(&database_file, contents)?;
        let output = run()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr == expected.as_bytes());
    }
    fs::remove_file(&database_file)?;
    let output = run()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr == b"database_url_file_error\n");
    Ok(())
}
