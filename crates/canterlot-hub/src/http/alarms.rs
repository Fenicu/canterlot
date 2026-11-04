//! HTTP-доступ владельца к будильникам разговора: чтение и отмена.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::ApiPath,
};
use crate::{idempotency, session_tools::ToolErrorCode, session_tools::alarms};
use axum::{Json, extract::State};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list, cancel))
}

#[derive(serde::Serialize, ToSchema)]
pub struct AlarmList {
    pub alarms: Vec<alarms::AlarmView>,
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/alarms",tag="alarms",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Будильники разговора",body=AlarmList),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<AlarmList>, ApiError> {
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
    let rows = alarms::snapshot_rows(&mut conn, id).await?;
    Ok(Json(AlarmList {
        alarms: alarms::snapshot_views(&rows),
    }))
}

#[utoipa::path(delete,path="/api/v1/conversations/{id}/alarms/{alarm_id}",tag="alarms",params(("id"=Uuid,Path,description="Разговор"),("alarm_id"=Uuid,Path,description="Будильник"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Будильник отменён или уже завершён",body=alarms::AlarmView),
    (status=404,description="Будильник не найден",body=super::error::ErrorEnvelope)
))]
async fn cancel(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, alarm_id)): ApiPath<(Uuid, Uuid)>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<Json<alarms::AlarmView>, ApiError> {
    let owner_id = owner.owner_id;
    let request = json!({"conversation_id": id, "alarm_id": alarm_id});
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "delete_alarm",
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                let view = alarms::cancel_owner_in_tx(conn, owner_id, id, alarm_id, correlation)
                    .await
                    .map_err(|error| match error.code {
                        ToolErrorCode::AlarmNotFound => ApiError::new(ErrorCode::NotFound),
                        _ => ApiError::new(ErrorCode::Internal),
                    })?;
                Ok::<_, ApiError>(view)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
