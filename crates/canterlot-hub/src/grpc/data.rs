use canterlot_proto::worker::v1::{worker_data_server::WorkerData, *};
use tonic::{Request, Response, Status, Streaming};

use super::{
    HubGrpc, IngestLane, LaneKind, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked, rejection,
};

#[tonic::async_trait]
impl WorkerData for HubGrpc {
    type DataStream = ReplyStream<DataDown>;

    async fn data(
        &self,
        mut request: Request<Streaming<DataUp>>,
    ) -> Result<Response<Self::DataStream>, Status> {
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
            let Some(data_up::Msg::Hello(hello)) = first.msg else {
                Err(Status::invalid_argument("first data message must be hello"))?;
                return;
            };
            let connection_id = match state.validate_hello(&hello, machine_id, StreamKind::Data) {
                Ok(id) => id,
                Err(rejected) => {
                    let reply = HelloReply { result: Some(hello_reply::Result::Rejected(rejected)) };
                    yield checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
                    return;
                }
            };
            let reply = state.accepted(machine_id).await?;
            let reply = checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
            let Some(lease) = state.registry.attach_data(machine_id, connection_id) else {
                let reply = HelloReply {
                    result: Some(hello_reply::Result::Rejected(rejection(RejectReason::ConnectionBindingMismatch))),
                };
                yield checked(DataDown { msg: Some(data_down::Msg::HelloReply(reply)) })?;
                return;
            };
            yield reply;
            let mut lane = IngestLane::start(state.ingest.clone(), machine_id, LaneKind::Sj);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = state.shutdown.cancelled() => break,
                    _ = lease.cancel.cancelled() => break,
                    ack = lane.next_ack() => Ok(ack),
                    next = inbound.message() => Err(next),
                };
                let message = match next {
                    Ok(ack) => {
                        let Some(ack) = ack else { break; };
                        let ack = ack?;
                        #[cfg(feature = "fault-injection")]
                        if state.faults.ack(&state.pool, machine_id, &ack).await { continue; }
                        yield checked(DataDown { msg: Some(data_down::Msg::SjAck(ack)) })?;
                        continue;
                    }
                    Err(message) => {
                        let Some(message) = message? else { break; };
                        message
                    }
                };
                match message.msg {
                    Some(data_up::Msg::SjRecords(records)) => lane.submit(records)?,
                    _ => Err(Status::invalid_argument("unexpected data message"))?,
                }
            }
        };
        Ok(Response::new(Box::pin(output)))
    }
}
