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
    cloud::{
        budget,
        windows::{self, CloudWindowView, DeleteRequested},
    },
    idempotency,
    session_tools::cloud_propose::{self, Opened},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(delete_now))
        .routes(routes!(budget_get))
        .routes(routes!(budget_put))
        .routes(routes!(budget_clear_block))
        .routes(routes!(propose))
}

/// Столько законченных окон показывает список вместе с живыми.
const ENDED_LIMIT: i64 = 20;

#[derive(Serialize, ToSchema)]
pub struct WindowList {
    /// Живые окна и последние 20 законченных, новые первыми.
    pub items: Vec<CloudWindowView>,
    /// Код блока провайдера (`abuse_prepayment_required`), пока он стоит: новые окна не
    /// создаются, пока владелец не снимет блок. Пусто, когда блока нет.
    pub provider_block_code: Option<String>,
    pub provider_block_at: Option<chrono::DateTime<Utc>>,
    /// Облако настроено на хабе (`[runners.cloud]`): без него окна не создаются и форма
    /// запуска в PWA не показывается.
    pub configured: bool,
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
    let block = windows::provider_block(&mut conn).await?;
    Ok(Json(WindowList {
        items,
        provider_block_code: block.as_ref().map(|(code, _)| code.clone()),
        provider_block_at: block.map(|(_, at)| at),
        configured: state.cloud.is_some(),
    }))
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

/// Бюджет облака: настройки владельца, расход текущего месяца и цены профилей.
#[utoipa::path(get,path="/api/v1/cloud/budget",tag="cloud",responses(
    (status=200,description="Бюджет облака: месяц, расход, остаток, режим, ревизия, блок и профили с ценой часа",body=budget::BudgetView)
))]
async fn budget_get(
    State(state): State<AppState>,
    _owner: Owner,
) -> Result<Json<budget::BudgetView>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let pricing = match state.cloud.as_ref() {
        Some(cloud) => cloud.pricing().await.ok(),
        None => None,
    };
    Ok(Json(
        budget::view(&mut conn, pricing.as_ref(), state.cloud.is_some()).await?,
    ))
}

/// Изменить бюджет и режим: устаревшая ревизия меняет ничего и даёт 409.
#[utoipa::path(put,path="/api/v1/cloud/budget",tag="cloud",params(("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=budget::Update,responses(
    (status=200,description="Новая ревизия настроек",body=budget::BudgetView),
    (status=409,description="Настройки изменились с другого устройства",body=super::error::ErrorEnvelope)
))]
async fn budget_put(
    State(state): State<AppState>,
    owner: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Json(body): Json<budget::Update>,
) -> Result<Json<budget::BudgetView>, ApiError> {
    let owner_id = owner.owner_id;
    let fingerprint = body.clone();
    let result = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "cloud_budget_put",
        &fingerprint,
        None,
        move |conn| {
            Box::pin(async move {
                let revision =
                    budget::update_in_tx(conn, &body)
                        .await
                        .map_err(|error| match error {
                            budget::UpdateError::Stale => ApiError::new(ErrorCode::StaleRevision),
                            budget::UpdateError::Invalid(reason) => ApiError::new(
                                ErrorCode::InvalidRequest,
                            )
                            .with_details(super::error::ErrorDetails {
                                reason: Some(reason.to_owned()),
                                ..Default::default()
                            }),
                            budget::UpdateError::Db(error) => error.into(),
                        })?;
                Ok::<_, ApiError>(revision)
            })
        },
    )
    .await?;
    let _ = result;
    budget_get(State(state), owner).await
}

/// Снять блок провайдера: снова разрешить создание окон.
#[utoipa::path(post,path="/api/v1/cloud/budget/clear-block",tag="cloud",params(("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Блок снят (или его не было)")
))]
async fn budget_clear_block(
    State(state): State<AppState>,
    owner: Owner,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner.owner_id,
        &key,
        "cloud_budget_clear_block",
        &(),
        None,
        move |conn| {
            Box::pin(async move {
                let cleared = windows::clear_provider_block(conn).await?;
                Ok::<_, ApiError>(serde_json::json!({"cleared": cleared}))
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ProposeBody {
    /// Сколько часов: 1..=4.
    hours: u32,
    /// Причина, которую увидит владелец на карточке.
    reason: String,
}

/// Форма владельца «Запустить облачный раннер»: создаёт карточку, не окно.
#[utoipa::path(post,path="/api/v1/cloud/proposals",tag="cloud",params(("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=ProposeBody,responses(
    (status=200,description="Карточка создана: владелец решает отдельно",body=Opened),
    (status=409,description="Облако выключено, есть живое окно или открытая карточка",body=super::error::ErrorEnvelope)
))]
async fn propose(
    State(state): State<AppState>,
    owner: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Json(body): Json<ProposeBody>,
) -> Result<Json<Opened>, ApiError> {
    let Some(cloud) = state.cloud.clone() else {
        return Err(ApiError::new(ErrorCode::InvalidState));
    };
    // Та же граница, что у инструмента агента, и до умножения: переполнение `u32` не должно
    // доходить до цены и резерва.
    if !(1..=4).contains(&body.hours) {
        return Err(ApiError::new(ErrorCode::CloudHoursOutOfRange).with_details(
            super::error::ErrorDetails {
                reason: Some("hours".to_owned()),
                ..Default::default()
            },
        ));
    }
    let carrier = cloud.config().carrier_machine_id;
    let owner_id = owner.owner_id;
    let fingerprint = body.clone();
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "cloud_proposal",
        &fingerprint,
        None,
        move |conn| {
            let body = body.clone();
            let cloud = cloud.clone();
            Box::pin(async move {
                let proposer = cloud_propose::system_proposer(conn, owner_id, carrier).await?;
                let pricing = cloud
                    .pricing()
                    .await
                    .map_err(|_| ApiError::new(ErrorCode::InvalidState))?;
                cloud_propose::open_in_tx(
                    conn,
                    cloud.config(),
                    &pricing,
                    &proposer,
                    body.hours * 60,
                    body.reason.clone(),
                )
                .await
                .map_err(|error| match error {
                    cloud_propose::CardError::Db(error) => ApiError::from(error),
                    other => ApiError::new(ErrorCode::InvalidState).with_details(
                        super::error::ErrorDetails {
                            reason: Some(other.to_string()),
                            ..Default::default()
                        },
                    ),
                })
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
