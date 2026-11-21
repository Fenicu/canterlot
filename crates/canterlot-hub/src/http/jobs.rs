//! HTTP владельца: задачи на месте (`job_run`) — список, карточка, журнал и отмена.
//! Владелец видит все задачи своих машин, в том числе те, чьего заказчика уже нет.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode, ErrorDetails},
    extract::{ApiPath, ApiQuery},
};
use crate::{
    idempotency,
    inplace_jobs::{self, Job, JobView, State as JobState, logs},
    session::{Availability, Registry},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(log))
        .routes(routes!(cancel))
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Serialize, ToSchema)]
pub struct JobList {
    pub items: Vec<JobView>,
}

/// Задача и последний хвост её вывода.
#[derive(Serialize, ToSchema)]
pub struct JobDetail {
    pub job: JobView,
    /// Хвост на хабе: живой, пока идёт, и итоговый после конца (не больше 64 КиБ).
    pub output_tail: String,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Машина (id).
    machine: Option<Uuid>,
    /// Состояние: `queued`, `dispatched`, `waiting_lease`, `running`, `succeeded`, `failed`,
    /// `timed_out`, `canceled` или `lost`.
    state: Option<String>,
    /// Разговор-заказчик.
    conversation: Option<Uuid>,
    /// Не больше 200, по умолчанию 50; новые первыми.
    limit: Option<i64>,
}

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct LogParams {
    /// Позиция в выводе; без неё — последние `limit` байт хвоста.
    offset: Option<u64>,
    /// Не больше 65536, по умолчанию 16384.
    limit: Option<usize>,
}

fn offline(registry: &Registry, job: &Job) -> bool {
    !job.is_terminal()
        && job.state() != JobState::Queued
        && registry.availability(job.machine_id) != Availability::Available
}

async fn view(
    conn: &mut sqlx::PgConnection,
    registry: &Registry,
    job: &Job,
) -> Result<JobView, sqlx::Error> {
    let mut view = inplace_jobs::view(conn, job).await?;
    view.machine_offline = offline(registry, job);
    Ok(view)
}

async fn owned(conn: &mut sqlx::PgConnection, owner: Uuid, id: Uuid) -> Result<Job, ApiError> {
    let job: Option<Job> = sqlx::query_as("select * from jobs where id=$1 and owner_id=$2")
        .bind(id)
        .bind(owner)
        .fetch_optional(conn)
        .await?;
    job.ok_or_else(|| ApiError::new(ErrorCode::NotFound))
}

#[utoipa::path(get,path="/api/v1/jobs",tag="jobs",params(ListParams),responses(
    (status=200,description="Задачи на месте, новые первыми",body=JobList),
    (status=400,description="Неизвестное состояние или предел",body=super::error::ErrorEnvelope)
))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiQuery(params): ApiQuery<ListParams>,
) -> Result<Json<JobList>, ApiError> {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit)
        || params
            .state
            .as_deref()
            .is_some_and(|state| JobState::parse(state).is_none())
    {
        return Err(
            ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
                reason: Some("jobs: limit is 1..200 and state a known state".to_owned()),
                ..Default::default()
            }),
        );
    }
    let mut conn = state.pool.acquire().await?;
    let rows: Vec<Job> = sqlx::query_as(
        "select * from jobs
         where owner_id=$1
           and ($2::uuid is null or machine_id=$2)
           and ($3::text is null or state=$3)
           and ($4::uuid is null or conversation_id=$4)
         order by created_at desc, id desc limit $5",
    )
    .bind(owner.owner_id)
    .bind(params.machine)
    .bind(params.state.as_deref())
    .bind(params.conversation)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for job in &rows {
        items.push(view(&mut conn, &state.registry, job).await?);
    }
    Ok(Json(JobList { items }))
}

#[utoipa::path(get,path="/api/v1/jobs/{id}",tag="jobs",params(("id"=Uuid,Path,description="Задача")),responses(
    (status=200,description="Задача и хвост вывода",body=JobDetail),
    (status=404,description="Задачи нет у владельца",body=super::error::ErrorEnvelope)
))]
async fn get(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<JobDetail>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let job = owned(&mut conn, owner.owner_id, id).await?;
    let tail = logs::read_tail(&job, None, logs::MAX_READ_BYTES)
        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
    Ok(Json(JobDetail {
        job: view(&mut conn, &state.registry, &job).await?,
        output_tail: tail.text,
    }))
}

#[utoipa::path(get,path="/api/v1/jobs/{id}/log",tag="jobs",params(("id"=Uuid,Path,description="Задача"),LogParams),responses(
    (status=200,description="Кусок журнала: хвост хаба, пока задача идёт, и полный журнал после конца",body=logs::LogSlice),
    (status=404,description="Задачи нет у владельца",body=super::error::ErrorEnvelope)
))]
async fn log(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(params): ApiQuery<LogParams>,
) -> Result<Json<logs::LogSlice>, ApiError> {
    if params
        .limit
        .is_some_and(|limit| !(1..=logs::MAX_READ_BYTES).contains(&limit))
    {
        return Err(
            ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
                reason: Some("jobs: limit is 1..65536".to_owned()),
                ..Default::default()
            }),
        );
    }
    let mut conn = state.pool.acquire().await?;
    let job = owned(&mut conn, owner.owner_id, id).await?;
    drop(conn);
    let slice = logs::read(
        &*state.blobs,
        &job,
        params.offset,
        params.limit.unwrap_or(logs::DEFAULT_READ_BYTES),
    )
    .await
    .map_err(|error| {
        tracing::warn!(kind = "job_log_read", error = %error.kind(), "job log read failed");
        ApiError::new(ErrorCode::Internal)
    })?;
    Ok(Json(slice))
}

#[utoipa::path(post,path="/api/v1/jobs/{id}/cancel",tag="jobs",params(("id"=Uuid,Path,description="Задача"),("Idempotency-Key"=String,Header,description="Ключ операции")),responses(
    (status=200,description="Отмена принята: очередь закрыта сразу, идущую задачу воркер подтвердит; потерянной команда уйдёт, когда машина вернётся",body=JobView),
    (status=404,description="Задачи нет у владельца",body=super::error::ErrorEnvelope)
))]
async fn cancel(
    State(state): State<AppState>,
    owner: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(correlation): Correlation,
) -> Result<Json<JobView>, ApiError> {
    let owner_id = owner.owner_id;
    let registry = state.registry.clone();
    let outcome = idempotency::run_in_tx(
        &state.pool,
        owner_id,
        &key,
        "cancel_job",
        &id,
        None,
        move |conn| {
            Box::pin(async move {
                owned(conn, owner_id, id).await?;
                let job = inplace_jobs::cancel_in_tx(conn, id, "canceled_by_owner", correlation)
                    .await?
                    .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
                Ok::<_, ApiError>(view(conn, &registry, &job).await?)
            })
        },
    )
    .await?;
    Ok(Json(outcome.value))
}
