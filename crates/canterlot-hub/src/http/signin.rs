//! `/api/v1/auth`: вход владельца, сеансы, настройки и пароль (ADR-0005).
//!
//! Публичные пути (`auth::PUBLIC_AUTH`) проходят транспортный шлюз и CSRF, но не сеанс.
//! Ключа идемпотентности здесь нет: действия идемпотентны сами (выход, отзыв, имя,
//! срок бездействия), а запросы с паролем нельзя ни хранить, ни повторять из записи.

use axum::{
    Json,
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Client, CurrentSession, Owner, clear_session_cookie, device_cookie, session_cookie},
    error::{ApiError, ErrorCode, ErrorDetails, ErrorEnvelope},
    extract::{ApiJson, ApiPath},
};
use crate::{
    notify,
    owner_auth::{
        self, Method, RevokeReason, bucket_key,
        password::{self},
        recovery,
        sessions::{self, ClientInfo, SessionRow},
        throttle::{self, Reservation},
    },
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(methods))
        .routes(routes!(sign_in_password))
        .routes(routes!(sign_in_recovery))
        .routes(routes!(list_sessions))
        .routes(routes!(rename_session, delete_session))
        .routes(routes!(revoke_others))
        .routes(routes!(logout))
        .routes(routes!(update_settings))
        .routes(routes!(set_password))
        .routes(routes!(reauth_password))
}

#[derive(Serialize, ToSchema)]
pub struct MethodsView {
    pub password: bool,
    pub passkey: bool,
}

/// Какие способы входа сейчас доступны: кнопка ключа доступа прячется без ключей.
#[utoipa::path(get, path = "/api/v1/auth/methods", tag = "auth", responses(
    (status = 200, description = "Способы входа", body = MethodsView),
))]
async fn methods(State(state): State<AppState>) -> Result<Json<MethodsView>, ApiError> {
    Ok(Json(MethodsView {
        password: password::exists(&state.pool, state.installation.owner_id).await?,
        passkey: state.webauthn.is_some()
            && owner_auth::passkeys::exists(&state.pool, state.installation.owner_id).await?,
    }))
}

/// Место по IP из локальной базы DB-IP: русские названия, где они есть, иначе английские.
/// `local`: частная, петлевая, link-local или CGNAT сеть («локальная сеть»), остальные поля пусты.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default)]
pub struct SessionLocationView {
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    pub country_code: Option<String>,
    #[serde(default)]
    pub local: bool,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerSessionView {
    pub id: Uuid,
    /// Имя от владельца.
    pub name: Option<String>,
    /// Подпись устройства по user agent и подсказкам клиента.
    pub label: Option<String>,
    /// Сырой user agent для подробностей; заявлен клиентом.
    pub user_agent: Option<String>,
    pub method: Method,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// `null` — адрес неизвестен («IP неизвестен»).
    pub created_ip: Option<String>,
    pub last_ip: Option<String>,
    pub location: Option<SessionLocationView>,
    /// Сеанс, подтвердивший вход по QR.
    pub approved_by: Option<Uuid>,
    /// Живые сеансы, вход которых подтвердил этот.
    pub approved: Vec<Uuid>,
    pub current: bool,
}

impl OwnerSessionView {
    pub(super) fn from_row(row: SessionRow, current: Uuid) -> Self {
        // Место последнего адреса: при входе оно же место создания.
        let location = row
            .last_location
            .and_then(|value| serde_json::from_value(value).ok());
        Self {
            label: owner_auth::label(row.user_agent.as_deref(), row.client_hints.as_ref()),
            current: row.id == current,
            id: row.id,
            name: row.name,
            user_agent: row.user_agent,
            method: Method::parse(&row.method).unwrap_or(Method::Password),
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            created_ip: row.created_ip,
            last_ip: row.last_ip,
            location,
            approved_by: row.approved_by,
            approved: row.approved,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct SignInView {
    pub session: OwnerSessionView,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasswordSignIn {
    pub login: String,
    pub password: String,
    /// Подсказки клиента для подписи устройства: только для показа, не больше 2 КиБ.
    #[serde(default)]
    pub device: Option<Value>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoverySignIn {
    pub token: String,
    #[serde(default)]
    pub device: Option<Value>,
}

pub(super) fn too_many(retry_after: std::time::Duration) -> ApiError {
    let seconds = retry_after.as_secs().max(1);
    ApiError::new(ErrorCode::TooManyAttempts).with_details(ErrorDetails {
        retry_after_seconds: Some(seconds),
        ..Default::default()
    })
}

pub(super) fn refused(retry_after: std::time::Duration) -> Response {
    let seconds = retry_after.as_secs().max(1);
    let mut response = too_many(retry_after).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

/// Корзина попытки: у известного устройства своя, иначе по адресу клиента.
pub(super) async fn bucket_for(
    state: &AppState,
    client: &Client,
) -> Result<(String, bool), ApiError> {
    if let Some(token) = &client.device_token
        && sessions::device_known(&state.pool, state.installation.owner_id, &token.0).await?
    {
        return Ok((sessions::device_bucket(&token.0), true));
    }
    Ok((bucket_key(client.ip), false))
}

/// Неудача: журнал, общая блокировка и пуши. Ошибка пуша не меняет ответ.
pub(super) async fn record_failure(state: &AppState, bucket: &str) -> Result<(), ApiError> {
    let failure = throttle::failed(&state.pool, bucket).await?;
    if failure.global_locked
        && let Err(error) = notify::on_sign_in_lock(&state.pool).await
    {
        tracing::warn!(kind = "sign_in_push_failed", error = %crate::dberr::describe(&error), "sign-in push not queued");
    }
    if let Err(error) = notify::on_sign_in_failure(&state.pool).await {
        tracing::warn!(kind = "sign_in_push_failed", error = %crate::dberr::describe(&error), "sign-in push not queued");
    }
    Ok(())
}

/// Новый сеанс по доказанному паролю, ключу или ссылке: сеанс и известное устройство
/// одной транзакцией, затем cookie, пуш, вид сеанса.
pub(super) async fn signed_in(
    state: &AppState,
    client: &Client,
    method: Method,
    device: Option<Value>,
) -> Result<Response, ApiError> {
    let info = ClientInfo::located(client.ip, client.user_agent.clone(), device, &state.geo);
    let (created, device) = sessions::sign_in(
        &state.pool,
        state.installation.owner_id,
        method,
        &info,
        client.device_token.as_ref().map(|token| token.0.as_str()),
    )
    .await?;
    let (session, mut cookies) = issue(state, &created, &info, method).await?;
    cookies.push(device_cookie(&device));
    Ok(with_cookies(
        Json(SignInView { session }).into_response(),
        cookies,
    ))
}

/// Сеанс создан и записан: уходит пуш о входе. Возвращает вид сеанса и `Set-Cookie`
/// сеанса. Ошибка после записи сеанса отзывает его (`server`): ответ без cookie не
/// оставляет живого сеанса, которым никто не владеет.
pub(super) async fn issue(
    state: &AppState,
    created: &sessions::Created,
    info: &ClientInfo,
    method: Method,
) -> Result<(OwnerSessionView, Vec<HeaderValue>), ApiError> {
    match describe(state, created, info, method).await {
        Ok(session) => Ok((
            session,
            vec![session_cookie(&created.token, created.max_age_secs)],
        )),
        Err(error) => {
            if let Err(revoke) =
                sessions::revoke(&state.pool, created.id, RevokeReason::Server, None).await
            {
                tracing::warn!(kind = "sign_in_orphan", session_id = %created.id, error = %crate::dberr::describe(&revoke), "new session not revoked after a failure");
            }
            Err(error)
        }
    }
}

async fn describe(
    state: &AppState,
    created: &sessions::Created,
    info: &ClientInfo,
    method: Method,
) -> Result<OwnerSessionView, ApiError> {
    #[cfg(feature = "fault-injection")]
    if state
        .auth_faults
        .take_drop(crate::fault_injection::Point::SignInAfterSession, "")
    {
        return Err(ApiError::new(ErrorCode::DbUnavailable));
    }
    let owner_id = state.installation.owner_id;
    let row = sessions::get(&state.pool, owner_id, created.id)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::Internal))?;
    let text = owner_auth::sign_in_text(
        owner_auth::label(row.user_agent.as_deref(), row.client_hints.as_ref()).as_deref(),
        info.location
            .as_ref()
            .map(|location| location.place())
            .as_deref(),
        info.ip,
        method,
    );
    if let Err(error) = notify::on_sign_in(&state.pool, created.id, &text).await {
        tracing::warn!(kind = "sign_in_push_failed", error = %crate::dberr::describe(&error), "sign-in push not queued");
    }
    tracing::info!(kind = "signed_in", method = method.as_str(), session_id = %created.id, "owner signed in");
    Ok(OwnerSessionView::from_row(row, created.id))
}

pub(super) fn with_cookies(mut response: Response, cookies: Vec<HeaderValue>) -> Response {
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// Вход по логину и паролю. Неверный логин, неверный пароль и отсутствие пароля дают
/// один и тот же 401 и одну проверку argon2.
#[utoipa::path(post, path = "/api/v1/auth/password", tag = "auth", request_body = PasswordSignIn, responses(
    (status = 200, description = "Вход выполнен; Set-Cookie сеанса и устройства", body = SignInView),
    (status = 401, description = "invalid_credentials", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts, details.retry_after_seconds", body = ErrorEnvelope),
))]
async fn sign_in_password(
    State(state): State<AppState>,
    client: Client,
    ApiJson(body): ApiJson<PasswordSignIn>,
) -> Result<Response, ApiError> {
    let Some(_permit) = state.passwords.permit().await else {
        return Ok(refused(std::time::Duration::from_secs(5)));
    };
    let (bucket, device) = bucket_for(&state, &client).await?;
    if let Reservation::Refused { retry_after } =
        throttle::reserve(&state.pool, &bucket, !device).await?
    {
        return Ok(refused(retry_after));
    }
    let stored = password::load(&state.pool, state.installation.owner_id).await?;
    let login_matches: bool = body
        .login
        .as_bytes()
        .ct_eq(state.http.owner_login.as_bytes())
        .into();
    let verified = state.passwords.verify(body.password, stored).await;
    drop(_permit);
    if !(verified && login_matches) {
        record_failure(&state, &bucket).await?;
        tracing::info!(kind = "sign_in_failed", method = "password", bucket = %bucket, "password sign-in failed");
        return Err(ApiError::new(ErrorCode::InvalidCredentials));
    }
    throttle::succeeded(&state.pool, &bucket).await?;
    signed_in(&state, &client, Method::Password, body.device).await
}

/// Вход по ссылке восстановления: одноразовый токен из `canterlot-hub auth recovery-link`.
#[utoipa::path(post, path = "/api/v1/auth/recovery", tag = "auth", request_body = RecoverySignIn, responses(
    (status = 200, description = "Вход выполнен; Set-Cookie сеанса и устройства", body = SignInView),
    (status = 401, description = "invalid_credentials: токен неизвестен, использован или истёк", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn sign_in_recovery(
    State(state): State<AppState>,
    client: Client,
    ApiJson(body): ApiJson<RecoverySignIn>,
) -> Result<Response, ApiError> {
    let (bucket, _) = bucket_for(&state, &client).await?;
    if let Reservation::Refused { retry_after } =
        throttle::reserve(&state.pool, &bucket, false).await?
    {
        return Ok(refused(retry_after));
    }
    let owner = recovery::redeem(&state.pool, &body.token).await?;
    if owner != Some(state.installation.owner_id) {
        record_failure(&state, &bucket).await?;
        tracing::info!(kind = "sign_in_failed", method = "recovery", bucket = %bucket, "recovery sign-in failed");
        return Err(ApiError::new(ErrorCode::InvalidCredentials));
    }
    throttle::succeeded(&state.pool, &bucket).await?;
    signed_in(&state, &client, Method::Recovery, body.device).await
}

#[derive(Serialize, ToSchema)]
pub struct OwnerSessionsView {
    pub current_id: Uuid,
    pub idle_timeout_days: i32,
    /// До какого момента текущий сеанс свеж (действия управления без повторного входа).
    pub fresh_until: Option<DateTime<Utc>>,
    /// Живые сеансы: текущий первым, затем по последней активности.
    pub sessions: Vec<OwnerSessionView>,
}

#[utoipa::path(get, path = "/api/v1/auth/sessions", tag = "auth", responses(
    (status = 200, description = "Живые сеансы", body = OwnerSessionsView),
))]
async fn list_sessions(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
) -> Result<Json<OwnerSessionsView>, ApiError> {
    let rows = sessions::list(&state.pool, owner.owner_id, Some(session.id)).await?;
    Ok(Json(OwnerSessionsView {
        current_id: session.id,
        idle_timeout_days: sessions::idle_days(&state.pool, owner.owner_id).await?,
        fresh_until: session.fresh_until.filter(|_| session.fresh),
        sessions: rows
            .into_iter()
            .map(|row| OwnerSessionView::from_row(row, session.id))
            .collect(),
    }))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSession {
    /// До 64 символов; пустая строка снимает имя.
    pub name: String,
}

#[utoipa::path(patch, path = "/api/v1/auth/sessions/{id}", tag = "auth", request_body = RenameSession, responses(
    (status = 200, description = "Имя сохранено", body = OwnerSessionView),
    (status = 404, description = "Живого сеанса нет", body = ErrorEnvelope),
))]
async fn rename_session(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<RenameSession>,
) -> Result<Json<OwnerSessionView>, ApiError> {
    let name = body.name.trim();
    if name.chars().count() > sessions::NAME_CHARS || name.chars().any(char::is_control) {
        return Err(
            ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
                field: Some("name".into()),
                ..Default::default()
            }),
        );
    }
    let name = (!name.is_empty()).then_some(name);
    if !sessions::rename(&state.pool, owner.owner_id, id, name).await? {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    let row = sessions::get(&state.pool, owner.owner_id, id)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    Ok(Json(OwnerSessionView::from_row(row, owner.session_id)))
}

#[derive(Serialize, ToSchema)]
pub struct RevokedView {
    /// Отозванные сеансы, включая каскад.
    pub revoked: Vec<Uuid>,
}

fn logged_out(revoked: Vec<Uuid>) -> Response {
    let mut response = Json(RevokedView { revoked }).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_session_cookie());
    response
}

/// Завершает сеанс. Свой сеанс — это выход; чужой требует свежего сеанса и уносит
/// каскадом сеансы, вход которых он подтвердил.
#[utoipa::path(delete, path = "/api/v1/auth/sessions/{id}", tag = "auth", responses(
    (status = 200, description = "Сеанс завершён", body = RevokedView),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 404, description = "Живого сеанса нет", body = ErrorEnvelope),
))]
async fn delete_session(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Response, ApiError> {
    if id == session.id {
        let revoked = sessions::revoke(&state.pool, id, RevokeReason::Logout, None).await?;
        return Ok(logged_out(revoked));
    }
    if !sessions::live_exists(&state.pool, owner.owner_id, id).await? {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    session.require_fresh()?;
    let revoked = sessions::revoke(&state.pool, id, RevokeReason::Owner, Some(session.id)).await?;
    tracing::info!(kind = "session_revoked", session_id = %id, cascade = revoked.len().saturating_sub(1), "owner revoked a session");
    Ok(Json(RevokedView { revoked }).into_response())
}

/// «Завершить все остальные сеансы». Требует свежего сеанса.
#[utoipa::path(post, path = "/api/v1/auth/sessions/revoke-others", tag = "auth", responses(
    (status = 200, description = "Остальные сеансы завершены", body = RevokedView),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
))]
async fn revoke_others(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
) -> Result<Json<RevokedView>, ApiError> {
    session.require_fresh()?;
    let revoked = sessions::revoke_all_except(
        &state.pool,
        owner.owner_id,
        Some(session.id),
        RevokeReason::Others,
    )
    .await?;
    tracing::info!(
        kind = "sessions_revoked",
        count = revoked.len(),
        "owner revoked other sessions"
    );
    Ok(Json(RevokedView { revoked }))
}

/// Выход: сеанс отзывается, cookie стирается. Cookie устройства остаётся.
#[utoipa::path(post, path = "/api/v1/auth/logout", tag = "auth", responses(
    (status = 200, description = "Сеанс завершён", body = RevokedView),
))]
async fn logout(
    State(state): State<AppState>,
    session: CurrentSession,
) -> Result<Response, ApiError> {
    let revoked = sessions::revoke(&state.pool, session.id, RevokeReason::Logout, None).await?;
    Ok(logged_out(revoked))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthSettingsBody {
    /// 7, 30, 90, 180 или 365.
    pub idle_timeout_days: i32,
}

#[derive(Serialize, ToSchema)]
pub struct AuthSettingsView {
    pub idle_timeout_days: i32,
}

/// Срок бездействия для всех сеансов, включая текущий; cookie текущего получает новый срок.
#[utoipa::path(patch, path = "/api/v1/auth/settings", tag = "auth", request_body = AuthSettingsBody, responses(
    (status = 200, description = "Настройки сохранены", body = AuthSettingsView),
))]
async fn update_settings(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(body): ApiJson<AuthSettingsBody>,
) -> Result<Response, ApiError> {
    if !sessions::IDLE_CHOICES.contains(&body.idle_timeout_days) {
        return Err(
            ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
                field: Some("idle_timeout_days".into()),
                ..Default::default()
            }),
        );
    }
    sessions::set_idle_days(&state.pool, owner.owner_id, body.idle_timeout_days).await?;
    let mut response = Json(AuthSettingsView {
        idle_timeout_days: body.idle_timeout_days,
    })
    .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(&session.token.0, i64::from(body.idle_timeout_days) * 86400),
    );
    Ok(response)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPasswordBody {
    /// Обязателен, если пароль уже задан.
    #[serde(default)]
    pub current_password: Option<String>,
    /// 10..1024 байт.
    pub new_password: String,
}

fn invalid_field(field: &'static str) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
        field: Some(field.into()),
        ..Default::default()
    })
}

/// Проверка пароля владельцем, уже вошедшим: та же корзина перебора, без общей блокировки.
/// Неверный пароль — 403 `wrong_password`, не 401: сеанс при этом жив.
async fn check_password(
    state: &AppState,
    client: &Client,
    given: String,
    stored: Option<String>,
) -> Result<(), ApiError> {
    let (bucket, _) = bucket_for(state, client).await?;
    if let Reservation::Refused { retry_after } =
        throttle::reserve(&state.pool, &bucket, false).await?
    {
        return Err(too_many(retry_after));
    }
    if !state.passwords.verify(given, stored).await {
        record_failure(state, &bucket).await?;
        tracing::info!(kind = "reauth_failed", bucket = %bucket, "password check failed");
        return Err(ApiError::new(ErrorCode::WrongPassword));
    }
    throttle::succeeded(&state.pool, &bucket).await?;
    Ok(())
}

/// Задать или сменить пароль. Смена несёт `current_password`; первый пароль требует
/// свежего сеанса. Успех завершает все остальные сеансы и делает текущий свежим.
#[utoipa::path(put, path = "/api/v1/auth/password", tag = "auth", request_body = SetPasswordBody, responses(
    (status = 200, description = "Пароль сохранён; отозванные сеансы", body = RevokedView),
    (status = 403, description = "wrong_password или reauth_required", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn set_password(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    client: Client,
    ApiJson(body): ApiJson<SetPasswordBody>,
) -> Result<Json<RevokedView>, ApiError> {
    if !password::acceptable(&body.new_password) {
        return Err(invalid_field("new_password"));
    }
    let stored = password::load(&state.pool, owner.owner_id).await?;
    if stored.is_none() {
        session.require_fresh()?;
    }
    let Some(_permit) = state.passwords.permit().await else {
        return Err(too_many(std::time::Duration::from_secs(5)));
    };
    if stored.is_some() {
        let current = body
            .current_password
            .ok_or_else(|| invalid_field("current_password"))?;
        check_password(&state, &client, current, stored).await?;
    }
    let phc = state
        .passwords
        .hash(body.new_password)
        .await
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    drop(_permit);
    let mut conn = state.pool.acquire().await?;
    password::store(&mut conn, owner.owner_id, &phc).await?;
    drop(conn);
    let revoked = sessions::revoke_all_except(
        &state.pool,
        owner.owner_id,
        Some(session.id),
        RevokeReason::PasswordChanged,
    )
    .await?;
    sessions::forget_all_devices(&state.pool, owner.owner_id).await?;
    sessions::mark_fresh(&state.pool, session.id).await?;
    tracing::info!(
        kind = "password_changed",
        revoked = revoked.len(),
        "owner password changed"
    );
    Ok(Json(RevokedView { revoked }))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReauthPassword {
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct FreshView {
    pub fresh_until: DateTime<Utc>,
}

/// Повторный вход паролем: сеанс становится свежим на 10 минут. То же ключом доступа —
/// `/reauth/passkey/*` (`http::signin_passkeys`).
#[utoipa::path(post, path = "/api/v1/auth/reauth/password", tag = "auth", request_body = ReauthPassword, responses(
    (status = 200, description = "Сеанс свеж", body = FreshView),
    (status = 403, description = "wrong_password", body = ErrorEnvelope),
    (status = 429, description = "too_many_attempts", body = ErrorEnvelope),
))]
async fn reauth_password(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    client: Client,
    ApiJson(body): ApiJson<ReauthPassword>,
) -> Result<Json<FreshView>, ApiError> {
    let Some(_permit) = state.passwords.permit().await else {
        return Err(too_many(std::time::Duration::from_secs(5)));
    };
    let stored = password::load(&state.pool, owner.owner_id).await?;
    check_password(&state, &client, body.password, stored).await?;
    sessions::mark_fresh(&state.pool, session.id).await?;
    Ok(Json(FreshView {
        fresh_until: Utc::now() + chrono::TimeDelta::minutes(sessions::FRESH_MINUTES),
    }))
}
