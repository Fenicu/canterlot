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
    attachments::{AttachmentSource, ChunkStream, FetchError},
    config::{ConfigError, ConfigErrorCategory, WorkerConfig, read_token},
    exec::{ExecutionManager, SjRelay},
    media::{MediaSink, PutError},
    session_store::StoreThread,
    store::{Accept, StoreError, WorkerStore},
};

mod data;

pub const OPTIONAL_CAPABILITIES: &[&str] = &[
    "exec_v1",
    "pilot_v1",
    "instructions_v1",
    "workdir_suggest_v1",
    "todo_v1",
    "compact_v1",
    "background_v1",
];

#[derive(Clone)]
pub struct CachedLimits {
    pub observations: Vec<LimitObservation>,
    pub weekly_reserve_active: bool,
    pub received_at: Instant,
}
#[derive(Clone, Default)]
pub struct LimitsCache(
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, CachedLimits>>>,
);
impl LimitsCache {
    pub fn get(&self, provider: &str) -> Option<CachedLimits> {
        self.0
            .lock()
            .expect("limits cache lock")
            .get(provider)
            .cloned()
    }
    pub fn update(&self, snapshot: LimitsSnapshot) {
        self.0.lock().expect("limits cache lock").insert(
            snapshot.provider,
            CachedLimits {
                observations: snapshot.current,
                weekly_reserve_active: snapshot.weekly_reserve_active,
                received_at: Instant::now(),
            },
        );
    }
}
#[derive(Clone, Default)]
pub(super) struct JournalDiagnostics(
    std::sync::Arc<std::sync::Mutex<std::collections::HashSet<Uuid>>>,
    std::sync::Arc<std::sync::Mutex<std::collections::HashSet<Uuid>>>,
);
impl JournalDiagnostics {
    /// Журнал, чью запись хаб не принимает, ждёт нового сеанса: новая версия хаба или воркера
    /// начинает именно его. Диапазон не подтверждён и остаётся в журнале.
    pub(super) fn refused(&self, journal: Uuid, reason: &str) {
        if self
            .1
            .lock()
            .expect("journal diagnostics lock")
            .insert(journal)
        {
            tracing::error!(journal_id=%journal,kind="journal_refused",reason,"hub refused a journal record; journal parked until the next session");
        }
    }

    pub(super) fn parked(&self) -> std::collections::HashSet<Uuid> {
        self.1.lock().expect("journal diagnostics lock").clone()
    }

    pub(super) fn withheld(&self, journal: Uuid) {
        if self
            .0
            .lock()
            .expect("journal diagnostics lock")
            .insert(journal)
        {
            tracing::warn!(journal_id=%journal,kind="journal_withheld","journal capability missing");
        }
    }
}

/// Отказ определяется записью журнала: повтор упрётся в неё же до обновления хаба или воркера.
fn refused_record(message: &str) -> bool {
    matches!(
        message,
        "unknown_record"
            | "invalid_record"
            | "rejected_value"
            | "foreign_journal"
            | "foreign_execution"
            | "journal_kind_mismatch"
            | "execution"
    )
}

pub fn try_send_metrics(tx: &mpsc::Sender<ControlUp>, metrics: MachineMetrics) -> bool {
    let message = ControlUp {
        msg: Some(control_up::Msg::MachineMetrics(metrics)),
    };
    message.encoded_len() <= MAX_MESSAGE_BYTES && tx.try_send(message).is_ok()
}

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
    Execution,
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
    #[error("execution manager failed")]
    Execution,
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
            Self::Execution => ErrorKind::Execution,
        }
    }
}

#[derive(Clone)]
struct Bearer(MetadataValue<Ascii>);

pub struct GrpcAttachmentSource {
    client: WorkerDataClient<
        tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Bearer>,
    >,
}

impl GrpcAttachmentSource {
    pub fn from_data_channel(
        channel: tonic::transport::Channel,
        credential: MetadataValue<Ascii>,
    ) -> Self {
        Self {
            client: WorkerDataClient::with_interceptor(channel, Bearer(credential))
                .max_encoding_message_size(MAX_MESSAGE_BYTES)
                .max_decoding_message_size(MAX_MESSAGE_BYTES),
        }
    }
}

fn fetch_error(status: Status) -> FetchError {
    match status.code() {
        tonic::Code::NotFound | tonic::Code::PermissionDenied => FetchError::NotFound,
        tonic::Code::OutOfRange | tonic::Code::InvalidArgument => FetchError::SizeMismatch,
        _ => FetchError::Transport,
    }
}

#[async_trait::async_trait]
impl AttachmentSource for GrpcAttachmentSource {
    async fn open(&self, attachment_id: Uuid, offset: u64) -> Result<ChunkStream, FetchError> {
        use tokio_stream::StreamExt;
        let stream = self
            .client
            .clone()
            .fetch_attachment(FetchAttachmentRequest {
                attachment_id: attachment_id.to_string(),
                offset,
            })
            .await
            .map_err(fetch_error)?
            .into_inner();
        Ok(Box::pin(stream.map(|chunk| chunk.map_err(fetch_error))))
    }
}

/// Загрузка картинок агента по отдельному соединению HTTP/2: поток SJ и управление её не ждут.
pub struct GrpcMediaSink {
    client: WorkerDataClient<
        tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Bearer>,
    >,
}

/// Вызовы инструментов агента в хаб через канал данных: то же соединение, что и вложения.
pub struct GrpcAgentCaller {
    client: WorkerDataClient<
        tonic::service::interceptor::InterceptedService<tonic::transport::Channel, Bearer>,
    >,
}

#[async_trait::async_trait]
impl crate::exec::AgentCaller for GrpcAgentCaller {
    async fn call(
        &self,
        request: canterlot_proto::worker::v1::AgentCallRequest,
    ) -> Result<canterlot_proto::worker::v1::AgentCallResponse, tonic::Status> {
        self.client
            .clone()
            .agent_call(request)
            .await
            .map(|response| response.into_inner())
    }
}

fn put_error(status: &Status) -> PutError {
    match status.code() {
        tonic::Code::AlreadyExists => PutError::Conflict,
        tonic::Code::DataLoss => PutError::Corrupt,
        tonic::Code::InvalidArgument
        | tonic::Code::NotFound
        | tonic::Code::PermissionDenied
        | tonic::Code::FailedPrecondition
        | tonic::Code::OutOfRange
        | tonic::Code::Unimplemented => PutError::Rejected,
        _ => PutError::Transport,
    }
}

#[async_trait::async_trait]
impl MediaSink for GrpcMediaSink {
    async fn put(&self, header: MediaHeader, mut file: tokio::fs::File) -> Result<(), PutError> {
        use tokio::io::AsyncReadExt;
        let media_id = header.media_id.clone();
        let (frames, rx) = mpsc::channel(2);
        // Файл читается кусками по мере отправки: в памяти не больше пары чанков.
        let producer = tokio::spawn(async move {
            let first = MediaChunk {
                part: Some(media_chunk::Part::Header(header)),
            };
            if frames.send(first).await.is_err() {
                return Ok(());
            }
            loop {
                let mut data = vec![0; canterlot_proto::limits::MAX_FILE_CHUNK_BYTES];
                let read = file.read(&mut data).await?;
                if read == 0 {
                    return Ok::<_, std::io::Error>(());
                }
                data.truncate(read);
                let chunk = MediaChunk {
                    part: Some(media_chunk::Part::Data(data)),
                };
                if frames.send(chunk).await.is_err() {
                    return Ok(());
                }
            }
        });
        let response = self.client.clone().put_media(ReceiverStream::new(rx)).await;
        producer.abort();
        match response {
            Ok(stored) if stored.get_ref().media_id == media_id => Ok(()),
            Ok(_) => Err(PutError::Rejected),
            Err(status) => Err(put_error(&status)),
        }
    }
}

impl Interceptor for Bearer {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert("authorization", self.0.clone());
        Ok(request)
    }
}

pub struct Worker {
    #[cfg(feature = "fault-injection")]
    probe: crate::fault_injection::SessionProbe,
    config: WorkerConfig,
    store: StoreThread,
    state: watch::Sender<SessionState>,
    manager: Option<ExecutionManager>,
    relay: Option<mpsc::Receiver<SjRelay>>,
    limits_cache: LimitsCache,
    limits_source: std::sync::Arc<dyn crate::machine::limits::LimitsSource>,
    live_limits: crate::machine::limits::LiveLimits,
    idle_ticker: crate::machine::limits::IdleTicker,
    metrics: std::sync::Arc<dyn crate::machine::metrics::MetricsSource>,
    metrics_task: Option<MetricsFuture>,
    workdirs: crate::machine::workdirs::WorkdirSuggester,
    claude_version: Option<crate::adapter::claude_code::version::CliVersion>,
    codex_catalog: Option<crate::adapter::codex::CodexCatalog>,
    opencode_catalog: Option<crate::adapter::opencode::OpenCodeCatalog>,
}

type MetricsFuture = Pin<
    Box<dyn Future<Output = Result<MachineMetrics, crate::machine::metrics::MetricsError>> + Send>,
>;

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
        use std::collections::HashMap;
        use std::sync::Arc;

        #[cfg(feature = "synthetic-adapter")]
        let default_source: Arc<dyn crate::machine::limits::LimitsSource> = Arc::new(
            crate::machine::limits::FileLimitsSource::new(config.data_dir.clone()),
        );
        #[cfg(not(feature = "synthetic-adapter"))]
        let default_source: Arc<dyn crate::machine::limits::LimitsSource> =
            Arc::new(crate::machine::limits::NoLimitsSource);
        let live_limits = crate::machine::limits::LiveLimits::default();
        let limits_cache = LimitsCache::default();
        let mut idle_ticker = crate::machine::limits::IdleTicker::default();
        let mut routes: HashMap<String, Arc<dyn crate::machine::limits::LimitsSource>> =
            HashMap::new();
        if let Some(claude) = &config.claude_code {
            idle_ticker.register("claude", claude.oauth_poll_interval());
            routes.insert(
                "claude".into(),
                Arc::new(crate::machine::limits::ClaudeOAuthLimitsSource::new(
                    claude,
                    live_limits.clone(),
                )),
            );
        }
        if let Some(codex) = &config.codex {
            idle_ticker.register("codex", crate::machine::limits::CODEX_IDLE_TICK_INTERVAL);
            let cache_for_hub = limits_cache.clone();
            let hub_age: crate::machine::limits::HubLimitsAge =
                Arc::new(move |provider| cache_for_hub.get(provider).map(|c| c.received_at));
            routes.insert(
                "codex".into(),
                Arc::new(crate::machine::limits::CodexRateLimitsSource::new(
                    codex,
                    live_limits.clone(),
                    hub_age,
                )),
            );
        }
        let limits_source: Arc<dyn crate::machine::limits::LimitsSource> = if routes.is_empty() {
            default_source
        } else {
            Arc::new(crate::machine::limits::RoutedLimitsSource::new(
                routes,
                default_source,
            ))
        };
        let metrics = crate::machine::metrics::ProcMetricsSource::new(
            crate::machine::metrics::MetricsCollector::new(
                config.data_dir.clone(),
                config.execution.allowed_workdir_roots.clone(),
                "/proc".into(),
            ),
        );
        let workdirs = crate::machine::workdirs::WorkdirSuggester::new(
            config.execution.allowed_workdir_roots.clone(),
        );
        Self {
            limits_cache,
            limits_source,
            live_limits,
            idle_ticker,
            metrics: std::sync::Arc::new(metrics),
            metrics_task: None,
            workdirs,
            claude_version: None,
            codex_catalog: None,
            opencode_catalog: None,
            #[cfg(feature = "fault-injection")]
            probe: Default::default(),
            config,
            store: StoreThread::new(store),
            state: watch::channel(SessionState::Disconnected).0,
            manager: None,
            relay: None,
        }
    }

    pub fn limits_cache(&self) -> LimitsCache {
        self.limits_cache.clone()
    }
    pub fn with_limits_source(
        mut self,
        source: std::sync::Arc<dyn crate::machine::limits::LimitsSource>,
    ) -> Self {
        self.limits_source = source;
        self
    }

    pub fn with_metrics_source(
        mut self,
        source: std::sync::Arc<dyn crate::machine::metrics::MetricsSource>,
    ) -> Self {
        self.metrics = source;
        self
    }

    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.state.subscribe()
    }

    #[cfg(feature = "fault-injection")]
    pub fn with_session_probe(mut self, probe: crate::fault_injection::SessionProbe) -> Self {
        self.probe = probe;
        self
    }

    fn transition(&self, state: SessionState) {
        transition(&self.state, state);
    }

    pub async fn run(mut self, shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let result = self.run_inner(shutdown).await;
        if let Some(task) = self.metrics_task.take() {
            let _ = task.await;
        }
        if let Some(manager) = self.manager.take() {
            manager.set_attachment_source(None);
            manager.set_media_sink(None);
            manager.set_agent_caller(None);
            manager.shutdown().await;
        }
        let closed = self.store.close().await;
        result.and(closed)
    }

    async fn run_inner(&mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let configured = self.config.restore_generation;
        self.store
            .call(move |store| initialize_generation(store.as_mut().unwrap(), configured))
            .await?;
        let (sender, relay) = mpsc::channel(8);
        if let Some(cfg) = &self.config.claude_code {
            self.claude_version =
                match crate::adapter::claude_code::version::check(&cfg.executable).await {
                    Ok(version) => Some(version),
                    Err(reason) => {
                        tracing::warn!(
                            kind = "claude_code_unavailable",
                            ?reason,
                            "Claude Code omitted from catalog"
                        );
                        None
                    }
                };
        }
        if let Some(cfg) = &self.config.codex {
            self.codex_catalog = match crate::adapter::codex::discover(cfg).await {
                Ok(catalog) => Some(catalog),
                Err(reason) => {
                    tracing::warn!(
                        kind = "codex_unavailable",
                        ?reason,
                        "Codex omitted from catalog"
                    );
                    None
                }
            };
        }
        let manager = ExecutionManager::new_with_attachments(
            self.config.execution.clone(),
            self.store.client(),
            sender,
            self.config.attachments.clone(),
        );
        if let Some(cfg) = &self.config.opencode {
            self.opencode_catalog =
                match crate::adapter::opencode::discover(cfg, &self.config.data_dir).await {
                    Ok(catalog) => {
                        if catalog.models.is_empty() {
                            tracing::warn!(
                                kind = "opencode_no_models",
                                "OpenCode has no available models"
                            );
                        }
                        Some(catalog)
                    }
                    Err(reason) => {
                        tracing::warn!(
                            kind = "opencode_unavailable",
                            ?reason,
                            "OpenCode omitted from catalog"
                        );
                        None
                    }
                };
        }
        manager.configure_adapters(
            self.config.claude_code.clone(),
            self.config.codex.clone(),
            self.config.opencode.clone(),
        );
        manager
            .startup_verify()
            .await
            .map_err(|_| WorkerError::Execution)?;
        self.manager = Some(manager);
        self.relay = Some(relay);
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
            self.config.machine.metrics_interval,
        ]
        .contains(&Duration::ZERO)
            || timers.backoff_min > timers.backoff_max
            || !self.config.attachments.valid()
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
        let mut refusal = None;
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
            if let Some(task) = self.metrics_task.take() {
                let _ = task.await;
            }
            self.manager.as_ref().unwrap().set_attachment_source(None);
            self.manager.as_ref().unwrap().set_media_sink(None);
            self.manager.as_ref().unwrap().set_agent_caller(None);
            let error = result.err().unwrap_or(WorkerError::Transport);
            let last_error = error.kind();
            if live_since.is_some() {
                refusal = None;
            }
            if let WorkerError::Hub { code, message } = &error {
                if refused_record(message) && refusal.as_ref() != Some(message) {
                    tracing::error!(kind = "journal_refused", reason = %message,
                        "hub refused a journal record; retrying until hub or worker is updated");
                    refusal = Some(message.clone());
                }
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
        let cj_pilot = self
            .store
            .call(|slot| {
                let store = slot.as_ref().unwrap();
                Ok(store.journal_next_requires_pilot(store.cj_journal_id()?)?)
            })
            .await?;
        let initial_sj = self
            .manager
            .as_ref()
            .unwrap()
            .permitted_cursors(false)
            .await?;
        let mut hello = Hello {
            machine_time_ms: crate::machine::now_ms(),
            installation_id: self.config.installation_id.to_string(),
            machine_id: self.config.machine_id.to_string(),
            restore_generation: generation.to_string(),
            protocol_version: PROTOCOL_VERSION,
            connection_id: connection_id.to_string(),
            cj_journal_id: cursor.journal_id.clone(),
            cursors: (!cj_pilot)
                .then_some(cursor.clone())
                .into_iter()
                .chain(initial_sj)
                .collect(),
            boot_id: boot_id.to_owned(),
            stream_kind: StreamKind::Control as i32,
            required_capabilities: vec![],
            optional_capabilities: OPTIONAL_CAPABILITIES
                .iter()
                .map(|cap| (*cap).into())
                .collect(),
        };
        let mut control = WorkerControlClient::with_interceptor(control, auth.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        // Третье соединение подключается при первой загрузке картинки.
        let media = WorkerDataClient::with_interceptor(endpoint.connect_lazy(), auth.clone())
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let mut data = WorkerDataClient::with_interceptor(data, auth)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        let (tx, rx) = mpsc::channel(1);
        let (data_tx, data_rx) = mpsc::channel(8);
        let (mut inbound, data_inbound, accepted) = timeout(self.config.timers.handshake, async {
            hello.machine_time_ms = crate::machine::now_ms();
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
            let mut accepted = match inbound
                .message()
                .await
                .map_err(status_error)?
                .and_then(|message| message.msg)
            {
                Some(control_down::Msg::HelloReply(reply)) => accept(reply)?,
                _ => return Err(WorkerError::Protocol),
            };
            check_generation(generation, &accepted)?;
            let pilot = accepted.capabilities.iter().any(|c| c == "pilot_v1");
            if pilot {
                hello.cursors = std::iter::once(cursor.clone())
                    .chain(self.manager.as_ref().unwrap().journal_cursors())
                    .collect();
            }
            hello.stream_kind = StreamKind::Data as i32;
            hello.machine_time_ms = crate::machine::now_ms();
            send(
                &data_tx,
                DataUp {
                    msg: Some(data_up::Msg::Hello(hello.clone())),
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
            for remote in data_accepted.committed {
                if !accepted
                    .committed
                    .iter()
                    .any(|c| c.journal_id == remote.journal_id)
                {
                    accepted.committed.push(remote);
                }
            }
            Ok((inbound, data_inbound, accepted))
        })
        .await
        .map_err(|_| WorkerError::Timeout)??;
        let pilot = accepted.capabilities.iter().any(|cap| cap == "pilot_v1");
        let diagnostics = JournalDiagnostics::default();
        if cj_pilot && !pilot {
            diagnostics
                .withheld(Uuid::parse_str(&cursor.journal_id).map_err(|_| WorkerError::Protocol)?);
        }
        if !pilot {
            for local in self.manager.as_ref().unwrap().journal_cursors() {
                if !hello
                    .cursors
                    .iter()
                    .any(|c| c.journal_id == local.journal_id)
                {
                    diagnostics.withheld(
                        Uuid::parse_str(&local.journal_id).map_err(|_| WorkerError::Protocol)?,
                    );
                }
            }
        }
        let exec_enabled = accepted.capabilities.iter().any(|cap| cap == "exec_v1");
        let suggest_enabled = accepted
            .capabilities
            .iter()
            .any(|cap| cap == "workdir_suggest_v1");
        self.manager
            .as_ref()
            .unwrap()
            .set_attachment_source(pilot.then(|| {
                std::sync::Arc::new(GrpcAttachmentSource {
                    client: data.clone(),
                }) as std::sync::Arc<dyn AttachmentSource>
            }));
        self.manager
            .as_ref()
            .unwrap()
            .set_media_sink(pilot.then(|| {
                std::sync::Arc::new(GrpcMediaSink {
                    client: media.clone(),
                }) as std::sync::Arc<dyn MediaSink>
            }));
        self.manager
            .as_ref()
            .unwrap()
            .set_agent_caller(pilot.then(|| {
                std::sync::Arc::new(GrpcAgentCaller {
                    client: data.clone(),
                }) as std::sync::Arc<dyn crate::exec::AgentCaller>
            }));
        self.manager.as_ref().unwrap().set_enabled(exec_enabled);
        self.transition(SessionState::Reconciling);
        check_generation(generation, &accepted)?;
        let withheld_cj = cj_pilot && !pilot;
        let committed = accepted
            .committed
            .iter()
            .find(|c| c.journal_id == cursor.journal_id)
            .map_or(
                if withheld_cj {
                    cursor.acked_seq_known_to_worker
                } else {
                    0
                },
                |c| c.committed_seq,
            );
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
        self.manager.as_ref().unwrap().on_cj_ack(committed);
        let replay_end = if withheld_cj {
            committed
        } else {
            cursor.last_seq
        };
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
        let mut report_outbound = None;
        let mut report_created = false;
        let mut reconciled = false;
        let mut machine_outbound = if pilot {
            let config = self.config.clone();
            let cli = self.claude_version.clone();
            let codex = self.codex_catalog.clone();
            let opencode = self.opencode_catalog.clone();
            Some(ControlUp {
                msg: Some(control_up::Msg::MachineReport(
                    tokio::task::spawn_blocking(move || {
                        crate::machine::build_report(
                            &config,
                            &crate::adapter::catalog_with(
                                config.claude_code.as_ref(),
                                cli.as_ref(),
                                codex.as_ref(),
                                opencode.as_ref(),
                            ),
                        )
                    })
                    .await
                    .map_err(|_| WorkerError::StoreThread)?,
                )),
            })
        } else {
            None
        };
        let mut metrics_at = Instant::now() + self.config.machine.metrics_interval;
        // Провайдер и источник запроса: `true` — тикер простоя, `false` — запрос хаба.
        let mut polls: VecDeque<(String, bool)> = VecDeque::new();
        let mut limits_outbound = None;
        type LimitsFuture = Pin<Box<dyn Future<Output = Vec<LimitObservation>> + Send>>;
        let mut limits_operation: Option<LimitsFuture> = None;
        // Ждёт только последний запрос: пока идёт листинг, ввод уже ушёл дальше.
        let mut suggest_next: Option<SuggestWorkdirs> = None;
        let mut suggest_outbound = None;
        type SuggestFuture = Pin<Box<dyn Future<Output = WorkdirSuggestions> + Send>>;
        let mut suggest_operation: Option<SuggestFuture> = None;
        let session_state = self.state.clone();
        let manager = self.manager.as_ref().unwrap();
        let (data_state, mut data_attached) = watch::channel(true);
        let (activate_data, data_active) = watch::channel(false);
        let data_loop = data::run(
            data::Stream {
                #[cfg(feature = "fault-injection")]
                probe: self.probe.clone(),
                pilot,
                diagnostics: diagnostics.clone(),
                live_limits: self.live_limits.clone(),
                client: data,
                hello: hello.clone(),
                tx: data_tx,
                inbound: data_inbound,
                committed: data::session_committed(
                    &hello,
                    &accepted,
                    self.manager.as_ref().unwrap(),
                )?,
            },
            manager,
            self.relay.as_mut().unwrap(),
            &self.config.timers,
            data_state,
            self.store.client(),
            data_active,
        );
        tokio::pin!(data_loop);
        let idle_ticker = &mut self.idle_ticker;
        type StoreFuture = Pin<Box<dyn Future<Output = Result<StoreReply, WorkerError>> + Send>>;
        let mut operation: Option<StoreFuture> = None;
        type ExecutionFuture<'a> =
            Pin<Box<dyn Future<Output = Result<(), WorkerError>> + Send + 'a>>;
        let mut execution_operation: Option<ExecutionFuture<'_>> = None;
        loop {
            if limits_operation.is_none()
                && limits_outbound.is_none()
                && let Some((provider, idle)) = polls.pop_front()
            {
                let source = self.limits_source.clone();
                limits_operation = Some(Box::pin(async move {
                    if idle {
                        source.poll_idle(&provider).await
                    } else {
                        source.poll(&provider).await
                    }
                }));
            }
            if suggest_operation.is_none()
                && suggest_outbound.is_none()
                && let Some(request) = suggest_next.take()
            {
                let workdirs = self.workdirs.clone();
                suggest_operation = Some(Box::pin(async move {
                    WorkdirSuggestions {
                        directories: workdirs.suggest(request.path).await,
                        request_id: request.request_id,
                    }
                }));
            }
            if !report_created && acknowledged >= replay_end {
                report_created = true;
                if exec_enabled {
                    let report = ControlUp {
                        msg: Some(control_up::Msg::ReconcileReport(ReconcileReport {
                            executions: manager
                                .observations()
                                .into_iter()
                                .map(|mut observation| {
                                    if !pilot
                                        && !hello
                                            .cursors
                                            .iter()
                                            .any(|c| c.journal_id == observation.sj_journal_id)
                                    {
                                        observation.sj_journal_id.clear();
                                    }
                                    observation
                                })
                                .collect(),
                        })),
                    };
                    if report.encoded_len() > MAX_MESSAGE_BYTES {
                        return Err(WorkerError::Protocol);
                    }
                    report_outbound = Some(report);
                } else {
                    manager.on_hub_session(data::session_committed(
                        &hello,
                        &accepted,
                        self.manager.as_ref().unwrap(),
                    )?);
                    activate_data.send_replace(true);
                    reconciled = true;
                    if *data_attached.borrow() {
                        *live_since = Some(Instant::now());
                        transition(&session_state, SessionState::Live);
                    }
                }
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
                        let store = store.as_ref().unwrap();
                        let withheld = !pilot
                            && store
                                .cj_records_from(from, 0)?
                                .first()
                                .and_then(|r| r.body.as_ref())
                                .is_some_and(crate::store::requires_pilot);
                        Ok(StoreReply::Batch(
                            from,
                            batch_for_session(store, from, pilot)?.map(Box::new),
                            withheld,
                        ))
                    })));
                    dirty = false;
                } else if execution_operation.is_none()
                    && let Some(command) = commands.pop_front()
                {
                    operation = Some(Box::pin(self.store.call(move |store| {
                        let accept = store
                            .as_mut()
                            .unwrap()
                            .accept_command_with_pilot(&command, pilot)?;
                        Ok(StoreReply::Command(Box::new(command), accept))
                    })));
                }
            }
            let heartbeat_deadline = pending_heartbeat
                .as_ref()
                .map_or(heartbeat_at, |p| p.deadline.unwrap());
            let batch_deadline = pending_batches.front().and_then(|p| p.deadline);
            let can_send = heartbeat_outbound.is_some()
                || machine_outbound.is_some()
                || limits_outbound.is_some()
                || suggest_outbound.is_some()
                || report_outbound.is_some()
                || range_outbound.is_some()
                || (ready_batch.is_some() && pending_batches.len() < 8);
            tokio::select! {
                message = inbound.message(), if commands.len() < 32 && polls.len()<8 => {
                    let message = message.map_err(status_error)?.ok_or(WorkerError::Transport)?;
                    match message.msg {
                        Some(control_down::Msg::PollLimits(request)) if pilot => {polls.push_back((request.provider, false));}
                        Some(control_down::Msg::LimitsSnapshot(snapshot)) if pilot => {self.limits_cache.update(snapshot);}
                        Some(control_down::Msg::SuggestWorkdirs(request)) if suggest_enabled => {suggest_next = Some(request);}
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
                                #[cfg(feature = "fault-injection")]
                                self.probe.ack(ack.seq);
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
                        StoreReply::Command(command, accept) => {
                            dirty |= matches!(accept, Accept::Stored { .. } | Accept::Redelivered { .. } | Accept::Rejected { .. });
                            // Без exec_v1 сохраняется протокол воркеров 1.2a.
                            if exec_enabled || matches!(command.kind, Some(command::Kind::StartExecution(_))) {
                                execution_operation = Some(Box::pin(async move {
                                    manager.handle(*command, accept).await.map_err(|_| WorkerError::Execution)
                                }));
                            }
                        },
                        StoreReply::Ack(up_to) => {
                            acknowledged = acknowledged.max(up_to);
                            self.manager.as_ref().unwrap().on_cj_ack(up_to);
                        },
                        StoreReply::Batch(from, result,withheld) => {
                            if withheld {diagnostics.withheld(Uuid::parse_str(&journal_id).map_err(|_|WorkerError::Protocol)?);}
                            if from == next_seq { ready_batch = result.map(|batch| *batch); }
                            else { dirty = true; }
                        }
                    }
                }
                result = async { execution_operation.as_mut().unwrap().await }, if execution_operation.is_some() => {
                    execution_operation = None;
                    result?;
                    dirty = true;
                }
                permit = tx.reserve(), if can_send => {
                    let permit = permit.map_err(|_| WorkerError::Transport)?;
                    let mut message = if let Some(message) = heartbeat_outbound.take() { message }
                    else if let Some(message)=machine_outbound.take() {message}
                    else if let Some(message)=limits_outbound.take() {message}
                    else if let Some(message)=suggest_outbound.take() {message}
                    else if let Some(message) = report_outbound.take() {
                        manager.on_hub_session(data::session_committed(&hello, &accepted,self.manager.as_ref().unwrap())?);
                        activate_data.send_replace(true);
                        reconciled = true;
                        if *data_attached.borrow() {
                            *live_since = Some(Instant::now());
                            transition(&session_state, SessionState::Live);
                        }
                        message
                    }
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
                    if let Some(control_up::Msg::Heartbeat(heartbeat))=&mut message.msg {heartbeat.machine_time_ms=crate::machine::now_ms();}
                    permit.send(message);
                }
                _ = sleep_until(metrics_at), if pilot => {
                    metrics_at=Instant::now()+self.config.machine.metrics_interval;
                    if self.metrics_task.is_none() {
                        let source=self.metrics.clone();let probes=manager.probes();
                        self.metrics_task=Some(Box::pin(async move{source.sample(probes).await}));
                    }
                }
                result = async {self.metrics_task.as_mut().unwrap().await}, if self.metrics_task.is_some() => {
                    self.metrics_task=None;
                    if let Ok(sample)=result && heartbeat_outbound.is_none() && Instant::now()<heartbeat_deadline {try_send_metrics(&tx,sample);}
                }
                due = idle_ticker.tick(), if pilot => {
                    for provider in due {
                        if polls.len() < 8 && !polls.iter().any(|(queued, _)| *queued == provider) { polls.push_back((provider, true)); }
                    }
                }
                observations=async{limits_operation.as_mut().unwrap().await}, if limits_operation.is_some()=>{
                    limits_operation=None;
                    // Хаб держит один необработанный отчёт: пустой вытеснил бы настоящий.
                    if !observations.is_empty() {limits_outbound=Some(ControlUp{msg:Some(control_up::Msg::LimitsReport(LimitsReport{observations}))});}
                }
                reply=async{suggest_operation.as_mut().unwrap().await}, if suggest_operation.is_some()=>{
                    suggest_operation=None;suggest_outbound=Some(ControlUp{msg:Some(control_up::Msg::WorkdirSuggestions(reply))});
                }
                result = &mut data_loop => return result,
                changed = data_attached.changed() => {
                    changed.map_err(|_| WorkerError::Transport)?;
                    if *data_attached.borrow_and_update() {
                        if reconciled {
                            *live_since = Some(Instant::now());
                            transition(&session_state, SessionState::Live);
                        }
                    } else {
                        *live_since = None;
                        transition(&session_state, SessionState::ControlOnly);
                    }
                }
                _ = sleep_until(heartbeat_deadline) => {
                    dirty = true;
                    if let Some(pending) = &mut pending_heartbeat {
                        heartbeat_outbound = Some(pending.resend(self.config.timers.heartbeat_ack)?);
                    } else {
                        heartbeat_seq += 1;
                        #[cfg(feature = "fault-injection")]
                        self.probe.sent(heartbeat_seq);
                        let message = ControlUp { msg: Some(control_up::Msg::Heartbeat(Heartbeat { seq: heartbeat_seq, machine_time_ms: crate::machine::now_ms() })) };
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
    Command(Box<Command>, Accept),
    Ack(u64),
    Batch(u64, Option<Box<(ControlUp, u64)>>, bool),
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

fn transition(sender: &watch::Sender<SessionState>, state: SessionState) {
    tracing::info!(state = state_kind(&state), "session state");
    sender.send_replace(state);
}

fn batch_for_session(
    store: &WorkerStore,
    from: u64,
    pilot: bool,
) -> Result<Option<(ControlUp, u64)>, WorkerError> {
    let mut batch = JournalRecords {
        journal_id: store.cj_journal_id()?.to_string(),
        records: vec![],
    };
    for record in store
        .cj_records_from(from, MAX_EVENT_BATCH_BYTES)?
        .into_iter()
        .take(256)
    {
        if !pilot
            && record
                .body
                .as_ref()
                .is_some_and(crate::store::requires_pilot)
        {
            break;
        }
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
