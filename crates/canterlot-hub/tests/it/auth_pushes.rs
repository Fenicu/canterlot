//! Пуши о входе: без разговора, сводка неудач не чаще раза в час, одна на блокировку
//! (спецификация §5.6).

use canterlot_hub::{
    config::HubConfig,
    notify::{self, PushSettings, Sender},
};
use sqlx::PgPool;

use crate::support::auth::{AuthHub, PASSWORD, fake_gotify};

pub(crate) async fn enable(pool: &PgPool) {
    notify::configure(
        pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished: true,
            failures: true,
            timezone: "UTC".into(),
        }),
    )
    .await
    .unwrap();
}

pub(crate) fn sender(url: &str) -> (tempfile::TempDir, Sender) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token"), "gotify-test-token\n").unwrap();
    let path = dir.path().join("hub.toml");
    std::fs::write(
        &path,
        format!(
            "database_url_file = 'db'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'c'\ntls_key = 'k'\n\
             manifest = 'm'\n[notify.gotify]\nurl = '{url}'\ntoken_file = 'token'\n\
             app_url = 'https://app.example.test'\n"
        ),
    )
    .unwrap();
    let config = HubConfig::load(&path).unwrap();
    let sender = Sender::new(config.notify.gotify.as_ref().unwrap()).unwrap();
    (dir, sender)
}

async fn kinds(pool: &PgPool) -> Vec<(String, Option<uuid::Uuid>)> {
    sqlx::query_as("select kind,conversation_id from owner_pushes order by created_at,id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn auth_pushes_new_sign_in_is_pushed_without_a_conversation() {
    let pool = canterlot_hub::testkit::db().await;
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let quiet = hub.sign_in_owner(&hub.anonymous(Some("203.0.113.9"))).await;
    assert_eq!(quiet.status, 200);
    assert!(
        kinds(&hub.pool).await.is_empty(),
        "no push without [notify.gotify]"
    );

    enable(&hub.pool).await;
    let response = hub
        .sign_in_owner(
            &hub.anonymous(Some("203.0.113.9"))
                .with_header("User-Agent", "Firefox/159.0"),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(kinds(&hub.pool).await, vec![("sign_in".to_owned(), None)]);
    let (seen, url) = fake_gotify().await;
    let (_dir, sender) = sender(&url);
    let report = sender.deliver_due(&hub.pool).await.unwrap();
    assert_eq!(report.sent, 1);
    let message = seen.lock().unwrap()[0].clone();
    assert_eq!(message["title"], "Новый вход в canterlot");
    assert_eq!(message["message"], "Firefox 159 · 203.0.113.9 · по паролю");
    assert_eq!(message["priority"], 8);
    assert_eq!(
        message["extras"]["client::notification"]["click"]["url"],
        "https://app.example.test/settings/sign-in"
    );
    // Пуш о входе не снимается отменой по разговору.
    let none: i64 = sqlx::query_scalar("select count(*) from owner_pushes where state='cancelled'")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(none, 0);
}
