//! Пуши о входе: без разговора, сводка неудач не чаще раза в час, одна на блокировку
//! (спецификация §5.6).

use canterlot_hub::{
    config::HubConfig,
    notify::{self, PushSettings, Sender},
    testkit::TEST_OWNER_LOGIN,
};
use sqlx::PgPool;

use crate::support::auth::{AuthHub, PASSWORD, fake_gotify};

async fn enable(pool: &PgPool) {
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

fn sender(url: &str) -> (tempfile::TempDir, Sender) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_pushes_new_sign_in_is_pushed_without_a_conversation(pool: PgPool) {
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
    assert_eq!(
        message["message"],
        "Firefox/159.0 · 203.0.113.9 · по паролю"
    );
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_pushes_failures_are_summarised_at_most_hourly(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    enable(&hub.pool).await;
    for n in 0..4 {
        let client = hub.anonymous(Some(&format!("192.0.2.{}", n + 1)));
        hub.sign_in(&client, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    assert!(kinds(&hub.pool).await.is_empty(), "below the threshold");
    for n in 4..21 {
        let client = hub.anonymous(Some(&format!("192.0.2.{}", n + 1)));
        hub.sign_in(&client, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    assert_eq!(
        kinds(&hub.pool).await,
        vec![("sign_in_failures".to_owned(), None)]
    );
    let (seen, url) = fake_gotify().await;
    let (_dir, sender) = sender(&url);
    sender.deliver_due(&hub.pool).await.unwrap();
    let message = seen.lock().unwrap()[0].clone();
    assert_eq!(message["title"], "Попытки входа в canterlot");
    assert_eq!(
        message["message"],
        "Неудачные попытки входа: 21 с 21 адреса за час"
    );
    // Через час — снова можно.
    sqlx::query("update owner_pushes set created_at=created_at-interval '61 minutes'")
        .execute(&hub.pool)
        .await
        .unwrap();
    hub.sign_in(
        &hub.anonymous(Some("192.0.2.200")),
        TEST_OWNER_LOGIN,
        "guess guess guess",
    )
    .await;
    let count: i64 =
        sqlx::query_scalar("select count(*) from owner_pushes where kind='sign_in_failures'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn auth_pushes_failure_text_declines_addresses() {
    assert_eq!(
        notify::sign_in_failures_text(37, 12),
        "Неудачные попытки входа: 37 с 12 адресов за час"
    );
    assert_eq!(
        notify::sign_in_failures_text(5, 1),
        "Неудачные попытки входа: 5 с 1 адреса за час"
    );
    assert_eq!(
        notify::sign_in_failures_text(9, 2),
        "Неудачные попытки входа: 9 с 2 адресов за час"
    );
    assert_eq!(
        notify::sign_in_failures_text(30, 11),
        "Неудачные попытки входа: 30 с 11 адресов за час"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_pushes_sign_in_kinds_have_no_conversation_and_others_need_one(pool: PgPool) {
    let insert = |kind: &'static str, key: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
                 values(gen_random_uuid(),null,$1,$2,'{}',now())",
            )
            .bind(kind)
            .bind(key)
            .execute(&pool)
            .await
        }
    };
    for kind in [
        "sign_in",
        "sign_in_failures",
        "sign_in_lock",
        "session_country",
    ] {
        insert(kind, kind).await.unwrap();
    }
    for kind in ["waiting", "turn_finished", "failure", "other"] {
        assert!(insert(kind, kind).await.is_err(), "{kind}");
    }
}
