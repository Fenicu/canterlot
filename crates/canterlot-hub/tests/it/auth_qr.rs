//! Вход по QR (спецификация §7): машина состояний, ровно одна выдача сеанса, отказ,
//! истечение, общие пределы, подтверждение только из сеанса, каскад от подтвердившего.

use std::time::Duration;

use canterlot_hub::{
    owner_auth::{Method as SignInMethod, SESSION_COOKIE},
    testkit::{Method, TEST_ORIGIN, TEST_SESSION_ID, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::auth::{AuthHub, error_code, set_cookie};

const FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Gecko/20100101 Firefox/159.0";

struct Started {
    secret: String,
    poll_token: String,
    body: Value,
}

async fn start(hub: &AuthHub, ip: &str) -> TestResponse {
    hub.anonymous(Some(ip))
        .with_header("User-Agent", FIREFOX)
        .send(
            Method::POST,
            "/api/v1/auth/qr/start",
            None,
            Some(json!({"device": {"standalone": true}})),
        )
        .await
}

async fn started(hub: &AuthHub, ip: &str) -> Started {
    let response = start(hub, ip).await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let body: Value = response.json();
    let url = body["url"].as_str().unwrap();
    let secret = url
        .strip_prefix(&format!("{TEST_ORIGIN}/qr#"))
        .unwrap_or_else(|| panic!("{url}"))
        .to_owned();
    Started {
        secret,
        poll_token: body["poll_token"].as_str().unwrap().to_owned(),
        body,
    }
}

fn poller(hub: &AuthHub) -> TestClient {
    hub.anonymous(Some("203.0.113.50"))
        .with_header("User-Agent", FIREFOX)
}

async fn poll(client: &TestClient, token: &str, after: Option<&str>) -> TestResponse {
    let mut body = json!({"poll_token": token});
    if let Some(after) = after {
        body["after"] = json!(after);
    }
    client
        .send(Method::POST, "/api/v1/auth/qr/poll", None, Some(body))
        .await
}

fn state(response: &TestResponse) -> String {
    assert_eq!(response.status, 200, "{:?}", response.body);
    response.json::<Value>()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn act(client: &TestClient, action: &str, secret: &str) -> TestResponse {
    client
        .send(
            Method::POST,
            &format!("/api/v1/auth/qr/{action}"),
            None,
            Some(json!({"secret": secret})),
        )
        .await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_start_scan_approve_signs_in_once(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let qr = started(&hub, "203.0.113.50").await;
    assert_eq!(qr.secret.len(), 43);
    assert_eq!(qr.poll_token.len(), 43);
    assert_ne!(qr.secret, qr.poll_token);
    let expires: chrono::DateTime<chrono::Utc> =
        serde_json::from_value(qr.body["expires_at"].clone()).unwrap();
    let ttl = expires - chrono::Utc::now();
    assert!(ttl <= chrono::TimeDelta::seconds(60) && ttl > chrono::TimeDelta::seconds(50));

    // Опрос ждёт, пока код не отсканируют.
    let client = poller(&hub);
    let waiting = {
        let client = client.clone();
        let token = qr.poll_token.clone();
        tokio::spawn(async move { poll(&client, &token, None).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "the poll waits while pending");
    let card = act(&hub.owner(), "inspect", &qr.secret).await;
    assert_eq!(card.status, 200, "{:?}", card.body);
    let card: Value = card.json();
    assert_eq!(card["ip"], "203.0.113.50");
    assert_eq!(card["label"], "Firefox 159 · Linux · приложение");
    assert_eq!(card["user_agent"], FIREFOX);
    assert!(card["age_seconds"].as_i64().unwrap() <= 2);
    assert_eq!(card["other_country"], false);
    let scanned = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("woken by the scan")
        .unwrap();
    assert_eq!(state(&scanned), "scanned");
    assert!(set_cookie(&scanned, SESSION_COOKIE).is_none());

    let waiting = {
        let client = client.clone();
        let token = qr.poll_token.clone();
        tokio::spawn(async move { poll(&client, &token, Some("scanned")).await })
    };
    let approved = act(&hub.owner(), "approve", &qr.secret).await;
    assert_eq!(approved.status, 200, "{:?}", approved.body);
    let delivered = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("woken by the approval")
        .unwrap();
    assert_eq!(state(&delivered), "approved");
    let (token, _) = set_cookie(&delivered, SESSION_COOKIE).expect("session cookie");
    let session = &delivered.json::<Value>()["session"];
    assert_eq!(session["method"], "qr");
    assert_eq!(session["approved_by"], TEST_SESSION_ID.to_string());
    assert_eq!(session["label"], "Firefox 159 · Linux · приложение");
    let id: Uuid = session["id"].as_str().unwrap().parse().unwrap();
    let authenticated: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select authenticated_at from owner_sessions where id=$1")
            .bind(id)
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(authenticated, None, "a QR-born session is never fresh");
    let me = hub
        .with_session(&token, Some("203.0.113.50"))
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(me.status, 200);

    // Повтор опроса сеанса не выдаёт.
    let again = poll(&client, &qr.poll_token, Some("scanned")).await;
    assert_eq!(state(&again), "expired");
    assert!(set_cookie(&again, SESSION_COOKIE).is_none());
    assert_eq!(act(&hub.owner(), "approve", &qr.secret).await.status, 410);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_two_concurrent_polls_get_exactly_one_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    for _ in 0..5 {
        let qr = started(&hub, "203.0.113.50").await;
        assert_eq!(act(&hub.owner(), "approve", &qr.secret).await.status, 200);
        let polls: Vec<_> = (0..2)
            .map(|_| {
                let client = poller(&hub);
                let token = qr.poll_token.clone();
                tokio::spawn(async move { poll(&client, &token, None).await })
            })
            .collect();
        let mut cookies = 0;
        let mut states = Vec::new();
        for handle in polls {
            let response = handle.await.unwrap();
            states.push(state(&response));
            cookies += usize::from(set_cookie(&response, SESSION_COOKIE).is_some());
        }
        states.sort();
        assert_eq!(states, ["approved", "expired"]);
        assert_eq!(cookies, 1);
    }
    let qr_sessions: i64 =
        sqlx::query_scalar("select count(*) from owner_sessions where method='qr'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(qr_sessions, 5);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_deny_and_expiry_end_the_code(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let client = poller(&hub);
    let denied = started(&hub, "203.0.113.50").await;
    let deny = act(&hub.owner(), "deny", &denied.secret).await;
    assert_eq!(deny.status, 200);
    assert_eq!(
        state(&poll(&client, &denied.poll_token, None).await),
        "denied"
    );
    let late = act(&hub.owner(), "approve", &denied.secret).await;
    assert_eq!(late.status, 410);
    assert_eq!(error_code(&late), "qr_expired");

    let expired = started(&hub, "203.0.113.50").await;
    sqlx::query("update owner_qr_logins set expires_at=now()-interval '1 second'")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(
        state(&poll(&client, &expired.poll_token, None).await),
        "expired"
    );
    for action in ["inspect", "approve", "deny"] {
        let response = act(&hub.owner(), action, &expired.secret).await;
        assert_eq!(response.status, 410, "{action}");
        assert_eq!(error_code(&response), "qr_expired", "{action}");
    }

    // Одобрение старше минуты больше не выдаёт сеанс.
    let stale = started(&hub, "203.0.113.50").await;
    assert_eq!(
        act(&hub.owner(), "approve", &stale.secret).await.status,
        200
    );
    sqlx::query(
        "update owner_qr_logins set approved_at=now()-interval '61 seconds' where state='approved'",
    )
    .execute(&hub.pool)
    .await
    .unwrap();
    let response = poll(&client, &stale.poll_token, None).await;
    assert_eq!(state(&response), "expired");
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
    // Неизвестный токен опроса — тоже «истёк».
    let unknown = poll(&client, "x".repeat(43).as_str(), None).await;
    assert_eq!(state(&unknown), "expired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_inspect_approve_and_deny_need_a_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let qr = started(&hub, "203.0.113.50").await;
    for action in ["inspect", "approve", "deny"] {
        let response = act(&hub.anonymous(Some("203.0.113.50")), action, &qr.secret).await;
        assert_eq!(response.status, 401, "{action}");
        assert_eq!(error_code(&response), "unauthenticated", "{action}");
    }
    let state_now: String = sqlx::query_scalar("select state from owner_qr_logins")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(state_now, "pending");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_secret_and_poll_token_are_not_interchangeable(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let qr = started(&hub, "203.0.113.50").await;
    let wrong = act(&hub.owner(), "inspect", &qr.poll_token).await;
    assert_eq!(wrong.status, 410);
    assert_eq!(
        act(&hub.owner(), "approve", &qr.poll_token).await.status,
        410
    );
    let response = poll(&poller(&hub), &qr.secret, None).await;
    assert_eq!(state(&response), "expired");
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_start_is_limited_per_bucket_and_in_total(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    for _ in 0..10 {
        assert_eq!(start(&hub, "203.0.113.50").await.status, 200);
    }
    let limited = start(&hub, "203.0.113.50").await;
    assert_eq!(limited.status, 429);
    assert_eq!(error_code(&limited), "too_many_attempts");
    let retry = limited.json::<Value>()["error"]["details"]["retry_after_seconds"]
        .as_u64()
        .unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    // Тот же /64 IPv6 — та же корзина.
    for n in 0..10 {
        let ip = format!("2001:db8:5:6::{n:x}");
        assert_eq!(start(&hub, &ip).await.status, 200);
    }
    assert_eq!(start(&hub, "2001:db8:5:6:ffff::1").await.status, 429);
    // Всего не больше 20 ожидающих кодов: 21-й с нового адреса получает 429.
    let total = start(&hub, "198.51.100.77").await;
    assert_eq!(total.status, 429);
    assert_eq!(error_code(&total), "too_many_attempts");
    sqlx::query("update owner_qr_logins set expires_at=now()-interval '1 second'")
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(start(&hub, "198.51.100.77").await.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_at_most_eight_polls_wait_at_once(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let qr = started(&hub, "203.0.113.50").await;
    let waiting: Vec<_> = (0..8)
        .map(|_| {
            let client = poller(&hub);
            let token = qr.poll_token.clone();
            tokio::spawn(async move { poll(&client, &token, None).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ninth = poll(&poller(&hub), &qr.poll_token, None).await;
    assert_eq!(ninth.status, 429);
    assert_eq!(error_code(&ninth), "too_many_attempts");
    assert_eq!(act(&hub.owner(), "deny", &qr.secret).await.status, 200);
    for handle in waiting {
        let response = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state(&response), "denied");
    }
    assert_eq!(
        state(&poll(&poller(&hub), &qr.poll_token, None).await),
        "denied"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_born_session_dies_with_its_approver(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (approver, approver_token) = hub.session(SignInMethod::Password, None).await;
    let approver_client = hub.with_session(&approver_token, Some("198.51.100.9"));
    let qr = started(&hub, "203.0.113.50").await;
    assert_eq!(
        act(&approver_client, "approve", &qr.secret).await.status,
        200
    );
    let delivered = poll(&poller(&hub), &qr.poll_token, None).await;
    assert_eq!(state(&delivered), "approved");
    let (token, _) = set_cookie(&delivered, SESSION_COOKIE).unwrap();
    let qr_client = hub.with_session(&token, Some("203.0.113.50"));
    assert_eq!(
        delivered.json::<Value>()["session"]["approved_by"],
        approver.to_string()
    );
    let revoked = hub
        .owner()
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{approver}"),
            None,
            None,
        )
        .await;
    assert_eq!(revoked.status, 200);
    let after = qr_client.send(Method::GET, "/api/v1/me", None, None).await;
    assert_eq!(after.status, 401);
    assert_eq!(error_code(&after), "session_revoked");
    assert_eq!(
        after.json::<Value>()["error"]["details"]["reason"],
        "cascade"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_approval_by_a_revoked_session_is_not_delivered(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (approver, approver_token) = hub.session(SignInMethod::Password, None).await;
    let qr = started(&hub, "203.0.113.50").await;
    let approve = act(
        &hub.with_session(&approver_token, None),
        "approve",
        &qr.secret,
    )
    .await;
    assert_eq!(approve.status, 200);
    canterlot_hub::owner_auth::sessions::revoke(
        &hub.pool,
        approver,
        canterlot_hub::owner_auth::RevokeReason::Owner,
        None,
    )
    .await
    .unwrap();
    let response = poll(&poller(&hub), &qr.poll_token, None).await;
    assert_eq!(state(&response), "expired");
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_one_session_approves_at_most_five_live_sessions(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (_, approver_token) = hub.session(SignInMethod::Password, None).await;
    let approver = hub.with_session(&approver_token, Some("198.51.100.9"));
    // Одобрено, но ещё не забрано: тоже считается.
    let mut waiting = Vec::new();
    for n in 0..5 {
        let qr = started(&hub, &format!("203.0.113.{}", 100 + n)).await;
        assert_eq!(act(&approver, "approve", &qr.secret).await.status, 200);
        waiting.push(qr);
    }
    let sixth = started(&hub, "203.0.113.110").await;
    let refused = act(&approver, "approve", &sixth.secret).await;
    assert_eq!(refused.status, 409, "{:?}", refused.body);
    assert_eq!(error_code(&refused), "qr_approvals_exhausted");
    // Забранные одобрения — живые сеансы этого сеанса, предел тот же.
    for qr in &waiting {
        assert_eq!(
            state(&poll(&poller(&hub), &qr.poll_token, None).await),
            "approved"
        );
    }
    assert_eq!(
        error_code(&act(&approver, "approve", &sixth.secret).await),
        "qr_approvals_exhausted"
    );
    // Другой сеанс одобряет как обычно; код всё ещё ждёт.
    assert_eq!(
        act(&hub.owner(), "approve", &sixth.secret).await.status,
        200
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_qr_delivery_never_evicts_other_sessions(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    // Заранее заведённый сеанс владельца плюс 49: предел 50 достигнут.
    for _ in 0..49 {
        hub.session(SignInMethod::Password, None).await;
    }
    let qr = started(&hub, "203.0.113.50").await;
    assert_eq!(act(&hub.owner(), "approve", &qr.secret).await.status, 200);
    let response = poll(&poller(&hub), &qr.poll_token, None).await;
    assert_eq!(state(&response), "expired");
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
    let (live, limited): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where revoked_at is null),
                count(*) filter (where revoke_reason='limit') from owner_sessions",
    )
    .fetch_one(&hub.pool)
    .await
    .unwrap();
    assert_eq!(
        (live, limited),
        (50, 0),
        "no session evicted for a QR-born one"
    );
}
