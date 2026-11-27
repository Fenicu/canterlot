//! Общее для тестов машины воркера: локальный сервер релизов, заглушка `WorkerEnroll`
//! с УЦ из теста и `WorkerHost` без терминала и без пауз.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use canterlot_ctl::{
    release::ReleaseSource,
    tls::{Ca, issue_hub_cert},
    worker::WorkerHost,
};
use canterlot_proto::enroll::v1::{
    GetCaRequest, GetCaResponse, JoinRequest, JoinResponse,
    worker_enroll_server::{WorkerEnroll, WorkerEnrollServer},
};
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use tonic::{
    Request, Response as TonicResponse, Status,
    transport::{Identity, Server, ServerTlsConfig},
};

pub const MACHINE: &str = "00000000-0000-7000-8000-000000000005";
pub const INSTALLATION: &str = "00000000-0000-7000-8000-000000000001";
pub const GENERATION: &str = "00000000-0000-7000-8000-000000000002";
pub const TOKEN: &str = "test-machine-token";

pub fn host() -> WorkerHost {
    WorkerHost {
        user: "worker".into(),
        hostname: "machine-b".into(),
        is_root: false,
        interactive: false,
        self_exe: None,
        in_unit: true,
        polls: 3,
        poll_delay: Duration::ZERO,
        hub_wait_polls: 4,
        hub_wait_delay: Duration::ZERO,
        answer: None,
    }
}

pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

type Bodies = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

async fn serve(State(bodies): State<Bodies>, uri: Uri) -> Response {
    // Путь `/<repo>/releases/download/v<версия>/<имя>`: ключ — `<версия>/<имя>`.
    let parts: Vec<&str> = uri.path().rsplitn(3, '/').collect();
    let key = match parts.as_slice() {
        [name, tag, _] => format!("{}/{name}", tag.trim_start_matches('v')),
        _ => String::new(),
    };
    match bodies.lock().unwrap().get(&key).cloned() {
        Some(bytes) => bytes.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Архивы воркера и утилиты для каждой версии, с верным `SHA256SUMS`.
pub fn release_files(version: &str) -> Vec<(String, Vec<u8>)> {
    let files = vec![
        (
            format!("canterlot-worker-{version}-x86_64-linux-gnu.tar.gz"),
            tar_gz(&[("canterlot-worker", format!("worker {version}").as_bytes())]),
        ),
        (
            format!("canterlot-{version}-x86_64-linux.tar.gz"),
            tar_gz(&[("canterlot", format!("ctl {version}").as_bytes())]),
        ),
    ];
    let mut sums = String::new();
    for (name, body) in &files {
        let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(body)));
    }
    let mut all = files;
    all.push(("SHA256SUMS".into(), sums.into_bytes()));
    all
}

/// Каталог выпуска в форме `--from-dir`: архивы и `SHA256SUMS` из того же набора.
pub fn release_dir(dir: &Path, version: &str) {
    for (name, body) in release_files(version) {
        std::fs::write(dir.join(name), body).unwrap();
    }
    std::fs::write(dir.join("RELEASE"), format!("version={version}\n")).unwrap();
}

pub async fn release_server(versions: &[&str]) -> ReleaseSource {
    let bodies: Bodies = Arc::default();
    for version in versions {
        for (name, body) in release_files(version) {
            bodies
                .lock()
                .unwrap()
                .insert(format!("{version}/{name}"), body);
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(get(serve)).with_state(bodies);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    ReleaseSource {
        api_base: format!("http://{addr}/").parse().unwrap(),
        download_base: format!("http://{addr}/").parse().unwrap(),
        repo: "Fenicu/canterlot".into(),
    }
}

#[derive(Clone)]
struct Enroll {
    served_ca: Vec<u8>,
    join_called: Arc<AtomicBool>,
    endpoint: String,
}

#[tonic::async_trait]
impl WorkerEnroll for Enroll {
    async fn get_ca(
        &self,
        _request: Request<GetCaRequest>,
    ) -> Result<TonicResponse<GetCaResponse>, Status> {
        Ok(TonicResponse::new(GetCaResponse {
            ca_der: self.served_ca.clone(),
        }))
    }

    async fn join(
        &self,
        request: Request<JoinRequest>,
    ) -> Result<TonicResponse<JoinResponse>, Status> {
        self.join_called.store(true, Ordering::SeqCst);
        if request.into_inner().code != "good-code" {
            return Err(Status::unauthenticated("join_code_invalid"));
        }
        Ok(TonicResponse::new(JoinResponse {
            machine_id: MACHINE.into(),
            installation_id: INSTALLATION.into(),
            restore_generation: GENERATION.into(),
            token: TOKEN.into(),
            ca_der: self.served_ca.clone(),
            grpc_endpoint: self.endpoint.clone(),
        }))
    }
}

pub struct Stub {
    pub addr: SocketAddr,
    pub join_called: Arc<AtomicBool>,
}

impl Stub {
    pub fn joined(&self) -> bool {
        self.join_called.load(Ordering::SeqCst)
    }
}

/// `served` отдаётся в `GetCa`; сертификат сервера подписан `signer` и называет `names`.
pub async fn enroll_stub(served: &Ca, signer: &Ca, names: &[&str], endpoint: &str) -> Stub {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let (cert, key) = issue_hub_cert(signer, &names).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let join_called = Arc::new(AtomicBool::new(false));
    let service = Enroll {
        served_ca: served.cert_der.clone(),
        join_called: join_called.clone(),
        endpoint: endpoint.into(),
    };
    let server = Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))
        .unwrap()
        .add_service(WorkerEnrollServer::new(service));
    tokio::spawn(async move {
        server
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    Stub { addr, join_called }
}

pub fn home_of(root: &std::path::Path) -> PathBuf {
    let home = root.join("home/worker");
    std::fs::create_dir_all(&home).unwrap();
    home
}
