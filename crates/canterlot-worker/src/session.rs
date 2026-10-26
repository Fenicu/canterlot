use std::{collections::VecDeque, fs, future::Future, pin::Pin, time::Duration};

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
    config::{ConfigError, ConfigErrorCategory, WorkerConfig, read_token},
    session_store::StoreThread,
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
    Backoff { last_error: ErrorKind },
    Rejected { reason: i32, retryable: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreErrorKind {
    InsecureDataDir,
    NeedsReopen,
    Io,
    Journal,
    CjSealed,
    LocalSealed,
    InvalidCommandId,
    Limits,
    Decode,
    Metadata,
    EncodeMetadata,
    MetadataUtf8,
}

impl StoreError {
    pub fn kind(&self) -> StoreErrorKind {
        match self {
            Self::InsecureDataDir => StoreErrorKind::InsecureDataDir,
            Self::NeedsReopen => StoreErrorKind::NeedsReopen,
            Self::Io(_) => StoreErrorKind::Io,
            Self::Journal(_) => StoreErrorKind::Journal,
            Self::CjSealed => StoreErrorKind::CjSealed,
            Self::LocalSealed(_) => StoreErrorKind::LocalSealed,
            Self::InvalidCommandId(_) => StoreErrorKind::InvalidCommandId,
            Self::Limits(_) => StoreErrorKind::Limits,
            Self::Decode(_) => StoreErrorKind::Decode,
            Self::Metadata(_) => StoreErrorKind::Metadata,
            Self::EncodeMetadata(_) => StoreErrorKind::EncodeMetadata,
            Self::MetadataUtf8(_) => StoreErrorKind::MetadataUtf8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Hub(tonic::Code),
    Store(StoreErrorKind),
    Rejected(i32),
    Config,
    Io,
    Transport,
    Timeout,
    Protocol,
    RestoreGenerationChanged,
    GenerationMismatch,
    StateDivergence,
    InvalidConfig,
    StoreThread,
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
    #[error("hub status: {code}: {message}")]
    Hub { code: tonic::Code, message: String },
    #[error("store reopen failed ({kind:?}): {source}", kind = .0.kind(), source = .0)]
    Reopen(#[source] StoreError),
    #[error("store thread failed")]
    StoreThread,
    #[error("session receipt or handshake timed out")]
    Timeout,
    #[error("invalid session message")]
    Protocol,
    #[error("hub restore generation changed; restore reconciliation is required")]
    RestoreGenerationChanged,
    #[error(
        "configured restore generation differs from persisted generation; re-pairing is required"
    )]
    GenerationMismatch,
    #[error("hub and worker journal cursors diverged")]
    StateDivergence,
    #[error("invalid worker timers or credential metadata")]
    InvalidConfig,
}

impl WorkerError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Hub { code, .. } => ErrorKind::Hub(*code),
            Self::Store(error) | Self::Reopen(error) => ErrorKind::Store(error.kind()),
            Self::Rejected { reason, .. } => ErrorKind::Rejected(*reason),
            Self::Config(_) => ErrorKind::Config,
            Self::Io(_) => ErrorKind::Io,
            Self::Transport => ErrorKind::Transport,
            Self::Timeout => ErrorKind::Timeout,
            Self::Protocol => ErrorKind::Protocol,
            Self::RestoreGenerationChanged => ErrorKind::RestoreGenerationChanged,
            Self::GenerationMismatch => ErrorKind::GenerationMismatch,
            Self::StateDivergence => ErrorKind::StateDivergence,
            Self::InvalidConfig => ErrorKind::InvalidConfig,
            Self::StoreThread => ErrorKind::StoreThread,
        }
    }
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
    store: StoreThread,
    state: watch::Sender<SessionState>,
}

struct Pending {
    message: ControlUp,
    seq: u64,
    deadline: Option<Instant>,
    retried: bool,
}

impl Pending {
    fn new(message: ControlUp, seq: u64, after: Duration) -> Self {
        Self {
            message,
            seq,
            deadline: Some(Instant::now() + after),
            retried: false,
        }
    }

    fn resend(&mut self, after: Duration) -> Result<ControlUp, WorkerError> {
        if self.retried {
            return Err(WorkerError::Timeout);
        }
        self.retried = true;
        self.deadline = Some(Instant::now() + after);
        Ok(self.message.clone())
    }

    fn reset(&mut self, after: Duration) {
        self.retried = false;
        self.deadline = Some(Instant::now() + after);
    }
}

impl Worker {
    pub fn new(config: WorkerConfig, store: WorkerStore) -> Worker {
        Self {
            config,
            store: StoreThread::new(store),
            state: watch::channel(SessionState::Disconnected).0,
        }
    }

    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.state.subscribe()
    }

    fn transition(&self, state: SessionState) {
        tracing::info!(state = state_kind(&state), "session state");
        self.state.send_replace(state);
    }

    pub async fn run(mut self, shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let result = self.run_inner(shutdown).await;
        let closed = self.store.close().await;
        result.and(closed)
    }

    async fn run_inner(&mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let configured = self.config.restore_generation;
        self.store
            .call(move |store| initialize_generation(store.as_mut().unwrap(), configured))
            .await?;
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
        let boot_id =
            tokio::task::spawn_blocking(|| fs::read_to_string("/proc/sys/kernel/random/boot_id"))
                .await
                .map_err(|_| WorkerError::StoreThread)??;
        let boot_id = Uuid::parse_str(boot_id.trim())
            .map_err(|_| WorkerError::Protocol)?
            .to_string();
        let mut cap = self.config.timers.backoff_min;
        let mut read_retries = 0;
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
            let error = result.err().unwrap_or(WorkerError::Transport);
            let last_error = error.kind();
            if let WorkerError::Hub { code, .. } = &error {
                tracing::warn!(error_kind = ?last_error, code = ?code, "session attempt failed");
            } else {
                tracing::warn!(error_kind = ?last_error, "session attempt failed");
            }
            let read_error = matches!(
                &error,
                WorkerError::Config(ConfigError {
                    category: ConfigErrorCategory::Io(_) | ConfigErrorCategory::TokenPermissions,
                    ..
                }) | WorkerError::Io(_)
            );
            let read_delay = if read_error {
                if read_retries == 3 {
                    return Err(error);
                }
                read_retries += 1;
                Some(Duration::from_secs(1))
            } else {
                read_retries = 0;
                None
            };
            match error {
                error @ WorkerError::Hub {
                    code: tonic::Code::Unauthenticated,
                    ..
                } => {
                    self.transition(SessionState::Rejected {
                        reason: RejectReason::CredentialRevoked as i32,
                        retryable: false,
                    });
                    return Err(error);
                }
                error @ WorkerError::Rejected {
                    reason,
                    retryable: false,
                } => {
                    self.transition(SessionState::Rejected {
                        reason,
                        retryable: false,
                    });
                    return Err(error);
                }
                WorkerError::Rejected {
                    reason,
                    retryable: true,
                } => {
                    self.transition(SessionState::Rejected {
                        reason,
                        retryable: true,
                    });
                }
                WorkerError::Reopen(ref source) if transient_store_error(source) => {}
                error @ (WorkerError::RestoreGenerationChanged
                | WorkerError::GenerationMismatch
                | WorkerError::StateDivergence
                | WorkerError::InvalidConfig
                | WorkerError::StoreThread
                | WorkerError::Reopen(_)) => return Err(error),
                error @ WorkerError::Config(_) if !read_error => return Err(error),
                _ => {}
            }
            if live_since.is_some_and(|since: Instant| {
                since.elapsed() >= self.config.timers.backoff_reset_after
            }) {
                cap = self.config.timers.backoff_min;
            }
            self.transition(SessionState::Backoff { last_error });
            let delay = read_delay.unwrap_or_else(|| {
                Duration::from_nanos(rand::random_range(
                    self.config
                        .timers
                        .backoff_min
                        .as_nanos()
                        .min(u64::MAX as u128) as u64
                        ..=cap.as_nanos().min(u64::MAX as u128) as u64,
                ))
            });
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
        let configured = self.config.restore_generation;
        let data_dir = self.config.data_dir.clone();
        let (cursor, generation) = self
            .store
            .call(move |slot| {
                if slot.is_none() {
                    *slot = Some(WorkerStore::open(&data_dir).map_err(WorkerError::Reopen)?);
                }
                let store = slot.as_mut().unwrap();
                initialize_generation(store, configured)?;
                Ok((store.cj_cursor()?, store.restore_generation()?.unwrap()))
            })
            .await?;
        let token_file = self.config.token_file.clone();
        let ca_cert = self.config.ca_cert.clone();
        let (token, ca) = tokio::task::spawn_blocking(move || {
            Ok::<_, WorkerError>((read_token(&token_file)?, fs::read(ca_cert)?))
        })
        .await
        .map_err(|_| WorkerError::StoreThread)??;
        let mut metadata: MetadataValue<Ascii> = format!("Bearer {token}")
            .parse()
            .map_err(|_| WorkerError::InvalidConfig)?;
        metadata.set_sensitive(true);
        let auth = Bearer(metadata);
        let endpoint = Endpoint::from_shared(self.config.hub_url.clone())
            .map_err(|_| WorkerError::InvalidConfig)?
            .tls_config(ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca)))
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
        tracing::info!(%connection_id, "session connected");
        let hello = Hello {
            installation_id: self.config.installation_id.to_string(),
            machine_id: self.config.machine_id.to_string(),
            restore_generation: generation.to_string(),
            protocol_version: PROTOCOL_VERSION,
            connection_id: connection_id.to_string(),
            cj_journal_id: cursor.journal_id.clone(),
            cursors: vec![cursor.clone()],
            boot_id: boot_id.to_owned(),
            stream_kind: StreamKind::Control as i32,
            required_capabilities: vec![],
            optional_capabilities: vec![],
        };
        let mut control = WorkerControlClient::with_interceptor(control, auth.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut data = WorkerDataClient::with_interceptor(data, auth)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let (tx, rx) = mpsc::channel(1);
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
                check_generation(generation, &accepted)?;
                let mut hello = hello;
                hello.stream_kind = StreamKind::Data as i32;
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
        check_generation(generation, &accepted)?;
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
        self.store
            .call(move |store| {
                store.as_mut().unwrap().on_cj_ack(committed)?;
                Ok(())
            })
            .await?;
        let replay_end = cursor.last_seq;
        let mut acknowledged = committed;
        let mut received_ack = committed;
        let mut next_seq = committed + 1;
        let mut pending_batches: VecDeque<Pending> = VecDeque::new();
        let mut pending_heartbeat: Option<Pending> = None;
        let mut heartbeat_seq = 0u64;
        let mut heartbeat_at = Instant::now() + self.config.timers.heartbeat;
        let journal_id = cursor.journal_id;
        let mut commands = VecDeque::new();
        let mut store_ack: Option<u64> = None;
        let mut dirty = true;
        let mut ready_batch: Option<(ControlUp, u64)> = None;
        let mut heartbeat_outbound = None;
        let mut range_outbound = None;
        type StoreFuture = Pin<Box<dyn Future<Output = Result<StoreReply, WorkerError>> + Send>>;
        let mut operation: Option<StoreFuture> = None;
        loop {
            if live_since.is_none() && acknowledged >= replay_end {
                *live_since = Some(Instant::now());
                self.transition(SessionState::Live);
            }
            if operation.is_none() {
                if let Some(up_to) = store_ack.take() {
                    operation = Some(Box::pin(self.store.call(move |store| {
                        store.as_mut().unwrap().on_cj_ack(up_to)?;
                        Ok(StoreReply::Ack(up_to))
                    })));
                } else if dirty && ready_batch.is_none() && pending_batches.len() < 8 {
                    let from = next_seq;
                    operation = Some(Box::pin(self.store.call(move |store| {
                        Ok(StoreReply::Batch(
                            from,
                            batch(store.as_ref().unwrap(), from)?.map(Box::new),
                        ))
                    })));
                    dirty = false;
                } else if let Some(command) = commands.pop_front() {
                    operation = Some(Box::pin(self.store.call(move |store| {
                        store.as_mut().unwrap().accept_command(&command)?;
                        Ok(StoreReply::Command)
                    })));
                }
            }
            let heartbeat_deadline = pending_heartbeat
                .as_ref()
                .map_or(heartbeat_at, |p| p.deadline.unwrap());
            let batch_deadline = pending_batches.front().and_then(|p| p.deadline);
            let can_send = heartbeat_outbound.is_some()
                || range_outbound.is_some()
                || (ready_batch.is_some() && pending_batches.len() < 8);
            tokio::select! {
                message = inbound.message(), if commands.len() < 32 => {
                    let message = message.map_err(status_error)?.ok_or(WorkerError::Transport)?;
                    match message.msg {
                        Some(control_down::Msg::Command(command)) => { commands.push_back(command); }
                        Some(control_down::Msg::CjAck(ack)) => {
                            if ack.journal_id != journal_id { return Err(WorkerError::Protocol); }
                            store_ack = Some(store_ack.unwrap_or(0).max(ack.up_to_seq));
                            next_seq = next_seq.max(ack.up_to_seq.saturating_add(1));
                            while pending_batches.front().is_some_and(|p| ack.up_to_seq >= p.seq) { pending_batches.pop_front(); }
                            if ack.up_to_seq > received_ack {
                                received_ack = ack.up_to_seq;
                                for pending in &mut pending_batches { pending.deadline = None; pending.retried = false; }
                                if let Some(head) = pending_batches.front_mut() { head.reset(self.config.timers.range_ack); }
                                range_outbound = None;
                            }
                            if ready_batch.as_ref().is_some_and(|(m, _)| batch_first(m) < next_seq) {
                                ready_batch = None;
                            }
                            dirty = true;
                            tracing::debug!(up_to_seq = ack.up_to_seq, "journal acknowledged");
                        }
                        Some(control_down::Msg::HeartbeatAck(ack)) => {
                            if pending_heartbeat.as_ref().is_some_and(|p| ack.seq == p.seq) {
                                pending_heartbeat = None;
                                heartbeat_outbound = None;
                                tracing::debug!(seq = ack.seq, "heartbeat acknowledged");
                            }
                        }
                        _ => return Err(WorkerError::Protocol),
                    }
                }
                result = async { operation.as_mut().unwrap().await }, if operation.is_some() => {
                    operation = None;
                    match result? {
                        StoreReply::Command => dirty = true,
                        StoreReply::Ack(up_to) => acknowledged = acknowledged.max(up_to),
                        StoreReply::Batch(from, result) => {
                            if from == next_seq { ready_batch = result.map(|batch| *batch); }
                            else { dirty = true; }
                        }
                    }
                }
                permit = tx.reserve(), if can_send => {
                    let permit = permit.map_err(|_| WorkerError::Transport)?;
                    let message = if let Some(message) = heartbeat_outbound.take() { message }
                    else if let Some(message) = range_outbound.take() { message }
                    else {
                        let (message, last) = ready_batch.take().unwrap();
                        let mut pending = Pending::new(message.clone(), last, self.config.timers.range_ack);
                        if !pending_batches.is_empty() { pending.deadline = None; }
                        pending_batches.push_back(pending);
                        next_seq = last + 1;
                        dirty = true;
                        message
                    };
                    if message.encoded_len() > MAX_MESSAGE_BYTES { return Err(WorkerError::Protocol); }
                    permit.send(message);
                }
                message = data_inbound.message() => {
                    self.transition(SessionState::ControlOnly);
                    return Err(match message {
                        Err(status) => status_error(status),
                        Ok(Some(_)) => WorkerError::Protocol,
                        Ok(None) => WorkerError::Transport,
                    });
                }
                _ = sleep_until(heartbeat_deadline) => {
                    if let Some(pending) = &mut pending_heartbeat {
                        heartbeat_outbound = Some(pending.resend(self.config.timers.heartbeat_ack)?);
                    } else {
                        heartbeat_seq += 1;
                        let message = ControlUp { msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq: heartbeat_seq })) };
                        heartbeat_outbound = Some(message.clone());
                        pending_heartbeat = Some(Pending::new(message, heartbeat_seq, self.config.timers.heartbeat_ack));
                        heartbeat_at = Instant::now() + self.config.timers.heartbeat;
                    }
                }
                _ = async {
                    if let Some(deadline) = batch_deadline { sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => {
                    range_outbound = Some(pending_batches.front_mut().unwrap().resend(self.config.timers.range_ack)?);
                }
            }
        }
    }
}

enum StoreReply {
    Command,
    Ack(u64),
    Batch(u64, Option<Box<(ControlUp, u64)>>),
}

fn batch_first(message: &ControlUp) -> u64 {
    match &message.msg {
        Some(control_up::Msg::CjRecords(batch)) => batch.records[0].seq,
        _ => unreachable!(),
    }
}

fn initialize_generation(store: &mut WorkerStore, configured: Uuid) -> Result<(), WorkerError> {
    match store.restore_generation()? {
        None => store.set_restore_generation(configured)?,
        Some(persisted) if persisted == configured => {}
        Some(_) => return Err(WorkerError::GenerationMismatch),
    }
    Ok(())
}

fn check_generation(generation: Uuid, accepted: &Accepted) -> Result<(), WorkerError> {
    let accepted =
        Uuid::parse_str(&accepted.restore_generation).map_err(|_| WorkerError::Protocol)?;
    if accepted != generation {
        return Err(WorkerError::RestoreGenerationChanged);
    }
    Ok(())
}

fn transient_store_error(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Io(_)
            | StoreError::Journal(
                canterlot_journal::JournalError::Io(_) | canterlot_journal::JournalError::Locked
            )
    )
}

fn state_kind(state: &SessionState) -> &'static str {
    match state {
        SessionState::Disconnected => "disconnected",
        SessionState::Connecting => "connecting",
        SessionState::Handshaking => "handshaking",
        SessionState::Reconciling => "reconciling",
        SessionState::Live => "live",
        SessionState::ControlOnly => "control_only",
        SessionState::Backoff { .. } => "backoff",
        SessionState::Rejected { .. } => "rejected",
    }
}

fn batch(store: &WorkerStore, from: u64) -> Result<Option<(ControlUp, u64)>, WorkerError> {
    let mut batch = JournalRecords {
        journal_id: store.cj_journal_id()?.to_string(),
        records: vec![],
    };
    for record in store
        .cj_records_from(from, MAX_EVENT_BATCH_BYTES)?
        .into_iter()
        .take(256)
    {
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
    WorkerError::Hub {
        code: status.code(),
        message: status.message().to_owned(),
    }
}

#[cfg(test)]
mod tests;
