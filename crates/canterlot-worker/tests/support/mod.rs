use std::{fs, net::SocketAddr, os::unix::fs::PermissionsExt, time::Duration};

use canterlot_hub::testkit::tls::test_pki;
use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::v1::{
        worker_control_server::{WorkerControl, WorkerControlServer},
        worker_data_server::{WorkerData, WorkerDataServer},
        *,
    },
};
use canterlot_worker::{
    config::{WorkerConfig, WorkerTimers},
    store::WorkerStore,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Identity, Server, ServerTlsConfig},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Behavior {
    pub generation: Uuid,
    pub committed: u64,
    pub heartbeat_ack: bool,
    pub range_ack: bool,
    pub hello_reply: bool,
    pub command_on_first_batch: Option<Command>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            generation: Uuid::now_v7(),
            committed: 0,
            heartbeat_ack: true,
            range_ack: true,
            hello_reply: true,
            command_on_first_batch: None,
        }
    }
}

pub enum Observed {
    Hello(Hello, SocketAddr),
    Control(ControlUp),
}

#[derive(Clone)]
struct Service {
    behavior: Behavior,
    events: mpsc::Sender<Observed>,
}

impl Service {
    fn reply(&self, hello: &Hello) -> HelloReply {
        HelloReply {
            result: Some(hello_reply::Result::Accepted(Accepted {
                restore_generation: self.behavior.generation.to_string(),
                committed: vec![JournalCommitted {
                    journal_id: hello.cj_journal_id.clone(),
                    committed_seq: self.behavior.committed,
                }],
            })),
        }
    }

    fn check<T>(&self, request: &Request<T>) {
        assert!(
            request
                .metadata()
                .get("authorization")
                .is_some_and(|m| m == "Bearer protocol-test")
        );
    }
}

#[tonic::async_trait]
impl WorkerControl for Service {
    type ControlStream = ReceiverStream<Result<ControlDown, Status>>;
    async fn control(
        &self,
        request: Request<Streaming<ControlUp>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        self.check(&request);
        let peer = request.remote_addr().unwrap();
        let mut inbound = request.into_inner();
        let Some(control_up::Msg::Hello(hello)) = inbound.message().await?.and_then(|m| m.msg)
        else {
            panic!("missing control hello");
        };
        let (tx, rx) = mpsc::channel(8);
        let reply = self.reply(&hello);
        self.events
            .send(Observed::Hello(hello, peer))
            .await
            .unwrap();
        if self.behavior.hello_reply {
            tx.send(Ok(ControlDown {
                msg: Some(control_down::Msg::HelloReply(reply)),
            }))
            .await
            .unwrap();
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut command = service.behavior.command_on_first_batch.clone();
            loop {
                let message = tokio::select! {
                    _ = tx.closed() => break,
                    message = inbound.message() => match message { Ok(Some(message)) => message, _ => break },
                };
                let response = match &message.msg {
                    Some(control_up::Msg::Heartbeat(h)) if service.behavior.heartbeat_ack => {
                        Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq: h.seq }))
                    }
                    Some(control_up::Msg::CjRecords(batch)) if service.behavior.range_ack => {
                        Some(control_down::Msg::CjAck(JournalRangeAck {
                            journal_id: batch.journal_id.clone(),
                            up_to_seq: batch.records.last().unwrap().seq,
                        }))
                    }
                    _ => None,
                };
                if matches!(message.msg, Some(control_up::Msg::CjRecords(_)))
                    && let Some(command) = command.take()
                    && tx
                        .send(Ok(ControlDown {
                            msg: Some(control_down::Msg::Command(command)),
                        }))
                        .await
                        .is_err()
                {
                    break;
                }
                if service
                    .events
                    .send(Observed::Control(message))
                    .await
                    .is_err()
                {
                    break;
                }
                if let Some(msg) = response
                    && tx.send(Ok(ControlDown { msg: Some(msg) })).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tonic::async_trait]
impl WorkerData for Service {
    type DataStream = ReceiverStream<Result<DataDown, Status>>;
    async fn data(
        &self,
        request: Request<Streaming<DataUp>>,
    ) -> Result<Response<Self::DataStream>, Status> {
        self.check(&request);
        let peer = request.remote_addr().unwrap();
        let mut inbound = request.into_inner();
        let Some(data_up::Msg::Hello(hello)) = inbound.message().await?.and_then(|m| m.msg) else {
            panic!("missing data hello");
        };
        let reply = self.reply(&hello);
        self.events
            .send(Observed::Hello(hello, peer))
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(8);
        tx.send(Ok(DataDown {
            msg: Some(data_down::Msg::HelloReply(reply)),
        }))
        .await
        .unwrap();
        tokio::spawn(async move {
            tokio::select! {
                _ = tx.closed() => {},
                _ = async { while inbound.message().await.is_ok_and(|m| m.is_some()) {} } => {},
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

pub struct ProtocolHub {
    pub events: mpsc::Receiver<Observed>,
    _directory: TempDir,
    pub config: WorkerConfig,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl ProtocolHub {
    pub async fn start(behavior: Behavior) -> (Self, WorkerStore) {
        let directory = TempDir::new().unwrap();
        let pki = test_pki();
        let ca_cert = directory.path().join("ca.pem");
        fs::write(&ca_cert, &pki.ca_pem).unwrap();
        let token_file = directory.path().join("token");
        fs::write(&token_file, "protocol-test").unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (events, receiver) = mpsc::channel(64);
        let restore_generation = behavior.generation;
        let service = Service { behavior, events };
        let task = tokio::spawn(async move {
            Server::builder()
                .tls_config(
                    ServerTlsConfig::new()
                        .identity(Identity::from_pem(pki.server_cert_pem, pki.server_key_pem)),
                )?
                .add_service(
                    WorkerControlServer::new(service.clone())
                        .max_encoding_message_size(MAX_MESSAGE_BYTES)
                        .max_decoding_message_size(MAX_MESSAGE_BYTES),
                )
                .add_service(
                    WorkerDataServer::new(service)
                        .max_encoding_message_size(MAX_MESSAGE_BYTES)
                        .max_decoding_message_size(MAX_MESSAGE_BYTES),
                )
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
        });
        let config = WorkerConfig {
            hub_url: format!("https://localhost:{}", address.port()),
            ca_cert,
            token_file,
            machine_id: Uuid::now_v7(),
            installation_id: Uuid::now_v7(),
            restore_generation,
            data_dir: directory.path().join("store"),
            timers: WorkerTimers {
                handshake: Duration::from_millis(300),
                heartbeat: Duration::from_millis(30),
                heartbeat_ack: Duration::from_millis(70),
                range_ack: Duration::from_millis(90),
                backoff_min: Duration::from_millis(250),
                backoff_max: Duration::from_millis(500),
                ..Default::default()
            },
        };
        let store = WorkerStore::open(&config.data_dir).unwrap();
        (
            Self {
                events: receiver,
                _directory: directory,
                config,
                task,
            },
            store,
        )
    }

    pub async fn event(&mut self) -> Observed {
        tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for ProtocolHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}
