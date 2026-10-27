use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::{ApiPath, ApiQuery},
};
use crate::snapshot::{self, ConversationSnapshot, ListSnapshot, MachinesSnapshot, Snapshot};
use axum::{Json, extract::State};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(conversations))
        .routes(routes!(machines))
        .routes(routes!(conversation))
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct SnapshotParams {
    pub history_limit: Option<i64>,
}
#[utoipa::path(get,path="/api/v1/snapshot/conversations",tag="snapshots",responses((status=200,description="Снимок списка и граница журнала",body=Snapshot<ListSnapshot>)))]
async fn conversations(
    State(s): State<AppState>,
    o: Owner,
) -> Result<Json<Snapshot<ListSnapshot>>, ApiError> {
    Ok(Json(snapshot::conversations(&s.pool, o.owner_id).await?))
}
#[utoipa::path(get,path="/api/v1/snapshot/machines",tag="snapshots",responses((status=200,description="Снимок машин и граница журнала",body=Snapshot<MachinesSnapshot>)))]
async fn machines(
    State(s): State<AppState>,
    o: Owner,
) -> Result<Json<Snapshot<MachinesSnapshot>>, ApiError> {
    Ok(Json(
        snapshot::machines_with_config(&s.pool, o.owner_id, &s.machines).await?,
    ))
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/snapshot",tag="snapshots",params(("id"=Uuid,Path),SnapshotParams),responses((status=200,description="Снимок разговора и граница журнала",body=Snapshot<ConversationSnapshot>)))]
async fn conversation(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(p): ApiQuery<SnapshotParams>,
) -> Result<Json<Snapshot<ConversationSnapshot>>, ApiError> {
    let limit = p.history_limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    Ok(Json(
        snapshot::conversation_with_registry(&s.pool, o.owner_id, id, limit, Some(&s.registry))
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?,
    ))
}
