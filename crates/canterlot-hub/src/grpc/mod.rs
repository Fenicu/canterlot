use std::{future::Future, net::SocketAddr, pin::Pin, sync::Arc};

use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::v1::{
        worker_control_server::WorkerControlServer, worker_data_server::WorkerDataServer, *,
    },
};
use prost::Message;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio_stream::{Stream, wrappers::TcpListenerStream};
use tonic::{
    Status,
    transport::{Identity, Server, ServerTlsConfig, server::Router},
};
use uuid::Uuid;

use crate::{config::HubTimers, installation::Installation, session::Registry};

mod auth;
mod control;
mod data;

pub const PROTOCOL_VERSION: u32 = canterlot_proto::limits::PROTOCOL_VERSION;
pub const KNOWN_CAPABILITIES: &[&str] = &[];

#[tonic::async_trait]
/// ACK возвращается только после commit всех записей диапазона.
pub trait Ingest: Send + Sync {
    async fn cj_records(
        &self,
        machine_id: uuid::Uuid,
        records: JournalRecords,
    ) -> Result<JournalRangeAck, Status>;
    async fn sj_records(
        &self,
        machine_id: uuid::Uuid,
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
}

type ReplyStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

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
        }
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
        let committed = cursors
            .into_iter()
            .map(|(journal_id, seq)| {
                Ok(JournalCommitted {
                    journal_id: journal_id.to_string(),
                    committed_seq: u64::try_from(seq)
                        .map_err(|_| Status::internal("invalid journal cursor"))?,
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
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
    router(state, tls_cert_pem, tls_key_pem)?
        .serve_with_shutdown(listen, shutdown)
        .await
}

pub async fn serve_with_listener(
    state: HubGrpc,
    listener: TcpListener,
    tls_cert_pem: &str,
    tls_key_pem: &str,
    shutdown: impl Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    router(state, tls_cert_pem, tls_key_pem)?
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
}
