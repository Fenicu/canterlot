use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::{ApiPath, ApiQuery},
};
use crate::{
    events::{self, ReadAfter, Scope, WireEvent},
    history::{
        self, BlockPage, DelegationNode, HistoryCursor, HistoryError, HistoryPage, LanePage,
        RunInfo,
    },
};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(page))
        .routes(routes!(block))
        .routes(routes!(run))
        .routes(routes!(delegation))
        .routes(routes!(lane))
        .routes(routes!(range))
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    pub before: Option<String>,
    pub after: Option<String>,
    pub limit: Option<i64>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct BlockParams {
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct LaneParams {
    pub before: Option<String>,
    pub limit: Option<i64>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct RangeParams {
    pub scope: String,
    pub after: String,
    pub limit: Option<i64>,
}
fn invalid() -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest)
}
pub(crate) fn decimal(value: &str) -> Result<i64, ApiError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    value.parse().map_err(|_| invalid())
}
fn history_key(value: &str) -> Result<i64, ApiError> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    value.parse().map_err(|_| invalid())
}
fn missing() -> ApiError {
    ApiError::new(ErrorCode::NotFound)
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/history",tag="history",params(("id"=Uuid,Path),HistoryParams),responses((status=200,description="Окно истории в порядке scope_seq",body=HistoryPage)))]
async fn page(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(p): ApiQuery<HistoryParams>,
) -> Result<Json<HistoryPage>, ApiError> {
    let limit = p.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) || p.before.is_some() && p.after.is_some() {
        return Err(invalid());
    }
    let cursor = match (p.before, p.after) {
        (Some(n), None) => HistoryCursor::Before(history_key(&n)?),
        (None, Some(n)) => HistoryCursor::After(history_key(&n)?),
        _ => HistoryCursor::Tail,
    };
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            history::page(&mut *conn, o.owner_id, id, cursor, limit)
                .await?
                .ok_or_else(missing)
        })
    })
    .await?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/blocks/{block_id}",tag="history",params(("id"=Uuid,Path),("block_id"=Uuid,Path),BlockParams),responses((status=200,description="Текст блока; смещения в байтах UTF-8",body=BlockPage)))]
async fn block(
    State(s): State<AppState>,
    o: Owner,
    ApiPath((id, b)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(p): ApiQuery<BlockParams>,
) -> Result<Json<BlockPage>, ApiError> {
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            history::block_content(
                &mut *conn,
                o.owner_id,
                id,
                b,
                p.offset.unwrap_or(0),
                p.limit.unwrap_or(262144),
            )
            .await
            .map_err(|e| match e {
                HistoryError::InvalidOffset => ApiError::new(ErrorCode::InvalidOffset),
                HistoryError::InvalidLimit => invalid(),
                HistoryError::Db(e) => ApiError::from(e),
            })?
            .ok_or_else(missing)
        })
    })
    .await?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/runs/{run_id}",tag="history",params(("id"=Uuid,Path),("run_id"=Uuid,Path)),responses((status=200,description="Сведения о ходе",body=RunInfo)))]
async fn run(
    State(s): State<AppState>,
    o: Owner,
    ApiPath((id, r)): ApiPath<(Uuid, Uuid)>,
) -> Result<Json<RunInfo>, ApiError> {
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            history::run_info(&mut *conn, o.owner_id, id, r)
                .await?
                .ok_or_else(missing)
        })
    })
    .await?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/delegation",tag="history",params(("id"=Uuid,Path)),responses((status=200,description="Вызовы субагентов",body=Vec<DelegationNode>)))]
async fn delegation(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Vec<DelegationNode>>, ApiError> {
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            history::delegation(&mut *conn, o.owner_id, id)
                .await?
                .ok_or_else(missing)
        })
    })
    .await?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/conversations/{id}/lanes/{block_id}",tag="history",params(("id"=Uuid,Path),("block_id"=Uuid,Path),LaneParams),responses((status=200,description="Страница ленты субагента: корень и потомки от нового к старому",body=LanePage)))]
async fn lane(
    State(s): State<AppState>,
    o: Owner,
    ApiPath((id, root)): ApiPath<(Uuid, Uuid)>,
    ApiQuery(p): ApiQuery<LaneParams>,
) -> Result<Json<LanePage>, ApiError> {
    let limit = p.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(invalid());
    }
    let before = p.before.as_deref().map(history_key).transpose()?;
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            history::lane(&mut *conn, o.owner_id, id, root, before, limit)
                .await?
                .ok_or_else(missing)
        })
    })
    .await?;
    Ok(Json(result))
}
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum RangeResponse {
    Events {
        events: Vec<WireEvent>,
        head: String,
    },
    Resync {
        resync_required: ResyncRequired,
    },
}
#[derive(Serialize, ToSchema)]
pub struct ResyncRequired {
    pub reason: ResyncReason,
    pub oldest_available_seq: String,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    CursorTooOld,
    CursorAhead,
}
#[utoipa::path(get,path="/api/v1/events",tag="events",params(RangeParams),responses((status=200,description="Диапазон событий либо требование нового снимка",body=RangeResponse)))]
async fn range(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(p): ApiQuery<RangeParams>,
) -> Result<Json<RangeResponse>, ApiError> {
    let scope = match p.scope.as_str() {
        "conversations" => Scope::Conversations {
            owner_id: o.owner_id,
        },
        "machines" => Scope::Machines {
            owner_id: o.owner_id,
        },
        wire => {
            let id = wire
                .strip_prefix("conversation:")
                .and_then(|id| id.parse::<Uuid>().ok())
                .ok_or_else(invalid)?;
            if !history::owned(&mut *s.pool.acquire().await?, o.owner_id, id).await? {
                return Err(missing());
            }
            Scope::Conversation { id }
        }
    };
    let after = decimal(&p.after)?;
    let limit = p.limit.unwrap_or(256);
    if !(1..=256).contains(&limit) {
        return Err(invalid());
    }
    let range = events::range(&s.pool, &scope, after, limit).await?;
    Ok(Json(match range.result {
        ReadAfter::Events(es) => RangeResponse::Events {
            events: events::project_for_client(es),
            head: range.head.to_string(),
        },
        ReadAfter::ResyncRequired {
            oldest_available_seq,
        } => RangeResponse::Resync {
            resync_required: ResyncRequired {
                reason: if range.cursor_ahead {
                    ResyncReason::CursorAhead
                } else {
                    ResyncReason::CursorTooOld
                },
                oldest_available_seq: oldest_available_seq.to_string(),
            },
        },
    }))
}
