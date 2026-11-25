//! Живая диктовка: словарь замен и WebSocket-пересылка службе речи.

use crate::support::task7 as support;

use axum::http::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::Fixture;

fn term(pattern: &str, replacement: &str) -> Value {
    json!({"pattern": pattern, "replacement": replacement})
}

#[tokio::test]
async fn terms_start_empty_and_survive_a_round_trip_in_order() {
    let pool = canterlot_hub::testkit::db().await;
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

// ------------------------------------------------------- служба речи и статус

mod fake {
    use std::{future::Future, path::PathBuf, sync::Arc};

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
        let handler = Arc::new(handler);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                tokio::spawn(async move { handler(FakeConn { stream }).await });
            }
        });
        FakeSpeech { socket, _dir: dir }
    }
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

#[tokio::test]
async fn live_text_and_final_carry_the_dictionary() {
    let pool = canterlot_hub::testkit::db().await;
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
