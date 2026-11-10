use super::{
    AppState,
    auth::{Correlation, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiPath, ApiQuery},
};
use crate::bookmarks::{self, BookmarkError, BookmarkPage, BookmarkView, TargetKind};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_conversation))
        .routes(routes!(list_owner))
        .routes(routes!(add, remove))
}

fn map(error: BookmarkError) -> ApiError {
    match error {
        BookmarkError::NotFound => ApiError::new(ErrorCode::NotFound),
        BookmarkError::InvalidKind => {
            let mut error = ApiError::new(ErrorCode::InvalidRequest);
            error.status = StatusCode::UNPROCESSABLE_ENTITY;
            error
        }
        BookmarkError::Invalid => ApiError::new(ErrorCode::InvalidRequest),
        BookmarkError::Db(error) => error.into(),
    }
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct BookmarkQuery {
    /// 1–100, по умолчанию 50.
    pub limit: Option<i64>,
    /// `next_cursor` предыдущей страницы.
    pub cursor: Option<String>,
}

#[utoipa::path(get,path="/api/v1/conversations/{id}/bookmarks",tag="bookmarks",params(("id"=Uuid,Path,description="Разговор")),responses((status=200,description="Все закладки разговора в порядке ленты, до 500",body=Vec<BookmarkView>),(status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)))]
async fn list_conversation(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Vec<BookmarkView>>, ApiError> {
    Ok(Json(
        bookmarks::list_conversation(&state.pool, owner.owner_id, id)
            .await
            .map_err(map)?,
    ))
}

#[utoipa::path(get,path="/api/v1/bookmarks",tag="bookmarks",params(BookmarkQuery),responses((status=200,description="Избранное владельца, новые закладки первыми",body=BookmarkPage),(status=400,description="Неверный курсор или размер страницы",body=super::error::ErrorEnvelope)))]
async fn list_owner(
    State(state): State<AppState>,
    owner: Owner,
    ApiQuery(query): ApiQuery<BookmarkQuery>,
) -> Result<Json<BookmarkPage>, ApiError> {
    Ok(Json(
        bookmarks::list_owner(&state.pool, owner.owner_id, query.limit, query.cursor)
            .await
            .map_err(map)?,
    ))
}

#[utoipa::path(put,path="/api/v1/conversations/{id}/bookmarks/{kind}/{target_id}",tag="bookmarks",params(("id"=Uuid,Path,description="Разговор"),("kind"=String,Path,description="input или block"),("target_id"=Uuid,Path,description="Ввод или верхний блок ленты")),responses((status=201,description="Закладка создана",body=BookmarkView),(status=200,description="Такая закладка уже есть",body=BookmarkView),(status=404,description="Разговор или цель не найдены",body=super::error::ErrorEnvelope),(status=422,description="Неизвестный вид цели",body=super::error::ErrorEnvelope)))]
async fn add(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, kind, target_id)): ApiPath<(Uuid, String, Uuid)>,
    Correlation(correlation): Correlation,
) -> Result<(StatusCode, Json<BookmarkView>), ApiError> {
    let kind = TargetKind::parse(&kind).ok_or_else(|| map(BookmarkError::InvalidKind))?;
    let (bookmark, created) = bookmarks::add(
        &state.pool,
        owner.owner_id,
        id,
        kind,
        target_id,
        correlation,
    )
    .await
    .map_err(map)?;
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(bookmark),
    ))
}

#[utoipa::path(delete,path="/api/v1/conversations/{id}/bookmarks/{kind}/{target_id}",tag="bookmarks",params(("id"=Uuid,Path,description="Разговор"),("kind"=String,Path,description="input или block"),("target_id"=Uuid,Path,description="Ввод или верхний блок ленты")),responses((status=204,description="Закладка удалена; повтор безвреден"),(status=404,description="Разговор не найден",body=super::error::ErrorEnvelope),(status=422,description="Неизвестный вид цели",body=super::error::ErrorEnvelope)))]
async fn remove(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath((id, kind, target_id)): ApiPath<(Uuid, String, Uuid)>,
    Correlation(correlation): Correlation,
) -> Result<StatusCode, ApiError> {
    let kind = TargetKind::parse(&kind).ok_or_else(|| map(BookmarkError::InvalidKind))?;
    bookmarks::remove(
        &state.pool,
        owner.owner_id,
        id,
        kind,
        target_id,
        correlation,
    )
    .await
    .map_err(map)?;
    Ok(StatusCode::NO_CONTENT)
}
