use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicI64, Ordering},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{Extensions, HeaderMap, HeaderValue, Method, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::{
    AppState,
    error::{ApiError, ErrorCode, ErrorDetails},
};
use crate::owner_auth::{
    DEVICE_COOKIE, DEVICE_COOKIE_MAX_AGE_SECS, RevokeReason, SESSION_COOKIE,
    sessions::{self, Lookup},
};

pub const PROXY_SECRET_HEADER: &str = "x-canterlot-proxy";
pub const CLIENT_HEADER: &str = "x-canterlot-client";
pub const CLIENT_IP_HEADER: &str = "x-canterlot-client-ip";
pub const PROXY_LEAK_HEADER: &str = "x-canterlot-proxy-leak";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";
const WARN_EVERY_SECS: i64 = 600;

/// Пути без транспортного шлюза и без сеанса: только проверки живости и готовности.
const HEALTH_PATHS: [&str; 2] = ["/healthz", "/readyz"];

/// Публичные пути входа: транспортный шлюз и CSRF действуют, сеанс не нужен.
/// Точный метод и путь; часть 2 добавляет сюда ключи доступа и QR.
pub const PUBLIC_AUTH: [(Method, &str); 3] = [
    (Method::GET, "/api/v1/auth/methods"),
    (Method::POST, "/api/v1/auth/password"),
    (Method::POST, "/api/v1/auth/recovery"),
];

#[derive(Clone, Debug)]
pub struct Owner {
    pub owner_id: Uuid,
    pub login: String,
    pub session_id: Uuid,
}

/// Сеанс запроса: свежесть для действий управления доступом.
#[derive(Clone, Debug)]
pub struct CurrentSession {
    pub id: Uuid,
    pub fresh: bool,
    pub fresh_until: Option<DateTime<Utc>>,
    /// Токен из cookie: нужен, чтобы выдать cookie с новым сроком.
    pub token: Token,
}

/// Токен из cookie в памяти запроса; в `Debug` и журналы не попадает.
#[derive(Clone)]
pub struct Token(pub String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token([redacted])")
    }
}

impl CurrentSession {
    /// Действие управления доступом требует пароля или ключа за последние 10 минут.
    pub fn require_fresh(&self) -> Result<(), ApiError> {
        if self.fresh {
            Ok(())
        } else {
            Err(ApiError::new(ErrorCode::ReauthRequired))
        }
    }
}

/// Клиент запроса после транспортного шлюза. Адрес известен только от прокси с секретом;
/// user agent и cookie устройства ничего не удостоверяют.
#[derive(Clone, Debug, Default)]
pub struct Client {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub device_token: Option<Token>,
}

pub struct IdempotencyKey(pub String);

#[derive(Clone, Copy, Debug)]
pub struct Correlation(pub Uuid);

/// Доверенный собеседник: адрес сокета входит в одну из сетей `trusted_proxies`.
/// `::ffff:a.b.c.d` сравнивается и как есть, и как `a.b.c.d`, чтобы двойной стек не обходил список.
pub fn peer_is_trusted(peer: IpAddr, trusted: &[IpNet]) -> bool {
    let mapped = match peer {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4),
        IpAddr::V4(_) => None,
    };
    trusted
        .iter()
        .any(|net| net.contains(&peer) || mapped.is_some_and(|ip| net.contains(&ip)))
}

fn secret_matches(headers: &HeaderMap, secret: &[u8]) -> bool {
    let mut values = headers.get_all(PROXY_SECRET_HEADER).iter();
    let (Some(given), None) = (values.next(), values.next()) else {
        return false;
    };
    // Сравнение дайджестов не зависит от длины и содержимого заголовка.
    let given = Sha256::digest(given.as_bytes());
    let expected = Sha256::digest(secret);
    given.as_slice().ct_eq(expected.as_slice()).into()
}

fn refuse(code: ErrorCode, reason: &'static str, peer: Option<IpAddr>, quiet: bool) -> ApiError {
    let peer = peer.map(|ip| ip.to_string());
    let peer = peer.as_deref().unwrap_or("-");
    if quiet {
        tracing::debug!(kind = "auth_rejected", reason, peer, "request refused");
    } else {
        tracing::warn!(kind = "auth_rejected", reason, peer, "request refused");
    }
    ApiError::new(code)
}

/// Предупреждение не чаще раза в 10 минут на процесс.
fn warn_rarely(last: &AtomicI64) -> bool {
    let now = Utc::now().timestamp();
    let previous = last.load(Ordering::Relaxed);
    now - previous >= WARN_EVERY_SECS
        && last
            .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

static CLIENT_IP_WARNED: AtomicI64 = AtomicI64::new(i64::MIN / 2);
static LEAK_WARNED: AtomicI64 = AtomicI64::new(i64::MIN / 2);

/// Транспортный шлюз: собеседник из `trusted_proxies`, при настроенном секрете ровно один
/// `X-Canterlot-Proxy`, равный секрету. `X-Forwarded-*` не читается.
pub(crate) fn transport(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), ApiError> {
    let Some(ConnectInfo(peer)) = extensions.get::<ConnectInfo<SocketAddr>>() else {
        return Err(refuse(
            ErrorCode::Unauthenticated,
            "no_peer_address",
            None,
            false,
        ));
    };
    let peer = peer.ip();
    if !peer_is_trusted(peer, &state.http.trusted_proxies) {
        return Err(refuse(
            ErrorCode::Unauthenticated,
            "untrusted_peer",
            Some(peer),
            true,
        ));
    }
    if let Some(secret) = &state.proxy_secret
        && !secret_matches(headers, secret)
    {
        return Err(refuse(
            ErrorCode::Unauthenticated,
            "proxy_secret",
            Some(peer),
            false,
        ));
    }
    Ok(())
}

/// Адрес клиента верен только от прокси, доказавшего секрет, и только из одного
/// разборчивого `X-Canterlot-Client-Ip`. Иначе адрес неизвестен.
fn client_ip(headers: &HeaderMap, state: &AppState) -> Option<IpAddr> {
    let ip = state.proxy_secret.as_ref().and_then(|_| {
        let mut values = headers.get_all(CLIENT_IP_HEADER).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => value.to_str().ok()?.trim().parse::<IpAddr>().ok(),
            _ => None,
        }
    });
    if ip.is_none() && warn_rarely(&CLIENT_IP_WARNED) {
        tracing::warn!(
            kind = "client_ip_missing",
            secret_configured = state.proxy_secret.is_some(),
            "client address unknown: brute-force buckets and the sessions list lose it"
        );
    }
    ip
}

/// Значения cookie с точным именем во всех заголовках `Cookie`.
pub(crate) fn cookie_values(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then(|| value.trim().to_owned())
        })
        .collect()
}

/// Ровно одно значение; два и больше значат подброс и не принимаются.
fn single_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = cookie_values(headers, name).into_iter();
    match (values.next(), values.next()) {
        (Some(value), None) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn cookie(name: &str, value: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}"
    ))
    .expect("cookie is a valid header value")
}

pub fn session_cookie(token: &str, max_age: i64) -> HeaderValue {
    cookie(SESSION_COOKIE, token, max_age)
}

pub fn clear_session_cookie() -> HeaderValue {
    cookie(SESSION_COOKIE, "", 0)
}

pub fn device_cookie(token: &str) -> HeaderValue {
    cookie(DEVICE_COOKIE, token, DEVICE_COOKIE_MAX_AGE_SECS)
}

fn sets_session_cookie(response: &Response) -> bool {
    let prefix = format!("{SESSION_COOKIE}=");
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .any(|value| value.as_bytes().starts_with(prefix.as_bytes()))
}

/// Отказ сеанса: 401 со стиранием cookie.
fn session_refusal(error: ApiError) -> Response {
    let mut response = error.into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, clear_session_cookie());
    response
}

/// Отказ отозванному сеансу: причина в `details.reason`, по ней PWA решает, стирать ли данные.
pub fn revoked_error(reason: RevokeReason) -> ApiError {
    ApiError::new(ErrorCode::SessionRevoked).with_details(ErrorDetails {
        reason: Some(reason.as_str().into()),
        ..Default::default()
    })
}

/// `^[A-Za-z0-9_-]{1,256}$`: без `:`, поэтому пространства `sched:` и `wake:` остаются внутренними.
pub(crate) fn valid_key(key: &str) -> bool {
    (1..=256).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl FromRequestParts<AppState> for Owner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Owner>()
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthenticated))
    }
}

impl FromRequestParts<AppState> for CurrentSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<CurrentSession>()
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Unauthenticated))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(parts
            .extensions
            .get::<Client>()
            .cloned()
            .unwrap_or_default())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        let mut values = parts.headers.get_all(IDEMPOTENCY_HEADER).iter();
        match (values.next(), values.next()) {
            (Some(value), None) => value
                .to_str()
                .ok()
                .filter(|key| valid_key(key))
                .map(|key| Self(key.to_owned()))
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest)),
            _ => Err(ApiError::new(ErrorCode::InvalidRequest)),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Correlation {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(parts
            .extensions
            .get::<Correlation>()
            .copied()
            .unwrap_or_else(|| Correlation(Uuid::now_v7())))
    }
}

fn is_health(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD) && HEALTH_PATHS.contains(&path)
}

pub fn is_public_auth(method: &Method, path: &str) -> bool {
    PUBLIC_AUTH
        .iter()
        .any(|(allowed, public)| allowed == method && *public == path)
}

/// Единственное место, где запрос получает личность. Запрет по умолчанию: сеанс нужен на
/// любом пути, кроме проверок живости и публичных путей входа, даже если обработчик забыл
/// про экстрактор `Owner`.
///
/// 1. Транспорт: доверенный собеседник и секрет прокси (`transport`).
/// 2. Клиент: адрес из `X-Canterlot-Client-Ip` только при секрете, user agent, cookie устройства.
/// 3. Личность: ровно одна cookie сеанса, поиск по хешу токена в базе без кэша.
///    Ошибка базы даёт 503 `db_unavailable`, а не 401: сбой не должен выглядеть как выход.
///
/// Заголовок личности старого прокси (`[http].user_header`) больше ничего не значит;
/// если он дошёл до хаба, ответ несёт `X-Canterlot-Proxy-Leak: 1`.
pub(crate) async fn identity_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if is_health(request.method(), request.uri().path()) {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    if let Err(error) = transport(&parts.extensions, &parts.headers, &state) {
        return error.into_response();
    }
    let leak = parts.headers.contains_key(&state.user_header);
    if leak && warn_rarely(&LEAK_WARNED) {
        tracing::warn!(
            kind = "identity_header_forwarded",
            header = state.user_header.as_str(),
            "the proxy forwards the old identity header; strip it in Caddy"
        );
    }
    let client = Client {
        ip: client_ip(&parts.headers, &state),
        user_agent: user_agent(&parts.headers),
        device_token: single_cookie(&parts.headers, DEVICE_COOKIE).map(Token),
    };
    let mut response = if is_public_auth(&parts.method, parts.uri.path()) {
        parts.extensions.insert(client);
        next.run(Request::from_parts(parts, body)).await
    } else {
        gate_session(&state, parts, body, client, next).await
    };
    if leak {
        response
            .headers_mut()
            .insert(PROXY_LEAK_HEADER, HeaderValue::from_static("1"));
    }
    response
}

async fn gate_session(
    state: &AppState,
    mut parts: Parts,
    body: axum::body::Body,
    client: Client,
    next: Next,
) -> Response {
    let Some(token) = single_cookie(&parts.headers, SESSION_COOKIE) else {
        let count = cookie_values(&parts.headers, SESSION_COOKIE).len();
        let error = refuse(
            ErrorCode::Unauthenticated,
            if count > 1 {
                "session_cookie_duplicated"
            } else {
                "no_session"
            },
            client.ip,
            count <= 1,
        );
        return if count > 1 {
            session_refusal(error)
        } else {
            error.into_response()
        };
    };
    let live = match sessions::authenticate(&state.pool, &token, client.ip).await {
        // Владелец один: личность берётся из записи установки, сеанс лишь доказывает её.
        Ok(Lookup::Live(live)) => live,
        Ok(Lookup::Unknown) => {
            return session_refusal(refuse(
                ErrorCode::Unauthenticated,
                "unknown_session",
                client.ip,
                true,
            ));
        }
        Ok(Lookup::Revoked(reason)) => return session_refusal(revoked_error(reason)),
        Ok(Lookup::Expired) => {
            return session_refusal(ApiError::new(ErrorCode::SessionExpired));
        }
        Err(error) => {
            tracing::warn!(
                kind = "session_lookup_failed",
                error = %crate::dberr::describe(&error),
                "session lookup failed"
            );
            return ApiError::new(ErrorCode::DbUnavailable).into_response();
        }
    };
    parts.extensions.insert(Owner {
        owner_id: state.installation.owner_id,
        login: state.http.owner_login.clone(),
        session_id: live.id,
    });
    parts.extensions.insert(CurrentSession {
        id: live.id,
        fresh: live.fresh,
        fresh_until: live.fresh_until,
        token: Token(token.clone()),
    });
    parts.extensions.insert(client);
    let mut response = next.run(Request::from_parts(parts, body)).await;
    if live.touched && !sets_session_cookie(&response) {
        response.headers_mut().append(
            header::SET_COOKIE,
            session_cookie(&token, live.max_age_secs),
        );
    }
    response
}

fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let (Some(origin), None) = (values.next(), values.next()) else {
        return false;
    };
    origin
        .to_str()
        .is_ok_and(|origin| allowed.iter().any(|entry| entry == origin))
}

fn client_header_present(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CLIENT_HEADER).iter();
    matches!((values.next(), values.next()), (Some(value), None) if value.as_bytes() == b"1")
}

/// CSRF: изменяющий запрос несёт разрешённый `Origin` и `X-Canterlot-Client: 1`;
/// WSS-апгрейд (любой запрос с `Upgrade`) несёт разрешённый `Origin`.
/// Проверка идёт до личности и до любого обработчика, публичные пути входа тоже под ней.
pub(crate) async fn csrf_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    let upgrade = request.headers().contains_key(header::UPGRADE);
    if safe && !upgrade {
        return next.run(request).await;
    }
    if !origin_allowed(request.headers(), &state.http.allowed_origins) {
        return refuse(ErrorCode::OriginRejected, "origin", None, false).into_response();
    }
    if !safe && !client_header_present(request.headers()) {
        return refuse(ErrorCode::OriginRejected, "client_header", None, false).into_response();
    }
    next.run(request).await
}

/// `OPTIONS` не поддерживается нигде: ни предпросмотра CORS, ни заголовков `Access-Control-*`.
pub(crate) async fn options_gate(request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS {
        return ApiError::new(ErrorCode::MethodNotAllowed).into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookies: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in cookies {
            map.append(header::COOKIE, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn only_one_exact_session_cookie_counts() {
        let name = SESSION_COOKIE;
        assert_eq!(
            single_cookie(&headers(&[&format!("a=1; {name}=tok; b=2")]), name),
            Some("tok".into())
        );
        assert_eq!(
            single_cookie(
                &headers(&[&format!("{name}=one"), &format!("{name}=two")]),
                name
            ),
            None
        );
        assert_eq!(
            single_cookie(&headers(&[&format!("{name}=one; {name}=two")]), name),
            None
        );
        assert_eq!(
            single_cookie(
                &headers(&["canterlot_session=tok; __host-canterlot_session=x"]),
                name
            ),
            None
        );
    }

    #[test]
    fn cookies_carry_the_host_prefix_attributes() {
        assert_eq!(
            session_cookie("tok", 7776000).to_str().unwrap(),
            "__Host-canterlot_session=tok; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=7776000"
        );
        assert_eq!(
            clear_session_cookie().to_str().unwrap(),
            "__Host-canterlot_session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
        );
        assert!(
            device_cookie("dev")
                .to_str()
                .unwrap()
                .ends_with("Max-Age=34560000")
        );
    }
}
