//! Очередь запусков (§5.4 + §10.7): элемент хранит проверенный запрос, пока запуск
//! упирается в часовую частоту, параллельность, лимит провайдера или страж машины.
//!
//! Задание `session_task` с действием `spawn_dequeue` запускает элемент тем же путём,
//! что и одобренный запуск. Очередь FIFO по готовности: элемент, которому мешает только
//! он сам (своя машина, свой лимит), не держит следующие элементы того же родителя.
//!
//! Порядок блокировок как у `session_spawn`: сначала владелец (`conversations::lock_owner`),
//! потом очередь родителя. Поэтому подсчёт частоты, занятости и вставка элемента не
//! расходятся с параллельным запуском.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, guard, origin, routing, spawn, subs,
    whitelist,
};
use crate::{
    config::{GuardsConfig, SessionToolsConfig, SpawnQueueConfig},
    conversations,
    crunch::JobOutcome,
    delivery::SubmitError,
    events::{self, NewEvent, Scope},
    scheduler::ClaimedJob,
    session::Registry,
};

const HINT_QUEUED: &str = "The spawn is queued; a [canterlot] message tells you when the session starts or fails. Do not retry; carry on or end your turn.";
pub(crate) const REASON_RATE: &str = "rate";
pub(crate) const REASON_RESOURCES: &str = "resources";
const STATE_QUEUED: &str = "queued";
const STATE_STARTED: &str = "started";
const STATE_CANCELLED: &str = "cancelled";
const STATE_FAILED: &str = "failed";
const STATE_EXPIRED: &str = "expired";

/// Проверенные аргументы запуска и глубина хода родителя на момент постановки.
#[derive(Serialize, Deserialize)]
pub(crate) struct QueuedSpawn {
    pub(crate) args: spawn::SpawnArgs,
    pub(crate) chain_depth: i32,
    /// Разрешённая пара прямого запуска: нужна только для карточки очереди.
    #[serde(default)]
    pub(crate) resolved_harness: Option<String>,
    #[serde(default)]
    pub(crate) resolved_model: Option<String>,
}

#[derive(Clone, sqlx::FromRow)]
pub struct QueueRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub parent_id: Uuid,
    pub call_id: Uuid,
    pub spec: Value,
    pub reason: String,
    pub detail: Value,
    pub state: String,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,
    pub child_id: Option<Uuid>,
    pub result: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Элемент очереди для PWA, снимка и события.
#[derive(Debug, Serialize, ToSchema)]
pub struct QueueItemView {
    pub id: Uuid,
    pub parent_id: Uuid,
    /// Запрошенный заголовок будущей сессии; `null` — автоназвание.
    pub title: Option<String>,
    /// Тип задачи маршрута; `null` у прямого запуска.
    pub task_type: Option<String>,
    pub level: Option<String>,
    /// Пара прямого запуска; у маршрута появляется только после старта.
    pub harness: Option<String>,
    pub model: Option<String>,
    pub reason: String,
    pub detail: Value,
    pub state: String,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub attempts: i32,
    pub child_id: Option<Uuid>,
    pub result: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub position: i64,
}

impl QueueItemView {
    fn of(row: &QueueRow, position: i64) -> Self {
        let queued = serde_json::from_value::<QueuedSpawn>(row.spec.clone()).ok();
        let (title, task_type, level, harness, model) = match queued {
            Some(queued) => {
                let title = queued.args.title.clone();
                match queued.args.route.as_ref() {
                    Some(route) => (
                        title,
                        Some(route.task_type.clone()),
                        route.level.clone(),
                        None,
                        None,
                    ),
                    None => (
                        title,
                        None,
                        None,
                        queued
                            .resolved_harness
                            .clone()
                            .or_else(|| queued.args.harness.clone()),
                        queued
                            .resolved_model
                            .clone()
                            .or_else(|| queued.args.model.clone()),
                    ),
                }
            }
            None => (None, None, None, None, None),
        };
        Self {
            id: row.id,
            parent_id: row.parent_id,
            title,
            task_type,
            level,
            harness,
            model,
            reason: row.reason.clone(),
            detail: row.detail.clone(),
            state: row.state.clone(),
            not_before: row.not_before,
            expires_at: row.expires_at,
            attempts: row.attempts,
            child_id: row.child_id,
            result: row.result.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            position,
        }
    }
}

/// Код отказа «Запустить сейчас». Замкнутое перечисление: `details.spawn_refusal.code`
/// не превращается в свободный текст. Значения совпадают с кодами инструментов запуска
/// и состояниями элемента очереди.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpawnRefusalCode {
    NotInWhitelist,
    PermissionModeTooWide,
    MachineUnavailable,
    CapabilityMissing,
    NoExecutorAvailable,
    InvalidArguments,
    HarnessUnknown,
    ModelUnknown,
    EffortNotAllowed,
    WorkdirNotAllowed,
    ProfileUnknown,
    ConversationClosed,
    QueueFull,
    RateLimited,
    MachineBusy,
    ParentStopped,
    ParentArchived,
    Cancelled,
    Expired,
    JobFailed,
    InternalError,
    Unknown,
}

impl SpawnRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInWhitelist => "not_in_whitelist",
            Self::PermissionModeTooWide => "permission_mode_too_wide",
            Self::MachineUnavailable => "machine_unavailable",
            Self::CapabilityMissing => "capability_missing",
            Self::NoExecutorAvailable => "no_executor_available",
            Self::InvalidArguments => "invalid_arguments",
            Self::HarnessUnknown => "harness_unknown",
            Self::ModelUnknown => "model_unknown",
            Self::EffortNotAllowed => "effort_not_allowed",
            Self::WorkdirNotAllowed => "workdir_not_allowed",
            Self::ProfileUnknown => "profile_unknown",
            Self::ConversationClosed => "conversation_closed",
            Self::QueueFull => "queue_full",
            Self::RateLimited => "rate_limited",
            Self::MachineBusy => "machine_busy",
            Self::ParentStopped => "parent_stopped",
            Self::ParentArchived => "parent_archived",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::JobFailed => "job_failed",
            Self::InternalError => "internal_error",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::NotInWhitelist,
            Self::PermissionModeTooWide,
            Self::MachineUnavailable,
            Self::CapabilityMissing,
            Self::NoExecutorAvailable,
            Self::InvalidArguments,
            Self::HarnessUnknown,
            Self::ModelUnknown,
            Self::EffortNotAllowed,
            Self::WorkdirNotAllowed,
            Self::ProfileUnknown,
            Self::ConversationClosed,
            Self::QueueFull,
            Self::RateLimited,
            Self::MachineBusy,
            Self::ParentStopped,
            Self::ParentArchived,
            Self::Cancelled,
            Self::Expired,
            Self::JobFailed,
            Self::InternalError,
            Self::Unknown,
        ]
        .into_iter()
        .find(|code| code.as_str() == value)
    }

    fn message(self) -> &'static str {
        match self {
            Self::NotInWhitelist => "The harness and model are not in the owner's spawn whitelist",
            Self::PermissionModeTooWide => "The permission mode is wider than the parent session's",
            Self::MachineUnavailable => "The machine is not available",
            Self::CapabilityMissing => "The worker does not support this yet",
            Self::NoExecutorAvailable => "No executor available for this task type",
            Self::InvalidArguments => "Invalid spawn arguments",
            Self::HarnessUnknown => "Unknown harness",
            Self::ModelUnknown => "Unknown model",
            Self::EffortNotAllowed => "This effort is not available for the model",
            Self::WorkdirNotAllowed => "This working directory is not allowed",
            Self::ProfileUnknown => "Unknown instruction preset",
            Self::ConversationClosed => "The session is closed",
            Self::QueueFull => "Too many spawns are already queued for this session",
            Self::RateLimited => "Too many requests, try later",
            Self::MachineBusy => "The machine is too busy for a new session",
            Self::ParentStopped => "The parent session stopped",
            Self::ParentArchived => "The parent session was archived",
            Self::Cancelled => "The queued spawn was cancelled",
            Self::Expired => "The queued spawn expired",
            Self::JobFailed => "The spawn job failed",
            Self::InternalError => "The hub could not complete the call, try later",
            Self::Unknown => "The queued spawn could not be started",
        }
    }
}

/// Структурный отказ «Запустить сейчас»: код, сообщение и состояние элемента.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpawnRefusal {
    pub code: SpawnRefusalCode,
    pub message: String,
    /// Состояние элемента после отказа: `queued`, `failed`, `cancelled` или `expired`.
    pub state: String,
    /// Когда элемент снова попробуют запустить (`queued`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub retry_at: Option<DateTime<Utc>>,
}

impl SpawnRefusal {
    fn new(code: SpawnRefusalCode, message: impl Into<String>, state: &str) -> Self {
        Self {
            code,
            message: message.into(),
            state: state.to_owned(),
            retry_at: None,
        }
    }
}

/// Итог «Запустить сейчас»: обычный исход всегда коммитится, чтобы переход
/// `failed`/`expired` не откатывался вместе с ответом.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct StartNowOutcome {
    pub status: String,
    pub queue_id: Uuid,
    pub session_id: Option<Uuid>,
    pub title: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub refusal: Option<SpawnRefusal>,
}

fn existing_outcome(row: &QueueRow) -> StartNowOutcome {
    let refusal = (row.state != STATE_STARTED).then(|| {
        let wire = row
            .result
            .as_ref()
            .and_then(|value| value.get("code").or_else(|| value.get("reason")))
            .and_then(Value::as_str);
        let code = wire
            .and_then(SpawnRefusalCode::parse)
            .unwrap_or(SpawnRefusalCode::Unknown);
        SpawnRefusal::new(code, code.message(), &row.state)
    });
    StartNowOutcome {
        status: row.state.clone(),
        queue_id: row.id,
        session_id: row.child_id,
        title: None,
        harness: None,
        model: None,
        refusal,
    }
}

/// Что нужно очереди от хаба: реестр, настройки и маршрут.
pub struct Runtime<'a> {
    pub registry: Option<&'a Registry>,
    pub session_tools: &'a SessionToolsConfig,
    pub spawn_queue: &'a SpawnQueueConfig,
    pub guards: &'a GuardsConfig,
    pub routing_enabled: bool,
    pub routing_stale: std::time::Duration,
    pub routing_queue_horizon: std::time::Duration,
}

impl Runtime<'_> {
    fn spawn_context(&self) -> spawn::SpawnContext<'_> {
        spawn::SpawnContext {
            registry: self.registry.expect("checked before validation"),
            routing_enabled: self.routing_enabled,
            routing_stale: self.routing_stale,
        }
    }
}

/// Что поставить в очередь: уже проверенный запрос и найденная причина.
pub struct Enqueue {
    pub(crate) owner_id: Uuid,
    pub(crate) parent_id: Uuid,
    pub(crate) call_id: Uuid,
    pub(crate) args: spawn::SpawnArgs,
    pub(crate) chain_depth: i32,
    pub(crate) reason: &'static str,
    pub(crate) detail: Value,
    pub(crate) not_before: DateTime<Utc>,
    /// Разрешённая пара прямого запуска, если она уже известна.
    pub(crate) resolved: Option<(String, String)>,
}

fn time(at: DateTime<Utc>) -> String {
    at.format("%H:%M UTC").to_string()
}

fn answer_queued(row: &QueueRow, position: i64) -> Value {
    json!({
        "status": "queued",
        "queue_id": row.id,
        "position": position,
        "reason": row.reason,
        "detail": row.detail.get("detail").and_then(Value::as_str).unwrap_or_default(),
        "not_before": row.not_before,
        "expires_at": row.expires_at,
        "hint": HINT_QUEUED,
    })
}

fn answer_existing(row: &QueueRow) -> Value {
    match row.state.as_str() {
        STATE_STARTED => json!({
            "status": "started",
            "session_id": row.child_id,
            "queue_id": row.id,
            "hint": spawn::HINT_STARTED,
        }),
        STATE_CANCELLED => json!({"status": "cancelled", "queue_id": row.id}),
        STATE_FAILED | STATE_EXPIRED => json!({
            "status": row.state,
            "queue_id": row.id,
            "reason": row.result,
        }),
        _ => answer_queued(row, 0),
    }
}

/// Повтор вызова транспортом: элемент уже создан этим `call_id`.
pub async fn find_by_call(
    conn: &mut PgConnection,
    owner_id: Uuid,
    call_id: Uuid,
) -> Result<Option<QueueRow>, sqlx::Error> {
    sqlx::query_as("select * from spawn_queue where owner_id=$1 and call_id=$2")
        .bind(owner_id)
        .bind(call_id)
        .fetch_optional(conn)
        .await
}

pub async fn answer_replay(conn: &mut PgConnection, row: &QueueRow) -> Result<Value, sqlx::Error> {
    if row.state == STATE_QUEUED {
        let position = position_of(conn, row).await?;
        return Ok(answer_queued(row, position));
    }
    Ok(answer_existing(row))
}

async fn position_of(conn: &mut PgConnection, row: &QueueRow) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from spawn_queue
          where parent_id=$1 and state='queued'
            and (created_at, id) <= ($2, $3)",
    )
    .bind(row.parent_id)
    .bind(row.created_at)
    .bind(row.id)
    .fetch_one(conn)
    .await
}

async fn waiting_count(conn: &mut PgConnection, parent: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select count(*) from spawn_queue where parent_id=$1 and state='queued'")
        .bind(parent)
        .fetch_one(conn)
        .await
}

/// Событие очереди в области разговора родителя.
async fn changed_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
    view: &QueueItemView,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation { id: parent },
            kind: "spawn_queue_changed",
            conversation_id: Some(parent),
            execution_id: None,
            payload: serde_json::to_value(view).unwrap_or(Value::Null),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Очередь родителя в снимке и HTTP: только ожидающие и недавно завершённые.
pub async fn snapshot_rows(
    conn: &mut PgConnection,
    parent: Uuid,
) -> Result<Vec<QueueRow>, sqlx::Error> {
    sqlx::query_as(
        "select * from spawn_queue
          where parent_id=$1 and state in ('queued','starting','started','failed','expired','cancelled')
          order by (state='queued') desc, created_at, id
          limit 50",
    )
    .bind(parent)
    .fetch_all(conn)
    .await
}

pub async fn snapshot_views(
    conn: &mut PgConnection,
    parent: Uuid,
) -> Result<Vec<QueueItemView>, sqlx::Error> {
    let rows = snapshot_rows(conn, parent).await?;
    let mut position = 0;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        if row.state == STATE_QUEUED {
            position += 1;
        }
        out.push(QueueItemView::of(row, position));
    }
    Ok(out)
}

/// Заметка родителю отдельным заданием: очередь не ждёт его ввода.
async fn note_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent: Uuid,
    text: String,
    depth: i32,
    event: origin::HubEvent,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    subs::insert_job(
        conn,
        owner_id,
        parent,
        now,
        &spawn::note_job(text, origin::note_origin(depth), event),
    )
    .await?;
    Ok(())
}

fn queue_event(row: &QueueRow) -> origin::HubEvent {
    origin::HubEvent {
        kind: "spawn_queue".to_owned(),
        state: Some(row.state.clone()),
        child_id: row.child_id,
        detail: row
            .result
            .as_ref()
            .and_then(|value| value.get("code"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..origin::HubEvent::default()
    }
}

fn spec_depth(spec: &Value) -> i32 {
    spec.get("chain_depth").and_then(Value::as_i64).unwrap_or(0) as i32
}

/// Создаёт элемент очереди и задание на его срок. Под блокировкой владельца.
pub async fn enqueue_in_tx(
    conn: &mut PgConnection,
    runtime: &Runtime<'_>,
    request: Enqueue,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<Value, ToolError> {
    let waiting = waiting_count(conn, request.parent_id).await?;
    if waiting >= runtime.spawn_queue.max_waiting {
        return Err(ToolError::with_message(
            ToolErrorCode::SpawnQueueFull,
            format!(
                "{} spawns are already waiting for this session; cancel one with spawn_cancel or wait",
                waiting
            ),
        ));
    }
    let id = Uuid::now_v7();
    let expires_at =
        now + TimeDelta::from_std(runtime.spawn_queue.ttl).unwrap_or_else(|_| TimeDelta::days(1));
    let (resolved_harness, resolved_model) = match request.resolved {
        Some((harness, model)) => (Some(harness), Some(model)),
        None => (None, None),
    };
    let spec = serde_json::to_value(QueuedSpawn {
        args: request.args,
        chain_depth: request.chain_depth,
        resolved_harness,
        resolved_model,
    })
    .map_err(|_| ToolError::new(ToolErrorCode::Internal))?;
    let mut tx = conn.begin().await?; // db::begin: savepoint
    let row: QueueRow = sqlx::query_as(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,detail,not_before,expires_at)
         values($1,$2,$3,$4,$5,$6,$7,$8,$9) returning *",
    )
    .bind(id)
    .bind(request.owner_id)
    .bind(request.parent_id)
    .bind(request.call_id)
    .bind(&spec)
    .bind(request.reason)
    .bind(&request.detail)
    .bind(request.not_before)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    let position = waiting + 1;
    changed_in_tx(
        &mut tx,
        request.parent_id,
        &QueueItemView::of(&row, position),
        correlation_id,
    )
    .await?;
    subs::insert_job(
        &mut tx,
        request.owner_id,
        request.parent_id,
        request.not_before,
        &json!({"action": "spawn_dequeue", "queue_id": row.id}),
    )
    .await?;
    tx.commit().await?;
    Ok(answer_queued(&row, position))
}

/// Задание на ближайший готовый элемент; текущее задание не считается.
/// Родитель остановлен владельцем или родителем: очередь не растёт и не запускается.
pub async fn parent_stopped_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
) -> Result<bool, sqlx::Error> {
    let stopped: Option<bool> = sqlx::query_scalar(
        "select pause_reasons @> '[{\"reason\":\"user_stop\"}]'::jsonb
           from queue_state where conversation_id=$1",
    )
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(stopped.unwrap_or(false))
}

pub async fn wake_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent: Uuid,
    now: DateTime<Utc>,
    exclude_job: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let next: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select min(not_before) from spawn_queue
          where parent_id=$1 and state='queued' and expires_at>$2",
    )
    .bind(parent)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    let Some(next) = next else {
        return Ok(());
    };
    let at = next.max(now);
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs
          where conversation_id=$1 and kind='session_task' and state='pending'
            and payload->>'action'='spawn_dequeue' and run_at <= $3
            and id is distinct from $2)",
    )
    .bind(parent)
    .bind(exclude_job)
    .bind(at)
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Ok(());
    }
    // У задания всегда есть durable-идентичность головы: отказ задания знает,
    // какой элемент закрыть, а обход очереди всё равно идёт по готовности.
    let head: Option<Uuid> = sqlx::query_scalar(
        "select id from spawn_queue
          where parent_id=$1 and state='queued' and expires_at>$2
          order by created_at, id limit 1",
    )
    .bind(parent)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    let mut payload = json!({"action": "spawn_dequeue"});
    if let Some(head) = head {
        payload["queue_id"] = json!(head);
    }
    subs::insert_job(conn, owner_id, parent, at, &payload).await?;
    Ok(())
}

async fn set_state_in_tx(
    conn: &mut PgConnection,
    row: &mut QueueRow,
    state: &str,
    result: Option<Value>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    row.state = state.to_owned();
    row.result = result;
    row.updated_at = now;
    sqlx::query(
        "update spawn_queue
            set state=$2,result=$3,attempts=$4,child_id=$5,updated_at=$6,
                reason=$7,detail=$8,not_before=$9
          where id=$1",
    )
    .bind(row.id)
    .bind(&row.state)
    .bind(&row.result)
    .bind(row.attempts)
    .bind(row.child_id)
    .bind(row.updated_at)
    .bind(&row.reason)
    .bind(&row.detail)
    .bind(row.not_before)
    .execute(&mut *conn)
    .await?;
    let position = if row.state == STATE_QUEUED {
        position_of(conn, row).await?
    } else {
        0
    };
    changed_in_tx(
        conn,
        row.parent_id,
        &QueueItemView::of(row, position),
        correlation_id,
    )
    .await?;
    Ok(())
}

/// Отмена ожидающего элемента. Ответ повторяет `spawn_cancel`.
pub async fn cancel_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent: Uuid,
    queue_id: Uuid,
    correlation_id: Uuid,
    now: DateTime<Utc>,
    note: bool,
) -> Result<Value, ToolError> {
    // Порядок блокировок как у удаления: строка разговора раньше элемента очереди.
    sqlx::query("select id from conversations where id=$1 and owner_id=$2 for no key update")
        .bind(parent)
        .bind(owner_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotParent))?;
    let mut row: Option<QueueRow> = sqlx::query_as(
        "select * from spawn_queue where id=$1 and parent_id=$2 and owner_id=$3 for update",
    )
    .bind(queue_id)
    .bind(parent)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(mut row) = row.take() else {
        return Err(ToolError::new(ToolErrorCode::NotParent));
    };
    let answer = match row.state.as_str() {
        STATE_QUEUED => {
            set_state_in_tx(
                conn,
                &mut row,
                STATE_CANCELLED,
                Some(json!({"reason": "cancelled"})),
                now,
                correlation_id,
            )
            .await?;
            wake_in_tx(conn, owner_id, parent, now, None).await?;
            if note {
                note_in_tx(
                    conn,
                    owner_id,
                    parent,
                    format!(
                        "[canterlot] Queued spawn q-… ({}) was cancelled. Nothing was started.",
                        &row.id.to_string()[..8]
                    ),
                    spec_depth(&row.spec),
                    queue_event(&row),
                    now,
                )
                .await?;
            }
            json!({"status": "cancelled", "queue_id": row.id})
        }
        STATE_STARTED => json!({
            "status": "already_started",
            "queue_id": row.id,
            "session_id": row.child_id,
        }),
        STATE_CANCELLED => json!({"status": "already_cancelled", "queue_id": row.id}),
        other => json!({
            "status": other,
            "queue_id": row.id,
            "reason": row.result,
        }),
    };
    Ok(answer)
}

/// Остановка, корзина или приём родителя: ожидающие элементы отменяются.
pub async fn cancel_for_parent_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent: Uuid,
    reason: &'static str,
    correlation_id: Uuid,
    now: DateTime<Utc>,
    note: bool,
) -> Result<usize, sqlx::Error> {
    let mut rows: Vec<QueueRow> = sqlx::query_as(
        "select * from spawn_queue where parent_id=$1 and state='queued' order by created_at,id for update",
    )
    .bind(parent)
    .fetch_all(&mut *conn)
    .await?;
    let count = rows.len();
    for row in &mut rows {
        row.state = STATE_CANCELLED.to_owned();
        row.result = Some(json!({"reason": reason}));
        row.updated_at = now;
        sqlx::query("update spawn_queue set state='cancelled',result=$2,updated_at=$3 where id=$1")
            .bind(row.id)
            .bind(&row.result)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        changed_in_tx(conn, parent, &QueueItemView::of(row, 0), correlation_id).await?;
    }
    if note && count > 0 {
        let text = format!(
            "[canterlot] Your session stopped: {count} queued spawn{} cancelled. Nothing was started.",
            if count == 1 { " was" } else { "s were" },
        );
        let depth = rows.first().map_or(0, |row| spec_depth(&row.spec));
        note_in_tx(
            conn,
            owner_id,
            parent,
            text,
            depth,
            origin::HubEvent {
                kind: "spawn_queue".to_owned(),
                state: Some(STATE_CANCELLED.to_owned()),
                ..origin::HubEvent::default()
            },
            now,
        )
        .await?;
    }
    Ok(count)
}

/// «Запустить сейчас»: снимает частоту, занятость, лимиты и страж, но не белый список
/// и не режим. Элемент запускается в этой же транзакции.
pub async fn start_now_in_tx(
    conn: &mut PgConnection,
    runtime: &Runtime<'_>,
    owner_id: Uuid,
    parent: Uuid,
    queue_id: Uuid,
    correlation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<StartNowOutcome, ToolError> {
    conversations::lock_owner(&mut *conn, owner_id).await?;
    let Some(parent_session) = access::session_in_tx(conn, owner_id, parent).await? else {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    };
    // Очередь раньше строки разговора (README, шаги 6–7): продолжение остановленного
    // родителя (`executions::start_in_tx`) держит очередь и правит ту же строку.
    crate::session_tools::send::lock_queue(conn, parent).await?;
    sqlx::query("select id from conversations where id=$1 and owner_id=$2 for no key update")
        .bind(parent)
        .bind(owner_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    let stopped = parent_stopped_in_tx(&mut *conn, parent).await?;
    let mut row: QueueRow = sqlx::query_as(
        "select * from spawn_queue where id=$1 and parent_id=$2 and owner_id=$3 for update",
    )
    .bind(queue_id)
    .bind(parent)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| ToolError::new(ToolErrorCode::NotParent))?;
    if row.state != STATE_QUEUED {
        return Ok(existing_outcome(&row));
    }
    if stopped {
        row.result = Some(json!({"reason": "parent_stopped"}));
        let result = row.result.clone();
        set_state_in_tx(conn, &mut row, STATE_CANCELLED, result, now, correlation_id).await?;
        return Ok(existing_outcome(&row));
    }
    if row.expires_at <= now {
        row.result = Some(json!({"reason": "expired"}));
        let result = row.result.clone();
        set_state_in_tx(conn, &mut row, STATE_EXPIRED, result, now, correlation_id).await?;
        note_in_tx(
            conn,
            owner_id,
            parent,
            format!(
                "[canterlot] Queued spawn q-{} expired before it could start. Nothing was started.",
                &row.id.to_string()[..8]
            ),
            spec_depth(&row.spec),
            queue_event(&row),
            now,
        )
        .await?;
        wake_in_tx(conn, owner_id, parent, now, None).await?;
        return Ok(existing_outcome(&row));
    }
    let machine = machine_of(conn, parent).await?;
    let caller = Caller {
        owner_id,
        conversation_id: parent,
        execution_id: Uuid::nil(),
        machine_id: machine,
        call_id: row.call_id,
    };
    match try_start(
        conn,
        runtime,
        &caller,
        &parent_session,
        &mut row,
        now,
        correlation_id,
        true,
    )
    .await?
    {
        Attempt::Started(started) => {
            let AttemptStarted { child, spec } = *started;
            Ok(StartNowOutcome {
                status: "started".to_owned(),
                queue_id: row.id,
                session_id: Some(child.session_id),
                title: Some(child.title),
                harness: Some(spec.harness),
                model: Some(spec.model),
                refusal: None,
            })
        }
        Attempt::Blocked { reason, detail, at } => {
            row.reason = reason.to_owned();
            row.detail = json!({"detail": detail.clone()});
            row.not_before = at;
            row.attempts += 1;
            set_state_in_tx(conn, &mut row, STATE_QUEUED, None, now, correlation_id).await?;
            wake_in_tx(conn, owner_id, parent, now, None).await?;
            let code = match reason {
                REASON_RATE => SpawnRefusalCode::RateLimited,
                "concurrency" => SpawnRefusalCode::MachineBusy,
                "limits" => SpawnRefusalCode::NoExecutorAvailable,
                REASON_RESOURCES => SpawnRefusalCode::MachineUnavailable,
                _ => SpawnRefusalCode::Unknown,
            };
            Ok(StartNowOutcome {
                status: "queued".to_owned(),
                queue_id: row.id,
                session_id: None,
                title: None,
                harness: None,
                model: None,
                refusal: Some(SpawnRefusal {
                    code,
                    message: detail,
                    state: STATE_QUEUED.to_owned(),
                    retry_at: Some(at),
                }),
            })
        }
        Attempt::Failed { code, message } => {
            row.result = Some(json!({"code": code}));
            let result = row.result.clone();
            set_state_in_tx(conn, &mut row, STATE_FAILED, result, now, correlation_id).await?;
            note_in_tx(
                conn,
                owner_id,
                parent,
                format!(
                    "[canterlot] Queued spawn q-{} failed: {code}. Nothing was started.",
                    &row.id.to_string()[..8]
                ),
                spec_depth(&row.spec),
                queue_event(&row),
                now,
            )
            .await?;
            wake_in_tx(conn, owner_id, parent, now, None).await?;
            let refusal = SpawnRefusalCode::parse(code).unwrap_or(SpawnRefusalCode::Unknown);
            Ok(StartNowOutcome {
                status: "failed".to_owned(),
                queue_id: row.id,
                session_id: None,
                title: None,
                harness: None,
                model: None,
                refusal: Some(SpawnRefusal::new(refusal, message, STATE_FAILED)),
            })
        }
    }
}

async fn machine_of(conn: &mut PgConnection, parent: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("select machine_id from conversations where id=$1")
        .bind(parent)
        .fetch_one(&mut *conn)
        .await
}

struct AttemptStarted {
    child: spawn::Started,
    spec: spawn::SpawnSpec,
}

enum Attempt {
    Started(Box<AttemptStarted>),
    Blocked {
        reason: &'static str,
        detail: String,
        at: DateTime<Utc>,
    },
    Failed {
        code: &'static str,
        message: String,
    },
}

/// Повторная проверка элемента на текущих данных и запуск.
#[allow(clippy::too_many_arguments)]
async fn try_start(
    conn: &mut PgConnection,
    runtime: &Runtime<'_>,
    caller: &Caller,
    parent: &access::Session,
    row: &mut QueueRow,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    start_now: bool,
) -> Result<Attempt, sqlx::Error> {
    let Some(registry) = runtime.registry else {
        return Ok(Attempt::Blocked {
            reason: "resources",
            detail: "the machine is not connected".to_owned(),
            at: retry_at(runtime.spawn_queue, now),
        });
    };
    // После перезапуска хаба тик может обогнать повторное подключение воркера:
    // отсутствие связи — не постоянная нехватка возможности.
    if registry.session_info(caller.machine_id).is_none() {
        return Ok(Attempt::Blocked {
            reason: "resources",
            detail: "the machine is not connected".to_owned(),
            at: retry_at(runtime.spawn_queue, now),
        });
    }
    let Ok(queued) = serde_json::from_value::<QueuedSpawn>(row.spec.clone()) else {
        return Ok(Attempt::Failed {
            code: "invalid_arguments",
            message: SpawnRefusalCode::InvalidArguments.message().to_owned(),
        });
    };
    let args = queued.args;
    let context = runtime.spawn_context();
    let spec = match args.route.clone() {
        Some(route) => match spawn::validate_routed(
            &mut *conn,
            &context,
            caller,
            parent,
            args.clone(),
            route,
            queued.chain_depth,
            now,
            start_now,
        )
        .await
        {
            Ok((spec, _)) => spec,
            Err(spawn::RouteFailure::Tool(error)) => {
                return Ok(Attempt::Failed {
                    code: error.code.as_str(),
                    message: error.message,
                });
            }
            Err(spawn::RouteFailure::NoExecutor { considered, reset }) => {
                let detail = routing::no_executor_message(&considered, reset);
                match routing::queue_reason(&considered, reset, now, runtime.routing_queue_horizon)
                {
                    Some(reason) if !start_now => {
                        return Ok(Attempt::Blocked {
                            reason,
                            detail,
                            at: retry_at(runtime.spawn_queue, now),
                        });
                    }
                    _ => {
                        return Ok(Attempt::Failed {
                            code: "no_executor_available",
                            message: detail,
                        });
                    }
                }
            }
        },
        None => match spawn::validate(
            &mut *conn,
            &context,
            caller,
            parent,
            args.clone(),
            queued.chain_depth,
        )
        .await
        {
            Ok((spec, _)) => spec,
            Err(error) => {
                return Ok(Attempt::Failed {
                    code: error.code.as_str(),
                    message: error.message,
                });
            }
        },
    };
    let list = whitelist::view(&mut *conn, caller.owner_id).await?;
    let first_model: String = sqlx::query_scalar(
        "select coalesce(models->0->>'id','') from machine_harnesses
          where machine_id=$1 and harness=$2",
    )
    .bind(caller.machine_id)
    .bind(&spec.harness)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or_default();
    if spec.wider_than_parent
        || !whitelist::allows(&list.entries, &spec.harness, &spec.model, &first_model)
    {
        return Ok(Attempt::Failed {
            code: "not_in_whitelist",
            message: SpawnRefusalCode::NotInWhitelist.message().to_owned(),
        });
    }
    let mut metrics_stale = false;
    if !start_now {
        let rate = access::rate_state(&mut *conn, runtime.session_tools, parent.id, now).await?;
        if rate.blocked {
            let at = rate
                .retry_at
                .unwrap_or_else(|| retry_at(runtime.spawn_queue, now));
            if at >= row.expires_at {
                return Ok(Attempt::Failed {
                    code: "rate_limited",
                    message: SpawnRefusalCode::RateLimited.message().to_owned(),
                });
            }
            return Ok(Attempt::Blocked {
                reason: REASON_RATE,
                detail: format!(
                    "{} spawns in the last hour; the next slot opens at {}",
                    rate.count,
                    time(at)
                ),
                at,
            });
        }
        let verdict = guard::check_in_tx(
            &mut *conn,
            caller.owner_id,
            caller.machine_id,
            runtime.guards,
            now,
        )
        .await?;
        metrics_stale = verdict.stale;
        if verdict.blocked() {
            return Ok(Attempt::Blocked {
                reason: REASON_RESOURCES,
                detail: verdict.detail.clone().unwrap_or_default(),
                at: retry_at(runtime.spawn_queue, now),
            });
        }
    }
    let mut attempt = conn.begin().await?; // db::begin: savepoint
    let started = spawn::start_child(
        &mut attempt,
        spawn::Launch {
            registry,
            owner: caller.owner_id,
            machine: caller.machine_id,
            parent,
            spec: &spec,
            key: format!("spawnq:{}", row.id),
            correlation_id,
        },
        false,
    )
    .await;
    match started {
        Ok(child) => {
            attempt.commit().await?;
            row.child_id = Some(child.session_id);
            row.state = STATE_STARTED.to_owned();
            row.result = None;
            row.updated_at = now;
            sqlx::query(
                "update spawn_queue set state='started',child_id=$2,result=null,updated_at=$3 where id=$1",
            )
            .bind(row.id)
            .bind(child.session_id)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            let position = position_of(conn, row).await?;
            changed_in_tx(
                conn,
                row.parent_id,
                &QueueItemView::of(row, position),
                correlation_id,
            )
            .await?;
            subs::insert_job(
                conn,
                caller.owner_id,
                row.parent_id,
                now,
                &json!({"action": "spawn_attributes", "conversation_id": child.session_id}),
            )
            .await?;
            let follow = if spec.subscribe {
                "You are subscribed."
            } else {
                "You are not subscribed."
            };
            let stale = if metrics_stale {
                " Machine metrics are stale."
            } else {
                ""
            };
            note_in_tx(
                conn,
                caller.owner_id,
                row.parent_id,
                format!(
                    "[canterlot] Queued spawn q-{} started as session \"{}\" ({}, {}, model {}). {follow}{stale}{}",
                    &row.id.to_string()[..8],
                    origin::clean_title(&child.title),
                    child.session_id,
                    spec.harness,
                    spec.model,
                    child.plan.as_ref().map(|plan| format!(" {}", plan.note())).unwrap_or_default(),
                ),
                spec.chain_depth,
                origin::HubEvent {
                    kind: "spawn_queue".to_owned(),
                    state: Some(STATE_STARTED.to_owned()),
                    session_id: Some(child.session_id),
                    child_id: Some(child.session_id),
                    ..origin::HubEvent::default()
                },
                now,
            )
            .await?;
            wake_in_tx(conn, caller.owner_id, row.parent_id, now, None).await?;
            Ok(Attempt::Started(Box::new(AttemptStarted { child, spec })))
        }
        Err(error) => {
            attempt.rollback().await?;
            use crate::session_tools::ToolErrorCode as Code;
            let retry = matches!(error.code, Code::MachineUnavailable | Code::Internal);
            if retry {
                Ok(Attempt::Blocked {
                    reason: "resources",
                    detail: error.message,
                    at: retry_at(runtime.spawn_queue, now),
                })
            } else {
                Ok(Attempt::Failed {
                    code: error.code.as_str(),
                    message: error.message,
                })
            }
        }
    }
}

fn retry_at(config: &SpawnQueueConfig, now: DateTime<Utc>) -> DateTime<Utc> {
    now + TimeDelta::from_std(config.retry_delay).unwrap_or_else(|_| TimeDelta::seconds(60))
}

/// Задание `spawn_dequeue`: FIFO по готовности с пропуском занятой головы.
pub(crate) async fn run_job_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent: Uuid,
    runtime: &Runtime<'_>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    conversations::lock_owner(&mut *conn, job.owner_id).await?;
    crate::session_tools::send::lock_queue(conn, parent).await?;
    let Some(parent_session) = access::session_in_tx(conn, job.owner_id, parent).await? else {
        return Ok(JobOutcome::Done(json!({"skipped": "parent_missing"})));
    };
    if parent_stopped_in_tx(&mut *conn, parent).await? {
        cancel_for_parent_in_tx(
            conn,
            job.owner_id,
            parent,
            "parent_stopped",
            correlation_id,
            now,
            false,
        )
        .await?;
        return Ok(JobOutcome::Done(json!({"cancelled": "parent_stopped"})));
    }
    if parent_session.archived {
        cancel_for_parent_in_tx(
            conn,
            job.owner_id,
            parent,
            "parent_archived",
            correlation_id,
            now,
            false,
        )
        .await?;
        return Ok(JobOutcome::Done(json!({"cancelled": true})));
    }
    let machine = machine_of(conn, parent).await?;
    let rows: Vec<QueueRow> = sqlx::query_as(
        "select * from spawn_queue
          where parent_id=$1 and state='queued' and not_before<=$2
          order by created_at,id for update",
    )
    .bind(parent)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let mut started = false;
    let mut blocked = 0;
    for mut row in rows {
        if row.expires_at <= now {
            row.result = Some(json!({"reason": "expired"}));
            expire_in_tx(conn, &mut row, job.owner_id, now, correlation_id).await?;
            continue;
        }
        let caller = Caller {
            owner_id: job.owner_id,
            conversation_id: parent,
            execution_id: Uuid::nil(),
            machine_id: machine,
            call_id: row.call_id,
        };
        match try_start(
            conn,
            runtime,
            &caller,
            &parent_session,
            &mut row,
            now,
            correlation_id,
            false,
        )
        .await?
        {
            Attempt::Started(_) => {
                started = true;
                break;
            }
            Attempt::Blocked { reason, detail, at } => {
                blocked += 1;
                row.reason = reason.to_owned();
                row.detail = json!({"detail": detail});
                row.attempts += 1;
                if at >= row.expires_at {
                    row.result = Some(json!({"reason": "expired"}));
                    expire_in_tx(conn, &mut row, job.owner_id, now, correlation_id).await?;
                    continue;
                }
                row.not_before = at;
                set_state_in_tx(conn, &mut row, STATE_QUEUED, None, now, correlation_id).await?;
            }
            Attempt::Failed { code, .. } => {
                row.result = Some(json!({"code": code}));
                let result = row.result.clone();
                set_state_in_tx(conn, &mut row, STATE_FAILED, result, now, correlation_id).await?;
                note_in_tx(
                    conn,
                    job.owner_id,
                    parent,
                    format!(
                        "[canterlot] Queued spawn q-{} failed: {code}. Nothing was started.",
                        &row.id.to_string()[..8]
                    ),
                    spec_depth(&row.spec),
                    queue_event(&row),
                    now,
                )
                .await?;
            }
        }
    }
    wake_in_tx(conn, job.owner_id, parent, now, Some(job.id)).await?;
    Ok(JobOutcome::Done(json!({
        "started": started,
        "blocked": blocked,
    })))
}

async fn expire_in_tx(
    conn: &mut PgConnection,
    row: &mut QueueRow,
    owner_id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let result = row.result.clone();
    set_state_in_tx(conn, row, STATE_EXPIRED, result, now, correlation_id).await?;
    note_in_tx(
        conn,
        owner_id,
        row.parent_id,
        format!(
            "[canterlot] Queued spawn q-{} expired before it could start ({}). Nothing was started.",
            &row.id.to_string()[..8],
            row.reason
        ),
        spec_depth(&row.spec),
        queue_event(row),
        now,
    )
    .await?;
    Ok(())
}

/// Задание исчерпало попытки: элемент `failed`, следующий становится доступным.
pub async fn abandon_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(owner_id) =
        sqlx::query_scalar::<_, Uuid>("select owner_id from conversations where id=$1")
            .bind(parent)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(());
    };
    let queue_id = payload
        .get("queue_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok());
    if let Some(queue_id) = queue_id {
        let mut row: Option<QueueRow> = sqlx::query_as(
            "select * from spawn_queue where id=$1 and parent_id=$2 and state='queued' for update",
        )
        .bind(queue_id)
        .bind(parent)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(mut row) = row.take() {
            row.result = Some(json!({"code": "job_failed"}));
            let result = row.result.clone();
            set_state_in_tx(conn, &mut row, STATE_FAILED, result, now, Uuid::now_v7()).await?;
            note_in_tx(
                conn,
                owner_id,
                parent,
                format!(
                    "[canterlot] Queued spawn q-{} failed: job_failed. Nothing was started.",
                    &row.id.to_string()[..8]
                ),
                spec_depth(&row.spec),
                queue_event(&row),
                now,
            )
            .await?;
        }
    }
    // Задание без идентичности — обход очереди: даже если закрывать нечего,
    // следующий элемент обязан получить задание.
    wake_in_tx(conn, owner_id, parent, now, None).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Инструмент `spawn_cancel`
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    queue_id: Uuid,
}

pub async fn cancel(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: CancelArgs = super::parse_arguments(arguments)?;
    let mut tx = crate::db::begin(&context.pool).await?;
    conversations::lock_owner(&mut tx, caller.owner_id).await?;
    let answer = cancel_in_tx(
        &mut tx,
        caller.owner_id,
        caller.conversation_id,
        args.queue_id,
        caller.call_id,
        Utc::now(),
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(answer)
}

/// `queued_spawns` в `sessions_list`: число и ближайший срок.
pub async fn waiting_summary(conn: &mut PgConnection, parent: Uuid) -> Result<Value, sqlx::Error> {
    let (count, next): (i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "select count(*), min(not_before) from spawn_queue
          where parent_id=$1 and state='queued'",
    )
    .bind(parent)
    .fetch_one(conn)
    .await?;
    Ok(json!({"count": count, "nearest": next}))
}
