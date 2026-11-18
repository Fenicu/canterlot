//! Заготовки сообщений: список глобальных и заготовок проекта, CRUD с ревизией и порядок.
//! Все изменения идут с `Idempotency-Key`; события уходят в область владельца.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::message_snippets::{self, MessageSnippet, NewSnippet, ReorderSnippets, SnippetPatch};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct SnippetQuery {
    /// `global` или `project`.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Канонический workdir сессии; непустой только для `project`.
    #[serde(default)]
    pub project_key: String,
}

fn default_scope() -> String {
    message_snippets::SCOPE_GLOBAL.to_owned()
}

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create, reorder))
        .routes(routes!(get_one, update, delete))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSnippetBody {
    pub scope: String,
    #[serde(default)]
    pub project_key: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSnippetBody {
    pub title: Option<String>,
    pub body: Option<String>,
    pub position: Option<i32>,
    pub base_revision: i64,
}

fn parse_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id)
        .map_err(|_| crate::message_snippets::MessageSnippetError::Invalid { field: "id" }.into())
}

#[utoipa::path(get,path="/api/v1/message-snippets",tag="message-snippets",params(SnippetQuery),responses((status=200,description="Заготовки владельца в области",body=Vec<MessageSnippet>)))]
async fn list(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(q): ApiQuery<SnippetQuery>,
) -> Result<Json<Vec<MessageSnippet>>, ApiError> {
    Ok(Json(
        message_snippets::list(
            &mut *s.pool.acquire().await?,
            o.owner_id,
            &q.scope,
            &q.project_key,
        )
        .await?,
    ))
}

#[utoipa::path(get,path="/api/v1/message-snippets/{id}",tag="message-snippets",params(("id"=String,Path)),responses((status=200,description="Заготовка",body=MessageSnippet)))]
async fn get_one(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<MessageSnippet>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    Ok(Json(
        message_snippets::get(&mut conn, o.owner_id, parse_id(&id)?).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/message-snippets",tag="message-snippets",params(("Idempotency-Key"=String,Header)),request_body=CreateSnippetBody,responses((status=201,description="Заготовка создана",body=MessageSnippet)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<CreateSnippetBody>,
) -> Result<(StatusCode, Json<MessageSnippet>), ApiError> {
    let body = NewSnippet {
        scope: b.scope,
        project_key: b.project_key,
        title: b.title,
        body: b.body,
        position: b.position,
    };
    Ok((
        StatusCode::CREATED,
        Json(message_snippets::create(&s.pool, o.owner_id, &key, body, c).await?),
    ))
}

#[utoipa::path(patch,path="/api/v1/message-snippets/{id}",tag="message-snippets",params(("id"=String,Path),("Idempotency-Key"=String,Header)),request_body=UpdateSnippetBody,responses((status=200,description="Заготовка изменена",body=MessageSnippet)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<UpdateSnippetBody>,
) -> Result<Json<MessageSnippet>, ApiError> {
    let patch = SnippetPatch {
        title: b.title,
        body: b.body,
        position: b.position,
        base_revision: b.base_revision,
    };
    Ok(Json(
        message_snippets::update(&s.pool, o.owner_id, parse_id(&id)?, &key, patch, c).await?,
    ))
}

#[utoipa::path(delete,path="/api/v1/message-snippets/{id}",tag="message-snippets",params(("id"=String,Path),("Idempotency-Key"=String,Header)),responses((status=204,description="Заготовка удалена")))]
async fn delete(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<StatusCode, ApiError> {
    message_snippets::delete(&s.pool, o.owner_id, parse_id(&id)?, &key, c).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put,path="/api/v1/message-snippets/order",tag="message-snippets",params(("Idempotency-Key"=String,Header)),request_body=ReorderSnippets,responses((status=200,description="Порядок сохранён",body=Vec<MessageSnippet>)))]
async fn reorder(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<ReorderSnippets>,
) -> Result<Json<Vec<MessageSnippet>>, ApiError> {
    Ok(Json(
        message_snippets::reorder(&s.pool, o.owner_id, &key, b, c).await?,
    ))
}
