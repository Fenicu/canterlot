//! Живая диктовка: словарь замен и WebSocket-пересылка службе речи.

use crate::support::task7 as support;

use axum::http::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::Fixture;

fn term(pattern: &str, replacement: &str) -> Value {
    json!({"pattern": pattern, "replacement": replacement})
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_start_empty_and_survive_a_round_trip_in_order(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, json!({"terms": []}));

    let terms = json!({"terms": [term("  мейн ", " main "), term("ветка", "branch")]});
    let response = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-terms-1"),
            Some(terms.clone()),
        )
        .await;
    assert_eq!(response.status, 200);
    let stored: Value = response.json();
    assert_eq!(
        stored,
        json!({"terms": [term("мейн", "main"), term("ветка", "branch")]})
    );
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, stored);

    // Повтор с тем же ключом отдаёт сохранённое и ничего не добавляет.
    let again = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-terms-1"),
            Some(terms),
        )
        .await;
    assert_eq!(again.status, 200);
    assert_eq!(again.json::<Value>(), stored);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_put_replaces_the_whole_list_and_is_idempotent(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let put = |key: &'static str, body: Value| {
        let client = f.client.clone();
        async move {
            client
                .send(
                    Method::PUT,
                    "/api/v1/dictation/terms",
                    Some(key),
                    Some(body),
                )
                .await
        }
    };
    assert_eq!(
        put(
            "dictation-a",
            json!({"terms": [term("раз", "one"), term("два", "two"), term("три", "three")]})
        )
        .await
        .status,
        200
    );
    let replaced = put("dictation-b", json!({"terms": [term("три", "three")]})).await;
    assert_eq!(replaced.status, 200);
    assert_eq!(
        replaced.json::<Value>(),
        json!({"terms": [term("три", "three")]})
    );
    // Замена целиком, а не слияние: старых строк не осталось.
    let rows: i64 = sqlx::query_scalar("select count(*) from dictation_terms where owner_id=$1")
        .bind(f.seed.owner_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    // Пустой список допустим и стирает всё.
    let cleared = put("dictation-c", json!({"terms": []})).await;
    assert_eq!(cleared.status, 200);
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, json!({"terms": []}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_validation_names_the_code_and_keeps_the_stored_list(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let good = json!({"terms": [term("мейн", "main")]});
    assert_eq!(
        f.client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some("dictation-good"),
                Some(good.clone()),
            )
            .await
            .status,
        200
    );

    let too_many: Vec<Value> = (0..201).map(|i| term(&format!("слово{i}"), "x")).collect();
    let long: String = "я".repeat(101);
    let cases: Vec<(Value, &str)> = vec![
        (json!({"terms": too_many}), "too_many_terms"),
        (json!({"terms": [term("   ", "main")]}), "term_empty"),
        (json!({"terms": [term("мейн", "")]}), "term_empty"),
        (json!({"terms": [term(&long, "x")]}), "term_too_long"),
        (json!({"terms": [term("мейн", &long)]}), "term_too_long"),
        (
            json!({"terms": [term("Мейн", "a"), term("мейн", "b")]}),
            "duplicate_term",
        ),
    ];
    for (index, (body, code)) in cases.into_iter().enumerate() {
        let response = f
            .client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some(&format!("dictation-bad-{index}")),
                Some(body),
            )
            .await;
        assert_eq!(response.status, 422, "{code}");
        let error: Value = response.json();
        assert_eq!(error["error"]["code"], code);
    }
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, good);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_put_requires_an_idempotency_key_and_rejects_its_reuse(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let body = json!({"terms": [term("мейн", "main")]});
    let without = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(without.status, 400);

    assert_eq!(
        f.client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some("dictation-same"),
                Some(body.clone())
            )
            .await
            .status,
        200
    );
    let conflict = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-same"),
            Some(json!({"terms": [term("ветка", "branch")]})),
        )
        .await;
    assert_eq!(conflict.status, 409);
    let error: Value = conflict.json();
    assert_eq!(error["error"]["code"], "idempotency_conflict");
}

// ------------------------------------------------------- служба речи и статус

mod fake {
    use std::{
        future::Future,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use canterlot_hub::dictation::frame::{self, Frame, ReadLimits};
    use serde_json::Value;
    use tokio::net::{UnixListener, UnixStream};

    pub struct FakeConn {
        stream: UnixStream,
    }

    impl FakeConn {
        pub async fn next_frame(&mut self) -> Option<Frame> {
            frame::read_frame(&mut self.stream, ReadLimits::CLIENT)
                .await
                .ok()
        }

        pub async fn next(&mut self) -> Option<Value> {
            match self.next_frame().await? {
                Frame::Json(bytes) => serde_json::from_slice(&bytes).ok(),
                Frame::Audio(_) => None,
            }
        }

        pub async fn send(&mut self, value: Value) {
            frame::write_json(&mut self.stream, &value)
                .await
                .expect("fake speech write");
        }

        pub async fn send_frame(&mut self, kind: u8, payload: &[u8]) {
            frame::write_frame(&mut self.stream, kind, payload)
                .await
                .expect("fake speech frame");
        }
    }

    pub struct FakeSpeech {
        pub socket: PathBuf,
        pub connections: Arc<AtomicUsize>,
        pub accepted: tokio::sync::mpsc::UnboundedReceiver<()>,
        _dir: tempfile::TempDir,
    }

    pub async fn start<F, Fut>(handler: F) -> FakeSpeech
    where
        F: Fn(FakeConn) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        start_in(tempfile::tempdir().expect("speech dir"), handler).await
    }

    pub async fn start_in<F, Fut>(dir: tempfile::TempDir, handler: F) -> FakeSpeech
    where
        F: Fn(FakeConn) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let socket = dir.path().join("speech.sock");
        let listener = UnixListener::bind(&socket).expect("fake speech socket");
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        let (tx, accepted) = tokio::sync::mpsc::unbounded_channel();
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(());
                let handler = handler.clone();
                tokio::spawn(async move { handler(FakeConn { stream }).await });
            }
        });
        FakeSpeech {
            socket,
            connections,
            accepted,
            _dir: dir,
        }
    }

    /// Каталог без сокета: служба не запущена.
    pub fn missing_socket(dir: &Path) -> PathBuf {
        dir.join("absent.sock")
    }
}

async fn hub_with_dictation(
    pool: PgPool,
    socket: std::path::PathBuf,
    enabled: bool,
) -> (
    canterlot_hub::testkit::TestHub,
    canterlot_hub::testkit::TestClient,
) {
    use canterlot_hub::{config::DictationConfig, testkit};
    let hub = testkit::spawn_test_hub_with(
        pool,
        testkit::TestHubOptions {
            http: Some(testkit::test_http_config()),
            dictation: Some(DictationConfig { enabled, socket }),
            ..Default::default()
        },
    )
    .await;
    let client = testkit::TestClient::over_socket(hub.http_addr())
        .await
        .as_owner();
    (hub, client)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn status_reports_disabled_without_touching_the_socket(pool: PgPool) {
    let (_hub, client) = hub_with_dictation(
        pool,
        fake::missing_socket(std::path::Path::new("/nonexistent")),
        false,
    )
    .await;
    let response = client
        .send(Method::GET, "/api/v1/dictation/status", None, None)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json::<Value>(),
        json!({"available": false, "reason": "disabled", "mode": null, "engine": null})
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn status_reports_no_service_when_the_socket_is_missing(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let (_hub, client) = hub_with_dictation(pool, fake::missing_socket(dir.path()), true).await;
    let response = client
        .send(Method::GET, "/api/v1/dictation/status", None, None)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json::<Value>(),
        json!({"available": false, "reason": "no_service", "mode": null, "engine": null})
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn status_copies_health_and_caches_it_for_ten_seconds(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let Some(request) = conn.next().await else {
            return;
        };
        assert_eq!(request["type"], "health");
        conn.send(json!({
            "type": "health",
            "ok": true,
            "engine": "warm",
            "mode": "gpu",
            "model": "parakeet",
            "busy": false
        }))
        .await;
    })
    .await;
    let (_hub, client) = hub_with_dictation(pool, speech.socket.clone(), true).await;
    let expected = json!({"available": true, "reason": null, "mode": "gpu", "engine": "warm"});
    for _ in 0..3 {
        let response = client
            .send(Method::GET, "/api/v1/dictation/status", None, None)
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.json::<Value>(), expected);
    }
    // Один запрос health на все вызовы внутри десятисекундного окна.
    assert_eq!(
        speech.connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn status_reports_unhealthy_and_failed_loads(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type": "health", "ok": false, "engine": "cold", "mode": null, "model": "m", "busy": false}))
            .await;
    })
    .await;
    let (_hub, client) = hub_with_dictation(pool.clone(), speech.socket.clone(), true).await;
    let response = client
        .send(Method::GET, "/api/v1/dictation/status", None, None)
        .await;
    assert_eq!(
        response.json::<Value>(),
        json!({"available": false, "reason": "unhealthy", "mode": null, "engine": "cold"})
    );

    // Служба отвечает мусором — это тоже недоступность, а не unhealthy.
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type": "state", "state": "listening", "mode": "gpu"}))
            .await;
    })
    .await;
    let (_hub, client) = hub_with_dictation(pool, speech.socket.clone(), true).await;
    let response = client
        .send(Method::GET, "/api/v1/dictation/status", None, None)
        .await;
    assert_eq!(
        response.json::<Value>(),
        json!({"available": false, "reason": "no_service", "mode": null, "engine": null})
    );
}
