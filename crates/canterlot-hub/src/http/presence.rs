//! Присутствие: PWA говорит, что чат открыт и виден на экране.
//!
//! Отметка живёт в памяти хаба и только глушит пуши о сессии; в базе её нет.
//! Отдельный `POST`, а не операция WSS: скрытие обязано доехать и при `pagehide`,
//! когда сокет уже закрывается, а `keepalive`-fetch с CSRF-заголовком переживает уход.

use axum::{extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::ApiJson,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(report))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresenceBody {
    pub conversation_id: Uuid,
    /// `true` — чат открыт и виден; `false` — скрыт, закрыт или страница уходит.
    pub visible: bool,
}

#[utoipa::path(post, path="/api/v1/presence", tag="events", request_body=PresenceBody, responses(
    (status=204, description="Отметка учтена"),
    (status=404, description="Разговор не найден", body=super::error::ErrorEnvelope),
))]
async fn report(
    State(s): State<AppState>,
    o: Owner,
    ApiJson(b): ApiJson<PresenceBody>,
) -> Result<StatusCode, ApiError> {
    let known: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(b.conversation_id)
    .bind(o.owner_id)
    .fetch_one(&s.pool)
    .await?;
    if !known {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    s.presence.report(
        b.conversation_id,
        &o.session_id.to_string(),
        b.visible,
        tokio::time::Instant::now(),
    );
    Ok(StatusCode::NO_CONTENT)
}
