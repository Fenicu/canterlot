use canterlot_proto::worker::v1::{worker_control_server::WorkerControl, *};
use tonic::{Request, Response, Status, Streaming};

use super::{
    HubGrpc, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked, rejection,
};

enum Next {
    Inbound(Result<Option<ControlUp>, Status>),
    Command(Option<ControlDown>),
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
            yield reply;
            loop {
                let next = tokio::select! {
                    biased;
                    _ = state.shutdown.cancelled() => break,
                    _ = lease.cancel.cancelled() => break,
                    next = inbound.message() => Next::Inbound(next),
                    command = commands.recv() => Next::Command(command),
                };
                match next {
                    Next::Inbound(next) => {
                        let Some(message) = next? else { break; };
                        match message.msg {
                            Some(control_up::Msg::Heartbeat(Heartbeat { seq })) => {
                                if !state.registry.heartbeat(machine_id, lease.key) { break; }
                                yield checked(ControlDown { msg: Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq })) })?;
                            }
                            Some(control_up::Msg::CjRecords(records)) => {
                                let ack = state.ingest.cj_records(machine_id, records).await?;
                                yield checked(ControlDown { msg: Some(control_down::Msg::CjAck(ack)) })?;
                            }
                            _ => Err(Status::invalid_argument("unexpected control message"))?,
                        }
                    }
                    Next::Command(command) => {
                        let Some(command) = command else { break; };
                        if command.msg.is_none() { break; }
                        yield checked(command)?;
                    }
                }
            }
        };
        Ok(Response::new(Box::pin(output)))
    }
}
