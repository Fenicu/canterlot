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
        .routes(routes!(decision))
        .routes(routes!(interrupt))
        .routes(routes!(stop))
        .routes(routes!(continue_queue))
        .routes(routes!(cancel_input))
        .routes(routes!(settings))
        .routes(routes!(submit))
        .routes(routes!(start_execution))
        .routes(routes!(resume))
}
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeBody {
    #[serde(default)]
    continue_queue: bool,
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/executions",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=201,description="Исполнение создано",body=crate::executions::StartedExecution)))]
async fn start_execution(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<(StatusCode, Json<crate::executions::StartedExecution>), ApiError> {
    let value = crate::executions::start_with_key(
        &s.pool,
        o.owner_id,
        id,
        &key,
        (false, false),
        c,
        #[cfg(feature = "fault-injection")]
        &s.scheduler.faults,
    )
    .await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(value)))
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/resume",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=ResumeBody,responses((status=201,description="Исполнение возобновлено",body=crate::executions::StartedExecution)))]
async fn resume(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<ResumeBody>,
) -> Result<(StatusCode, Json<crate::executions::StartedExecution>), ApiError> {
    let value = crate::executions::start_with_key(
        &s.pool,
        o.owner_id,
        id,
        &key,
        (true, body.continue_queue),
        c,
        #[cfg(feature = "fault-injection")]
        &s.scheduler.faults,
    )
    .await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(value)))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitBody {
    content: Vec<crate::delivery::ContentBlock>,
    #[serde(default)]
    attachment_ids: Vec<Uuid>,
    retry_of: Option<Uuid>,
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/inputs",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=SubmitBody,responses((status=201,description="Сообщение в очереди",body=crate::delivery::SubmittedInput)))]
async fn submit(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<SubmitBody>,
) -> Result<(StatusCode, Json<crate::delivery::SubmittedInput>), ApiError> {
    let result = conversations::submit_message(
        &s.pool,
        conversations::SubmitMessage {
            owner_id: o.owner_id,
            conversation_id: id,
            key,
            content: body.content,
            attachment_ids: body.attachment_ids,
            retry_of: body.retry_of,
            received_at: chrono::Utc::now(),
        },
        c,
    )
    .await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(result)))
}
#[utoipa::path(patch,path="/api/v1/conversations/{id}/settings",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=conversations::SettingsPatch,responses((status=200,description="Настройки изменены",body=conversations::SettingsView)))]
async fn settings(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(patch): ApiJson<conversations::SettingsPatch>,
) -> Result<Json<conversations::SettingsView>, ApiError> {
    Ok(Json(
        conversations::change_settings(&s.pool, o.owner_id, id, &key, patch, c).await?,
    ))
}
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StopBody {
    #[serde(default)]
    force: bool,
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/interrupt",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Остановка запрошена",body=crate::delivery::control::StopOutcome)))]
async fn interrupt(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::StopOutcome>, ApiError> {
    let result = crate::delivery::control::request_stop(&s.pool, o.owner_id, id, &key, c).await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/stop",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=StopBody,responses((status=200,description="Остановка исполнения запрошена",body=crate::delivery::control::StopExecutionOutcome)))]
async fn stop(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<StopBody>,
) -> Result<Json<crate::delivery::control::StopExecutionOutcome>, ApiError> {
    let result = crate::delivery::control::request_stop_execution(
        &s.pool, o.owner_id, id, body.force, &key, c,
    )
    .await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/queue/continue",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Очередь продолжена",body=crate::delivery::control::ContinueOutcome)))]
async fn continue_queue(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::ContinueOutcome>, ApiError> {
    let result = crate::delivery::control::continue_queue(&s.pool, o.owner_id, id, &key, c).await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/inputs/{id}/cancel",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Отмена запрошена",body=crate::delivery::control::CancelOutcome)))]
async fn cancel_input(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::CancelOutcome>, ApiError> {
    let conversation: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from inputs where id=$1 and owner_id=$2")
            .bind(id)
            .bind(o.owner_id)
            .fetch_optional(&s.pool)
            .await?;
    let result = crate::delivery::control::cancel_input(&s.pool, o.owner_id, id, &key, c).await?;
    if let Some(id) = conversation {
        s.dispatcher.trigger(id);
    }
    Ok(Json(result))
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/requests/{request_id}/decision",tag="conversations",params(("id"=Uuid,Path),("request_id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=501,description="Решение пока не поддерживается")))]
async fn decision(
    State(s): State<AppState>,
    o: Owner,
    ApiPath((id, _request_id)): ApiPath<(Uuid, Uuid)>,
    IdempotencyKey(_key): IdempotencyKey,
) -> Result<StatusCode, ApiError> {
    let owned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(o.owner_id)
    .fetch_one(&s.pool)
    .await?;
    if !owned {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    Err(ApiError::new(ErrorCode::NotImplemented))
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
        let mut conn = s.pool.acquire().await?;
        let input = conversations::capability_hold_input(&mut conn, id).await?;
        if let Some(input_id) = input {
            d.holds
                .push(crate::model::HoldView::CapabilityMissing { input_id });
            if let Some(reasons) = d.queue.hold_reasons.as_array_mut() {
                reasons
                    .push(serde_json::json!({"reason":"capability_missing","input_id":input_id}));
            }
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
