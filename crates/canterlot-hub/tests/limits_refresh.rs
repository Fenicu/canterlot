//! `POST /api/v1/limits/refresh`: просьба владельца обновить лимиты уходит воркерам не чаще раза в 15 секунд.

#[path = "support/task7.rs"]
mod support;

use axum::http::Method;
use canterlot_hub::testkit;
use canterlot_proto::worker::v1::control_down;
use serde_json::Value;
use sqlx::PgPool;
use support::Fixture;

async fn post(f: &Fixture) -> (u16, Value) {
    let r = f
        .client
        .send(Method::POST, "/api/v1/limits/refresh", None, None)
        .await;
    (r.status.as_u16(), r.json())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn refresh_asks_the_worker_once_and_throttles_the_repeat(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut session =
        testkit::fake_session(&f.state.registry, f.seed.machine_id, &["pilot_v1"], true);

    let (status, body) = post(&f).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["requested"], serde_json::json!(["claude"]));
    assert_eq!(body["retry_after_secs"], 15);
    let message = session.commands.try_recv().expect("poll request");
    assert!(matches!(
        message.msg,
        Some(control_down::Msg::PollLimits(p)) if p.provider == "claude" && p.owner_refresh
    ));

    let (status, body) = post(&f).await;
    assert_eq!(status, 429, "{body}");
    assert_eq!(body["error"]["code"], "too_many_requests");
    assert!(session.commands.try_recv().is_err(), "no second request");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn refresh_without_an_available_worker_is_machine_unavailable(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (status, body) = post(&f).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "machine_unavailable");
}
