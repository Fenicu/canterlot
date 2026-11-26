//! Настройка напоминаний агентам: меняет только владелец, GET отдаёт действующую.

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiJson,
};
use crate::{
    idempotency,
    reminders::{self, AgentReminderSettings},
};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read, replace))
}

#[utoipa::path(get,path="/api/v1/agent-reminder-settings",tag="conversations",responses((status=200,description="Напоминания агентам о плане и списке задач",body=AgentReminderSettings)))]
async fn read(
    State(s): State<AppState>,
    o: Owner,
) -> Result<Json<AgentReminderSettings>, ApiError> {
    Ok(Json(
        reminders::settings(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/agent-reminder-settings",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=AgentReminderSettings,responses((status=200,description="Настройка сохранена; действует со следующей выдачи ввода",body=AgentReminderSettings)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<AgentReminderSettings>,
) -> Result<Json<AgentReminderSettings>, ApiError> {
    let owner = o.owner_id;
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_agent_reminder_settings",
        &b,
        None,
        move |conn| {
            Box::pin(async move {
                Ok::<_, ApiError>(reminders::replace_settings_in_tx(conn, owner, b).await?)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
