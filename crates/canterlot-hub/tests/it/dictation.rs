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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_term_replacements_serialize_per_owner(pool: PgPool) {
    let f = Fixture::new(pool).await;
    // Оба DELETE обязаны пройти до вставок: иначе блокировки строк сами
    // выстроят транзакции и гонка не проявится.
    sqlx::query(
        "create function dictation_terms_slow_delete() returns trigger language plpgsql
         as $$ begin perform pg_sleep(0.1); return null; end $$",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger dictation_terms_slow_delete after delete on dictation_terms
         for each statement execute function dictation_terms_slow_delete()",
    )
    .execute(&f.pool)
    .await
    .unwrap();
    let list = |prefix: &str| {
        let terms: Vec<Value> = (0..5)
            .map(|i| term(&format!("{prefix}{i}"), &format!("x{i}")))
            .collect();
        json!({"terms": terms})
    };
    let a = list("альфа");
    let b = list("бета");
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
    let (ra, rb) = tokio::join!(
        put("dictation-concurrent-a", a.clone()),
        put("dictation-concurrent-b", b.clone())
    );
    assert_eq!(ra.status, 200, "first concurrent PUT");
    assert_eq!(rb.status, 200, "second concurrent PUT");
    let ra: Value = ra.json();
    let rb: Value = rb.json();
    assert_eq!(ra, a);
    assert_eq!(rb, b);
    let stored: Value = f.get("/api/v1/dictation/terms").await;
    assert!(
        stored == a || stored == b,
        "stored list is one of the two requests: {stored}"
    );
    let rows: i64 = sqlx::query_scalar("select count(*) from dictation_terms where owner_id=$1")
        .bind(f.seed.owner_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 5);
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
    }

    pub struct FakeSpeech {
        pub socket: PathBuf,
        pub connections: Arc<AtomicUsize>,
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
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let handler = handler.clone();
                tokio::spawn(async move { handler(FakeConn { stream }).await });
            }
        });
        FakeSpeech {
            socket,
            connections,
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

// ------------------------------------------------------------- WebSocket диктовки

mod ws {
    use std::net::SocketAddr;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpStream;
    use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

    pub struct DictationWs {
        pub stream: WebSocketStream<TcpStream>,
        pub close_code: Option<u16>,
    }

    impl DictationWs {
        pub async fn connect(
            addr: SocketAddr,
            origin: &str,
            session: Option<&str>,
        ) -> Result<Self, tokio_tungstenite::tungstenite::Error> {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest;
            let mut request = format!("ws://{addr}/api/v1/dictation").into_client_request()?;
            request.headers_mut().insert("origin", origin.parse()?);
            if let Some(token) = session {
                request.headers_mut().insert(
                    "cookie",
                    format!("{}={token}", canterlot_hub::owner_auth::SESSION_COOKIE).parse()?,
                );
            }
            let socket = TcpStream::connect(addr).await?;
            let (stream, _) = tokio_tungstenite::client_async(request, socket).await?;
            Ok(Self {
                stream,
                close_code: None,
            })
        }

        pub async fn start(&mut self) {
            self.send(json!({
                "type": "start",
                "sample_rate": 16000,
                "format": "s16le",
                "client": "pwa/test"
            }))
            .await;
        }

        pub async fn send(&mut self, value: Value) {
            assert!(
                self.stream
                    .send(Message::Text(value.to_string().into()))
                    .await
                    .is_ok(),
                "ws send"
            );
        }

        /// Текст как есть, без JSON-обёртки.
        pub async fn send_text_raw(&mut self, text: &str) {
            let _ = self
                .stream
                .send(Message::Text(text.to_owned().into()))
                .await;
        }

        /// Звук без паники: в тесте зависшей записи отправка может оборваться.
        pub async fn try_audio(
            &mut self,
            bytes: &[u8],
        ) -> Result<(), tokio_tungstenite::tungstenite::Error> {
            self.stream
                .send(Message::Binary(bytes.to_vec().into()))
                .await
        }

        pub async fn audio(&mut self, bytes: &[u8]) {
            assert!(
                self.stream
                    .send(Message::Binary(bytes.to_vec().into()))
                    .await
                    .is_ok(),
                "ws audio"
            );
        }

        /// Следующее текстовое сообщение; `None` — закрытие (код в `close_code`).
        pub async fn next(&mut self, timeout: std::time::Duration) -> Option<Value> {
            tokio::time::timeout(timeout, async {
                loop {
                    match self.stream.next().await {
                        Some(Ok(Message::Text(text))) => {
                            return serde_json::from_str(&text).ok();
                        }
                        Some(Ok(Message::Close(frame))) => {
                            self.close_code = frame.map(|f| f.code.into());
                            return None;
                        }
                        Some(Ok(_)) => {}
                        _ => return None,
                    }
                }
            })
            .await
            .ok()
            .flatten()
        }

        pub async fn expect(&mut self, timeout: std::time::Duration) -> Value {
            self.next(timeout)
                .await
                .expect("a text frame before the deadline")
        }
    }
}

use std::{path::PathBuf, time::Duration};

use canterlot_hub::{
    config::DictationConfig,
    dictation::Timers,
    owner_auth::{RevokeReason, Secret, digest, sessions},
    testkit::{self, TestHub, TestHubOptions},
};

const TIMEOUT: Duration = Duration::from_secs(5);

fn fast_timers() -> Timers {
    Timers {
        first_message: Duration::from_millis(300),
        speech_connect: Duration::from_millis(300),
        idle: Duration::from_millis(600),
        start_deadline: Duration::from_millis(1_200),
        lifetime: Duration::from_millis(2_500),
        finish_final: Duration::from_millis(800),
        write_stall: Duration::from_millis(500),
        status_cache: Duration::from_secs(10),
        max_lead: Duration::from_secs(5),
    }
}

async fn spawn_hub(pool: PgPool, socket: PathBuf, timers: Timers) -> TestHub {
    testkit::spawn_test_hub_with(
        pool,
        TestHubOptions {
            http: Some(testkit::test_http_config()),
            dictation: Some(DictationConfig {
                enabled: true,
                socket,
            }),
            dictation_timers: Some(timers),
            ..Default::default()
        },
    )
    .await
}

async fn open(hub: &TestHub) -> ws::DictationWs {
    ws::DictationWs::connect(
        hub.http_addr(),
        testkit::TEST_ORIGIN,
        Some(testkit::TEST_SESSION_TOKEN),
    )
    .await
    .expect("dictation websocket")
}

/// Второй сеанс того же владельца: другое устройство.
async fn second_session(hub: &TestHub) -> (uuid::Uuid, String) {
    // Токен сеанса — base64url ровно 43 символа.
    let token = "s".repeat(43);
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "insert into owner_sessions(id,owner_id,token_sha256,method,authenticated_at)
         values($1,$2,$3,'password',now())",
    )
    .bind(id)
    .bind(hub.installation.owner_id)
    .bind(digest(Secret::Session, &token))
    .execute(&hub.app().pool)
    .await
    .expect("second session");
    (id, token)
}

async fn error_code(mut socket: ws::DictationWs) -> (String, Option<u16>) {
    let frame = socket.expect(TIMEOUT).await;
    assert_eq!(frame["type"], "error");
    let code = frame["code"].as_str().unwrap().to_owned();
    assert_eq!(socket.next(TIMEOUT).await, None);
    (code, socket.close_code)
}

async fn seed_term(
    pool: &PgPool,
    owner_id: uuid::Uuid,
    position: i32,
    pattern: &str,
    replacement: &str,
) {
    sqlx::query(
        "insert into dictation_terms(owner_id,position,pattern,replacement) values($1,$2,$3,$4)",
    )
    .bind(owner_id)
    .bind(position)
    .bind(pattern)
    .bind(replacement)
    .execute(pool)
    .await
    .expect("seed term");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn websocket_needs_a_live_session_and_an_allowed_origin(pool: PgPool) {
    let speech =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;
    assert!(
        ws::DictationWs::connect(hub.http_addr(), testkit::TEST_ORIGIN, None)
            .await
            .is_err(),
        "no session is rejected"
    );
    assert!(
        ws::DictationWs::connect(
            hub.http_addr(),
            "https://evil.example",
            Some(testkit::TEST_SESSION_TOKEN)
        )
        .await
        .is_err(),
        "origin is checked"
    );
    let (id, token) = second_session(&hub).await;
    let mut revoked = ws::DictationWs::connect(hub.http_addr(), testkit::TEST_ORIGIN, Some(&token))
        .await
        .expect("upgrade before revocation");
    revoked.start().await;
    sessions::revoke(&hub.app().pool, id, RevokeReason::Owner, None)
        .await
        .unwrap();
    assert_eq!(revoked.next(TIMEOUT).await, None);
    assert_eq!(revoked.close_code, Some(4401));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn revocation_between_upgrade_and_start_never_reaches_speech(pool: PgPool) {
    let speech =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    let (id, token) = second_session(&hub).await;
    let mut socket = ws::DictationWs::connect(hub.http_addr(), testkit::TEST_ORIGIN, Some(&token))
        .await
        .expect("upgrade before revocation");
    sessions::revoke(&hub.app().pool, id, RevokeReason::Owner, None)
        .await
        .unwrap();
    socket.start().await;
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(4401));
    assert_eq!(
        speech.connections.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "speech never sees start"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn protocol_violations_answer_bad_request_or_bad_audio(pool: PgPool) {
    let speech =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;

    // Первое сообщение не пришло за 300 мс.
    let silent = open(&hub).await;
    assert_eq!(
        error_code(silent).await,
        ("bad_request".to_owned(), Some(1000))
    );

    // Звук вместо start.
    let mut early = open(&hub).await;
    early.audio(&[0; 320]).await;
    assert_eq!(error_code(early).await.0, "bad_request");

    // Повторный start.
    let mut twice = open(&hub).await;
    twice.start().await;
    twice.start().await;
    assert_eq!(error_code(twice).await.0, "bad_request");

    // Неизвестный тип и не JSON.
    let mut unknown = open(&hub).await;
    unknown.send(json!({"type": "wat"})).await;
    assert_eq!(error_code(unknown).await.0, "bad_request");
    let mut garbage = open(&hub).await;
    garbage.send_text_raw("не json").await;
    assert_eq!(error_code(garbage).await.0, "bad_request");

    // Неверный формат звука в start.
    let mut rate = open(&hub).await;
    rate.send(json!({"type":"start","sample_rate":8000,"format":"s16le","client":"pwa/test"}))
        .await;
    assert_eq!(error_code(rate).await.0, "bad_audio");
    let mut format = open(&hub).await;
    format
        .send(json!({"type":"start","sample_rate":16000,"format":"f32le","client":"pwa/test"}))
        .await;
    assert_eq!(error_code(format).await.0, "bad_audio");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn bad_audio_for_empty_odd_large_and_ahead_of_time_frames(pool: PgPool) {
    let speech =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;

    for bytes in [vec![], vec![0u8; 321], vec![0u8; 8200]] {
        let mut socket = open(&hub).await;
        socket.start().await;
        socket.audio(&bytes).await;
        assert_eq!(
            error_code(socket).await.0,
            "bad_audio",
            "{} bytes",
            bytes.len()
        );
    }

    // 30 кадров по 8192 байта — 7,68 с звука за доли секунды: дальше пяти секунд
    // опережения.
    let mut ahead = open(&hub).await;
    ahead.start().await;
    for _ in 0..30 {
        ahead.audio(&vec![0u8; 8192]).await;
    }
    assert_eq!(error_code(ahead).await.0, "bad_audio");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn live_text_and_final_carry_the_dictionary(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let Some(start) = conn.next().await else {
            return;
        };
        assert_eq!(start["type"], "start");
        assert_eq!(start["protocol"], 1);
        assert_eq!(start["sample_rate"], 16000);
        assert_eq!(start["format"], "s16le");
        assert!(start["client"].as_str().unwrap().starts_with("hub/"));
        conn.send(json!({"type":"state","state":"loading","mode":null}))
            .await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        loop {
            match conn.next_frame().await {
                Some(canterlot_hub::dictation::frame::Frame::Audio(bytes)) => {
                    assert!(bytes.len() <= 8192);
                }
                Some(canterlot_hub::dictation::frame::Frame::Json(bytes)) => {
                    let value: Value = serde_json::from_slice(&bytes).unwrap();
                    if value["type"] == "finish" {
                        break;
                    }
                }
                None => return,
            }
        }
        conn.send(json!({"type":"text","committed":"нужен сессион","tentative":"спаун","seq":1}))
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.send(
            json!({"type":"text","committed":"нужен сессион спаун.","tentative":"Ещё","seq":2}),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.send(json!({"type":"final","text":"нужен сессион спаун.","reason":"finish"}))
            .await;
    })
    .await;
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;
    let owner = hub.installation.owner_id;
    seed_term(&hub.app().pool, owner, 0, "сессион спаун", "session_spawn").await;

    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(
        socket.expect(TIMEOUT).await,
        json!({"type": "state", "state": "loading", "mode": null})
    );
    assert_eq!(
        socket.expect(TIMEOUT).await,
        json!({"type": "state", "state": "listening", "mode": "gpu"})
    );
    socket.audio(&vec![0u8; 3200]).await;
    socket.send(json!({"type": "finish"})).await;

    let mut texts = Vec::new();
    let final_frame = loop {
        let frame = socket.expect(TIMEOUT).await;
        match frame["type"].as_str() {
            Some("text") => {
                assert!(
                    !frame["committed"]
                        .as_str()
                        .unwrap()
                        .contains("сессион спаун"),
                    "сырой шаблон не должен доходить до PWA"
                );
                texts.push(frame);
            }
            Some("final") => break frame,
            other => panic!("неожиданный кадр {other:?}: {frame}"),
        }
    };
    assert!(!texts.is_empty());
    let last = texts.last().unwrap();
    assert_eq!(last["committed"], "нужен session_spawn.");
    assert_eq!(last["tentative"], "Ещё");
    for pair in texts.windows(2) {
        assert!(
            pair[1]["seq"].as_u64().unwrap() > pair[0]["seq"].as_u64().unwrap(),
            "seq растёт"
        );
    }
    assert_eq!(final_frame["text"], "нужен session_spawn.");
    assert_eq!(final_frame["reason"], "finish");
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn too_long_final_is_passed_through(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"cpu"}))
            .await;
        conn.send(json!({"type":"final","text":"длинно","reason":"too_long"}))
            .await;
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["mode"], "cpu");
    assert_eq!(
        socket.expect(TIMEOUT).await,
        json!({"type": "final", "text": "длинно", "reason": "too_long"})
    );
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_expanded_past_the_old_write_buffer_reaches_the_client(pool: PgPool) {
    let raw = vec!["a"; 3_000].join(" ");
    let speech = fake::start(move |mut conn| {
        let raw = raw.clone();
        async move {
            let _ = conn.next().await;
            conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
                .await;
            conn.send(json!({"type":"final","text":raw,"reason":"finish"}))
                .await;
        }
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    seed_term(
        &hub.app().pool,
        hub.installation.owner_id,
        0,
        "a",
        &"b".repeat(100),
    )
    .await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    let frame = socket.expect(TIMEOUT).await;
    assert_eq!(frame["type"], "final");
    let text = frame["text"].as_str().unwrap();
    assert_eq!(text.len(), 3_000 * 100 + 2_999);
    assert!(text.starts_with(&"b".repeat(100)));
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_expanded_past_the_outbound_limit_is_engine_failed(pool: PgPool) {
    let raw = vec!["a"; 11_000].join(" ");
    let speech = fake::start(move |mut conn| {
        let raw = raw.clone();
        async move {
            let _ = conn.next().await;
            conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
                .await;
            conn.send(json!({"type":"final","text":raw,"reason":"finish"}))
                .await;
        }
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    seed_term(
        &hub.app().pool,
        hub.installation.owner_id,
        0,
        "a",
        &"b".repeat(100),
    )
    .await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    assert_eq!(
        error_code(socket).await,
        ("engine_failed".to_owned(), Some(1000))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_device_replaces_and_another_device_gets_busy(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;

    let mut first = open(&hub).await;
    first.start().await;
    assert_eq!(first.expect(TIMEOUT).await["state"], "listening");

    let mut second = open(&hub).await;
    second.start().await;
    // Старая диктовка того же сеанса уступает новой.
    assert_eq!(error_code(first).await, ("replaced".to_owned(), Some(1000)));
    assert_eq!(second.expect(TIMEOUT).await["state"], "listening");

    // Другое устройство получает busy, пока живёт вторая.
    let (_, other_token) = second_session(&hub).await;
    let mut other =
        ws::DictationWs::connect(hub.http_addr(), testkit::TEST_ORIGIN, Some(&other_token))
            .await
            .expect("other device");
    other.start().await;
    assert_eq!(error_code(other).await, ("busy".to_owned(), Some(1000)));

    // После конца второй слот свободен.
    second.send(json!({"type": "cancel"})).await;
    assert_eq!(second.next(TIMEOUT).await, None);
    assert_eq!(second.close_code, Some(1000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn slot_release_is_generation_safe(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;

    let mut old = open(&hub).await;
    old.start().await;
    assert_eq!(old.expect(TIMEOUT).await["state"], "listening");
    let mut new = open(&hub).await;
    new.start().await;
    assert_eq!(error_code(old).await.0, "replaced");
    assert_eq!(new.expect(TIMEOUT).await["state"], "listening");

    // Уборка старой диктовки не освободила слот новой.
    let (_, other_token) = second_session(&hub).await;
    let mut other =
        ws::DictationWs::connect(hub.http_addr(), testkit::TEST_ORIGIN, Some(&other_token))
            .await
            .expect("other device");
    other.start().await;
    assert_eq!(error_code(other).await.0, "busy");
    new.send(json!({"type": "cancel"})).await;
    assert_eq!(new.next(TIMEOUT).await, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn missing_speech_socket_answers_speech_unavailable(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let hub = spawn_hub(pool, fake::missing_socket(dir.path()), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(
        error_code(socket).await,
        ("speech_unavailable".to_owned(), Some(1000))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn speech_errors_map_to_client_codes(pool: PgPool) {
    for (speech_code, expected) in [
        ("engine_unavailable", "speech_unavailable"),
        ("shutting_down", "speech_unavailable"),
        ("busy", "busy"),
        ("engine_failed", "engine_failed"),
        ("bad_request", "engine_failed"),
        ("idle", "idle"),
        ("lifetime", "lifetime"),
        ("start_timeout", "start_timeout"),
        ("bad_audio", "bad_audio"),
    ] {
        let speech = fake::start(move |mut conn| async move {
            let _ = conn.next().await;
            conn.send(json!({"type":"state","state":"loading","mode":null}))
                .await;
            conn.send(json!({"type":"error","code":speech_code})).await;
        })
        .await;
        let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;
        let mut socket = open(&hub).await;
        socket.start().await;
        assert_eq!(socket.expect(TIMEOUT).await["state"], "loading");
        assert_eq!(
            error_code(socket).await,
            (expected.to_owned(), Some(1000)),
            "speech {speech_code}"
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn speech_closing_mid_dictation_is_engine_failed(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"loading","mode":null}))
            .await;
        // Соединение закрывается без итога.
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "loading");
    assert_eq!(error_code(socket).await.0, "engine_failed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancel_closes_without_any_message(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    socket.send(json!({"type": "cancel"})).await;
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1000));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_before_finish_ends_the_dictation(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let timers = Timers {
        idle: Duration::from_millis(300),
        start_deadline: Duration::from_secs(60),
        lifetime: Duration::from_secs(60),
        ..fast_timers()
    };
    let hub = spawn_hub(pool, speech.socket.clone(), timers).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    assert_eq!(error_code(socket).await.0, "idle");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_timeout_and_lifetime_are_enforced(pool: PgPool) {
    let silent =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let timers = Timers {
        start_deadline: Duration::from_millis(300),
        idle: Duration::from_secs(60),
        lifetime: Duration::from_secs(60),
        ..fast_timers()
    };
    let hub = spawn_hub(pool.clone(), silent.socket.clone(), timers).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(error_code(socket).await.0, "start_timeout");

    let listening = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"cpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let timers = Timers {
        lifetime: Duration::from_millis(300),
        idle: Duration::from_secs(60),
        start_deadline: Duration::from_secs(60),
        ..fast_timers()
    };
    let hub = spawn_hub(pool, listening.socket.clone(), timers).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    assert_eq!(error_code(socket).await.0, "lifetime");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_final_after_finish_is_engine_failed(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while let Some(frame) = conn.next_frame().await {
            if let canterlot_hub::dictation::frame::Frame::Json(bytes) = frame {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                if value["type"] == "finish" {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
            }
        }
    })
    .await;
    let timers = Timers {
        finish_final: Duration::from_millis(300),
        idle: Duration::from_secs(60),
        ..fast_timers()
    };
    let hub = spawn_hub(pool, speech.socket.clone(), timers).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    socket.send(json!({"type": "finish"})).await;
    assert_eq!(error_code(socket).await.0, "engine_failed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stalled_speech_write_is_engine_failed(pool: PgPool) {
    // Служба принимает соединение и ничего не читает: буферы сокета заполняются.
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;
    let timers = Timers {
        write_stall: Duration::from_millis(300),
        idle: Duration::from_secs(60),
        start_deadline: Duration::from_secs(60),
        lifetime: Duration::from_secs(60),
        max_lead: Duration::from_secs(120),
        ..fast_timers()
    };
    let hub = spawn_hub(pool, speech.socket.clone(), timers).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "listening");
    let frame = vec![0u8; 8192];
    for _ in 0..200 {
        let sent = tokio::time::timeout(Duration::from_millis(50), socket.try_audio(&frame)).await;
        if sent.is_err() {
            break;
        }
    }
    let mut code = None;
    while let Some(value) = socket.next(TIMEOUT).await {
        if value["type"] == "error" {
            code = value["code"].as_str().map(str::to_owned);
            break;
        }
    }
    assert_eq!(code.as_deref(), Some("engine_failed"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_shutdown_during_dictionary_snapshot_drains_without_speech(pool: PgPool) {
    let speech =
        fake::start(|mut conn| async move { while conn.next_frame().await.is_some() {} }).await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("lock table dictation_terms in access exclusive mode")
        .execute(&mut *lock)
        .await
        .unwrap();
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    hub.shutdown.send(()).unwrap();
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1001));
    let joined = tokio::time::timeout(TIMEOUT, hub.join)
        .await
        .expect("hub drains the blocked preparation")
        .expect("hub join");
    assert!(joined.is_ok(), "hub task failed");
    assert_eq!(
        speech.connections.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    lock.rollback().await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replacement_during_dictionary_snapshot_closes_the_old_socket(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"listening","mode":"gpu"}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("lock table dictation_terms in access exclusive mode")
        .execute(&mut *lock)
        .await
        .unwrap();
    let hub = spawn_hub(pool.clone(), speech.socket.clone(), fast_timers()).await;
    let mut first = open(&hub).await;
    first.start().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut second = open(&hub).await;
    second.start().await;
    assert_eq!(error_code(first).await, ("replaced".to_owned(), Some(1000)));
    assert_eq!(
        speech.connections.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the replaced preparation never opens speech"
    );
    lock.rollback().await.unwrap();
    assert_eq!(second.expect(TIMEOUT).await["state"], "listening");
    assert_eq!(
        speech.connections.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_shutdown_closes_with_1001(pool: PgPool) {
    let speech = fake::start(|mut conn| async move {
        let _ = conn.next().await;
        conn.send(json!({"type":"state","state":"loading","mode":null}))
            .await;
        while conn.next_frame().await.is_some() {}
    })
    .await;
    let hub = spawn_hub(pool, speech.socket.clone(), fast_timers()).await;
    let mut socket = open(&hub).await;
    socket.start().await;
    assert_eq!(socket.expect(TIMEOUT).await["state"], "loading");
    hub.shutdown.send(()).unwrap();
    assert_eq!(socket.next(TIMEOUT).await, None);
    assert_eq!(socket.close_code, Some(1001));
}
