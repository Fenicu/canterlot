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
    config::SchedulerConfig,
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    dispatcher::Dispatcher,
    events::{self, NewEvent, Scope},
    idempotency,
    singleton::Singleton,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Message,
    Wake,
    LimitCheck,
    LimitRule,
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
    AttachmentsUnavailable,
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
            other => f.write_str(match other {
                Self::NotFound => "NotFound",
                Self::NotPending => "NotPending",
                Self::TimeOutOfRange => "TimeOutOfRange",
                Self::TooManyPending => "TooManyPending",
                Self::InvalidContent(_) => "InvalidContent",
                Self::AttachmentsUnavailable => "AttachmentsUnavailable",
                Self::IdempotencyConflict => "IdempotencyConflict",
                Self::Db(_) => unreachable!(),
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
fn validate_content(content: &[ContentBlock]) -> Result<(), ScheduleError> {
    crate::delivery::submit::validate_scheduled_content(content)
        .map_err(|_| ScheduleError::InvalidContent("invalid_content"))
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: Uuid,
    kind: String,
    owner_id: Uuid,
    conversation_id: Option<Uuid>,
    run_at: DateTime<Utc>,
    payload: Value,
    state: String,
    attempts: i32,
    result: Option<Value>,
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
        validate_time(req.run_at,now)?; validate_content(&req.content)?;
        if !req.attachment_ids.is_empty(){return Err(ScheduleError::AttachmentsUnavailable);}
        let pending:i64=sqlx::query_scalar("select count(*) from scheduled_jobs where conversation_id=$1 and kind='message' and state='pending'").bind(conversation_id).fetch_one(&mut *conn).await?;
        if pending>=config.max_pending_per_conversation as i64{return Err(ScheduleError::TooManyPending);}
        let job:JobRow=sqlx::query_as("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'message',$2,$3,$4,$5) returning *")
            .bind(Uuid::now_v7()).bind(owner_id).bind(conversation_id).bind(req.run_at).bind(json!({"content":req.content,"attachment_ids":req.attachment_ids})).fetch_one(&mut *conn).await?;
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
            if let Some(content)=patch.content {validate_content(&content)?;job.payload["content"]=json!(content);}
            sqlx::query("update scheduled_jobs set run_at=$2,payload=$3,updated_at=clock_timestamp() where id=$1").bind(id).bind(job.run_at).bind(&job.payload).execute(&mut *conn).await?;
        } else {
            job.state="cancelled".into();
            sqlx::query("update scheduled_jobs set state='cancelled',updated_at=clock_timestamp() where id=$1").bind(id).execute(&mut *conn).await?;
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
    let jobs:Vec<JobRow>=sqlx::query_as("select j.* from scheduled_jobs j join conversations c on c.id=j.conversation_id where c.owner_id=$1 and j.owner_id=$1 and j.conversation_id=$2 and j.kind='message' order by (j.state='pending') desc,j.run_at,j.id limit 200").bind(owner_id).bind(conversation_id).fetch_all(conn).await?;
    jobs.iter().map(JobRow::message).collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub fired: usize,
    pub retried: usize,
    pub failed: usize,
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

#[derive(Clone)]
pub struct Scheduler {
    pool: PgPool,
    dispatcher: Dispatcher,
    service: Arc<Service>,
    history_days: u32,
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
            history_days: crate::config::LimitsConfig::default().history_days,
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
        }
    }
    pub fn with_history_days(mut self, days: u32) -> Self {
        self.history_days = days;
        self
    }
    pub fn notify(&self) {
        self.service.notify.notify_one();
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> Result<TickReport, sqlx::Error> {
        let mut report = TickReport::default();
        loop {
            let mut tx = self.pool.begin().await?;
            let job:Option<JobRow>=sqlx::query_as("select * from scheduled_jobs where kind='message' and state='pending' and run_at<=$1 order by run_at,id for update skip locked limit 1").bind(now).fetch_optional(&mut *tx).await?;
            let Some(job) = job else {
                tx.commit().await?;
                break;
            };
            debug_assert_eq!(job.kind, "message");
            #[cfg(feature = "fault-injection")]
            self.faults
                .pause(crate::fault_injection::Point::SchedulerBeforeSubmit)
                .await;
            let message = job.message()?;
            let key = format!("sched:{}", job.id);
            let existing: Option<Uuid> = sqlx::query_scalar(
                "select id from inputs where owner_id=$1 and idempotency_key=$2",
            )
            .bind(job.owner_id)
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
            let result = if let Some(input_id) = existing {
                Ok(input_id)
            } else {
                submit_input_in_tx(
                    &mut tx,
                    &SubmitInput {
                        owner_id: job.owner_id,
                        conversation_id: message.conversation_id,
                        idempotency_key: key.clone(),
                        content: message.content.clone(),
                        received_at: job.run_at,
                        correlation_id: Uuid::now_v7(),
                    },
                )
                .await
                .map(|s| s.input_id)
            };
            match result {
                Ok(input_id) => {
                    self.finish(&mut tx, job, "fired", json!({"input_id":input_id}))
                        .await?;
                    tx.commit().await?;
                    self.dispatcher.trigger(message.conversation_id);
                    report.fired += 1;
                }
                Err(SubmitError::QueueFull { .. }) => {
                    tx.rollback().await?;
                    // После отката блокировка задания берётся заново, до любых блокировок очереди.
                    let mut retry = self.pool.begin().await?;
                    let current:Option<JobRow>=sqlx::query_as("select * from scheduled_jobs where id=$1 and state='pending' and attempts=$2 and run_at=$3 for update skip locked").bind(job.id).bind(job.attempts).bind(job.run_at).fetch_optional(&mut *retry).await?;
                    if let Some(mut current) = current {
                        current.attempts += 1;
                        if current.attempts as u32 >= self.service.config.max_attempts {
                            self.finish(
                                &mut retry,
                                current,
                                "failed",
                                json!({"error":"queue_full"}),
                            )
                            .await?;
                            report.failed += 1;
                        } else {
                            current.run_at = now
                                + TimeDelta::from_std(self.service.config.retry_queue_full)
                                    .map_err(|_| {
                                        sqlx::Error::InvalidArgument(
                                            "retry duration out of range".into(),
                                        )
                                    })?;
                            let updated:JobRow=sqlx::query_as("update scheduled_jobs set attempts=$2,run_at=$3,updated_at=clock_timestamp() where id=$1 returning *").bind(current.id).bind(current.attempts).bind(current.run_at).fetch_one(&mut *retry).await?;
                            changed(&mut retry, &updated.message()?, Uuid::now_v7()).await?;
                            report.retried += 1;
                        }
                    }
                    retry.commit().await?;
                }
                Err(
                    error @ (SubmitError::ConversationClosed | SubmitError::ConversationNotFound),
                ) => {
                    let reason = if matches!(error, SubmitError::ConversationNotFound) {
                        "conversation_not_found"
                    } else {
                        "conversation_closed"
                    };
                    self.finish(&mut tx, job, "failed", json!({"error":reason}))
                        .await?;
                    tx.commit().await?;
                    report.failed += 1;
                }
                Err(SubmitError::KeyRaceLost) => {
                    tx.rollback().await?;
                    let mut retry = self.pool.begin().await?;
                    let current:Option<JobRow>=sqlx::query_as("select * from scheduled_jobs where id=$1 and state='pending' for update skip locked").bind(job.id).fetch_optional(&mut *retry).await?;
                    if let Some(current) = current {
                        let input_id: Uuid = sqlx::query_scalar(
                            "select id from inputs where owner_id=$1 and idempotency_key=$2",
                        )
                        .bind(current.owner_id)
                        .bind(&key)
                        .fetch_one(&mut *retry)
                        .await?;
                        self.finish(&mut retry, current, "fired", json!({"input_id":input_id}))
                            .await?;
                        report.fired += 1;
                    }
                    retry.commit().await?;
                    self.dispatcher.trigger(message.conversation_id);
                }
                Err(SubmitError::Db(e)) => {
                    tx.rollback().await?;
                    return Err(e);
                }
                Err(_) => {
                    tx.rollback().await?;
                    return Err(sqlx::Error::Protocol("scheduled submit rejected".into()));
                }
            }
        }
        report.next_due = sqlx::query_scalar(
            "select min(run_at) from scheduled_jobs where kind='message' and state='pending'",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(report)
    }

    async fn finish(
        &self,
        conn: &mut PgConnection,
        job: JobRow,
        state: &str,
        result: Value,
    ) -> Result<(), sqlx::Error> {
        let updated:JobRow=sqlx::query_as("update scheduled_jobs set state=$2,attempts=$3,result=$4,updated_at=clock_timestamp(),fired_at=case when $2='fired' then clock_timestamp() else null end where id=$1 returning *")
            .bind(job.id).bind(state).bind(job.attempts).bind(result).fetch_one(&mut *conn).await?;
        changed(conn, &updated.message()?, Uuid::now_v7()).await
    }

    pub async fn maintenance(&self) -> Result<(), sqlx::Error> {
        events::purge_older_than(&self.pool, Duration::from_secs(7 * 86400)).await?;
        sqlx::query("delete from limit_snapshots where observed_at < clock_timestamp() - $1 * interval '1 day'").bind(self.history_days as f64).execute(&self.pool).await?;
        sqlx::query("delete from scheduled_jobs where state in ('fired','cancelled') and updated_at < clock_timestamp() - interval '30 days'").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn wait_for_due(&self, next_due: Option<DateTime<Utc>>, now: DateTime<Utc>) {
        let until = next_due
            .map(|at| (at - now).to_std().unwrap_or(Duration::ZERO))
            .unwrap_or(self.service.config.idle_poll)
            .min(self.service.config.idle_poll)
            .max(Duration::from_millis(1));
        tokio::select! {_=self.service.notify.notified()=>{},_=tokio::time::sleep(until)=>{}}
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let options = self.pool.connect_options();
        let mut singleton = loop {
            let acquired = tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>return,result=Singleton::acquire(&options,"scheduler")=>result};
            match acquired {
                Ok(Some(lock)) => break lock,
                Err(e) => log_error(&e),
                _ => {}
            }
            tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>return,_=tokio::time::sleep(self.service.config.idle_poll.max(Duration::from_millis(1)))=>{}}
        };
        let work = async {
            let mut maintenance = tokio::time::interval(Duration::from_secs(3600));
            maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if !singleton.still_held().await {
                    break;
                }
                let next = match self.tick(Utc::now()).await {
                    Ok(r) => r.next_due,
                    Err(e) => {
                        log_error(&e);
                        None
                    }
                };
                tokio::select! {
                    _=self.wait_for_due(next,Utc::now())=>{},
                    _=maintenance.tick()=>if let Err(e)=self.maintenance().await{log_error(&e);},
                }
            }
        };
        tokio::select! {biased;_=crate::dispatcher::wait_for_shutdown(&mut shutdown)=>{},_=work=>{}}
        singleton.release().await;
    }
}
fn log_error(e: &sqlx::Error) {
    tracing::error!(kind="scheduler_db_error",error=%crate::dberr::describe(e),"scheduler operation failed");
}
