use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{Extensions, HeaderMap, Method, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ipnet::IpNet;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::{
    AppState,
    error::{ApiError, ErrorCode},
};

pub const PROXY_SECRET_HEADER: &str = "x-canterlot-proxy";
pub const CLIENT_HEADER: &str = "x-canterlot-client";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Пути без личности: только проверки живости и готовности.
const PUBLIC_PATHS: [&str; 2] = ["/healthz", "/readyz"];

#[derive(Clone, Debug)]
pub struct Owner {
    pub owner_id: Uuid,
    pub login: String,
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

/// Единственное место, где запрос получает личность.
///
/// 1. Адрес собеседника берётся из сокета (`ConnectInfo`); его нет, значит отказ.
/// 2. Собеседник должен входить в `trusted_proxies`.
/// 3. При настроенном секрете нужен ровно один `X-Canterlot-Proxy`, равный секрету.
/// 4. Заголовок личности присутствует ровно один раз и равен `owner_login`.
///
/// Всё, кроме чужого логина, даёт одинаковый `unauthenticated`: подделка заголовка
/// недоверенным адресом неотличима от его отсутствия. `X-Forwarded-*` не читается.
pub(crate) fn authenticate(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Owner, ApiError> {
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
    let mut values = headers.get_all(&state.user_header).iter();
    let (Some(login), None) = (values.next(), values.next()) else {
        return Err(refuse(
            ErrorCode::Unauthenticated,
            "user_header_count",
            Some(peer),
            false,
        ));
    };
    if login.is_empty() {
        return Err(refuse(
            ErrorCode::Unauthenticated,
            "user_header_empty",
            Some(peer),
            false,
        ));
    }
    if login.as_bytes() != state.http.owner_login.as_bytes() {
        return Err(refuse(
            ErrorCode::ForbiddenUser,
            "other_user",
            Some(peer),
            false,
        ));
    }
    Ok(Owner {
        owner_id: state.installation.owner_id,
        login: state.http.owner_login.clone(),
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

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if let Some(owner) = parts.extensions.get::<Owner>() {
            return Ok(owner.clone());
        }
        authenticate(&parts.extensions, &parts.headers, state)
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

fn is_public(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD) && PUBLIC_PATHS.contains(&path)
}

/// Запрет по умолчанию: личность нужна на любом пути, кроме перечисленных публичных,
/// даже если обработчик забыл про экстрактор `Owner`.
pub(crate) async fn identity_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if is_public(request.method(), request.uri().path()) {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    match authenticate(&parts.extensions, &parts.headers, &state) {
        Ok(owner) => {
            parts.extensions.insert(owner);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(error) => error.into_response(),
    }
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
/// Проверка идёт до личности и до любого обработчика.
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
