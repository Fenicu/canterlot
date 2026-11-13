//! Словарь замен живой диктовки: GET отдаёт сохранённый список, PUT заменяет его целиком.

use axum::{
    Json,
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    response::Response,
};
use utoipa_axum::{router::OpenApiRouter, routes};

use serde::Serialize;
use utoipa::ToSchema;

use crate::{
    dictation::{
        HealthState,
        dictionary::{self, TermError, TermsBody},
        terms,
    },
    idempotency,
};

use super::{
    AppState,
    auth::{IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::ApiJson,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(socket))
        .routes(routes!(status))
        .routes(routes!(read_terms, replace_terms))
}

#[utoipa::path(get,path="/api/v1/dictation",tag="dictation",responses((status=101,description="WSS диктовки: start, звук, finish/cancel; ответ state/text/final/error, см. docs/design/dictation.md §9")))]
async fn socket(
    State(state): State<AppState>,
    owner: Owner,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let ws = ws.map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?;
    if state.shutdown.is_cancelled() {
        return Err(ApiError::new(ErrorCode::DbUnavailable));
    }
    Ok(crate::dictation::relay::upgrade(ws, state, owner))
}

/// Состояние службы распознавания речи для кнопки микрофона.
#[derive(Serialize, ToSchema)]
pub struct DictationStatus {
    /// Можно начинать диктовку: включена в конфигурации и служба здорова.
    pub available: bool,
    /// Почему недоступна: `disabled`, `no_service` или `unhealthy`.
    #[schema(nullable = false, value_type = Option<String>)]
    pub reason: Option<String>,
    #[schema(nullable = false, value_type = Option<String>)]
    pub mode: Option<String>,
    #[schema(nullable = false, value_type = Option<String>)]
    pub engine: Option<String>,
}

#[utoipa::path(get,path="/api/v1/dictation/status",tag="dictation",responses((status=200,description="Служба речи доступна или причина отказа",body=DictationStatus)))]
async fn status(State(s): State<AppState>, _o: Owner) -> Result<Json<DictationStatus>, ApiError> {
    let unavailable = |reason: &str| DictationStatus {
        available: false,
        reason: Some(reason.to_owned()),
        mode: None,
        engine: None,
    };
    if !s.dictation.enabled {
        return Ok(Json(unavailable("disabled")));
    }
    let health = s
        .dictation_state
        .health(&s.dictation, &s.dictation_timers)
        .await;
    Ok(Json(match health {
        HealthState::Down => unavailable("no_service"),
        HealthState::Up(health) if !health.ok => DictationStatus {
            available: false,
            reason: Some("unhealthy".to_owned()),
            mode: health.mode,
            engine: health.engine,
        },
        HealthState::Up(health) => DictationStatus {
            available: true,
            reason: None,
            mode: health.mode,
            engine: health.engine,
        },
    }))
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
