//! HTTP владельца: очередь запусков разговора — чтение, отмена, «Запустить сейчас».

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode, ErrorDetails, ErrorReason},
    extract::ApiPath,
};
use crate::{
    idempotency,
    session_tools::{ToolError, ToolErrorCode, spawn_queue},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(read))
        .routes(routes!(cancel))
        .routes(routes!(start_now))
}

#[derive(Serialize, ToSchema)]
pub struct SpawnQueueView {
    pub items: Vec<spawn_queue::QueueItemView>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct SpawnQueueActionView {
    pub status: String,
    pub queue_id: Uuid,
    pub session_id: Option<Uuid>,
    pub title: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
}

impl From<spawn_queue::StartNowOutcome> for SpawnQueueActionView {
    fn from(value: spawn_queue::StartNowOutcome) -> Self {
        Self {
            status: value.status,
            queue_id: value.queue_id,
            session_id: value.session_id,
            title: value.title,
            harness: value.harness,
            model: value.model,
        }
    }
}

fn action_of(value: &serde_json::Value) -> SpawnQueueActionView {
    SpawnQueueActionView {
        status: value["status"].as_str().unwrap_or_default().to_owned(),
        queue_id: serde_json::from_value(value["queue_id"].clone()).unwrap_or_else(|_| Uuid::nil()),
        session_id: serde_json::from_value(value["session_id"].clone()).ok(),
        title: value["title"].as_str().map(str::to_owned),
        harness: value["harness"].as_str().map(str::to_owned),
        model: value["model"].as_str().map(str::to_owned),
    }
}

fn tool_error(error: ToolError) -> ApiError {
    match error.code {
        ToolErrorCode::NotParent | ToolErrorCode::SessionNotFound => {
            ApiError::new(ErrorCode::NotFound)
        }
        ToolErrorCode::MachineBusy | ToolErrorCode::SpawnQueueFull => {
            ApiError::new(ErrorCode::InvalidState).with_details(ErrorDetails {
                reason: Some(error.message),
                ..Default::default()
            })
        }
        ToolErrorCode::InvalidArguments => {
            ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
                reason: Some(error.message),
                ..Default::default()
            })
        }
        _ => ApiError::new(ErrorCode::Internal),
    }
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/spawn-queue",tag="spawn_queue",params(("id"=Uuid,Path,description="Разговор")),responses(
    (status=200,description="Очередь запусков разговора",body=SpawnQueueView),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn read(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<SpawnQueueView>, ApiError> {
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
    let items = spawn_queue::snapshot_views(&mut conn, id).await?;
    Ok(Json(SpawnQueueView { items }))
}

#[utoipa::path(delete,path="/api/v1/conversations/{id}/spawn-queue/{queue_id}",tag="spawn_queue",params(("id"=Uuid,Path,description="Разговор"),("queue_id"=Uuid,Path,description="Элемент очереди"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Элемент отменён или уже завершён",body=SpawnQueueActionView),
    (status=404,description="Элемент не найден",body=super::error::ErrorEnvelope)
))]
async fn cancel(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, queue_id)): ApiPath<(Uuid, Uuid)>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<Json<SpawnQueueActionView>, ApiError> {
    let owner_id = owner.owner_id;
    let request = serde_json::json!({"conversation_id": id, "queue_id": queue_id});
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "delete_spawn_queue",
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                crate::conversations::lock_owner(&mut *conn, owner_id).await?;
                let value = spawn_queue::cancel_in_tx(
                    conn,
                    owner_id,
                    id,
                    queue_id,
                    correlation,
                    chrono::Utc::now(),
                    false,
                )
                .await
                .map_err(tool_error)?;
                Ok::<_, ApiError>(action_of(&value))
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/spawn-queue/{queue_id}/start-now",tag="spawn_queue",params(("id"=Uuid,Path,description="Разговор"),("queue_id"=Uuid,Path,description="Элемент очереди"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Сессия запущена",body=SpawnQueueActionView),
    (status=404,description="Элемент не найден",body=super::error::ErrorEnvelope),
    (status=409,description="Элемент уже завершён или запуск отклонён: `details.reason` = `spawn_refused`, разбор в `details.spawn_refusal`",body=super::error::ErrorEnvelope)
))]
async fn start_now(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, queue_id)): ApiPath<(Uuid, Uuid)>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<Json<SpawnQueueActionView>, ApiError> {
    let owner_id = owner.owner_id;
    let registry = state.registry.clone();
    let session_tools = state.session_tools.clone();
    let spawn_queue_config = state.spawn_queue.clone();
    let guards = state.guards.clone();
    let routing_enabled = true;
    let routing_stale = state.routing_stale;
    let routing_queue_horizon = state.routing_queue_horizon;
    let request = serde_json::json!({"conversation_id": id, "queue_id": queue_id});
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "start_spawn_queue",
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                let runtime = spawn_queue::Runtime {
                    registry: Some(&registry),
                    session_tools: &session_tools,
                    spawn_queue: &spawn_queue_config,
                    guards: &guards,
                    routing_enabled,
                    routing_stale,
                    routing_queue_horizon,
                };
                let outcome = spawn_queue::start_now_in_tx(
                    conn,
                    &runtime,
                    owner_id,
                    id,
                    queue_id,
                    correlation,
                    chrono::Utc::now(),
                )
                .await
                .map_err(tool_error)?;
                Ok::<_, ApiError>(outcome)
            })
        },
    )
    .await?;
    let outcome = outcome.value;
    match outcome.status.as_str() {
        "started" | "already_started" => Ok(Json(SpawnQueueActionView::from(outcome))),
        _ => Err(
            ApiError::new(ErrorCode::InvalidState).with_details(ErrorDetails {
                reason: Some(ErrorReason::SpawnRefused.as_str().to_owned()),
                spawn_refusal: outcome.refusal,
                ..Default::default()
            }),
        ),
    }
}
