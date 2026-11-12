//! Отзыв сеанса закрывает его WebSocket кодом 4401 с причиной `revoked` (спецификация §5.5).

use std::time::Duration;

use canterlot_hub::{
    owner_auth::{Method as SignInMethod, RevokeReason, sessions},
    testkit::{TEST_ORIGIN, TestWs},
};
use serde_json::json;
use sqlx::PgPool;

use crate::support::auth::AuthHub;

async fn hub(pool: PgPool, check: Duration) -> AuthHub {
    AuthHub::with(pool, |c| c.ws.session_check_interval = check, false)
        .await
        .serve()
        .await
}

async fn closed_as_revoked(ws: &mut TestWs, within: Duration) {
    let frame = ws
        .next(within)
        .await
        .expect("close frame before the deadline");
    assert_eq!(frame, json!({"op": "close", "reason": "revoked"}));
    assert_eq!(ws.next(Duration::from_secs(2)).await, None);
    assert_eq!(ws.close_code, Some(4401));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_ws_revocation_closes_the_socket_within_two_seconds(pool: PgPool) {
    let hub = hub(pool, Duration::from_secs(60)).await;
    let addr = hub.addr.unwrap();
    let (id, token) = hub.session(SignInMethod::Password, None).await;
    let (_, other_token) = hub.session(SignInMethod::Password, None).await;
    let mut ws = TestWs::connect(addr, TEST_ORIGIN, Some(&token))
        .await
        .unwrap();
    let mut other = TestWs::connect(addr, TEST_ORIGIN, Some(&other_token))
        .await
        .unwrap();
    // Сокет жив и отвечает до отзыва.
    ws.send(json!({"op": "unsubscribe", "id": "x"})).await;
    assert!(ws.next(Duration::from_secs(2)).await.is_some());
    sessions::revoke(&hub.pool, id, RevokeReason::Owner, None)
        .await
        .unwrap();
    closed_as_revoked(&mut ws, Duration::from_secs(2)).await;
    // Чужой сокет уведомление тоже будит, но его сеанс жив.
    other.send(json!({"op": "unsubscribe", "id": "y"})).await;
    assert!(other.next(Duration::from_secs(2)).await.is_some());
    assert_eq!(other.close_code, None);
    // Отозванный токен больше не открывает сокет.
    assert!(
        TestWs::connect(addr, TEST_ORIGIN, Some(&token))
            .await
            .is_err()
    );
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_ws_lost_notice_is_caught_by_the_periodic_check(pool: PgPool) {
    let hub = hub(pool, Duration::from_millis(500)).await;
    let (id, token) = hub.session(SignInMethod::Password, None).await;
    let mut ws = TestWs::connect(hub.addr.unwrap(), TEST_ORIGIN, Some(&token))
        .await
        .unwrap();
    canterlot_hub::bus::EventBus::local().drop_next_notice(canterlot_hub::owner_auth::BUS_SCOPE);
    sessions::revoke(&hub.pool, id, RevokeReason::Owner, None)
        .await
        .unwrap();
    closed_as_revoked(&mut ws, Duration::from_secs(3)).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_ws_idle_expiry_closes_the_socket_on_the_periodic_check(pool: PgPool) {
    let hub = hub(pool, Duration::from_millis(300)).await;
    let (id, token) = hub.session(SignInMethod::Password, None).await;
    let mut ws = TestWs::connect(hub.addr.unwrap(), TEST_ORIGIN, Some(&token))
        .await
        .unwrap();
    // Живой сеанс переживает несколько проверок.
    tokio::time::sleep(Duration::from_millis(800)).await;
    ws.send(json!({"op": "unsubscribe", "id": "x"})).await;
    assert!(ws.next(Duration::from_secs(2)).await.is_some());
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '91 days' where id=$1")
        .bind(id)
        .execute(&hub.pool)
        .await
        .unwrap();
    closed_as_revoked(&mut ws, Duration::from_secs(3)).await;
    assert_eq!(hub.revoke_reason(id).await.as_deref(), Some("idle"));
}
