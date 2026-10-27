use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode, ErrorEnvelope},
    extract::{ApiJson, ApiPath},
};
use crate::{
    delivery::ContentBlock,
    scheduler::{self, NewScheduled, PatchScheduled, ScheduledMessage},
};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(update, cancel))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleBody {
    pub run_at: DateTime<Utc>,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub attachment_ids: Vec<Uuid>,
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/scheduled",tag="scheduled",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Отложенные сообщения",body=Vec<ScheduledMessage>),
    (status=404,description="Разговор не найден",body=ErrorEnvelope)
))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Vec<ScheduledMessage>>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let found: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(owner.owner_id)
    .fetch_one(&mut *conn)
    .await?;
    if !found {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    Ok(Json(
        scheduler::list_messages(&mut conn, owner.owner_id, id).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/scheduled",tag="scheduled",params(("id"=Uuid,Path,description="Разговор"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=ScheduleBody,responses(
    (status=201,description="Сообщение запланировано",body=ScheduledMessage),
    (status=409,description="Конфликт или слишком много сообщений",body=ErrorEnvelope),
    (status=422,description="Время вне допустимого диапазона",body=ErrorEnvelope)
))]
async fn create(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<ScheduleBody>,
) -> Result<(StatusCode, Json<ScheduledMessage>), ApiError> {
    let message = scheduler::schedule_message(
        &state.pool,
        owner.owner_id,
        &key,
        NewScheduled {
            conversation_id: id,
            run_at: body.run_at,
            content: body.content,
            attachment_ids: body.attachment_ids,
        },
        Utc::now(),
        correlation,
    )
    .await?;
    state.scheduler.notify();
    Ok((StatusCode::CREATED, Json(message)))
}

#[utoipa::path(patch,path="/api/v1/scheduled/{id}",tag="scheduled",params(("id"=Uuid,Path,description="Отложенное сообщение"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=PatchScheduled,responses(
    (status=200,description="Сообщение изменено",body=ScheduledMessage),
    (status=409,description="Сообщение уже обработано",body=ErrorEnvelope)
))]
async fn update(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(patch): ApiJson<PatchScheduled>,
) -> Result<Json<ScheduledMessage>, ApiError> {
    let message = scheduler::update_message(
        &state.pool,
        owner.owner_id,
        id,
        &key,
        patch,
        Utc::now(),
        correlation,
    )
    .await?;
    state.scheduler.notify();
    Ok(Json(message))
}

#[utoipa::path(delete,path="/api/v1/scheduled/{id}",tag="scheduled",params(("id"=Uuid,Path,description="Отложенное сообщение"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Сообщение отменено",body=ScheduledMessage),
    (status=409,description="Сообщение уже обработано",body=ErrorEnvelope)
))]
async fn cancel(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<Json<ScheduledMessage>, ApiError> {
    let message =
        scheduler::cancel_message(&state.pool, owner.owner_id, id, &key, correlation).await?;
    state.scheduler.notify();
    Ok(Json(message))
}
