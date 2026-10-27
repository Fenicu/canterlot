use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::conversations::{
    self, Attributes, ConversationDetail, Created, CrunchSetting, ListCursor, ListFilter, ListPage,
    ListQuery, NewConversation,
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get, update))
        .routes(routes!(set_groups))
        .routes(routes!(set_crunch))
        .routes(routes!(read))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub machine_id: Uuid,
    pub harness: String,
    pub workdir: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: Option<String>,
    pub instruction_profile: Option<String>,
    pub title: Option<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    #[serde(default)]
    pub crunch: CrunchSetting,
    #[serde(default)]
    pub start: bool,
}
#[derive(Default, Deserialize, IntoParams)]
pub struct ListParams {
    pub filter: Option<String>,
    pub q: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupsBody {
    pub group_ids: Vec<Uuid>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadBody {
    #[schema(pattern = "^[0-9]+$")]
    pub seq: String,
}
#[utoipa::path(post,path="/api/v1/conversations",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=CreateBody,responses((status=201,description="Сессия создана",body=Created)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<CreateBody>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let result = conversations::create(
        &s.pool,
        &key,
        NewConversation {
            owner_id: o.owner_id,
            machine_id: b.machine_id,
            harness: b.harness,
            workdir: b.workdir,
            model: b.model,
            effort: b.effort,
            permission_mode: b.permission_mode,
            instruction_profile: b.instruction_profile,
            title: b.title,
            color: b.color,
            group_ids: b.group_ids,
            crunch: b.crunch,
            start: b.start,
        },
        &s.limits,
        c,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}
#[utoipa::path(get,path="/api/v1/conversations",tag="conversations",params(ListParams),responses((status=200,description="Список сессий",body=ListPage)))]
async fn list(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(p): ApiQuery<ListParams>,
) -> Result<Json<ListPage>, ApiError> {
    let filter = match p.filter.as_deref().unwrap_or("all") {
        "all" => ListFilter::All,
        "pinned" => ListFilter::Pinned,
        "archived" => ListFilter::Archived,
        other => ListFilter::Group(
            other
                .strip_prefix("group:")
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest))?,
        ),
    };
    let cursor = p
        .cursor
        .as_deref()
        .map(|v| ListCursor::decode(v).ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest)))
        .transpose()?;
    let mut tx = s.pool.begin().await?;
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *tx)
        .await?;
    if let ListFilter::Group(id) = filter {
        let owned: bool = sqlx::query_scalar(
            "select exists(select 1 from conversation_groups where owner_id=$1 and id=$2)",
        )
        .bind(o.owner_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if !owned {
            return Err(ApiError::new(ErrorCode::NotFound));
        }
    }
    let result = conversations::list(
        &mut tx,
        &ListQuery {
            owner_id: o.owner_id,
            filter,
            q: p.q,
            cursor,
            limit: p.limit.unwrap_or(50),
        },
    )
    .await
    .map_err(query_error)?;
    tx.commit().await?;
    Ok(Json(result))
}
pub(super) fn query_error(e: sqlx::Error) -> ApiError {
    if matches!(e, sqlx::Error::InvalidArgument(_)) {
        ApiError::new(ErrorCode::InvalidRequest)
    } else {
        e.into()
    }
}
#[utoipa::path(get,path="/api/v1/conversations/{id}",tag="conversations",params(("id"=Uuid,Path)),responses((status=200,description="Сессия",body=ConversationDetail)))]
async fn get(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let mut tx = s.pool.begin().await?;
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *tx)
        .await?;
    let mut d = conversations::detail(&mut tx, o.owner_id, id)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    tx.commit().await?;
    if !s.registry.is_reconciled(d.attributes.machine.id) {
        d.holds.push(crate::model::HoldView::MachineNotReconciled);
    }
    if !s
        .registry
        .has_capability(d.attributes.machine.id, "pilot_v1")
        && !d
            .holds
            .iter()
            .any(|h| matches!(h, crate::model::HoldView::CapabilityMissing { .. }))
    {
        let input:Option<Uuid>=sqlx::query_scalar("select i.id from inputs i join commands cmd on cmd.id=i.command_id where i.conversation_id=$1 and i.state='queued' and (cmd.payload ? 'settings' or jsonb_array_length(coalesce(cmd.payload->'attachment_ids','[]'))>0) order by i.input_seq limit 1").bind(id).fetch_optional(&s.pool).await?;
        if let Some(input_id) = input {
            d.holds
                .push(crate::model::HoldView::CapabilityMissing { input_id });
        }
    }
    Ok(Json(d))
}
#[utoipa::path(patch,path="/api/v1/conversations/{id}",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=Attributes,responses((status=200,description="Атрибуты изменены",body=ConversationDetail)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<Attributes>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::update_attributes(&s.pool, o.owner_id, id, &key, b, c).await?,
    ))
}
#[utoipa::path(put,path="/api/v1/conversations/{id}/groups",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=GroupsBody,responses((status=200,description="Группы изменены",body=ConversationDetail)))]
async fn set_groups(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<GroupsBody>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::set_groups(&s.pool, o.owner_id, id, &key, b.group_ids, c).await?,
    ))
}
#[utoipa::path(put,path="/api/v1/conversations/{id}/crunch",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=CrunchSetting,responses((status=200,description="Кранч изменён",body=ConversationDetail)))]
async fn set_crunch(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<CrunchSetting>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::set_crunch(&s.pool, o.owner_id, id, &key, b, &s.limits, c).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/read",tag="conversations",params(("id"=Uuid,Path)),request_body=ReadBody,responses((status=204,description="Прочитанность сохранена")))]
async fn read(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<ReadBody>,
) -> Result<StatusCode, ApiError> {
    if b.seq.is_empty() || !b.seq.bytes().all(|c| c.is_ascii_digit()) {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let seq = b
        .seq
        .parse()
        .map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?;
    conversations::mark_read(&s.pool, o.owner_id, id, seq).await?;
    Ok(StatusCode::NO_CONTENT)
}
