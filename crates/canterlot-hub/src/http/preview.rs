//! Уменьшенный показ картинок: лента и фон просмотрщика качают JPEG не больше 1024
//! по длинной стороне, оригинал остаётся для полного вида и скачивания.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::AsyncReadExt;

use super::{
    error::{ApiError, ErrorCode},
    etag_matches,
};
use crate::{
    attachments::blobstore::IO_CHUNK_BYTES,
    rendition::{self, Rendition},
};

/// Одновременно раскодируются не больше двух картинок: показ 16,7 Мп занимает ~67 МБ.
pub const MAX_CONCURRENT_RENDITIONS: usize = 2;
static DECODE_SLOTS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_RENDITIONS);

/// Показ оригинала или его уменьшенной копии: кеш браузера, `If-None-Match`, тяжёлая
/// раскодировка в blocking-потоке, чтобы не останавливать цикл сервера.
/// Одновременно раскодируются не больше `MAX_CONCURRENT_RENDITIONS` картинок.
pub async fn render_preview(
    original: Vec<u8>,
    mime: String,
    decode: fn(&[u8], &str) -> Rendition,
) -> Result<(Vec<u8>, String), ApiError> {
    // Слот берётся до ухода в поток и держится до конца кодирования.
    let permit = DECODE_SLOTS
        .acquire()
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match decode(&original, &mime) {
            Rendition::Scaled { bytes, .. } => (bytes, "image/jpeg".to_owned()),
            // Оригинал возвращается из потока вместе с решением: без лишнего копирования.
            Rendition::Original => (original, mime),
        }
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal))
}

/// Показ оригинала или его уменьшенной копии: кеш браузера, `If-None-Match`, тяжёлая
/// раскодировка в blocking-потоке, чтобы не останавливать цикл сервера.
pub async fn serve_preview(
    mime: String,
    size: u64,
    sha256: String,
    file: tokio::fs::File,
    request: &HeaderMap,
) -> Result<Response, ApiError> {
    let etag = rendition::rendition_etag(&sha256);
    if etag_matches(request, &etag) {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let mut limited = file.take(size);
    // Размер ограничен потолками приёма, показ неизменен, поэтому читаем целиком.
    let mut original = Vec::with_capacity(size.min(8 * IO_CHUNK_BYTES as u64) as usize);
    limited
        .read_to_end(&mut original)
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let rendered = render_preview(original, mime, rendition::rendition).await?;
    let (bytes, content_type) = rendered;
    let content_type =
        HeaderValue::from_str(&content_type).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let etag = HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(bytes.len() as u64),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    headers.insert(header::ETAG, etag);
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    // Показ выводится из неизменного оригинала, версия зашита в ETag.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    Ok((StatusCode::OK, headers, Body::from(bytes)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    fn slow_decode(_: &[u8], _: &str) -> Rendition {
        let active = ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
        Rendition::Original
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn at_most_two_decodes_run_at_once() {
        ACTIVE.store(0, Ordering::SeqCst);
        PEAK.store(0, Ordering::SeqCst);
        let mut tasks = Vec::new();
        for _ in 0..10 {
            tasks.push(tokio::spawn(render_preview(
                vec![0u8; 8],
                "image/png".to_owned(),
                slow_decode,
            )));
        }
        for task in tasks {
            let (bytes, content_type) = task.await.unwrap().unwrap();
            assert_eq!(bytes, vec![0u8; 8]);
            assert_eq!(content_type, "image/png");
        }
        let peak = PEAK.load(Ordering::SeqCst);
        assert!(
            peak <= MAX_CONCURRENT_RENDITIONS,
            "одновременно раскодировались {peak}, а можно не больше {MAX_CONCURRENT_RENDITIONS}"
        );
    }
}
