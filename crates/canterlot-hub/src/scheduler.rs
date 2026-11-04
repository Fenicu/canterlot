use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

use crate::{
    config::{LimitsConfig, SchedulerConfig},
    conversations::{SubmitMessage, submit_message_in_tx},
    crunch::{self, JobOutcome},
    delivery::{ContentBlock, SubmitError},
    dispatcher::Dispatcher,
    events::{self, NewEvent, Scope},
    idempotency,
    session::Registry,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Message,
    Wake,
    LimitCheck,
    LimitRule,
    SessionTask,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Fired,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ScheduledMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    #[serde(with = "crate::activity::time")]
    pub run_at: DateTime<Utc>,
    pub content: Vec<ContentBlock>,
    pub attachment_ids: Vec<Uuid>,
    pub state: JobState,
    pub input_id: Option<Uuid>,
    pub attempts: i32,
}
#[derive(Clone, Debug, Serialize)]
pub struct NewScheduled {
    pub conversation_id: Uuid,
    #[serde(with = "crate::activity::time")]
    pub run_at: DateTime<Utc>,
    pub content: Vec<ContentBlock>,
    pub attachment_ids: Vec<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchScheduled {
    pub run_at: Option<DateTime<Utc>>,
    pub content: Option<Vec<ContentBlock>>,
}

pub enum ScheduleError {
    NotFound,
    NotPending,
    TimeOutOfRange,
    TooManyPending,
    InvalidContent(&'static str),
    Attachment(crate::attachments::AttachmentError),
    IdempotencyConflict,
    Db(sqlx::Error),
}
impl fmt::Debug for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(e) => f
                .debug_tuple("Db")
                .field(&crate::dberr::describe(e))
                .finish(),
            Self::Attachment(e) => f.debug_tuple("Attachment").field(e).finish(),
            other => f.write_str(match other {
                Self::NotFound => "NotFound",
                Self::NotPending => "NotPending",
                Self::TimeOutOfRange => "TimeOutOfRange",
                Self::TooManyPending => "TooManyPending",
                Self::InvalidContent(_) => "InvalidContent",
                Self::IdempotencyConflict => "IdempotencyConflict",
                Self::Db(_) | Self::Attachment(_) => unreachable!(),
            }),
        }
    }
}
impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for ScheduleError {}
impl From<sqlx::Error> for ScheduleError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}
impl From<crate::attachments::AttachmentError> for ScheduleError {
    fn from(e: crate::attachments::AttachmentError) -> Self {
        match e {
            crate::attachments::AttachmentError::Db(e) => Self::Db(e),
            other => Self::Attachment(other),
        }
    }
}
impl From<idempotency::IdempotencyConflict> for ScheduleError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

struct Service {
    config: SchedulerConfig,
    notify: Arc<Notify>,
}
static SERVICES: OnceLock<Mutex<HashMap<String, Weak<Service>>>> = OnceLock::new();
fn services() -> &'static Mutex<HashMap<String, Weak<Service>>> {
    SERVICES.get_or_init(Default::default)
}
fn pool_key(pool: &PgPool) -> String {
    let o = pool.connect_options();
    format!(
        "{}:{}:{}",
        o.get_host(),
        o.get_port(),
        o.get_database().unwrap_or("postgres")
    )
}
/// Будит планировщик этого пула после commit, создавшего задание.
pub(crate) fn notify_pool(pool: &PgPool) {
    service(pool).notify.notify_one();
}
fn service(pool: &PgPool) -> Arc<Service> {
    services()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pool_key(pool))
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            Arc::new(Service {
                config: SchedulerConfig::default(),
                notify: Arc::new(Notify::new()),
            })
        })
}

fn validate_key(key: &str) -> Result<(), ScheduleError> {
    if key.is_empty()
        || key.len() > 256
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ScheduleError::InvalidContent("invalid_idempotency_key"));
    }
    Ok(())
}
fn validate_time(run_at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), ScheduleError> {
    if run_at < now + TimeDelta::seconds(1) || run_at > now + TimeDelta::days(366) {
        return Err(ScheduleError::TimeOutOfRange);
    }
    Ok(())
}
fn validate_content(content: &[ContentBlock], has_attachments: bool) -> Result<(), ScheduleError> {
    crate::delivery::submit::validate_message_content(content, has_attachments)
        .map_err(|_| ScheduleError::InvalidContent("invalid_content"))
}

fn validate_refs(
    content: &[ContentBlock],
    refs: &[canterlot_proto::worker::v1::AttachmentRef],
) -> Result<(), ScheduleError> {
    let mut blocks = content.to_vec();
    for reference in refs {
        blocks.push(ContentBlock::from_ref(reference)?);
    }
    crate::delivery::submit::check_bounds(&blocks)
        .map_err(|_| ScheduleError::InvalidContent("invalid_content"))
}

/// Переполненная очередь подписчика не повод потерять итог другой сессии: такое задание
/// ждёт её разбора, пока срок не выйдет, и в счёт отказов не идёт.
const SESSION_TASK_QUEUE_PATIENCE: TimeDelta = TimeDelta::hours(24);

#[derive(Clone, sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    kind: String,
    owner_id: Uuid,
    conversation_id: Option<Uuid>,
    run_at: DateTime<Utc>,
    payload: Value,
    state: String,
    attempts: i32,
    failures: i32,
    result: Option<Value>,
    /// Начало терпения переполненной очереди: первая заблокированная попытка.
    blocked_since: Option<DateTime<Utc>>,
}
impl JobRow {
    fn message(&self) -> Result<ScheduledMessage, sqlx::Error> {
        let content = serde_json::from_value(self.payload["content"].clone())
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let attachment_ids = serde_json::from_value(self.payload["attachment_ids"].clone())
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let state = match self.state.as_str() {
            "pending" => JobState::Pending,
            "fired" => JobState::Fired,
            "cancelled" => JobState::Cancelled,
            "failed" => JobState::Failed,
            _ => return Err(sqlx::Error::Protocol("invalid job state".into())),
        };
        Ok(ScheduledMessage {
            id: self.id,
            conversation_id: self.conversation_id.ok_or(sqlx::Error::RowNotFound)?,
            run_at: self.run_at,
            content,
            attachment_ids,
            state,
            input_id: self
                .result
                .as_ref()
                .and_then(|r| r["input_id"].as_str())
                .and_then(|s| Uuid::parse_str(s).ok()),
            attempts: self.attempts,
        })
    }
}

async fn changed(
    conn: &mut PgConnection,
    message: &ScheduledMessage,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: message.conversation_id,
            },
            kind: "scheduled_changed",
            conversation_id: Some(message.conversation_id),
            execution_id: None,
            payload: serde_json::to_value(message).map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn replay_locked(
    conn: &mut PgConnection,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    hash: &[u8],
) -> Result<Option<ScheduledMessage>, ScheduleError> {
    match idempotency::lookup(conn, owner_id, key, operation, hash).await? {
        idempotency::Begin::New => Ok(None),
        idempotency::Begin::Conflict => Err(ScheduleError::IdempotencyConflict),
        idempotency::Begin::Replay(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)).into()),
    }
}

pub async fn schedule_message(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    req: NewScheduled,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<ScheduledMessage, ScheduleError> {
    validate_key(key)?;
    let settings = service(pool);
    let config = settings.config.clone();
    let conversation_id = req.conversation_id;
    let body = req.clone();
    let hash = idempotency::request_hash("schedule_message", &body);
    let locked_key = key.to_owned();
    let value=idempotency::run_in_tx(pool,owner_id,key,"schedule_message",&body,Some(conversation_id),move|conn|Box::pin(async move {
        let found:Option<Uuid>=sqlx::query_scalar("select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id where c.id=$1 and c.owner_id=$2 for update of q").bind(conversation_id).bind(owner_id).fetch_optional(&mut *conn).await?;
        if found.is_none(){return Err(ScheduleError::NotFound);}
        if let Some(replay)=replay_locked(conn,owner_id,&locked_key,"schedule_message",&hash).await?{return Ok(replay);}
        validate_time(req.run_at,now)?; validate_content(&req.content,!req.attachment_ids.is_empty())?;
        crate::attachments::check_ids(&req.attachment_ids)?;
        let pending:i64=sqlx::query_scalar("select count(*) from scheduled_jobs where conversation_id=$1 and kind='message' and state='pending'").bind(conversation_id).fetch_one(&mut *conn).await?;
        if pending>=config.max_pending_per_conversation as i64{return Err(ScheduleError::TooManyPending);}
        let job:JobRow=sqlx::query_as("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'message',$2,$3,$4,$5) returning *")
            .bind(Uuid::now_v7()).bind(owner_id).bind(conversation_id).bind(req.run_at).bind(json!({"content":req.content,"attachment_ids":req.attachment_ids})).fetch_one(&mut *conn).await?;
        let refs=crate::attachments::link_scheduled_in_tx(conn,owner_id,conversation_id,job.id,&req.attachment_ids).await?;
        validate_refs(&req.content,&refs)?;
        let message=job.message()?; changed(conn,&message,correlation_id).await?; Ok(message)
    })).await?.value;
    settings.notify.notify_one();
    Ok(value)
}

pub async fn update_message(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    patch: PatchScheduled,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<ScheduledMessage, ScheduleError> {
    mutate(pool, owner_id, id, key, Some(patch), now, correlation_id).await
}
pub async fn cancel_message(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<ScheduledMessage, ScheduleError> {
    mutate(pool, owner_id, id, key, None, Utc::now(), correlation_id).await
}

async fn mutate(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    patch: Option<PatchScheduled>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<ScheduledMessage, ScheduleError> {
    validate_key(key)?;
    let body = json!({"id":id,"patch":patch});
    let operation = if patch.is_some() {
        "update_scheduled"
    } else {
        "cancel_scheduled"
    };
    let hash = idempotency::request_hash(operation, &body);
    let locked_key = key.to_owned();
    let message=idempotency::run_in_tx(pool,owner_id,key,operation,&body,None,move|conn|Box::pin(async move {
        let mut job:JobRow=sqlx::query_as("select * from scheduled_jobs where id=$1 and owner_id=$2 and kind='message' for update").bind(id).bind(owner_id).fetch_optional(&mut *conn).await?.ok_or(ScheduleError::NotFound)?;
        if let Some(replay)=replay_locked(conn,owner_id,&locked_key,operation,&hash).await?{return Ok(replay);}
        if job.state!="pending" {return Err(ScheduleError::NotPending);}
        if let Some(patch)=patch {
            if let Some(at)=patch.run_at {validate_time(at,now)?;job.run_at=at;}
            if let Some(content)=patch.content {
                let has_attachments=job.payload["attachment_ids"].as_array().is_some_and(|ids|!ids.is_empty());
                validate_content(&content,has_attachments)?;job.payload["content"]=json!(content);
            }
            let message=job.message()?;
            let refs=crate::attachments::link_scheduled_in_tx(conn,owner_id,message.conversation_id,job.id,&message.attachment_ids).await?;
            validate_refs(&message.content,&refs)?;
            sqlx::query("update scheduled_jobs set run_at=$2,payload=$3,updated_at=clock_timestamp() where id=$1").bind(id).bind(job.run_at).bind(&job.payload).execute(&mut *conn).await?;
        } else {
            job.state="cancelled".into();
            sqlx::query("update scheduled_jobs set state='cancelled',updated_at=clock_timestamp() where id=$1").bind(id).execute(&mut *conn).await?;
            crate::attachments::unlink_scheduled_in_tx(conn,&[id]).await?;
        }
        let message=job.message()?;changed(conn,&message,correlation_id).await?; Ok(message)
    })).await?.value;
    service(pool).notify.notify_one();
    Ok(message)
}

pub async fn list_messages(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<Vec<ScheduledMessage>, sqlx::Error> {
    let jobs:Vec<JobRow>=sqlx::query_as("select j.* from scheduled_jobs j join conversations c on c.id=j.conversation_id where c.owner_id=$1 and j.owner_id=$1 and j.conversation_id=$2 and j.kind='message' order by (j.state='pending') desc,case when j.state='pending' then j.run_at end,case when j.state<>'pending' then j.updated_at end desc,j.id desc limit 200").bind(owner_id).bind(conversation_id).fetch_all(conn).await?;
    jobs.iter().map(JobRow::message).collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub fired: usize,
    pub retried: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub next_due: Option<DateTime<Utc>>,
}
#[derive(Clone)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub owner_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub run_at: DateTime<Utc>,
    pub payload: Value,
    pub attempts: i32,
}
impl fmt::Debug for ClaimedJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimedJob")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

/// Причина отказа в журнале задания и признак того, что повторять бесполезно.
fn submit_failure(error: &SubmitError) -> (&'static str, bool) {
    match error {
        SubmitError::QueueFull { .. } => ("queue_full", false),
        SubmitError::ConversationClosed => ("conversation_closed", true),
        SubmitError::ConversationNotFound => ("conversation_not_found", true),
        SubmitError::ResourceLimit { .. } => ("resource_limit", true),
        SubmitError::InvalidContent { .. } => ("invalid_content", true),
        SubmitError::InvalidRequest { .. } => ("invalid_request", true),
        SubmitError::IdempotencyConflict => ("idempotency_conflict", true),
        SubmitError::KeyRaceLost => ("key_race_lost", false),
        SubmitError::Db(_) => ("database_error", false),
        SubmitError::Attachment(error) => match error {
            crate::attachments::AttachmentError::Db(_) => ("database_error", false),
            crate::attachments::AttachmentError::CapabilityMissing => ("capability_missing", true),
            crate::attachments::AttachmentError::LimitsMismatch => ("limits_mismatch", true),
            crate::attachments::AttachmentError::Unsupported => ("attachment_unsupported", true),
            _ => ("attachment_unavailable", true),
        },
    }
}

#[derive(Clone)]
pub struct Scheduler {
    pool: PgPool,
    dispatcher: Dispatcher,
    service: Arc<Service>,
    history_days: u32,
    registry: Option<Registry>,
    limits: Arc<LimitsConfig>,
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
}
impl Scheduler {
    pub fn new(pool: PgPool, dispatcher: Dispatcher, config: SchedulerConfig) -> Self {
        let mut services = services().lock().unwrap_or_else(|e| e.into_inner());
        services.retain(|_, v| v.strong_count() > 0);
        let key = pool_key(&pool);
        let notify = services
            .get(&key)
            .and_then(Weak::upgrade)
            .map(|s| s.notify.clone())
            .unwrap_or_else(|| Arc::new(Notify::new()));
        let service = Arc::new(Service { config, notify });
        services.insert(key, Arc::downgrade(&service));
        Self {
            pool,
            dispatcher,
            service,
            history_days: LimitsConfig::default().history_days,
            registry: None,
            limits: Arc::new(LimitsConfig::default()),
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
        }
    }
    pub fn with_history_days(mut self, days: u32) -> Self {
        self.history_days = days;
        self
    }
    /// Правила лимитов: опрос и снимки идут через сеансы этого Registry после commit.
    pub fn with_limits(mut self, registry: Registry, limits: LimitsConfig) -> Self {
        self.history_days = limits.history_days;
        self.registry = Some(registry);
        self.limits = Arc::new(limits);
        self
    }
    pub fn notify(&self) {
        self.service.notify.notify_one();
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> Result<TickReport, sqlx::Error> {
        crunch::expire_pending(&self.pool, now).await?;
        let mut report = TickReport::default();
        loop {
            let mut tx = self.pool.begin().await?;
            let job = sqlx::query_as("select * from scheduled_jobs where state='pending' and run_at<=$1 order by run_at,id for update skip locked limit 1").bind(now).fetch_optional(&mut *tx).await;
            let job: Option<JobRow> = match job {
                Ok(job) => job,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            };
            let Some(job) = job else {
                tx.commit().await?;
                break;
            };
            let claimed = job.clone();
            let outcome = if job.kind == "message" {
                self.fire_message(tx, job, now, &mut report).await
            } else {
                self.fire_limit_job(tx, job, now, &mut report).await
            };
            if let Err(error) = outcome {
                log_error(&error);
                let (reason, permanent) = match error {
                    sqlx::Error::Decode(_)
                    | sqlx::Error::ColumnDecode { .. }
                    | sqlx::Error::RowNotFound => ("invalid_payload", true),
                    sqlx::Error::InvalidArgument(_) => ("invalid_job", true),
                    _ => ("job_error", false),
                };
                self.record_failure(&claimed, now, reason, permanent, &mut report)
                    .await?;
            }
        }
        report.next_due =
            sqlx::query_scalar("select min(run_at) from scheduled_jobs where state='pending'")
                .fetch_one(&self.pool)
                .await?;
        Ok(report)
    }

    async fn fire_message(
        &self,
        mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
        job: JobRow,
        now: DateTime<Utc>,
        report: &mut TickReport,
    ) -> Result<(), sqlx::Error> {
        debug_assert_eq!(job.kind, "message");
        #[cfg(feature = "fault-injection")]
        self.faults
            .pause(crate::fault_injection::Point::SchedulerBeforeSubmit)
            .await;
        let message = match job.message() {
            Ok(message) => message,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        let key = format!("sched:{}", job.id);
        let existing =
            sqlx::query_scalar("select id from inputs where owner_id=$1 and idempotency_key=$2")
                .bind(job.owner_id)
                .bind(&key)
                .fetch_optional(&mut *tx)
                .await;
        let existing: Option<Uuid> = match existing {
            Ok(existing) => existing,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        let result = if let Some(input_id) = existing {
            Ok(input_id)
        } else {
            submit_message_in_tx(
                &mut tx,
                &SubmitMessage {
                    owner_id: job.owner_id,
                    conversation_id: message.conversation_id,
                    key: key.clone(),
                    content: message.content.clone(),
                    attachment_ids: message.attachment_ids.clone(),
                    retry_of: None,
                    received_at: job.run_at,
                },
                Uuid::now_v7(),
            )
            .await
            .map(|s| s.input_id)
        };
        match result {
            Ok(input_id) => {
                if let Err(error) = self
                    .finish(&mut tx, job, "fired", json!({"input_id":input_id}))
                    .await
                {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
                tx.commit().await?;
                self.dispatcher.trigger(message.conversation_id);
                report.fired += 1;
            }
            Err(error) => {
                let (reason, permanent) = submit_failure(&error);
                if let SubmitError::Db(error) = &error {
                    log_error(error);
                }
                tx.rollback().await?;
                self.record_failure(&job, now, reason, permanent, report)
                    .await?;
            }
        }
        Ok(())
    }

    async fn record_failure(
        &self,
        job: &JobRow,
        now: DateTime<Utc>,
        reason: &'static str,
        permanent: bool,
        report: &mut TickReport,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let current = sqlx::query_as("select * from scheduled_jobs where id=$1 and state='pending' and attempts=$2 and run_at=$3 and failures=$4 for update")
            .bind(job.id).bind(job.attempts).bind(job.run_at).bind(job.failures).fetch_optional(&mut *tx).await;
        let current: Option<JobRow> = match current {
            Ok(current) => current,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
        };
        if let Some(mut current) = current {
            let waits_for_queue = current.kind == "session_task" && reason == "queue_full";
            if waits_for_queue && current.blocked_since.is_none() {
                current.blocked_since = Some(now);
            }
            if !waits_for_queue {
                current.failures = current.failures.saturating_add(1);
            }
            if matches!(current.kind.as_str(), "message" | "session_task") {
                current.attempts = current.attempts.saturating_add(1);
            }
            let release_job = matches!(current.kind.as_str(), "wake" | "limit_check");
            let exhausted = if waits_for_queue {
                current
                    .blocked_since
                    .is_some_and(|since| now - since >= SESSION_TASK_QUEUE_PATIENCE)
            } else {
                !release_job && current.failures as u32 >= self.service.config.max_attempts
            };
            if permanent || exhausted {
                if current.kind == "message" {
                    if let Err(error) = self
                        .finish(&mut tx, current, "failed", json!({"error":reason}))
                        .await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                } else {
                    if let Err(error) =
                        sqlx::query("update scheduled_jobs set failures=$2,attempts=$3,blocked_since=$4 where id=$1")
                            .bind(current.id)
                            .bind(current.failures)
                            .bind(current.attempts)
                            .bind(current.blocked_since)
                            .execute(&mut *tx)
                            .await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                    if let Err(error) =
                        finish_limit(&mut tx, current.id, "failed", json!({"error":reason})).await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                    // Запрос и уборка закрываются вместе с заданием, иначе они зависли бы навсегда.
                    if current.kind == "session_task"
                        && let Err(error) = crate::session_tools::jobs::abandon_in_tx(
                            &mut tx,
                            current.owner_id,
                            current.conversation_id,
                            &current.payload,
                            now,
                        )
                        .await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                }
                report.failed += 1;
            } else {
                let multiplier = if reason == "queue_full" {
                    1
                } else {
                    1u32 << (current.failures - 1).min(10)
                };
                let mut delay = self
                    .service
                    .config
                    .retry_queue_full
                    .saturating_mul(multiplier)
                    .max(Duration::from_secs(1));
                if release_job {
                    delay = delay.min(Duration::from_secs(60));
                }
                let at = match TimeDelta::from_std(delay)
                    .map_err(|_| sqlx::Error::InvalidArgument("retry duration out of range".into()))
                {
                    Ok(at) => now + at,
                    Err(error) => {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                };
                let updated = sqlx::query_as("update scheduled_jobs set attempts=$2,run_at=$3,result=$4,failures=$5,blocked_since=$6,updated_at=clock_timestamp() where id=$1 returning *")
                    .bind(current.id).bind(current.attempts).bind(at).bind(json!({"error":reason})).bind(current.failures).bind(current.blocked_since).fetch_one(&mut *tx).await;
                let updated: JobRow = match updated {
                    Ok(updated) => updated,
                    Err(error) => {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                };
                if updated.kind == "message" {
                    let message = match updated.message() {
                        Ok(message) => message,
                        Err(error) => {
                            let _ = tx.rollback().await;
                            return Err(error);
                        }
                    };
                    if let Err(error) = changed(&mut tx, &message, Uuid::now_v7()).await {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                }
                report.retried += 1;
            }
        }
        tx.commit().await
    }

    async fn finish(
        &self,
        conn: &mut PgConnection,
        job: JobRow,
        state: &str,
        result: Value,
    ) -> Result<(), sqlx::Error> {
        if job.kind == "message" && job.message().is_err() {
            sqlx::query("update scheduled_jobs set payload='{\"content\":[],\"attachment_ids\":[]}' where id=$1").bind(job.id).execute(&mut *conn).await?;
        }
        let updated:JobRow=sqlx::query_as("update scheduled_jobs set state=$2,attempts=$3,result=$4,failures=$5,blocked_since=case when $2='fired' then null else blocked_since end,updated_at=clock_timestamp(),fired_at=case when $2='fired' then clock_timestamp() else null end where id=$1 returning *")
            .bind(job.id).bind(state).bind(job.attempts).bind(result).bind(job.failures).fetch_one(&mut *conn).await?;
        // После отправки ссылку держит ввод; после отказа файлы отдаются сборщику.
        crate::attachments::unlink_scheduled_in_tx(conn, &[job.id]).await?;
        if updated.conversation_id.is_some() {
            changed(conn, &updated.message()?, Uuid::now_v7()).await?;
        }
        Ok(())
    }

    /// Задания лимитов: обработчик и завершение в одной транзакции, опрос и снимок после commit.
    async fn fire_limit_job(
        &self,
        mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
        job: JobRow,
        now: DateTime<Utc>,
        report: &mut TickReport,
    ) -> Result<(), sqlx::Error> {
        let kind = match job.kind.as_str() {
            "wake" => JobKind::Wake,
            "limit_check" => JobKind::LimitCheck,
            "limit_rule" => JobKind::LimitRule,
            "session_task" => JobKind::SessionTask,
            _ => {
                if let Err(error) = finish_limit(
                    &mut tx,
                    job.id,
                    "cancelled",
                    json!({"reason":"unknown_kind"}),
                )
                .await
                {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
                tx.commit().await?;
                report.cancelled += 1;
                return Ok(());
            }
        };
        let claimed = ClaimedJob {
            id: job.id,
            kind,
            owner_id: job.owner_id,
            conversation_id: job.conversation_id,
            run_at: job.run_at,
            payload: job.payload.clone(),
            attempts: job.attempts,
        };
        #[cfg(feature = "fault-injection")]
        if kind == JobKind::Wake {
            self.faults
                .pause(crate::fault_injection::Point::WakeBeforeSubmit)
                .await;
        }
        let provider = job.payload["provider"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let correlation = Uuid::now_v7();
        let mut left = Vec::new();
        let mut reserve_left = false;
        let outcome = match kind {
            JobKind::Wake => {
                crunch::handle_wake_in_tx(&mut tx, &claimed, &self.limits, now, correlation).await
            }
            JobKind::LimitCheck => {
                crunch::handle_limit_check_in_tx(&mut tx, &claimed, &self.limits, now, correlation)
                    .await
                    .inspect(|outcome| {
                        if let JobOutcome::Done(result) = outcome
                            && let Some(ids) = result.get("left")
                        {
                            left = serde_json::from_value(ids.clone()).unwrap_or_default();
                            reserve_left = true;
                        }
                    })
            }
            JobKind::LimitRule => crunch::apply_weekly_rule_in_tx(
                &mut tx,
                job.owner_id,
                &provider,
                &self.limits,
                now,
                correlation,
            )
            .await
            .and_then(|effect| {
                left = effect.left.clone();
                serde_json::to_value(&effect)
                    .map(JobOutcome::Done)
                    .map_err(|e| sqlx::Error::Encode(Box::new(e)))
            }),
            JobKind::SessionTask => {
                match crate::session_tools::jobs::run_in_tx(
                    &mut tx,
                    &claimed,
                    now,
                    correlation,
                    self.registry.as_ref(),
                )
                .await
                {
                    Ok(outcome) => Ok(outcome),
                    Err(SubmitError::Db(error)) => Err(error),
                    Err(error) => {
                        tx.rollback().await?;
                        let (reason, permanent) = submit_failure(&error);
                        self.record_failure(&job, now, reason, permanent, report)
                            .await?;
                        return Ok(());
                    }
                }
            }
            JobKind::Message => unreachable!("message jobs are fired separately"),
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        match outcome {
            JobOutcome::Done(result) => {
                if let Err(error) = finish_limit(&mut tx, job.id, "fired", result).await {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
                tx.commit().await?;
                report.fired += 1;
                match kind {
                    JobKind::Wake | JobKind::SessionTask => {
                        if let Some(id) = job.conversation_id {
                            self.dispatcher.trigger(id);
                        }
                    }
                    JobKind::LimitRule | JobKind::LimitCheck
                        if kind == JobKind::LimitRule || reserve_left =>
                    {
                        for id in left {
                            self.dispatcher.trigger(id);
                        }
                        if let Some(registry) = &self.registry
                            && let Err(e) =
                                crate::limits::push_limits_snapshot(&self.pool, registry, &provider)
                                    .await
                        {
                            log_error(&e);
                        }
                    }
                    _ => {}
                }
            }
            JobOutcome::Cancelled(reason) => {
                if reason == "invalid_payload" {
                    if let Err(error) =
                        sqlx::query("update scheduled_jobs set failures=$2 where id=$1")
                            .bind(job.id)
                            .bind(job.failures.saturating_add(1))
                            .execute(&mut *tx)
                            .await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                    if let Err(error) =
                        finish_limit(&mut tx, job.id, "failed", json!({"error":reason})).await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                    report.failed += 1;
                } else {
                    if let Err(error) =
                        finish_limit(&mut tx, job.id, "cancelled", json!({"reason":reason})).await
                    {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                    report.cancelled += 1;
                }
                tx.commit().await?;
            }
            JobOutcome::Retry { at, poll } => {
                // Повтор не раньше следующей секунды: цикл тика не крутит одно задание.
                let at = at.max(now + TimeDelta::seconds(1));
                if let Err(error) = sqlx::query("update scheduled_jobs set run_at=$2,attempts=$3,updated_at=clock_timestamp() where id=$1")
                    .bind(job.id).bind(at).bind(job.attempts.saturating_add(i32::from(poll))).execute(&mut *tx).await {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
                tx.commit().await?;
                report.retried += 1;
                if poll && let Some(registry) = &self.registry {
                    let interval = crunch::poll_interval(&self.limits, &provider, job.attempts);
                    if let Err(e) =
                        crunch::request_poll(&self.pool, registry, &provider, interval, now).await
                    {
                        log_error(&e);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn maintenance(&self) -> Result<(), sqlx::Error> {
        let results = [
            events::purge_older_than(&self.pool, Duration::from_secs(7 * 86400)).await.map(|_| ()),
            sqlx::query("delete from limit_snapshots where observed_at < clock_timestamp() - $1 * interval '1 day'").bind(self.history_days as f64).execute(&self.pool).await.map(|_| ()),
            sqlx::query("delete from scheduled_jobs where state in ('fired','cancelled','failed') and updated_at < clock_timestamp() - interval '30 days'").execute(&self.pool).await.map(|_| ()),
            sqlx::query("delete from alarms where updated_at < clock_timestamp() - interval '30 days' and (state in ('cancelled','expired','failed') or (state='fired' and delivered_input_id is not null))").execute(&self.pool).await.map(|_| ()),
            sqlx::query("delete from idempotency_keys where created_at < clock_timestamp() - interval '7 days'").execute(&self.pool).await.map(|_| ()),
        ];
        let mut first = None;
        for result in results {
            if let Err(error) = result {
                log_error(&error);
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    pub async fn wait_for_due(&self, next_due: Option<DateTime<Utc>>, now: DateTime<Utc>) {
        let until = next_due
            .map(|at| (at - now).to_std().unwrap_or(Duration::ZERO))
            .unwrap_or(self.service.config.idle_poll)
            .min(self.service.config.idle_poll)
            .max(Duration::from_millis(1));
        tokio::select! {_=self.service.notify.notified()=>{},_=tokio::time::sleep(until)=>{}}
    }

    pub async fn run(self, shutdown: watch::Receiver<bool>) {
        crate::singleton::supervise(&self.pool, "scheduler", shutdown, || async {
            loop {
                let next = match self.tick(Utc::now()).await {
                    Ok(r) => r.next_due,
                    Err(e) => {
                        log_error(&e);
                        None
                    }
                };
                self.wait_for_due(next, Utc::now()).await;
            }
        })
        .await;
    }

    /// Срок хранения и уборка идут своей службой: долгий проход по журналу событий
    /// не задерживает тик планировщика.
    pub async fn run_maintenance(self, shutdown: watch::Receiver<bool>) {
        crate::singleton::supervise(&self.pool, "maintenance", shutdown, || async {
            // Дельты блоков, закрытых до того, как закрытие стало их удалять.
            match crate::blocks::purge_sealed_chunks(&self.pool, 200).await {
                Ok(0) => {}
                Ok(blocks) => tracing::info!(
                    kind = "sealed_chunks_purged",
                    blocks,
                    "leftover block chunks removed"
                ),
                Err(e) => log_error(&e),
            }
            let mut maintenance = tokio::time::interval(Duration::from_secs(3600));
            maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                maintenance.tick().await;
                if let Err(e) = self.maintenance().await {
                    log_error(&e);
                }
            }
        })
        .await;
    }
}
fn log_error(e: &sqlx::Error) {
    tracing::error!(kind="scheduler_db_error",error=%crate::dberr::describe(e),"scheduler operation failed");
}

/// Внутренние задания лимитов не публикуют `scheduled_changed`: это не сообщения владельца.
async fn finish_limit(
    conn: &mut PgConnection,
    id: Uuid,
    state: &str,
    result: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("update scheduled_jobs set state=$2,result=$3,blocked_since=case when $2='fired' then null else blocked_since end,updated_at=clock_timestamp(),fired_at=case when $2='fired' then clock_timestamp() else null end where id=$1")
        .bind(id).bind(state).bind(result).execute(conn).await?;
    Ok(())
}

pub(crate) async fn publish_archive_cancellations(
    conn: &mut PgConnection,
    ids: &[Uuid],
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let jobs = sqlx::query_as::<_, JobRow>(
        "select * from scheduled_jobs where id=any($1) and kind='message' order by id",
    )
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    for job in jobs {
        changed(conn, &job.message()?, correlation_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Терпение очереди считается с первой заблокированной попытки, а не с создания
    /// задания: давно созданное событие не сгорает на первом `queue_full`.
    #[sqlx::test(migrations = "./migrations")]
    async fn queue_full_patience_runs_from_the_first_block(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let seed = crate::testkit::seed(&pool).await;
        let id = uuid::Uuid::now_v7();
        let now = chrono::Utc::now();
        let created_at = now - chrono::TimeDelta::days(2);
        sqlx::query(
            "insert into scheduled_jobs (id, owner_id, conversation_id, kind, run_at, payload, attempts, failures, created_at)
             values ($1, $2, $3, 'session_task', $4, $5, 0, 0, $6)",
        )
        .bind(id)
        .bind(seed.owner_id)
        .bind(seed.conversation_id)
        .bind(now - chrono::TimeDelta::seconds(1))
        .bind(serde_json::json!({"action": "event"}))
        .bind(created_at)
        .execute(&pool)
        .await?;
        let scheduler = super::Scheduler::new(
            pool.clone(),
            crate::dispatcher::Dispatcher::channel().0,
            crate::config::SchedulerConfig::default(),
        );
        let mut report = super::TickReport::default();
        let attempt: super::JobRow = sqlx::query_as("select * from scheduled_jobs where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
        scheduler
            .record_failure(&attempt, now, "queue_full", false, &mut report)
            .await?;
        let (state, blocked_since, failures): (String, Option<chrono::DateTime<chrono::Utc>>, i32) =
            sqlx::query_as("select state,blocked_since,failures from scheduled_jobs where id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(state, "pending", "первый blocked не сжигает задание");
        assert_eq!(failures, 0, "queue_full не расходует отказы");
        let blocked_since = blocked_since.expect("first block records its start");
        assert!(
            (now - blocked_since).num_seconds().abs() < 5,
            "blocked_since is the attempt time: {blocked_since}"
        );

        let attempt: super::JobRow = sqlx::query_as("select * from scheduled_jobs where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
        let mut report = super::TickReport::default();
        scheduler
            .record_failure(
                &attempt,
                blocked_since + chrono::TimeDelta::hours(24),
                "queue_full",
                false,
                &mut report,
            )
            .await?;
        let state: String = sqlx::query_scalar("select state from scheduled_jobs where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(state, "failed", "сутки терпения от первого blocked истекли");
        Ok(())
    }

    /// Повтор с ядовитой нагрузкой: `record_failure` возвращает ошибку и синхронно
    /// откатывает транзакцию, не оставляя блокировки строки задания.
    #[sqlx::test(migrations = "./migrations")]
    async fn record_failure_retry_rolls_back_row_lock(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let seed = crate::testkit::seed(&pool).await;
        let job = super::JobRow {
            id: uuid::Uuid::now_v7(),
            kind: "message".to_string(),
            owner_id: seed.owner_id,
            conversation_id: Some(seed.conversation_id),
            run_at: chrono::Utc::now() - chrono::TimeDelta::seconds(10),
            payload: serde_json::json!({"content": 42, "attachment_ids": []}),
            state: "pending".to_string(),
            attempts: 0,
            failures: 0,
            result: None,
            blocked_since: None,
        };
        sqlx::query(
            "insert into scheduled_jobs (id, owner_id, conversation_id, kind, run_at, payload, attempts, failures)
             values ($1, $2, $3, 'message', $4, $5, 0, 0)",
        )
        .bind(job.id)
        .bind(seed.owner_id)
        .bind(seed.conversation_id)
        .bind(job.run_at)
        .bind(&job.payload)
        .execute(&pool)
        .await?;
        let scheduler = super::Scheduler::new(
            pool.clone(),
            crate::dispatcher::Dispatcher::channel().0,
            crate::config::SchedulerConfig::default(),
        );
        let mut probe = pool.acquire().await?;
        for i in 0..50 {
            let id = uuid::Uuid::now_v7();
            sqlx::query(
                "insert into scheduled_jobs (id, owner_id, conversation_id, kind, run_at, payload, attempts, failures)
                 values ($1, $2, $3, 'message', $4, $5, 0, 0)",
            )
            .bind(id)
            .bind(seed.owner_id)
            .bind(seed.conversation_id)
            .bind(job.run_at)
            .bind(&job.payload)
            .execute(&pool)
            .await?;
            let attempt = super::JobRow { id, ..job.clone() };
            let mut report = super::TickReport::default();
            let result = scheduler
                .record_failure(
                    &attempt,
                    chrono::Utc::now(),
                    "job_error",
                    false,
                    &mut report,
                )
                .await;
            assert!(result.is_err(), "iteration {i}: poison job must fail");
            let row: Option<uuid::Uuid> = sqlx::query_scalar(
                "select id from scheduled_jobs where id=$1 for update skip locked",
            )
            .bind(id)
            .fetch_optional(&mut *probe)
            .await?;
            assert!(
                row.is_some(),
                "scheduled job skipped: record_failure did not roll back synchronously"
            );
        }
        Ok(())
    }
}
