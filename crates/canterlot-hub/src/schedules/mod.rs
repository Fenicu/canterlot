//! Повторяющиеся запуски по расписанию (план #190, часть A).

pub mod admission;
pub mod cron;

use std::fmt;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    idempotency,
};

pub use cron::{CronError, CronSchedule};

mod time_opt {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<DateTime<Utc>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(at) => crate::activity::time::serialize(at, s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
        Option::<DateTime<Utc>>::deserialize(d)
    }
}

const MAX_NAME_BYTES: usize = 200;
const MAX_PRECHECK_BYTES: usize = 4096;
const DEFAULT_TITLE_TEMPLATE: &str = "{name} · {date}";

/// Цель запуска: новая сессия на машине или существующий разговор.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ScheduleTarget {
    NewSession {
        machine_id: Uuid,
        workdir: String,
        /// Тип задачи из политики маршрутизации; ровно одно из `route`/`executor`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<RouteTarget>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        executor: Option<ExplicitExecutor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group_id: Option<Uuid>,
    },
    ExistingSession {
        conversation_id: Uuid,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct RouteTarget {
    pub task_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ExplicitExecutor {
    pub harness: String,
    pub model: String,
    pub effort: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    Skip,
    Allow,
}
impl OverlapPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Allow => "allow",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSchedule {
    pub name: String,
    pub cron: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub target: ScheduleTarget,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck_timeout_seconds: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchSchedule {
    pub base_revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ScheduleTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precheck_timeout_seconds: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catch_up_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlap: Option<OverlapPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_template: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct Schedule {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub cron: String,
    pub timezone: String,
    pub target: ScheduleTarget,
    pub prompt: String,
    pub precheck: Option<String>,
    pub precheck_timeout_seconds: i32,
    pub catch_up_minutes: i32,
    pub overlap: OverlapPolicy,
    pub title_template: String,
    #[serde(with = "crate::activity::time")]
    pub next_fire_at: DateTime<Utc>,
    #[serde(with = "crate::activity::time")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::activity::time")]
    pub updated_at: DateTime<Utc>,
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Claimed,
    Prechecking,
    Started,
    SkippedPrecheck,
    SkippedLimits,
    SkippedOverlap,
    SkippedMachineOffline,
    SkippedChanged,
    Missed,
    Failed,
}
impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Prechecking => "prechecking",
            Self::Started => "started",
            Self::SkippedPrecheck => "skipped_precheck",
            Self::SkippedLimits => "skipped_limits",
            Self::SkippedOverlap => "skipped_overlap",
            Self::SkippedMachineOffline => "skipped_machine_offline",
            Self::SkippedChanged => "skipped_changed",
            Self::Missed => "missed",
            Self::Failed => "failed",
        }
    }
    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "claimed" => Ok(Self::Claimed),
            "prechecking" => Ok(Self::Prechecking),
            "started" => Ok(Self::Started),
            "skipped_precheck" => Ok(Self::SkippedPrecheck),
            "skipped_limits" => Ok(Self::SkippedLimits),
            "skipped_overlap" => Ok(Self::SkippedOverlap),
            "skipped_machine_offline" => Ok(Self::SkippedMachineOffline),
            "skipped_changed" => Ok(Self::SkippedChanged),
            "missed" => Ok(Self::Missed),
            "failed" => Ok(Self::Failed),
            _ => Err(sqlx::Error::Decode("invalid schedule run state".into())),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct ScheduleRun {
    pub id: Uuid,
    pub schedule_id: Uuid,
    #[serde(with = "crate::activity::time")]
    pub scheduled_at: DateTime<Utc>,
    pub claimed_revision: i64,
    pub state: RunState,
    #[serde(with = "crate::activity::time")]
    pub created_at: DateTime<Utc>,
    #[serde(default, with = "time_opt")]
    pub started_at: Option<DateTime<Utc>>,
    pub delay_ms: Option<i64>,
    pub session_id: Option<Uuid>,
    pub input_id: Option<Uuid>,
    pub manual: bool,
    pub idempotency_key: String,
    pub detail: Value,
}

#[derive(Clone, sqlx::FromRow)]
struct ScheduleRow {
    id: Uuid,
    owner_id: Uuid,
    name: String,
    enabled: bool,
    cron: String,
    timezone: String,
    target: Value,
    prompt: String,
    precheck: Option<String>,
    precheck_timeout_seconds: i32,
    catch_up_minutes: i32,
    overlap: String,
    title_template: String,
    next_fire_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    revision: i64,
}
impl ScheduleRow {
    fn into_schedule(self) -> Result<Schedule, sqlx::Error> {
        let target =
            serde_json::from_value(self.target).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let overlap = match self.overlap.as_str() {
            "skip" => OverlapPolicy::Skip,
            "allow" => OverlapPolicy::Allow,
            _ => return Err(sqlx::Error::Decode("invalid overlap policy".into())),
        };
        Ok(Schedule {
            id: self.id,
            owner_id: self.owner_id,
            name: self.name,
            enabled: self.enabled,
            cron: self.cron,
            timezone: self.timezone,
            target,
            prompt: self.prompt,
            precheck: self.precheck,
            precheck_timeout_seconds: self.precheck_timeout_seconds,
            catch_up_minutes: self.catch_up_minutes,
            overlap,
            title_template: self.title_template,
            next_fire_at: self.next_fire_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            revision: self.revision,
        })
    }
}

#[derive(Clone, sqlx::FromRow)]
struct RunRow {
    id: Uuid,
    schedule_id: Uuid,
    scheduled_at: DateTime<Utc>,
    claimed_revision: i64,
    state: String,
    started_at: Option<DateTime<Utc>>,
    delay_ms: Option<i64>,
    session_id: Option<Uuid>,
    input_id: Option<Uuid>,
    manual: bool,
    idempotency_key: String,
    detail: Value,
    created_at: DateTime<Utc>,
}
impl RunRow {
    fn into_run(self) -> Result<ScheduleRun, sqlx::Error> {
        Ok(ScheduleRun {
            id: self.id,
            schedule_id: self.schedule_id,
            scheduled_at: self.scheduled_at,
            claimed_revision: self.claimed_revision,
            state: RunState::parse(&self.state)?,
            created_at: self.created_at,
            started_at: self.started_at,
            delay_ms: self.delay_ms,
            session_id: self.session_id,
            input_id: self.input_id,
            manual: self.manual,
            idempotency_key: self.idempotency_key,
            detail: self.detail,
        })
    }
}

#[derive(Debug)]
pub enum ScheduleError {
    NotFound,
    StaleRevision,
    Invalid(&'static str),
    Conflict(&'static str),
    Db(sqlx::Error),
}
impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "not_found",
            Self::StaleRevision => "stale_revision",
            Self::Invalid(reason) | Self::Conflict(reason) => reason,
            Self::Db(e) => return fmt::Debug::fmt(&crate::dberr::describe(e), f),
        })
    }
}
impl std::error::Error for ScheduleError {}
impl From<sqlx::Error> for ScheduleError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}
impl From<CronError> for ScheduleError {
    fn from(e: CronError) -> Self {
        Self::Invalid(match e {
            CronError::InvalidExpression => "invalid_cron",
            CronError::InvalidTimezone => "invalid_timezone",
            CronError::TooFrequent => "too_frequent",
        })
    }
}
impl From<idempotency::IdempotencyConflict> for ScheduleError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::Conflict("idempotency_conflict")
    }
}

/// Часовой пояс владельца из существующей настройки пушей; при её отсутствии — Европа/Москва.
pub async fn owner_timezone(conn: &mut PgConnection) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "select coalesce(nullif((select timezone from push_settings), ''), 'Europe/Moscow')",
    )
    .fetch_one(conn)
    .await
}

#[allow(clippy::too_many_arguments)]
fn validate_common(
    name: &str,
    cron: &str,
    timezone: &str,
    prompt: &str,
    precheck: Option<&str>,
    timeout: i32,
    catch_up: i32,
    template: &str,
    now: DateTime<Utc>,
) -> Result<(CronSchedule, chrono_tz::Tz), ScheduleError> {
    if name.trim().is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(ScheduleError::Invalid("invalid_name"));
    }
    if prompt.len() > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES {
        return Err(ScheduleError::Invalid("prompt_too_long"));
    }
    if precheck.is_some_and(|value| value.len() > MAX_PRECHECK_BYTES) {
        return Err(ScheduleError::Invalid("precheck_too_long"));
    }
    if !(1..=300).contains(&timeout) {
        return Err(ScheduleError::Invalid("invalid_precheck_timeout"));
    }
    if !(0..=1440).contains(&catch_up) {
        return Err(ScheduleError::Invalid("invalid_catch_up"));
    }
    if template.trim().is_empty() || template.len() > MAX_NAME_BYTES * 4 {
        return Err(ScheduleError::Invalid("invalid_title_template"));
    }
    let parsed = CronSchedule::parse(cron)?;
    let tz = cron::timezone(timezone)?;
    parsed.validate_min_interval(now, tz)?;
    Ok((parsed, tz))
}

fn validate_target(target: &ScheduleTarget) -> Result<(), ScheduleError> {
    match target {
        ScheduleTarget::NewSession {
            workdir,
            route,
            executor,
            ..
        } => {
            if workdir.is_empty() || !workdir.starts_with('/') || workdir.contains("..") {
                return Err(ScheduleError::Invalid("invalid_workdir"));
            }
            match (route, executor) {
                (Some(route), None) if !route.task_type.trim().is_empty() => Ok(()),
                (None, Some(executor))
                    if !executor.harness.trim().is_empty()
                        && !executor.model.trim().is_empty()
                        && !executor.effort.trim().is_empty() =>
                {
                    Ok(())
                }
                _ => Err(ScheduleError::Invalid("invalid_executor")),
            }
        }
        ScheduleTarget::ExistingSession { .. } => Ok(()),
    }
}

async fn changed(
    conn: &mut PgConnection,
    schedule: &Schedule,
    kind: &'static str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Schedules {
                owner_id: schedule.owner_id,
            },
            kind,
            conversation_id: None,
            execution_id: None,
            payload: serde_json::to_value(schedule)
                .map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Создание расписания. Ключ идемпотентности защищает повтор lost-response.
pub async fn create(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    req: NewSchedule,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let mut conn = pool.acquire().await?;
    let timezone = match req.timezone.as_deref() {
        Some(tz) => tz.to_owned(),
        None => owner_timezone(&mut conn).await?,
    };
    validate_target(&req.target)?;
    let (parsed, tz) = validate_common(
        &req.name,
        &req.cron,
        &timezone,
        &req.prompt,
        req.precheck.as_deref(),
        req.precheck_timeout_seconds.unwrap_or(60),
        req.catch_up_minutes.unwrap_or(30),
        req.title_template
            .as_deref()
            .unwrap_or(DEFAULT_TITLE_TEMPLATE),
        now,
    )?;
    let next_fire_at = parsed
        .next_after(now, tz)
        .ok_or(ScheduleError::Invalid("invalid_cron"))?;
    let body = json!({"owner_id": owner_id, "request": &req, "resolved_timezone": timezone});
    let hash = idempotency::request_hash("create_schedule", &body);
    let owned_key = key.to_owned();
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "create_schedule",
        &body,
        None,
        move |conn| {
            Box::pin(async move {
                match idempotency::lookup(conn, owner_id, &owned_key, "create_schedule", &hash).await? {
                    idempotency::Begin::Replay(value) => {
                        return serde_json::from_value(value).map_err(|e| {
                            ScheduleError::Db(sqlx::Error::Decode(Box::new(e)))
                        });
                    }
                    idempotency::Begin::Conflict => return Err(ScheduleError::Conflict("idempotency_conflict")),
                    idempotency::Begin::New => {}
                }
                let id = Uuid::now_v7();
                let row: ScheduleRow = sqlx::query_as(
                    "insert into schedules(id,owner_id,name,cron,timezone,target,prompt,precheck,
                        precheck_timeout_seconds,catch_up_minutes,overlap,title_template,next_fire_at)
                     values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) returning *",
                )
                .bind(id)
                .bind(owner_id)
                .bind(&req.name)
                .bind(&req.cron)
                .bind(&timezone)
                .bind(json!(req.target))
                .bind(&req.prompt)
                .bind(req.precheck.as_deref())
                .bind(req.precheck_timeout_seconds.unwrap_or(60))
                .bind(req.catch_up_minutes.unwrap_or(30))
                .bind(req.overlap.unwrap_or(OverlapPolicy::Skip).as_str())
                .bind(req.title_template.as_deref().unwrap_or(DEFAULT_TITLE_TEMPLATE))
                .bind(next_fire_at)
                .fetch_one(&mut *conn)
                .await?;
                let schedule = row.into_schedule()?;
                changed(conn, &schedule, "schedule_changed", correlation_id).await?;
                Ok::<Schedule, ScheduleError>(schedule)
            })
        },
    )
    .await?
    .value;
    Ok(value)
}

pub async fn get(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<Schedule>, sqlx::Error> {
    let row: Option<ScheduleRow> =
        sqlx::query_as("select * from schedules where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    row.map(ScheduleRow::into_schedule).transpose()
}

pub async fn list(
    conn: &mut PgConnection,
    owner_id: Uuid,
    limit: i64,
) -> Result<Vec<Schedule>, sqlx::Error> {
    let rows: Vec<ScheduleRow> = sqlx::query_as(
        "select * from schedules where owner_id=$1 order by created_at desc,id desc limit $2",
    )
    .bind(owner_id)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(ScheduleRow::into_schedule).collect()
}

/// Правка с проверкой ревизии; смена cron или пояса пересчитывает следующий запуск от now.
pub async fn update(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    patch: PatchSchedule,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let base_revision = patch.base_revision;
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "update_schedule",
        &json!({"id": id, "base_revision": base_revision, "patch": &patch}),
        None,
        move |conn| {
            Box::pin(async move {
                let row: Option<ScheduleRow> =
                    sqlx::query_as("select * from schedules where id=$1 and owner_id=$2 for update")
                        .bind(id)
                        .bind(owner_id)
                        .fetch_optional(&mut *conn)
                        .await?;
                let mut schedule = row
                    .ok_or(ScheduleError::NotFound)?
                    .into_schedule()?;
                if schedule.revision != base_revision {
                    return Err(ScheduleError::StaleRevision);
                }
                if let Some(name) = patch.name {
                    schedule.name = name;
                }
                if let Some(precheck) = patch.precheck {
                    schedule.precheck = precheck;
                }
                if let Some(timeout) = patch.precheck_timeout_seconds {
                    schedule.precheck_timeout_seconds = timeout;
                }
                if let Some(catch_up) = patch.catch_up_minutes {
                    schedule.catch_up_minutes = catch_up;
                }
                if let Some(overlap) = patch.overlap {
                    schedule.overlap = overlap;
                }
                if let Some(template) = patch.title_template {
                    schedule.title_template = template;
                }
                if let Some(prompt) = patch.prompt {
                    schedule.prompt = prompt;
                }
                if let Some(target) = patch.target {
                    validate_target(&target)?;
                    schedule.target = target;
                }
                let cron_changed = patch.cron.is_some();
                let tz_changed = patch.timezone.is_some();
                if let Some(cron) = patch.cron {
                    schedule.cron = cron;
                }
                if let Some(timezone) = patch.timezone {
                    schedule.timezone = timezone;
                }
                validate_common(
                    &schedule.name,
                    &schedule.cron,
                    &schedule.timezone,
                    &schedule.prompt,
                    schedule.precheck.as_deref(),
                    schedule.precheck_timeout_seconds,
                    schedule.catch_up_minutes,
                    &schedule.title_template,
                    now,
                )?;
                if cron_changed || tz_changed {
                    let parsed = CronSchedule::parse(&schedule.cron)?;
                    let tz = cron::timezone(&schedule.timezone)?;
                    schedule.next_fire_at = parsed
                        .next_after(now, tz)
                        .ok_or(ScheduleError::Invalid("invalid_cron"))?;
                }
                schedule.revision += 1;
                schedule.updated_at = Utc::now();
                sqlx::query(
                    "update schedules set name=$2,cron=$3,timezone=$4,target=$5,prompt=$6,precheck=$7,
                        precheck_timeout_seconds=$8,catch_up_minutes=$9,overlap=$10,title_template=$11,
                        next_fire_at=$12,updated_at=clock_timestamp(),revision=$13 where id=$1",
                )
                .bind(id)
                .bind(&schedule.name)
                .bind(&schedule.cron)
                .bind(&schedule.timezone)
                .bind(json!(schedule.target))
                .bind(&schedule.prompt)
                .bind(schedule.precheck.as_deref())
                .bind(schedule.precheck_timeout_seconds)
                .bind(schedule.catch_up_minutes)
                .bind(schedule.overlap.as_str())
                .bind(&schedule.title_template)
                .bind(schedule.next_fire_at)
                .bind(schedule.revision)
                .execute(&mut *conn)
                .await?;
                changed(conn, &schedule, "schedule_changed", correlation_id).await?;
                Ok::<Schedule, ScheduleError>(schedule)
            })
        },
    )
    .await?
    .value;
    Ok(value)
}

pub async fn set_enabled(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    enabled: bool,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "set_schedule_enabled",
        &json!({"id": id, "enabled": enabled}),
        None,
        move |conn| {
            Box::pin(async move {
                let row: Option<ScheduleRow> =
                    sqlx::query_as("select * from schedules where id=$1 and owner_id=$2 for update")
                        .bind(id)
                        .bind(owner_id)
                        .fetch_optional(&mut *conn)
                        .await?;
                let mut schedule = row.ok_or(ScheduleError::NotFound)?.into_schedule()?;
                schedule.enabled = enabled;
                schedule.revision += 1;
                sqlx::query("update schedules set enabled=$2,revision=$3,updated_at=clock_timestamp() where id=$1")
                    .bind(id)
                    .bind(enabled)
                    .bind(schedule.revision)
                    .execute(&mut *conn)
                    .await?;
                changed(conn, &schedule, "schedule_changed", correlation_id).await?;
                Ok::<Schedule, ScheduleError>(schedule)
            })
        },
    )
    .await?
    .value;
    Ok(value)
}

pub async fn delete(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "delete_schedule",
        &json!({"id": id}),
        None,
        move |conn| {
            Box::pin(async move {
                let row: Option<ScheduleRow> = sqlx::query_as(
                    "select * from schedules where id=$1 and owner_id=$2 for update",
                )
                .bind(id)
                .bind(owner_id)
                .fetch_optional(&mut *conn)
                .await?;
                let schedule = row.ok_or(ScheduleError::NotFound)?.into_schedule()?;
                sqlx::query("delete from schedules where id=$1")
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                changed(conn, &schedule, "schedule_changed", correlation_id).await?;
                Ok::<Schedule, ScheduleError>(schedule)
            })
        },
    )
    .await?
    .value;
    Ok(value)
}

pub async fn runs(
    conn: &mut PgConnection,
    owner_id: Uuid,
    schedule_id: Uuid,
    before: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<ScheduleRun>, sqlx::Error> {
    let rows: Vec<RunRow> = sqlx::query_as(
        "select r.* from schedule_runs r join schedules s on s.id=r.schedule_id
         where r.owner_id=$1 and r.schedule_id=$2 and ($3::timestamptz is null or r.scheduled_at < $3)
         order by r.scheduled_at desc,r.id desc limit $4",
    )
    .bind(owner_id)
    .bind(schedule_id)
    .bind(before)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(RunRow::into_run).collect()
}

pub async fn get_run(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<ScheduleRun>, sqlx::Error> {
    let row: Option<RunRow> =
        sqlx::query_as("select * from schedule_runs where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    row.map(RunRow::into_run).transpose()
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_run(
    conn: &mut PgConnection,
    id: Uuid,
    schedule_id: Uuid,
    owner_id: Uuid,
    scheduled_at: DateTime<Utc>,
    claimed_revision: i64,
    state: RunState,
    manual: bool,
    key: &str,
    detail: Value,
) -> Result<ScheduleRun, sqlx::Error> {
    let row: RunRow = sqlx::query_as(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,detail)
         values($1,$2,$3,$4,$5,$6,$7,$8,$9) returning *",
    )
    .bind(id)
    .bind(schedule_id)
    .bind(owner_id)
    .bind(scheduled_at)
    .bind(claimed_revision)
    .bind(state.as_str())
    .bind(manual)
    .bind(key)
    .bind(detail)
    .fetch_one(conn)
    .await?;
    row.into_run()
}

#[allow(clippy::too_many_arguments)]
pub async fn set_run_state(
    conn: &mut PgConnection,
    id: Uuid,
    state: RunState,
    detail: Value,
    started_at: Option<DateTime<Utc>>,
    delay_ms: Option<i64>,
    session_id: Option<Uuid>,
    input_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update schedule_runs set state=$2,detail=$3,started_at=coalesce($4,started_at),
            delay_ms=coalesce($5,delay_ms),session_id=coalesce($6,session_id),
            input_id=coalesce($7,input_id),updated_at=clock_timestamp() where id=$1",
    )
    .bind(id)
    .bind(state.as_str())
    .bind(detail)
    .bind(started_at)
    .bind(delay_ms)
    .bind(session_id)
    .bind(input_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Заявление, готовое к приёму вне тика.
#[derive(Clone, Debug)]
pub struct ClaimedRun {
    pub run: ScheduleRun,
    pub schedule: Schedule,
}

async fn run_changed(
    conn: &mut PgConnection,
    run: &ScheduleRun,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let owner_id = run_detail_owner(conn, run.id).await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Schedules { owner_id },
            kind: "schedule_run_changed",
            conversation_id: None,
            execution_id: None,
            payload: serde_json::to_value(run).map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn run_detail_owner(conn: &mut PgConnection, run_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("select owner_id from schedule_runs where id=$1")
        .bind(run_id)
        .fetch_one(conn)
        .await
}

/// Короткая транзакция тика: выбрать просроченные срабатывания, записать один
/// запуск на последнее из них и сдвинуть `next_fire_at`. Повтор тика или
/// перезапуск не создаст вторую строку благодаря уникальности пары.
pub async fn claim_due(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<ClaimedRun>, sqlx::Error> {
    let rows: Vec<ScheduleRow> = sqlx::query_as(
        "select * from schedules where enabled and next_fire_at<=$1
         order by next_fire_at,id for update skip locked limit $2",
    )
    .bind(now)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    let mut claimed = Vec::new();
    for row in rows {
        let schedule = match row.into_schedule() {
            Ok(schedule) => schedule,
            Err(error) => {
                tracing::warn!(kind = "schedule_claim_invalid_row", error = %error, "schedule skipped");
                continue;
            }
        };
        let parsed = match CronSchedule::parse(&schedule.cron) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let tz = match cron::timezone(&schedule.timezone) {
            Ok(tz) => tz,
            Err(_) => continue,
        };
        let mut occurrences = Vec::new();
        let mut cursor = schedule.next_fire_at;
        while cursor <= now && occurrences.len() < 100_000 {
            occurrences.push(cursor);
            match parsed.next_after(cursor, tz) {
                Some(next) => cursor = next,
                None => break,
            }
        }
        if occurrences.is_empty() {
            continue;
        }
        let window = effective_window(schedule.catch_up_minutes);
        let within: Vec<DateTime<Utc>> = occurrences
            .iter()
            .copied()
            .filter(|at| now - *at <= window)
            .collect();
        let (scheduled_at, missed_before, state) = match within.last().copied() {
            Some(at) => (
                at,
                occurrences.iter().filter(|other| **other < at).count(),
                RunState::Claimed,
            ),
            None => (
                *occurrences.last().unwrap(),
                occurrences.len(),
                RunState::Missed,
            ),
        };
        let detail = json!({"missed_before": missed_before});
        let run_id = Uuid::now_v7();
        let key = format!("schedule:{}:{}", schedule.id, scheduled_at.to_rfc3339());
        let row: Option<RunRow> = sqlx::query_as(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,
                state,idempotency_key,detail)
             values($1,$2,$3,$4,$5,$6,$7,$8)
             on conflict (schedule_id,scheduled_at) do nothing returning *",
        )
        .bind(run_id)
        .bind(schedule.id)
        .bind(schedule.owner_id)
        .bind(scheduled_at)
        .bind(schedule.revision)
        .bind(state.as_str())
        .bind(&key)
        .bind(&detail)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(row) = row else {
            continue;
        };
        let run = row.into_run()?;
        run_changed(conn, &run, Uuid::now_v7()).await?;
        let next_fire = parsed
            .next_after(*occurrences.last().unwrap(), tz)
            .unwrap_or(now + TimeDelta::days(366));
        sqlx::query(
            "update schedules set next_fire_at=$2,updated_at=clock_timestamp() where id=$1",
        )
        .bind(schedule.id)
        .bind(next_fire)
        .execute(&mut *conn)
        .await?;
        claimed.push(ClaimedRun { run, schedule });
    }
    Ok(claimed)
}

/// Уборка: не больше 200 последних запусков на расписание.
pub async fn prune_runs(conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from schedule_runs where id in (
           select id from (
             select id,row_number() over (partition by schedule_id order by scheduled_at desc,id desc) as n
             from schedule_runs
           ) ranked where n > 200
         )",
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// Сколько попыток ещё в полёте у расписания (для политики `skip`).
pub async fn in_flight(conn: &mut PgConnection, schedule_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from schedule_runs where schedule_id=$1 and state in ('claimed','prechecking')",
    )
    .bind(schedule_id)
    .fetch_one(conn)
    .await
}

/// Тип шаблона:
pub fn render_title(template: &str, name: &str, at: DateTime<Utc>) -> String {
    let date = at.format("%d.%m").to_string();
    template.replace("{name}", name).replace("{date}", &date)
}

/// Сколько времени прошло от запланированного момента до подтверждённой очереди.
pub fn delay_ms(scheduled_at: DateTime<Utc>, committed_at: DateTime<Utc>) -> i64 {
    (committed_at - scheduled_at).num_milliseconds().max(0)
}

/// Задержка, с которой тик видит просроченное срабатывание.
pub fn catch_up_window(catch_up_minutes: i32) -> TimeDelta {
    TimeDelta::minutes(i64::from(catch_up_minutes))
}

/// Тик идёт не чаще раза в минуту: срабатывание, опоздавшее меньше чем
/// на окно punctuality, всё ещё считается текущим даже при `catch_up=0`.
/// Ноль отключает повтор старых пропусков, но не отменяет текущий запуск.
pub const ON_TIME_WINDOW: TimeDelta = TimeDelta::minutes(1);

/// Эффективное окно: `catch_up_minutes`, но не уже пунктуальности.
pub fn effective_window(catch_up_minutes: i32) -> TimeDelta {
    catch_up_window(catch_up_minutes).max(ON_TIME_WINDOW)
}
