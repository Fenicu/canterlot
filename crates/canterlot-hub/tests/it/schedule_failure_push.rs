//! Пуши о несработавших запусках расписаний: охлаждение проверяется при доставке,
//! а не при постановке (план #190, 6).

use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};

use axum::{Router, extract::State, routing::post};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    auth_pushes::{enable, sender},
    support::task7::Fixture,
};

/// Поддельный Gotify с управляемым статусом ответа.
async fn gotify(status: Arc<AtomicU16>) -> (Arc<std::sync::Mutex<Vec<Value>>>, String) {
    #[derive(Clone)]
    struct Shared {
        seen: Arc<std::sync::Mutex<Vec<Value>>>,
        status: Arc<AtomicU16>,
    }
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/gotify/message",
            post(
                |State(shared): State<Shared>, body: axum::body::Bytes| async move {
                    shared
                        .seen
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                    axum::http::StatusCode::from_u16(shared.status.load(Ordering::SeqCst)).unwrap()
                },
            ),
        )
        .with_state(Shared {
            seen: seen.clone(),
            status,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (seen, format!("http://{addr}/gotify"))
}

/// Расписание с битой целью: каждый тик даёт `failed` (разговора нет).
async fn broken_schedule(f: &Fixture) -> Uuid {
    let r = f
        .client
        .send(
            axum::http::Method::POST,
            "/api/v1/schedules",
            Some("push-create"),
            Some(json!({
                "name": "Битая цель",
                "cron": "0 9 * * *",
                "target": {"kind": "existing_session", "conversation_id": Uuid::now_v7()},
                "prompt": "не дойдёт",
            })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    Uuid::parse_str(r.json::<Value>()["id"].as_str().unwrap()).unwrap()
}

/// Один исход `failed`: тик, ожидание итога.
async fn fail_once(f: &Fixture, id: Uuid) {
    sqlx::query("update schedules set next_fire_at=$2 where id=$1")
        .bind(id)
        .bind(Utc::now() - TimeDelta::seconds(30))
        .execute(&f.pool)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar(
        "select count(*) from schedule_runs where schedule_id=$1 and state='failed'",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    crate::scheduled_runs_pipeline::scheduler(f)
        .tick(Utc::now())
        .await
        .unwrap();
    for _ in 0..20_000 {
        let now: i64 = sqlx::query_scalar(
            "select count(*) from schedule_runs where schedule_id=$1 and state='failed'",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        if now > before {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("run never failed");
}

#[tokio::test]
async fn delayed_first_push_does_not_open_a_second_one_within_the_hour() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let status = Arc::new(AtomicU16::new(200));
    let (seen, url) = gotify(status).await;
    let (_dir, sender) = sender(&url);
    // Первый пуш поставлен давно, доставлен только теперь: охлаждение
    // считается от доставки.
    fail_once(&f, id).await;
    sqlx::query("update owner_pushes set created_at=created_at-interval '20 minutes',run_at=run_at-interval '20 minutes' where kind='schedule_failed'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(sender.deliver_due(&pool).await.unwrap().sent, 1);
    // Второй исход через сорок минут после доставки: час ещё не прошёл.
    sqlx::query("update schedules set last_failure_push_at=clock_timestamp()-interval '41 minutes' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    fail_once(&f, id).await;
    let report = sender.deliver_due(&pool).await.unwrap();
    assert_eq!((report.sent, report.suppressed), (0, 1));
    // Через час после доставки новый пуш уходит.
    sqlx::query("update schedules set last_failure_push_at=clock_timestamp()-interval '61 minutes' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    fail_once(&f, id).await;
    assert_eq!(sender.deliver_due(&pool).await.unwrap().sent, 1);
    assert_eq!(seen.lock().unwrap().len(), 2);
}
