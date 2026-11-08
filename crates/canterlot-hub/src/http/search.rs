//! Поиск владельца по истории сессий. Запрос идёт в теле POST, а не в адресе: адрес попадает
//! в журналы прокси, тело — нет.

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
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
use crate::search::{self, RawQuery, SearchError, SearchKind, SearchPage};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(search_history))
}

/// Поиск по ответам агента и сообщениям ленты; новые первыми.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchBody {
    /// Слова запроса, до 200 символов: кавычки ищут фразу, `-слово` исключает, `or` — любое.
    pub q: String,
    pub conversation_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub harness: Option<String>,
    pub kind: Option<SearchKind>,
    /// `next_cursor` предыдущей страницы.
    pub cursor: Option<String>,
    /// 1–50, по умолчанию 20.
    pub limit: Option<i64>,
}

/// Текст запроса — данные уровня сообщения.
impl std::fmt::Debug for SearchBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchBody")
            .field("q_chars", &self.q.chars().count())
            .field("conversation_id", &self.conversation_id)
            .field("kind", &self.kind)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

#[utoipa::path(post,path="/api/v1/search",tag="search",request_body=SearchBody,responses(
    (status=200,description="Совпадения, новые первыми",body=SearchPage),
    (status=400,description="Пустой или длинный запрос, неверный курсор, размер страницы или период",body=super::error::ErrorEnvelope),
    (status=503,description="Поиск не уложился в срок",body=super::error::ErrorEnvelope)
))]
async fn search_history(
    State(state): State<AppState>,
    owner: Owner,
    ApiJson(body): ApiJson<SearchBody>,
) -> Result<Json<SearchPage>, ApiError> {
    let query = RawQuery {
        text: body.q,
        conversation_id: body.conversation_id,
        since: body.since,
        until: body.until,
        harness: body.harness,
        kind: body.kind,
        cursor: body.cursor,
        limit: body.limit,
    }
    .check(owner.owner_id, None)
    .map_err(api_error)?;
    let page = search::search(&state.pool, &query)
        .await
        .map_err(api_error)?;
    Ok(Json(page))
}

fn api_error(error: SearchError) -> ApiError {
    match error {
        SearchError::Invalid => ApiError::new(ErrorCode::InvalidRequest),
        SearchError::Timeout => ApiError::new(ErrorCode::DbUnavailable),
        SearchError::Db(error) => error.into(),
    }
}
