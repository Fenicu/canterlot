//! Словарь замен живой диктовки: GET отдаёт сохранённый список, PUT заменяет его целиком.

use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    dictation::{
        dictionary::{self, TermError, TermsBody},
        terms,
    },
    idempotency,
};

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::ApiError,
    extract::ApiJson,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(read_terms, replace_terms))
}

impl From<TermError> for ApiError {
    fn from(error: TermError) -> Self {
        Self::new(match error {
            TermError::TooMany => super::error::ErrorCode::TooManyTerms,
            TermError::Empty => super::error::ErrorCode::TermEmpty,
            TermError::TooLong => super::error::ErrorCode::TermTooLong,
            TermError::Duplicate => super::error::ErrorCode::DuplicateTerm,
        })
    }
}

#[utoipa::path(get,path="/api/v1/dictation/terms",tag="dictation",responses((status=200,description="Словарь замен в порядке владельца",body=TermsBody)))]
async fn read_terms(State(s): State<AppState>, o: Owner) -> Result<Json<TermsBody>, ApiError> {
    let terms = terms::load(&mut *s.pool.acquire().await?, o.owner_id).await?;
    Ok(Json(TermsBody { terms }))
}

#[utoipa::path(put,path="/api/v1/dictation/terms",tag="dictation",params(("Idempotency-Key"=String,Header)),request_body=TermsBody,responses((status=200,description="Список заменён целиком",body=TermsBody)))]
async fn replace_terms(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(body): ApiJson<TermsBody>,
) -> Result<Json<TermsBody>, ApiError> {
    let stored = dictionary::validate(body.terms.clone())?;
    let owner = o.owner_id;
    let outcome = idempotency::run_in_tx(
        &s.pool,
        owner,
        &key,
        "put_dictation_terms",
        &body,
        None,
        move |conn| {
            Box::pin(async move {
                terms::replace(conn, owner, &stored).await?;
                Ok::<_, ApiError>(TermsBody { terms: stored })
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
