//! Свежесть сеанса и повторный вход паролем (спецификация §5.4).

use canterlot_hub::{
    owner_auth::{Method as SignInMethod, recovery},
    testkit::{Method, TEST_SESSION_ID, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::auth::{AuthHub, PASSWORD, error_code, session_token};

/// Все действия управления доступом, которым нужна свежесть.
async fn guarded(hub: &AuthHub, client: &TestClient) -> Vec<(&'static str, TestResponse)> {
    let (other, _) = hub.session(SignInMethod::Password, None).await;
    vec![
        (
            "terminate another",
            client
                .send(
                    Method::DELETE,
                    &format!("/api/v1/auth/sessions/{other}"),
                    None,
                    None,
                )
                .await,
        ),
        (
            "terminate all others",
            client
                .send(
                    Method::POST,
                    "/api/v1/auth/sessions/revoke-others",
                    None,
                    None,
                )
                .await,
        ),
        (
            "first password",
            client
                .send(
                    Method::PUT,
                    "/api/v1/auth/password",
                    None,
                    Some(json!({"new_password": "first password here"})),
                )
                .await,
        ),
    ]
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_stepup_qr_born_session_is_never_fresh(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (qr, token) = hub.session(SignInMethod::Qr, Some(TEST_SESSION_ID)).await;
    let client = hub.with_session(&token, None);
    let list: Value = client
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    assert_eq!(list["fresh_until"], Value::Null);
    for (name, response) in guarded(&hub, &client).await {
        assert_eq!(response.status, 403, "{name}");
        assert_eq!(error_code(&response), "reauth_required", "{name}");
    }
    let authenticated: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select authenticated_at from owner_sessions where id=$1")
            .bind(qr)
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(authenticated, None);
    // Свободные действия: имя, срок бездействия, выход.
    let renamed = client
        .send(
            Method::PATCH,
            &format!("/api/v1/auth/sessions/{qr}"),
            None,
            Some(json!({"name": "Планшет"})),
        )
        .await;
    assert_eq!(renamed.status, 200);
    let settings = client
        .send(
            Method::PATCH,
            "/api/v1/auth/settings",
            None,
            Some(json!({"idle_timeout_days": 30})),
        )
        .await;
    assert_eq!(settings.status, 200);
    let logout = client
        .send(Method::POST, "/api/v1/auth/logout", None, None)
        .await;
    assert_eq!(logout.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_stepup_stale_session_reauths_with_the_password(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let (qr, token) = hub.session(SignInMethod::Qr, Some(TEST_SESSION_ID)).await;
    let client = hub.with_session(&token, Some("203.0.113.90"));
    let (other, _) = hub.session(SignInMethod::Password, None).await;
    let delete = || {
        let client = client.clone();
        async move {
            client
                .send(
                    Method::DELETE,
                    &format!("/api/v1/auth/sessions/{other}"),
                    None,
                    None,
                )
                .await
        }
    };
    assert_eq!(error_code(&delete().await), "reauth_required");
    let reauth = |password: &str| {
        let client = client.clone();
        let body = json!({"password": password});
        async move {
            client
                .send(
                    Method::POST,
                    "/api/v1/auth/reauth/password",
                    None,
                    Some(body),
                )
                .await
        }
    };
    let wrong = reauth("not the password").await;
    assert_eq!(wrong.status, 403);
    assert_eq!(error_code(&wrong), "wrong_password");
    let failures: i64 = sqlx::query_scalar("select count(*) from auth_failures")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(failures, 1, "a wrong reauth password counts");
    let ok = reauth(PASSWORD).await;
    assert_eq!(ok.status, 200);
    assert!(ok.json::<Value>()["fresh_until"].is_string());
    let deleted = delete().await;
    assert_eq!(deleted.status, 200);
    assert_eq!(hub.revoke_reason(other).await.as_deref(), Some("owner"));
    // Свежесть живёт 10 минут.
    hub.make_stale(qr).await;
    let (another, _) = hub.session(SignInMethod::Password, None).await;
    let response = client
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{another}"),
            None,
            None,
        )
        .await;
    assert_eq!(error_code(&response), "reauth_required");
    // Без пароля повторный вход невозможен: тот же ответ, что на неверный.
    sqlx::query("delete from owner_passwords")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(error_code(&reauth(PASSWORD).await), "wrong_password");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_stepup_each_guarded_action_works_when_fresh(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.make_stale(TEST_SESSION_ID).await;
    for (name, response) in guarded(&hub, &hub.owner()).await {
        assert_eq!(error_code(&response), "reauth_required", "{name}");
    }
    // Вход по ссылке восстановления свеж: им задают первый пароль.
    let token = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    let recovered = hub
        .anonymous(Some("203.0.113.91"))
        .send(
            Method::POST,
            "/api/v1/auth/recovery",
            None,
            Some(json!({"token": token})),
        )
        .await;
    assert_eq!(recovered.status, 200);
    let client = hub.with_session(&session_token(&recovered), Some("203.0.113.91"));
    for (name, response) in guarded(&hub, &client).await {
        assert_eq!(response.status, 200, "{name}: {:?}", response.body);
    }
    // После «завершить все остальные» и смены пароля жив только этот сеанс.
    let live: Vec<Uuid> =
        sqlx::query_scalar("select id from owner_sessions where revoked_at is null")
            .fetch_all(&hub.pool)
            .await
            .unwrap();
    assert_eq!(live.len(), 1);
}
