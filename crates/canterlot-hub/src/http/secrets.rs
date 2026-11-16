//! HTTP владельца для секретных запросов: список, ответ и отзыв.
//!
//! Путь ответа намеренно не проходит через общий слой идемпотентности: тот
//! хеширует тело запроса и хранит его в `idempotency_keys`. Здесь тело не
//! хешируется и не сохраняется ни в каком виде, а `Idempotency-Key` (его
//! присылает PWA) игнорируется.

use axum::{Json, body::Bytes, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Correlation, Owner},
    error::{ApiError, ErrorCode},
    extract::ApiPath,
};
use crate::secrets::{self, AnswerBody, SecretRequestView};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(answer))
        .routes(routes!(revoke))
}

#[derive(serde::Serialize, utoipa::ToSchema)]
pub struct SecretRequestList {
    pub items: Vec<SecretRequestView>,
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/secret-requests",tag="secrets",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Все запросы разговора, новые первыми",body=SecretRequestList),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<SecretRequestList>, ApiError> {
    let items = secrets::list(&state.pool, owner.owner_id, id).await?;
    Ok(Json(SecretRequestList { items }))
}

#[utoipa::path(post,path="/api/v1/secret-requests/{id}/answer",tag="secrets",params(("id"=Uuid,Path,description="Запрос секрета")),request_body=AnswerBody,responses(
    (status=200,description="Ответ принят; для значения — доставка началась",body=SecretRequestView),
    (status=404,description="Запрос не найден",body=super::error::ErrorEnvelope),
    (status=409,description="Машина не на связи, запрос закрыт или превышен предел",body=super::error::ErrorEnvelope),
    (status=413,description="Значение больше 16 КиБ",body=super::error::ErrorEnvelope)
))]
async fn answer(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    Correlation(correlation): Correlation,
    body: Bytes,
) -> Result<Json<SecretRequestView>, ApiError> {
    // Разбор без эха тела: ошибка serde называет поле, но не значение, а наружу
    // уходит только `invalid_request`.
    let body: AnswerBody =
        serde_json::from_slice(&body).map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?;
    let answer = body.parse().ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest))?;
    let view = secrets::answer(
        &state.pool,
        &state.registry,
        owner.owner_id,
        id,
        answer,
        correlation,
    )
    .await?;
    Ok(Json(view))
}

#[utoipa::path(post,path="/api/v1/secret-requests/{id}/revoke",tag="secrets",params(("id"=Uuid,Path,description="Запрос секрета")),responses(
    (status=200,description="Файл будет удалён воркером",body=SecretRequestView),
    (status=404,description="Запрос не найден",body=super::error::ErrorEnvelope),
    (status=409,description="Значение ещё не доставлено",body=super::error::ErrorEnvelope)
))]
async fn revoke(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    Correlation(correlation): Correlation,
) -> Result<Json<SecretRequestView>, ApiError> {
    let view = secrets::revoke(&state.pool, owner.owner_id, id, correlation).await?;
    Ok(Json(view))
}
