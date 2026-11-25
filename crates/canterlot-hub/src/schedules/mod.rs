//! Повторяющиеся запуски по расписанию (план #190, часть A).

pub mod admission;
pub mod cron;
pub mod fault;

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

pub(crate) mod time_opt {
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

/// `Option<Option<String>>`: различать «поле пропущено» (`None`),
/// явный `null` (`Some(None)` — очистить) и строку (`Some(Some(_))`).
/// Без этого serde складывает пропуск и `null` в одно `None`.
fn nullable_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

const MAX_NAME_BYTES: usize = 200;
const MAX_PRECHECK_BYTES: usize = 4096;
const DEFAULT_TITLE_TEMPLATE: &str = "{name} · {date}";

/// Цель запуска: новая сессия на машине, существующий разговор или отчёт из контекста сессии.
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
    /// «Отчёт из сессии»: скрытая боковая копия читает контекст разговора, не трогая его,
    /// а её ответ ложится карточкой в ленту этого разговора.
    SessionReport {
        conversation_id: Uuid,
    },
}

impl ScheduleTarget {
    /// Разговор, на котором запуск держит очередь и лимиты: у существующей сессии и у отчёта.
    pub fn conversation_id(&self) -> Option<Uuid> {
        match self {
            Self::ExistingSession { conversation_id } | Self::SessionReport { conversation_id } => {
                Some(*conversation_id)
            }
            Self::NewSession { .. } => None,
        }
    }
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
    #[serde(
        default,
        deserialize_with = "nullable_string",
        skip_serializing_if = "Option::is_none"
    )]
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
    pub run_anyway: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<Uuid>,
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
    run_anyway: bool,
    attempt_id: Option<Uuid>,
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
            run_anyway: self.run_anyway,
            attempt_id: self.attempt_id,
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
        ScheduleTarget::ExistingSession { .. } | ScheduleTarget::SessionReport { .. } => Ok(()),
    }
}

/// Проверка цели «Отчёт из сессии» при создании и правке: разговор есть, принадлежит
/// владельцу, не в корзине и сам не боковая копия, а машина умеет боковой чат.
/// Остальные цели проверяются без обращения к базе в `validate_target`.
pub async fn check_target(
    conn: &mut PgConnection,
    registry: &crate::session::Registry,
    owner_id: Uuid,
    target: &ScheduleTarget,
) -> Result<(), ScheduleError> {
    let ScheduleTarget::SessionReport { conversation_id } = target else {
        return Ok(());
    };
    let row: Option<(Uuid, bool)> = sqlx::query_as(
        "select machine_id, side_of is not null from conversations
          where id=$1 and owner_id=$2 and trashed_at is null",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((machine_id, is_side)) = row else {
        return Err(ScheduleError::Invalid("invalid_report_target"));
    };
    if is_side {
        return Err(ScheduleError::Invalid("invalid_report_target"));
    }
    if !registry.has_capability(machine_id, crate::session::CAPABILITY_SIDE_CHAT_V1)
        || !registry.has_capability(machine_id, crate::session::CAPABILITY_RUN_OUTCOME_V1)
    {
        return Err(ScheduleError::Invalid("report_capability_missing"));
    }
    Ok(())
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
    base_revision: i64,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "set_schedule_enabled",
        &json!({"id": id, "enabled": enabled, "base_revision": base_revision}),
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
                let mut schedule = row.ok_or(ScheduleError::NotFound)?.into_schedule()?;
                if schedule.revision != base_revision {
                    return Err(ScheduleError::StaleRevision);
                }
                // Возобновление считает следующее срабатывание от этого момента. Пока расписание
                // стояло на паузе, тик его момент не двигал, и просроченный момент внутри окна
                // catch-up запустился бы сразу: владелец ставил паузу как раз затем, чтобы
                // этого запуска не было. У включённого расписания момент не трогаем: тик мог
                // ещё не заявить срабатывание, которое уже наступило.
                if enabled && !schedule.enabled {
                    let parsed = CronSchedule::parse(&schedule.cron)?;
                    let tz = cron::timezone(&schedule.timezone)?;
                    schedule.next_fire_at = parsed
                        .next_after(Utc::now(), tz)
                        .ok_or(ScheduleError::Invalid("invalid_cron"))?;
                }
                schedule.enabled = enabled;
                schedule.revision += 1;
                sqlx::query(
                    "update schedules set enabled=$2,revision=$3,next_fire_at=$4,
                        updated_at=clock_timestamp() where id=$1",
                )
                .bind(id)
                .bind(enabled)
                .bind(schedule.revision)
                .bind(schedule.next_fire_at)
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
    base_revision: i64,
    correlation_id: Uuid,
) -> Result<Schedule, ScheduleError> {
    let value = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "delete_schedule",
        &json!({"id": id, "base_revision": base_revision}),
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
                if schedule.revision != base_revision {
                    return Err(ScheduleError::StaleRevision);
                }
                // Порядок приёмки: строка расписания, затем замок владельца и копии.
                crate::conversations::lock_owner(&mut *conn, owner_id).await?;
                crate::routines::on_schedule_deleted_in_tx(
                    &mut *conn,
                    owner_id,
                    id,
                    correlation_id,
                )
                .await?;
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
    before: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<ScheduleRun>, sqlx::Error> {
    let (before_at, before_id) = match before {
        Some((at, id)) => (Some(at), Some(id)),
        None => (None, None),
    };
    let rows: Vec<RunRow> = sqlx::query_as(
        "select r.* from schedule_runs r join schedules s on s.id=r.schedule_id
         where r.owner_id=$1 and r.schedule_id=$2
           and ($3::timestamptz is null or $4::uuid is null
                or (r.scheduled_at, r.id) < ($3, $4))
         order by r.scheduled_at desc,r.id desc limit $5",
    )
    .bind(owner_id)
    .bind(schedule_id)
    .bind(before_at)
    .bind(before_id)
    .bind(limit)
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(RunRow::into_run).collect()
}

/// Курсор истории: `scheduled_at` + `id`, без потерь при одинаковых моментах.
pub fn encode_runs_cursor(run: &ScheduleRun) -> String {
    format!(
        "{}:{}",
        run.scheduled_at
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        run.id
    )
}

/// Старый формат (только время) тоже принимается: тогда идём строго раньше момента.
pub fn decode_runs_cursor(cursor: &str) -> Option<(DateTime<Utc>, Uuid)> {
    if let Some((at, id)) = cursor.rsplit_once(':')
        && let (Ok(at), Ok(id)) = (
            chrono::DateTime::parse_from_rfc3339(at),
            Uuid::parse_str(id),
        )
    {
        // Осторожно с `rsplit_once(':')`: у RFC3339 внутри тоже есть двоеточия.
        // Проверяем, что хвост — именно UUID, иначе считаем курсор старым форматом.
        return Some((at.with_timezone(&chrono::Utc), id));
    }
    chrono::DateTime::parse_from_rfc3339(cursor)
        .ok()
        .map(|at| (at.with_timezone(&chrono::Utc), Uuid::nil()))
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

/// Строка резерва ручного запуска (ключ общей идемпотентности пишет вызывающий
/// в той же транзакции, см. `admission::reserve_manual`).
#[allow(clippy::too_many_arguments)]
pub async fn reserve_run(
    conn: &mut PgConnection,
    id: Uuid,
    schedule_id: Uuid,
    owner_id: Uuid,
    scheduled_at: DateTime<Utc>,
    claimed_revision: i64,
    key: &str,
    run_anyway: bool,
    detail: Value,
) -> Result<Option<ScheduleRun>, sqlx::Error> {
    let row: Option<RunRow> = sqlx::query_as(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,run_anyway,attempt_id,lease_at,idempotency_key,detail)
         values($1,$2,$3,$4,$5,$6,true,$7,$8,clock_timestamp(),$9,$10)
         on conflict (owner_id, idempotency_key) where manual do nothing returning *",
    )
    .bind(id)
    .bind(schedule_id)
    .bind(owner_id)
    .bind(scheduled_at)
    .bind(claimed_revision)
    .bind(RunState::Claimed.as_str())
    .bind(run_anyway)
    .bind(Uuid::now_v7())
    .bind(key)
    .bind(detail)
    .fetch_optional(conn)
    .await?;
    row.map(RunRow::into_run).transpose()
}

/// Ответ ручного запуска фиксируется в общем ключе идемпотентности той же
/// транзакцией, что и итоговый переход строки (3): ключ занят с момента
/// заявки, а повтор после потерянного ответа читает итог оттуда, не из
/// журнала запусков, который уборка может обрезать.
pub async fn finalize_manual_key(
    conn: &mut PgConnection,
    owner_id: Uuid,
    run: &ScheduleRun,
) -> Result<(), sqlx::Error> {
    if !run.manual {
        return Ok(());
    }
    let response = serde_json::to_value(run).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "update idempotency_keys set response=$3
          where owner_id=$1 and key=$2 and operation='run_now'
            and response->>'id' = $4",
    )
    .bind(owner_id)
    .bind(&run.idempotency_key)
    .bind(response)
    .bind(run.id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Условный переход (A): только из ожидаемого состояния и с ожидаемой
/// попыткой владения. Вернул `false` — строку уже забрал другой
/// (восстановление или параллельная приёмка): перезаписывать нельзя.
/// `None` как ожидаемая попытка совпадает только с `NULL` (старые строки
/// без владения — их сначала забирает восстановление).
#[allow(clippy::too_many_arguments)]
pub async fn transition_run(
    conn: &mut PgConnection,
    id: Uuid,
    from: RunState,
    attempt: Option<Uuid>,
    state: RunState,
    detail: Value,
    started_at: Option<DateTime<Utc>>,
    delay_ms: Option<i64>,
    session_id: Option<Uuid>,
    input_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "update schedule_runs set state=$3,detail=$4,started_at=coalesce($5,started_at),
            delay_ms=coalesce($6,delay_ms),session_id=coalesce($7,session_id),
            input_id=coalesce($8,input_id),updated_at=clock_timestamp()
          where id=$1 and state=$2
            and ((attempt_id = $9) or (attempt_id is null and $9 is null))",
    )
    .bind(id)
    .bind(from.as_str())
    .bind(state.as_str())
    .bind(detail)
    .bind(started_at)
    .bind(delay_ms)
    .bind(session_id)
    .bind(input_id)
    .bind(attempt)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Заявление, готовое к приёму вне тика.
#[derive(Clone, Debug)]
pub struct ClaimedRun {
    pub run: ScheduleRun,
    pub schedule: Schedule,
}

pub(crate) async fn run_changed(
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
        let attempt = Uuid::now_v7();
        let row: Option<RunRow> = sqlx::query_as(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,
                state,manual,run_anyway,attempt_id,lease_at,idempotency_key,detail)
             values($1,$2,$3,$4,$5,$6,false,false,$7,clock_timestamp(),$8,$9)
             on conflict (schedule_id,scheduled_at) where not manual do nothing returning *",
        )
        .bind(run_id)
        .bind(schedule.id)
        .bind(schedule.owner_id)
        .bind(scheduled_at)
        .bind(schedule.revision)
        .bind(state.as_str())
        .bind(attempt)
        .bind(&key)
        .bind(&detail)
        .fetch_optional(&mut *conn)
        .await?;
        // Срабатывание уже записано (повтор или параллельный тик): строку не
        // дублируем, но курсор всё равно двигаем, иначе тик застрянет на нём.
        let run = match row {
            Some(row) => {
                let run = row.into_run()?;
                run_changed(conn, &run, Uuid::now_v7()).await?;
                Some(run)
            }
            None => None,
        };
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
        if let Some(run) = run {
            claimed.push(ClaimedRun { run, schedule });
        }
    }
    Ok(claimed)
}

/// Уборка: не больше 200 последних запусков на расписание, и только
/// завершённых (кроме запусков с собирающимся отчётом и свежим текстом отчёта). Заявленный или идущий запуск (`claimed`, `prechecking`) за
/// пределом окна остаётся: иначе приёмка теряет владение строкой, а повтор
/// ручного запуска идёт по ответу, которому не на что опереться.
pub async fn prune_runs(conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from schedule_runs where id in (
           select id from (
             select id,state,row_number() over (partition by schedule_id order by scheduled_at desc,id desc) as n
             from schedule_runs
           ) ranked where n > 200 and state not in ('claimed','prechecking')
         )
           -- Отчёт рутины не стирается пределом: собирающийся запуск живёт до финала,
           -- готовый текст — 30 дней от завершения (его убирает `routines::prune`).
           and not exists (select 1 from routine_reports rr
                            where rr.run_id = schedule_runs.id and rr.state = 'pending')
           and not (detail #> '{report,text}' is not null
                    and (detail #>> '{report,completed_at}')::timestamptz
                        > clock_timestamp() - interval '30 days')",
    )
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// Возраст, после которого заявка считается брошенной погибшей приёмкой.
/// Живая приёмка держит `prechecking` не дольше таймаута предпроверки
/// (до 5 минут) плюс dispatch, поэтому 10 минут никого живого не заденут.
pub const RECLAIM_LEASE: TimeDelta = TimeDelta::minutes(10);

/// Запуски, которые прямо сейчас ведёт приёмка этого процесса.
/// Восстановление их пропускает: иначе тик, заставший заявку в ожидании
/// семафора приёма дольше аренды, плодил бы вторую приёмку той же строки (A).
static ADMITTING_RUNS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Сторож реестра приёмок: на drop строка выбывает из реестра.
pub struct AdmittingGuard {
    id: Uuid,
}
impl AdmittingGuard {
    pub fn insert(id: Uuid) -> Self {
        ADMITTING_RUNS
            .lock()
            .expect("admitting runs lock")
            .insert(id);
        Self { id }
    }
}
impl Drop for AdmittingGuard {
    fn drop(&mut self) {
        ADMITTING_RUNS
            .lock()
            .expect("admitting runs lock")
            .remove(&self.id);
    }
}

/// Ведёт ли этот процесс приёмку строки прямо сейчас.
pub fn is_admitting(id: &Uuid) -> bool {
    ADMITTING_RUNS
        .lock()
        .expect("admitting runs lock")
        .contains(id)
}

fn admitting_ids() -> Vec<Uuid> {
    ADMITTING_RUNS
        .lock()
        .expect("admitting runs lock")
        .iter()
        .copied()
        .collect()
}

/// Возврат застрявших заявок после гибели приёмки или перезапуска хаба:
/// `claimed` отдаются на повторный приём (со сменой владельца), оборванные
/// `prechecking` завершаются как `failed` с `precheck_result_unknown` и никогда
/// не перезапускаются (ответ воркера уже не придёт).
/// Каждый кандидат идёт в своей короткой транзакции (2): счётчик событий
/// владельца отпускается до замка следующего расписания, поэтому удаление
/// другого расписания не сходится с восстановлением в цикл ожидания, а сбой
/// одного кандидата не откатывает остальных. Порядок замков внутри
/// транзакции — расписание, затем запуск, как у удаления.
/// Возраст аренды меряется от последней смены владельца (`lease_at`),
/// а не от создания заявки. Строки приёмок этого процесса исключаются
/// до предела пачки (5): иначе 32 охраняемых строки на малых id вытесняли
/// бы брошенную заявку позже в очереди. Заблокированные расписания
/// пропускаются в самом выборе (`skip locked`) до предела пачки: 32 заявки под
/// одним замком не вытесняют остальные.
/// `max_age=None` — только для стартового прохода под синглтоном (C);
/// вне синглтона так звать нельзя: второй хаб положил бы живую предпроверку.
pub async fn recover_interrupted(
    pool: &PgPool,
    max_age: Option<TimeDelta>,
    now: DateTime<Utc>,
) -> Result<Vec<ClaimedRun>, sqlx::Error> {
    let cutoff: Option<DateTime<Utc>> = max_age.map(|age| now - age);
    let guarded = admitting_ids();
    let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select r.id,r.schedule_id from schedule_runs r
           join schedules s on s.id = r.schedule_id
          where r.state in ('claimed','prechecking')
            and ($1::timestamptz is null
                 or coalesce(r.lease_at, r.updated_at) < $1)
            and not (r.id = any($2))
          order by r.schedule_id,r.updated_at,r.id limit 32
          for update of s skip locked",
    )
    .bind(cutoff)
    .bind(&guarded)
    .fetch_all(pool)
    .await?;
    let mut claimed = Vec::new();
    for (run_id, schedule_id) in candidates {
        match recover_one(pool, run_id, schedule_id, cutoff, now).await {
            Ok(Some(run)) => claimed.push(run),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    kind = "schedule_recover_failed",
                    error = %crate::dberr::describe(&error),
                    "recovery of one run failed; the rest continue"
                );
            }
        }
    }
    Ok(claimed)
}

async fn recover_one(
    pool: &PgPool,
    run_id: Uuid,
    schedule_id: Uuid,
    cutoff: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Option<ClaimedRun>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    // Сначала расписание (B): удаление идёт тем же порядком.
    let schedule_row: Option<ScheduleRow> =
        sqlx::query_as("select * from schedules where id=$1 for update skip locked")
            .bind(schedule_id)
            .fetch_optional(&mut *tx)
            .await?;
    // Заблокированное расписание (или удалённое) пропускаем: одна блокировка
    // не должна задерживать пачку, строку возьмёт следующий проход.
    let Some(schedule_row) = schedule_row else {
        tx.rollback().await?;
        return Ok(None);
    };
    let Ok(schedule) = schedule_row.into_schedule() else {
        tx.rollback().await?;
        return Ok(None);
    };
    if is_admitting(&run_id) {
        tx.rollback().await?;
        return Ok(None);
    }
    let state: Option<String> = sqlx::query_scalar("select state from schedule_runs where id=$1")
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?;
    let result = match state.as_deref() {
        Some("prechecking") => {
            let detail = json!({"reason": "precheck_result_unknown"});
            let updated = sqlx::query(
                "update schedule_runs set state='failed',detail=$2,updated_at=clock_timestamp()
                  where id=$1 and state='prechecking'
                    and ($3::timestamptz is null or coalesce(lease_at, updated_at) < $3)",
            )
            .bind(run_id)
            .bind(&detail)
            .bind(cutoff)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if updated == 0 {
                None
            } else {
                let mut run: ScheduleRun =
                    sqlx::query_as::<_, RunRow>("select * from schedule_runs where id=$1")
                        .bind(run_id)
                        .fetch_one(&mut *tx)
                        .await?
                        .into_run()?;
                run.state = RunState::Failed;
                run.detail = detail.clone();
                run_changed(&mut tx, &run, Uuid::now_v7()).await?;
                // Итог ручного запуска пишется в ключ идемпотентности той же
                // транзакцией (3).
                finalize_manual_key(&mut tx, schedule.owner_id, &run).await?;
                // Оборванная предпроверка — тоже несработавший запуск (15):
                // пуш встаёт в той же транзакции восстановления.
                crate::notify::enqueue_schedule_failure_in_tx(
                    &mut tx,
                    schedule.id,
                    &schedule.name,
                    "precheck_result_unknown",
                )
                .await?;
                None
            }
        }
        Some("claimed") => {
            let attempt = Uuid::now_v7();
            let row: Option<RunRow> = sqlx::query_as(
                "update schedule_runs
                    set attempt_id=$2,lease_at=clock_timestamp(),updated_at=clock_timestamp()
                  where id=$1 and state='claimed'
                    and ($3::timestamptz is null or coalesce(lease_at, updated_at) < $3)
                  returning *",
            )
            .bind(run_id)
            .bind(attempt)
            .bind(cutoff)
            .fetch_optional(&mut *tx)
            .await?;
            match row.map(RunRow::into_run).transpose()? {
                // Окно catch-up проверялось при заявке. Заявка, пережившая долгий простой
                // хаба, его уже покинула: владелец просил позже не запускать, поэтому она
                // записывается пропущенной. Ручной запуск окна не имеет.
                Some(mut run)
                    if !run.manual
                        && now - run.scheduled_at > effective_window(schedule.catch_up_minutes) =>
                {
                    let mut detail = run.detail.clone();
                    if !detail.is_object() {
                        detail = json!({});
                    }
                    detail["reason"] = json!("catch_up_expired");
                    sqlx::query(
                        "update schedule_runs set state='missed',detail=$2,
                            updated_at=clock_timestamp() where id=$1",
                    )
                    .bind(run_id)
                    .bind(&detail)
                    .execute(&mut *tx)
                    .await?;
                    run.state = RunState::Missed;
                    run.detail = detail;
                    run_changed(&mut tx, &run, Uuid::now_v7()).await?;
                    None
                }
                Some(run) => Some(ClaimedRun { run, schedule }),
                None => None,
            }
        }
        _ => None,
    };
    tx.commit().await?;
    Ok(result)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_precheck_distinguishes_missing_null_and_string() {
        let base = r#"{"base_revision": 1}"#;
        let patch: PatchSchedule = serde_json::from_str(base).unwrap();
        assert!(patch.precheck.is_none(), "missing → None");

        let null = r#"{"base_revision": 1, "precheck": null}"#;
        let patch: PatchSchedule = serde_json::from_str(null).unwrap();
        assert_eq!(patch.precheck, Some(None), "null → Some(None)");

        let value = r#"{"base_revision": 1, "precheck": "true"}"#;
        let patch: PatchSchedule = serde_json::from_str(value).unwrap();
        assert_eq!(
            patch.precheck,
            Some(Some("true".to_owned())),
            "string → Some(Some)"
        );
    }

    #[test]
    fn runs_cursor_round_trips_and_accepts_legacy_time_only() {
        // Время с миллисекундной точностью: старый формат (миллисекунды)
        // обязан давать то же значение, новый — бит-в-бит.
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-18T05:48:30.368Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let run = ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: Uuid::now_v7(),
            scheduled_at: at,
            claimed_revision: 1,
            state: RunState::Claimed,
            created_at: at,
            started_at: None,
            delay_ms: None,
            session_id: None,
            input_id: None,
            manual: false,
            run_anyway: false,
            attempt_id: None,
            idempotency_key: String::new(),
            detail: json!({}),
        };
        let cursor = encode_runs_cursor(&run);
        let (at, id) = decode_runs_cursor(&cursor).expect("new cursor decodes");
        assert_eq!(id, run.id);
        assert_eq!(at, run.scheduled_at);

        let legacy = run
            .scheduled_at
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (at, _) = decode_runs_cursor(&legacy).expect("legacy cursor decodes");
        assert_eq!(at, run.scheduled_at);
    }
}
