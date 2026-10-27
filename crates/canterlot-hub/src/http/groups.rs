use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::{ApiJson, ApiPath},
};
use crate::groups::{self, Group};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(update, delete))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupBody {
    pub name: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupPatch {
    pub name: Option<String>,
    pub position: Option<i32>,
}
#[utoipa::path(get,path="/api/v1/groups",tag="groups",responses((status=200,description="Группы",body=Vec<Group>)))]
async fn list(State(s): State<AppState>, o: Owner) -> Result<Json<Vec<Group>>, ApiError> {
    Ok(Json(
        groups::list(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/groups",tag="groups",params(("Idempotency-Key"=String,Header)),request_body=GroupBody,responses((status=201,description="Группа создана",body=Group)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<GroupBody>,
) -> Result<(StatusCode, Json<Group>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(groups::create_group(&s.pool, o.owner_id, &key, &b.name, c).await?),
    ))
}
#[utoipa::path(patch,path="/api/v1/groups/{id}",tag="groups",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=GroupPatch,responses((status=200,description="Группа изменена",body=Group)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<GroupPatch>,
) -> Result<Json<Group>, ApiError> {
    Ok(Json(
        groups::update_group(&s.pool, o.owner_id, id, &key, b.name, b.position, c).await?,
    ))
}
#[utoipa::path(delete,path="/api/v1/groups/{id}",tag="groups",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=204,description="Группа удалена")))]
async fn delete(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<StatusCode, ApiError> {
    groups::delete_group(&s.pool, o.owner_id, id, &key, c).await?;
    Ok(StatusCode::NO_CONTENT)
}
