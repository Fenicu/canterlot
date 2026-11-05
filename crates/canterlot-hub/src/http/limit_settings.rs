//! Пороги лимитов владельца: окраска остатка и недельный резерв. Как и белый список
//! делегирования, меняет только владелец через HTTP; GET отдаёт действующие значения.

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::ApiJson,
};
use crate::{
    crunch, idempotency,
    limit_settings::{self, LimitSettings, LimitSettingsView},
};
use axum::{Json, extract::State};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read, replace))
}

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitSettingsBody {
    /// `null` возвращает значения по умолчанию; поле обязательно, чтобы пустое тело не сбрасывало.
    #[serde(deserialize_with = "nullable_settings")]
    #[schema(required = true)]
    pub settings: Option<LimitSettings>,
}

fn nullable_settings<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<LimitSettings>, D::Error> {
    Option::deserialize(d)
}

#[utoipa::path(get,path="/api/v1/limit-settings",tag="limits",responses((status=200,description="Пороги лимитов владельца",body=LimitSettingsView)))]
async fn read(State(s): State<AppState>, o: Owner) -> Result<Json<LimitSettingsView>, ApiError> {
    Ok(Json(
        limit_settings::view(&mut *s.pool.acquire().await?, o.owner_id, &s.limits).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/limit-settings",tag="limits",params(("Idempotency-Key"=String,Header)),request_body=LimitSettingsBody,responses((status=200,description="Пороги сохранены",body=LimitSettingsView)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<LimitSettingsBody>,
) -> Result<Json<LimitSettingsView>, ApiError> {
    if b.settings.is_some_and(|settings| !settings.valid()) {
        return Err(ApiError::new(ErrorCode::LimitSettingsInvalid));
    }
    let owner = o.owner_id;
    let settings = b.settings;
    let config = s.limits.clone();
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_limit_settings",
        &b,
        None,
        move |conn| {
            Box::pin(async move {
                let before = limit_settings::effective(conn, owner, &config).await?;
                limit_settings::replace(conn, owner, settings).await?;
                let after = limit_settings::effective(conn, owner, &config).await?;
                // Окраска и резерв меняются независимо: смена только цветов не должна
                // трогать действующий резерв и его гистерезис.
                if before.reserve_remaining_pct != after.reserve_remaining_pct {
                    let now = Utc::now();
                    // Пересчёт сразу в той же транзакции: сохранение не ждёт следующего показания.
                    crunch::reapply_reserve_in_tx(conn, owner, &config, now, Uuid::now_v7())
                        .await?;
                }
                Ok::<_, ApiError>(limit_settings::view(conn, owner, &config).await?)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
