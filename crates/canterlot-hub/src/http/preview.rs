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
    let rendered = tokio::task::spawn_blocking(move || match rendition::rendition(&original, &mime) {
        Rendition::Scaled { bytes, .. } => (bytes, "image/jpeg".to_owned()),
        // Оригинал возвращается из потока вместе с решением: без лишнего копирования.
        Rendition::Original => (original, mime),
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let (bytes, content_type) = rendered;
    let content_type =
        HeaderValue::from_str(&content_type).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let etag = HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len() as u64));
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
