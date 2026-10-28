use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use canterlot_proto::{
    limits::MAX_FILE_CHUNK_BYTES,
    worker::v1::{AttachmentChunk, FetchAttachmentRequest},
};
use tokio::{
    io::AsyncReadExt,
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::{
    HubGrpc, ReplyStream,
    auth::{self, AuthenticatedMachine},
    checked,
};
use crate::attachments::blobstore::BlobError;

const STREAMS_PER_MACHINE: usize = 4;

#[derive(Clone, Default)]
pub(super) struct FetchSlots(Arc<Mutex<HashMap<Uuid, Weak<Semaphore>>>>);

impl FetchSlots {
    #[allow(clippy::result_large_err)]
    fn acquire(&self, machine: Uuid) -> Result<OwnedSemaphorePermit, Status> {
        let mut slots = self.0.lock().unwrap_or_else(|error| error.into_inner());
        slots.retain(|_, slot| slot.strong_count() > 0);
        let semaphore = slots
            .get(&machine)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let semaphore = Arc::new(Semaphore::new(STREAMS_PER_MACHINE));
                slots.insert(machine, Arc::downgrade(&semaphore));
                semaphore
            });
        semaphore
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("attachment_fetch_limit"))
    }
}

pub(super) async fn fetch(
    hub: &HubGrpc,
    mut request: Request<FetchAttachmentRequest>,
) -> Result<Response<ReplyStream<AttachmentChunk>>, Status> {
    auth::intercept(&hub.pool, &mut request).await?;
    let machine = request
        .extensions()
        .get::<AuthenticatedMachine>()
        .expect("authenticated machine")
        .machine_id;
    if hub.shutdown.is_cancelled() {
        return Err(Status::unavailable("hub_shutdown"));
    }
    let request = request.into_inner();
    let id = Uuid::parse_str(&request.attachment_id)
        .map_err(|_| Status::invalid_argument("invalid attachment id"))?;
    let allowed: bool = sqlx::query_scalar(
        "select exists(select 1 from attachments a
         join input_attachments ia on ia.attachment_id=a.id
         join inputs i on i.id=ia.input_id
         join conversations c on c.id=i.conversation_id
         where a.id=$1 and a.state='ready' and c.machine_id=$2
           and i.state not in ('cancelled','rejected'))",
    )
    .bind(id)
    .bind(machine)
    .fetch_one(&hub.pool)
    .await
    .map_err(|_| Status::internal("attachment_lookup_unavailable"))?;
    if !allowed {
        return Err(Status::not_found("attachment_not_found"));
    }
    let permit = hub.fetch_slots.acquire(machine)?;
    let blobs = hub
        .blobs
        .as_ref()
        .ok_or_else(|| Status::unavailable("attachment_store_unavailable"))?;
    let (mut file, size) = blobs
        .open(id, request.offset)
        .await
        .map_err(|error| match error {
            BlobError::NotFound => Status::not_found("attachment_not_found"),
            BlobError::OffsetBeyondEnd => Status::out_of_range("attachment_offset_beyond_end"),
            _ => Status::unavailable("attachment_read_unavailable"),
        })?;
    let shutdown = hub.shutdown.clone();
    #[cfg(feature = "fault-injection")]
    let faults = hub.faults.clone();
    let output = async_stream::try_stream! {
        let _permit = permit;
        let mut offset = request.offset;
        loop {
            let data = tokio::select! {
                biased;
                _ = shutdown.cancelled() => Err(Status::unavailable("hub_shutdown")),
                data = async {
                    #[cfg(feature = "fault-injection")]
                    faults.pause(crate::fault_injection::Point::BeforeFetchChunk).await;
                    let len = (size - offset).min(MAX_FILE_CHUNK_BYTES as u64) as usize;
                    let mut data = vec![0; len];
                    file.read_exact(&mut data).await
                        .map_err(|_| Status::unavailable("attachment_read_unavailable"))?;
                    Ok(data)
                } => data,
            }?;
            let next = offset + data.len() as u64;
            let last = next == size;
            yield checked(AttachmentChunk { offset, data, last })?;
            if last { break; }
            offset = next;
        }
    };
    Ok(Response::new(Box::pin(output)))
}
