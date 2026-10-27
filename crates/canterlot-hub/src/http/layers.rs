use std::{
    panic::AssertUnwindSafe,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{MatchedPath, Request},
    http::{HeaderMap, HeaderValue, Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures_util::FutureExt;
use uuid::Uuid;

use super::{
    auth::Correlation,
    error::{ApiError, ErrorCode, with_correlation},
};

const CORRELATION_HEADER: &str = "x-correlation-id";

fn correlation_from(headers: &HeaderMap) -> Uuid {
    let mut values = headers.get_all(CORRELATION_HEADER).iter();
    let accepted = match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .ok()
            .filter(|value| value.len() == 36)
            .and_then(|value| Uuid::parse_str(value).ok())
            .filter(|id| !id.is_nil()),
        _ => None,
    };
    accepted.unwrap_or_else(Uuid::now_v7)
}

fn logged_method(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        Method::TRACE => "TRACE",
        Method::CONNECT => "CONNECT",
        _ => "OTHER",
    }
}

/// Внешний слой: идентификатор корреляции и единственная строка журнала на запрос.
/// В строку попадают только метод, шаблон маршрута, статус и задержка.
pub(crate) async fn request_context(mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let correlation = correlation_from(request.headers());
    let method = logged_method(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    request.extensions_mut().insert(Correlation(correlation));
    let mut response = with_correlation(correlation, next.run(request)).await;
    if let Ok(value) = HeaderValue::from_str(&correlation.to_string()) {
        response.headers_mut().insert(CORRELATION_HEADER, value);
    }
    tracing::info!(
        kind = "http_request",
        method,
        route = route.as_deref().unwrap_or("unmatched"),
        status = response.status().as_u16(),
        latency_ms = started.elapsed().as_millis() as u64,
        correlation_id = %correlation,
        "request"
    );
    response
}

pub(crate) async fn response_headers(request: Request, next: Next) -> Response {
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    headers
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    if api {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        headers.insert("x-server-time-ms", HeaderValue::from(now as u64));
    }
    response
}

pub(crate) async fn panic_guard(request: Request, next: Next) -> Response {
    match AssertUnwindSafe(next.run(request)).catch_unwind().await {
        Ok(response) => response,
        Err(_) => {
            tracing::error!(kind = "http_panic", "handler panicked");
            ApiError::new(ErrorCode::Internal).into_response()
        }
    }
}

pub(crate) async fn not_found() -> ApiError {
    ApiError::new(ErrorCode::NotFound)
}

pub(crate) async fn method_not_allowed() -> ApiError {
    ApiError::new(ErrorCode::MethodNotAllowed)
}
