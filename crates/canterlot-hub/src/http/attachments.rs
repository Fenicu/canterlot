//! HTTP вложений: создание, загрузка частями, завершение, отмена и безопасная выдача.

use std::{fmt::Write, io};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath},
};
use crate::attachments::{
    self, AttachmentError, AttachmentView, IMAGE_MIMES, NewAttachment, UPLOAD_CHUNK_BYTES, upload,
};

const UPLOAD_OFFSET: &str = "upload-offset";
const UPLOAD_LENGTH: &str = "upload-length";

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create))
        .routes(routes!(get_one, cancel))
        .routes(routes!(complete))
        // Тело загрузки читается потоком самим обработчиком с пределом `upload_chunk_bytes`;
        // общий предел JSON к этому маршруту не относится.
        .merge(
            OpenApiRouter::new()
                .routes(routes!(download, upload_offset, upload_chunk))
                .routes(routes!(preview))
                .layer(DefaultBodyLimit::disable()),
        )
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct UploadOffer {
    pub offset: u64,
    pub upload_chunk_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedAttachment {
    #[serde(flatten)]
    pub attachment: AttachmentView,
    pub upload: UploadOffer,
}

/// Двоичное содержимое файла.
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct BinaryContent(#[allow(dead_code)] Vec<u8>);

#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteBody {
    /// Контрольная сумма, с которой клиент завершает загрузку; другая сумма даёт конфликт.
    pub sha256: Option<String>,
}

#[utoipa::path(post,path="/api/v1/attachments",tag="attachments",params(("Idempotency-Key"=String,Header)),request_body=NewAttachment,responses((status=201,description="Вложение создано, загрузка с нулевого смещения",body=CreatedAttachment)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<NewAttachment>,
) -> Result<(StatusCode, Json<CreatedAttachment>), ApiError> {
    let attachment = attachments::create(&s.pool, o.owner_id, &key, body, c).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedAttachment {
            attachment,
            upload: UploadOffer {
                offset: 0,
                upload_chunk_bytes: UPLOAD_CHUNK_BYTES,
            },
        }),
    ))
}

#[utoipa::path(get,path="/api/v1/attachments/{id}",tag="attachments",params(("id"=Uuid,Path)),responses((status=200,description="Вложение",body=AttachmentView)))]
async fn get_one(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AttachmentView>, ApiError> {
    Ok(Json(
        attachments::get(&s.pool, &*s.blobs, o.owner_id, id).await?,
    ))
}

#[utoipa::path(delete,path="/api/v1/attachments/{id}",tag="attachments",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=204,description="Загрузка отменена или неиспользованное вложение удалено")))]
async fn cancel(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<StatusCode, ApiError> {
    attachments::cancel(&s.pool, &*s.blobs, o.owner_id, id, &key, c).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post,path="/api/v1/attachments/{id}/complete",tag="attachments",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body(content=CompleteBody,description="Необязательное тело"),responses((status=200,description="Содержимое проверено, надёжно опубликовано, вложение готово",body=AttachmentView)))]
async fn complete(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<AttachmentView>, ApiError> {
    let body = match body {
        Ok(bytes) if bytes.is_empty() => CompleteBody::default(),
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?
        }
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return Err(ApiError::new(ErrorCode::ResourceLimit));
        }
        Err(_) => return Err(ApiError::new(ErrorCode::InvalidRequest)),
    };
    let view = attachments::complete(
        &s.pool,
        &*s.blobs,
        o.owner_id,
        id,
        body.sha256.as_deref(),
        &key,
        c,
    )
    .await?;
    Ok(Json(view))
}

fn offset_headers(offset: u64, length: Option<u64>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(UPLOAD_OFFSET, HeaderValue::from(offset));
    if let Some(length) = length {
        headers.insert(UPLOAD_LENGTH, HeaderValue::from(length));
    }
    headers
}

#[utoipa::path(head,path="/api/v1/attachments/{id}/content",tag="attachments",params(("id"=Uuid,Path)),responses((status=200,description="Принятое смещение загрузки",headers(("Upload-Offset"=u64,description="Принятые байты"),("Upload-Length"=u64,description="Объявленный размер")))))]
async fn upload_offset(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    let (offset, size) = upload::upload_offset(&s.pool, &*s.blobs, o.owner_id, id).await?;
    Ok((StatusCode::OK, offset_headers(offset, Some(size))).into_response())
}

fn parse_offset(headers: &HeaderMap) -> Result<u64, ApiError> {
    let mut values = headers.get_all(UPLOAD_OFFSET).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .ok()
            .filter(|v| !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest)),
        _ => Err(ApiError::new(ErrorCode::InvalidRequest)),
    }
}

#[utoipa::path(patch,path="/api/v1/attachments/{id}/content",tag="attachments",params(("id"=Uuid,Path),("Upload-Offset"=u64,Header,description="Смещение, с которого продолжается загрузка")),request_body(content=BinaryContent,content_type="application/octet-stream",description="Не больше upload_chunk_bytes"),responses((status=204,description="Часть принята",headers(("Upload-Offset"=u64,description="Принятые байты")))))]
async fn upload_chunk(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    Correlation(c): Correlation,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let offset = parse_offset(&headers)?;
    let Ok(_slot) = s.upload_slots.clone().try_acquire_owned() else {
        return Err(AttachmentError::TooManyUploads.into());
    };
    let stream = body
        .into_data_stream()
        .map_err(|_| io::Error::other("request body interrupted"))
        .boxed();
    let written = upload::upload_chunk(
        &s.pool,
        &*s.blobs,
        o.owner_id,
        id,
        offset,
        stream,
        UPLOAD_CHUNK_BYTES,
        c,
    )
    .await?;
    Ok((StatusCode::NO_CONTENT, offset_headers(written, None)).into_response())
}

/// Ролики, которые плеер браузера играет прямо из вложения; остальные только скачиваются.
const VIDEO_MIMES: &[&str] = &["video/mp4", "video/quicktime", "video/webm"];

/// Тип изображения или ролика отдаётся только из списка и при совпадающей сигнатуре файла.
/// Без верного типа `<video>` не играет: iOS Safari не берёт поток байтов.
async fn served_type(mime: &str, file: &mut tokio::fs::File) -> io::Result<&'static str> {
    let Some(expected) = IMAGE_MIMES.iter().chain(VIDEO_MIMES).find(|m| **m == mime) else {
        return Ok("application/octet-stream");
    };
    let mut head = [0u8; 12];
    let mut read = 0;
    while read < head.len() {
        let n = file.read(&mut head[read..]).await?;
        if n == 0 {
            break;
        }
        read += n;
    }
    file.seek(io::SeekFrom::Start(0)).await?;
    let head = &head[..read];
    let matches = match *expected {
        "image/png" => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => head.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a"),
        "image/webp" => head.len() == 12 && &head[..4] == b"RIFF" && &head[8..] == b"WEBP",
        // MP4 и QuickTime начинаются с атома `ftyp` после его длины; WebM — с заголовка EBML.
        "video/mp4" | "video/quicktime" => head.len() >= 8 && &head[4..8] == b"ftyp",
        "video/webm" => head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        _ => false,
    };
    Ok(if matches {
        expected
    } else {
        "application/octet-stream"
    })
}

/// `filename*` по RFC 8187: всё вне attr-char кодируется процентами.
/// Проверенная растровая картинка показывается на месте: браузер даёт её открыть,
/// скопировать и сохранить своим меню. Всё остальное только скачивается.
fn disposition(name: &str, content_type: &str) -> HeaderValue {
    let kind = if content_type.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    let mut value = format!("{kind}; filename*=UTF-8''");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            value.push(b as char);
        } else {
            let _ = write!(value, "%{b:02X}");
        }
    }
    HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("attachment"))
}

#[utoipa::path(get,path="/api/v1/attachments/{id}/preview",tag="attachments",params(("id"=Uuid,Path)),responses((status=200,description="Уменьшенный показ картинки: JPEG не больше 1024 по длинной стороне; анимация отдаётся как есть; у не-картинок показа нет",content_type="image/jpeg",body=BinaryContent),(status=304,description="Показ не изменился: отдаётся при совпадении If-None-Match")))]
async fn preview(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let mut found = attachments::open_for_download(&s.pool, &*s.blobs, o.owner_id, id).await?;
    // Показ есть только у настоящих картинок: подделка под картинку и текст идут мимо.
    let shown = served_type(&found.mime, &mut found.file)
        .await
        .map_err(AttachmentError::from)?;
    if !shown.starts_with("image/") {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    super::preview::serve_preview(found.mime, found.size, found.sha256, found.file, &headers).await
}

#[utoipa::path(get,path="/api/v1/attachments/{id}/content",tag="attachments",params(("id"=Uuid,Path),("Range"=Option<String>,Header,description="Один байтовый диапазон: bytes=a-b, bytes=a- или bytes=-n; несколько диапазонов и неверная запись игнорируются"),("If-Range"=Option<String>,Header,description="Сильный ETag: при другом ETag диапазон игнорируется и отдаётся весь файл")),responses((status=200,description="Содержимое готового вложения: проверенная картинка показывается на месте, ролик (mp4, quicktime, webm с верной сигнатурой) отдаётся своим типом для плеера, остальное скачивается; имя файла в Content-Disposition",content_type="application/octet-stream",body=BinaryContent,headers(("ETag"=String,description="SHA-256 содержимого в кавычках"),("Accept-Ranges"=String,description="Всегда bytes"))),(status=206,description="Запрошенный диапазон; те же заголовки, что у полного ответа",content_type="application/octet-stream",body=BinaryContent,headers(("Content-Range"=String,description="bytes первый-последний/размер"))),(status=304,description="Содержимое не изменилось: отдаётся при совпадении If-None-Match"),(status=416,description="Диапазон целиком за пределами файла",headers(("Content-Range"=String,description="bytes */размер")))))]
async fn download(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let mut found = attachments::open_for_download(&s.pool, &*s.blobs, o.owner_id, id).await?;
    let etag = format!("\"{}\"", found.sha256);
    if super::etag_matches(&headers, &etag) {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }
    let etag_value =
        HeaderValue::from_str(&etag).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let content_type = served_type(&found.mime, &mut found.file)
        .await
        .map_err(AttachmentError::from)?;
    let mut response = HeaderMap::new();
    response.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.insert(header::ETAG, etag_value);
    response.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.insert(
        header::CONTENT_DISPOSITION,
        disposition(&found.name, content_type),
    );
    response.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    // Готовое вложение неизменно, а строки ленты при прокрутке монтируются заново.
    response.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400, immutable"),
    );
    super::range::respond(found.file, found.size, &headers, &etag, response).await
}
