use canterlot_proto::worker::v1::{worker_control_server::WorkerControl, *};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::{
    HubGrpc, IngestLane, LaneKind, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked,
};

// Команды растут с аддитивными полями; ожидание живёт на стеке один шаг.
#[allow(clippy::large_enum_variant)]
enum Next {
    Ack(Option<Result<Option<JournalRangeAck>, Status>>),
    Inbound(Result<Option<ControlUp>, Status>),
    Command(Option<ControlDown>),
    #[cfg(feature = "fault-injection")]
    Fault(tonic::Code),
}

#[tonic::async_trait]
impl WorkerControl for HubGrpc {
    type ControlStream = ReplyStream<ControlDown>;

    async fn control(
        &self,
        mut request: Request<Streaming<ControlUp>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        if let Err(error) = auth::intercept(&self.pool, &mut request).await {
            tracing::warn!(kind = "session_rejected", reason = ?error.code(), machine_id = %uuid::Uuid::nil(), connection_id = %uuid::Uuid::nil(), stream = "control", "session authentication rejected");
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
            let mut session_log = super::revocation::SessionLog { machine_id, connection_id: uuid::Uuid::nil(), stream: "control",
                reason: "handshake_incomplete", cancelled: state.shutdown.clone(), accepted: false };
            let first = tokio::select! {
                biased;
                _ = state.shutdown.cancelled() => return,
                first = tokio::time::timeout(state.timers.handshake, inbound.message()) => first,
            }
                .map_err(|_| session_log.error(Status::deadline_exceeded("stream handshake timed out")))?
                .map_err(|error| session_log.error(error))?
                .ok_or_else(|| session_log.error(Status::invalid_argument("missing stream handshake")))?;
            let Some(control_up::Msg::Hello(hello)) = first.msg else {
                Err(session_log.error(Status::invalid_argument("first control message must be hello")))?;
                return;
            };
            #[cfg(feature = "fault-injection")]
            state.faults.hello(hello.clone());
            session_log.connection_id = uuid::Uuid::parse_str(&hello.connection_id).unwrap_or_default();
            let connection_id = match state.validate_hello(&hello, machine_id, StreamKind::Control) {
                Ok(id) => id,
                Err(rejected) => {
                    session_log.reason = super::rejection_kind(rejected.reason);
                    tracing::warn!(kind = "session_rejected", reason = super::rejection_kind(rejected.reason),
                        machine_id = %machine_id, connection_id = ?uuid::Uuid::parse_str(&hello.connection_id).ok(), stream = "control", "handshake rejected");
                    let reply = HelloReply { result: Some(hello_reply::Result::Rejected(rejected)) };
                    yield checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
                    return;
                }
            };
            let (lease, mut commands) = match state.admit_control(machine_id, connection_id,
                Uuid::parse_str(&hello.cj_journal_id).expect("validated journal"), authenticated.credential.clone()).await.map_err(|error| session_log.error(error))? {
                Ok(session) => session,
                Err(rejected) => {
                    session_log.reason = super::rejection_kind(rejected.reason);
                    tracing::warn!(kind = "session_rejected", reason = super::rejection_kind(rejected.reason), machine_id = %machine_id,
                        connection_id = %connection_id, stream = "control", "handshake rejected");
                    let reply = HelloReply { result: Some(hello_reply::Result::Rejected(rejected)) };
                    yield checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
                    return;
                }
            };
            let reply = state.accepted(machine_id, &hello).await?;
            if let Some(hello_reply::Result::Accepted(accepted)) = &reply.result {
                state.registry.mark_accepted(machine_id, connection_id, &accepted.capabilities);
            }
            state.registry.observe_clock(machine_id,hello.machine_time_ms,chrono::Utc::now());
            let reply = checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
            #[cfg(feature = "fault-injection")]
            let mut failures = state.faults.failures();
            #[cfg(not(feature = "fault-injection"))]
            let mut failures = ();
            tracing::info!(kind = "session_accepted", reason = "handshake_accepted", machine_id = %machine_id,
                connection_id = %connection_id, stream = "control", "session accepted");
            session_log.accepted = true;
            session_log.reason = "stream_end";
            session_log.cancelled = lease.cancel.clone();
            yield reply;
            let cj_journal = Uuid::parse_str(&hello.cj_journal_id).ok();
            let mut lane = IngestLane::start(state.ingest.clone(), machine_id, authenticated.credential.clone(), LaneKind::Cj);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = state.shutdown.cancelled() => break,
                    _ = lease.cancel.cancelled() => break,
                    fault = fault_signal(&mut failures) => fault,
                    ack = lane.next_ack() => Next::Ack(ack),
                    next = inbound.message() => Next::Inbound(next),
                    command = commands.recv() => Next::Command(command),
                };
                match next {
                    Next::Ack(ack) => {
                        let Some(ack) = ack else { break; };
                        let Some(ack) = ack.map_err(|error| session_log.error(state.revoked_by_ingest(machine_id, connection_id, error)))? else {
                            state.registry.mark_reconciled(machine_id, connection_id);
                            continue;
                        };
                        #[cfg(feature = "fault-injection")]
                        if state.faults.ack(&state.pool, machine_id, &ack).await { continue; }
                        yield checked(ControlDown { msg: Some(control_down::Msg::CjAck(ack)) })?;
                    }
                    Next::Inbound(next) => {
                        let Some(message) = next.map_err(|error| session_log.error(error))? else { break; };
                        match message.msg {
                            Some(control_up::Msg::Heartbeat(Heartbeat { seq, machine_time_ms, .. })) => {
                                state.registry.observe_clock(machine_id,machine_time_ms,chrono::Utc::now());
                                if !state.registry.heartbeat(machine_id, lease.key) { break; }
                                yield checked(ControlDown { msg: Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq })) })?;
                            }
                            Some(control_up::Msg::MachineReport(report)) => {
                                if !state.registry.has_capability(machine_id,crate::session::CAPABILITY_PILOT_V1){Err(session_log.error(Status::invalid_argument("capability_missing")))?;}
                                lane.submit_report(report).map_err(|error|session_log.error(error))?;
                            }
                            Some(control_up::Msg::MachineMetrics(metrics)) => {
                                if !state.registry.has_capability(machine_id,crate::session::CAPABILITY_PILOT_V1){Err(session_log.error(Status::invalid_argument("capability_missing")))?;}
                                state.registry.observe_clock(machine_id,metrics.machine_time_ms,chrono::Utc::now());
                                lane.submit_metrics(metrics);
                            }
                            Some(control_up::Msg::LimitsReport(report)) => {
                                if !state.registry.has_capability(machine_id,crate::session::CAPABILITY_PILOT_V1){Err(session_log.error(Status::invalid_argument("capability_missing")))?;}
                                lane.submit_limits(report);
                            }
                            Some(control_up::Msg::WorkdirSuggestions(reply)) => {
                                if !state.registry.has_capability(machine_id,crate::session::CAPABILITY_WORKDIR_SUGGEST_V1){Err(session_log.error(Status::invalid_argument("capability_missing")))?;}
                                state.registry.finish_suggest(machine_id, reply);
                            }
                            Some(control_up::Msg::CjRecords(records)) => {
                                if Uuid::parse_str(&records.journal_id).ok() != cj_journal {
                                    Err(session_log.error(Status::invalid_argument("invalid_record")))?;
                                }
                                lane.submit(records).map_err(|error| session_log.error(error))?;
                            }
                            Some(control_up::Msg::ReconcileReport(report)) => {
                                lane.submit_reconcile(report).map_err(|error| session_log.error(error))?;
                            }
                            _ => Err(session_log.error(Status::invalid_argument("unexpected control message")))?,
                        }
                    }
                    Next::Command(command) => {
                        let Some(command) = command else { break; };
                        if command.msg.is_none() { session_log.reason = "command_stored_timeout"; break; }
                        #[cfg(feature = "fault-injection")]
                        {
                            tokio::select! {
                                biased;
                                _ = state.shutdown.cancelled() => break,
                                _ = lease.cancel.cancelled() => break,
                                _ = state.faults.pause(crate::fault_injection::Point::BeforeCommandSend) => {},
                            }
                            if let Some(control_down::Msg::Command(value)) = &command.msg {
                                state.faults.command(value.clone(), connection_id);
                            }
                        }
                        yield checked(command)?;
                    }
                    #[cfg(feature = "fault-injection")]
                    Next::Fault(code) => Err(session_log.error(Status::new(code, "injected transport failure")))?,
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
async fn fault_signal(failures: &mut tokio::sync::broadcast::Receiver<tonic::Code>) -> Next {
    loop {
        match failures.recv().await {
            Ok(code) => return Next::Fault(code),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return std::future::pending().await;
            }
        }
    }
}

#[cfg(not(feature = "fault-injection"))]
fn fault_signal(_: &mut ()) -> std::future::Pending<Next> {
    std::future::pending()
}
