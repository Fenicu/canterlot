use axum::{
    extract::{FromRequest, FromRequestParts, Request, rejection::PathRejection},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use super::error::{ApiError, ErrorCode};

/// Тело JSON: отказ экстрактора не повторяет текст запроса, превышение лимита даёт 413.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Err(ApiError::new(ErrorCode::ResourceLimit))
            }
            Err(_) => Err(ApiError::new(ErrorCode::InvalidRequest)),
        }
    }
}

pub struct ApiPath<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Self(value)),
            Err(PathRejection::MissingPathParams(_)) => Err(ApiError::new(ErrorCode::Internal)),
            Err(_) => Err(ApiError::new(ErrorCode::InvalidRequest)),
        }
    }
}

pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(_) => Err(ApiError::new(ErrorCode::InvalidRequest)),
        }
    }
}
