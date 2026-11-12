//! `/api/v1/auth/qr/*`: вход по QR (спецификация §7, ADR-0005).
//!
//! `start` и `poll` публичны (`auth::PUBLIC_AUTH`); `inspect`, `approve`, `deny` требуют
//! сеанса и свежести не требуют: риск ограничен каскадным отзывом.

use std::time::Instant;

use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    AppState,
    auth::{Client, CurrentSession, Owner},
    error::{ApiError, ErrorCode, ErrorEnvelope},
    extract::ApiJson,
    signin::{OwnerSessionView, SessionLocationView, issue, refused, with_cookies},
};
use crate::{
    bus::EventBus,
    owner_auth::{
        self, BUS_SCOPE, Method, bucket_key,
        geo::Location,
        qr::{self, State as QrState},
        sessions::{self, ClientInfo},
    },
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(qr_start))
        .routes(routes!(qr_poll))
        .routes(routes!(qr_inspect))
        .routes(routes!(qr_approve))
        .routes(routes!(qr_deny))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QrStartBody {
    /// Подсказки клиента для подписи устройства (как у входа по паролю).
    #[serde(default)]
    pub device: Option<Value>,
}

#[derive(Serialize, ToSchema)]
pub struct QrStartView {
    /// Содержимое QR: `{origin}/qr#<secret>`.
    pub url: String,
    /// Токен опроса: только для `POST /qr/poll` этого устройства, в код не попадает.
    pub poll_token: String,
    pub expires_at: DateTime<Utc>,
}

/// Новый код входа на 60 секунд. Не больше 10 запусков в минуту с одной корзины адресов
/// и 20 ожидающих кодов всего; сверх предела 429 сразу.
#[utoipa::path(post, path = "/api/v1/auth/qr/start", tag = "auth", request_body = QrStartBody, responses(
    (status = 200, description = "Код создан", body = QrStartView),
    (status = 429, description = "too_many_attempts, details.retry_after_seconds", body = ErrorEnvelope),
))]
async fn qr_start(
    State(state): State<AppState>,
    client: Client,
    ApiJson(body): ApiJson<QrStartBody>,
) -> Result<Response, ApiError> {
    let info = ClientInfo::located(
        client.ip,
        client.user_agent.clone(),
        body.device,
        &state.geo,
    );
    let bucket = bucket_key(client.ip);
    match qr::start(&state.pool, state.installation.owner_id, &bucket, &info).await? {
        qr::Start::Started {
            secret,
            poll_token,
            expires_at,
        } => Ok(Json(QrStartView {
            url: format!("{}/qr#{secret}", state.auth.origin()),
            poll_token,
            expires_at,
        })
        .into_response()),
        qr::Start::Limited { retry_after } => {
            tracing::info!(kind = "qr_start_limited", bucket = %bucket, "QR start refused");
            Ok(refused(retry_after))
        }
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QrPollBody {
    pub poll_token: String,
    /// Состояние, которое устройство уже показывает (по умолчанию `pending`): опрос
    /// отвечает, как только состояние станет другим, или через 25 секунд.
    #[serde(default)]
    pub after: Option<QrState>,
}

#[derive(Serialize, ToSchema)]
pub struct QrPollView {
    /// `expired` также значит «код не найден» и «сеанс уже выдан другому опросу»:
    /// устройство начинает заново.
    pub state: QrState,
    /// Только при `approved`: новый сеанс, cookie сеанса в `Set-Cookie` (cookie известного
    /// устройства вход по QR не получает).
    pub session: Option<OwnerSessionView>,
}

fn poll_view(state: QrState) -> Response {
    Json(QrPollView {
        state,
        session: None,
    })
    .into_response()
}

/// Ждёт уведомления шины о сеансах или QR (`auth`, а также `*` после переподключения).
async fn auth_notice(
    notices: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<crate::bus::Notice>>,
) {
    loop {
        match notices.recv().await {
            Ok(notice) if notice.scope == BUS_SCOPE || notice.scope == "*" => return,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                std::future::pending::<()>().await
            }
        }
    }
}

/// Долгий опрос: до 25 секунд, перепроверка строки каждые 500 мс или по уведомлению
/// шины (работает между процессами). Опрос, увидевший `approved`, создаёт сеанс; из
/// двух одновременных его получает ровно один, второй видит `expired`.
#[utoipa::path(post, path = "/api/v1/auth/qr/poll", tag = "auth", request_body = QrPollBody, responses(
    (status = 200, description = "Состояние кода; при approved — Set-Cookie сеанса", body = QrPollView),
    (status = 429, description = "too_many_attempts: больше 8 опросов ждут одновременно", body = ErrorEnvelope),
))]
async fn qr_poll(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<QrPollBody>,
) -> Result<Response, ApiError> {
    let Ok(_permit) = state.qr_polls.clone().try_acquire_owned() else {
        return Ok(refused(std::time::Duration::from_secs(1)));
    };
    let after = body.after.unwrap_or(QrState::Pending);
    let mut notices = EventBus::local().subscribe();
    let deadline = Instant::now() + qr::POLL_WAIT;
    loop {
        let (observed, id) = qr::observe(&state.pool, &body.poll_token).await?;
        if let (QrState::Approved, Some(id)) = (observed, id) {
            let Some((created, info)) =
                qr::deliver(&state.pool, state.installation.owner_id, id).await?
            else {
                return Ok(poll_view(QrState::Expired));
            };
            // Сеанс по QR не получает cookie известного устройства: он не доказал пароль.
            let (session, cookies) = issue(&state, &created, &info, Method::Qr).await?;
            let view = Json(QrPollView {
                state: QrState::Approved,
                session: Some(session),
            });
            return Ok(with_cookies(view.into_response(), cookies));
        }
        if observed != after || matches!(observed, QrState::Denied | QrState::Expired) {
            return Ok(poll_view(observed));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(poll_view(observed));
        }
        tokio::select! {
            () = tokio::time::sleep(qr::RECHECK.min(deadline - now)) => {}
            () = auth_notice(&mut notices) => {}
            () = state.shutdown.cancelled() => return Ok(poll_view(observed)),
        }
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QrSecretBody {
    /// Фрагмент ссылки из кода (после `#`).
    pub secret: String,
}

/// Карточка запроса на вход. Адрес, место и возраст кода видит хаб; устройство заявлено
/// клиентом и показывается как «заявлено устройством: …».
#[derive(Serialize, ToSchema)]
pub struct QrRequestView {
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Сколько секунд назад код показан.
    pub age_seconds: i64,
    /// `null` — адрес неизвестен.
    pub ip: Option<String>,
    pub location: Option<SessionLocationView>,
    /// Подпись по user agent и подсказкам клиента; заявлена клиентом.
    pub label: Option<String>,
    pub user_agent: Option<String>,
    /// Страна запроса известна и отличается от страны текущего сеанса.
    pub other_country: bool,
}

#[derive(Serialize, ToSchema)]
pub struct QrDecisionView {
    pub state: QrState,
}

fn expired() -> ApiError {
    ApiError::new(ErrorCode::QrExpired)
}

fn country(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|value| serde_json::from_value::<Location>(value.clone()).ok())
        .and_then(|location| location.country_code)
}

/// Открыть код на устройстве с сеансом: запрос переходит в `scanned`, опрос нового
/// устройства показывает «Код отсканирован».
#[utoipa::path(post, path = "/api/v1/auth/qr/inspect", tag = "auth", request_body = QrSecretBody, responses(
    (status = 200, description = "Запрос на вход", body = QrRequestView),
    (status = 410, description = "qr_expired: кода нет, он истёк или уже решён", body = ErrorEnvelope),
))]
async fn qr_inspect(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(body): ApiJson<QrSecretBody>,
) -> Result<Json<QrRequestView>, ApiError> {
    let request = qr::inspect(&state.pool, owner.owner_id, &body.secret)
        .await?
        .ok_or_else(expired)?;
    let mine = sessions::get(&state.pool, owner.owner_id, session.id).await?;
    let theirs = country(request.location.as_ref());
    let other_country = matches!(
        (theirs, mine.and_then(|row| country(row.last_location.as_ref()))),
        (Some(a), Some(b)) if a != b
    );
    Ok(Json(QrRequestView {
        age_seconds: (Utc::now() - request.created_at).num_seconds().max(0),
        created_at: request.created_at,
        expires_at: request.expires_at,
        label: owner_auth::label(request.user_agent.as_deref(), request.client_hints.as_ref()),
        location: request
            .location
            .and_then(|value| serde_json::from_value(value).ok()),
        ip: request.ip,
        user_agent: request.user_agent,
        other_country,
    }))
}

/// Разрешить вход: новое устройство получит сеанс с `approved_by` = текущий сеанс.
/// Такой сеанс несвеж и завершается вместе с подтвердившим (каскад). Один сеанс держит
/// не больше 5 живых или ждущих выдачи входов, подтверждённых им.
#[utoipa::path(post, path = "/api/v1/auth/qr/approve", tag = "auth", request_body = QrSecretBody, responses(
    (status = 200, description = "Вход разрешён", body = QrDecisionView),
    (status = 409, description = "qr_approvals_exhausted: этот сеанс уже подтвердил 5 живых входов", body = ErrorEnvelope),
    (status = 410, description = "qr_expired", body = ErrorEnvelope),
))]
async fn qr_approve(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(body): ApiJson<QrSecretBody>,
) -> Result<Json<QrDecisionView>, ApiError> {
    match qr::decide(&state.pool, owner.owner_id, &body.secret, Some(session.id)).await? {
        qr::Decision::Done => {}
        qr::Decision::Gone => return Err(expired()),
        qr::Decision::Exhausted => {
            return Err(ApiError::new(ErrorCode::QrApprovalsExhausted));
        }
    }
    tracing::info!(kind = "qr_approved", session_id = %session.id, "QR sign-in approved");
    Ok(Json(QrDecisionView {
        state: QrState::Approved,
    }))
}

#[utoipa::path(post, path = "/api/v1/auth/qr/deny", tag = "auth", request_body = QrSecretBody, responses(
    (status = 200, description = "Вход отклонён", body = QrDecisionView),
    (status = 410, description = "qr_expired", body = ErrorEnvelope),
))]
async fn qr_deny(
    State(state): State<AppState>,
    owner: Owner,
    ApiJson(body): ApiJson<QrSecretBody>,
) -> Result<Json<QrDecisionView>, ApiError> {
    if qr::decide(&state.pool, owner.owner_id, &body.secret, None).await? != qr::Decision::Done {
        return Err(expired());
    }
    tracing::info!(kind = "qr_denied", "QR sign-in denied");
    Ok(Json(QrDecisionView {
        state: QrState::Denied,
    }))
}
