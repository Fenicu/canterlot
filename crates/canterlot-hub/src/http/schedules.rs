use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode, ErrorEnvelope},
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::schedules::{self, NewSchedule, PatchSchedule, Schedule, ScheduleRun};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(preview))
        .routes(routes!(defaults))
        .routes(routes!(get, update, delete))
        .routes(routes!(pause))
        .routes(routes!(resume))
        .routes(routes!(run))
        .routes(routes!(runs))
}

fn map(error: schedules::ScheduleError) -> ApiError {
    match error {
        schedules::ScheduleError::NotFound => ApiError::new(ErrorCode::NotFound),
        schedules::ScheduleError::StaleRevision => {
            ApiError::new(ErrorCode::ScheduleRevisionConflict)
        }
        schedules::ScheduleError::Invalid(_) => ApiError::new(ErrorCode::ScheduleInvalid),
        schedules::ScheduleError::Conflict(_) => ApiError::new(ErrorCode::IdempotencyConflict),
        // Ручная финализация после каскадного удаления: строка запуска ушла
        // вместе с расписанием, это 404, а не 500.
        schedules::ScheduleError::Db(sqlx::Error::RowNotFound) => {
            ApiError::new(ErrorCode::NotFound)
        }
        schedules::ScheduleError::Db(error) => error.into(),
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewBody {
    pub cron: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default = "default_count")]
    pub count: usize,
}
fn default_count() -> usize {
    5
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RunBody {
    /// «Запустить в любом случае»: обходит проверки расписания.
    #[serde(default)]
    pub run_anyway: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ScheduleRunPage {
    pub items: Vec<ScheduleRun>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct RunsQuery {
    /// `next_cursor` предыдущей страницы; момент последнего запуска.
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// Значения по умолчанию для редактора расписаний.
#[derive(Serialize, ToSchema)]
pub struct ScheduleDefaults {
    /// Пояс владельца из настроек пушей (IANA); без настройки — `Europe/Moscow`.
    pub timezone: String,
}

#[utoipa::path(get,path="/api/v1/schedules/defaults",tag="schedules",responses((status=200,description="Пояс владельца по умолчанию для нового расписания",body=ScheduleDefaults)))]
async fn defaults(
    State(state): State<AppState>,
    owner: Owner,
) -> Result<Json<ScheduleDefaults>, ApiError> {
    // `owner` нужен только для аутентификации маршрута: настройка пушей одна на хаб.
    let _ = owner;
    Ok(Json(ScheduleDefaults {
        timezone: schedules::owner_timezone(&mut *state.pool.acquire().await?).await?,
    }))
}

#[utoipa::path(get,path="/api/v1/schedules",tag="schedules",responses((status=200,description="Расписания владельца",body=Vec<Schedule>)))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
) -> Result<Json<Vec<Schedule>>, ApiError> {
    Ok(Json(
        schedules::list(&mut *state.pool.acquire().await?, owner.owner_id, 200).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/schedules",tag="schedules",params(("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=NewSchedule,responses((status=200,description="Расписание создано",body=Schedule),(status=422,description="Некорректное расписание",body=ErrorEnvelope),(status=409,description="Конфликт ключа",body=ErrorEnvelope)))]
async fn create(
    State(state): State<AppState>,
    owner: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(req): ApiJson<NewSchedule>,
) -> Result<Json<Schedule>, ApiError> {
    Ok(Json(
        schedules::create(
            &state.pool,
            owner.owner_id,
            &key,
            req,
            chrono::Utc::now(),
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[utoipa::path(post,path="/api/v1/schedules/preview",tag="schedules",request_body=PreviewBody,responses((status=200,description="Следующие моменты в UTC",body=Vec<String>),(status=422,description="Некорректное выражение",body=ErrorEnvelope)))]
async fn preview(
    State(state): State<AppState>,
    owner: Owner,
    ApiJson(req): ApiJson<PreviewBody>,
) -> Result<Json<Vec<String>>, ApiError> {
    let cron = schedules::CronSchedule::parse(&req.cron).map_err(|e| map(e.into()))?;
    let timezone = match req.timezone.as_deref() {
        Some(tz) => schedules::cron::timezone(tz).map_err(|e| map(e.into()))?,
        None => {
            let name = schedules::owner_timezone(&mut *state.pool.acquire().await?).await?;
            schedules::cron::timezone(&name).map_err(|e| map(e.into()))?
        }
    };
    let count = req.count.clamp(1, 20);
    let now = chrono::Utc::now();
    cron.validate_min_interval(now, timezone)
        .map_err(|e| map(e.into()))?;
    let fires = cron
        .next_n_after(now, timezone, count)
        .into_iter()
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .collect();
    // `owner` нужен только для аутентификации маршрута.
    let _ = owner;
    Ok(Json(fires))
}

#[utoipa::path(get,path="/api/v1/schedules/{id}",tag="schedules",params(("id"=Uuid,Path,description="Расписание")),responses((status=200,description="Расписание",body=Schedule),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn get(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Schedule>, ApiError> {
    schedules::get(&mut *state.pool.acquire().await?, owner.owner_id, id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))
}

#[utoipa::path(patch,path="/api/v1/schedules/{id}",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=PatchSchedule,responses((status=200,description="Расписание изменено",body=Schedule),(status=404,description="Расписание не найдено",body=ErrorEnvelope),(status=409,description="Устаревшая ревизия",body=ErrorEnvelope),(status=422,description="Некорректное расписание",body=ErrorEnvelope)))]
async fn update(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(patch): ApiJson<PatchSchedule>,
) -> Result<Json<Schedule>, ApiError> {
    Ok(Json(
        schedules::update(
            &state.pool,
            owner.owner_id,
            id,
            &key,
            patch,
            chrono::Utc::now(),
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionBody {
    /// Ревизия из последнего виденного клиентом состояния (как у PATCH).
    pub base_revision: i64,
}

#[utoipa::path(delete,path="/api/v1/schedules/{id}",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=RevisionBody,responses((status=200,description="Расписание удалено",body=Schedule),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn delete(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<RevisionBody>,
) -> Result<Json<Schedule>, ApiError> {
    Ok(Json(
        schedules::delete(
            &state.pool,
            owner.owner_id,
            id,
            &key,
            body.base_revision,
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[utoipa::path(post,path="/api/v1/schedules/{id}/pause",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=RevisionBody,responses((status=200,description="Расписание на паузе",body=Schedule),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn pause(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<RevisionBody>,
) -> Result<Json<Schedule>, ApiError> {
    Ok(Json(
        schedules::set_enabled(
            &state.pool,
            owner.owner_id,
            id,
            &key,
            false,
            body.base_revision,
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[utoipa::path(post,path="/api/v1/schedules/{id}/resume",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=RevisionBody,responses((status=200,description="Расписание возобновлено",body=Schedule),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn resume(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<RevisionBody>,
) -> Result<Json<Schedule>, ApiError> {
    Ok(Json(
        schedules::set_enabled(
            &state.pool,
            owner.owner_id,
            id,
            &key,
            true,
            body.base_revision,
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[utoipa::path(post,path="/api/v1/schedules/{id}/run",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),("Idempotency-Key"=String,Header,description="Ключ операции")),request_body=RunBody,responses((status=200,description="Запуск принят",body=ScheduleRun),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn run(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
    ApiJson(body): ApiJson<RunBody>,
) -> Result<Json<ScheduleRun>, ApiError> {
    Ok(Json(
        crate::schedules::admission::run_now(
            &state,
            owner.owner_id,
            id,
            &key,
            body.run_anyway,
            correlation,
        )
        .await
        .map_err(map)?,
    ))
}

#[utoipa::path(get,path="/api/v1/schedules/{id}/runs",tag="schedules",params(("id"=Uuid,Path,description="Расписание"),RunsQuery),responses((status=200,description="История запусков",body=ScheduleRunPage),(status=404,description="Расписание не найдено",body=ErrorEnvelope)))]
async fn runs(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(query): ApiQuery<RunsQuery>,
) -> Result<Json<ScheduleRunPage>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let before = match query.cursor.as_deref() {
        Some(cursor) => Some(
            schedules::decode_runs_cursor(cursor)
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidOffset))?,
        ),
        None => None,
    };
    let mut conn = state.pool.acquire().await?;
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from schedules where id=$1 and owner_id=$2)")
            .bind(id)
            .bind(owner.owner_id)
            .fetch_one(&mut *conn)
            .await?;
    if !exists {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    let mut items = schedules::runs(&mut conn, owner.owner_id, id, before, limit + 1).await?;
    let next_cursor = if items.len() as i64 > limit {
        // Лишний ряд — только признак конца: курсор строим от последнего
        // ВОЗВРАЩЁННОГО ряда, иначе он потеряется на следующей странице.
        items.pop();
        items.last().map(schedules::encode_runs_cursor)
    } else {
        None
    };
    Ok(Json(ScheduleRunPage { items, next_cursor }))
}
