//! Пуши о конце входа Claude: предупреждение за неделю — один раз на машину, провайдера
//! и дату; отказ входа — один раз на эпизод, а новый эпизод открывает успешный ход.

use canterlot_hub::{
    activity::Evidence,
    config::HubConfig,
    limits,
    notify::{self, PushSettings, Sender},
    session_tools::settle::{SettleFacts, Settled},
    testkit,
};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::auth;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn at(offset: Duration) -> DateTime<Utc> {
    DateTime::from_timestamp_micros((Utc::now() + offset).timestamp_micros()).unwrap()
}

async fn record(
    pool: &PgPool,
    machine_id: Uuid,
    provider: &str,
    expires_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
) -> bool {
    let mut tx = pool.begin().await.unwrap();
    let changed = limits::record_credential_in_tx(
        &mut tx,
        machine_id,
        provider,
        expires_at,
        observed_at,
        Utc::now(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    changed
}

/// Оседание сессии без исполнения: то же решение, что принимает проектор.
async fn settle(
    pool: &PgPool,
    conversation: Uuid,
    state: Settled,
    evidence: &Evidence,
    now: DateTime<Utc>,
    fingerprint: &str,
) {
    let facts = SettleFacts {
        now,
        since: now - Duration::seconds(5),
        last_turn_at: None,
        last_event_at: None,
        phase_since: None,
    };
    let mut tx = pool.begin().await.unwrap();
    notify::on_settled_in_tx(&mut tx, conversation, state, fingerprint, &facts, evidence)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn enable_pushes(pool: &PgPool) {
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

async fn push_rows(pool: &PgPool, kind: &str) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as("select state,payload from owner_pushes where kind=$1 order by created_at,id")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn expiry_push_fires_once_per_date_and_rearms_after_a_new_login() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    enable_pushes(&pool).await;
    let soon = at(Duration::days(3));
    record(
        &pool,
        s.machine_id,
        "claude",
        soon,
        at(Duration::seconds(-1)),
    )
    .await;

    assert_eq!(notify::credential_expiry_sweep(&pool).await?, 1);
    assert_eq!(
        notify::credential_expiry_sweep(&pool).await?,
        0,
        "the same date is not pushed twice"
    );
    let rows = push_rows(&pool, notify::KIND_CREDENTIAL_EXPIRY).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "pending");

    let (seen, url) = auth::fake_gotify().await;
    let (_dir, sender) = sender(&url);
    assert_eq!(sender.deliver_due(&pool).await?.sent, 1);
    let message = seen.lock().unwrap()[0].clone();
    assert_eq!(message["title"], "Вход в Claude Code заканчивается");
    assert_eq!(
        message["message"],
        format!(
            "Машина synthetic: до {}. Запустите claude на машине и выполните /login.",
            soon.format("%d.%m")
        )
    );
    assert_eq!(
        message["extras"]["client::notification"]["click"]["url"],
        "https://app.example.test/machines"
    );

    // Повторный вход дал новую дату, и она тоже уже близко: пуш взводится заново.
    let renewed = at(Duration::days(6));
    record(
        &pool,
        s.machine_id,
        "claude",
        renewed,
        at(Duration::seconds(-1)),
    )
    .await;
    assert_eq!(notify::credential_expiry_sweep(&pool).await?, 1);
    assert_eq!(
        push_rows(&pool, notify::KIND_CREDENTIAL_EXPIRY).await.len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn auth_required_pushes_once_per_episode_and_rearms_after_a_successful_turn() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    enable_pushes(&pool).await;
    let now = Utc::now();
    let failed = Evidence::Failed {
        reason: "auth_required".into(),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now,
        "auth-1",
    )
    .await;
    assert_eq!(
        push_rows(&pool, notify::KIND_CREDENTIAL_AUTH).await.len(),
        1
    );
    // Дребезг того же эпизода (новый отпечаток) второго пуша не даёт.
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now + Duration::seconds(5),
        "auth-2",
    )
    .await;
    assert_eq!(
        push_rows(&pool, notify::KIND_CREDENTIAL_AUTH).await.len(),
        1
    );

    let (seen, url) = auth::fake_gotify().await;
    let (_dir, sender) = sender(&url);
    sender.deliver_due(&pool).await?;
    let seen = seen.lock().unwrap().clone();
    let message = seen
        .iter()
        .find(|message| message["title"] == "Claude Code просит войти заново")
        .expect("auth push delivered");
    assert_eq!(
        message["message"],
        "Машина synthetic: запустите claude и выполните /login."
    );

    // Успешный ход открывает новый эпизод.
    let idle = Evidence::Idle {
        last_turn_at: Some(now + Duration::minutes(1)),
        background_work: None,
        process_alive: Some(true),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Idle,
        &idle,
        now + Duration::minutes(1),
        "idle-ok",
    )
    .await;
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now + Duration::minutes(2),
        "auth-3",
    )
    .await;
    assert_eq!(
        push_rows(&pool, notify::KIND_CREDENTIAL_AUTH).await.len(),
        2
    );
    Ok(())
}
