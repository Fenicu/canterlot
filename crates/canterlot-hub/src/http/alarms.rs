//! HTTP-доступ владельца к будильникам разговора: чтение и отмена.

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::ApiPath,
};
use crate::{session_tools::alarms, session_tools::ToolErrorCode};
use axum::{Json, extract::State};
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

#[utoipa::path(delete,path="/api/v1/conversations/{id}/alarms/{alarm_id}",tag="alarms",params(("id"=Uuid,Path,description="Разговор"),("alarm_id"=Uuid,Path,description="Будильник")),responses(
    (status=200,description="Будильник отменён или уже завершён",body=alarms::AlarmView),
    (status=404,description="Будильник не найден",body=super::error::ErrorEnvelope)
))]
async fn cancel(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, alarm_id)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<alarms::AlarmView>, ApiError> {
    match alarms::cancel_owner(&state.pool, owner.owner_id, id, alarm_id).await {
        Ok(view) => Ok(Json(view)),
        Err(error) if error.code == ToolErrorCode::AlarmNotFound => {
            Err(ApiError::new(ErrorCode::NotFound))
        }
        Err(_) => Err(ApiError::new(ErrorCode::Internal)),
    }
}
