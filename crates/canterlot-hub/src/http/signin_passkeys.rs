//! Ключи доступа: вход (`/auth/passkey/*`, публично), управление (`/auth/passkeys*`)
//! и повторный вход (`/auth/reauth/passkey/*`). Спецификация §4.2, ADR-0005.
//!
//! `options` — JSON для `navigator.credentials.create/get` в форме `{publicKey: …}`
//! (двоичные поля base64url); `credential` — `PublicKeyCredential.toJSON()` браузера.

use std::sync::Arc;

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
use uuid::Uuid;
use webauthn_rs::prelude::{
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    Webauthn,
};

use super::{
    AppState,
    auth::{Client, CurrentSession, Owner},
    error::{ApiError, ErrorCode, ErrorDetails, ErrorEnvelope},
    extract::{ApiJson, ApiPath},
    signin::{FreshView, SignInView, bucket_for, record_failure, refused, signed_in, too_many},
};
use crate::owner_auth::{
    Method, device,
    passkeys::{self, Kind, Stored},
    sessions,
    throttle::{self, Reservation},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(passkey_start))
        .routes(routes!(passkey_finish))
        .routes(routes!(list_passkeys))
        .routes(routes!(register_start))
        .routes(routes!(register_finish))
        .routes(routes!(rename_passkey, delete_passkey))
        .routes(routes!(reauth_start))
        .routes(routes!(reauth_finish))
}

/// Проверяющая сторона из `[auth]`: `rp_id`, `origin`, имя «canterlot». `None`, если
/// WebAuthn не принимает такой origin (например, IP-адрес).
pub fn relying_party(auth: &crate::config::AuthConfig) -> Option<Arc<Webauthn>> {
    let origin = webauthn_rs::prelude::Url::parse(auth.origin()).ok()?;
    match webauthn_rs::WebauthnBuilder::new(auth.rp_id(), &origin)
        .and_then(|builder| builder.rp_name("canterlot").build())
    {
        Ok(webauthn) => Some(Arc::new(webauthn)),
        Err(error) => {
            tracing::warn!(kind = "passkeys_unavailable", error = %error, "WebAuthn rejects [auth].origin / rp_id");
            None
        }
    }
}

fn relying(state: &AppState) -> Result<&Webauthn, ApiError> {
    state
        .webauthn
        .as_deref()
        .ok_or_else(|| ApiError::new(ErrorCode::PasskeysUnavailable))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasskeyStartBody {}

#[derive(Serialize, ToSchema)]
pub struct PasskeyChallengeView {
    /// Обряд, к которому относится ответ ключа; живёт 5 минут, используется один раз.
    pub challenge_id: Uuid,
    /// `{publicKey: …}` для `navigator.credentials.create()` или `get()`.
    #[schema(value_type = Object)]
    pub options: Value,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasskeySignIn {
    pub challenge_id: Uuid,
    /// `PublicKeyCredential.toJSON()`.
    #[schema(value_type = Object)]
    pub credential: Value,
    /// Подсказки клиента для подписи устройства.
    #[serde(default)]
    pub device: Option<Value>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasskeyRegisterFinish {
    pub challenge_id: Uuid,
    #[schema(value_type = Object)]
    pub credential: Value,
    /// До 64 символов; без имени — подпись устройства, на котором ключ добавлен.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasskeyReauthFinish {
    pub challenge_id: Uuid,
    #[schema(value_type = Object)]
    pub credential: Value,
}

#[derive(Serialize, ToSchema)]
pub struct PasskeyView {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Ключ синхронизируется между устройствами (менеджер паролей, iCloud, Google).
    pub backed_up: bool,
}

impl From<&Stored> for PasskeyView {
    fn from(stored: &Stored) -> Self {
        Self {
            id: stored.id,
            name: stored.name.clone(),
            created_at: stored.created_at,
            last_used_at: stored.last_used_at,
            backed_up: stored.backed_up,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct PasskeysView {
    pub passkeys: Vec<PasskeyView>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenamePasskey {
    /// 1..64 символа.
    pub name: String,
}

fn rejected() -> ApiError {
    ApiError::new(ErrorCode::PasskeyRejected)
}

fn options(value: &impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|_| ApiError::new(ErrorCode::Internal))
}

fn state_json(value: &impl Serialize) -> Result<Value, ApiError> {
    options(value)
}

fn clean_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()
        && name.chars().count() <= passkeys::NAME_CHARS
        && !name.chars().any(char::is_control))
    .then(|| name.to_owned())
}

fn invalid_name() -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
        field: Some("name".into()),
        ..Default::default()
    })
}

/// Начало входа по ключу: все ключи владельца в `allowCredentials`, проверка пользователя
/// обязательна. Не больше 10 начал в минуту с корзины и 500 живых обрядов всего; известное
/// устройство общим пределом не ограничено, атакующий не запирает им вход владельца.
#[utoipa::path(post, path = "/api/v1/auth/passkey/start", tag = "auth", request_body = PasskeyStartBody, responses(
    (status = 200, description = "Опции для navigator.credentials.get()", body = PasskeyChallengeView),
    (status = 409, description = "no_passkeys или passkeys_unavailable", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn passkey_start(
    State(state): State<AppState>,
    client: Client,
    ApiJson(_): ApiJson<PasskeyStartBody>,
) -> Result<Response, ApiError> {
    let webauthn = relying(&state)?;
    let (bucket, device) = bucket_for(&state, &client).await?;
    if let Some(retry_after) = passkeys::sign_in_limited(&state.pool, &bucket, device).await? {
        tracing::info!(kind = "passkey_start_limited", bucket = %bucket, "passkey start refused");
        return Ok(refused(retry_after));
    }
    let owner_id = state.installation.owner_id;
    let keys: Vec<_> = passkeys::list(&state.pool, owner_id)
        .await?
        .into_iter()
        .map(|stored| stored.passkey)
        .collect();
    if keys.is_empty() {
        return Err(ApiError::new(ErrorCode::NoPasskeys));
    }
    let (challenge, ceremony) = webauthn
        .start_passkey_authentication(&keys)
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let id = passkeys::store_challenge(
        &state.pool,
        owner_id,
        Kind::SignIn,
        None,
        Some(&bucket),
        state_json(&ceremony)?,
    )
    .await?;
    Ok(Json(PasskeyChallengeView {
        challenge_id: id,
        options: options(&challenge)?,
    })
    .into_response())
}

/// Проверка ответа ключа: состояние обряда забирается один раз, ключ должен быть
/// среди ключей владельца, счётчик и резервная копия сохраняются.
async fn verify(
    state: &AppState,
    webauthn: &Webauthn,
    kind: Kind,
    session: Option<Uuid>,
    challenge_id: Uuid,
    credential: Value,
) -> Result<bool, ApiError> {
    let owner_id = state.installation.owner_id;
    let Some(stored) =
        passkeys::take_challenge(&state.pool, owner_id, challenge_id, kind, session).await?
    else {
        return Ok(false);
    };
    let (Ok(ceremony), Ok(credential)) = (
        serde_json::from_value::<PasskeyAuthentication>(stored),
        serde_json::from_value::<PublicKeyCredential>(credential),
    ) else {
        return Ok(false);
    };
    let Ok(result) = webauthn.finish_passkey_authentication(&credential, &ceremony) else {
        return Ok(false);
    };
    Ok(passkeys::record_use(&state.pool, owner_id, &result).await?)
}

/// Завершение входа по ключу. Неудача (чужой, испорченный, повторный или просроченный
/// ответ) — 401 `invalid_credentials` и неудача перебора в корзине клиента; общая
/// блокировка входа по паролю ключи не касается.
#[utoipa::path(post, path = "/api/v1/auth/passkey/finish", tag = "auth", request_body = PasskeySignIn, responses(
    (status = 200, description = "Вход выполнен; Set-Cookie сеанса и устройства", body = SignInView),
    (status = 401, description = "invalid_credentials", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn passkey_finish(
    State(state): State<AppState>,
    client: Client,
    ApiJson(body): ApiJson<PasskeySignIn>,
) -> Result<Response, ApiError> {
    let webauthn = relying(&state)?;
    let (bucket, _) = bucket_for(&state, &client).await?;
    if let Reservation::Refused { retry_after } =
        throttle::reserve(&state.pool, &bucket, false).await?
    {
        return Ok(refused(retry_after));
    }
    if !verify(
        &state,
        webauthn,
        Kind::SignIn,
        None,
        body.challenge_id,
        body.credential,
    )
    .await?
    {
        record_failure(&state, &bucket).await?;
        tracing::info!(kind = "sign_in_failed", method = "passkey", bucket = %bucket, "passkey sign-in failed");
        return Err(ApiError::new(ErrorCode::InvalidCredentials));
    }
    throttle::succeeded(&state.pool, &bucket).await?;
    signed_in(&state, &client, Method::Passkey, body.device).await
}

#[utoipa::path(get, path = "/api/v1/auth/passkeys", tag = "auth", responses(
    (status = 200, description = "Ключи доступа владельца", body = PasskeysView),
))]
async fn list_passkeys(
    State(state): State<AppState>,
    owner: Owner,
) -> Result<Json<PasskeysView>, ApiError> {
    let stored = passkeys::list(&state.pool, owner.owner_id).await?;
    Ok(Json(PasskeysView {
        passkeys: stored.iter().map(PasskeyView::from).collect(),
    }))
}

/// Начало добавления ключа: нужен свежий сеанс. Уже добавленные ключи в
/// `excludeCredentials`, `residentKey: preferred`.
#[utoipa::path(post, path = "/api/v1/auth/passkeys/register/start", tag = "auth", request_body = PasskeyStartBody, responses(
    (status = 200, description = "Опции для navigator.credentials.create()", body = PasskeyChallengeView),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 409, description = "passkeys_unavailable", body = ErrorEnvelope),
))]
async fn register_start(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(_): ApiJson<PasskeyStartBody>,
) -> Result<Json<PasskeyChallengeView>, ApiError> {
    session.require_fresh()?;
    let webauthn = relying(&state)?;
    let exclude = passkeys::list(&state.pool, owner.owner_id)
        .await?
        .iter()
        .map(|stored| stored.passkey.cred_id().clone())
        .collect();
    let (challenge, ceremony) = webauthn
        .start_passkey_registration(owner.owner_id, &owner.login, &owner.login, Some(exclude))
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let mut options = options(&challenge)?;
    // Состояние регистрации residentKey не хранит: «preferred» в опциях не мешает finish.
    if let Some(selection) = options
        .pointer_mut("/publicKey/authenticatorSelection")
        .and_then(Value::as_object_mut)
    {
        selection.insert("residentKey".into(), Value::from("preferred"));
    }
    let id = passkeys::store_challenge(
        &state.pool,
        owner.owner_id,
        Kind::Register,
        Some(session.id),
        None,
        state_json(&ceremony)?,
    )
    .await?;
    Ok(Json(PasskeyChallengeView {
        challenge_id: id,
        options,
    }))
}

/// Завершение добавления в том же сеансе, что начал его.
#[utoipa::path(post, path = "/api/v1/auth/passkeys/register/finish", tag = "auth", request_body = PasskeyRegisterFinish, responses(
    (status = 200, description = "Ключ добавлен", body = PasskeyView),
    (status = 400, description = "invalid_request: имя", body = ErrorEnvelope),
    (status = 403, description = "passkey_rejected", body = ErrorEnvelope),
))]
async fn register_finish(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    client: Client,
    ApiJson(body): ApiJson<PasskeyRegisterFinish>,
) -> Result<Json<PasskeyView>, ApiError> {
    let webauthn = relying(&state)?;
    let name = match body.name.as_deref() {
        Some(name) if !name.trim().is_empty() => Some(clean_name(name).ok_or_else(invalid_name)?),
        _ => None,
    };
    let stored = passkeys::take_challenge(
        &state.pool,
        owner.owner_id,
        body.challenge_id,
        Kind::Register,
        Some(session.id),
    )
    .await?
    .ok_or_else(rejected)?;
    let ceremony: PasskeyRegistration = serde_json::from_value(stored).map_err(|_| rejected())?;
    let credential: RegisterPublicKeyCredential =
        serde_json::from_value(body.credential).map_err(|_| rejected())?;
    let passkey = webauthn
        .finish_passkey_registration(&credential, &ceremony)
        .map_err(|_| rejected())?;
    let name = match name {
        Some(name) => name,
        None => {
            let hints = sessions::get(&state.pool, owner.owner_id, session.id)
                .await?
                .and_then(|row| row.client_hints);
            device::passkey_name(client.user_agent.as_deref(), hints.as_ref())
        }
    };
    let id = passkeys::add(&state.pool, owner.owner_id, &passkey, &name)
        .await?
        .ok_or_else(rejected)?;
    tracing::info!(kind = "passkey_added", passkey_id = %id, "owner added a passkey");
    let stored = passkeys::list(&state.pool, owner.owner_id).await?;
    stored
        .iter()
        .find(|stored| stored.id == id)
        .map(|stored| Json(PasskeyView::from(stored)))
        .ok_or_else(|| ApiError::new(ErrorCode::Internal))
}

/// Новое имя ключа; свежести не требует.
#[utoipa::path(patch, path = "/api/v1/auth/passkeys/{id}", tag = "auth", request_body = RenamePasskey, responses(
    (status = 200, description = "Имя сохранено", body = PasskeyView),
    (status = 400, description = "invalid_request", body = ErrorEnvelope),
    (status = 404, description = "Ключа нет", body = ErrorEnvelope),
))]
async fn rename_passkey(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<RenamePasskey>,
) -> Result<Json<PasskeyView>, ApiError> {
    let name = clean_name(&body.name).ok_or_else(invalid_name)?;
    if !passkeys::rename(&state.pool, owner.owner_id, id, &name).await? {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    passkeys::list(&state.pool, owner.owner_id)
        .await?
        .iter()
        .find(|stored| stored.id == id)
        .map(|stored| Json(PasskeyView::from(stored)))
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))
}

/// Удаление ключа: нужен свежий сеанс. Ответ — оставшиеся ключи.
#[utoipa::path(delete, path = "/api/v1/auth/passkeys/{id}", tag = "auth", responses(
    (status = 200, description = "Ключ удалён; оставшиеся ключи", body = PasskeysView),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 404, description = "Ключа нет", body = ErrorEnvelope),
))]
async fn delete_passkey(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<PasskeysView>, ApiError> {
    session.require_fresh()?;
    if !passkeys::delete(&state.pool, owner.owner_id, id).await? {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    tracing::info!(kind = "passkey_deleted", passkey_id = %id, "owner deleted a passkey");
    list_passkeys(State(state), owner).await
}

/// Повторный вход ключом: обряд привязан к текущему сеансу.
#[utoipa::path(post, path = "/api/v1/auth/reauth/passkey/start", tag = "auth", request_body = PasskeyStartBody, responses(
    (status = 200, description = "Опции для navigator.credentials.get()", body = PasskeyChallengeView),
    (status = 409, description = "no_passkeys или passkeys_unavailable", body = ErrorEnvelope),
))]
async fn reauth_start(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(_): ApiJson<PasskeyStartBody>,
) -> Result<Json<PasskeyChallengeView>, ApiError> {
    let webauthn = relying(&state)?;
    let keys: Vec<_> = passkeys::list(&state.pool, owner.owner_id)
        .await?
        .into_iter()
        .map(|stored| stored.passkey)
        .collect();
    if keys.is_empty() {
        return Err(ApiError::new(ErrorCode::NoPasskeys));
    }
    let (challenge, ceremony) = webauthn
        .start_passkey_authentication(&keys)
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let id = passkeys::store_challenge(
        &state.pool,
        owner.owner_id,
        Kind::Reauth,
        Some(session.id),
        None,
        state_json(&ceremony)?,
    )
    .await?;
    Ok(Json(PasskeyChallengeView {
        challenge_id: id,
        options: options(&challenge)?,
    }))
}

/// Завершение повторного входа: сеанс свеж 10 минут. Неудача — 403 `passkey_rejected`
/// и неудача перебора (сеанс при этом жив).
#[utoipa::path(post, path = "/api/v1/auth/reauth/passkey/finish", tag = "auth", request_body = PasskeyReauthFinish, responses(
    (status = 200, description = "Сеанс свеж", body = FreshView),
    (status = 403, description = "passkey_rejected", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn reauth_finish(
    State(state): State<AppState>,
    session: CurrentSession,
    client: Client,
    ApiJson(body): ApiJson<PasskeyReauthFinish>,
) -> Result<Json<FreshView>, ApiError> {
    let webauthn = relying(&state)?;
    let (bucket, _) = bucket_for(&state, &client).await?;
    if let Reservation::Refused { retry_after } =
        throttle::reserve(&state.pool, &bucket, false).await?
    {
        return Err(too_many(retry_after));
    }
    if !verify(
        &state,
        webauthn,
        Kind::Reauth,
        Some(session.id),
        body.challenge_id,
        body.credential,
    )
    .await?
    {
        record_failure(&state, &bucket).await?;
        tracing::info!(kind = "reauth_failed", method = "passkey", bucket = %bucket, "passkey check failed");
        return Err(rejected());
    }
    throttle::succeeded(&state.pool, &bucket).await?;
    sessions::mark_fresh(&state.pool, session.id).await?;
    Ok(Json(FreshView {
        fresh_until: Utc::now() + chrono::TimeDelta::minutes(sessions::FRESH_MINUTES),
    }))
}
