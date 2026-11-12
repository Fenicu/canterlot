//! Уровни журнала входа: ожидаемые исходы от интернета (неверный пароль, отказ CSRF на
//! публичном пути, предел запусков QR) пишутся INFO, чтобы verify.sh не принимал их за
//! новый вид WARN; общая блокировка входа остаётся WARN.

use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock},
};

use canterlot_hub::testkit::{Method, TEST_OWNER_LOGIN};
use serde_json::json;
use sqlx::PgPool;
use tracing_subscriber::fmt::MakeWriter;

use crate::support::auth::{AuthHub, PASSWORD};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

fn captured() -> &'static Arc<Mutex<Vec<u8>>> {
    static CAPTURE: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Capture(buffer.clone()))
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        buffer
    })
}

/// Уровни строк журнала с этим видом (и причиной, если задана).
fn levels(kind: &str, reason: Option<&str>) -> Vec<String> {
    let text = String::from_utf8_lossy(&captured().lock().unwrap()).into_owned();
    text.lines()
        .filter(|line| line.contains(&format!("kind=\"{kind}\"")))
        .filter(|line| reason.is_none_or(|reason| line.contains(&format!("reason=\"{reason}\""))))
        .map(|line| {
            ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"]
                .into_iter()
                .find(|level| line.contains(&format!(" {level} ")))
                .unwrap_or("?")
                .to_owned()
        })
        .collect()
}

fn clear() {
    captured().lock().unwrap().clear();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_logs_expected_internet_outcomes_are_info(pool: PgPool) {
    captured();
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;

    // Неверный пароль.
    clear();
    let client = hub.anonymous(Some("203.0.113.70"));
    let failed = hub.sign_in(&client, TEST_OWNER_LOGIN, "wrong password!").await;
    assert_eq!(failed.status, 401);
    assert_eq!(levels("sign_in_failed", None), ["INFO"]);

    // Неудачный вход ключом доступа.
    clear();
    let failed = client
        .send(
            Method::POST,
            "/api/v1/auth/passkey/finish",
            None,
            Some(json!({"challenge_id": uuid::Uuid::now_v7(), "credential": {}})),
        )
        .await;
    assert_eq!(failed.status, 401, "{:?}", failed.body);
    assert_eq!(levels("sign_in_failed", None), ["INFO"]);

    // Неверный пароль повторного входа.
    clear();
    let wrong = hub
        .owner()
        .send(
            Method::POST,
            "/api/v1/auth/reauth/password",
            None,
            Some(json!({"password": "not the password"})),
        )
        .await;
    assert_eq!(wrong.status, 403);
    assert_eq!(levels("reauth_failed", None), ["INFO"]);

    // CSRF на публичном пути: бот без Origin и без X-Canterlot-Client.
    clear();
    let body = Some(json!({"login": TEST_OWNER_LOGIN, "password": "x"}));
    let no_origin = client
        .clone()
        .without_header("Origin")
        .send(Method::POST, "/api/v1/auth/password", None, body.clone())
        .await;
    assert_eq!(no_origin.status, 403);
    let no_client = client
        .clone()
        .without_header("X-Canterlot-Client")
        .send(Method::POST, "/api/v1/auth/password", None, body)
        .await;
    assert_eq!(no_client.status, 403);
    assert_eq!(levels("auth_rejected", Some("origin")), ["INFO"]);
    assert_eq!(levels("auth_rejected", Some("client_header")), ["INFO"]);

    // CSRF на остальных путях остаётся WARN.
    clear();
    let other = hub
        .owner()
        .without_header("Origin")
        .send(Method::POST, "/api/v1/auth/logout", None, None)
        .await;
    assert_eq!(other.status, 403);
    let lookalike = client
        .clone()
        .without_header("Origin")
        .send(Method::POST, "/api/v1/auth/password/", None, Some(json!({})))
        .await;
    assert_eq!(lookalike.status, 403);
    assert_eq!(levels("auth_rejected", Some("origin")), ["WARN", "WARN"]);

    // Предел запусков QR.
    clear();
    let qr = hub.anonymous(Some("203.0.113.71"));
    let mut limited = 0;
    for _ in 0..11 {
        let response = qr
            .send(Method::POST, "/api/v1/auth/qr/start", None, Some(json!({})))
            .await;
        if response.status == 429 {
            limited += 1;
        }
    }
    assert_eq!(limited, 1);
    assert_eq!(levels("qr_start_limited", None), ["INFO"]);

    // Общая блокировка входа по паролю остаётся WARN.
    clear();
    sqlx::query(
        "insert into auth_failures(bucket,at) select 'ip:192.0.2.'||g,now() from generate_series(1,31) g",
    )
    .execute(&hub.pool)
    .await
    .unwrap();
    let failed = hub
        .sign_in(&hub.anonymous(Some("203.0.113.72")), TEST_OWNER_LOGIN, "nope nope nope")
        .await;
    assert_eq!(failed.status, 401);
    assert_eq!(levels("sign_in_global_lock", None), ["WARN"]);
}
