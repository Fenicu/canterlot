//! Настройка подсказок следующего сообщения: меняет только владелец, GET отдаёт действующую.

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiJson,
};
use crate::{
    idempotency,
    prompt_suggestions::{self, PromptSuggestionSettings},
};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read, replace))
}

#[utoipa::path(get,path="/api/v1/prompt-suggestion-settings",tag="conversations",responses((status=200,description="Подсказки следующего сообщения владельца",body=PromptSuggestionSettings)))]
async fn read(
    State(s): State<AppState>,
    o: Owner,
) -> Result<Json<PromptSuggestionSettings>, ApiError> {
    Ok(Json(
        prompt_suggestions::settings(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/prompt-suggestion-settings",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=PromptSuggestionSettings,responses((status=200,description="Настройка сохранена; выключение стирает показанные подсказки",body=PromptSuggestionSettings)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<PromptSuggestionSettings>,
) -> Result<Json<PromptSuggestionSettings>, ApiError> {
    let owner = o.owner_id;
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_prompt_suggestion_settings",
        &b,
        None,
        move |conn| {
            Box::pin(async move {
                Ok::<_, ApiError>(
                    prompt_suggestions::replace_settings_in_tx(conn, owner, b, Uuid::now_v7())
                        .await?,
                )
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
