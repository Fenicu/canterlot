//! Уменьшенный показ картинок: лента и фон просмотрщика качают JPEG не больше 1024
//! по длинной стороне, оригинал остаётся для полного вида и скачивания.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

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

/// Оригинал крупнее этого не раскодируется вовсе: показ отдаёт его как есть, потоком.
/// Вложение владельца бывает до 100 МиБ, и держать такие оригиналы в памяти ради показа
/// незачем.
pub const MAX_PREVIEW_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// Уменьшенная копия картинки или `None`, если показом служит сам оригинал. Тяжёлая
/// раскодировка идёт в blocking-потоке, чтобы не останавливать цикл сервера.
/// Одновременно раскодируются не больше `MAX_CONCURRENT_RENDITIONS` картинок, и оригинал
/// читается (`load`) уже под слотом: запросы, ждущие очереди, память не занимают.
pub async fn render_preview(
    load: impl Future<Output = Result<Vec<u8>, ApiError>>,
    mime: String,
    decode: fn(&[u8], &str) -> Rendition,
) -> Result<Option<Vec<u8>>, ApiError> {
    // Слот берётся до чтения файла и держится до конца кодирования.
    let permit = DECODE_SLOTS
        .acquire()
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let original = load.await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match decode(&original, &mime) {
            Rendition::Scaled { bytes, .. } => Some(bytes),
            // Оригинал освобождается здесь же: вызывающий отдаст его потоком из файла.
            Rendition::Original => None,
        }
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal))
}

async fn read_original(file: &mut tokio::fs::File, size: u64) -> Result<Vec<u8>, ApiError> {
    let mut original = Vec::with_capacity(size.min(8 * IO_CHUNK_BYTES as u64) as usize);
    file.take(size)
        .read_to_end(&mut original)
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    Ok(original)
}

/// Показ оригинала или его уменьшенной копии: кеш браузера, `If-None-Match`, тяжёлая
/// раскодировка в blocking-потоке, чтобы не останавливать цикл сервера.
pub async fn serve_preview(
    mime: String,
    size: u64,
    sha256: String,
    mut file: tokio::fs::File,
    request: &HeaderMap,
) -> Result<Response, ApiError> {
    let etag = rendition::rendition_etag(&sha256);
    if etag_matches(request, &etag) {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    // В память читается только то, что будет раскодировано: анимация, чужой тип и слишком
    // крупный файл идут потоком, не занимая ни памяти, ни слота раскодировки.
    let scaled = if rendition::scalable(&mime) && size <= MAX_PREVIEW_SOURCE_BYTES {
        render_preview(
            read_original(&mut file, size),
            mime.clone(),
            rendition::rendition,
        )
        .await?
    } else {
        None
    };
    let (content_type, length, body) = match scaled {
        Some(bytes) => (
            "image/jpeg".to_owned(),
            bytes.len() as u64,
            Body::from(bytes),
        ),
        None => {
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|_| ApiError::new(ErrorCode::Internal))?;
            let stream =
                tokio_util::io::ReaderStream::with_capacity(file.take(size), IO_CHUNK_BYTES);
            (mime, size, Body::from_stream(stream))
        }
    };
    let content_type =
        HeaderValue::from_str(&content_type).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let etag = HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
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
    Ok((StatusCode::OK, headers, body).into_response())
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
                async { Ok(vec![0u8; 8]) },
                "image/png".to_owned(),
                slow_decode,
            )));
        }
        for task in tasks {
            assert_eq!(
                task.await.unwrap().unwrap(),
                None,
                "показом остаётся оригинал"
            );
        }
        let peak = PEAK.load(Ordering::SeqCst);
        assert!(
            peak <= MAX_CONCURRENT_RENDITIONS,
            "одновременно раскодировались {peak}, а можно не больше {MAX_CONCURRENT_RENDITIONS}"
        );
    }

    static LOADED: AtomicUsize = AtomicUsize::new(0);
    static LOADED_PEAK: AtomicUsize = AtomicUsize::new(0);

    fn release(_: &[u8], _: &str) -> Rendition {
        LOADED.fetch_sub(1, Ordering::SeqCst);
        Rendition::Original
    }

    /// Оригинал читается только под слотом раскодировки: десять одновременных запросов
    /// держат в памяти не больше двух оригиналов, а не десять в ожидании очереди.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn originals_are_loaded_only_inside_a_decode_slot() {
        LOADED.store(0, Ordering::SeqCst);
        LOADED_PEAK.store(0, Ordering::SeqCst);
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let load = async {
                tokio::task::yield_now().await;
                let loaded = LOADED.fetch_add(1, Ordering::SeqCst) + 1;
                LOADED_PEAK.fetch_max(loaded, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(vec![0u8; 8])
            };
            tasks.push(tokio::spawn(render_preview(
                load,
                "image/png".to_owned(),
                release,
            )));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }
        let peak = LOADED_PEAK.load(Ordering::SeqCst);
        assert!(
            (1..=MAX_CONCURRENT_RENDITIONS).contains(&peak),
            "в памяти было {peak} оригиналов, а можно не больше {MAX_CONCURRENT_RENDITIONS}"
        );
    }
}
