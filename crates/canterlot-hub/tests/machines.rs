use std::{fs, process::Command, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use canterlot_hub::{installation, machines, testkit::tls::test_pki};
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);
    let url = pool.connect_options().to_url_lossy().to_string();
    fs::write(dir.path().join("database-url"), &url)?;
    fs::write(
        &config,
        format!(
            "database_url_file = 'database-url'\ngrpc_listen = '{addr}'\ntls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n"
        ),
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
    assert_eq!(lines.len(), 4);
    let machine_id = Uuid::parse_str(
        lines[0]
            .strip_prefix("machine_id=")
            .expect("machine id missing"),
    )?;
    assert_eq!(
        lines[1],
        format!("installation_id={}", current.installation_id)
    );
    assert_eq!(
        lines[2],
        format!("restore_generation={}", current.restore_generation)
    );
    let token = lines[3].strip_prefix("token=").expect("token missing");
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
    let refuses_serve = |expected: &str| -> TestResult {
        let served = run(&["serve"])?;
        assert!(!served.status.success());
        assert!(served.stdout.is_empty());
        let error = String::from_utf8(served.stderr)?;
        assert_eq!(error.lines().last(), Some(expected));
        assert!(!error.contains(token));
        assert!(!error.contains(&url));
        Ok(())
    };
    refuses_serve("tls_cert_file_error")?;
    let pki = test_pki();
    fs::write(dir.path().join("cert.pem"), &pki.server_cert_pem)?;
    refuses_serve("tls_key_file_error")?;
    fs::write(dir.path().join("key.pem"), "invalid-private-key")?;
    refuses_serve("grpc_server_error")?;
    fs::write(dir.path().join("key.pem"), &pki.server_key_pem)?;
    fs::write(dir.path().join("cert.pem"), "invalid-certificate")?;
    refuses_serve("grpc_server_error")?;
    fs::write(dir.path().join("cert.pem"), &pki.server_cert_pem)?;
    let saved_manifest = fs::read(&manifest)?;
    fs::rename(&manifest, dir.path().join("saved-manifest"))?;
    refuses_serve("manifest_missing")?;
    fs::write(&manifest, "invalid-manifest")?;
    refuses_serve("manifest_mismatch")?;
    fs::write(&manifest, saved_manifest)?;

    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
        .args(["serve", "--config"])
        .arg(&config)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                server.try_wait()?.is_none(),
                "serve exited before listening"
            );
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                return Ok::<_, std::io::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let signalled = Command::new("kill")
        .arg("-TERM")
        .arg(server.id().expect("serve process id").to_string())
        .output()?;
    assert!(signalled.status.success());
    let served = tokio::time::timeout(Duration::from_secs(10), server.wait_with_output()).await??;
    assert_eq!(served.status.code(), Some(0));
    assert!(served.stdout.is_empty());
    assert!(String::from_utf8_lossy(&served.stderr).contains("migrations_applied"));
    let revoked = run(&["machine", "revoke", "--machine-id", &machine_id.to_string()])?;
    assert!(revoked.status.success());
    assert!(revoked.stdout.is_empty() && revoked.stderr.is_empty());
    assert_eq!(machines::verify(&pool, token).await?, None);
    let refused = run(&[
        "machine",
        "rotate-token",
        "--machine-id",
        &machine_id.to_string(),
    ])?;
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert_eq!(refused.stderr, b"machine_revoked\n");
    let reactivated = run(&[
        "machine",
        "rotate-token",
        "--machine-id",
        &machine_id.to_string(),
        "--reactivate",
    ])?;
    assert!(reactivated.status.success());
    assert!(reactivated.stderr.is_empty());
    let text = String::from_utf8(reactivated.stdout)?;
    let new_token = text
        .trim_end()
        .strip_prefix("token=")
        .expect("token missing");
    assert_eq!(text.lines().count(), 1);
    assert_eq!(machines::verify(&pool, new_token).await?, Some(machine_id));
    assert_eq!(machines::verify(&pool, token).await?, None);
    let missing = run(&[
        "machine",
        "revoke",
        "--machine-id",
        &Uuid::now_v7().to_string(),
    ])?;
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert_eq!(missing.stderr, b"machine_not_found\n");
    let repeated = run(&["init"])?;
    assert!(!repeated.status.success());
    assert!(repeated.stdout.is_empty());
    assert!(repeated.stderr == b"already_initialized\n");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn revoke_and_rotate_notify_transactionally_and_rotation_is_atomic(
    pool: PgPool,
) -> TestResult {
    use sqlx::postgres::PgListener;
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(&pool)
        .await?;
    let machine = machines::add(&pool, owner, "notification").await?;
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen("canterlot_revoke").await?;
    sqlx::query("create function block_revoke_commit() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(774123); return new; end $$")
        .execute(&pool).await?;
    sqlx::query("create constraint trigger block_revoke_commit after update on machine_credentials deferrable initially deferred for each row execute function block_revoke_commit()")
        .execute(&pool).await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(774123)")
        .execute(&mut *blocker)
        .await?;
    let task_pool = pool.clone();
    let id = machine.machine_id;
    let revoked = tokio::spawn(async move { machines::revoke(&task_pool, id).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname = current_database() and query = 'COMMIT' and wait_event = 'advisory')").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.recv())
            .await
            .is_err()
    );
    blocker.commit().await?;
    assert!(revoked.await??);
    let notification = tokio::time::timeout(Duration::from_secs(2), listener.recv()).await??;
    assert_eq!(notification.channel(), "canterlot_revoke");
    assert_eq!(notification.payload(), id.to_string());
    let token = machines::rotate_token(&pool, id, false, true)
        .await?
        .unwrap();
    let notification = tokio::time::timeout(Duration::from_secs(2), listener.recv()).await??;
    assert_eq!(notification.channel(), "canterlot_revoke");
    assert_eq!(notification.payload(), id.to_string());
    sqlx::query("alter table machine_credentials add constraint no_rotation check (revoked_at is not null) not valid")
        .execute(&pool).await?;
    assert!(
        machines::rotate_token(&pool, id, true, false)
            .await
            .is_err()
    );
    assert_eq!(machines::verify(&pool, &token).await?, Some(id));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.recv())
            .await
            .is_err()
    );
    Ok(())
}
