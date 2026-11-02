use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};

use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::v1::{
        worker_control_server::WorkerControlServer, worker_data_server::WorkerDataServer, *,
    },
};
use prost::Message;
use sqlx::PgPool;
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::{Stream, wrappers::TcpListenerStream};
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::{
    Status,
    transport::{Identity, Server, ServerTlsConfig, server::Router},
};
use uuid::Uuid;

use crate::{
    config::HubTimers,
    installation::Installation,
    session::{
        CAPABILITY_BACKGROUND_V1, CAPABILITY_COMPACT_V1, CAPABILITY_EXEC_V1,
        CAPABILITY_INSTRUCTIONS_V1, CAPABILITY_OWNER_EXIT_V1, CAPABILITY_OWNER_INSTRUCTIONS_V1,
        CAPABILITY_PILOT_V1, CAPABILITY_SEND_NOW_V1, CAPABILITY_SESSION_TOOLS_V1,
        CAPABILITY_TODO_V1, CAPABILITY_WORKDIR_SUGGEST_V1, Registry,
    },
};

mod agent;
mod attachments;
mod auth;
mod control;
mod data;
mod media;
mod revocation;

pub const PROTOCOL_VERSION: u32 = canterlot_proto::limits::PROTOCOL_VERSION;
pub const KNOWN_CAPABILITIES: &[&str] = &[];
pub const KNOWN_OPTIONAL_CAPABILITIES: &[&str] = &[
    CAPABILITY_EXEC_V1,
    CAPABILITY_PILOT_V1,
    CAPABILITY_INSTRUCTIONS_V1,
    CAPABILITY_TODO_V1,
    CAPABILITY_COMPACT_V1,
    CAPABILITY_BACKGROUND_V1,
    CAPABILITY_SESSION_TOOLS_V1,
    CAPABILITY_SEND_NOW_V1,
    CAPABILITY_OWNER_EXIT_V1,
    CAPABILITY_WORKDIR_SUGGEST_V1,
    CAPABILITY_OWNER_INSTRUCTIONS_V1,
];

#[tonic::async_trait]
/// ACK возвращается только после commit всех записей диапазона.
pub trait Ingest: Send + Sync {
    async fn reconcile(&self, machine_id: Uuid, report: ReconcileReport) -> Result<(), Status>;
    async fn machine_report(
        &self,
        _machine_id: Uuid,
        _report: MachineReport,
    ) -> Result<(), Status> {
        Ok(())
    }
    async fn machine_metrics(
        &self,
        _machine_id: Uuid,
        _metrics: MachineMetrics,
    ) -> Result<(), Status> {
        Ok(())
    }
    async fn limits_report(&self, _machine_id: Uuid, _report: LimitsReport) -> Result<(), Status> {
        Ok(())
    }
    async fn cj_records(
        &self,
        machine_id: uuid::Uuid,
        credential: &[u8],
        records: JournalRecords,
    ) -> Result<JournalRangeAck, Status>;
    async fn sj_records(
        &self,
        machine_id: uuid::Uuid,
        credential: &[u8],
        records: JournalRecords,
    ) -> Result<JournalRangeAck, Status>;
}

#[derive(Clone)]
pub struct HubGrpc {
    pub pool: PgPool,
    pub registry: Registry,
    pub installation: Installation,
    pub timers: HubTimers,
    pub ingest: Arc<dyn Ingest>,
    pub session_tools: Arc<crate::config::SessionToolsConfig>,
    pub settle: crate::session_tools::settle::SettleConfig,
    pub dispatcher: Option<crate::dispatcher::Dispatcher>,
    blobs: Option<Arc<dyn crate::attachments::BlobStore>>,
    fetch_slots: attachments::FetchSlots,
    media_slots: media::UploadSlots,
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
    shutdown: CancellationToken,
}

const INGEST_BACKLOG: usize = 32;

#[derive(Clone, Copy)]
enum LaneKind {
    Cj,
    Sj,
}

enum IngestJob {
    Records(JournalRecords),
    Reconcile(ReconcileReport),
    MachineReport(MachineReport),
    MachineMetrics(MachineMetrics),
    LimitsReport(LimitsReport),
}

/// Метрики и опрос лимитов теряются без остановки потока: следующий образец заменит их.
fn volatile(result: Result<(), Status>) {
    if result.is_err() {
        tracing::warn!(kind = "volatile_ingest_failed", "volatile ingest failed");
    }
}

/// Приём журнала потока в отдельной задаче: подтверждения уходят строго по порядку,
/// а цикл потока продолжает обслуживать heartbeat и команды.
struct IngestLane {
    batches: mpsc::Sender<IngestJob>,
    acks: mpsc::Receiver<Result<Option<JournalRangeAck>, Status>>,
    _stop: DropGuard,
    limits: tokio::sync::watch::Sender<Option<LimitsReport>>,
}

impl IngestLane {
    fn start(
        ingest: Arc<dyn Ingest>,
        machine_id: Uuid,
        credential: Vec<u8>,
        kind: LaneKind,
    ) -> Self {
        let (batches, mut queue) = mpsc::channel::<IngestJob>(INGEST_BACKLOG);
        let (reply, acks) = mpsc::channel(INGEST_BACKLOG);
        let (limits, mut latest) = tokio::sync::watch::channel(None);
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        tokio::spawn(async move {
            while let Some(job) = tokio::select! {
                biased;
                _ = cancelled.cancelled() => None,
                records = queue.recv() => records,
                changed=latest.changed()=>if changed.is_ok(){latest.borrow_and_update().clone().map(IngestJob::LimitsReport)}else{None},
            } {
                let applied = async {
                    match job {
                        IngestJob::Records(records) => Some(
                            match kind {
                                LaneKind::Cj => {
                                    ingest.cj_records(machine_id, &credential, records).await
                                }
                                LaneKind::Sj => {
                                    ingest.sj_records(machine_id, &credential, records).await
                                }
                            }
                            .map(Some),
                        ),
                        IngestJob::Reconcile(report) => {
                            Some(ingest.reconcile(machine_id, report).await.map(|()| None))
                        }
                        // Отчёт машины применяется по порядку; сбой закрывает поток, воркер пришлёт отчёт заново.
                        IngestJob::MachineReport(report) => ingest
                            .machine_report(machine_id, report)
                            .await
                            .err()
                            .map(Err),
                        IngestJob::MachineMetrics(metrics) => {
                            volatile(ingest.machine_metrics(machine_id, metrics).await);
                            None
                        }
                        IngestJob::LimitsReport(report) => {
                            volatile(ingest.limits_report(machine_id, report).await);
                            None
                        }
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => break,
                    result = applied => result,
                };
                let Some(result) = result else {
                    continue;
                };
                let failed = result.is_err();
                if reply.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            batches,
            acks,
            _stop: stop.drop_guard(),
            limits,
        }
    }

    #[allow(clippy::result_large_err)]
    fn submit(&self, records: JournalRecords) -> Result<(), Status> {
        self.batches
            .try_send(IngestJob::Records(records))
            .map_err(|_| Status::resource_exhausted("ingest_backlog"))
    }

    #[allow(clippy::result_large_err)]
    fn submit_reconcile(&self, report: ReconcileReport) -> Result<(), Status> {
        self.batches
            .try_send(IngestJob::Reconcile(report))
            .map_err(|_| Status::resource_exhausted("ingest_backlog"))
    }

    #[allow(clippy::result_large_err)]
    fn submit_report(&self, report: MachineReport) -> Result<(), Status> {
        self.batches
            .try_send(IngestJob::MachineReport(report))
            .map_err(|_| Status::resource_exhausted("ingest_backlog"))
    }
    fn submit_metrics(&self, metrics: MachineMetrics) {
        let _ = self.batches.try_send(IngestJob::MachineMetrics(metrics));
    }
    fn submit_limits(&self, report: LimitsReport) {
        self.limits.send_replace(Some(report));
    }

    async fn next_ack(&mut self) -> Option<Result<Option<JournalRangeAck>, Status>> {
        self.acks.recv().await
    }
}

type ReplyStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

fn rejection_kind(reason: i32) -> &'static str {
    match RejectReason::try_from(reason).unwrap_or(RejectReason::Unspecified) {
        RejectReason::CredentialRevoked => "credential_revoked",
        RejectReason::InstallationMismatch => "installation_mismatch",
        RejectReason::ProtocolIncompatible => "protocol_incompatible",
        RejectReason::UnknownRequiredCapability => "unknown_required_capability",
        RejectReason::IdentityInUse => "identity_in_use",
        RejectReason::ConnectionBindingMismatch => "connection_binding_mismatch",
        RejectReason::HubNotOperational => "hub_not_operational",
        RejectReason::LocalStateReset => "local_state_reset",
        RejectReason::Unspecified => "unspecified",
    }
}

fn rejection(reason: RejectReason) -> Rejected {
    Rejected {
        reason: reason as i32,
        detail: String::new(),
        retryable: matches!(
            reason,
            RejectReason::IdentityInUse
                | RejectReason::ConnectionBindingMismatch
                | RejectReason::HubNotOperational
        ),
    }
}

#[allow(clippy::result_large_err)]
fn checked<T: Message>(message: T) -> Result<T, Status> {
    if message.encoded_len() > MAX_MESSAGE_BYTES {
        Err(Status::resource_exhausted("outgoing message exceeds limit"))
    } else {
        Ok(message)
    }
}

impl HubGrpc {
    pub fn new(
        pool: PgPool,
        registry: Registry,
        installation: Installation,
        timers: HubTimers,
    ) -> Self {
        let ingest =
            Arc::new(crate::ingest::Ingest::new(pool.clone()).with_registry(registry.clone()));
        Self {
            pool,
            registry,
            installation,
            timers,
            ingest,
            session_tools: Default::default(),
            settle: (&crate::config::ActivityConfig::default()).into(),
            dispatcher: None,
            blobs: None,
            fetch_slots: Default::default(),
            media_slots: Default::default(),
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
            shutdown: CancellationToken::new(),
        }
    }

    pub fn with_session_tools(mut self, config: crate::config::SessionToolsConfig) -> Self {
        self.session_tools = Arc::new(config);
        self
    }

    pub fn with_activity(mut self, config: &crate::config::ActivityConfig) -> Self {
        self.settle = config.into();
        self
    }

    pub fn with_dispatcher(mut self, dispatcher: crate::dispatcher::Dispatcher) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    pub fn with_blob_store(mut self, blobs: Arc<dyn crate::attachments::BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Приём заметил отзыв раньше слушателя: сессия помечается отозванной до закрытия потока,
    /// иначе парный поток закончится без `Unauthenticated`.
    fn revoked_by_ingest(&self, machine_id: Uuid, connection_id: Uuid, status: Status) -> Status {
        if status.code() == tonic::Code::Unauthenticated && status.message() == "credential_revoked"
        {
            self.registry.revoke(machine_id, connection_id);
        }
        status
    }

    async fn admit_control(
        &self,
        machine_id: Uuid,
        connection_id: Uuid,
        journal_id: Uuid,
        credential: Vec<u8>,
    ) -> Result<Result<(crate::session::ControlLease, mpsc::Receiver<ControlDown>), Rejected>, Status>
    {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| Status::internal("database"))?;
        let (owner_id, previous): (Uuid, Option<Uuid>) = match sqlx::query_as(
            "select owner_id, cj_journal_id from machines where id = $1 for no key update",
        )
        .bind(machine_id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(_) => {
                let _ = tx.rollback().await;
                return Err(Status::internal("database"));
            }
        };
        let credential_active =
            match crate::machines::credential_active(&mut tx, machine_id, &credential).await {
                Ok(active) => active,
                Err(_) => {
                    let _ = tx.rollback().await;
                    return Err(Status::internal("database"));
                }
            };
        if !credential_active {
            let _ = tx.rollback().await;
            return Err(Status::unauthenticated("credential_revoked"));
        }
        if let Some(previous) = previous.filter(|previous| *previous != journal_id) {
            tracing::warn!(kind = "local_state_reset", machine_id = %machine_id,
                old_journal_id = %previous, new_journal_id = %journal_id, "local state reset");
            match crate::events::append(&mut tx, crate::events::NewEvent {
                scope: crate::events::Scope::Conversations { owner_id },
                kind: "local_state_reset",
                conversation_id: None,
                execution_id: None,
                payload: serde_json::json!({"machine_id": machine_id, "old_journal_id": previous, "new_journal_id": journal_id}),
                correlation_id: Uuid::now_v7(),
            }).await {
                Ok(_) => {},
                Err(_) => {
                    let _ = tx.rollback().await;
                    return Err(Status::internal("database"));
                }
            }
            tx.commit()
                .await
                .map_err(|_| Status::internal("database"))?;
            return Ok(Err(rejection(RejectReason::LocalStateReset)));
        }
        let Some(session) = self
            .registry
            .register(machine_id, connection_id, credential)
        else {
            let _ = tx.rollback().await;
            return Ok(Err(rejection(RejectReason::IdentityInUse)));
        };
        match sqlx::query("update machines set cj_journal_id = $2 where id = $1")
            .bind(machine_id)
            .bind(journal_id)
            .execute(&mut *tx)
            .await
        {
            Ok(_) => {}
            Err(_) => {
                let _ = tx.rollback().await;
                return Err(Status::internal("database"));
            }
        }
        tx.commit()
            .await
            .map_err(|_| Status::internal("database"))?;
        Ok(Ok(session))
    }

    fn validate_hello(
        &self,
        hello: &Hello,
        machine_id: Uuid,
        kind: StreamKind,
    ) -> Result<Uuid, Rejected> {
        if Uuid::parse_str(&hello.installation_id).ok() != Some(self.installation.installation_id) {
            return Err(rejection(RejectReason::InstallationMismatch));
        }
        if Uuid::parse_str(&hello.restore_generation).ok()
            != Some(self.installation.restore_generation)
        {
            return Err(rejection(RejectReason::HubNotOperational));
        }
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(rejection(RejectReason::ProtocolIncompatible));
        }
        if hello
            .required_capabilities
            .iter()
            .any(|capability| !KNOWN_CAPABILITIES.contains(&capability.as_str()))
        {
            return Err(rejection(RejectReason::UnknownRequiredCapability));
        }
        if hello.stream_kind != kind as i32 {
            return Err(rejection(if kind == StreamKind::Data {
                RejectReason::ConnectionBindingMismatch
            } else {
                RejectReason::ProtocolIncompatible
            }));
        }
        if Uuid::parse_str(&hello.machine_id).ok() != Some(machine_id) {
            return Err(rejection(RejectReason::ConnectionBindingMismatch));
        }
        let connection_id = Uuid::parse_str(&hello.connection_id)
            .ok()
            .filter(|id| !id.is_nil())
            .ok_or_else(|| rejection(RejectReason::ConnectionBindingMismatch))?;
        if [&hello.cj_journal_id, &hello.boot_id]
            .into_iter()
            .chain(hello.cursors.iter().map(|cursor| &cursor.journal_id))
            .any(|id| Uuid::parse_str(id).ok().is_none_or(|id| id.is_nil()))
        {
            return Err(rejection(RejectReason::ProtocolIncompatible));
        }
        Ok(connection_id)
    }

    async fn accepted(&self, machine_id: Uuid, hello: &Hello) -> Result<HelloReply, Status> {
        let mut named = vec![Uuid::parse_str(&hello.cj_journal_id).expect("validated journal")];
        named.extend(
            hello
                .cursors
                .iter()
                .map(|cursor| Uuid::parse_str(&cursor.journal_id).expect("validated journal")),
        );
        named.sort_unstable();
        named.dedup();
        let cursors: Vec<(Uuid, i64)> = sqlx::query_as(
            "select named.journal_id, coalesce(c.committed_seq, 0) from unnest($2::uuid[]) as named(journal_id)
             left join journal_cursors c on c.journal_id = named.journal_id and c.machine_id = $1 order by named.journal_id",
        ).bind(machine_id).bind(&named).fetch_all(&self.pool).await.map_err(|_| Status::internal("journal cursors unavailable"))?;
        let committed: Vec<_> = cursors
            .into_iter()
            .map(|(journal_id, seq)| {
                Ok(JournalCommitted {
                    journal_id: journal_id.to_string(),
                    committed_seq: u64::try_from(seq)
                        .map_err(|_| Status::internal("invalid journal cursor"))?,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        #[cfg(feature = "fault-injection")]
        self.faults.accepted(&committed);
        Ok(HelloReply {
            result: Some(hello_reply::Result::Accepted(Accepted {
                capabilities: KNOWN_OPTIONAL_CAPABILITIES
                    .iter()
                    .filter(|known| {
                        hello
                            .optional_capabilities
                            .iter()
                            .any(|capability| capability == **known)
                    })
                    .map(|capability| (*capability).to_owned())
                    .collect(),
                restore_generation: self.installation.restore_generation.to_string(),
                committed,
            })),
        })
    }
}

fn router(state: HubGrpc, cert: &str, key: &str) -> Result<Router, tonic::transport::Error> {
    Ok(Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))?
        .http2_keepalive_interval(Some(state.timers.http2_keepalive_interval))
        .http2_keepalive_timeout(Some(state.timers.http2_keepalive_timeout))
        .add_service(
            WorkerControlServer::new(state.clone())
                .max_decoding_message_size(MAX_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_MESSAGE_BYTES),
        )
        .add_service(
            WorkerDataServer::new(state)
                .max_decoding_message_size(MAX_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_MESSAGE_BYTES),
        ))
}

pub async fn serve(
    state: HubGrpc,
    listen: SocketAddr,
    tls_cert_pem: &str,
    tls_key_pem: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let _revocations = revocation::start(state.pool.clone(), state.registry.clone()).await;
    let cancel = state.shutdown.clone();
    router(state, tls_cert_pem, tls_key_pem)?
        .serve_with_shutdown(listen, async move {
            shutdown.await;
            cancel.cancel();
        })
        .await
}

pub async fn serve_with_listener(
    state: HubGrpc,
    listener: TcpListener,
    tls_cert_pem: &str,
    tls_key_pem: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let _revocations = revocation::start(state.pool.clone(), state.registry.clone()).await;
    let cancel = state.shutdown.clone();
    router(state, tls_cert_pem, tls_key_pem)?
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
            shutdown.await;
            cancel.cancel();
        })
        .await
}

#[cfg(all(feature = "fault-injection", feature = "testkit"))]
pub(crate) async fn serve_test_with_goaway(
    state: HubGrpc,
    listener: TcpListener,
    cert: &str,
    key: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    let _revocations = revocation::start(state.pool.clone(), state.registry.clone()).await;
    let cancel = state.shutdown.clone();
    let faults = state.faults.clone();
    let mut goaway = faults.goaway_signals();
    let (graceful, graceful_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = router(state, cert, key)?.serve_with_incoming_shutdown(
        TcpListenerStream::new(listener),
        async {
            let _ = graceful_rx.await;
        },
    );
    tokio::pin!(serving, shutdown);
    tokio::select! {
        result = &mut serving => result,
        _ = &mut shutdown => {
            cancel.cancel();
            drop(graceful);
            serving.await
        }
        _ = goaway.recv() => {
            // Hyper закрывает приём новых потоков через GOAWAY, живые RPC остаются.
            drop(graceful);
            faults.goaway_started();
            tokio::select! {
                result = &mut serving => result,
                _ = &mut shutdown => { cancel.cancel(); serving.await }
            }
        }
    }
}
