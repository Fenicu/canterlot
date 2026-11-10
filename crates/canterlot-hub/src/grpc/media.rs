use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use bytes::Bytes;
use canterlot_proto::{
    limits::MAX_FILE_CHUNK_BYTES,
    worker::v1::{MediaChunk, MediaStored, media_chunk::Part},
};
use futures_util::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use super::{
    HubGrpc,
    auth::{self, AuthenticatedMachine},
};
use crate::media::{self, MediaError};

const UPLOADS_PER_MACHINE: usize = 4;

#[derive(Clone, Default)]
pub(super) struct UploadSlots(Arc<Mutex<HashMap<Uuid, Weak<Semaphore>>>>);

impl UploadSlots {
    #[allow(clippy::result_large_err)]
    fn acquire(&self, machine: Uuid) -> Result<OwnedSemaphorePermit, Status> {
        let mut slots = self.0.lock().unwrap_or_else(|error| error.into_inner());
        slots.retain(|_, slot| slot.strong_count() > 0);
        let semaphore = slots
            .get(&machine)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let semaphore = Arc::new(Semaphore::new(UPLOADS_PER_MACHINE));
                slots.insert(machine, Arc::downgrade(&semaphore));
                semaphore
            });
        semaphore
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("media_upload_limit"))
    }
}

fn status(error: &MediaError) -> Status {
    let kind = error.kind();
    match error {
        // Сообщение статуса — стабильный код: воркер переводит его в свой `share_*`.
        MediaError::Invalid
        | MediaError::TooLarge
        | MediaError::Quota
        | MediaError::InvalidName
        | MediaError::InvalidType
        | MediaError::InvalidText => Status::invalid_argument(kind),
        MediaError::NotFound => Status::not_found(kind),
        MediaError::Conflict => Status::already_exists(kind),
        MediaError::ChecksumMismatch => Status::data_loss(kind),
        MediaError::Busy | MediaError::Db(_) | MediaError::Blob(_) => Status::unavailable(kind),
        MediaError::Interrupted => Status::cancelled(kind),
    }
}

/// Клиентский поток воркера: заголовок, затем данные до 128 КиБ. Ответ приходит только после
/// надёжной публикации и commit метаданных; до этого воркер не пишет ссылку в журнал.
pub(super) async fn put(
    hub: &HubGrpc,
    mut request: Request<Streaming<MediaChunk>>,
) -> Result<Response<MediaStored>, Status> {
    auth::intercept(&hub.pool, &mut request).await?;
    let machine = request
        .extensions()
        .get::<AuthenticatedMachine>()
        .expect("authenticated machine")
        .machine_id;
    if hub.shutdown.is_cancelled() {
        return Err(Status::unavailable("hub_shutdown"));
    }
    let mut inbound = request.into_inner();
    let first = tokio::select! {
        biased;
        _ = hub.shutdown.cancelled() => return Err(Status::unavailable("hub_shutdown")),
        first = tokio::time::timeout(hub.timers.handshake, inbound.message()) => first,
    }
    .map_err(|_| Status::deadline_exceeded("media header timed out"))??;
    let Some(MediaChunk {
        part: Some(Part::Header(raw)),
    }) = first
    else {
        return Err(Status::invalid_argument("media_header_required"));
    };
    let header = media::parse_header(&raw).map_err(|error| status(&error))?;
    let _permit = hub.media_slots.acquire(machine)?;
    let blobs = hub
        .blobs
        .as_ref()
        .ok_or_else(|| Status::unavailable("media_store_unavailable"))?
        .clone();
    #[cfg(feature = "fault-injection")]
    let faults = hub.faults.clone();
    let invalid = Arc::new(AtomicBool::new(false));
    let seen = invalid.clone();
    let body = async_stream::stream! {
        loop {
            #[cfg(feature = "fault-injection")]
            faults.pause(crate::fault_injection::Point::BeforeMediaChunk).await;
            match inbound.message().await {
                Ok(Some(MediaChunk { part: Some(Part::Data(data)) })) if data.len() <= MAX_FILE_CHUNK_BYTES => {
                    yield Ok(Bytes::from(data));
                }
                Ok(None) => {
                    #[cfg(feature = "fault-injection")]
                    faults.pause(crate::fault_injection::Point::BeforeMediaPublish).await;
                    break;
                }
                Ok(Some(_)) => {
                    seen.store(true, Ordering::Relaxed);
                    yield Err(io::Error::other("invalid media frame"));
                    break;
                }
                Err(_) => {
                    yield Err(io::Error::other("media stream interrupted"));
                    break;
                }
            }
        }
    }
    .boxed();
    let result = tokio::select! {
        biased;
        _ = hub.shutdown.cancelled() => return Err(Status::unavailable("hub_shutdown")),
        result = media::put(&hub.pool, &*blobs, machine, &header, hub.media_quota_bytes, body) => result,
    };
    match result {
        Ok(()) => Ok(Response::new(MediaStored {
            media_id: header.id.to_string(),
        })),
        Err(MediaError::Interrupted) if invalid.load(Ordering::Relaxed) => {
            Err(Status::invalid_argument("media_invalid_frame"))
        }
        Err(error) => {
            if matches!(error, MediaError::Db(_) | MediaError::Blob(_)) {
                tracing::warn!(kind = error.kind(), machine_id = %machine, media_id = %header.id, "media upload failed");
            }
            Err(status(&error))
        }
    }
}
