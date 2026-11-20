//! Журнал задачи: приём gzip-вложения от воркера и чтение (хвост, пока идёт, и полный журнал
//! после конца).

use std::io::{self, Read};

use bytes::Bytes;
use canterlot_proto::{limits::MAX_MEDIA_BYTES, worker::v1::MediaHeader};
use futures_util::stream::BoxStream;
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use super::Job;
use crate::{
    attachments::{BlobError, BlobStore},
    media::{self, MediaError},
};

/// Допустимый размер одного чтения журнала.
pub const MAX_READ_BYTES: usize = 64 * 1024;
pub const DEFAULT_READ_BYTES: usize = 16 * 1024;

/// Проверенный заголовок журнала задачи.
pub struct LogHeader {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt: i32,
    pub size: u64,
    pub sha256: String,
}

pub fn parse_header(raw: &MediaHeader) -> Result<LogHeader, MediaError> {
    let id = Uuid::parse_str(&raw.media_id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(MediaError::Invalid)?;
    let job_id = Uuid::parse_str(&raw.job_id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(MediaError::Invalid)?;
    if raw.mime != "application/gzip" {
        return Err(MediaError::InvalidType);
    }
    if raw.size == 0 || raw.size > MAX_MEDIA_BYTES {
        return Err(MediaError::TooLarge);
    }
    let sha = raw.sha256.as_str();
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(MediaError::Invalid);
    }
    Ok(LogHeader {
        id,
        job_id,
        attempt: i32::try_from(raw.job_attempt).map_err(|_| MediaError::Invalid)?,
        size: raw.size,
        sha256: sha.to_owned(),
    })
}

/// Принимает журнал и привязывает к задаче этой машины. Повтор с теми же данными — успех:
/// идентификатор выведен воркером из задачи и попытки.
pub async fn put(
    pool: &PgPool,
    store: &dyn BlobStore,
    machine_id: Uuid,
    header: &LogHeader,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
) -> Result<(), MediaError> {
    let row: Option<(Option<Uuid>, Option<String>, i32)> = sqlx::query_as(
        "select log_media_id, log_sha256, attempt from jobs where id=$1 and machine_id=$2",
    )
    .bind(header.job_id)
    .bind(machine_id)
    .fetch_optional(pool)
    .await?;
    let Some((stored, stored_sha, attempt)) = row else {
        return Err(MediaError::NotFound);
    };
    if attempt != header.attempt {
        return Err(MediaError::NotFound);
    }
    if let Some(stored) = stored {
        return if stored == header.id && stored_sha.as_deref() == Some(header.sha256.as_str()) {
            Ok(())
        } else {
            Err(MediaError::Conflict)
        };
    }
    let Some(locked) = store.try_lock(header.id)? else {
        return Err(MediaError::Busy);
    };
    let store = &*locked;
    match store.open(header.id, 0).await {
        // Повтор после сбоя между публикацией и записью строки: тело не нужно.
        Ok(_) => match store.publish(header.id, header.size, &header.sha256).await {
            Ok(()) => {}
            Err(BlobError::ChecksumMismatch | BlobError::LengthMismatch { .. }) => {
                return Err(MediaError::Conflict);
            }
            Err(error) => return Err(error.into()),
        },
        Err(BlobError::NotFound) => {
            store.delete_staging(header.id).await?;
            let media_header = media::Header {
                id: header.id,
                execution_id: Uuid::nil(),
                block_id: Uuid::nil(),
                mime: "application/gzip".to_owned(),
                size: header.size,
                sha256: header.sha256.clone(),
                share: None,
            };
            if let Err(error) = media::receive(store, &media_header, body).await {
                if let Err(error) = store.delete_staging(header.id).await {
                    tracing::warn!(kind = "job_log_staging_delete_failed", job_id = %header.job_id, error = %error.kind(), "staging delete failed");
                }
                return Err(error);
            }
        }
        Err(error) => return Err(error.into()),
    }
    let (id, job_id, size, sha) = (header.id, header.job_id, header.size, header.sha256.clone());
    let attached = sqlx::query(
        "update jobs set log_media_id=$2, log_size=$3, log_sha256=$4, updated_at=clock_timestamp()
         where id=$1 and (log_media_id is null or log_media_id=$2)",
    )
    .bind(job_id)
    .bind(id)
    .bind(i64::try_from(size).unwrap_or(i64::MAX))
    .bind(sha)
    .execute(pool)
    .await?
    .rows_affected();
    if attached == 0 {
        return Err(MediaError::Conflict);
    }
    Ok(())
}

/// Кусок журнала для инструмента и HTTP.
#[derive(Serialize, ToSchema, Debug, Clone, PartialEq, Eq)]
pub struct LogSlice {
    /// `tail` — живой или итоговый хвост на хабе, `log` — полный журнал после конца.
    pub source: String,
    pub offset: u64,
    pub next_offset: u64,
    /// Размер журнала, когда он известен.
    pub total_bytes: Option<u64>,
    pub eof: bool,
    pub text: String,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Хвост, который хаб держит сам. Позиции — в потоке вывода команды.
fn tail_slice(job: &Job, offset: Option<u64>, limit: usize) -> LogSlice {
    let terminal = job.state().is_terminal();
    let (bytes, end): (&[u8], u64) = if terminal {
        let tail = job.tail.as_deref().unwrap_or(&[]);
        (
            tail,
            u64::try_from(job.output_bytes)
                .unwrap_or(0)
                .max(tail.len() as u64),
        )
    } else {
        (
            job.live_tail.as_slice(),
            u64::try_from(job.live_offset).unwrap_or(0),
        )
    };
    let start = end - bytes.len() as u64;
    let from = match offset {
        Some(offset) => offset.clamp(start, end),
        None => end.saturating_sub(limit as u64).max(start),
    };
    let upto = (from + limit as u64).min(end);
    let slice = &bytes
        [usize::try_from(from - start).unwrap_or(0)..usize::try_from(upto - start).unwrap_or(0)];
    LogSlice {
        source: "tail".to_owned(),
        offset: from,
        next_offset: upto,
        total_bytes: terminal.then_some(end),
        eof: terminal && upto >= end,
        text: text(slice),
    }
}

/// Только хвост на хабе: у вызывающего нет хранилища блобов.
pub fn read_tail(job: &Job, offset: Option<u64>, limit: usize) -> Result<LogSlice, MediaError> {
    Ok(tail_slice(job, offset, limit.clamp(1, MAX_READ_BYTES)))
}

/// Читает журнал: пока задача идёт — хвост хаба, после конца — вложение, а если оно ещё не
/// дошло, итоговый хвост.
pub async fn read(
    store: &dyn BlobStore,
    job: &Job,
    offset: Option<u64>,
    limit: usize,
) -> Result<LogSlice, MediaError> {
    let limit = limit.clamp(1, MAX_READ_BYTES);
    let Some(media_id) = job.log_media_id.filter(|_| job.is_terminal()) else {
        return Ok(tail_slice(job, offset, limit));
    };
    let (file, _) = match store.open(media_id, 0).await {
        Ok(opened) => opened,
        Err(BlobError::NotFound) => return Ok(tail_slice(job, offset, limit)),
        Err(error) => return Err(error.into()),
    };
    let file = file.into_std().await;
    let start = offset.unwrap_or(0);
    let slice = tokio::task::spawn_blocking(move || -> io::Result<(Vec<u8>, bool)> {
        let mut decoder = flate2::read::GzDecoder::new(file);
        io::copy(&mut (&mut decoder).take(start), &mut io::sink())?;
        let mut data = Vec::with_capacity(limit);
        (&mut decoder).take(limit as u64).read_to_end(&mut data)?;
        // Дошли ли до конца: следующий байт есть или нет.
        let mut probe = [0_u8; 1];
        let eof = decoder.read(&mut probe)? == 0;
        Ok((data, eof))
    })
    .await
    .map_err(|_| MediaError::Interrupted)??;
    let (data, eof) = slice;
    let next = start + data.len() as u64;
    Ok(LogSlice {
        source: "log".to_owned(),
        offset: start,
        next_offset: next,
        total_bytes: eof.then_some(next),
        eof,
        text: text(&data),
    })
}
