use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_stream::{Stream, StreamExt, wrappers::TcpListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::{
    Request, Response, Status, Streaming,
    metadata::{Ascii, MetadataValue},
    transport::{
        Certificate, Channel, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig,
        server::{Connected, TcpConnectInfo},
    },
};
use uuid::Uuid;

pub mod wire {
    tonic::include_proto!("probe");
}

pub mod measurement;

use wire::{Down, Up, probe_client::ProbeClient, probe_server::Probe, probe_server::ProbeServer};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type ReplyStream = Pin<Box<dyn Stream<Item = Result<Down, Status>> + Send>>;
pub const MESSAGE_LIMIT: usize = 1_048_576;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
pub const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ProbeService {
    credential: MetadataValue<Ascii>,
    handshake_timeout: Duration,
    calls: Arc<AtomicUsize>,
}

impl ProbeService {
    #[allow(clippy::result_large_err)]
    fn open(
        &self,
        request: Request<Streaming<Up>>,
        echo: bool,
    ) -> Result<Response<ReplyStream>, Status> {
        if request.metadata().get("authorization") != Some(&self.credential) {
            return Err(Status::unauthenticated("invalid machine credential"));
        }
        let peer = request
            .remote_addr()
            .ok_or_else(|| Status::internal("missing peer address"))?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut inbound = request.into_inner();
        let handshake_timeout = self.handshake_timeout;
        let output = async_stream::try_stream! {
            tokio::time::timeout(handshake_timeout, inbound.message())
                .await
                .map_err(|_| Status::deadline_exceeded("stream handshake timed out"))??
                .ok_or_else(|| Status::invalid_argument("missing stream handshake"))?;
            yield Down::default();
            while let Some(up) = inbound.message().await? {
                if up.response_bytes as usize > MESSAGE_LIMIT {
                    Err(Status::resource_exhausted("requested response is too large"))?;
                }
                let payload = if up.response_bytes > 0 {
                    vec![0; up.response_bytes as usize]
                } else if echo {
                    up.payload
                } else {
                    Vec::new()
                };
                yield Down { sequence: up.sequence, payload };
            }
        };
        let mut response = Response::new(Box::pin(output) as ReplyStream);
        response.metadata_mut().insert(
            "x-probe-peer",
            peer.to_string()
                .parse()
                .map_err(|_| Status::internal("invalid peer address"))?,
        );
        Ok(response)
    }
}

#[tonic::async_trait]
impl Probe for ProbeService {
    type ControlStream = ReplyStream;
    type DataStream = ReplyStream;

    async fn control(
        &self,
        request: Request<Streaming<Up>>,
    ) -> Result<Response<ReplyStream>, Status> {
        self.open(request, false)
    }

    async fn data(&self, request: Request<Streaming<Up>>) -> Result<Response<ReplyStream>, Status> {
        self.open(request, true)
    }
}

pub struct ProbeHost {
    pub address: SocketAddr,
    pub ca_pem: String,
    credential: MetadataValue<Ascii>,
    calls: Arc<AtomicUsize>,
    disconnect: CancellationToken,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl ProbeHost {
    pub async fn start(handshake_timeout: Duration, message_limit: usize) -> Result<Self, Error> {
        let mut ca_params = CertificateParams::new(Vec::new())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate()?)?;
        let server_key = KeyPair::generate()?;
        let server_certificate =
            CertificateParams::new(vec!["localhost".into()])?.signed_by(&server_key, &ca)?;
        let identity = Identity::from_pem(server_certificate.pem(), server_key.serialize_pem());
        let mut credential: MetadataValue<Ascii> = format!("Bearer {}", Uuid::now_v7()).parse()?;
        credential.set_sensitive(true);
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ProbeServer::new(ProbeService {
            credential: credential.clone(),
            handshake_timeout,
            calls: calls.clone(),
        })
        .max_decoding_message_size(message_limit)
        .max_encoding_message_size(message_limit);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let disconnect = CancellationToken::new();
        let connection_cancel = disconnect.clone();
        let incoming = TcpListenerStream::new(listener).map(move |socket| {
            socket.map(|socket| InterruptibleIo {
                socket,
                cancelled: Box::pin(connection_cancel.clone().cancelled_owned()),
            })
        });
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(
            Server::builder()
                .tls_config(ServerTlsConfig::new().identity(identity))?
                .http2_keepalive_interval(Some(KEEPALIVE_INTERVAL))
                .http2_keepalive_timeout(Some(KEEPALIVE_TIMEOUT))
                .max_concurrent_streams(16)
                .add_service(service)
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = shutdown_rx.await;
                }),
        );
        Ok(Self {
            address,
            ca_pem: ca.pem(),
            credential,
            calls,
            disconnect,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub async fn channel(&self) -> Result<Channel, Error> {
        self.channel_with_ca(&self.ca_pem, "localhost").await
    }

    pub async fn channel_with_ca(&self, ca_pem: &str, domain: &str) -> Result<Channel, Error> {
        Ok(Endpoint::from_shared(format!("https://{}", self.address))?
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(ca_pem))
                    .domain_name(domain),
            )?
            .connect_timeout(Duration::from_secs(3))
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .buffer_size(8)
            .connect()
            .await?)
    }

    pub fn request<T>(&self, body: T) -> Request<T> {
        let mut request = Request::new(body);
        request
            .metadata_mut()
            .insert("authorization", self.credential.clone());
        request
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn begin_graceful_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    pub fn disconnect(&self) {
        self.disconnect.cancel();
    }
}

impl Drop for ProbeHost {
    fn drop(&mut self) {
        self.disconnect.cancel();
        self.task.abort();
    }
}

pub fn client(channel: Channel, message_limit: usize) -> ProbeClient<Channel> {
    ProbeClient::new(channel)
        .max_decoding_message_size(message_limit)
        .max_encoding_message_size(message_limit)
}

struct InterruptibleIo {
    socket: TcpStream,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Connected for InterruptibleIo {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.socket.connect_info()
    }
}

impl InterruptibleIo {
    fn check_connection(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "probe server stopped",
            ))
        } else {
            Ok(())
        }
    }
}

impl AsyncRead for InterruptibleIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check_connection(cx)?;
        Pin::new(&mut self.socket).poll_read(cx, buf)
    }
}

impl AsyncWrite for InterruptibleIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check_connection(cx)?;
        Pin::new(&mut self.socket).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check_connection(cx)?;
        Pin::new(&mut self.socket).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
