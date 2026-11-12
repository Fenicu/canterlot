//! Ссылка восстановления и команды `canterlot-hub auth …` (спецификация §4.3).

use std::{fs, io::Write, process::Command};

use canterlot_hub::{
    installation,
    owner_auth::{password::Passwords, recovery},
    testkit::Method,
};
use serde_json::{Value, json};
use sqlx::{ConnectOptions, PgPool};

use crate::support::auth::{AuthHub, PASSWORD, error_code, fake_gotify, session_token};

async fn redeem(hub: &AuthHub, ip: &str, token: &str) -> canterlot_hub::testkit::TestResponse {
    hub.anonymous(Some(ip))
        .send(
            Method::POST,
            "/api/v1/auth/recovery",
            None,
            Some(json!({"token": token})),
        )
        .await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_recovery_token_is_single_use_expires_and_is_replaced(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (other, _) = hub
        .session(canterlot_hub::owner_auth::Method::Password, None)
        .await;
    let first = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    let second = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    let replaced = redeem(&hub, "203.0.113.100", &first).await;
    assert_eq!(replaced.status, 401);
    assert_eq!(error_code(&replaced), "invalid_credentials");
    let ok = redeem(&hub, "203.0.113.100", &second).await;
    assert_eq!(ok.status, 200);
    let body: Value = ok.json();
    assert_eq!(body["session"]["method"], "recovery");
    let token = session_token(&ok);
    let list: Value = hub
        .with_session(&token, Some("203.0.113.100"))
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    assert!(list["fresh_until"].is_string(), "recovery proves the owner");
    assert_eq!(
        hub.revoke_reason(other).await,
        None,
        "recovery revokes nothing"
    );
    assert_eq!(
        redeem(&hub, "203.0.113.100", &second).await.status,
        401,
        "single use"
    );

    let expiring = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    sqlx::query("update owner_recovery_tokens set expires_at=now()-interval '1 second'")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(redeem(&hub, "203.0.113.101", &expiring).await.status, 401);
    // Токен другого вида не подходит, даже действующий токен сеанса.
    assert_eq!(redeem(&hub, "203.0.113.101", &token).await.status, 401);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_recovery_guessing_is_limited_per_bucket(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    for n in 0..5 {
        let guess = format!("{:0>43}", n);
        assert_eq!(redeem(&hub, "203.0.113.110", &guess).await.status, 401);
    }
    let token = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    let locked = redeem(&hub, "203.0.113.110", &token).await;
    assert_eq!(locked.status, 429);
    assert_eq!(redeem(&hub, "203.0.113.111", &token).await.status, 200);
}

struct Cli {
    _dir: tempfile::TempDir,
    config: std::path::PathBuf,
}

impl Cli {
    async fn new(pool: &PgPool, gotify: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        installation::init(pool, &dir.path().join("installation.toml"))
            .await
            .unwrap();
        fs::write(
            dir.path().join("database-url"),
            pool.connect_options().to_url_lossy().as_str().as_bytes(),
        )
        .unwrap();
        let mut text = "database_url_file = 'database-url'\ngrpc_listen = '127.0.0.1:7443'\n\
            tls_cert = 'cert.pem'\ntls_key = 'key.pem'\nmanifest = 'installation.toml'\n\
            [http]\ntrusted_proxies = ['127.0.0.1/32']\nowner_login = 'owner'\n\
            allowed_origins = ['https://hub.example.test']\n"
            .to_owned();
        if let Some(url) = gotify {
            fs::write(dir.path().join("gotify-token"), "gotify-test-token\n").unwrap();
            text.push_str(&format!(
                "[notify.gotify]\nurl = '{url}'\ntoken_file = 'gotify-token'\n"
            ));
        }
        let config = dir.path().join("hub.toml");
        fs::write(&config, text).unwrap();
        Self { _dir: dir, config }
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_canterlot-hub"));
        command
            .arg("auth")
            .args(&args[..1])
            .arg("--config")
            .arg(&self.config)
            .args(&args[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        let output = child.wait_with_output().unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_recovery_cli_prints_a_working_link_and_manages_sessions(pool: PgPool) {
    let cli = Cli::new(&pool, None).await;
    let (ok, out, err) = cli.run(&["recovery-link", "--minutes", "5"], None);
    assert!(ok, "{err}");
    let token = out
        .trim_end()
        .strip_prefix("https://hub.example.test/recover#")
        .expect("link on the configured origin")
        .to_owned();
    assert_eq!(token.len(), 43);
    let minutes: f64 = sqlx::query_scalar(
        "select extract(epoch from expires_at-created_at)::float8/60 from owner_recovery_tokens",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((4.9..=5.1).contains(&minutes), "{minutes}");
    let (ok, _, err) = cli.run(&["recovery-link", "--minutes", "61"], None);
    assert!(!ok);
    assert!(err.contains("minutes_out_of_range"));
    let (ok, _, err) = cli.run(&["recovery-link", "--push"], None);
    assert!(!ok, "push needs [notify.gotify]");
    assert!(err.contains("notify_not_configured"));

    let hub = AuthHub::new(pool.clone()).await;
    let signed = redeem(&hub, "203.0.113.120", &token).await;
    assert_eq!(signed.status, 200);

    let (ok, out, _) = cli.run(&["set-password", "--stdin"], Some("short\n"));
    assert!(!ok, "{out}");
    let (ok, out, err) = cli.run(&["set-password", "--stdin"], Some(&format!("{PASSWORD}\n")));
    assert!(ok, "{err}");
    assert_eq!(out, "password_set\n");
    let phc: String = sqlx::query_scalar("select phc from owner_passwords")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        phc.starts_with("$argon2id$v=19$m=65536,t=3,p=1$"),
        "production parameters"
    );
    assert!(
        Passwords::default()
            .verify(PASSWORD.into(), Some(phc))
            .await
    );
    let (ok, _, err) = cli.run(&["set-password"], None);
    assert!(!ok, "without a TTY the password is not read");
    assert!(err.contains("tty_required"));

    let (ok, out, _) = cli.run(&["sessions"], None);
    assert!(ok);
    let lines: Vec<&str> = out.lines().collect();
    // Сеанс восстановления и заранее заведённый тестовый сеанс.
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("\trecovery\t") && l.contains("203.0.113.120"))
    );
    assert!(!out.contains(&token));

    let (ok, out, _) = cli.run(&["revoke-all"], None);
    assert!(ok);
    assert_eq!(out, "revoked=2\n");
    let reasons: Vec<String> =
        sqlx::query_scalar("select distinct revoke_reason from owner_sessions")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(reasons, vec!["server".to_owned()]);
    let (ok, out, _) = cli.run(
        &["set-password", "--stdin", "--revoke-sessions"],
        Some("another long one\n"),
    );
    assert!(ok);
    assert_eq!(out, "password_set revoked=0\n");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_recovery_cli_push_sends_only_to_gotify(pool: PgPool) {
    let (seen, url) = fake_gotify().await;
    let cli = Cli::new(&pool, Some(&url)).await;
    let (ok, out, err) = tokio::task::spawn_blocking(move || {
        let result = cli.run(&["recovery-link", "--push", "--minutes", "10"], None);
        drop(cli);
        result
    })
    .await
    .unwrap();
    assert!(ok, "{err}");
    assert_eq!(out, "sent\n");
    assert!(!err.contains("recover#"));
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    let link = messages[0]["extras"]["client::notification"]["click"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let token = link
        .strip_prefix("https://hub.example.test/recover#")
        .expect("link to the hub origin");
    assert!(messages[0]["message"].as_str().unwrap().contains(&link));
    // Ссылка не лежит в очереди пушей открытым текстом.
    let queued: i64 = sqlx::query_scalar("select count(*) from owner_pushes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
    let hub = AuthHub::new(pool).await;
    assert_eq!(redeem(&hub, "203.0.113.121", token).await.status, 200);
}
