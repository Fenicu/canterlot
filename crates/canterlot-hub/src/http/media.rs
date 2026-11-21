//! Выдача картинок, на которые смотрел агент: только владельцу и только связанные с блоком.

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    attachments::BinaryContent,
    auth::Owner,
    error::{ApiError, ErrorCode},
    etag_matches,
    extract::ApiPath,
    range,
};
use crate::media::{self, MediaError};

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

#[utoipa::path(get,path="/api/v1/media/{id}",tag="media",params(("id"=Uuid,Path),("Range"=Option<String>,Header,description="Один байтовый диапазон: bytes=a-b, bytes=a- или bytes=-n; несколько диапазонов и неверная запись игнорируются"),("If-Range"=Option<String>,Header,description="Сильный ETag: при другом ETag диапазон игнорируется и отдаётся весь файл")),responses((status=200,description="Картинка, на которую смотрел агент: тип из проверенного списка, показ на месте, ETag равен SHA-256",content((BinaryContent = "image/png"),(BinaryContent = "image/jpeg"),(BinaryContent = "image/webp"),(BinaryContent = "image/gif")),headers(("ETag"=String,description="SHA-256 содержимого в кавычках"),("Accept-Ranges"=String,description="Всегда bytes"))),(status=206,description="Запрошенный диапазон; те же заголовки, что у полного ответа",content((BinaryContent = "image/png"),(BinaryContent = "image/jpeg"),(BinaryContent = "image/webp"),(BinaryContent = "image/gif")),headers(("Content-Range"=String,description="bytes первый-последний/размер"))),(status=304,description="Содержимое не изменилось: отдаётся при совпадении If-None-Match"),(status=416,description="Диапазон целиком за пределами файла",headers(("Content-Range"=String,description="bytes */размер")))))]
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
    let etag_value =
        HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let mut response = HeaderMap::new();
    response.insert(header::CONTENT_TYPE, content_type);
    response.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    response.insert(header::ETAG, etag_value);
    response.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    // Медиа неизменно: повторный показ и прокрутка ленты берутся из кеша браузера.
    response.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    range::respond(found.file, found.size, &headers, &etag, response).await
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
