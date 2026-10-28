use sqlx::{PgPool, types::Json};
use uuid::Uuid;

use crate::model::HoldReason;

pub const TEST_OWNER_LOGIN: &str = "owner";
pub const TEST_ORIGIN: &str = "https://chat.test";

pub struct TestHub {
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
    pub addr: std::net::SocketAddr,
    pub http_addr: Option<std::net::SocketAddr>,
    pub app: Option<crate::http::AppState>,
    pub pki: tls::TestPki,
    pub installation: crate::installation::Installation,
    pub registry: crate::session::Registry,
    pub dispatcher: crate::dispatcher::Dispatcher,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    pub join: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestHub {
    pub fn http_addr(&self) -> std::net::SocketAddr {
        self.http_addr.expect("the test hub was started with HTTP")
    }

    pub fn app(&self) -> &crate::http::AppState {
        self.app
            .as_ref()
            .expect("the test hub was started with HTTP")
    }
}

/// Конфигурация HTTP для тестов: доверен только 127.0.0.1, владелец `owner`.
pub fn test_http_config() -> crate::config::HttpConfig {
    crate::config::HttpConfig {
        trusted_proxies: vec!["127.0.0.1/32".parse().expect("test proxy network")],
        owner_login: TEST_OWNER_LOGIN.into(),
        allowed_origins: vec![TEST_ORIGIN.into()],
        ..Default::default()
    }
}

/// Установка с манифестом во временном каталоге; каталог должен жить, пока жив хаб.
async fn test_installation(
    pool: &PgPool,
) -> (
    crate::installation::Installation,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    use crate::installation;
    let directory = tempfile::tempdir().expect("test installation directory");
    let manifest = directory.path().join("manifest");
    let existing: Option<installation::Installation> =
        sqlx::query_as("select installation_id, restore_generation, owner_id from installation")
            .fetch_optional(pool)
            .await
            .expect("test installation lookup");
    if let Some(existing) = existing {
        std::fs::write(&manifest, format!(
            "installation_id = \"{}\"\nrestore_generation = \"{}\"\nrestore_in_progress = false\n",
            existing.installation_id, existing.restore_generation,
        )).expect("test manifest");
    } else {
        installation::init(pool, &manifest)
            .await
            .expect("test installation init");
    }
    let installation = installation::check(pool, &manifest)
        .await
        .expect("test installation check");
    (installation, manifest, directory)
}

/// Зависимости HTTP-слоя для тестов, которые собирают `AppState` сами.
pub async fn test_app_deps(
    pool: PgPool,
    http: crate::config::HttpConfig,
    proxy_secret: Option<Vec<u8>>,
) -> (crate::http::AppDeps, tempfile::TempDir) {
    let (installation, manifest, directory) = test_installation(&pool).await;
    let (attachments, blobs) = test_blob_store(directory.path());
    let timers = crate::config::HubTimers::default();
    let (dispatcher, _rx) = crate::dispatcher::Dispatcher::channel();
    let scheduler =
        crate::scheduler::Scheduler::new(pool.clone(), dispatcher.clone(), Default::default());
    let deps = crate::http::AppDeps {
        pool,
        registry: crate::session::Registry::new(timers),
        installation,
        dispatcher,
        scheduler,
        http,
        proxy_secret,
        limits: Default::default(),
        activity: Default::default(),
        machines: Default::default(),
        attachments,
        blobs,
        manifest,
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    (deps, directory)
}

/// Хранилище вложений во временном каталоге теста.
pub fn test_blob_store(
    directory: &std::path::Path,
) -> (
    crate::config::AttachmentsConfig,
    std::sync::Arc<dyn crate::attachments::BlobStore>,
) {
    let config = crate::config::AttachmentsConfig {
        blob_dir: directory.join("attachments"),
        ..Default::default()
    };
    let store = crate::attachments::LocalBlobStore::open(
        &config,
        std::sync::Arc::new(crate::attachments::NoTrace),
    )
    .expect("test blob store");
    (config, std::sync::Arc::new(store))
}

pub async fn spawn_test_hub(pool: PgPool, timers: crate::config::HubTimers) -> TestHub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    spawn_on(
        pool,
        timers,
        listener,
        tls::test_pki(),
        None,
        None,
        #[cfg(feature = "fault-injection")]
        Default::default(),
    )
    .await
}

/// Хаб с gRPC и HTTP на случайных портах loopback; HTTP использует `http`.
pub async fn spawn_test_hub_http(
    pool: PgPool,
    timers: crate::config::HubTimers,
    http: crate::config::HttpConfig,
) -> TestHub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    spawn_on(
        pool,
        timers,
        listener,
        tls::test_pki(),
        Some(http),
        None,
        #[cfg(feature = "fault-injection")]
        Default::default(),
    )
    .await
}

#[cfg(feature = "fault-injection")]
pub async fn spawn_test_hub_with_faults(
    pool: PgPool,
    timers: crate::config::HubTimers,
    addr: Option<std::net::SocketAddr>,
    pki: tls::TestPki,
    faults: crate::fault_injection::Faults,
) -> TestHub {
    let listener =
        tokio::net::TcpListener::bind(addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
            .await
            .expect("test listener");
    spawn_on(pool, timers, listener, pki, None, None, faults).await
}

#[cfg(feature = "fault-injection")]
pub async fn spawn_test_hub_with_attachment_store(
    pool: PgPool,
    timers: crate::config::HubTimers,
    addr: Option<std::net::SocketAddr>,
    pki: tls::TestPki,
    faults: crate::fault_injection::Faults,
    attachments: (
        crate::config::AttachmentsConfig,
        std::sync::Arc<dyn crate::attachments::BlobStore>,
    ),
) -> TestHub {
    let listener =
        tokio::net::TcpListener::bind(addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
            .await
            .expect("test listener");
    spawn_on(pool, timers, listener, pki, None, Some(attachments), faults).await
}

async fn spawn_on(
    pool: PgPool,
    timers: crate::config::HubTimers,
    listener: tokio::net::TcpListener,
    pki: tls::TestPki,
    http: Option<crate::config::HttpConfig>,
    attachment_store: Option<(
        crate::config::AttachmentsConfig,
        std::sync::Arc<dyn crate::attachments::BlobStore>,
    )>,
    #[cfg(feature = "fault-injection")] faults: crate::fault_injection::Faults,
) -> TestHub {
    use crate::{
        background::{Background, BackgroundDeps},
        dispatcher, grpc, ingest,
        session::Registry,
    };
    let (installation, manifest, directory) = test_installation(&pool).await;
    let (attachments, blobs) =
        attachment_store.unwrap_or_else(|| test_blob_store(directory.path()));
    let registry = Registry::new(timers.clone());
    let (dispatcher, dispatch_rx) = dispatcher::Dispatcher::channel();
    let mut state =
        grpc::HubGrpc::new(pool.clone(), registry.clone(), installation, timers.clone())
            .with_blob_store(blobs.clone());
    let ingest = ingest::Ingest::new(pool.clone())
        .with_dispatcher(dispatcher.clone())
        .with_registry(registry.clone())
        .with_limits(Default::default());
    #[cfg(feature = "fault-injection")]
    let ingest = {
        state.faults = faults.clone();
        let mut ingest = ingest;
        ingest.faults = faults.clone();
        ingest
    };
    state.ingest = std::sync::Arc::new(ingest);
    let cert = pki.server_cert_pem.clone();
    let key = pki.server_key_pem.clone();
    let addr = listener.local_addr().expect("test address");
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let root = tokio_util::sync::CancellationToken::new();
    let scheduler =
        crate::scheduler::Scheduler::new(pool.clone(), dispatcher.clone(), Default::default())
            .with_limits(registry.clone(), Default::default());
    #[cfg(feature = "fault-injection")]
    let scheduler = {
        let mut scheduler = scheduler;
        scheduler.faults = faults.clone();
        scheduler
    };
    let background = Background::start(BackgroundDeps {
        pool: pool.clone(),
        registry: registry.clone(),
        timers,
        dispatch_rx,
        scheduler: scheduler.clone(),
        activity: Default::default(),
        machines: Default::default(),
        multi_process: http.as_ref().is_some_and(|config| config.multi_process),
        attachments: Some((blobs.clone(), attachments.clone())),
    });
    let (http_addr, app, http_listener) = match http {
        Some(config) => {
            let secret = config
                .proxy_secret_file
                .as_deref()
                .map(|path| crate::http::load_proxy_secret(path).expect("test proxy secret"));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("test http listener");
            let app = crate::http::AppState::new(crate::http::AppDeps {
                pool: pool.clone(),
                registry: registry.clone(),
                installation,
                dispatcher: dispatcher.clone(),
                scheduler,
                http: config,
                proxy_secret: secret,
                limits: Default::default(),
                activity: Default::default(),
                machines: Default::default(),
                attachments,
                blobs,
                manifest,
                shutdown: root.clone(),
            });
            (
                Some(listener.local_addr().expect("test http address")),
                Some(app),
                Some(listener),
            )
        }
        None => (None, None, None),
    };
    let http_app = app.clone();
    let root_for_http = root.clone();
    let join = tokio::spawn(async move {
        let _manifest_dir = directory;
        let trigger = root.clone();
        tokio::spawn(async move {
            let _ = stop.await;
            trigger.cancel();
        });
        #[cfg(feature = "fault-injection")]
        let serve = grpc::serve_test_with_goaway;
        #[cfg(not(feature = "fault-injection"))]
        let serve = grpc::serve_with_listener;
        let grpc_root = root.clone();
        let grpc_serving = async move {
            let result = serve(state, listener, &cert, &key, root.clone().cancelled_owned()).await;
            grpc_root.cancel();
            result
        };
        let http_root = root_for_http;
        let http_serving = async move {
            if let (Some(app), Some(listener)) = (http_app, http_listener) {
                crate::http::serve(app, listener, http_root.clone().cancelled_owned())
                    .await
                    .expect("test http server");
                http_root.cancel();
            }
        };
        let (result, ()) = tokio::join!(grpc_serving, http_serving);
        background.stop().await.expect("test background services");
        result
    });
    TestHub {
        #[cfg(feature = "fault-injection")]
        faults,
        addr,
        http_addr,
        app,
        pki,
        installation,
        registry,
        dispatcher,
        shutdown,
        join,
    }
}

pub mod tls {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

    #[derive(Clone)]
    pub struct TestPki {
        pub ca_pem: String,
        pub server_cert_pem: String,
        pub server_key_pem: String,
    }

    pub fn test_pki() -> TestPki {
        let mut params = CertificateParams::new(Vec::new()).expect("CA parameters");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("CA key"))
            .expect("CA certificate");
        let key = KeyPair::generate().expect("server key");
        let cert = CertificateParams::new(vec!["localhost".into()])
            .expect("server parameters")
            .signed_by(&key, &ca)
            .expect("server certificate");
        TestPki {
            ca_pem: ca.pem(),
            server_cert_pem: cert.pem(),
            server_key_pem: key.serialize_pem(),
        }
    }
}

/// Сеанс машины только в Registry, без gRPC: команды вниз читаются из `commands`.
pub struct FakeSession {
    pub commands: tokio::sync::mpsc::Receiver<canterlot_proto::worker::v1::ControlDown>,
    pub lease: FakeLease,
}

/// Пока жива, машина остаётся в Registry.
pub struct FakeLease {
    _control: crate::session::ControlLease,
    _data: Option<crate::session::DataLease>,
}

/// `data = false` оставляет машину `degraded`; часы машины совпадают с часами хаба.
pub fn fake_session(
    registry: &crate::session::Registry,
    machine_id: Uuid,
    capabilities: &[&str],
    data: bool,
) -> FakeSession {
    let connection = Uuid::now_v7();
    let (control, commands) = registry
        .register(machine_id, connection, Vec::new())
        .expect("free test machine slot");
    let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
    registry.mark_accepted(machine_id, connection, &capabilities);
    let data = data.then(|| {
        registry
            .attach_data(machine_id, connection, &[])
            .expect("test data lease")
    });
    let now = chrono::Utc::now();
    registry.observe_clock(machine_id, now.timestamp_millis() as u64, now);
    FakeSession {
        commands,
        lease: FakeLease {
            _control: control,
            _data: data,
        },
    }
}

pub struct Seed {
    pub owner_id: Uuid,
    pub machine_id: Uuid,
    pub conversation_id: Uuid,
}

impl Seed {
    pub fn credential_digest(&self) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.machine_id.as_bytes()).to_vec()
    }
}

pub async fn seed(pool: &PgPool) -> Seed {
    let owner_id = Uuid::now_v7();
    let machine_id = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed owner");
    sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, 'synthetic')")
        .bind(machine_id)
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed machine");
    use sha2::{Digest, Sha256};
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine_id)
        .bind(Sha256::digest(machine_id.as_bytes()).as_slice())
        .execute(pool)
        .await
        .expect("seed credential");
    let conversation_id = seed_conversation(pool, owner_id, machine_id).await;
    Seed {
        owner_id,
        machine_id,
        conversation_id,
    }
}

pub async fn seed_conversation(pool: &PgPool, owner_id: Uuid, machine_id: Uuid) -> Uuid {
    let conversation_id = Uuid::now_v7();
    let mut transaction = pool.begin().await.expect("begin seed conversation");
    sqlx::query(
        "insert into conversations (id, owner_id, machine_id, harness, workdir)
         values ($1, $2, $3, 'synthetic', '/tmp/w')",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .bind(machine_id)
    .execute(&mut *transaction)
    .await
    .expect("seed conversation");
    sqlx::query("insert into queue_state (conversation_id, hold_reasons) values ($1, $2)")
        .bind(conversation_id)
        .bind(Json([HoldReason::NoLiveExecution]))
        .execute(&mut *transaction)
        .await
        .expect("seed queue state");
    transaction
        .commit()
        .await
        .expect("commit seed conversation");
    conversation_id
}

pub async fn seed_catalog(pool: &PgPool, machine_id: Uuid) {
    let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id=$1")
        .bind(machine_id)
        .fetch_one(pool)
        .await
        .expect("catalog machine");
    let report = canterlot_proto::worker::v1::MachineReport {
        worker_version: "test".into(),
        allowed_roots: vec!["/tmp".into()],
        containment: "process_group".into(),
        os: "linux".into(),
        max_attachment_bytes: canterlot_proto::limits::MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: canterlot_proto::limits::MAX_ATTACHMENTS_PER_INPUT as u32,
        harnesses: vec![canterlot_proto::worker::v1::HarnessInfo {
            harness: "synthetic".into(),
            provider: "claude".into(),
            title: "Тестовый".into(),
            images: true,
            settings_midsession: true,
            permission_modes: vec!["bypass".into()],
            models: ["synthetic-small", "synthetic-large"]
                .into_iter()
                .map(|id| canterlot_proto::worker::v1::ModelInfo {
                    id: id.into(),
                    title: "Тестовая модель".into(),
                    efforts: vec!["low".into(), "high".into()],
                })
                .collect(),
        }],
        profiles: vec![canterlot_proto::worker::v1::InstructionProfile {
            name: "owner".into(),
            title: "Как у владельца".into(),
            description: String::new(),
            harnesses: vec!["synthetic".into()],
        }],
    };
    crate::machine_info::apply_report(pool, owner, machine_id, &report, Uuid::now_v7())
        .await
        .expect("seed catalog");
}

use axum::{
    Router,
    body::{Body, Bytes},
    extract::ConnectInfo,
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl TestResponse {
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).expect("response body is JSON")
    }
}

#[derive(Clone)]
enum Target {
    InProcess {
        router: Router,
        peer: Option<std::net::SocketAddr>,
    },
    Socket {
        addr: std::net::SocketAddr,
        source: Option<std::net::IpAddr>,
    },
}

/// HTTP-клиент для тестов: напрямую в роутер (адрес собеседника задаётся) или по настоящему сокету
/// (исходный адрес можно выбрать из 127.0.0.0/8).
#[derive(Clone)]
pub struct TestClient {
    target: Target,
    headers: Vec<(HeaderName, HeaderValue)>,
}

impl TestClient {
    pub fn in_process(router: Router) -> Self {
        Self {
            target: Target::InProcess {
                router,
                peer: Some("127.0.0.1:40000".parse().expect("test peer")),
            },
            headers: Vec::new(),
        }
    }

    pub async fn over_socket(addr: std::net::SocketAddr) -> Self {
        Self {
            target: Target::Socket { addr, source: None },
            headers: Vec::new(),
        }
    }

    /// Добавляет заголовок; повторное имя даёт второе значение, а не замену.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((
            HeaderName::from_bytes(name.as_bytes()).expect("test header name"),
            HeaderValue::from_bytes(value.as_bytes()).expect("test header value"),
        ));
        self
    }

    /// Заголовки, с которыми прокси пропускает владельца: личность, Origin и CSRF-заголовок.
    pub fn as_owner(self) -> Self {
        self.with_header("Remote-User", TEST_OWNER_LOGIN)
            .with_header("Origin", TEST_ORIGIN)
            .with_header("X-Canterlot-Client", "1")
    }

    /// Адрес собеседника для запросов напрямую в роутер.
    pub fn with_peer(mut self, peer: std::net::SocketAddr) -> Self {
        if let Target::InProcess { peer: slot, .. } = &mut self.target {
            *slot = Some(peer);
        }
        self
    }

    /// Запрос без `ConnectInfo`, как при неверной сборке сервера.
    pub fn without_peer(mut self) -> Self {
        if let Target::InProcess { peer: slot, .. } = &mut self.target {
            *slot = None;
        }
        self
    }

    /// Исходный адрес сокета клиента.
    pub fn from_source(mut self, source: std::net::IpAddr) -> Self {
        if let Target::Socket { source: slot, .. } = &mut self.target {
            *slot = Some(source);
        }
        self
    }

    pub async fn send(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> TestResponse {
        let bytes = body.map(|body| serde_json::to_vec(&body).expect("test body"));
        self.send_with(method, path, key, bytes).await
    }

    /// Тело как есть, с типом `application/json`.
    pub async fn send_raw(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Vec<u8>,
    ) -> TestResponse {
        self.send_with(method, path, key, Some(body)).await
    }

    async fn send_with(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        if let Some(key) = key {
            builder = builder.header(
                "idempotency-key",
                HeaderValue::from_bytes(key.as_bytes()).expect("test key"),
            );
        }
        let payload = match body {
            Some(bytes) => {
                builder = builder.header("content-type", "application/json");
                Body::from(bytes)
            }
            None => Body::empty(),
        };
        self.dispatch(builder.body(payload).expect("test request"), path)
            .await
    }

    /// Произвольное тело (например, поток для загрузки вложения) и дополнительные заголовки.
    pub async fn send_body(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, String)],
        body: Body,
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        for (name, value) in headers {
            builder = builder.header(
                HeaderName::from_bytes(name.as_bytes()).expect("test header name"),
                HeaderValue::from_str(value).expect("test header value"),
            );
        }
        self.dispatch(builder.body(body).expect("test request"), path)
            .await
    }

    async fn dispatch(&self, mut request: Request<Body>, path: &str) -> TestResponse {
        match &self.target {
            Target::InProcess { router, peer } => {
                use tower::ServiceExt;
                if let Some(peer) = peer {
                    request.extensions_mut().insert(ConnectInfo(*peer));
                }
                let response = router
                    .clone()
                    .oneshot(request)
                    .await
                    .expect("infallible router");
                let (parts, body) = response.into_parts();
                TestResponse {
                    status: parts.status,
                    headers: parts.headers,
                    body: axum::body::to_bytes(body, usize::MAX)
                        .await
                        .expect("response body"),
                }
            }
            Target::Socket { addr, source } => {
                use http_body_util::BodyExt;
                use hyper_util::{
                    client::legacy::{Client, connect::HttpConnector},
                    rt::TokioExecutor,
                };
                let mut connector = HttpConnector::new();
                connector.set_local_address(*source);
                let client: Client<_, Body> = Client::builder(TokioExecutor::new())
                    .pool_max_idle_per_host(0)
                    .build(connector);
                let (mut parts, body) = request.into_parts();
                parts.uri = format!("http://{addr}{path}").parse().expect("test uri");
                parts
                    .headers
                    .entry("host")
                    .or_insert(HeaderValue::from_str(&addr.to_string()).expect("test host"));
                let response = client
                    .request(Request::from_parts(parts, body))
                    .await
                    .expect("test socket request");
                let (parts, body) = response.into_parts();
                TestResponse {
                    status: parts.status,
                    headers: parts.headers,
                    body: body.collect().await.expect("response body").to_bytes(),
                }
            }
        }
    }
}

pub struct TestWs {
    pub stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    pub close_code: Option<u16>,
}
impl TestWs {
    pub async fn connect(
        addr: std::net::SocketAddr,
        origin: &str,
        user: Option<&str>,
    ) -> Result<Self, tokio_tungstenite::tungstenite::Error> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://{addr}/api/v1/ws").into_client_request()?;
        request.headers_mut().insert("origin", origin.parse()?);
        request
            .headers_mut()
            .insert("sec-websocket-protocol", "canterlot.v1".parse()?);
        if let Some(user) = user {
            request.headers_mut().insert("remote-user", user.parse()?);
        }
        let socket = tokio::net::TcpStream::connect(addr).await?;
        let (stream, response) = tokio_tungstenite::client_async(request, socket).await?;
        assert_eq!(response.headers()["sec-websocket-protocol"], "canterlot.v1");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(response.headers().contains_key("x-server-time-ms"));
        Ok(Self {
            stream,
            close_code: None,
        })
    }
    pub async fn send(&mut self, value: Value) {
        use futures_util::SinkExt;
        assert!(
            self.stream
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    value.to_string().into()
                ))
                .await
                .is_ok(),
            "WS send"
        );
    }
    pub async fn next(&mut self, timeout: std::time::Duration) -> Option<Value> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        tokio::time::timeout(timeout, async {
            loop {
                match self.stream.next().await {
                    Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).ok(),
                    Some(Ok(Message::Close(frame))) => {
                        self.close_code = frame.map(|f| f.code.into());
                        let _ = self.stream.flush().await;
                        return None;
                    }
                    Some(Ok(Message::Ping(_))) => {
                        let _ = self.stream.flush().await;
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
    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }
}
