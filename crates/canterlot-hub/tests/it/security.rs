use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{State, ws::WebSocketUpgrade},
    http::Method,
    response::Response,
    routing::{get, post},
};
use canterlot_hub::{
    config::HttpConfig,
    http::{self, AppState, extract::ApiJson},
    idempotency::{IdempotencyConflict, run_in_tx},
    testkit::{TEST_ORIGIN, TEST_SESSION_COOKIE, TestClient, test_app_deps, test_http_config},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpSocket, TcpStream},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_millis(250);
const WAIT: Duration = Duration::from_secs(3);

struct Server {
    addr: SocketAddr,
    stop: CancellationToken,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn echo(ApiJson(value): ApiJson<Value>) -> Json<Value> {
    Json(value)
}

async fn panic_probe() -> Response {
    panic!("{}", std::env::var("CANTERLOT_PANIC_MARKER").unwrap());
}

async fn websocket(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                () = state.shutdown.cancelled() => break,
                frame = socket.next() => match frame {
                    Some(Ok(axum::extract::ws::Message::Ping(_))) => {},
                    Some(Ok(frame)) => if socket.send(frame).await.is_err() { break; },
                    _ => break,
                },
            }
        }
    })
}

async fn server(pool: PgPool, config: HttpConfig) -> Server {
    let (deps, dir) = test_app_deps(pool, config, None).await;
    let state = AppState::new(deps);
    let routes = Router::new()
        .route("/api/v1/_security/echo", post(echo))
        .route("/api/v1/_security/panic", get(panic_probe))
        .route("/api/v1/_security/ws", get(websocket));
    let router = http::router_with(state.clone(), routes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http::serve_router(
        router,
        listener,
        (*state.http).clone(),
        state.shutdown.clone().cancelled_owned(),
    ));
    Server {
        addr,
        stop: state.shutdown,
        _dir: dir,
    }
}

async fn untrusted(addr: SocketAddr) -> TcpStream {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.2:0".parse().unwrap()).unwrap();
    socket.connect(addr).await.unwrap()
}

async fn closed(socket: &mut TcpStream, within: Duration) -> Vec<u8> {
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(within, socket.read_to_end(&mut bytes))
        .await
        .expect("connection deadline expired without closure");
    if let Err(error) = result {
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
        ));
    }
    bytes
}

#[sqlx::test(migrations = "./migrations")]
async fn security_untrusted_unfinished_headers_expire(pool: PgPool) {
    let mut config = test_http_config();
    config.header_read_timeout = DEADLINE;
    let h = server(pool, config).await;
    for prefix in [
        b"GET /healthz HTTP/1.1\r\nHost: hub\r\nX-Slow: ".as_slice(),
        b"G",
        b"",
    ] {
        let mut socket = untrusted(h.addr).await;
        socket.write_all(prefix).await.unwrap();
        let bytes = closed(&mut socket, WAIT).await;
        assert!(!bytes.starts_with(b"HTTP/1.1 200"));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn security_slow_body_expires_and_capacity_returns(pool: PgPool) {
    let mut config = test_http_config();
    config.body_read_timeout = DEADLINE;
    config.max_connections = 1;
    let h = server(pool, config).await;
    for (framing, prefix) in [
        ("Content-Length: 7", "{"),
        ("Transfer-Encoding: chunked", "1\r\n{\r\n"),
    ] {
        let mut socket = TcpStream::connect(h.addr).await.unwrap();
        socket.write_all(format!(
            "POST /api/v1/_security/echo HTTP/1.1\r\nHost: hub\r\nCookie: {TEST_SESSION_COOKIE}\r\nOrigin: {TEST_ORIGIN}\r\nX-Canterlot-Client: 1\r\nIdempotency-Key: slow\r\nContent-Type: application/json\r\n{framing}\r\n\r\n{prefix}"
        ).as_bytes()).await.unwrap();
        let bytes = closed(&mut socket, WAIT).await;
        assert!(!bytes.starts_with(b"HTTP/1.1 200"));
        assert_eq!(
            TestClient::over_socket(h.addr)
                .await
                .send(Method::GET, "/healthz", None, None)
                .await
                .status,
            200
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn security_connection_cap_rejects_immediately_and_recovers(pool: PgPool) {
    let mut config = test_http_config();
    config.max_connections = 2;
    config.header_read_timeout = Duration::from_secs(1);
    let h = server(pool, config).await;
    let mut first = untrusted(h.addr).await;
    first.write_all(b"G").await.unwrap();
    let mut second = untrusted(h.addr).await;
    second.write_all(b"G").await.unwrap();
    let mut excess = untrusted(h.addr).await;
    assert!(closed(&mut excess, DEADLINE).await.is_empty());
    closed(&mut first, WAIT).await;
    closed(&mut second, WAIT).await;
    assert_eq!(
        TestClient::over_socket(h.addr)
            .await
            .send(Method::GET, "/healthz", None, None)
            .await
            .status,
        200
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn security_keep_alive_idle_connection_expires(pool: PgPool) {
    let mut config = test_http_config();
    config.header_read_timeout = DEADLINE;
    let h = server(pool, config).await;
    let mut socket = untrusted(h.addr).await;
    socket
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: hub\r\n\r\n")
        .await
        .unwrap();
    let bytes = closed(&mut socket, WAIT).await;
    assert!(bytes.starts_with(b"HTTP/1.1 200"));
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrations = "./migrations")]
async fn security_upgraded_connection_outlives_body_deadline(pool: PgPool) {
    let mut config = test_http_config();
    config.header_read_timeout = DEADLINE;
    config.body_read_timeout = DEADLINE;
    config.max_connections = 1;
    let h = server(pool, config).await;
    let mut socket = TcpStream::connect(h.addr).await.unwrap();
    socket.write_all(format!(
        "GET /api/v1/_security/ws HTTP/1.1\r\nHost: hub\r\nCookie: {TEST_SESSION_COOKIE}\r\nOrigin: {TEST_ORIGIN}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    ).as_bytes()).await.unwrap();
    let mut header = Vec::new();
    tokio::time::timeout(WAIT, async {
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
        }
    })
    .await
    .unwrap();
    assert!(header.starts_with(b"HTTP/1.1 101"));
    tokio::time::sleep(DEADLINE * 3).await;
    socket
        .write_all(&[0x89, 0x81, 1, 2, 3, 4, b'x' ^ 1])
        .await
        .unwrap();
    let mut pong = [0; 3];
    tokio::time::timeout(WAIT, socket.read_exact(&mut pong))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pong, [0x8a, 1, b'x']);
    let mut excess = untrusted(h.addr).await;
    assert!(
        closed(&mut excess, DEADLINE).await.is_empty(),
        "upgrade retains its admission slot"
    );
    h.stop.cancel();
    closed(&mut socket, WAIT).await;
}

#[test]
fn security_panic_payload_is_sanitized_in_subprocess() {
    let marker = format!("PRIVATE_PANIC_{}", Uuid::now_v7());
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "security::security_panic_subprocess",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("CANTERLOT_PANIC_MARKER", &marker)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(&marker),
        "payload leaked to response/tracing"
    );
    assert!(!stderr.contains(&marker), "payload leaked to stderr");
    assert!(output.status.success(), "child failed: {stdout}\n{stderr}");
    for expected in ["kind=\"panic\"", "security.rs:", "thread="] {
        assert!(
            stderr.contains(expected),
            "missing panic metadata: {expected}: {stderr}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "subprocess fixture"]
async fn security_panic_subprocess(pool: PgPool) {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    http::layers::install_panic_hook();
    let h = server(pool, test_http_config()).await;
    let response = TestClient::over_socket(h.addr)
        .await
        .as_owner()
        .send(Method::GET, "/api/v1/_security/panic", None, None)
        .await;
    assert_eq!(response.status, 500);
    let body = String::from_utf8_lossy(&response.body);
    assert!(!body.contains(&std::env::var("CANTERLOT_PANIC_MARKER").unwrap()));
    println!("{body}");
}

#[derive(Debug)]
enum OpError {
    Db(#[allow(dead_code)] sqlx::Error),
    Conflict,
    State,
}
impl From<sqlx::Error> for OpError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}
impl From<IdempotencyConflict> for OpError {
    fn from(_: IdempotencyConflict) -> Self {
        Self::Conflict
    }
}
impl canterlot_hub::db::TransientError for OpError {
    fn is_transient(&self) -> bool {
        match self {
            Self::Db(error) => canterlot_hub::db::is_transient(error),
            _ => false,
        }
    }
}

async fn race(pool: PgPool, create: bool) {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("create table security_effect (id int primary key, credit int not null, effect uuid not null default gen_random_uuid())").execute(&pool).await.unwrap();
    let deadlocks: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname = current_database()",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut blocker = pool.begin().await.unwrap();
    if create {
        sqlx::query("insert into security_effect (id, credit) values (1, 1)")
            .execute(&mut *blocker)
            .await
            .unwrap();
    } else {
        sqlx::query("insert into security_effect (id, credit) values (1, 1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("select * from security_effect for update")
            .execute(&mut *blocker)
            .await
            .unwrap();
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let call = || {
        let pool = pool.clone();
        let barrier = barrier.clone();
        let calls = calls.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            run_in_tx::<Value, OpError, _>(&pool, owner, "race", "security_effect", &json!({"id":1}), None, move |conn| Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if create {
                    Ok(sqlx::query_scalar("insert into security_effect (id, credit) values (1, 0) returning jsonb_build_object('effect', effect)").fetch_one(conn).await?)
                } else {
                    sqlx::query_scalar("update security_effect set credit = credit - 1 where id = 1 and credit > 0 returning jsonb_build_object('credit', credit)").fetch_optional(conn).await?.ok_or(OpError::State)
                }
            })).await
        })
    };
    let (a, b) = (call(), call());
    let waiting = tokio::time::timeout(WAIT, async {
        loop {
            let n: i64 = sqlx::query_scalar("select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'").fetch_one(&pool).await.unwrap();
            if n >= 2 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    blocker.rollback().await.unwrap();
    waiting.expect("both requests must contend before releasing the row");
    let (a, b) = tokio::time::timeout(WAIT, async { (a.await.unwrap(), b.await.unwrap()) })
        .await
        .unwrap();
    let a = a.expect("first request succeeds or replays");
    let b = b.expect("competing retry succeeds or replays");
    assert_eq!(a.value, b.value);
    assert_eq!(usize::from(a.replayed) + usize::from(b.replayed), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let effect: (i64, i64) =
        sqlx::query_as("select count(*), sum(credit)::bigint from security_effect")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(effect, (1, 0));
    let keys: i64 = sqlx::query_scalar(
        "select count(*) from idempotency_keys where owner_id = $1 and key = 'race'",
    )
    .bind(owner)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(keys, 1);
    let after: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname = current_database()",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, deadlocks);
}

#[sqlx::test(migrations = "./migrations")]
async fn security_same_row_transition_race_replays_success(pool: PgPool) {
    race(pool, false).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn security_unique_create_race_replays_success(pool: PgPool) {
    race(pool, true).await;
}
