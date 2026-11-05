use super::{
    AppState,
    auth::{Correlation, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath},
};
use crate::drafts::{self, DraftError, DraftView, PutDraft};
use axum::{Json, extract::State};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get))
        .routes(routes!(put))
}

fn map(error: DraftError) -> ApiError {
    match error {
        DraftError::NotFound => ApiError::new(ErrorCode::NotFound),
        DraftError::Invalid => ApiError::new(ErrorCode::InvalidRequest),
        DraftError::TooLong => ApiError::new(ErrorCode::InvalidRequest),
        DraftError::Db(error) => ApiError::from(error),
    }
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/draft",tag="conversations",params(("id"=Uuid,Path,description="Разговор")),responses((status=200,description="Черновик поля ввода; пустой текст значит, что черновика нет",body=DraftView)))]
async fn get(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<DraftView>, ApiError> {
    Ok(Json(
        drafts::get(&state.pool, owner.owner_id, id)
            .await
            .map_err(map)?,
    ))
}

#[utoipa::path(put,path="/api/v1/conversations/{id}/draft",tag="conversations",params(("id"=Uuid,Path,description="Разговор")),request_body=PutDraft,responses((status=200,description="Черновик сохранён; пустой текст удаляет его",body=DraftView),(status=400,description="Текст длиннее 64 КиБ или вложение не готово/чужое",body=crate::http::error::ErrorEnvelope),(status=404,description="Разговор не найден",body=crate::http::error::ErrorEnvelope)))]
async fn put(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<PutDraft>,
) -> Result<Json<DraftView>, ApiError> {
    Ok(Json(
        drafts::put(&state.pool, owner.owner_id, id, body, correlation)
            .await
            .map_err(map)?,
    ))
}
