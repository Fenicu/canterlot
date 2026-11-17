//! Выдача картинок, на которые смотрел агент: только владельцу и только связанные с блоком.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::AsyncReadExt;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    attachments::BinaryContent,
    auth::Owner,
    error::{ApiError, ErrorCode},
    etag_matches,
    extract::ApiPath,
};
use crate::{
    attachments::blobstore::IO_CHUNK_BYTES,
    media::{self, MediaError},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_media))
        .routes(routes!(get_media_preview))
}

impl From<MediaError> for ApiError {
    fn from(error: MediaError) -> Self {
        match error {
            MediaError::NotFound => ApiError::new(ErrorCode::NotFound),
            MediaError::Db(error) => error.into(),
            other => {
                let api = ApiError::new(ErrorCode::Internal);
                tracing::error!(kind = other.kind(), correlation_id = %api.correlation_id, "media read failed");
                api
            }
        }
    }
}

#[utoipa::path(get,path="/api/v1/media/{id}",tag="media",params(("id"=Uuid,Path)),responses((status=200,description="Картинка, на которую смотрел агент: тип из проверенного списка, показ на месте, ETag равен SHA-256",content((BinaryContent = "image/png"),(BinaryContent = "image/jpeg"),(BinaryContent = "image/webp"),(BinaryContent = "image/gif")),headers(("ETag"=String,description="SHA-256 содержимого в кавычках"))),(status=304,description="Содержимое не изменилось: отдаётся при совпадении If-None-Match")))]
async fn get_media(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let found = media::open_for_download(&s.pool, &*s.blobs, o.owner_id, id).await?;
    let etag = format!("\"{}\"", found.sha256);
    if etag_matches(&headers, &etag) {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let content_type =
        HeaderValue::from_str(&found.mime).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let etag = HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let stream =
        tokio_util::io::ReaderStream::with_capacity(found.file.take(found.size), IO_CHUNK_BYTES);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(found.size));
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
    // Медиа неизменно: повторный показ и прокрутка ленты берутся из кеша браузера.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    Ok((StatusCode::OK, headers, Body::from_stream(stream)).into_response())
}

#[utoipa::path(get,path="/api/v1/media/{id}/preview",tag="media",params(("id"=Uuid,Path)),responses((status=200,description="Уменьшенный показ картинки: JPEG не больше 1024 по длинной стороне; анимация и слишком крупные файлы отдаются как есть",content((BinaryContent = "image/jpeg"),(BinaryContent = "image/gif"),(BinaryContent = "image/webp"),(BinaryContent = "image/png")),headers(("ETag"=String,description="SHA-256 оригинала плюс версия показа"))),(status=304,description="Показ не изменился: отдаётся при совпадении If-None-Match")))]
async fn get_media_preview(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let found = media::open_for_download(&s.pool, &*s.blobs, o.owner_id, id).await?;
    super::preview::serve_preview(found.mime, found.size, found.sha256, found.file, &headers).await
}
