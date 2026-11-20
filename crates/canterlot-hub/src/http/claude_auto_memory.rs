//! Настройка «Встроенная память Claude»: меняет только владелец, GET отдаёт действующую.

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiJson,
};
use crate::{
    idempotency,
    memory::auto_memory::{self, ClaudeAutoMemorySettings},
};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read, replace))
}

#[utoipa::path(get,path="/api/v1/claude-auto-memory-settings",tag="conversations",responses((status=200,description="Встроенная память Claude: on или off",body=ClaudeAutoMemorySettings)))]
async fn read(
    State(s): State<AppState>,
    o: Owner,
) -> Result<Json<ClaudeAutoMemorySettings>, ApiError> {
    Ok(Json(
        auto_memory::settings(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/claude-auto-memory-settings",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=ClaudeAutoMemorySettings,responses((status=200,description="Настройка сохранена; действует на сессии после их перезапуска",body=ClaudeAutoMemorySettings)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<ClaudeAutoMemorySettings>,
) -> Result<Json<ClaudeAutoMemorySettings>, ApiError> {
    let owner = o.owner_id;
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_claude_auto_memory_settings",
        &b,
        None,
        move |conn| {
            Box::pin(async move {
                Ok::<_, ApiError>(auto_memory::replace_settings_in_tx(conn, owner, b).await?)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
