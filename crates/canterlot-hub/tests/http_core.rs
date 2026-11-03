use std::{
    collections::BTreeSet,
    io::Write,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Extension, Json, Router,
    extract::{State, ws::WebSocketUpgrade},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use canterlot_hub::{
    background::{Background, BackgroundDeps},
    config::{ActivityConfig, HttpConfig, HubTimers, LimitsConfig, MachinesConfig},
    dberr,
    http::{
        self, AppState, UPLOAD_CHUNK_BYTES,
        auth::{Correlation, IdempotencyKey, Owner, peer_is_trusted},
        error::{ApiError, ErrorCode, ErrorScope, Retry},
        extract::{ApiJson, ApiPath, ApiQuery},
        openapi,
    },
    idempotency::{IdempotencyConflict, run_in_tx},
    testkit::{
        TEST_ORIGIN, TEST_OWNER_LOGIN, TestClient, TestResponse, spawn_test_hub_http,
        test_app_deps, test_http_config,
    },
};
use canterlot_proto::limits::{
    MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT, MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES,
};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

const SECRET: &[u8] = b"proxy-secret-0123456789-abcdefghijklmnop-SECRETVALUE";

// ---------------------------------------------------------------- журнал

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

fn captured() -> &'static Arc<Mutex<Vec<u8>>> {
    static CAPTURE: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(Capture(buffer.clone()))
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
        buffer
    })
}

fn log_text() -> String {
    String::from_utf8_lossy(&captured().lock().unwrap()).into_owned()
}

fn marker(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7().simple())
}

// ---------------------------------------------------------------- пробные маршруты

#[derive(Clone)]
struct Hits(Arc<AtomicUsize>);

#[derive(Deserialize, Serialize)]
struct EchoBody {
    n: u32,
}

async fn echo(
    _owner: Owner,
    key: IdempotencyKey,
    Correlation(correlation): Correlation,
    Extension(hits): Extension<Hits>,
    ApiJson(body): ApiJson<EchoBody>,
) -> Response {
    hits.0.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::CREATED,
        Json(json!({"key": key.0, "n": body.n, "correlation_id": correlation})),
    )
        .into_response()
}

async fn echo_get(_owner: Owner, Extension(hits): Extension<Hits>) -> Response {
    hits.0.fetch_add(1, Ordering::SeqCst);
    Json(json!({"ok": true})).into_response()
}

async fn bare(Extension(hits): Extension<Hits>) -> Response {
    hits.0.fetch_add(1, Ordering::SeqCst);
    Json(json!({"reached": true})).into_response()
}

async fn boom(_owner: Owner) -> Response {
    panic!("PANIC-PAYLOAD-MARKER-{}", Uuid::nil());
}

async fn ws(_owner: Owner, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(|socket| async move { drop(socket) })
}

async fn item(_owner: Owner, ApiPath(id): ApiPath<Uuid>) -> Response {
    Json(json!({"id": id})).into_response()
}

#[derive(Deserialize)]
struct QueryArgs {
    n: u32,
}

async fn query(_owner: Owner, ApiQuery(args): ApiQuery<QueryArgs>) -> Response {
    Json(json!({"n": args.n})).into_response()
}

#[derive(Deserialize, Serialize)]
struct EffectBody {
    tag: String,
}

#[derive(Deserialize, Serialize)]
struct Effect {
    id: Uuid,
}

async fn effect(
    owner: Owner,
    key: IdempotencyKey,
    State(state): State<AppState>,
    ApiJson(body): ApiJson<EffectBody>,
) -> Result<Response, ApiError> {
    let tag = body.tag.clone();
    let outcome = run_in_tx::<Effect, ApiError, _>(
        &state.pool,
        owner.owner_id,
        &key.0,
        "probe_effect",
        &body,
        None,
        move |conn| {
            Box::pin(async move {
                let id: Uuid =
                    sqlx::query_scalar("insert into probe_effects (tag) values ($1) returning id")
                        .bind(tag)
                        .fetch_one(conn)
                        .await?;
                Ok(Effect { id })
            })
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(outcome.value)).into_response())
}

async fn db_error(_owner: Owner, State(state): State<AppState>) -> Result<Response, ApiError> {
    sqlx::query("insert into probe_unique (v) values ($1)")
        .bind("LEAK-DETAIL-MARKER")
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn probe_routes(hits: Hits) -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/_probe/echo",
            get(echo_get).post(echo).put(echo).patch(echo).delete(echo),
        )
        .route("/api/v1/_probe/bare", get(bare))
        .route("/api/v1/_probe/panic", get(boom))
        .route("/api/v1/_probe/ws", get(ws))
        .route("/api/v1/_probe/item/{id}", get(item))
        .route("/api/v1/_probe/query", get(query))
        .route("/api/v1/_probe/effect", axum::routing::post(effect))
        .route("/api/v1/_probe/dberr", get(db_error))
        .layer(Extension(hits))
}

struct Harness {
    state: AppState,
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    async fn client(&self) -> TestClient {
        TestClient::over_socket(self.addr).await
    }

    async fn owner(&self) -> TestClient {
        self.client().await.as_owner()
    }

    async fn outsider(&self) -> TestClient {
        self.client()
            .await
            .from_source("127.0.0.2".parse().unwrap())
    }

    fn in_process(&self) -> TestClient {
        TestClient::in_process(http::router_with(
            self.state.clone(),
            probe_routes(Hits(self.hits.clone())),
        ))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
    }
}

async fn harness(pool: PgPool) -> Harness {
    harness_with(pool, test_http_config(), None).await
}

async fn harness_with(pool: PgPool, http: HttpConfig, secret: Option<&[u8]>) -> Harness {
    let (deps, dir) = test_app_deps(pool, http, secret.map(<[u8]>::to_vec)).await;
    harness_from(deps, dir).await
}

async fn harness_from(deps: http::AppDeps, dir: tempfile::TempDir) -> Harness {
    let state = AppState::new(deps);
    let hits = Arc::new(AtomicUsize::new(0));
    let router = http::router_with(state.clone(), probe_routes(Hits(hits.clone())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http::serve_router(
        router,
        listener,
        (*state.http).clone(),
        state.shutdown.clone().cancelled_owned(),
    ));
    Harness {
        state,
        addr,
        hits,
        _dir: dir,
    }
}

// ---------------------------------------------------------------- проверки

fn header<'a>(response: &'a TestResponse, name: &str) -> Option<&'a str> {
    response.headers.get(name).and_then(|v| v.to_str().ok())
}

fn text(response: &TestResponse) -> String {
    String::from_utf8_lossy(&response.body).into_owned()
}

fn has_cyrillic(value: &str) -> bool {
    value
        .chars()
        .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

fn assert_error(response: &TestResponse, status: u16, code: &str, scope: &str, retry: &str) {
    assert_eq!(response.status.as_u16(), status, "{}", text(response));
    let body: Value = response.json();
    let error = body["error"].as_object().expect("error envelope");
    let keys: BTreeSet<&str> = error.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from(["code", "correlation_id", "message", "retry", "scope"]),
        "{body}"
    );
    assert_eq!(error["code"], code, "{body}");
    assert_eq!(error["scope"], scope, "{body}");
    assert_eq!(error["retry"], retry, "{body}");
    assert!(has_cyrillic(error["message"].as_str().unwrap()), "{body}");
    let correlation = error["correlation_id"].as_str().unwrap();
    Uuid::parse_str(correlation).unwrap();
    assert_eq!(header(response, "x-correlation-id"), Some(correlation));
    assert_eq!(header(response, "content-type"), Some("application/json"));
}

/// Ответ без идентификатора корреляции и времени: по нему сравнивают «неотличимые» отказы.
fn normalized(response: &TestResponse) -> (u16, Value, Vec<(String, String)>) {
    let mut body: Value = response.json();
    body["error"]["correlation_id"] = Value::Null;
    let mut headers: Vec<(String, String)> = response
        .headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "x-correlation-id" | "x-server-time-ms" | "date"
            )
        })
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("?").to_owned()))
        .collect();
    headers.sort();
    (response.status.as_u16(), body, headers)
}

async fn me(client: &TestClient) -> TestResponse {
    client.send(Method::GET, "/api/v1/me", None, None).await
}

/// Первый запрос к `/readyz` открывает соединение пула и под нагрузкой может не уложиться в 1 с:
/// готовность проверяется как «в итоге готов», а отказы ниже детерминированы.
async fn wait_ready(client: &TestClient) -> TestResponse {
    let mut last = client.send(Method::GET, "/readyz", None, None).await;
    for _ in 0..100 {
        if last.status == 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        last = client.send(Method::GET, "/readyz", None, None).await;
    }
    last
}

async fn upgrade_status(addr: SocketAddr, source: Option<IpAddr>, headers: &[(&str, &str)]) -> u16 {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    if let Some(source) = source {
        socket.bind(SocketAddr::new(source, 0)).unwrap();
    }
    let mut stream = socket.connect(addr).await.unwrap();
    let mut request = String::from(
        "GET /api/v1/_probe/ws HTTP/1.1\r\nHost: hub\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buffer = vec![0; 4096];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .expect("upgrade answer")
        .unwrap();
    let head = String::from_utf8_lossy(&buffer[..read]).into_owned();
    head.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn ws_owner_headers() -> Vec<(&'static str, &'static str)> {
    vec![("Remote-User", TEST_OWNER_LOGIN), ("Origin", TEST_ORIGIN)]
}

async fn seed_probe_tables(pool: &PgPool) {
    for statement in [
        "create table probe_effects (id uuid primary key default gen_random_uuid(), tag text not null)",
        "create table probe_unique (v text primary key)",
        "insert into probe_unique (v) values ('LEAK-DETAIL-MARKER')",
    ] {
        sqlx::query(statement).execute(pool).await.unwrap();
    }
}

async fn effect_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from probe_effects")
        .fetch_one(pool)
        .await
        .unwrap()
}

// ---------------------------------------------------------------- здоровье и жизненный цикл

#[sqlx::test(migrations = "./migrations")]
async fn healthz_is_unauthenticated_and_readyz_reports_database_loss(pool: PgPool) {
    let hub = spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    let outsider = TestClient::over_socket(hub.http_addr())
        .await
        .from_source("127.0.0.2".parse().unwrap());
    let health = outsider.send(Method::GET, "/healthz", None, None).await;
    assert_eq!(health.status, 200);
    assert_eq!(health.json::<Value>(), json!({"status": "ok"}));
    assert_eq!(header(&health, "cache-control"), Some("no-store"));
    let ready = wait_ready(&outsider).await;
    assert_eq!(ready.status, 200, "{}", text(&ready));
    assert_eq!(ready.json::<Value>(), json!({"status": "ok"}));

    let original: Uuid = sqlx::query_scalar("select restore_generation from installation")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update installation set restore_generation = $1")
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    let mismatch = outsider.send(Method::GET, "/readyz", None, None).await;
    assert_eq!(mismatch.status, 503);
    assert_eq!(
        mismatch.json::<Value>(),
        json!({"status": "not_ready", "reason": "database"})
    );
    sqlx::query("update installation set restore_generation = $1")
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(wait_ready(&outsider).await.status, 200);

    pool.close().await;
    let lost = outsider.send(Method::GET, "/readyz", None, None).await;
    assert_eq!(lost.status, 503);
    assert_eq!(
        lost.json::<Value>(),
        json!({"status": "not_ready", "reason": "database"})
    );
    let alive = outsider.send(Method::GET, "/healthz", None, None).await;
    assert_eq!(alive.status, 200);
    let _ = hub.shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), hub.join).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn serve_starts_http_beside_grpc_and_stops_on_shutdown(pool: PgPool) {
    let hub = spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    let http_addr = hub.http_addr();
    let grpc_addr = hub.addr;
    let client = TestClient::over_socket(http_addr).await.as_owner();
    assert_eq!(me(&client).await.status, 200);
    let grpc = tokio::net::TcpStream::connect(grpc_addr).await;
    assert!(grpc.is_ok(), "gRPC port accepts connections");

    let locks = "select count(*) from pg_locks where locktype = 'advisory'
                 and database = (select oid from pg_database where datname = current_database())";
    let mut held = 0_i64;
    for _ in 0..100 {
        held = sqlx::query_scalar(locks).fetch_one(&pool).await.unwrap();
        if held > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(held > 0, "a singleton service holds its advisory lock");

    // Открытое keep-alive соединение не должно задерживать остановку.
    let idle = tokio::net::TcpStream::connect(http_addr).await.unwrap();
    hub.shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), hub.join)
        .await
        .expect("hub stops within 10 s")
        .expect("hub task")
        .expect("gRPC server result");
    drop(idle);

    assert!(tokio::net::TcpStream::connect(http_addr).await.is_err());
    assert!(tokio::net::TcpStream::connect(grpc_addr).await.is_err());

    // Ни одна служба не держит соединение пула, и никто не держит блокировку.
    let mut busy = usize::MAX;
    for _ in 0..100 {
        busy = (pool.size() as usize).saturating_sub(pool.num_idle());
        if busy == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        busy, 0,
        "no task keeps a pool connection after the shutdown"
    );
    // Замок держит своё соединение, а запись в `pg_locks` исчезает, когда серверный процесс
    // закрытого соединения завершится, то есть асинхронно: ждём условие, а не мгновение.
    tokio::time::timeout(Duration::from_secs(10), async {
        while sqlx::query_scalar::<_, i64>(locks)
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("no singleton lock outlives the shutdown");
}

#[sqlx::test(migrations = "./migrations")]
async fn background_services_stop_on_shutdown_and_release_their_locks(pool: PgPool) {
    let timers = HubTimers::default();
    let registry = canterlot_hub::session::Registry::new(timers.clone());
    let (dispatcher, dispatch_rx) = canterlot_hub::dispatcher::Dispatcher::channel();
    let blob_dir = tempfile::tempdir().unwrap();
    let (attachments, blobs) = canterlot_hub::testkit::test_blob_store(blob_dir.path());
    let background = Background::start(BackgroundDeps {
        pool: pool.clone(),
        registry,
        timers,
        dispatch_rx,
        scheduler: canterlot_hub::scheduler::Scheduler::new(
            pool.clone(),
            dispatcher,
            Default::default(),
        ),
        activity: Default::default(),
        machines: Default::default(),
        multi_process: false,
        attachments: Some((blobs, attachments)),
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    tokio::time::timeout(Duration::from_secs(10), background.stop())
        .await
        .expect("background stops")
        .expect("no service panicked");
    // Закрытие соединения асинхронно убирает запись из `pg_locks`: ждём условие,
    // а не мгновение. Инвариант тот же — ни один замок не переживает остановку.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let held: i64 = sqlx::query_scalar(
                "select count(*) from pg_locks where locktype = 'advisory'
                 and database = (select oid from pg_database where datname = current_database())",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if held == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("no singleton lock outlives the shutdown");
}

// ---------------------------------------------------------------- вход

#[sqlx::test(migrations = "./migrations")]
async fn forged_user_header_from_untrusted_peer_is_unauthenticated(pool: PgPool) {
    let h = harness(pool).await;
    let none = h
        .outsider()
        .await
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_error(&none, 401, "unauthenticated", "auth", "after_condition");
    let forged = h.outsider().await.as_owner();
    let forged = me(&forged).await;
    assert_error(&forged, 401, "unauthenticated", "auth", "after_condition");
    assert_eq!(normalized(&forged), normalized(&none));

    let other_name = h
        .outsider()
        .await
        .with_header("Remote-User", "attacker")
        .with_header("Origin", TEST_ORIGIN);
    assert_error(
        &me(&other_name).await,
        401,
        "unauthenticated",
        "auth",
        "after_condition",
    );

    let trusted = me(&h.owner().await).await;
    assert_eq!(trusted.status, 200);
    let body: Value = trusted.json();
    assert_eq!(body["login"], TEST_OWNER_LOGIN);
    assert_eq!(body["owner_id"], h.state.installation.owner_id.to_string());
    assert_eq!(h.hits(), 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn x_forwarded_for_never_grants_trust(pool: PgPool) {
    let h = harness(pool).await;
    let baseline = h
        .outsider()
        .await
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    for (name, value) in [
        ("X-Forwarded-For", "127.0.0.1"),
        ("X-Forwarded-For", "127.0.0.1, 127.0.0.1"),
        ("X-Real-IP", "127.0.0.1"),
        ("Forwarded", "for=127.0.0.1;proto=https"),
        ("X-Forwarded-Host", "127.0.0.1"),
        ("X-Forwarded-Proto", "https"),
        ("X-Forwarded-User", "owner"),
        ("X-Originating-IP", "127.0.0.1"),
        ("True-Client-IP", "127.0.0.1"),
        ("CF-Connecting-IP", "127.0.0.1"),
    ] {
        let forged = h.outsider().await.as_owner().with_header(name, value);
        let response = me(&forged).await;
        assert_error(&response, 401, "unauthenticated", "auth", "after_condition");
        assert_eq!(normalized(&response), normalized(&baseline), "{name}");
    }
    // Заголовок не отзывает и доверие от настоящего прокси.
    let trusted = h
        .owner()
        .await
        .with_header("X-Forwarded-For", "203.0.113.9")
        .with_header("Forwarded", "for=203.0.113.9");
    assert_eq!(me(&trusted).await.status, 200);
}

#[sqlx::test(migrations = "./migrations")]
async fn peer_address_is_taken_from_the_socket_not_headers(pool: PgPool) {
    let h = harness(pool).await;
    let from_proxy = me(&h.owner().await).await;
    assert_eq!(from_proxy.status, 200);
    let from_stranger = me(&h
        .outsider()
        .await
        .as_owner()
        .with_header("X-Forwarded-For", "127.0.0.1"))
    .await;
    assert_error(
        &from_stranger,
        401,
        "unauthenticated",
        "auth",
        "after_condition",
    );
    // Тот же запрос по сокету доверенного адреса принимается.
    let from_proxy_again = me(&h
        .client()
        .await
        .from_source("127.0.0.1".parse().unwrap())
        .as_owner()
        .with_header("X-Forwarded-For", "203.0.113.9"))
    .await;
    assert_eq!(from_proxy_again.status, 200);

    let upgrade_ok = upgrade_status(
        h.addr,
        Some("127.0.0.1".parse().unwrap()),
        &ws_owner_headers(),
    )
    .await;
    assert_eq!(upgrade_ok, 101);
    let mut forged = ws_owner_headers();
    forged.push(("X-Forwarded-For", "127.0.0.1"));
    let upgrade_forged = upgrade_status(h.addr, Some("127.0.0.2".parse().unwrap()), &forged).await;
    assert_eq!(upgrade_forged, 401);
}

#[sqlx::test(migrations = "./migrations")]
async fn proxy_secret_is_required_when_configured(pool: PgPool) {
    captured();
    let marker_tag = marker("WRONGSECRET");
    let h = harness_with(pool, test_http_config(), Some(SECRET)).await;
    let secret = std::str::from_utf8(SECRET).unwrap();
    let wrong_same_length = "x".repeat(SECRET.len());
    let shorter = &secret[..secret.len() - 1];
    let longer = format!("{secret}x");
    let lowercase = secret.to_lowercase();
    let baseline = h
        .outsider()
        .await
        .send(Method::GET, "/api/v1/me", None, None)
        .await;

    let mut rejected = vec![
        h.owner().await,
        h.owner().await.with_header("X-Canterlot-Proxy", ""),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy", &marker_tag),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy", &wrong_same_length),
        h.owner().await.with_header("X-Canterlot-Proxy", shorter),
        h.owner().await.with_header("X-Canterlot-Proxy", &longer),
        h.owner().await.with_header("X-Canterlot-Proxy", &lowercase),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy", secret)
            .with_header("X-Canterlot-Proxy", secret),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy", &marker_tag)
            .with_header("X-Canterlot-Proxy", secret),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy", secret)
            .with_header("X-Canterlot-Proxy", &marker_tag),
        h.owner().await.with_header("Proxy-Authorization", secret),
        h.owner()
            .await
            .with_header("X-Canterlot-Proxy-Extra", secret),
    ];
    // Правильный секрет не помогает недоверенному адресу.
    rejected.push(
        h.outsider()
            .await
            .as_owner()
            .with_header("X-Canterlot-Proxy", secret),
    );
    for client in &rejected {
        let response = me(client).await;
        assert_error(&response, 401, "unauthenticated", "auth", "after_condition");
        assert_eq!(normalized(&response), normalized(&baseline));
    }
    let accepted = me(&h.owner().await.with_header("X-Canterlot-Proxy", secret)).await;
    assert_eq!(accepted.status, 200, "{}", text(&accepted));
    assert_eq!(accepted.json::<Value>()["login"], TEST_OWNER_LOGIN);

    // Тот же барьер перед апгрейдом WSS.
    let with = |value: &'static str| {
        let mut headers = ws_owner_headers();
        headers.push(("X-Canterlot-Proxy", value));
        headers
    };
    assert_eq!(upgrade_status(h.addr, None, &ws_owner_headers()).await, 401);
    let wrong_ws = marker("WRONGWS");
    let wrong_ws: &'static str = Box::leak(wrong_ws.into_boxed_str());
    assert_eq!(upgrade_status(h.addr, None, &with(wrong_ws)).await, 401);
    let right: &'static str = Box::leak(secret.to_owned().into_boxed_str());
    assert_eq!(upgrade_status(h.addr, None, &with(right)).await, 101);
    let mut foreign_origin = with(right);
    foreign_origin.retain(|(name, _)| *name != "Origin");
    foreign_origin.push(("Origin", "https://evil.example"));
    assert_eq!(upgrade_status(h.addr, None, &foreign_origin).await, 403);

    assert_eq!(h.hits(), 0);
    let log = log_text();
    for leaked in [
        secret,
        &marker_tag,
        wrong_ws,
        &wrong_same_length,
        &longer,
        &lowercase,
    ] {
        assert!(!log.contains(leaked), "secret material reached the log");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn trusted_proxy_with_wrong_login_is_forbidden(pool: PgPool) {
    let h = harness(pool).await;
    for login in [
        "attacker",
        "Owner",
        "OWNER",
        "owner2",
        "owne",
        "owner,attacker",
        "root",
    ] {
        let client = h
            .client()
            .await
            .with_header("Remote-User", login)
            .with_header("Origin", TEST_ORIGIN);
        assert_error(&me(&client).await, 403, "forbidden_user", "auth", "never");
    }
    assert_eq!(h.hits(), 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn missing_or_duplicated_user_header_from_trusted_peer_is_unauthenticated(pool: PgPool) {
    let h = harness(pool).await;
    let cases = [
        ("none", h.client().await),
        ("empty", h.client().await.with_header("Remote-User", "")),
        (
            "twice the owner",
            h.client()
                .await
                .with_header("Remote-User", "owner")
                .with_header("Remote-User", "owner"),
        ),
        (
            "owner then attacker",
            h.client()
                .await
                .with_header("Remote-User", "owner")
                .with_header("Remote-User", "attacker"),
        ),
        (
            "attacker then owner",
            h.client()
                .await
                .with_header("Remote-User", "attacker")
                .with_header("Remote-User", "owner"),
        ),
        (
            "other header name",
            h.client().await.with_header("X-Remote-User", "owner"),
        ),
        (
            "lower-case name twice",
            h.client()
                .await
                .with_header("remote-user", "owner")
                .with_header("REMOTE-USER", "owner"),
        ),
    ];
    for (name, client) in cases {
        let response = me(&client).await;
        assert_error(&response, 401, "unauthenticated", "auth", "after_condition");
        assert!(response.status == 401, "{name}");
    }
    let single_lowercase = h.client().await.with_header("remote-user", "owner");
    assert_eq!(me(&single_lowercase).await.status, 200);
}

#[sqlx::test(migrations = "./migrations")]
async fn ipv4_mapped_ipv6_peer_matches_trusted_cidr(pool: PgPool) {
    let nets: Vec<IpNet> = ["127.0.0.1/32", "10.0.0.0/8", "::1/128"]
        .iter()
        .map(|net| net.parse().unwrap())
        .collect();
    for trusted in [
        "127.0.0.1",
        "::ffff:127.0.0.1",
        "10.1.2.3",
        "::ffff:10.1.2.3",
        "::1",
    ] {
        assert!(
            peer_is_trusted(trusted.parse().unwrap(), &nets),
            "{trusted}"
        );
    }
    for stranger in [
        "127.0.0.2",
        "::ffff:127.0.0.2",
        "11.0.0.1",
        "::ffff:11.0.0.1",
        "::2",
        "::127.0.0.1",
        "2001:db8::1",
    ] {
        assert!(
            !peer_is_trusted(stranger.parse().unwrap(), &nets),
            "{stranger}"
        );
    }
    assert!(!peer_is_trusted("127.0.0.1".parse().unwrap(), &[]));
    let mapped: Vec<IpNet> = vec!["::ffff:192.0.2.0/120".parse().unwrap()];
    assert!(peer_is_trusted(
        "::ffff:192.0.2.9".parse().unwrap(),
        &mapped
    ));
    assert!(!peer_is_trusted(
        "::ffff:192.0.3.9".parse().unwrap(),
        &mapped
    ));

    let h = harness(pool).await;
    let owner = |peer: &str| {
        h.in_process()
            .with_peer(SocketAddr::new(peer.parse().unwrap(), 40000))
            .as_owner()
    };
    assert_eq!(me(&owner("::ffff:127.0.0.1")).await.status, 200);
    assert_eq!(me(&owner("127.0.0.1")).await.status, 200);
    assert_eq!(me(&owner("::ffff:127.0.0.2")).await.status, 401);
    assert_eq!(me(&owner("::127.0.0.1")).await.status, 401);
    let no_peer = TestClient::in_process(http::router_with(
        h.state.clone(),
        probe_routes(Hits(h.hits.clone())),
    ))
    .without_peer()
    .as_owner();
    assert_eq!(
        me(&no_peer).await.status,
        401,
        "no ConnectInfo fails closed"
    );
}

#[test]
fn user_header_must_be_a_plain_header_name() {
    for bad in [
        "",
        "Remote User",
        "Remote-User\n",
        "Host",
        "Authorization",
        "Cookie",
        "Origin",
        "Forwarded",
        "X-Forwarded-For",
        "X-Real-IP",
        "X-Canterlot-Proxy",
        "X-Canterlot-Client",
        "Idempotency-Key",
        "Content-Type",
    ] {
        let mut config = test_http_config();
        config.user_header = bad.to_owned();
        assert!(config.validate().is_err(), "{bad:?}");
    }
    for good in ["Remote-User", "X-Auth-User", "x-remote-user"] {
        let mut config = test_http_config();
        config.user_header = good.to_owned();
        config.validate().unwrap();
    }
}

#[test]
fn proxy_secret_file_is_loaded_trimmed_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    let cat = |parts: &[&[u8]]| parts.concat();
    std::fs::write(&path, cat(&[SECRET, b"\n"])).unwrap();
    assert_eq!(http::load_proxy_secret(&path).unwrap(), SECRET);
    std::fs::write(&path, cat(&[b" ", SECRET, b" \r\n"])).unwrap();
    assert_eq!(http::load_proxy_secret(&path).unwrap(), SECRET);
    for bad in [
        b"short".to_vec(),
        cat(&[SECRET, b" with space"]),
        cat(&[SECRET, b"\nsecond line"]),
        cat(&[SECRET, "é".as_bytes()]),
        cat(&[SECRET, &[0x7f]]),
    ] {
        std::fs::write(&path, &bad).unwrap();
        assert!(http::load_proxy_secret(&path).is_err());
    }
    assert!(http::load_proxy_secret(&dir.path().join("missing")).is_err());
}

// ---------------------------------------------------------------- CSRF и Origin

#[sqlx::test(migrations = "./migrations")]
async fn mutation_requires_allowed_origin_and_client_header(pool: PgPool) {
    let h = harness(pool).await;
    let body = Some(json!({"n": 1}));
    let base = || h.client();
    let identity = |c: TestClient| c.with_header("Remote-User", TEST_OWNER_LOGIN);
    let table: Vec<(&str, TestClient, u16)> = vec![
        ("no origin, no client header", identity(base().await), 403),
        (
            "no origin",
            identity(base().await).with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "foreign origin",
            identity(base().await)
                .with_header("Origin", "https://evil.example")
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "null origin",
            identity(base().await)
                .with_header("Origin", "null")
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "other scheme",
            identity(base().await)
                .with_header("Origin", &TEST_ORIGIN.replace("https", "http"))
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "trailing slash",
            identity(base().await)
                .with_header("Origin", &format!("{TEST_ORIGIN}/"))
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "upper-case origin",
            identity(base().await)
                .with_header("Origin", &TEST_ORIGIN.to_uppercase())
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "origin with a path",
            identity(base().await)
                .with_header("Origin", &format!("{TEST_ORIGIN}/x"))
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "two origins, one allowed",
            identity(base().await)
                .with_header("Origin", TEST_ORIGIN)
                .with_header("Origin", "https://evil.example")
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "valid origin, no client header",
            identity(base().await).with_header("Origin", TEST_ORIGIN),
            403,
        ),
        (
            "client header 0",
            identity(base().await)
                .with_header("Origin", TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "0"),
            403,
        ),
        (
            "client header true",
            identity(base().await)
                .with_header("Origin", TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "true"),
            403,
        ),
        (
            "client header twice",
            identity(base().await)
                .with_header("Origin", TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "1")
                .with_header("X-Canterlot-Client", "1"),
            403,
        ),
        (
            "both valid",
            identity(base().await)
                .with_header("Origin", TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "1"),
            201,
        ),
    ];
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let before = h.hits();
        let mut accepted = 0;
        for (name, client, expected) in &table {
            let response = client
                .send(
                    method.clone(),
                    "/api/v1/_probe/echo",
                    Some("key-1"),
                    body.clone(),
                )
                .await;
            assert_eq!(
                response.status.as_u16(),
                *expected,
                "{method} {name}: {}",
                text(&response)
            );
            if *expected == 403 {
                assert_error(&response, 403, "origin_rejected", "auth", "never");
            } else {
                accepted += 1;
            }
        }
        assert_eq!(
            h.hits() - before,
            accepted,
            "{method}: only the valid request reached the handler"
        );
    }

    // Отказ по Origin наступает раньше проверки личности: чужак не доходит и до 401.
    let stranger = h.outsider().await.with_header("Remote-User", "owner");
    let response = stranger
        .send(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("key-1"),
            body.clone(),
        )
        .await;
    assert_error(&response, 403, "origin_rejected", "auth", "never");
}

#[sqlx::test(migrations = "./migrations")]
async fn safe_methods_need_no_origin(pool: PgPool) {
    let h = harness(pool).await;
    let bare_owner = h
        .client()
        .await
        .with_header("Remote-User", TEST_OWNER_LOGIN);
    let get = bare_owner.send(Method::GET, "/api/v1/me", None, None).await;
    assert_eq!(get.status, 200);
    let head = bare_owner
        .send(Method::HEAD, "/api/v1/me", None, None)
        .await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    // WSS-апгрейд — это GET, но Origin для него обязателен.
    let no_origin = upgrade_status(h.addr, None, &[("Remote-User", TEST_OWNER_LOGIN)]).await;
    assert_eq!(no_origin, 403);
    assert_eq!(upgrade_status(h.addr, None, &ws_owner_headers()).await, 101);
}

#[sqlx::test(migrations = "./migrations")]
async fn cross_origin_requests_get_no_cors_headers_and_options_is_405(pool: PgPool) {
    let h = harness(pool).await;
    let no_cors = |response: &TestResponse| {
        for name in response.headers.keys() {
            assert!(!name.as_str().starts_with("access-control-"), "{name}");
        }
        assert!(
            response.headers.get("vary").is_none_or(|v| !v
                .to_str()
                .unwrap()
                .to_lowercase()
                .contains("origin"))
        );
    };
    let foreign = h
        .client()
        .await
        .with_header("Remote-User", TEST_OWNER_LOGIN)
        .with_header("Origin", "https://evil.example");
    let get = me(&foreign).await;
    assert_eq!(get.status, 200);
    no_cors(&get);

    for origin in ["https://evil.example", TEST_ORIGIN] {
        for path in [
            "/api/v1/me",
            "/api/v1/_probe/echo",
            "/api/v1/missing",
            "/healthz",
        ] {
            for client in [
                h.client()
                    .await
                    .with_header("Origin", origin)
                    .with_header("Access-Control-Request-Method", "POST")
                    .with_header(
                        "Access-Control-Request-Headers",
                        "x-canterlot-client, idempotency-key",
                    ),
                h.outsider()
                    .await
                    .with_header("Origin", origin)
                    .with_header("Access-Control-Request-Method", "POST"),
            ] {
                let response = client.send(Method::OPTIONS, path, None, None).await;
                assert_error(&response, 405, "method_not_allowed", "browser", "never");
                no_cors(&response);
            }
        }
    }
    let rejected = h
        .client()
        .await
        .with_header("Remote-User", "owner")
        .with_header("Origin", "https://evil.example")
        .with_header("X-Canterlot-Client", "1")
        .send(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("k"),
            Some(json!({"n": 1})),
        )
        .await;
    assert_error(&rejected, 403, "origin_rejected", "auth", "never");
    no_cors(&rejected);
    let accepted = h
        .owner()
        .await
        .send(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("k"),
            Some(json!({"n": 1})),
        )
        .await;
    assert_eq!(accepted.status, 201);
    no_cors(&accepted);
}

#[sqlx::test(migrations = "./migrations")]
async fn every_route_requires_identity_unless_listed_public(pool: PgPool) {
    let h = harness(pool).await;
    let outsider = h.outsider().await.as_owner();
    let mut checked = 0;
    let document: Value = serde_json::from_str(&openapi::render()).unwrap();
    for (path, item) in document["paths"].as_object().unwrap() {
        if matches!(path.as_str(), "/healthz" | "/readyz") {
            continue;
        }
        let concrete = path.replace("{id}", &Uuid::now_v7().to_string());
        for method in item.as_object().unwrap().keys() {
            let method = Method::from_bytes(method.to_uppercase().as_bytes()).unwrap();
            let response = outsider
                .send(method.clone(), &concrete, Some("k"), Some(json!({})))
                .await;
            if method == Method::HEAD {
                // Ответ на HEAD не несёт тела: проверяется только статус.
                assert_eq!(response.status, 401, "HEAD {path}");
            } else {
                assert_error(&response, 401, "unauthenticated", "auth", "after_condition");
            }
            checked += 1;
        }
    }
    assert!(checked >= 3, "documented routes were exercised");
    // Обработчик без экстрактора Owner защищён общим барьером.
    let bare_outsider = outsider
        .send(Method::GET, "/api/v1/_probe/bare", None, None)
        .await;
    assert_error(
        &bare_outsider,
        401,
        "unauthenticated",
        "auth",
        "after_condition",
    );
    let unknown = outsider
        .send(Method::GET, "/api/v1/nothing-here", None, None)
        .await;
    assert_error(&unknown, 401, "unauthenticated", "auth", "after_condition");
    assert_eq!(h.hits(), 0);
    let bare_owner = h
        .owner()
        .await
        .send(Method::GET, "/api/v1/_probe/bare", None, None)
        .await;
    assert_eq!(bare_owner.status, 200);
    let missing = h
        .owner()
        .await
        .send(Method::GET, "/api/v1/nothing-here", None, None)
        .await;
    assert_error(&missing, 404, "not_found", "browser", "never");
}

// ---------------------------------------------------------------- Idempotency-Key

#[sqlx::test(migrations = "./migrations")]
async fn idempotency_key_is_required_on_mutations_and_validated(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let body = Some(json!({"n": 7}));
    let post = async |key: Option<&str>| {
        owner
            .send(Method::POST, "/api/v1/_probe/echo", key, body.clone())
            .await
    };

    assert_error(
        &post(None).await,
        400,
        "invalid_request",
        "browser",
        "never",
    );
    let before = h.hits();
    for bad in [
        "",
        " ",
        "a b",
        "a/b",
        "a.b",
        "a+b",
        "a=b",
        "é",
        "ключ",
        "a:b",
        &"a".repeat(257),
    ] {
        let response = post(Some(bad)).await;
        assert_error(&response, 400, "invalid_request", "browser", "never");
    }
    assert_eq!(h.hits(), before);
    for good in [
        "a",
        "A-z_0-9",
        &"a".repeat(256),
        "0190b3a2-7c1e-7000-8000-000000000001",
    ] {
        let response = post(Some(good)).await;
        assert_eq!(response.status, 201, "{good}: {}", text(&response));
        assert_eq!(response.json::<Value>()["key"], good);
    }
    let twice = owner
        .clone()
        .with_header("Idempotency-Key", "one")
        .send(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("two"),
            body.clone(),
        )
        .await;
    assert_error(&twice, 400, "invalid_request", "browser", "never");
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotency_key_charset_excludes_internal_namespaces(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    for key in ["sched:x", "wake:x", "sched:", "wake:0190b3a2", "SCHED:x"] {
        let response = owner
            .send(
                Method::POST,
                "/api/v1/_probe/echo",
                Some(key),
                Some(json!({"n": 1})),
            )
            .await;
        assert_error(&response, 400, "invalid_request", "browser", "never");
    }
    assert_eq!(h.hits(), 0);
}

// ---------------------------------------------------------------- формат ошибок

#[sqlx::test(migrations = "./migrations")]
async fn error_body_has_code_scope_retry_and_correlation_id_and_russian_message_without_user_content(
    pool: PgPool,
) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let leak = marker("USERCONTENT");
    let mut responses = Vec::new();

    responses.push((
        owner
            .send_raw(
                Method::POST,
                "/api/v1/_probe/echo",
                Some("k"),
                format!("{{\"n\": \"{leak}\"}}").into_bytes(),
            )
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send_raw(
                Method::POST,
                "/api/v1/_probe/echo",
                Some("k"),
                format!("{{\"n\": 1, \"{leak}").into_bytes(),
            )
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send_raw(Method::POST, "/api/v1/_probe/echo", Some("k"), Vec::new())
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send(
                Method::GET,
                &format!("/api/v1/_probe/item/{leak}"),
                None,
                None,
            )
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send(
                Method::GET,
                &format!("/api/v1/_probe/query?n={leak}"),
                None,
                None,
            )
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send(
                Method::GET,
                &format!("/api/v1/_probe/query?{leak}=1"),
                None,
                None,
            )
            .await,
        400,
        "invalid_request",
    ));
    responses.push((
        owner
            .send(Method::GET, &format!("/api/v1/missing/{leak}"), None, None)
            .await,
        404,
        "not_found",
    ));
    responses.push((
        owner
            .send(
                Method::DELETE,
                "/api/v1/me",
                Some("k"),
                Some(json!({"x": leak})),
            )
            .await,
        405,
        "method_not_allowed",
    ));
    for (response, status, code) in &responses {
        assert_eq!(response.status.as_u16(), *status, "{}", text(response));
        let body: Value = response.json();
        assert_eq!(body["error"]["code"], *code);
        let message = body["error"]["message"].as_str().unwrap();
        assert!(has_cyrillic(message));
        assert!(
            !text(response).contains(&leak),
            "user content echoed: {}",
            text(response)
        );
        assert!(body["error"].get("details").is_none());
    }
    let ok_item = owner
        .send(
            Method::GET,
            &format!("/api/v1/_probe/item/{}", Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(ok_item.status, 200);
    assert_error(&responses[0].0, 400, "invalid_request", "browser", "never");

    // Каждый код реестра собирается в корректный конверт без пользовательского содержимого.
    for code in ErrorCode::ALL {
        let response = ApiError::new(*code).into_response();
        assert_eq!(response.status(), code.status());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], code.as_str());
        assert_eq!(
            body["error"]["scope"],
            serde_json::to_value(code.scope()).unwrap()
        );
        assert_eq!(
            body["error"]["retry"],
            serde_json::to_value(code.retry()).unwrap()
        );
        assert!(has_cyrillic(body["error"]["message"].as_str().unwrap()));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn correlation_id_from_header_is_echoed_and_invalid_one_is_replaced(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let id = Uuid::now_v7();
    let ok = owner
        .clone()
        .with_header("X-Correlation-Id", &id.to_string())
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(
        header(&ok, "x-correlation-id"),
        Some(id.to_string().as_str())
    );
    let failed = h
        .outsider()
        .await
        .with_header("X-Correlation-Id", &id.to_string())
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(
        failed.json::<Value>()["error"]["correlation_id"],
        id.to_string()
    );
    assert_eq!(
        header(&failed, "x-correlation-id"),
        Some(id.to_string().as_str())
    );

    let upper = owner
        .clone()
        .with_header("X-Correlation-Id", &id.to_string().to_uppercase())
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(
        header(&upper, "x-correlation-id"),
        Some(id.to_string().as_str())
    );

    let junk = marker("NOTAUUID");
    let long = "a".repeat(10_000);
    for invalid in [
        "not-a-uuid",
        "",
        junk.as_str(),
        long.as_str(),
        "0190b3a27c1e70008000000000000001",
        "{0190b3a2-7c1e-7000-8000-000000000001}",
        "urn:uuid:0190b3a2-7c1e-7000-8000-000000000001",
    ] {
        let response = owner
            .clone()
            .with_header("X-Correlation-Id", invalid)
            .send(Method::GET, "/api/v1/me", None, None)
            .await;
        let echoed = header(&response, "x-correlation-id").expect("always echoed");
        let parsed = Uuid::parse_str(echoed).expect("generated id is a UUID");
        assert_ne!(echoed, invalid);
        assert!(!parsed.is_nil());
    }
    let two = owner
        .clone()
        .with_header("X-Correlation-Id", &id.to_string())
        .with_header("X-Correlation-Id", &Uuid::now_v7().to_string())
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_ne!(
        header(&two, "x-correlation-id"),
        Some(id.to_string().as_str())
    );
    let generated = owner.send(Method::GET, "/api/v1/me", None, None).await;
    Uuid::parse_str(header(&generated, "x-correlation-id").unwrap()).unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn request_log_has_no_header_values_query_or_body(pool: PgPool) {
    captured();
    let h = harness_with(pool, test_http_config(), Some(SECRET)).await;
    let leak = marker("LEAKED");
    let correlation = Uuid::now_v7();
    let secret = std::str::from_utf8(SECRET).unwrap();
    let owner = h
        .client()
        .await
        .with_header("Remote-User", TEST_OWNER_LOGIN)
        .with_header("Origin", TEST_ORIGIN)
        .with_header("X-Canterlot-Client", "1")
        .with_header("X-Canterlot-Proxy", secret)
        .with_header("Authorization", &format!("Bearer {leak}"))
        .with_header("Cookie", &format!("session={leak}"))
        .with_header("X-Custom", &leak)
        .with_header("X-Correlation-Id", &correlation.to_string());
    let paths = [
        (Method::GET, format!("/api/v1/me?q={leak}&token={leak}")),
        (
            Method::GET,
            format!("/api/v1/_probe/item/{}?q={leak}", Uuid::now_v7()),
        ),
        (Method::GET, format!("/api/v1/unknown/{leak}?q={leak}")),
        (Method::POST, format!("/api/v1/_probe/echo?q={leak}")),
        (Method::GET, "/api/v1/_probe/panic".into()),
    ];
    for (method, path) in paths {
        let body = (method == Method::POST).then(|| json!({"n": 1, "pad": leak}));
        let _ = owner.send(method, &path, Some(&leak), body).await;
    }
    let wrong = h
        .client()
        .await
        .with_header("Remote-User", &leak)
        .with_header("X-Canterlot-Proxy", &leak)
        .with_header("X-Correlation-Id", &correlation.to_string())
        .send_raw(
            Method::POST,
            &format!("/api/v1/me?q={leak}"),
            Some(&leak),
            format!("{{\"n\": \"{leak}\"}}").into_bytes(),
        )
        .await;
    assert_eq!(wrong.status, 403);
    let _ = h
        .outsider()
        .await
        .with_header("Remote-User", &leak)
        .with_header("Origin", TEST_ORIGIN)
        .with_header("X-Correlation-Id", &correlation.to_string())
        .send(Method::GET, &format!("/api/v1/me?q={leak}"), None, None)
        .await;

    let log = log_text();
    assert!(!log.contains(&leak), "request data reached the log");
    assert!(!log.contains(secret));
    let lines: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("http_request") && line.contains(&correlation.to_string()))
        .collect();
    assert!(lines.len() >= 6, "one line per request: {lines:?}");
    for line in &lines {
        for field in [
            "method=",
            "route=",
            "status=",
            "latency_ms=",
            "correlation_id=",
        ] {
            assert!(line.contains(field), "{field} missing in {line}");
        }
    }
    assert!(
        lines
            .iter()
            .any(|line| line.contains("route=\"/api/v1/me\""))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("route=\"/api/v1/_probe/item/{id}\""))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("route=\"unmatched\""))
    );
    assert!(lines.iter().any(|line| line.contains("status=500")));
    let panic_lines: Vec<&str> = log.lines().filter(|l| l.contains("http_panic")).collect();
    assert!(!panic_lines.is_empty());
    assert!(
        panic_lines
            .iter()
            .all(|l| !l.contains("PANIC-PAYLOAD-MARKER"))
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn oversized_json_body_is_413_resource_limit(pool: PgPool) {
    let mut config = test_http_config();
    config.max_json_body_bytes = 1024;
    let h = harness_with(pool, config, None).await;
    let owner = h.owner().await;
    let padded = |size: usize| format!("{{\"n\":1,\"pad\":\"{}\"}}", "x".repeat(size)).into_bytes();
    let small = owner
        .send_raw(Method::POST, "/api/v1/_probe/echo", Some("k"), padded(100))
        .await;
    assert_eq!(small.status, 201);
    let before = h.hits();
    let large = owner
        .send_raw(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("k2"),
            padded(4096),
        )
        .await;
    assert_error(&large, 413, "resource_limit", "browser", "never");
    assert!(!text(&large).contains("xxxx"));
    assert_eq!(h.hits(), before);
}

#[sqlx::test(migrations = "./migrations")]
async fn default_json_limit_is_512_kib(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let padded = |size: usize| format!("{{\"n\":1,\"pad\":\"{}\"}}", "x".repeat(size)).into_bytes();
    let ok = owner
        .send_raw(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("k"),
            padded(500 * 1024),
        )
        .await;
    assert_eq!(ok.status, 201);
    let over = owner
        .send_raw(
            Method::POST,
            "/api/v1/_probe/echo",
            Some("k2"),
            padded(525 * 1024),
        )
        .await;
    assert_error(&over, 413, "resource_limit", "browser", "never");
}

#[sqlx::test(migrations = "./migrations")]
async fn panic_in_a_handler_is_500_internal_without_details(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let response = owner
        .send(Method::GET, "/api/v1/_probe/panic", None, None)
        .await;
    assert_error(&response, 500, "internal", "hub_db", "safe_same_key");
    assert!(!text(&response).contains("PANIC-PAYLOAD-MARKER"));
    assert_eq!(header(&response, "cache-control"), Some("no-store"));
    assert!(header(&response, "x-server-time-ms").is_some());
    // Сервер продолжает отвечать.
    assert_eq!(me(&owner).await.status, 200);
}

// ---------------------------------------------------------------- /config, /me, заголовки, OpenAPI

#[sqlx::test(migrations = "./migrations")]
async fn config_endpoint_matches_proto_limits_and_config_structs(pool: PgPool) {
    let mut http = test_http_config();
    http.ws.delta_window = Duration::from_millis(40);
    let (mut deps, dir) = test_app_deps(pool, http, None).await;
    deps.limits = LimitsConfig {
        weekly_reserve_pct: 90,
        warn_pcts: vec![70, 85],
        stale_after: Duration::from_secs(1200),
        default_resume_text: "Продолжай работу.".into(),
        ..LimitsConfig::default()
    };
    deps.activity = ActivityConfig {
        stalled_generating: Duration::from_secs(120),
        stalled_tool: Duration::from_secs(600),
        ..ActivityConfig::default()
    };
    deps.machines = MachinesConfig {
        disk_warn_pct: 70,
        metrics_stale_after: Duration::from_secs(45),
    };
    let h = harness_from(deps, dir).await;
    let response = me_path(&h, "/api/v1/config").await;
    assert_eq!(response.status, 200, "{}", text(&response));
    assert_eq!(
        response.json::<Value>(),
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "schema_version": 1,
            "limits": {
                "max_input_text_bytes": MAX_INPUT_TEXT_BYTES,
                "max_input_meta_bytes": MAX_INPUT_META_BYTES,
                "max_attachment_bytes": MAX_ATTACHMENT_BYTES,
                "max_attachments_per_input": MAX_ATTACHMENTS_PER_INPUT,
                "queue_limit": canterlot_hub::delivery::QUEUE_LIMIT,
                "upload_chunk_bytes": UPLOAD_CHUNK_BYTES,
            },
            "ws": {"path": "/api/v1/ws", "protocol": "canterlot.v1", "delta_window_ms": 40},
            "activity": {"stalled_generating_secs": 120, "stalled_tool_secs": 600},
            "machines": {"disk_warn_pct": 70, "metrics_stale_after_secs": 45},
            "limit_thresholds": {"weekly_reserve_pct": 90, "warn_pcts": [70, 85], "stale_after_secs": 1200},
            "defaults": {"crunch_resume_text": "Продолжай работу."},
            "auth": {"logout_url": null},
        })
    );
    assert_eq!(UPLOAD_CHUNK_BYTES, 8 * 1024 * 1024);
    assert_eq!(MAX_ATTACHMENT_BYTES, 104_857_600);
    assert_eq!(MAX_ATTACHMENTS_PER_INPUT, 10);
}

async fn me_path(h: &Harness, path: &str) -> TestResponse {
    h.owner().await.send(Method::GET, path, None, None).await
}

#[sqlx::test(migrations = "./migrations")]
async fn config_exposes_logout_url(pool: PgPool) {
    let mut http = test_http_config();
    http.logout_url = Some("https://auth.example/logout?rd=https%3A%2F%2Fchat.test".into());
    let h = harness_with(pool.clone(), http, None).await;
    let with: Value = me_path(&h, "/api/v1/config").await.json();
    assert_eq!(
        with["auth"],
        json!({"logout_url": "https://auth.example/logout?rd=https%3A%2F%2Fchat.test"})
    );
    let h = harness(pool).await;
    let without: Value = me_path(&h, "/api/v1/config").await.json();
    assert_eq!(without["auth"], json!({"logout_url": null}));
}

#[sqlx::test(migrations = "./migrations")]
async fn config_exposes_reserve_and_warning_thresholds(pool: PgPool) {
    let h = harness(pool).await;
    let config: Value = me_path(&h, "/api/v1/config").await.json();
    let defaults = LimitsConfig::default();
    assert_eq!(
        config["limit_thresholds"],
        json!({
            "weekly_reserve_pct": defaults.weekly_reserve_pct,
            "warn_pcts": defaults.warn_pcts,
            "stale_after_secs": defaults.stale_after.as_secs(),
        })
    );
    assert_eq!(
        config["defaults"]["crunch_resume_text"],
        defaults.default_resume_text
    );
    assert_eq!(config["limit_thresholds"]["weekly_reserve_pct"], 95);
    assert_eq!(config["limit_thresholds"]["warn_pcts"], json!([80, 90, 95]));
}

#[sqlx::test(migrations = "./migrations")]
async fn api_responses_carry_no_store_and_server_time(pool: PgPool) {
    let h = harness(pool).await;
    let owner = h.owner().await;
    let stranger = h.outsider().await.as_owner();
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let responses = vec![
        me(&owner).await,
        me(&stranger).await,
        owner.send(Method::GET, "/api/v1/missing", None, None).await,
        owner.send(Method::OPTIONS, "/api/v1/me", None, None).await,
        owner
            .send(Method::DELETE, "/api/v1/me", Some("k"), Some(json!({})))
            .await,
        owner
            .send(Method::GET, "/api/v1/_probe/panic", None, None)
            .await,
        owner
            .send(Method::GET, "/api/v1/_probe/query?n=x", None, None)
            .await,
        owner
            .clone()
            .with_header("Origin", "https://evil.example")
            .send(
                Method::POST,
                "/api/v1/_probe/echo",
                Some("k"),
                Some(json!({"n": 1})),
            )
            .await,
        owner.send(Method::GET, "/api/v1/config", None, None).await,
        owner
            .send(Method::GET, "/api/v1/openapi.json", None, None)
            .await,
    ];
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let statuses: Vec<u16> = responses.iter().map(|r| r.status.as_u16()).collect();
    assert_eq!(statuses[..4], [200, 401, 404, 405]);
    for response in &responses {
        assert_eq!(
            header(response, "cache-control"),
            Some("no-store"),
            "{}",
            response.status
        );
        let stamp: u128 = header(response, "x-server-time-ms")
            .unwrap_or_else(|| panic!("server time missing on {}", response.status))
            .parse()
            .expect("integer milliseconds");
        assert!(
            stamp + 2000 >= before && stamp <= after + 2000,
            "{stamp} not in [{before}, {after}]"
        );
        assert_eq!(header(response, "x-content-type-options"), Some("nosniff"));
    }
    let health = owner.send(Method::GET, "/healthz", None, None).await;
    assert_eq!(header(&health, "cache-control"), Some("no-store"));
}

#[sqlx::test(migrations = "./migrations")]
async fn openapi_is_served_and_committed_file_matches_generated(pool: PgPool) {
    let h = harness(pool).await;
    let served = me_path(&h, "/api/v1/openapi.json").await;
    assert_eq!(served.status, 200);
    assert_eq!(header(&served, "content-type"), Some("application/json"));
    let generated = openapi::render();
    assert_eq!(
        serde_json::from_slice::<Value>(&served.body).unwrap(),
        serde_json::from_str::<Value>(&generated).unwrap()
    );
    let document: Value = serde_json::from_str(&generated).unwrap();
    for path in [
        "/healthz",
        "/readyz",
        "/api/v1/me",
        "/api/v1/config",
        "/api/v1/openapi.json",
    ] {
        assert!(
            document["paths"].get(path).is_some(),
            "{path} missing from the document"
        );
    }
    assert!(document["components"]["schemas"].get("ErrorCode").is_some());
    assert!(
        document["components"]["schemas"]
            .get("ErrorEnvelope")
            .is_some()
    );

    let committed = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/openapi.json"),
    )
    .expect("docs/api/openapi.json is committed");
    assert!(
        committed == generated,
        "docs/api/openapi.json is stale: run scripts/gen-openapi.sh and commit the result"
    );
}

#[test]
fn error_code_table_in_api_md_matches_the_enum_and_openapi() {
    let api_md = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/api.md"),
    )
    .unwrap();
    let section = api_md
        .split("\n## Errors")
        .nth(1)
        .expect("section Errors")
        .split("\n## ")
        .next()
        .unwrap();
    let mut rows = std::collections::BTreeMap::new();
    for line in section
        .lines()
        .filter(|l| l.starts_with("| `") && !l.starts_with("| `code`"))
    {
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 5, "{line}");
        let code = cells[0].trim_matches('`').to_owned();
        let row = (
            cells[1].parse::<u16>().unwrap(),
            cells[2].trim_matches('`').to_owned(),
            cells[3].trim_matches('`').to_owned(),
            cells[4].to_owned(),
        );
        assert!(
            rows.insert(code.clone(), row).is_none(),
            "duplicate row {code}"
        );
    }
    let wire: BTreeSet<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(wire.len(), ErrorCode::ALL.len(), "codes are unique");
    assert_eq!(
        rows.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        wire,
        "the table and the enum list different codes"
    );
    for code in ErrorCode::ALL {
        let (status, scope, retry, message) = &rows[code.as_str()];
        assert_eq!(*status, code.status().as_u16(), "{code:?}");
        assert_eq!(
            scope,
            serde_json::to_value(code.scope())
                .unwrap()
                .as_str()
                .unwrap(),
            "{code:?}"
        );
        assert_eq!(
            retry,
            serde_json::to_value(code.retry())
                .unwrap()
                .as_str()
                .unwrap(),
            "{code:?}"
        );
        assert_eq!(message, code.message(), "{code:?}");
        assert_eq!(
            serde_json::to_value(code).unwrap(),
            code.as_str(),
            "serde name equals the wire code"
        );
        assert!(
            code.as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_')
        );
        assert!(has_cyrillic(message));
        assert!(!message.contains(['{', '}', '%']));
        let self_clearing = matches!(code.status().as_u16(), 429 | 500 | 503);
        assert_eq!(
            code.retry() == Retry::SafeSameKey,
            self_clearing,
            "retry is safe_same_key only for 429/500/503: {code:?}"
        );
        if code.status().is_client_error() && !self_clearing {
            assert!(matches!(
                code.retry(),
                Retry::AfterCondition | Retry::ExplicitNewOperation | Retry::Never
            ));
        }
    }
    let _ = ErrorScope::Browser;
    let document: Value = serde_json::from_str(&openapi::render()).unwrap();
    let in_document: BTreeSet<&str> = document["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .expect("ErrorCode is an enum schema")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        in_document, wire,
        "components.schemas.ErrorCode differs from the enum"
    );
}

// ---------------------------------------------------------------- идемпотентность

#[derive(Debug)]
enum TestError {
    Db(#[allow(dead_code)] sqlx::Error),
    Conflict,
    Op,
}
impl From<sqlx::Error> for TestError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}
impl From<IdempotencyConflict> for TestError {
    fn from(_: IdempotencyConflict) -> Self {
        Self::Conflict
    }
}

async fn seed_owner(pool: &PgPool) -> Uuid {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    owner
}

#[sqlx::test(migrations = "./migrations")]
async fn run_replays_stored_response_and_conflicts_on_other_request(pool: PgPool) {
    seed_probe_tables(&pool).await;
    let owner = seed_owner(&pool).await;
    let other_owner = seed_owner(&pool).await;
    let runs = Arc::new(AtomicUsize::new(0));
    let call =
        |owner: Uuid, key: &'static str, operation: &'static str, request: Value, fail: bool| {
            let pool = pool.clone();
            let runs = runs.clone();
            async move {
                run_in_tx::<Effect, TestError, _>(
                    &pool,
                    owner,
                    key,
                    operation,
                    &request,
                    None,
                    move |conn| {
                        Box::pin(async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            let id: Uuid = sqlx::query_scalar(
                                "insert into probe_effects (tag) values ('x') returning id",
                            )
                            .fetch_one(conn)
                            .await?;
                            if fail {
                                return Err(TestError::Op);
                            }
                            Ok(Effect { id })
                        })
                    },
                )
                .await
            }
        };
    // Ошибка операции откатывает эффект и не занимает ключ.
    assert!(matches!(
        call(owner, "k1", "op", json!({"a": 1}), true).await,
        Err(TestError::Op)
    ));
    assert_eq!(effect_count(&pool).await, 0);
    let first = call(owner, "k1", "op", json!({"a": 1}), false)
        .await
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(effect_count(&pool).await, 1);
    let runs_after_first = runs.load(Ordering::SeqCst);

    let second = call(owner, "k1", "op", json!({"a": 1}), false)
        .await
        .unwrap();
    assert!(second.replayed);
    assert_eq!(second.value.id, first.value.id);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        runs_after_first,
        "op did not run on replay"
    );
    assert_eq!(effect_count(&pool).await, 1);
    // Порядок полей запроса не меняет хеш.
    let reordered = call(owner, "k1", "op", json!({"a": 1}), false)
        .await
        .unwrap();
    assert!(reordered.replayed);

    for (key, operation, request) in [
        ("k1", "op", json!({"a": 2})),
        ("k1", "other_op", json!({"a": 1})),
    ] {
        assert!(
            matches!(
                call(owner, key, operation, request, false).await,
                Err(TestError::Conflict)
            ),
            "{key} {operation}"
        );
    }
    assert_eq!(effect_count(&pool).await, 1, "a conflict has no effect");
    // Ключ принадлежит владельцу.
    let foreign = call(other_owner, "k1", "op", json!({"a": 1}), false)
        .await
        .unwrap();
    assert!(!foreign.replayed);
    assert_eq!(effect_count(&pool).await, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn run_in_tx_executes_once_for_concurrent_same_key(pool: PgPool) {
    seed_probe_tables(&pool).await;
    let owner = seed_owner(&pool).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let spawn_call = |tag: &'static str| {
        let pool = pool.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            run_in_tx::<Effect, TestError, _>(
                &pool,
                owner,
                "same-key",
                "op",
                &json!({"a": 1}),
                None,
                move |conn| {
                    Box::pin(async move {
                        let id: Uuid = sqlx::query_scalar(
                            "insert into probe_effects (tag) values ($1) returning id",
                        )
                        .bind(tag)
                        .fetch_one(conn)
                        .await?;
                        Ok(Effect { id })
                    })
                },
            )
            .await
        })
    };
    let (a, b) = (spawn_call("a"), spawn_call("b"));
    let a = a.await.unwrap().unwrap();
    let b = b.await.unwrap().unwrap();
    assert_eq!(effect_count(&pool).await, 1, "exactly one effect row");
    assert_eq!(
        a.replayed as u8 + b.replayed as u8,
        1,
        "one winner, one replay"
    );
    assert_eq!(a.value.id, b.value.id);
    let recorded: i64 =
        sqlx::query_scalar("select count(*) from idempotency_keys where key = 'same-key'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recorded, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotent_mutation_retried_after_a_restart_replays_the_stored_response(pool: PgPool) {
    seed_probe_tables(&pool).await;
    let first = harness(pool.clone()).await;
    let request = |h: Harness| async move {
        let response = h
            .owner()
            .await
            .send(
                Method::POST,
                "/api/v1/_probe/effect",
                Some("retry-key"),
                Some(json!({"tag": "t"})),
            )
            .await;
        (h, response)
    };
    let (first, created) = request(first).await;
    assert_eq!(created.status, 201, "{}", text(&created));
    assert_eq!(effect_count(&pool).await, 1);

    // «Перезапуск»: новый пул, новое состояние, новый маршрутизатор.
    drop(first);
    let reborn = PgPool::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let second = harness(reborn.clone()).await;
    let (second, replay) = request(second).await;
    assert_eq!(replay.status, 201);
    assert_eq!(replay.body, created.body, "byte-identical replay");
    assert_eq!(effect_count(&pool).await, 1);

    let conflicting = second
        .owner()
        .await
        .send(
            Method::POST,
            "/api/v1/_probe/effect",
            Some("retry-key"),
            Some(json!({"tag": "other"})),
        )
        .await;
    assert_error(
        &conflicting,
        409,
        "idempotency_conflict",
        "browser",
        "never",
    );

    // Простой базы: ответ 503 с повтором тем же ключом, затем исполнение и повтор.
    let outage_key = "outage-key";
    let down = harness(reborn.clone()).await;
    reborn.close().await;
    let during = down
        .owner()
        .await
        .send(
            Method::POST,
            "/api/v1/_probe/effect",
            Some(outage_key),
            Some(json!({"tag": "o"})),
        )
        .await;
    assert_error(&during, 503, "db_unavailable", "hub_db", "safe_same_key");
    assert_eq!(effect_count(&pool).await, 1);
    drop(down);
    let back = PgPool::connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let recovered = harness(back).await;
    let client = recovered.owner().await;
    let call = || {
        client.send(
            Method::POST,
            "/api/v1/_probe/effect",
            Some(outage_key),
            Some(json!({"tag": "o"})),
        )
    };
    let executed = call().await;
    assert_eq!(executed.status, 201);
    let replayed = call().await;
    assert_eq!(replayed.body, executed.body);
    assert_eq!(effect_count(&pool).await, 2);
}

// ---------------------------------------------------------------- ошибки базы данных

#[sqlx::test(migrations = "./migrations")]
async fn db_error_logging_helper_returns_only_sqlstate_and_constraint(pool: PgPool) {
    seed_probe_tables(&pool).await;
    let duplicate = sqlx::query("insert into probe_unique (v) values ('LEAK-DETAIL-MARKER')")
        .execute(&pool)
        .await
        .unwrap_err();
    // Контроль теста: сам драйвер действительно несёт значение строки в DETAIL.
    let driver = duplicate.as_database_error().unwrap();
    let detail = driver
        .try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
        .unwrap()
        .detail()
        .unwrap_or_default()
        .to_owned();
    assert!(detail.contains("LEAK-DETAIL-MARKER"), "{detail}");

    let info = dberr::describe(&duplicate);
    assert_eq!(info.sqlstate.as_deref(), Some("23505"));
    assert_eq!(info.constraint.as_deref(), Some("probe_unique_pkey"));
    assert_eq!(info.class, "constraint");
    for rendered in [format!("{info:?}"), format!("{info}")] {
        assert!(!rendered.contains("LEAK-DETAIL-MARKER"), "{rendered}");
        assert!(!rendered.contains("already exists"));
    }

    let missing_owner =
        sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, 'LEAK-FK-MARKER')")
            .bind(Uuid::now_v7())
            .bind(Uuid::now_v7())
            .execute(&pool)
            .await
            .unwrap_err();
    let fk = dberr::describe(&missing_owner);
    assert_eq!(fk.sqlstate.as_deref(), Some("23503"));
    assert!(
        fk.constraint
            .as_deref()
            .is_some_and(|c| c.starts_with("machines"))
    );
    assert!(!format!("{fk:?}{fk}").contains("LEAK-FK-MARKER"));

    let closed = dberr::describe(&sqlx::Error::PoolClosed);
    assert_eq!(
        (closed.sqlstate, closed.constraint, closed.class),
        (None, None, "unavailable")
    );
    assert_eq!(
        dberr::describe(&sqlx::Error::PoolTimedOut).class,
        "unavailable"
    );
    assert_eq!(dberr::describe(&sqlx::Error::RowNotFound).class, "internal");

    // Через HTTP значение строки не попадает ни в тело, ни в журнал.
    captured();
    let h = harness(pool).await;
    let response = h
        .owner()
        .await
        .send(Method::GET, "/api/v1/_probe/dberr", None, None)
        .await;
    assert_error(&response, 500, "internal", "hub_db", "safe_same_key");
    assert!(!text(&response).contains("LEAK-DETAIL-MARKER"));
    assert!(!log_text().contains("LEAK-DETAIL-MARKER"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_without_upgrade_headers_uses_api_error_envelope(pool: PgPool) {
    let (deps, _directory) = test_app_deps(pool, test_http_config(), None).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let response = client.send(Method::GET, "/api/v1/ws", None, None).await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json();
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(response.headers["cache-control"], "no-store");
    assert!(response.headers.contains_key("x-server-time-ms"));
}
