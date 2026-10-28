use canterlot_proto::worker::v1::{worker_data_server::WorkerData, *};
use tonic::{Request, Response, Status, Streaming};

use super::{
    HubGrpc, IngestLane, LaneKind, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked, rejection,
};

#[tonic::async_trait]
impl WorkerData for HubGrpc {
    type FetchAttachmentStream = ReplyStream<canterlot_proto::worker::v1::AttachmentChunk>;

    async fn fetch_attachment(
        &self,
        request: Request<canterlot_proto::worker::v1::FetchAttachmentRequest>,
    ) -> Result<Response<Self::FetchAttachmentStream>, Status> {
        super::attachments::fetch(self, request).await
    }

    type DataStream = ReplyStream<DataDown>;

    async fn data(
        &self,
        mut request: Request<Streaming<DataUp>>,
    ) -> Result<Response<Self::DataStream>, Status> {
        if let Err(error) = auth::intercept(&self.pool, &mut request).await {
            tracing::warn!(kind = "session_rejected", reason = ?error.code(), machine_id = %uuid::Uuid::nil(), connection_id = %uuid::Uuid::nil(), stream = "data", "session authentication rejected");
            return Err(error);
        }
        let authenticated = request
            .extensions()
            .get::<AuthenticatedMachine>()
            .expect("authenticated machine")
            .clone();
        let machine_id = authenticated.machine_id;
        let mut inbound = request.into_inner();
        let state = self.clone();
        let output = async_stream::try_stream! {
            let mut session_log = super::revocation::SessionLog { machine_id, connection_id: uuid::Uuid::nil(), stream: "data",
                reason: "handshake_incomplete", cancelled: state.shutdown.clone(), accepted: false };
            let first = tokio::select! {
                biased;
                _ = state.shutdown.cancelled() => return,
                first = tokio::time::timeout(state.timers.handshake, inbound.message()) => first,
            }
                .map_err(|_| session_log.error(Status::deadline_exceeded("stream handshake timed out")))?
                .map_err(|error| session_log.error(error))?
                .ok_or_else(|| session_log.error(Status::invalid_argument("missing stream handshake")))?;
            let Some(data_up::Msg::Hello(hello)) = first.msg else {
                Err(session_log.error(Status::invalid_argument("first data message must be hello")))?;
                return;
            };
            session_log.connection_id = uuid::Uuid::parse_str(&hello.connection_id).unwrap_or_default();
            let connection_id = match state.validate_hello(&hello, machine_id, StreamKind::Data) {
                Ok(id) => id,
                Err(rejected) => {
                    session_log.reason = super::rejection_kind(rejected.reason);
                    tracing::warn!(kind = "session_rejected", reason = super::rejection_kind(rejected.reason),
                        machine_id = %machine_id, connection_id = ?uuid::Uuid::parse_str(&hello.connection_id).ok(), stream = "data", "handshake rejected");
                    let reply = HelloReply { result: Some(hello_reply::Result::Rejected(rejected)) };
                    yield checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
                    return;
                }
            };
            let reply = state.accepted(machine_id, &hello).await?;
            let reply = checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
            let Some(lease) = state.registry.attach_data(machine_id, connection_id, &authenticated.credential) else {
                tracing::warn!(kind = "session_rejected", reason = "connection_binding_mismatch", machine_id = %machine_id,
                    connection_id = %connection_id, stream = "data", "handshake rejected");
                let reply = HelloReply {
                    result: Some(hello_reply::Result::Rejected(rejection(RejectReason::ConnectionBindingMismatch))),
                };
                yield checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
                return;
            };
            if !crate::machines::credential_active(&mut *state.pool.acquire().await.map_err(|_| Status::internal("database"))?, machine_id, &authenticated.credential)
                .await.map_err(|_| Status::internal("database"))? {
                state.registry.revoke(machine_id, connection_id);
                Err(session_log.error(Status::unauthenticated("credential_revoked")))?;
            }
            state.registry.observe_clock(machine_id,hello.machine_time_ms,chrono::Utc::now());
            tracing::info!(kind = "session_accepted", reason = "handshake_accepted", machine_id = %machine_id,
                connection_id = %connection_id, stream = "data", "session accepted");
            session_log.accepted = true;
            session_log.reason = "stream_end";
            session_log.cancelled = lease.cancel.clone();
            yield reply;
            #[cfg(feature = "fault-injection")]
            let mut failures = state.faults.data_failures();
            #[cfg(not(feature = "fault-injection"))]
            let mut failures = ();
            let mut lane = IngestLane::start(state.ingest.clone(), machine_id, authenticated.credential.clone(), LaneKind::Sj);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = state.shutdown.cancelled() => break,
                    _ = lease.cancel.cancelled() => break,
                    code = fault_signal(&mut failures) => Err(Err(Status::new(code, "injected data failure"))),
                    ack = lane.next_ack() => Ok(ack),
                    next = inbound.message() => Err(next),
                };
                let message = match next {
                    Ok(ack) => {
                        let Some(ack) = ack else { break; };
                        let Some(ack) = ack.map_err(|error| session_log.error(error))? else { continue; };
                        #[cfg(feature = "fault-injection")]
                        if state.faults.ack(&state.pool, machine_id, &ack).await { continue; }
                        yield checked(DataDown { msg: Some(data_down::Msg::SjAck(ack)) })?;
                        continue;
                    }
                    Err(message) => {
                        let Some(message) = message.map_err(|error| session_log.error(error))? else { break; };
                        message
                    }
                };
                match message.msg {
                    Some(data_up::Msg::SjRecords(records)) => lane.submit(records).map_err(|error| session_log.error(error))?,
                    _ => Err(session_log.error(Status::invalid_argument("unexpected data message")))?,
                }
            }
            if lease.is_revoked() {
                session_log.reason = "credential_revoked";
                Err(session_log.error(Status::unauthenticated("credential_revoked")))?;
            }
        };
        Ok(Response::new(Box::pin(output)))
    }
}

#[cfg(feature = "fault-injection")]
async fn fault_signal(failures: &mut tokio::sync::broadcast::Receiver<tonic::Code>) -> tonic::Code {
    loop {
        if let Ok(code) = failures.recv().await {
            return code;
        }
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_signal(_: &mut ()) -> std::future::Pending<tonic::Code> {
    std::future::pending()
}
