use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiPath,
};
use crate::crunch::{self, SpendOutcome};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(spend_reserve))
}

/// Ручное продолжение одной сессии за счёт недельного резерва до сброса недели.
#[utoipa::path(post,path="/api/v1/conversations/{id}/spend-reserve",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Решение о продолжении за счёт резерва",body=SpendOutcome)))]
async fn spend_reserve(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<SpendOutcome>, ApiError> {
    Ok(Json(
        crunch::spend_reserve(
            &s.pool,
            o.owner_id,
            id,
            &key,
            chrono::Utc::now(),
            &s.dispatcher,
            c,
        )
        .await?,
    ))
}
