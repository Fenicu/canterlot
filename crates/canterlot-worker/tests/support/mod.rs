use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

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

#[cfg(feature = "fault-injection")]
pub mod data_proxy;

type Downstream = Arc<Mutex<Option<mpsc::Sender<Result<ControlDown, Status>>>>>;
type DataDownstream = Arc<Mutex<Option<mpsc::Sender<Result<DataDown, Status>>>>>;

#[derive(Clone)]
pub struct Behavior {
    pub generation: Uuid,
    pub capabilities: Vec<String>,
    pub committed: u64,
    pub heartbeat_ack: bool,
    pub range_ack: bool,
    pub sj_ack: bool,
    pub hello_reply: bool,
    pub command_on_first_batch: Option<Command>,
    pub commands_on_first_batch: Vec<Command>,
    pub range_ack_delay: Duration,
    pub pause_after_first_batch: Duration,
    pub ack_override: Option<u64>,
    pub status: Option<(tonic::Code, &'static str)>,
    pub fetch_failures: Arc<Mutex<std::collections::VecDeque<tonic::Code>>>,
    pub attachment: Vec<u8>,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            generation: Uuid::now_v7(),
            capabilities: vec![],
            committed: 0,
            heartbeat_ack: true,
            range_ack: true,
            sj_ack: true,
            hello_reply: true,
            command_on_first_batch: None,
            commands_on_first_batch: vec![],
            range_ack_delay: Duration::ZERO,
            pause_after_first_batch: Duration::ZERO,
            ack_override: None,
            status: None,
            fetch_failures: Arc::default(),
            attachment: Vec::new(),
        }
    }
}

pub enum Observed {
    Hello(Hello, SocketAddr),
    Control(ControlUp),
    Data(DataUp),
}

#[derive(Clone)]
struct Service {
    behavior: Behavior,
    events: mpsc::Sender<Observed>,
    downstream: Downstream,
    committed: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    data_downstream: DataDownstream,
}

impl Service {
    fn reply(&self, hello: &Hello) -> HelloReply {
        HelloReply {
            result: Some(hello_reply::Result::Accepted(Accepted {
                capabilities: self
                    .behavior
                    .capabilities
                    .iter()
                    .filter(|c| hello.optional_capabilities.contains(c))
                    .cloned()
                    .collect(),
                restore_generation: self.behavior.generation.to_string(),
                committed: hello
                    .cursors
                    .iter()
                    .map(|cursor| JournalCommitted {
                        journal_id: cursor.journal_id.clone(),
                        committed_seq: self
                            .committed
                            .lock()
                            .unwrap()
                            .get(&cursor.journal_id)
                            .copied()
                            .unwrap_or(0)
                            .max(if cursor.journal_id == hello.cj_journal_id {
                                self.behavior.committed
                            } else {
                                0
                            }),
                    })
                    .collect(),
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
        if let Some((code, message)) = self.behavior.status {
            return Err(Status::new(code, message));
        }
        *self.downstream.lock().unwrap() = Some(tx.clone());
        if self.behavior.hello_reply {
            tx.send(Ok(ControlDown {
                msg: Some(control_down::Msg::HelloReply(reply)),
            }))
            .await
            .unwrap();
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut pause = service.behavior.pause_after_first_batch;
            let mut command = service.behavior.command_on_first_batch.clone();
            let mut commands = Some(service.behavior.commands_on_first_batch.clone());
            let (acks, mut ack_queue) = mpsc::channel::<JournalRangeAck>(32);
            let ack_tx = tx.clone();
            let delay = service.behavior.range_ack_delay;
            let committed = service.committed.clone();
            let ack_task = tokio::spawn(async move {
                let mut last = 0;
                while let Some(ack) = ack_queue.recv().await {
                    if ack.up_to_seq <= last {
                        continue;
                    }
                    tokio::time::sleep(delay).await;
                    last = ack.up_to_seq;
                    committed
                        .lock()
                        .unwrap()
                        .insert(ack.journal_id.clone(), ack.up_to_seq);
                    if ack_tx
                        .send(Ok(ControlDown {
                            msg: Some(control_down::Msg::CjAck(ack)),
                        }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
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
                        let ack = JournalRangeAck {
                            journal_id: batch.journal_id.clone(),
                            up_to_seq: service
                                .behavior
                                .ack_override
                                .unwrap_or(batch.records.last().unwrap().seq),
                        };
                        if acks.send(ack).await.is_err() {
                            break;
                        }
                        None
                    }
                    _ => None,
                };
                if matches!(message.msg, Some(control_up::Msg::CjRecords(_)))
                    && let Some(burst) = commands.take()
                {
                    for command in burst {
                        if tx
                            .send(Ok(ControlDown {
                                msg: Some(control_down::Msg::Command(command)),
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
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
                let is_batch = matches!(message.msg, Some(control_up::Msg::CjRecords(_)));
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
                if is_batch && !pause.is_zero() {
                    tokio::time::sleep(pause).await;
                    pause = Duration::ZERO;
                }
            }
            ack_task.abort();
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[tonic::async_trait]
impl WorkerData for Service {
    type FetchAttachmentStream =
        ReceiverStream<Result<canterlot_proto::worker::v1::AttachmentChunk, Status>>;

    async fn fetch_attachment(
        &self,
        request: Request<canterlot_proto::worker::v1::FetchAttachmentRequest>,
    ) -> Result<Response<Self::FetchAttachmentStream>, Status> {
        self.check(&request);
        if let Some(code) = self.behavior.fetch_failures.lock().unwrap().pop_front() {
            return Err(Status::new(code, "fetch_unavailable"));
        }
        let offset = request.into_inner().offset as usize;
        let (tx, rx) = mpsc::channel(1);
        let bytes = self.behavior.attachment.clone();
        tokio::spawn(async move {
            let mut offset = offset;
            loop {
                let end = (offset + 131_072).min(bytes.len());
                if offset > end {
                    break;
                }
                if tx
                    .send(Ok(AttachmentChunk {
                        offset: offset as u64,
                        data: bytes[offset..end].to_vec(),
                        last: end == bytes.len(),
                    }))
                    .await
                    .is_err()
                    || end == bytes.len()
                {
                    break;
                }
                offset = end;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn put_media(
        &self,
        request: Request<Streaming<canterlot_proto::worker::v1::MediaChunk>>,
    ) -> Result<Response<canterlot_proto::worker::v1::MediaStored>, Status> {
        self.check(&request);
        Err(Status::unimplemented("media_not_supported_by_test_hub"))
    }

    async fn agent_call(
        &self,
        _request: Request<canterlot_proto::worker::v1::AgentCallRequest>,
    ) -> Result<Response<canterlot_proto::worker::v1::AgentCallResponse>, Status> {
        Err(Status::unimplemented(
            "agent_calls_not_supported_by_test_hub",
        ))
    }

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
        *self.data_downstream.lock().unwrap() = Some(tx.clone());
        tx.send(Ok(DataDown {
            msg: Some(data_down::Msg::HelloReply(reply)),
        }))
        .await
        .unwrap();
        let events = self.events.clone();
        let committed = self.committed.clone();
        let sj_ack = self.behavior.sj_ack;
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    _ = tx.closed() => break,
                    message = inbound.message() => match message { Ok(Some(message)) => message, _ => break },
                };
                if let Some(data_up::Msg::SjRecords(batch)) = &message.msg
                    && sj_ack
                {
                    let ack = JournalRangeAck {
                        journal_id: batch.journal_id.clone(),
                        up_to_seq: batch.records.last().unwrap().seq,
                    };
                    committed
                        .lock()
                        .unwrap()
                        .insert(ack.journal_id.clone(), ack.up_to_seq);
                    if tx
                        .send(Ok(DataDown {
                            msg: Some(data_down::Msg::SjAck(ack)),
                        }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                if events.send(Observed::Data(message)).await.is_err() {
                    break;
                }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

pub struct ProtocolHub {
    pub events: mpsc::Receiver<Observed>,
    _directory: TempDir,
    pub config: WorkerConfig,
    downstream: Downstream,
    data_downstream: DataDownstream,
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
        let (events, receiver) = mpsc::channel(4096);
        let restore_generation = behavior.generation;
        let downstream = Arc::new(Mutex::new(None));
        let data_downstream = Arc::new(Mutex::new(None));
        let service = Service {
            behavior,
            events,
            downstream: downstream.clone(),
            committed: Arc::default(),
            data_downstream: data_downstream.clone(),
        };
        let task = tokio::spawn(async move {
            Server::builder()
                .initial_stream_window_size(65_535)
                .initial_connection_window_size(65_535)
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
            machine: Default::default(),
            attachments: Default::default(),
            claude_code: None,
            codex: None,
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
            execution: Default::default(),
            opencode: None,
        };
        let store = WorkerStore::open(&config.data_dir).unwrap();
        (
            Self {
                events: receiver,
                _directory: directory,
                config,
                downstream,
                data_downstream,
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

    pub async fn fail(&self, status: Status) {
        let sender = self.downstream.lock().unwrap().clone().unwrap();
        sender.send(Err(status)).await.unwrap();
    }

    pub async fn send(&self, message: ControlDown) {
        let sender = self.downstream.lock().unwrap().clone().unwrap();
        sender.send(Ok(message)).await.unwrap();
    }

    #[cfg(feature = "synthetic-adapter")]
    pub async fn poll_limits(&self, provider: &str) {
        self.send(ControlDown {
            msg: Some(control_down::Msg::PollLimits(PollLimits {
                provider: provider.into(),
            })),
        })
        .await;
    }

    #[allow(dead_code)]
    pub async fn fail_data(&self, status: Status) {
        let sender = self.data_downstream.lock().unwrap().clone().unwrap();
        sender.send(Err(status)).await.unwrap();
    }

    pub async fn send_data(&self, message: DataDown) {
        let sender = self.data_downstream.lock().unwrap().clone().unwrap();
        sender.send(Ok(message)).await.unwrap();
    }
}

impl Drop for ProtocolHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}
