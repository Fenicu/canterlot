//! Известные устройства: cookie выдаётся только входу паролем, ключом или ссылкой, строка
//! устройства привязана к сеансу последнего входа и уходит вместе с ним при отзыве
//! владельцем, сервером, сменой пароля или каскадом; выход и бездействие её не трогают.

use canterlot_hub::{
    owner_auth::{DEVICE_COOKIE, SESSION_COOKIE, cli, sessions},
    testkit::{Method, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::auth::{AuthHub, PASSWORD, set_cookie};

struct SignedIn {
    id: Uuid,
    token: String,
    device: String,
}

fn signed_in(response: &TestResponse) -> SignedIn {
    assert_eq!(response.status, 200, "{:?}", response.body);
    SignedIn {
        id: response.json::<Value>()["session"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
        token: set_cookie(response, SESSION_COOKIE).unwrap().0,
        device: set_cookie(response, DEVICE_COOKIE)
            .expect("device cookie")
            .0,
    }
}

async fn sign_in(hub: &AuthHub, client: &TestClient) -> SignedIn {
    signed_in(&hub.sign_in_owner(client).await)
}

async fn known(hub: &AuthHub, device: &str) -> bool {
    sessions::device_known(&hub.pool, hub.owner_id(), device)
        .await
        .unwrap()
}

async fn devices(hub: &AuthHub) -> i64 {
    sqlx::query_scalar("select count(*) from owner_devices")
        .fetch_one(&hub.pool)
        .await
        .unwrap()
}

fn with_device(client: TestClient, device: &str) -> TestClient {
    client.with_header("Cookie", &format!("{DEVICE_COOKIE}={device}"))
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_devices_qr_sign_in_gets_no_device_cookie(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let client = hub.anonymous(Some("203.0.113.80"));
    let started = client
        .send(Method::POST, "/api/v1/auth/qr/start", None, Some(json!({})))
        .await;
    assert_eq!(started.status, 200);
    let started: Value = started.json();
    let secret = started["url"].as_str().unwrap().split('#').nth(1).unwrap();
    let approved = hub
        .owner()
        .send(
            Method::POST,
            "/api/v1/auth/qr/approve",
            None,
            Some(json!({"secret": secret})),
        )
        .await;
    assert_eq!(approved.status, 200);
    let delivered = client
        .send(
            Method::POST,
            "/api/v1/auth/qr/poll",
            None,
            Some(json!({"poll_token": started["poll_token"]})),
        )
        .await;
    assert_eq!(delivered.json::<Value>()["state"], "approved");
    assert!(set_cookie(&delivered, SESSION_COOKIE).is_some());
    assert!(
        set_cookie(&delivered, DEVICE_COOKIE).is_none(),
        "a QR-born session is no proof of the password"
    );
    assert_eq!(devices(&hub).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_devices_follow_the_session_of_their_last_sign_in(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let a = sign_in(&hub, &hub.anonymous(Some("203.0.113.81"))).await;
    let b = sign_in(&hub, &hub.anonymous(Some("203.0.113.82"))).await;
    let c = sign_in(&hub, &hub.anonymous(Some("203.0.113.83"))).await;
    assert_eq!(devices(&hub).await, 3);

    // Выход не забывает устройство.
    let out = hub
        .with_session(&b.token, Some("203.0.113.82"))
        .send(Method::POST, "/api/v1/auth/logout", None, None)
        .await;
    assert_eq!(out.status, 200);
    assert!(known(&hub, &b.device).await, "logout keeps the device");

    // Завершение владельцем уносит устройство сеанса.
    let owner_a = hub.with_session(&a.token, Some("203.0.113.81"));
    let revoked = owner_a
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{}", c.id),
            None,
            None,
        )
        .await;
    assert_eq!(revoked.status, 200, "{:?}", revoked.body);
    assert!(
        !known(&hub, &c.device).await,
        "owner revocation forgets the device"
    );
    assert!(known(&hub, &a.device).await);

    // «Завершить остальные»: устройства остальных уходят, своё остаётся.
    let d = sign_in(&hub, &hub.anonymous(Some("203.0.113.84"))).await;
    let others = owner_a
        .send(
            Method::POST,
            "/api/v1/auth/sessions/revoke-others",
            None,
            None,
        )
        .await;
    assert_eq!(others.status, 200, "{:?}", others.body);
    assert!(!known(&hub, &d.device).await);
    assert!(known(&hub, &a.device).await);

    // Повторный вход с той же cookie переносит устройство на новый сеанс: отзыв старого
    // сеанса его больше не трогает.
    let e = sign_in(
        &hub,
        &with_device(hub.anonymous(Some("203.0.113.85")), &a.device),
    )
    .await;
    assert_eq!(e.device, a.device, "the same device cookie is kept");
    let revoked = hub
        .with_session(&e.token, Some("203.0.113.85"))
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{}", a.id),
            None,
            None,
        )
        .await;
    assert_eq!(revoked.status, 200, "{:?}", revoked.body);
    assert!(
        known(&hub, &a.device).await,
        "the device moved to session e"
    );

    // Смена пароля забывает все устройства, включая своё.
    let changed = hub
        .with_session(&e.token, Some("203.0.113.85"))
        .send(
            Method::PUT,
            "/api/v1/auth/password",
            None,
            Some(json!({"current_password": PASSWORD, "new_password": "another long password"})),
        )
        .await;
    assert_eq!(changed.status, 200, "{:?}", changed.body);
    assert_eq!(devices(&hub).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_devices_are_forgotten_by_revoke_all_and_set_password(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let a = sign_in(&hub, &hub.anonymous(Some("203.0.113.86"))).await;
    cli::revoke_all(&hub.pool, hub.owner_id()).await.unwrap();
    assert!(!known(&hub, &a.device).await);

    let b = sign_in(&hub, &hub.anonymous(Some("203.0.113.87"))).await;
    cli::set_password(&hub.pool, hub.owner_id(), PASSWORD.into(), false)
        .await
        .unwrap();
    assert!(
        !known(&hub, &b.device).await,
        "a new password forgets devices"
    );
    assert_eq!(
        hub.revoke_reason(b.id).await,
        None,
        "without --revoke-sessions the session lives on"
    );
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_devices_a_failure_after_the_session_commit_revokes_it(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    hub.state
        .auth_faults
        .drop_next_at(Point::SignInAfterSession, "");
    let failed = hub
        .sign_in_owner(&hub.anonymous(Some("203.0.113.88")))
        .await;
    assert_eq!(failed.status, 503, "{:?}", failed.body);
    assert!(set_cookie(&failed, SESSION_COOKIE).is_none());
    let reasons: Vec<Option<String>> = sqlx::query_scalar(
        "select revoke_reason from owner_sessions where created_ip='203.0.113.88'",
    )
    .fetch_all(&hub.pool)
    .await
    .unwrap();
    assert_eq!(
        reasons,
        [Some("server".to_owned())],
        "no live orphan session"
    );
    assert_eq!(devices(&hub).await, 0, "no orphan device row");
    // Следующая попытка проходит.
    let ok = hub
        .sign_in_owner(&hub.anonymous(Some("203.0.113.88")))
        .await;
    assert_eq!(ok.status, 200);
}
