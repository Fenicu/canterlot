//! Картинки, на которые смотрел агент (pilot.md §5.7): приём от воркера в staging, проверенная
//! надёжная публикация, затем строка `block_media`; ссылка из блока и событие появляются только
//! по записи `BlockMedia` журнала сопровождения, которую воркер пишет после `MediaStored`.

use std::{fmt, io};

use bytes::Bytes;
use canterlot_proto::{
    limits::{MAX_MEDIA_BYTES, MEDIA_MIMES},
    worker::v1::{BlockMedia, MediaHeader},
};
use futures_util::stream::BoxStream;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::io::AsyncReadExt;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    attachments::{BlobError, BlobStore},
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

/// Проверенный заголовок `PutMedia`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub id: Uuid,
    pub execution_id: Uuid,
    pub block_id: Uuid,
    pub mime: &'static str,
    pub size: u64,
    pub sha256: String,
}

pub enum MediaError {
    Invalid,
    NotFound,
    Conflict,
    Busy,
    ChecksumMismatch,
    Interrupted,
    Db(sqlx::Error),
    Blob(BlobError),
}

impl fmt::Debug for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(error) => f
                .debug_tuple("Db")
                .field(&crate::dberr::describe(error))
                .finish(),
            other => f.write_str(other.kind()),
        }
    }
}

impl MediaError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Invalid => "media_invalid",
            Self::NotFound => "media_not_found",
            Self::Conflict => "media_conflict",
            Self::Busy => "media_upload_in_progress",
            Self::ChecksumMismatch => "media_checksum_mismatch",
            Self::Interrupted => "media_body_interrupted",
            Self::Db(_) => "media_db_unavailable",
            Self::Blob(_) => "media_store_unavailable",
        }
    }
}

impl From<sqlx::Error> for MediaError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<BlobError> for MediaError {
    fn from(error: BlobError) -> Self {
        Self::Blob(error)
    }
}

impl From<io::Error> for MediaError {
    fn from(error: io::Error) -> Self {
        Self::Blob(BlobError::Io(error))
    }
}

fn uuid(value: &str) -> Result<Uuid, MediaError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(MediaError::Invalid)
}

pub fn parse_header(raw: &MediaHeader) -> Result<Header, MediaError> {
    let mime = MEDIA_MIMES
        .iter()
        .copied()
        .find(|mime| *mime == raw.mime)
        .ok_or(MediaError::Invalid)?;
    let sha256 = raw.sha256.as_str();
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(MediaError::Invalid);
    }
    if raw.size == 0 || raw.size > MAX_MEDIA_BYTES {
        return Err(MediaError::Invalid);
    }
    Ok(Header {
        id: uuid(&raw.media_id)?,
        execution_id: uuid(&raw.execution_id)?,
        block_id: uuid(&raw.block_id)?,
        mime,
        size: raw.size,
        sha256: sha256.to_owned(),
    })
}

/// Сигнатура файла должна совпадать с объявленным типом: иначе тип не отдаётся браузеру.
pub fn signature_matches(mime: &str, head: &[u8]) -> bool {
    match mime {
        "image/png" => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => head.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a"),
        "image/webp" => head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP",
        _ => false,
    }
}

#[derive(sqlx::FromRow)]
struct Stored {
    execution_id: Uuid,
    block_id: Uuid,
    mime: String,
    size_bytes: i64,
    sha256: String,
}

impl Stored {
    fn same(&self, header: &Header) -> bool {
        self.execution_id == header.execution_id
            && self.block_id == header.block_id
            && self.mime == header.mime
            && self.size_bytes == header.size as i64
            && self.sha256 == header.sha256
    }
}

async fn stored(pool: &PgPool, id: Uuid) -> Result<Option<Stored>, sqlx::Error> {
    sqlx::query_as(
        "select execution_id, block_id, mime, size_bytes, sha256 from block_media where id=$1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Ответ на повтор: та же запись - успех, другая - конфликт.
fn replay(row: Option<Stored>, header: &Header) -> Option<Result<(), MediaError>> {
    row.map(|row| {
        if row.same(header) {
            Ok(())
        } else {
            Err(MediaError::Conflict)
        }
    })
}

async fn read_head(store: &dyn BlobStore, id: Uuid) -> Result<Vec<u8>, MediaError> {
    let (file, _) = store.open(id, 0).await?;
    let mut head = Vec::with_capacity(12);
    file.take(12).read_to_end(&mut head).await?;
    Ok(head)
}

/// Принимает тело в staging, проверяет длину и SHA-256, публикует (fsync файла, rename, fsync
/// каталогов), сверяет сигнатуру и только затем фиксирует строку `block_media`. Сбой между
/// публикацией и commit оставляет blob без строки; повтор с тем же ID и checksum проверяет
/// опубликованный blob без копирования и фиксирует метаданные.
pub async fn put(
    pool: &PgPool,
    store: &dyn BlobStore,
    machine_id: Uuid,
    header: &Header,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
) -> Result<(), MediaError> {
    let conversation_id: Uuid = sqlx::query_scalar(
        "select e.conversation_id from executions e join conversations c on c.id=e.conversation_id
         where e.id=$1 and c.machine_id=$2",
    )
    .bind(header.execution_id)
    .bind(machine_id)
    .fetch_optional(pool)
    .await?
    .ok_or(MediaError::NotFound)?;
    let foreign: bool = sqlx::query_scalar("select exists(select 1 from attachments where id=$1)")
        .bind(header.id)
        .fetch_one(pool)
        .await?;
    if foreign {
        return Err(MediaError::Conflict);
    }
    if let Some(result) = replay(stored(pool, header.id).await?, header) {
        return result;
    }
    let Some(locked) = store.try_lock(header.id)? else {
        return Err(MediaError::Busy);
    };
    let store = &*locked;
    if let Some(result) = replay(stored(pool, header.id).await?, header) {
        return result;
    }
    let id = header.id;
    match store.open(id, 0).await {
        // Повтор после сбоя между публикацией и commit: тело не нужно, blob проверяется заново.
        Ok(_) => match store.publish(id, header.size, &header.sha256).await {
            Ok(()) => {}
            Err(BlobError::ChecksumMismatch | BlobError::LengthMismatch { .. }) => {
                return Err(MediaError::Conflict);
            }
            Err(error) => return Err(error.into()),
        },
        Err(BlobError::NotFound) => {
            store.delete_staging(id).await?;
            let received = receive(store, header, body).await;
            if let Err(error) = received {
                if let Err(error) = store.delete_staging(id).await {
                    tracing::warn!(kind = "media_staging_delete_failed", media_id = %id, error = %error.kind(), "staging delete failed");
                }
                return Err(error);
            }
        }
        Err(error) => return Err(error.into()),
    }
    if !signature_matches(header.mime, &read_head(store, id).await?) {
        store.delete_blob(id).await?;
        return Err(MediaError::Invalid);
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "insert into block_media (id, execution_id, conversation_id, block_id, mime, size_bytes, sha256)
         values ($1, $2, $3, $4, $5, $6, $7) on conflict (id) do nothing",
    )
    .bind(id)
    .bind(header.execution_id)
    .bind(conversation_id)
    .bind(header.block_id)
    .bind(header.mime)
    .bind(header.size as i64)
    .bind(&header.sha256)
    .execute(&mut *tx)
    .await?;
    let row: Stored = sqlx::query_as(
        "select execution_id, block_id, mime, size_bytes, sha256 from block_media where id=$1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !row.same(header) {
        return Err(MediaError::Conflict);
    }
    tx.commit().await?;
    Ok(())
}

async fn receive(
    store: &dyn BlobStore,
    header: &Header,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
) -> Result<(), MediaError> {
    let written = match store.append_staging(header.id, 0, body, header.size).await {
        Ok(written) => written,
        Err(BlobError::TooLarge { .. }) => return Err(MediaError::Invalid),
        Err(BlobError::BodyInterrupted { .. }) => return Err(MediaError::Interrupted),
        Err(error) => return Err(error.into()),
    };
    if written != header.size {
        return Err(MediaError::Invalid);
    }
    match store.publish(header.id, header.size, &header.sha256).await {
        Ok(()) => Ok(()),
        Err(BlobError::ChecksumMismatch) => Err(MediaError::ChecksumMismatch),
        Err(BlobError::LengthMismatch { .. }) => Err(MediaError::Invalid),
        Err(error) => Err(error.into()),
    }
}

/// Запись `BlockMedia`: ссылка появляется только на опубликованную строку того же исполнения,
/// блока, типа, размера и checksum. Иначе запись пропускается (`true`), курсор журнала идёт дальше.
pub async fn attach_in_tx(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockMedia,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let parse = |value: &str| {
        Uuid::parse_str(value)
            .ok()
            .filter(|id| !id.is_nil())
            .ok_or_else(|| sqlx::Error::InvalidArgument("invalid media reference".into()))
    };
    let id = parse(&fact.media_id)?;
    let block_id = parse(&fact.block_id)?;
    let row: Option<(Uuid, String, i64, String, bool)> = sqlx::query_as(
        "select block_id, mime, size_bytes, sha256, attached_at is not null from block_media
         where id=$1 and execution_id=$2 and conversation_id=$3 for update",
    )
    .bind(id)
    .bind(run.execution_id)
    .bind(run.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((stored_block, mime, size, sha256, attached)) = row else {
        return Ok(true);
    };
    if attached
        || stored_block != block_id
        || mime != fact.mime
        || size != fact.size as i64
        || sha256 != fact.sha256
    {
        return Ok(true);
    }
    let block: bool = sqlx::query_scalar(
        "select exists(select 1 from blocks where block_id=$1 and execution_id=$2)",
    )
    .bind(block_id)
    .bind(run.execution_id)
    .fetch_one(&mut *conn)
    .await?;
    if !block {
        return Ok(true);
    }
    let dimension = |value: u32| i32::try_from(value).ok().filter(|v| *v > 0);
    let (width, height) = (dimension(fact.width), dimension(fact.height));
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "block_media_attached",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({"block_id": block_id, "media_id": id, "mime": mime, "size": size,
                "width": width, "height": height}),
            correlation_id,
        },
    )
    .await?;
    sqlx::query(
        "update block_media set attached_at=$2, attached_seq=$3, width=$4, height=$5 where id=$1",
    )
    .bind(id)
    .bind(event.committed_at)
    .bind(event.scope_seq)
    .bind(width)
    .bind(height)
    .execute(conn)
    .await?;
    Ok(false)
}

/// Картинка в моделях чтения блока.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct MediaView {
    pub media_id: Uuid,
    pub mime: String,
    pub size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

pub async fn for_block(
    conn: &mut PgConnection,
    block_id: Uuid,
) -> Result<Vec<MediaView>, sqlx::Error> {
    sqlx::query_as(
        "select id media_id, mime, size_bytes size, width, height from block_media
         where block_id=$1 and attached_at is not null order by attached_seq, id",
    )
    .bind(block_id)
    .fetch_all(conn)
    .await
}

/// Связанная с блоком картинка владельца для выдачи браузеру.
pub struct Download {
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub file: tokio::fs::File,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

pub async fn open_for_download(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Download, MediaError> {
    let (mime, size, sha256): (String, i64, String) = sqlx::query_as(
        "select m.mime, m.size_bytes, m.sha256 from block_media m
         join conversations c on c.id=m.conversation_id
         where m.id=$1 and c.owner_id=$2 and m.attached_at is not null",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?
    .ok_or(MediaError::NotFound)?;
    let (file, len) = match store.open(id, 0).await {
        Ok(opened) => opened,
        Err(BlobError::NotFound) => {
            tracing::warn!(kind = "media_blob_missing", media_id = %id, "referenced media blob is missing");
            return Err(MediaError::NotFound);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Download {
        mime,
        size: len.min(size as u64),
        sha256,
        file,
    })
}
