use canterlot_proto::worker::v1::{worker_control_server::WorkerControl, *};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::{
    HubGrpc, IngestLane, LaneKind, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked, rejection,
};

enum Next {
    Ack(Option<Result<JournalRangeAck, Status>>),
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
        auth::intercept(&self.pool, &mut request).await?;
        let machine_id = request
            .extensions()
            .get::<AuthenticatedMachine>()
            .expect("authenticated machine")
            .0;
        let mut inbound = request.into_inner();
        let state = self.clone();
        let output = async_stream::try_stream! {
            let first = tokio::select! {
                biased;
                _ = state.shutdown.cancelled() => return,
                first = tokio::time::timeout(state.timers.handshake, inbound.message()) => first,
            }
                .map_err(|_| Status::deadline_exceeded("stream handshake timed out"))??
                .ok_or_else(|| Status::invalid_argument("missing stream handshake"))?;
            let Some(control_up::Msg::Hello(hello)) = first.msg else {
                Err(Status::invalid_argument("first control message must be hello"))?;
                return;
            };
            #[cfg(feature = "fault-injection")]
            state.faults.hello(hello.clone());
            let connection_id = match state.validate_hello(&hello, machine_id, StreamKind::Control) {
                Ok(id) => id,
                Err(rejected) => {
                    let reply = HelloReply { result: Some(hello_reply::Result::Rejected(rejected)) };
                    yield checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
                    return;
                }
            };
            let reply = state.accepted(machine_id).await?;
            let reply = checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
            let Some((lease, mut commands)) = state.registry.register(machine_id, connection_id) else {
                let reply = HelloReply {
                    result: Some(hello_reply::Result::Rejected(rejection(RejectReason::IdentityInUse))),
                };
                yield checked(ControlDown { msg: Some(control_down::Msg::HelloReply(reply)) })?;
                return;
            };
            #[cfg(feature = "fault-injection")]
            let mut failures = state.faults.failures();
            #[cfg(not(feature = "fault-injection"))]
            let mut failures = ();
            yield reply;
            let cj_journal = Uuid::parse_str(&hello.cj_journal_id).ok();
            let mut lane = IngestLane::start(state.ingest.clone(), machine_id, LaneKind::Cj);
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
                        let ack = ack?;
                        #[cfg(feature = "fault-injection")]
                        if state.faults.ack(&state.pool, machine_id, &ack).await { continue; }
                        yield checked(ControlDown { msg: Some(control_down::Msg::CjAck(ack)) })?;
                    }
                    Next::Inbound(next) => {
                        let Some(message) = next? else { break; };
                        match message.msg {
                            Some(control_up::Msg::Heartbeat(Heartbeat { seq })) => {
                                if !state.registry.heartbeat(machine_id, lease.key) { break; }
                                yield checked(ControlDown { msg: Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq })) })?;
                            }
                            Some(control_up::Msg::CjRecords(records)) => {
                                if Uuid::parse_str(&records.journal_id).ok() != cj_journal {
                                    Err(Status::invalid_argument("invalid_record"))?;
                                }
                                lane.submit(records)?;
                            }
                            _ => Err(Status::invalid_argument("unexpected control message"))?,
                        }
                    }
                    Next::Command(command) => {
                        let Some(command) = command else { break; };
                        if command.msg.is_none() { break; }
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
                    Next::Fault(code) => Err(Status::new(code, "injected transport failure"))?,
                }
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
