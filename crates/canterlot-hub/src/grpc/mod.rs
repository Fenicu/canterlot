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

use crate::{config::HubTimers, installation::Installation, session::Registry};

mod auth;
mod control;
mod data;
mod revocation;

pub const PROTOCOL_VERSION: u32 = canterlot_proto::limits::PROTOCOL_VERSION;
pub const KNOWN_CAPABILITIES: &[&str] = &[];

#[tonic::async_trait]
/// ACK возвращается только после commit всех записей диапазона.
pub trait Ingest: Send + Sync {
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

/// Приём журнала потока в отдельной задаче: подтверждения уходят строго по порядку,
/// а цикл потока продолжает обслуживать heartbeat и команды.
struct IngestLane {
    batches: mpsc::Sender<JournalRecords>,
    acks: mpsc::Receiver<Result<JournalRangeAck, Status>>,
    _stop: DropGuard,
}

impl IngestLane {
    fn start(
        ingest: Arc<dyn Ingest>,
        machine_id: Uuid,
        credential: Vec<u8>,
        kind: LaneKind,
    ) -> Self {
        let (batches, mut queue) = mpsc::channel::<JournalRecords>(INGEST_BACKLOG);
        let (reply, acks) = mpsc::channel(INGEST_BACKLOG);
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        tokio::spawn(async move {
            while let Some(records) = tokio::select! {
                biased;
                _ = cancelled.cancelled() => None,
                records = queue.recv() => records,
            } {
                let applied = async {
                    match kind {
                        LaneKind::Cj => ingest.cj_records(machine_id, &credential, records).await,
                        LaneKind::Sj => ingest.sj_records(machine_id, &credential, records).await,
                    }
                };
                let result = tokio::select! {
                    biased;
                    _ = cancelled.cancelled() => break,
                    result = applied => result,
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
        }
    }

    #[allow(clippy::result_large_err)]
    fn submit(&self, records: JournalRecords) -> Result<(), Status> {
        self.batches
            .try_send(records)
            .map_err(|_| Status::resource_exhausted("ingest_backlog"))
    }

    async fn next_ack(&mut self) -> Option<Result<JournalRangeAck, Status>> {
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
        let ingest = Arc::new(crate::ingest::Ingest::new(pool.clone()));
        Self {
            pool,
            registry,
            installation,
            timers,
            ingest,
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
            shutdown: CancellationToken::new(),
        }
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
        let (owner_id, previous): (Uuid, Option<Uuid>) = sqlx::query_as(
            "select owner_id, cj_journal_id from machines where id = $1 for no key update",
        )
        .bind(machine_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| Status::internal("database"))?;
        if !crate::machines::credential_active(&mut tx, machine_id, &credential)
            .await
            .map_err(|_| Status::internal("database"))?
        {
            return Err(Status::unauthenticated("credential_revoked"));
        }
        if let Some(previous) = previous.filter(|previous| *previous != journal_id) {
            tracing::warn!(kind = "local_state_reset", machine_id = %machine_id,
                old_journal_id = %previous, new_journal_id = %journal_id, "local state reset");
            crate::events::append(&mut tx, crate::events::NewEvent {
                scope: crate::events::Scope::Conversations { owner_id },
                kind: "local_state_reset",
                conversation_id: None,
                execution_id: None,
                payload: serde_json::json!({"machine_id": machine_id, "old_journal_id": previous, "new_journal_id": journal_id}),
                correlation_id: Uuid::now_v7(),
            }).await.map_err(|_| Status::internal("database"))?;
            tx.commit()
                .await
                .map_err(|_| Status::internal("database"))?;
            return Ok(Err(rejection(RejectReason::LocalStateReset)));
        }
        let Some(session) = self
            .registry
            .register(machine_id, connection_id, credential)
        else {
            return Ok(Err(rejection(RejectReason::IdentityInUse)));
        };
        sqlx::query("update machines set cj_journal_id = $2 where id = $1")
            .bind(machine_id)
            .bind(journal_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| Status::internal("database"))?;
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

    async fn accepted(&self, machine_id: Uuid) -> Result<HelloReply, Status> {
        let cursors: Vec<(Uuid, i64)> = sqlx::query_as("select journal_id, committed_seq from journal_cursors where machine_id = $1 order by journal_id")
            .bind(machine_id).fetch_all(&self.pool).await.map_err(|_| Status::internal("journal cursors unavailable"))?;
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
