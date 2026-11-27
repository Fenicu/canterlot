//! Коды привязки машины: выпуск, обмен по gRPC без токена машины, перевыпуск и отзыв,
//! первое рукопожатие снимает «ожидает подключения».

use std::{
    io::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_hub::{
    config::HubTimers,
    enroll::{self, EnrollError, EnrollSettings},
    grpc::{self, HubGrpc, PROTOCOL_VERSION},
    http::{self, AppState},
    installation::{self, Installation},
    machines,
    session::Registry,
    testkit::{self, Method, TestClient, tls::TestPki},
};
use canterlot_proto::{
    enroll::v1::{GetCaRequest, JoinRequest, worker_enroll_client::WorkerEnrollClient},
    worker::v1::*,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Code, Request,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

use crate::support::cases::run_cases;

const WAIT: Duration = Duration::from_secs(15);
const ENDPOINT: &str = "hub.example.internal:7443";
const INSTALL_URL: &str = "https://github.com/Fenicu/canterlot/releases/latest/download/install.sh";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Hub {
    pool: PgPool,
    installation: Installation,
    address: SocketAddr,
    pki: TestPki,
    ca_der: Vec<u8>,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
    _dir: tempfile::TempDir,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Hub {
    async fn start(pool: PgPool, with_ca: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let installation = installation::init(&pool, &dir.path().join("manifest"))
            .await
            .unwrap();
        let pki = testkit::tls::test_pki();
        let ca_der = enroll::ca_der_from_pem(&pki.ca_pem).expect("test CA in PEM");
        let timers = HubTimers::default();
        let state = HubGrpc::new(
            pool.clone(),
            Registry::new(timers.clone()),
            installation,
            timers,
        )
        .with_enroll(EnrollSettings {
            ca_der: with_ca.then(|| ca_der.clone()),
            grpc_endpoint: Some(ENDPOINT.into()),
            install_url: INSTALL_URL.into(),
        });
        let cert = pki.server_cert_pem.clone();
        let key = pki.server_key_pem.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            grpc::serve_with_listener(state, listener, &cert, &key, std::future::pending()).await
        });
        Self {
            pool,
            installation,
            address,
            pki,
            ca_der,
            task,
            _dir: dir,
        }
    }

    async fn channel(&self) -> Channel {
        Endpoint::from_shared(format!("https://{}", self.address))
            .unwrap()
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(&self.pki.ca_pem))
                    .domain_name("localhost"),
            )
            .unwrap()
            .connect_timeout(WAIT)
            .connect()
            .await
            .unwrap()
    }

    async fn enroll_client(&self) -> WorkerEnrollClient<Channel> {
        WorkerEnrollClient::new(self.channel().await)
    }

    async fn issue(&self, name: &str) -> enroll::JoinCode {
        enroll::issue(
            &self.pool,
            self.installation.owner_id,
            name,
            ENDPOINT,
            INSTALL_URL,
            &self.ca_der,
        )
        .await
        .unwrap()
    }

    async fn reissue(&self, machine_id: Uuid) -> Result<enroll::JoinCode, EnrollError> {
        enroll::reissue(
            &self.pool,
            self.installation.owner_id,
            machine_id,
            ENDPOINT,
            INSTALL_URL,
            &self.ca_der,
        )
        .await
    }

    async fn pending(&self, machine_id: Uuid) -> bool {
        sqlx::query_scalar("select pending_enrollment from machines where id = $1")
            .bind(machine_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Рукопожатие управляющего потока с токеном машины: `Ok` — принято.
    async fn handshake(&self, machine_id: Uuid, token: &str) -> Result<(), tonic::Status> {
        let hello = Hello {
            installation_id: self.installation.installation_id.to_string(),
            machine_id: machine_id.to_string(),
            restore_generation: self.installation.restore_generation.to_string(),
            protocol_version: PROTOCOL_VERSION,
            connection_id: Uuid::now_v7().to_string(),
            cj_journal_id: Uuid::now_v7().to_string(),
            boot_id: Uuid::now_v7().to_string(),
            stream_kind: StreamKind::Control as i32,
            ..Hello::default()
        };
        let (tx, rx) = mpsc::channel(8);
        tx.send(ControlUp {
            msg: Some(control_up::Msg::Hello(hello)),
        })
        .await
        .unwrap();
        let mut request = Request::new(ReceiverStream::new(rx));
        let mut credential = format!("Bearer {token}")
            .parse::<tonic::metadata::MetadataValue<_>>()
            .unwrap();
        credential.set_sensitive(true);
        request.metadata_mut().insert("authorization", credential);
        let mut client = worker_control_client::WorkerControlClient::new(self.channel().await);
        let mut stream = client.control(request).await?.into_inner();
        let reply = timeout(WAIT, stream.message())
            .await
            .expect("hello reply in time")?
            .expect("hello reply");
        match reply.msg {
            Some(control_down::Msg::HelloReply(HelloReply {
                result: Some(hello_reply::Result::Accepted(_)),
            })) => Ok(()),
            other => panic!("expected accepted handshake, got {other:?}"),
        }
    }
}

fn normalized(code: &str) -> String {
    code.replace('-', "").to_ascii_uppercase()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn join_codes_on_a_live_hub() {
    let pool = testkit::db().await;
    let hub = Hub::start(pool, true).await;
    run_cases!(&hub;
        issue_creates_pending_machine_and_hashed_code,
        redeem_is_single_use,
        redeem_ignores_case_dashes_and_spaces,
        redeem_expired_code_fails,
        reissue_invalidates_old_code_and_old_token,
        reissue_refuses_active_machine,
        first_handshake_clears_pending,
        revoke_pending_removes_machine,
        list_pending_shows_only_pending,
        grpc_get_ca_returns_configured_ca,
        grpc_join_returns_identity,
        grpc_join_rejects_unknown_code,
        join_code_never_logged,
    );
}

async fn issue_creates_pending_machine_and_hashed_code(hub: &Hub) -> TestResult {
    let issued = hub.issue("workstation").await;
    assert!(hub.pending(issued.machine_id).await);
    let credentials: i64 =
        sqlx::query_scalar("select count(*) from machine_credentials where machine_id = $1")
            .bind(issued.machine_id)
            .fetch_one(&hub.pool)
            .await?;
    assert_eq!(credentials, 0);
    // 26 знаков Crockford base32 группами по 4 через дефис.
    let groups: Vec<&str> = issued.code.split('-').collect();
    assert_eq!(groups.len(), 7, "{}", issued.code);
    assert!(groups[..6].iter().all(|group| group.len() == 4));
    assert_eq!(groups[6].len(), 2);
    let plain = normalized(&issued.code);
    assert!(
        plain
            .chars()
            .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c))
    );
    let stored: Vec<u8> =
        sqlx::query_scalar("select code_sha256 from machine_join_codes where machine_id = $1")
            .bind(issued.machine_id)
            .fetch_one(&hub.pool)
            .await?;
    assert_eq!(stored, Sha256::digest(plain.as_bytes()).to_vec());
    let left = issued.expires_at - chrono::Utc::now();
    assert!(left > chrono::TimeDelta::minutes(14) && left <= chrono::TimeDelta::minutes(15));
    let pin = enroll::pin_of(&hub.ca_der);
    assert_eq!(pin, format!("sha256:{}", hex(&Sha256::digest(&hub.ca_der))));
    assert_eq!(
        issued.command,
        format!("curl -fsSL {INSTALL_URL} | bash -s -- join --hub {ENDPOINT} --pin {pin}")
    );
    assert!(!issued.command.contains(&issued.code));
    for (table, query) in [
        (
            "machines",
            "select row_to_json(t)::text from machines t where id = $1",
        ),
        (
            "machine_join_codes",
            "select row_to_json(t)::text from machine_join_codes t where machine_id = $1",
        ),
    ] {
        let rows: Vec<String> = sqlx::query_scalar(query)
            .bind(issued.machine_id)
            .fetch_all(&hub.pool)
            .await?;
        for row in rows {
            for needle in [
                issued.code.clone(),
                plain.clone(),
                plain.to_ascii_lowercase(),
                hex(plain.as_bytes()),
            ] {
                assert!(!row.contains(&needle), "{table} keeps the plain code");
            }
        }
    }
    Ok(())
}

async fn redeem_is_single_use(hub: &Hub) -> TestResult {
    let issued = hub.issue("single-use").await;
    let redeemed = enroll::redeem(&hub.pool, &issued.code).await?;
    assert_eq!(redeemed.machine_id, issued.machine_id);
    assert_eq!(
        machines::verify(&hub.pool, &redeemed.token).await?,
        Some(issued.machine_id)
    );
    assert!(matches!(
        enroll::redeem(&hub.pool, &issued.code).await,
        Err(EnrollError::Invalid)
    ));
    Ok(())
}

async fn redeem_ignores_case_dashes_and_spaces(hub: &Hub) -> TestResult {
    let issued = hub.issue("typed-by-hand").await;
    let typed = format!(" {} ", issued.code.to_ascii_lowercase().replace('-', " "));
    let redeemed = enroll::redeem(&hub.pool, &typed).await?;
    assert_eq!(redeemed.machine_id, issued.machine_id);
    Ok(())
}

async fn redeem_expired_code_fails(hub: &Hub) -> TestResult {
    let issued = hub.issue("expired").await;
    sqlx::query(
        "update machine_join_codes set expires_at = now() - interval '1 second' where machine_id = $1",
    )
    .bind(issued.machine_id)
    .execute(&hub.pool)
    .await?;
    assert!(matches!(
        enroll::redeem(&hub.pool, &issued.code).await,
        Err(EnrollError::Invalid)
    ));
    Ok(())
}

async fn reissue_invalidates_old_code_and_old_token(hub: &Hub) -> TestResult {
    let first = hub.issue("lost-reply").await;
    let lost = enroll::redeem(&hub.pool, &first.code).await?;
    let second = hub.reissue(first.machine_id).await?;
    assert_eq!(second.machine_id, first.machine_id);
    assert_ne!(second.code, first.code);
    assert!(matches!(
        enroll::redeem(&hub.pool, &first.code).await,
        Err(EnrollError::Invalid)
    ));
    let fresh = enroll::redeem(&hub.pool, &second.code).await?;
    let rejected = hub
        .handshake(first.machine_id, &lost.token)
        .await
        .expect_err("the old token is gone");
    assert_eq!(rejected.code(), Code::Unauthenticated);
    hub.handshake(first.machine_id, &fresh.token).await?;
    Ok(())
}

async fn reissue_refuses_active_machine(hub: &Hub) -> TestResult {
    let issued = hub.issue("active").await;
    let redeemed = enroll::redeem(&hub.pool, &issued.code).await?;
    hub.handshake(issued.machine_id, &redeemed.token).await?;
    assert!(matches!(
        hub.reissue(issued.machine_id).await,
        Err(EnrollError::NotPending)
    ));
    assert!(matches!(
        enroll::revoke_pending(&hub.pool, hub.installation.owner_id, issued.machine_id).await,
        Err(EnrollError::NotPending)
    ));
    // Машина, добавленная `machine add`, никогда не ждала кода.
    let added = machines::add(&hub.pool, hub.installation.owner_id, "scripted").await?;
    assert!(matches!(
        hub.reissue(added.machine_id).await,
        Err(EnrollError::NotPending)
    ));
    assert!(matches!(
        hub.reissue(Uuid::now_v7()).await,
        Err(EnrollError::NotFound)
    ));
    Ok(())
}

async fn first_handshake_clears_pending(hub: &Hub) -> TestResult {
    let issued = hub.issue("first-handshake").await;
    let redeemed = enroll::redeem(&hub.pool, &issued.code).await?;
    assert!(
        hub.pending(issued.machine_id).await,
        "redemption alone keeps it pending"
    );
    hub.handshake(issued.machine_id, &redeemed.token).await?;
    assert!(!hub.pending(issued.machine_id).await);
    Ok(())
}

async fn revoke_pending_removes_machine(hub: &Hub) -> TestResult {
    let issued = hub.issue("revoked").await;
    let redeemed = enroll::redeem(&hub.pool, &issued.code).await?;
    let unused = hub.issue("revoked-unused").await;
    for machine_id in [issued.machine_id, unused.machine_id] {
        enroll::revoke_pending(&hub.pool, hub.installation.owner_id, machine_id).await?;
        let left: i64 = sqlx::query_scalar("select count(*) from machines where id = $1")
            .bind(machine_id)
            .fetch_one(&hub.pool)
            .await?;
        assert_eq!(left, 0);
    }
    assert_eq!(machines::verify(&hub.pool, &redeemed.token).await?, None);
    assert!(matches!(
        enroll::redeem(&hub.pool, &unused.code).await,
        Err(EnrollError::Invalid)
    ));
    assert!(matches!(
        enroll::revoke_pending(&hub.pool, hub.installation.owner_id, issued.machine_id).await,
        Err(EnrollError::NotFound)
    ));
    Ok(())
}

async fn list_pending_shows_only_pending(hub: &Hub) -> TestResult {
    let waiting = hub.issue("listed").await;
    let added = machines::add(&hub.pool, hub.installation.owner_id, "not-listed").await?;
    let pending = enroll::list_pending(&hub.pool, hub.installation.owner_id).await?;
    let listed = pending
        .iter()
        .find(|machine| machine.machine_id == waiting.machine_id)
        .expect("pending machine listed");
    assert_eq!(listed.name, "listed");
    assert_eq!(listed.code_expires_at, Some(waiting.expires_at));
    assert!(
        pending
            .iter()
            .all(|machine| machine.machine_id != added.machine_id)
    );
    Ok(())
}

async fn grpc_get_ca_returns_configured_ca(hub: &Hub) -> TestResult {
    let reply = hub.enroll_client().await.get_ca(GetCaRequest {}).await?;
    assert_eq!(reply.into_inner().ca_der, hub.ca_der);
    Ok(())
}

async fn grpc_join_returns_identity(hub: &Hub) -> TestResult {
    let issued = hub.issue("joined-over-grpc").await;
    let reply = hub
        .enroll_client()
        .await
        .join(JoinRequest {
            code: issued.code.clone(),
            hostname: "workstation".into(),
            worker_version: "1.0.0".into(),
        })
        .await?
        .into_inner();
    assert_eq!(reply.machine_id, issued.machine_id.to_string());
    assert_eq!(
        reply.installation_id,
        hub.installation.installation_id.to_string()
    );
    assert_eq!(
        reply.restore_generation,
        hub.installation.restore_generation.to_string()
    );
    assert_eq!(reply.ca_der, hub.ca_der);
    assert_eq!(reply.grpc_endpoint, ENDPOINT);
    assert_eq!(
        machines::verify(&hub.pool, &reply.token).await?,
        Some(issued.machine_id)
    );
    hub.handshake(issued.machine_id, &reply.token).await?;
    assert!(!hub.pending(issued.machine_id).await);
    Ok(())
}

async fn grpc_join_rejects_unknown_code(hub: &Hub) -> TestResult {
    let mut client = hub.enroll_client().await;
    for code in ["", "not a code", "0000-0000-0000-0000-0000-0000-00"] {
        let status = client
            .join(JoinRequest {
                code: code.into(),
                ..JoinRequest::default()
            })
            .await
            .expect_err("unknown code");
        assert_eq!(status.code(), Code::Unauthenticated);
        assert_eq!(status.message(), "join_code_invalid");
    }
    Ok(())
}

/// Буфер для захвата журнала: `fmt` пишет в него вместо stdout.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn join_code_never_logged(hub: &Hub) -> TestResult {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Тест идёт в однопоточном рантайме: сервер хаба пишет журнал в том же потоке.
    let guard = tracing::subscriber::set_default(subscriber);
    let issued = hub.issue("quiet").await;
    let reply = hub
        .enroll_client()
        .await
        .join(JoinRequest {
            code: issued.code.clone(),
            hostname: "workstation".into(),
            worker_version: "1.0.0".into(),
        })
        .await?
        .into_inner();
    drop(guard);
    let log = String::from_utf8(captured.0.lock().unwrap().clone())?;
    assert!(log.contains("join_code_issued"), "capture works: {log}");
    assert!(log.contains("machine_joined"), "capture works: {log}");
    let plain = normalized(&issued.code);
    for needle in [issued.code.as_str(), plain.as_str(), reply.token.as_str()] {
        assert!(!log.contains(needle), "secret leaked into the log");
    }
    Ok(())
}

#[tokio::test]
async fn grpc_get_ca_without_tls_ca_is_unavailable() {
    let pool = testkit::db().await;
    let hub = Hub::start(pool, false).await;
    let mut client = hub.enroll_client().await;
    let status = client
        .get_ca(GetCaRequest {})
        .await
        .expect_err("no CA configured");
    assert_eq!(status.message(), "ca_unavailable");
    let status = client
        .join(JoinRequest {
            code: "0000-0000-0000-0000-0000-0000-00".into(),
            ..JoinRequest::default()
        })
        .await
        .expect_err("no CA configured");
    assert_eq!(status.message(), "ca_unavailable");
}

#[tokio::test]
async fn grpc_join_rate_limited() {
    let pool = testkit::db().await;
    let hub = Hub::start(pool, true).await;
    let mut client = hub.enroll_client().await;
    for attempt in 1..=11 {
        let status = client
            .join(JoinRequest {
                code: "0000-0000-0000-0000-0000-0000-00".into(),
                ..JoinRequest::default()
            })
            .await
            .expect_err("unknown code");
        if attempt <= 10 {
            assert_eq!(status.message(), "join_code_invalid", "attempt {attempt}");
        } else {
            assert_eq!(status.code(), Code::ResourceExhausted);
            assert_eq!(status.message(), "rate_limited");
        }
    }
}

async fn owner_router(
    pool: PgPool,
    settings: EnrollSettings,
) -> (TestClient, Uuid, tempfile::TempDir) {
    let (mut deps, dir) = testkit::test_app_deps(pool, testkit::test_http_config(), None).await;
    deps.enroll = settings;
    let owner = deps.installation.owner_id;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    (client, owner, dir)
}

fn configured(ca_der: Vec<u8>) -> EnrollSettings {
    EnrollSettings {
        ca_der: Some(ca_der),
        grpc_endpoint: Some(ENDPOINT.into()),
        install_url: INSTALL_URL.into(),
    }
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn join_code_http_requires_fresh_session() {
    let pool = testkit::db().await;
    let pki = testkit::tls::test_pki();
    let ca_der = enroll::ca_der_from_pem(&pki.ca_pem).unwrap();
    let (client, owner, _dir) = owner_router(pool.clone(), configured(ca_der.clone())).await;

    let issued = client
        .send(
            Method::POST,
            "/api/v1/machines/join-codes",
            None,
            Some(json!({"name": "workstation"})),
        )
        .await;
    assert_eq!(issued.status, 200, "{:?}", issued.body);
    let issued: Value = issued.json();
    let machine_id = issued["machine_id"].as_str().unwrap().to_owned();
    let code = issued["code"].as_str().unwrap().to_owned();
    assert_eq!(
        issued["command"],
        format!(
            "curl -fsSL {INSTALL_URL} | bash -s -- join --hub {ENDPOINT} --pin {}",
            enroll::pin_of(&ca_der)
        )
    );
    assert!(issued["expires_at"].is_string());

    let pending = client
        .send(Method::GET, "/api/v1/machines/pending", None, None)
        .await;
    assert_eq!(pending.status, 200);
    let pending: Value = pending.json();
    assert_eq!(pending["items"][0]["machine_id"], machine_id.as_str());
    assert_eq!(pending["items"][0]["name"], "workstation");
    assert!(pending["items"][0]["code_expires_at"].is_string());
    assert!(
        !pending.to_string().contains(&code),
        "the list never shows the code"
    );

    let reissued = client
        .send(
            Method::POST,
            &format!("/api/v1/machines/{machine_id}/join-code"),
            None,
            None,
        )
        .await;
    assert_eq!(reissued.status, 200);
    let reissued: Value = reissued.json();
    assert_eq!(reissued["machine_id"], machine_id.as_str());
    assert_ne!(reissued["code"], code.as_str());
    assert!(matches!(
        enroll::redeem(&pool, &code).await,
        Err(EnrollError::Invalid)
    ));

    let missing = client
        .send(
            Method::POST,
            &format!("/api/v1/machines/{}/join-code", Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(error_code(&missing.json()), "machine_not_found");
    let active = machines::add(&pool, owner, "active").await.unwrap();
    let not_pending = client
        .send(
            Method::POST,
            &format!("/api/v1/machines/{}/join-code", active.machine_id),
            None,
            None,
        )
        .await;
    assert_eq!(not_pending.status, 409);
    assert_eq!(error_code(&not_pending.json()), "machine_not_pending");

    sqlx::query(
        "update owner_sessions set authenticated_at = now() - interval '11 minutes' where id = $1",
    )
    .bind(testkit::TEST_SESSION_ID)
    .execute(&pool)
    .await
    .unwrap();
    for (method, path, body) in [
        (
            Method::POST,
            "/api/v1/machines/join-codes".to_owned(),
            Some(json!({"name": "stale"})),
        ),
        (
            Method::POST,
            format!("/api/v1/machines/{machine_id}/join-code"),
            None,
        ),
        (
            Method::DELETE,
            format!("/api/v1/machines/{machine_id}/pending"),
            None,
        ),
    ] {
        let response = client.send(method, &path, None, body).await;
        assert_eq!(response.status, 403, "{path}");
        assert_eq!(error_code(&response.json()), "reauth_required", "{path}");
    }
    let names: Vec<String> = sqlx::query_scalar("select name from machines where owner_id = $1")
        .bind(owner)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(!names.iter().any(|name| name == "stale"));

    testkit::seed_test_session(&pool, owner).await;
    let revoked = client
        .send(
            Method::DELETE,
            &format!("/api/v1/machines/{machine_id}/pending"),
            None,
            None,
        )
        .await;
    assert_eq!(revoked.status, 204);
    let pending: Value = client
        .send(Method::GET, "/api/v1/machines/pending", None, None)
        .await
        .json();
    assert_eq!(pending["items"], json!([]));
}

#[tokio::test]
async fn join_code_http_without_enroll_settings() {
    let pool = testkit::db().await;
    let (client, _owner, _dir) = owner_router(pool, EnrollSettings::default()).await;
    let response = client
        .send(
            Method::POST,
            "/api/v1/machines/join-codes",
            None,
            Some(json!({"name": "workstation"})),
        )
        .await;
    assert_eq!(response.status, 409);
    assert_eq!(error_code(&response.json()), "ca_unavailable");
}
