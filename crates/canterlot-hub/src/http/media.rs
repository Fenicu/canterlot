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
    extract::ApiPath,
};
use crate::{
    attachments::blobstore::IO_CHUNK_BYTES,
    media::{self, MediaError},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_media))
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

#[utoipa::path(get,path="/api/v1/media/{id}",tag="media",params(("id"=Uuid,Path)),responses((status=200,description="Картинка, на которую смотрел агент: тип из проверенного списка, показ на месте, ETag равен SHA-256",content((BinaryContent = "image/png"),(BinaryContent = "image/jpeg"),(BinaryContent = "image/webp"),(BinaryContent = "image/gif")),headers(("ETag"=String,description="SHA-256 содержимого в кавычках")))))]
async fn get_media(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let found = media::open_for_download(&s.pool, &*s.blobs, o.owner_id, id).await?;
    let content_type =
        HeaderValue::from_str(&found.mime).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let etag = HeaderValue::from_str(&format!("\"{}\"", found.sha256))
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
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
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok((StatusCode::OK, headers, Body::from_stream(stream)).into_response())
}
