use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiPath,
};
use crate::crunch::{self, CancelResumeOutcome, ResumeAtResetOutcome, SpendOutcome};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(spend_reserve))
        .routes(routes!(resume_at_reset))
        .routes(routes!(cancel_resume_at_reset))
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

/// Одноразовое «Продолжить при сбросе»: обычная пауза лимита становится удержанием
/// с продолжением после подтверждённого сброса; «кранч» остаётся выключенным.
#[utoipa::path(post,path="/api/v1/conversations/{id}/auto-resume",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Одноразовое продолжение взведено",body=ResumeAtResetOutcome),(status=409,description="Нет паузы лимита",body=super::error::ErrorEnvelope)))]
async fn resume_at_reset(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<ResumeAtResetOutcome>, ApiError> {
    Ok(Json(
        crunch::resume_at_reset(
            &s.pool,
            o.owner_id,
            id,
            &key,
            &s.limits,
            chrono::Utc::now(),
            c,
        )
        .await?,
    ))
}

/// Отказ от одноразового продолжения: задание пробуждения отменяется, пауза лимита
/// возвращается с прежними провайдером, окном и сроком сброса.
#[utoipa::path(delete,path="/api/v1/conversations/{id}/auto-resume",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Одноразовое продолжение отменено",body=CancelResumeOutcome)))]
async fn cancel_resume_at_reset(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<CancelResumeOutcome>, ApiError> {
    Ok(Json(
        crunch::cancel_resume_at_reset(&s.pool, o.owner_id, id, &key, chrono::Utc::now(), c)
            .await?,
    ))
}
