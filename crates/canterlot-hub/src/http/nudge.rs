//! HTTP-доступ владельца к правилу автопинка: чтение, правка, пауза и снятие.
//!
//! Правило ставится на любую сессию владельца, в отличие от агента, который
//! ограничен своими потомками. Текст пресета здесь не отдаётся: только правило
//! и последние события толчков.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath},
};
use crate::{
    idempotency,
    session_tools::{
        ToolError, ToolErrorCode,
        nudge::{self, NudgeEventView, NudgeRuleView, OwnerUpdate},
    },
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(read))
        .routes(routes!(events))
        .routes(routes!(update, clear))
}

#[derive(Serialize, ToSchema)]
pub struct NudgeState {
    pub rule: Option<NudgeRuleView>,
    pub events: Vec<NudgeEventView>,
}

#[derive(Serialize, ToSchema)]
pub struct NudgeEvents {
    pub events: Vec<NudgeEventView>,
}

async fn owned(state: &AppState, owner: Uuid, id: Uuid) -> Result<(), ApiError> {
    let found: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(owner)
    .fetch_one(&state.pool)
    .await?;
    if found {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::NotFound))
    }
}

fn tool_error(error: ToolError) -> ApiError {
    match error.code {
        ToolErrorCode::SessionNotFound => ApiError::new(ErrorCode::NotFound),
        ToolErrorCode::ProfileUnknown => ApiError::new(ErrorCode::ProfileUnknown),
        ToolErrorCode::InvalidArguments => ApiError::new(ErrorCode::InvalidRequest),
        _ => ApiError::new(ErrorCode::Internal),
    }
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/nudge",tag="nudge",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Правило автопинка и последние события",body=NudgeState),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn read(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NudgeState>, ApiError> {
    owned(&state, owner.owner_id, id).await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(NudgeState {
        rule: nudge::snapshot_rule(&mut conn, id).await?,
        events: nudge::snapshot_events(&mut conn, id, 5).await?,
    }))
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/nudge/events",tag="nudge",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Последние события автопинка",body=NudgeEvents),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn events(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<NudgeEvents>, ApiError> {
    owned(&state, owner.owner_id, id).await?;
    let mut conn = state.pool.acquire().await?;
    Ok(Json(NudgeEvents {
        events: nudge::snapshot_events(&mut conn, id, 20).await?,
    }))
}

#[utoipa::path(put,path="/api/v1/conversations/{id}/nudge",tag="nudge",params(("id"=Uuid,Path,description="Разговор"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=OwnerUpdate,responses(
    (status=200,description="Правило сохранено",body=NudgeRuleView),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope),
    (status=422,description="Некорректные значения",body=super::error::ErrorEnvelope)
))]
async fn update(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<OwnerUpdate>,
) -> Result<Json<NudgeRuleView>, ApiError> {
    let owner_id = owner.owner_id;
    let request = serde_json::to_value(&body).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "put_nudge",
        &request,
        Some(id),
        move |conn| {
            let body = body.clone();
            Box::pin(async move {
                let rule = nudge::owner_update(conn, owner_id, id, &body, correlation)
                    .await
                    .map_err(tool_error)?;
                Ok::<_, ApiError>(rule)
            })
        },
    )
    .await?;
    state.dispatcher.trigger(id);
    Ok(Json(outcome.value))
}

#[utoipa::path(delete,path="/api/v1/conversations/{id}/nudge",tag="nudge",params(("id"=Uuid,Path,description="Разговор"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=204,description="Правило снято"),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn clear(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<StatusCode, ApiError> {
    let owner_id = owner.owner_id;
    let request = serde_json::json!({"conversation_id": id});
    idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "delete_nudge",
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                nudge::owner_clear(conn, owner_id, id, correlation)
                    .await
                    .map_err(tool_error)?;
                Ok::<_, ApiError>(())
            })
        },
    )
    .await?;
    state.dispatcher.trigger(id);
    Ok(StatusCode::NO_CONTENT)
}
