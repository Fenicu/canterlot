//! HTTP владельца: облачные окна раннеров — список с деньгами и «Удалить сейчас». Создание
//! окон (карточки, кнопка, автоматика) и настройки бюджета сюда не входят.

use axum::{Json, extract::State};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::ApiPath,
};
use crate::{
    cloud::windows::{self, CloudWindowView, DeleteRequested},
    idempotency,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(delete_now))
}

/// Столько законченных окон показывает список вместе с живыми.
const ENDED_LIMIT: i64 = 20;

#[derive(Serialize, ToSchema)]
pub struct WindowList {
    /// Живые окна и последние 20 законченных, новые первыми.
    pub items: Vec<CloudWindowView>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeleteWindowResult {
    /// Что случилось с окном: `deleting` (раннер отозван, сервер удаляется), `canceled`
    /// (сервер ещё не создавался), `not_yet` (создание вызвано, сервера в списках нет),
    /// `already_ended`.
    pub outcome: DeleteRequested,
    pub window: CloudWindowView,
}

#[utoipa::path(get,path="/api/v1/cloud/windows",tag="cloud",responses(
    (status=200,description="Окна облачных серверов раннеров: живые и последние 20 законченных, с ценой и расходом в копейках",body=WindowList)
))]
async fn list(State(state): State<AppState>, _owner: Owner) -> Result<Json<WindowList>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let now = Utc::now();
    let items = windows::list(&mut conn, ENDED_LIMIT)
        .await?
        .iter()
        .map(|window| windows::view(window, now))
        .collect();
    Ok(Json(WindowList { items }))
}

#[utoipa::path(post,path="/api/v1/cloud/windows/{id}/delete",tag="cloud",params(("id"=Uuid,Path,description="Окно"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Удаление принято: раннер отзывается, сервер удаляет служба окон и подтверждает конец оплаты списком проекта",body=DeleteWindowResult),
    (status=404,description="Окна нет",body=super::error::ErrorEnvelope)
))]
async fn delete_now(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<Json<DeleteWindowResult>, ApiError> {
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner.owner_id,
        &key,
        "cloud_window_delete",
        &id,
        None,
        move |conn| {
            Box::pin(async move {
                let now = Utc::now();
                let outcome =
                    windows::request_delete_in_tx(conn, id, windows::REASON_OWNER, now).await?;
                if outcome == DeleteRequested::NotFound {
                    return Err(ApiError::new(ErrorCode::NotFound));
                }
                let window = windows::load(conn, id)
                    .await?
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
                Ok::<_, ApiError>(DeleteWindowResult {
                    outcome,
                    window: windows::view(&window, now),
                })
            })
        },
    )
    .await?;
    if outcome.value.outcome == DeleteRequested::Deleting {
        // Токен раннера гаснет сразу после фиксации; если не вышло, служба окон отзовёт его
        // перед первой попыткой удаления.
        if let Err(error) = windows::revoke_cloud_machine(&state.pool).await {
            tracing::warn!(
                kind = "cloud_window_revoke",
                error = %crate::dberr::describe(&error),
                "the cloud runner is revoked by the window service instead"
            );
        }
    }
    Ok(Json(outcome.value))
}
