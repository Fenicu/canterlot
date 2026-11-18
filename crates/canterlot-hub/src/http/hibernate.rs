//! Настройка усыпления простаивающих сессий: меняет только владелец.

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiJson,
};
use crate::{
    hibernate::{self, HibernateSettings},
    idempotency,
};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read, replace))
}

#[utoipa::path(get,path="/api/v1/hibernate-settings",tag="conversations",responses((status=200,description="Настройка усыпления простаивающих сессий",body=HibernateSettings)))]
async fn read(State(s): State<AppState>, o: Owner) -> Result<Json<HibernateSettings>, ApiError> {
    Ok(Json(
        hibernate::settings(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(put,path="/api/v1/hibernate-settings",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=HibernateSettings,responses((status=200,description="Настройка сохранена",body=HibernateSettings)))]
async fn replace(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(b): ApiJson<HibernateSettings>,
) -> Result<Json<HibernateSettings>, ApiError> {
    let owner = o.owner_id;
    let request = b.clone();
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_hibernate_settings",
        &request,
        None,
        move |conn| {
            Box::pin(async move {
                Ok::<_, ApiError>(hibernate::replace_settings_in_tx(conn, owner, &b).await?)
            })
        },
    )
    .await?;
    let _ = outcome;
    Ok(Json(request))
}
