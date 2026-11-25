//! Пуши о несработавших запусках расписаний: охлаждение проверяется при доставке,
//! а не при постановке (план #190, 6).

use std::sync::{
    Arc,
    atomic::{AtomicU16, Ordering},
};

use axum::{Router, extract::State, routing::post};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
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

async fn pending(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from owner_pushes where kind='schedule_failed' and state='pending'
         and payload->>'schedule_id'=$1",
    )
    .bind(id.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn pending_failure_pushes_of_one_schedule_coalesce() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    fail_once(&f, id).await;
    fail_once(&f, id).await;
    assert_eq!(pending(&pool, id).await, 1, "one pending push per schedule");
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

#[tokio::test]
async fn dropped_first_push_does_not_suppress_the_next_failure() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let status = Arc::new(AtomicU16::new(400));
    let (seen, url) = gotify(status.clone()).await;
    let (_dir, sender) = sender(&url);
    fail_once(&f, id).await;
    let report = sender.deliver_due(&pool).await.unwrap();
    assert_eq!(report.dropped, 1, "a rejected push is dropped");
    let last: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("select last_failure_push_at from schedules where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        last.is_none(),
        "a push that was not delivered leaves no cooldown"
    );
    status.store(200, Ordering::SeqCst);
    fail_once(&f, id).await;
    assert_eq!(sender.deliver_due(&pool).await.unwrap().sent, 1);
    assert_eq!(seen.lock().unwrap().len(), 2);
}

/// Сервер принимает запрос и обрывает соединение без ответа: доставка неоднозначна.
pub(crate) async fn dropping_gotify() -> (Arc<std::sync::atomic::AtomicUsize>, String) {
    use tokio::io::AsyncReadExt;
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                // Читаем запрос целиком (заголовки и тело одним куском на практике).
                let _ = socket.read(&mut buf).await;
                counter.fetch_add(1, Ordering::SeqCst);
                drop(socket);
            });
        }
    });
    (hits, format!("http://{addr}/gotify"))
}

/// 3: потерянный ответ не повод слать второй раз. Попытка и метка охлаждения
/// ставятся атомарно, после неоднозначной доставки повтора нет.
#[tokio::test]
async fn lost_response_is_not_resent() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let (hits, url) = dropping_gotify().await;
    let (_dir, sender) = sender(&url);
    fail_once(&f, id).await;
    for _ in 0..4 {
        sender.deliver_due(&pool).await.unwrap();
        // Повтор «созрел»: пауза между попытками не должна прятать второй POST.
        sqlx::query("update owner_pushes set run_at=clock_timestamp() where state='pending'")
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "ambiguous delivery is not retried"
    );
    assert_eq!(pending(&pool, id).await, 0, "the row is settled");
    let stamp: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("select last_failure_push_at from schedules where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stamp.is_some(), "an ambiguous send keeps the cooldown");
}

/// 3: отказ, не доставивший пуш, возвращает ПРЕЖНЮЮ метку, а не NULL.
#[tokio::test]
async fn definite_rejection_restores_the_previous_stamp() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let status = Arc::new(AtomicU16::new(400));
    let (_seen, url) = gotify(status).await;
    let (_dir, sender) = sender(&url);
    sqlx::query("update schedules set last_failure_push_at=clock_timestamp()-interval '2 hours' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let seeded: chrono::DateTime<Utc> =
        sqlx::query_scalar("select last_failure_push_at from schedules where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    fail_once(&f, id).await;
    assert_eq!(sender.deliver_due(&pool).await.unwrap().dropped, 1);
    let after: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("select last_failure_push_at from schedules where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(after, Some(seeded), "prior stamp is restored, not NULL");
}

/// 4: свежий исход, слитый в давно ждущий пуш, продлевает его срок годности.
#[tokio::test]
async fn coalesced_fresh_failure_is_not_dropped_as_stale() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let (seen, url) = gotify(Arc::new(AtomicU16::new(200))).await;
    let (_dir, sender) = sender(&url);
    fail_once(&f, id).await;
    sqlx::query("update owner_pushes set created_at=created_at-interval '31 minutes' where kind='schedule_failed'")
        .execute(&pool)
        .await
        .unwrap();
    fail_once(&f, id).await;
    let report = sender.deliver_due(&pool).await.unwrap();
    assert_eq!((report.sent, report.dropped), (1, 0), "{report:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// 4: сброс как устаревшего условен по версии строки, которую видел отправитель.
#[tokio::test]
async fn stale_drop_loses_to_a_coalesce_that_lands_after_inspection() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let id = broken_schedule(&f).await;
    let (seen, url) = gotify(Arc::new(AtomicU16::new(200))).await;
    let (_dir, sender) = sender(&url);
    fail_once(&f, id).await;
    sqlx::query("update owner_pushes set created_at=created_at-interval '31 minutes' where kind='schedule_failed'")
        .execute(&pool)
        .await
        .unwrap();
    // Слияние держит строку: отправитель увидит её старой, а сбросить не успеет.
    let mut merge = pool.begin().await.unwrap();
    sqlx::query("select id from owner_pushes where kind='schedule_failed' for update")
        .execute(&mut *merge)
        .await
        .unwrap();
    let delivery = {
        let pool = pool.clone();
        let sender = sender.clone();
        tokio::spawn(async move { sender.deliver_due(&pool).await.unwrap() })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity where wait_event_type='Lock' and datname=current_database()",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the stale drop waits on the row");
    sqlx::query(
        "update owner_pushes set payload=jsonb_set(payload,'{reason}','\"fresh\"'),
           created_at=clock_timestamp(),updated_at=clock_timestamp()
         where kind='schedule_failed'",
    )
    .execute(&mut *merge)
    .await
    .unwrap();
    merge.commit().await.unwrap();
    let report = delivery.await.unwrap();
    assert_eq!(
        report.dropped, 0,
        "a row changed after inspection is not dropped"
    );
    // Следующий проход шлёт свежий пуш.
    assert_eq!(sender.deliver_due(&pool).await.unwrap().sent, 1);
    assert!(seen.lock().unwrap()[0].to_string().contains("fresh"));
}
