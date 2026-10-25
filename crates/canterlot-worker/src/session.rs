use std::{collections::VecDeque, fs, time::Duration};

use canterlot_proto::{
    limits::{MAX_EVENT_BATCH_BYTES, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, check_record_batch},
    worker::v1::{
        worker_control_client::WorkerControlClient, worker_data_client::WorkerDataClient, *,
    },
};
use prost::Message;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request, Status,
    metadata::{Ascii, MetadataValue},
    service::Interceptor,
    transport::{Certificate, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

use crate::{
    config::{ConfigError, WorkerConfig, read_token},
    store::{StoreError, WorkerStore},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Disconnected,
    Connecting,
    Handshaking,
    Reconciling,
    Live,
    ControlOnly,
    Backoff,
    Rejected { reason: i32, retryable: bool },
}

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("handshake rejected: reason={reason}, retryable={retryable}")]
    Rejected { reason: i32, retryable: bool },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("worker I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("session transport failed")]
    Transport,
    #[error("session receipt or handshake timed out")]
    Timeout,
    #[error("invalid session message")]
    Protocol,
    #[error("hub restore generation changed; restore reconciliation is required")]
    RestoreGenerationChanged,
    #[error("hub and worker journal cursors diverged")]
    StateDivergence,
    #[error("invalid worker timers or credential metadata")]
    InvalidConfig,
}

#[derive(Clone)]
struct Bearer(MetadataValue<Ascii>);

impl Interceptor for Bearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

pub struct Worker {
    config: WorkerConfig,
    store: Option<WorkerStore>,
    state: watch::Sender<SessionState>,
}

struct Pending {
    message: ControlUp,
    seq: u64,
    deadline: Instant,
    retried: bool,
}

impl Pending {
    fn new(message: ControlUp, seq: u64, after: Duration) -> Self {
        Self {
            message,
            seq,
            deadline: Instant::now() + after,
            retried: false,
        }
    }

    async fn resend(
        &mut self,
        tx: &mpsc::Sender<ControlUp>,
        after: Duration,
    ) -> Result<(), WorkerError> {
        if self.retried {
            return Err(WorkerError::Timeout);
        }
        send(tx, self.message.clone(), after).await?;
        self.retried = true;
        self.deadline = Instant::now() + after;
        Ok(())
    }
}

impl Worker {
    pub fn new(config: WorkerConfig, store: WorkerStore) -> Worker {
        Self {
            config,
            store: Some(store),
            state: watch::channel(SessionState::Disconnected).0,
        }
    }

    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.state.subscribe()
    }

    fn transition(&self, state: SessionState) {
        self.state.send_replace(state);
    }

    fn store(&self) -> &WorkerStore {
        self.store.as_ref().expect("open worker store")
    }
    fn store_mut(&mut self) -> &mut WorkerStore {
        self.store.as_mut().expect("open worker store")
    }

    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let hub_url =
            url::Url::parse(&self.config.hub_url).map_err(|_| WorkerError::InvalidConfig)?;
        if hub_url.scheme() != "https"
            || hub_url.host_str().is_none()
            || !hub_url.username().is_empty()
            || hub_url.password().is_some()
        {
            return Err(WorkerError::InvalidConfig);
        }
        let timers = &self.config.timers;
        if [
            timers.handshake,
            timers.heartbeat,
            timers.heartbeat_ack,
            timers.range_ack,
            timers.backoff_min,
            timers.backoff_max,
            timers.backoff_reset_after,
            timers.http2_keepalive_interval,
            timers.http2_keepalive_timeout,
        ]
        .contains(&Duration::ZERO)
            || timers.backoff_min > timers.backoff_max
        {
            return Err(WorkerError::InvalidConfig);
        }
        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        let boot_id = Uuid::parse_str(boot_id.trim())
            .map_err(|_| WorkerError::Protocol)?
            .to_string();
        let mut cap = self.config.timers.backoff_min;
        loop {
            if *shutdown.borrow() {
                self.transition(SessionState::Disconnected);
                return Ok(());
            }
            let mut live_since = None;
            self.transition(SessionState::Connecting);
            let result = tokio::select! {
                biased;
                _ = stopped(&mut shutdown) => {
                    self.transition(SessionState::Disconnected);
                    return Ok(());
                }
                result = self.attempt(&boot_id, &mut live_since) => result,
            };
            match result {
                Err(
                    error @ WorkerError::Rejected {
                        reason,
                        retryable: false,
                    },
                ) => {
                    self.transition(SessionState::Rejected {
                        reason,
                        retryable: false,
                    });
                    return Err(error);
                }
                Err(WorkerError::Rejected {
                    reason,
                    retryable: true,
                }) => {
                    self.transition(SessionState::Rejected {
                        reason,
                        retryable: true,
                    });
                }
                Err(
                    error @ (WorkerError::RestoreGenerationChanged
                    | WorkerError::StateDivergence
                    | WorkerError::Config(_)
                    | WorkerError::InvalidConfig
                    | WorkerError::Io(_)),
                ) => return Err(error),
                _ => {}
            }
            if self
                .store
                .as_ref()
                .is_some_and(|store| matches!(store.cj_cursor(), Err(StoreError::NeedsReopen)))
            {
                self.store.take();
            }
            if live_since.is_some_and(|since: Instant| {
                since.elapsed() >= self.config.timers.backoff_reset_after
            }) {
                cap = self.config.timers.backoff_min;
            }
            self.transition(SessionState::Backoff);
            let delay = Duration::from_nanos(rand::random_range(
                self.config
                    .timers
                    .backoff_min
                    .as_nanos()
                    .min(u64::MAX as u128) as u64
                    ..=cap.as_nanos().min(u64::MAX as u128) as u64,
            ));
            tokio::select! {
                biased;
                _ = stopped(&mut shutdown) => {
                    self.transition(SessionState::Disconnected);
                    return Ok(());
                }
                _ = sleep(delay) => {},
            }
            cap = cap.saturating_mul(2).min(self.config.timers.backoff_max);
        }
    }

    async fn attempt(
        &mut self,
        boot_id: &str,
        live_since: &mut Option<Instant>,
    ) -> Result<(), WorkerError> {
        let connection_id = Uuid::now_v7();
        if self.store.is_none() {
            self.store = Some(WorkerStore::open(&self.config.data_dir)?);
        }
        let token = read_token(&self.config.token_file)?;
        let mut metadata: MetadataValue<Ascii> = format!("Bearer {token}")
            .parse()
            .map_err(|_| WorkerError::InvalidConfig)?;
        metadata.set_sensitive(true);
        let auth = Bearer(metadata);
        let endpoint = Endpoint::from_shared(self.config.hub_url.clone())
            .map_err(|_| WorkerError::InvalidConfig)?
            .tls_config(
                ClientTlsConfig::new()
                    .ca_certificate(Certificate::from_pem(fs::read(&self.config.ca_cert)?)),
            )
            .map_err(|_| WorkerError::Transport)?
            .connect_timeout(self.config.timers.handshake)
            .http2_keep_alive_interval(self.config.timers.http2_keepalive_interval)
            .keep_alive_timeout(self.config.timers.http2_keepalive_timeout)
            .keep_alive_while_idle(true)
            .buffer_size(8);
        let (control, data) = timeout(self.config.timers.handshake, async {
            let control = endpoint
                .connect()
                .await
                .map_err(|_| WorkerError::Transport)?;
            let data = endpoint
                .connect()
                .await
                .map_err(|_| WorkerError::Transport)?;
            Ok::<_, WorkerError>((control, data))
        })
        .await
        .map_err(|_| WorkerError::Timeout)??;
        self.transition(SessionState::Handshaking);
        let hello = self.hello(connection_id, boot_id)?;
        let mut control = WorkerControlClient::with_interceptor(control, auth.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut data = WorkerDataClient::with_interceptor(data, auth)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let (tx, rx) = mpsc::channel(8);
        let (data_tx, data_rx) = mpsc::channel(8);
        let (mut inbound, mut data_inbound, accepted) =
            timeout(self.config.timers.handshake, async {
                send(
                    &tx,
                    ControlUp {
                        msg: Some(control_up::Msg::Hello(hello.clone())),
                    },
                    self.config.timers.handshake,
                )
                .await?;
                let mut inbound = control
                    .control(ReceiverStream::new(rx))
                    .await
                    .map_err(status_error)?
                    .into_inner();
                let accepted = match inbound
                    .message()
                    .await
                    .map_err(status_error)?
                    .and_then(|message| message.msg)
                {
                    Some(control_down::Msg::HelloReply(reply)) => accept(reply)?,
                    _ => return Err(WorkerError::Protocol),
                };
                self.check_generation(&accepted)?;
                let mut hello = hello;
                hello.stream_kind = StreamKind::Data as i32;
                hello
                    .restore_generation
                    .clone_from(&accepted.restore_generation);
                send(
                    &data_tx,
                    DataUp {
                        msg: Some(data_up::Msg::Hello(hello)),
                    },
                    self.config.timers.handshake,
                )
                .await?;
                let mut data_inbound = data
                    .data(ReceiverStream::new(data_rx))
                    .await
                    .map_err(status_error)?
                    .into_inner();
                let data_accepted = match data_inbound
                    .message()
                    .await
                    .map_err(status_error)?
                    .and_then(|message| message.msg)
                {
                    Some(data_down::Msg::HelloReply(reply)) => accept(reply)?,
                    _ => return Err(WorkerError::Protocol),
                };
                if data_accepted.restore_generation != accepted.restore_generation {
                    return Err(WorkerError::RestoreGenerationChanged);
                }
                Ok((inbound, data_inbound, accepted))
            })
            .await
            .map_err(|_| WorkerError::Timeout)??;
        let _keep_data_open = data_tx;
        self.transition(SessionState::Reconciling);
        self.check_generation(&accepted)?;
        let cursor = self.store().cj_cursor()?;
        let committed = accepted
            .committed
            .iter()
            .find(|c| c.journal_id == cursor.journal_id)
            .map_or(0, |c| c.committed_seq);
        if committed > cursor.last_seq
            || committed < cursor.acked_seq_known_to_worker
            || cursor.first_stored_seq > committed.saturating_add(1)
        {
            return Err(WorkerError::StateDivergence);
        }
        if self.store().restore_generation()?.is_none() {
            self.store_mut().set_restore_generation(
                Uuid::parse_str(&accepted.restore_generation).map_err(|_| WorkerError::Protocol)?,
            )?;
        }
        self.store_mut().on_cj_ack(committed)?;
        let replay_end = cursor.last_seq;
        let mut acknowledged = committed;
        let mut next_seq = committed + 1;
        let mut pending_batches: VecDeque<Pending> = VecDeque::new();
        let mut pending_heartbeat: Option<Pending> = None;
        let mut heartbeat_seq = 0u64;
        let mut heartbeat_at = Instant::now() + self.config.timers.heartbeat;
        loop {
            if live_since.is_none() && acknowledged >= replay_end {
                *live_since = Some(Instant::now());
                self.transition(SessionState::Live);
            }
            while pending_batches.len() < 8 {
                let Some((message, last)) = self.batch(next_seq)? else {
                    break;
                };
                send(&tx, message.clone(), self.config.timers.range_ack).await?;
                pending_batches.push_back(Pending::new(
                    message,
                    last,
                    self.config.timers.range_ack,
                ));
                next_seq = last + 1;
            }
            let heartbeat_deadline = pending_heartbeat
                .as_ref()
                .map_or(heartbeat_at, |p| p.deadline);
            let batch_deadline = pending_batches.iter().map(|p| p.deadline).min();
            tokio::select! {
                message = inbound.message() => {
                    let message = message.map_err(status_error)?.ok_or(WorkerError::Transport)?;
                    match message.msg {
                        Some(control_down::Msg::Command(command)) => { self.store_mut().accept_command(&command)?; }
                        Some(control_down::Msg::CjAck(ack)) => {
                            if ack.journal_id != self.store().cj_journal_id()?.to_string() { return Err(WorkerError::Protocol); }
                            self.store_mut().on_cj_ack(ack.up_to_seq)?;
                            acknowledged = acknowledged.max(ack.up_to_seq);
                            while pending_batches.front().is_some_and(|p| ack.up_to_seq >= p.seq) { pending_batches.pop_front(); }
                        }
                        Some(control_down::Msg::HeartbeatAck(ack)) => {
                            if pending_heartbeat.as_ref().is_some_and(|p| ack.seq == p.seq) {
                                pending_heartbeat = None;
                            }
                        }
                        _ => return Err(WorkerError::Protocol),
                    }
                }
                message = data_inbound.message() => {
                    match message {
                        Ok(Some(DataDown { msg: Some(data_down::Msg::SjAck(_)) })) => return Err(WorkerError::Protocol),
                        _ => { self.transition(SessionState::ControlOnly); return Err(WorkerError::Transport); }
                    }
                }
                _ = sleep_until(heartbeat_deadline) => {
                    if let Some(pending) = &mut pending_heartbeat {
                        pending.resend(&tx, self.config.timers.heartbeat_ack).await?;
                    } else {
                        heartbeat_seq += 1;
                        let message = ControlUp { msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq: heartbeat_seq })) };
                        send(&tx, message.clone(), self.config.timers.heartbeat_ack).await?;
                        pending_heartbeat = Some(Pending::new(message, heartbeat_seq, self.config.timers.heartbeat_ack));
                        heartbeat_at = Instant::now() + self.config.timers.heartbeat;
                    }
                }
                _ = async {
                    if let Some(deadline) = batch_deadline { sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    if let Some(pending) = pending_batches.iter_mut().min_by_key(|p| p.deadline) {
                        pending.resend(&tx, self.config.timers.range_ack).await?;
                    }
                }
            }
        }
    }

    fn hello(&self, connection: Uuid, boot_id: &str) -> Result<Hello, WorkerError> {
        Ok(Hello {
            installation_id: self.config.installation_id.to_string(),
            machine_id: self.config.machine_id.to_string(),
            restore_generation: self
                .store()
                .restore_generation()?
                .map_or_else(String::new, |g| g.to_string()),
            protocol_version: PROTOCOL_VERSION,
            connection_id: connection.to_string(),
            cj_journal_id: self.store().cj_journal_id()?.to_string(),
            cursors: vec![self.store().cj_cursor()?],
            boot_id: boot_id.to_owned(),
            stream_kind: StreamKind::Control as i32,
            required_capabilities: vec![],
            optional_capabilities: vec![],
        })
    }

    fn check_generation(&self, accepted: &Accepted) -> Result<(), WorkerError> {
        let generation =
            Uuid::parse_str(&accepted.restore_generation).map_err(|_| WorkerError::Protocol)?;
        if self
            .store()
            .restore_generation()?
            .is_some_and(|known| known != generation)
        {
            return Err(WorkerError::RestoreGenerationChanged);
        }
        Ok(())
    }

    fn batch(&self, from: u64) -> Result<Option<(ControlUp, u64)>, WorkerError> {
        let mut batch = JournalRecords {
            journal_id: self.store().cj_journal_id()?.to_string(),
            records: vec![],
        };
        for record in self.store().cj_records_from(from, MAX_EVENT_BATCH_BYTES)? {
            batch.records.push(record);
            if check_record_batch(&batch).is_err() {
                batch.records.pop();
                if batch.records.is_empty() {
                    return Err(WorkerError::Protocol);
                }
                break;
            }
        }
        let Some(last) = batch.records.last().map(|r| r.seq) else {
            return Ok(None);
        };
        check_record_batch(&batch).map_err(|_| WorkerError::Protocol)?;
        Ok(Some((
            ControlUp {
                msg: Some(control_up::Msg::CjRecords(batch)),
            },
            last,
        )))
    }
}

async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow_and_update() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

async fn send<T: Message>(
    tx: &mpsc::Sender<T>,
    message: T,
    after: Duration,
) -> Result<(), WorkerError> {
    if message.encoded_len() > MAX_MESSAGE_BYTES {
        return Err(WorkerError::Protocol);
    }
    timeout(after, tx.send(message))
        .await
        .map_err(|_| WorkerError::Timeout)?
        .map_err(|_| WorkerError::Transport)
}

fn accept(reply: HelloReply) -> Result<Accepted, WorkerError> {
    match reply.result {
        Some(hello_reply::Result::Accepted(accepted)) => Ok(accepted),
        Some(hello_reply::Result::Rejected(rejected)) => Err(WorkerError::Rejected {
            reason: rejected.reason,
            retryable: rejected.retryable,
        }),
        None => Err(WorkerError::Protocol),
    }
}

fn status_error(status: Status) -> WorkerError {
    if status.code() == tonic::Code::Unauthenticated {
        WorkerError::Rejected {
            reason: RejectReason::CredentialRevoked as i32,
            retryable: false,
        }
    } else {
        WorkerError::Transport
    }
}
