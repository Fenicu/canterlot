//! Загрузка частями: смещение равно длине staging, одно тело на вложение за раз.

use std::io;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use sqlx::PgPool;
use uuid::Uuid;

use super::{AttachmentError, BlobError, BlobStore, UploadLocks, state_event};

/// Принятые байты: длина staging, а после публикации без commit полный размер.
pub(crate) async fn received(
    store: &dyn BlobStore,
    id: Uuid,
    size: u64,
) -> Result<u64, AttachmentError> {
    if let Some(len) = store.staging_len(id).await? {
        return Ok(len);
    }
    match store.open(id, 0).await {
        Ok(_) => Ok(size),
        Err(BlobError::NotFound) => Ok(0),
        Err(error) => Err(error.into()),
    }
}

async fn row(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<(String, u64), AttachmentError> {
    let (state, size): (String, i64) =
        sqlx::query_as("select state, size_bytes from attachments where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(pool)
            .await?
            .ok_or(AttachmentError::NotFound)?;
    Ok((state, size as u64))
}

/// `HEAD …/content`: текущее смещение под тем же захватом, что и запись тела.
pub async fn upload_offset(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
) -> Result<(u64, u64), AttachmentError> {
    let (state, size) = row(pool, owner_id, id).await?;
    match state.as_str() {
        "created" | "uploading" => {
            let Some(_guard) = UploadLocks::global().try_lock(id) else {
                let current = received(store, id, size).await?;
                return Err(AttachmentError::UploadInProgress { current });
            };
            Ok((received(store, id, size).await?, size))
        }
        "ready" => Ok((size, size)),
        _ => Err(AttachmentError::NotUploadable),
    }
}

/// Отмечает активность загрузки; `created` переходит в `uploading` с событием.
async fn touch(pool: &PgPool, id: Uuid, correlation_id: Uuid) -> Result<(), AttachmentError> {
    let mut tx = pool.begin().await?;
    let current: Option<(String, Uuid)> =
        sqlx::query_as("select state, conversation_id from attachments where id=$1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((state, conversation_id)) = current else {
        return Err(AttachmentError::NotFound);
    };
    if !matches!(state.as_str(), "created" | "uploading") {
        return Err(AttachmentError::NotUploadable);
    }
    sqlx::query(
        "update attachments set state='uploading', updated_at=clock_timestamp() where id=$1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if state == "created" {
        state_event(
            &mut tx,
            conversation_id,
            id,
            "uploading",
            None,
            correlation_id,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `PATCH …/content`: тело пишется в staging потоком с ограниченным буфером, без транзакции.
/// Байты сверх объявленного размера или `chunk_limit` отклоняются, принятая часть остаётся.
#[allow(clippy::too_many_arguments)]
pub async fn upload_chunk(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
    offset: u64,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
    chunk_limit: u64,
    correlation_id: Uuid,
) -> Result<u64, AttachmentError> {
    let (state, size) = row(pool, owner_id, id).await?;
    if !matches!(state.as_str(), "created" | "uploading") {
        return Err(AttachmentError::NotUploadable);
    }
    let Some(_guard) = UploadLocks::global().try_lock(id) else {
        let current = received(store, id, size).await?;
        return Err(AttachmentError::UploadInProgress { current });
    };
    let current = store.staging_len(id).await?.unwrap_or(0);
    if current != offset {
        return Err(AttachmentError::OffsetMismatch { current });
    }
    touch(pool, id, correlation_id).await?;
    let max_total = size.min(offset.saturating_add(chunk_limit));
    let written = store.append_staging(id, offset, body, max_total).await;
    let still_open: bool = sqlx::query_scalar(
        "update attachments set updated_at=clock_timestamp()
         where id=$1 and state in ('created','uploading') returning true",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .unwrap_or(false);
    let written = match written {
        Ok(written) => written,
        Err(BlobError::OffsetMismatch { current }) => {
            return Err(AttachmentError::OffsetMismatch { current });
        }
        Err(BlobError::TooLarge { current }) => {
            return Err(AttachmentError::ExcessBytes { current });
        }
        Err(BlobError::BodyInterrupted { current }) => {
            return Err(AttachmentError::BodyInterrupted { current });
        }
        Err(error) => return Err(error.into()),
    };
    if !still_open {
        return Err(AttachmentError::NotUploadable);
    }
    Ok(written)
}
