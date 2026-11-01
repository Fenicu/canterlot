//! Белый список делегирования: что агент запускает без вопроса владельцу.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath},
};
use crate::{
    idempotency,
    session_tools::{
        accept::{self, AcceptState},
        whitelist::{self, WhitelistEntry, WhitelistView},
    },
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(read, replace))
        .routes(routes!(accept_session))
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WhitelistBody {
    /// `null` возвращает встроенный список; поле обязательно, чтобы пустое тело его не сбрасывало.
    #[serde(deserialize_with = "nullable_entries")]
    #[schema(required = true)]
    pub entries: Option<Vec<WhitelistEntry>>,
}

fn nullable_entries<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<WhitelistEntry>>, D::Error> {
    Option::deserialize(d)
}

#[utoipa::path(get,path="/api/v1/spawn-whitelist",tag="session_tools",responses((status=200,description="Белый список делегирования",body=WhitelistView)))]
async fn read(State(s): State<AppState>, o: Owner) -> Result<Json<WhitelistView>, ApiError> {
    Ok(Json(
        whitelist::view(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/spawn-whitelist",tag="session_tools",params(("Idempotency-Key"=String,Header)),request_body=WhitelistBody,responses((status=200,description="Список сохранён",body=WhitelistView)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<WhitelistBody>,
) -> Result<Json<WhitelistView>, ApiError> {
    if b.entries.as_deref().is_some_and(|e| !whitelist::valid(e)) {
        return Err(ApiError::new(ErrorCode::SpawnWhitelistInvalid));
    }
    let owner = o.owner_id;
    let entries = b.entries.clone();
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_spawn_whitelist",
        &b,
        None,
        move |conn| {
            Box::pin(async move {
                whitelist::replace(conn, owner, entries.as_deref()).await?;
                Ok::<_, ApiError>(whitelist::view(conn, owner).await?)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}

#[derive(Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptBody {
    /// Убрать рабочую копию, даже если в ней есть незафиксированные изменения.
    #[serde(default)]
    pub force: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct AcceptView {
    pub state: AcceptState,
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/accept",tag="session_tools",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=AcceptBody,responses((status=200,description="Приём результата делегированной сессии начат",body=AcceptView)))]
async fn accept_session(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<AcceptBody>,
) -> Result<Json<AcceptView>, ApiError> {
    let state = accept::accept_for_owner(&s.pool, o.owner_id, id, body.force, &key, c).await?;
    accept::wake(Some(&s.dispatcher), &s.pool, id);
    Ok(Json(AcceptView { state }))
}
