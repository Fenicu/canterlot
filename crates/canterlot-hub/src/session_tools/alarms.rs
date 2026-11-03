//! Будильники: разбудить сессию по времени или по устоявшемуся состоянию другой сессии.
//!
//! Срабатывание не пишет в чужую очередь напрямую: оно ставит задание `session_task`
//! с действием `alarm_deliver`, а доставка забирает все сработавшие будильники получателя
//! пачкой. Заметки и тексты сообщений — данные уровня сообщения: в журнал и `Debug` не попадают.

use std::collections::BTreeMap;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, completion, origin, parse_arguments,
    send, settle, subs,
};
use crate::{
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    events::{self, NewEvent, Scope},
    scheduler::ClaimedJob,
};

/// Сколько сработавших будильников получателя сливаются в одно сообщение.
const COALESCE: TimeDelta = TimeDelta::seconds(5);
const MAX_ARMED: i64 = 20;
const MAX_NOTE_BYTES: usize = 2 * 1024;
const MIN_AT_SECONDS: i64 = 30;
const MAX_AT_SECONDS: i64 = 366 * 86400;
const DEFAULT_DEADLINE_SECONDS: i64 = 86400;
const MIN_DEADLINE_SECONDS: i64 = 60;
const MAX_DEADLINE_SECONDS: i64 = 7 * 86400;

const DEFAULT_STATES: [&str; 5] = ["idle", "waiting_for_user", "failed", "stopped", "deleted"];
const ALL_STATES: [&str; 7] = [
    "idle",
    "waiting_for_user",
    "stalled",
    "quota_paused",
    "failed",
    "stopped",
    "deleted",
];

const HINT_ARMED: &str = "Alarm armed. End your turn; a [canterlot] message wakes you. Do not use sleep commands or polling for this wait.";
const HINT_SATISFIED: &str = "The condition already holds; nothing was armed.";
const AUTOMATION: &str = "This is canterlot automation, not the owner.";

/// Состояние условия `session_state`: отметки целей снимаются при взводе.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionCondition {
    kind: String,
    session_ids: Vec<Uuid>,
    states: Vec<String>,
    #[serde(default)]
    all: bool,
    /// Отпечаток каждой цели на момент взвода. Условие, уже истинное, не взводится.
    #[serde(default)]
    armed: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WhenArgs {
    kind: String,
    session_ids: Option<Vec<Uuid>>,
    states: Option<Vec<String>>,
    all: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    at: Option<String>,
    in_seconds: Option<i64>,
    when: Option<WhenArgs>,
    deadline_seconds: Option<i64>,
    only_on_timeout: Option<bool>,
    batch: Option<bool>,
    note: Option<String>,
    key: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    alarm_id: Option<Uuid>,
    key: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    include_finished: Option<bool>,
}

/// Строка будильника.
#[derive(Clone, sqlx::FromRow)]
pub struct AlarmRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub key: Option<String>,
    pub kind: String,
    pub condition: Option<Value>,
    pub fire_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub only_on_timeout: bool,
    pub batch: bool,
    pub collected: Value,
    pub note: Option<String>,
    pub chain_depth: i32,
    pub state: String,
    pub fired_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub call_id: Uuid,
    pub response: Value,
    pub delivered_input_id: Option<Uuid>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub fired_at: Option<DateTime<Utc>>,
}

/// То, что видят PWA и HTTP: без заметки и без внутренних отметок.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct AlarmView {
    pub alarm_id: Uuid,
    pub conversation_id: Uuid,
    pub key: Option<String>,
    pub kind: String,
    pub condition: String,
    pub fires_at: Option<String>,
    pub deadline_at: Option<String>,
    pub only_on_timeout: bool,
    pub batch: bool,
    pub state: String,
    pub fired_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub fired_at: Option<String>,
    pub note_bytes: i64,
    pub created_at: String,
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn clock(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn coarse(duration: TimeDelta) -> String {
    let minutes = duration.num_minutes().max(0);
    if minutes < 60 {
        format!("{minutes} min")
    } else if minutes < 24 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} d", minutes / (24 * 60))
    }
}

fn name_of(key: Option<&str>) -> String {
    key.filter(|key| !key.is_empty())
        .map(|key| format!("\"{key}\" "))
        .unwrap_or_default()
}

fn states_text(states: &[String]) -> String {
    states.join(", ")
}

fn condition_text(
    condition: &SessionCondition,
    deadline: DateTime<Utc>,
    only_on_timeout: bool,
) -> String {
    let ids = condition
        .session_ids
        .iter()
        .map(Uuid::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let watchdog = if only_on_timeout { "; watchdog" } else { "" };
    format!(
        "sessions {ids} settle in {} (deadline {}){watchdog}",
        states_text(&condition.states),
        clock(deadline)
    )
}

fn parse_condition(when: &WhenArgs) -> Result<(SessionCondition, Vec<String>), ToolError> {
    if when.kind != "session_state" {
        // Остальные виды условий появятся отдельными этапами; притворяться, что они
        // работают, нельзя.
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Only the session_state condition is supported yet",
        ));
    }
    if when.all == Some(true) {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "all is not supported yet",
        ));
    }
    let ids = when.session_ids.clone().unwrap_or_default();
    if ids.len() != 1 {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "session_state needs exactly one session id",
        ));
    }
    let states = when
        .states
        .clone()
        .unwrap_or_else(|| DEFAULT_STATES.iter().map(|s| s.to_string()).collect());
    if states.is_empty()
        || states
            .iter()
            .any(|state| !ALL_STATES.contains(&state.as_str()))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    Ok((
        SessionCondition {
            kind: "session_state".into(),
            session_ids: ids,
            states: states.clone(),
            all: false,
            armed: BTreeMap::new(),
        },
        states,
    ))
}

fn validate_key(key: Option<&str>) -> Result<(), ToolError> {
    if let Some(key) = key
        && (key.is_empty() || key.len() > 64 || key.chars().any(char::is_control))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    Ok(())
}

fn view(row: &AlarmRow) -> AlarmView {
    let condition = row
        .condition
        .as_ref()
        .and_then(|value| serde_json::from_value::<SessionCondition>(value.clone()).ok())
        .and_then(|condition| {
            row.deadline_at
                .map(|deadline| condition_text(&condition, deadline, row.only_on_timeout))
        })
        .unwrap_or_else(|| match row.kind.as_str() {
            "at" => row
                .fire_at
                .map(|at| format!("at {}", clock(at)))
                .unwrap_or_default(),
            _ => String::new(),
        });
    AlarmView {
        alarm_id: row.id,
        conversation_id: row.conversation_id,
        key: row.key.clone(),
        kind: row.kind.clone(),
        condition,
        fires_at: row.fire_at.map(time),
        deadline_at: row.deadline_at.map(time),
        only_on_timeout: row.only_on_timeout,
        batch: row.batch,
        state: row.state.clone(),
        fired_reason: row.fired_reason.clone(),
        failure_reason: row.failure_reason.clone(),
        fired_at: row.fired_at.map(time),
        note_bytes: row.note.as_deref().map_or(0, |note| note.len() as i64),
        created_at: time(row.created_at),
    }
}

async fn changed_in_tx(
    conn: &mut PgConnection,
    row: &AlarmRow,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: row.conversation_id,
            },
            kind: "alarm_changed",
            conversation_id: Some(row.conversation_id),
            execution_id: None,
            payload: serde_json::to_value(view(row)).unwrap_or(Value::Null),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn row_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<AlarmRow>, sqlx::Error> {
    sqlx::query_as("select * from alarms where id=$1")
        .bind(id)
        .fetch_optional(conn)
        .await
}

async fn insert_job_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    run_at: DateTime<Utc>,
    alarm_id: Uuid,
    timeout: bool,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(conversation_id)
    .bind(run_at)
    .bind(json!({"action":"alarm_deliver","alarm_id":alarm_id,"timeout":timeout}))
    .execute(conn)
    .await?;
    Ok(id)
}

async fn cancel_jobs_in_tx(
    conn: &mut PgConnection,
    alarm_id: Uuid,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where kind='session_task' and state='pending'
           and payload->>'action'='alarm_deliver' and payload->>'alarm_id'=$1",
    )
    .bind(alarm_id.to_string())
    .bind(json!({"reason": reason}))
    .execute(conn)
    .await?;
    Ok(())
}

/// Условие сработало раньше срока: задание срока больше не нужно, а задание доставки
/// этим не трогается.
async fn cancel_timeout_job_in_tx(
    conn: &mut PgConnection,
    alarm_id: Uuid,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where kind='session_task' and state='pending'
           and payload->>'action'='alarm_deliver' and payload->>'alarm_id'=$1
           and coalesce((payload->>'timeout')::boolean,false)",
    )
    .bind(alarm_id.to_string())
    .bind(json!({"reason": reason}))
    .execute(conn)
    .await?;
    Ok(())
}

/// Срабатывание условия: `armed -> fired` один раз, задание доставки и событие в той же
/// транзакции. Вызывается из проектора цели под блокировкой строки будильника.
pub async fn evaluate_session_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    activity: &crate::activity::Activity,
    queue: &crate::activity::QueueFacts,
    facts: &settle::SettleFacts,
    config: &settle::SettleConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some(state) = settle::settled(activity, queue, facts, config) else {
        return Ok(false);
    };
    if !completion::is_completion(state) {
        return Ok(false);
    }
    let fingerprint = settle::fingerprint(state, facts.since, facts.last_turn_at, facts.phase_since);
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='session_state'
           and condition->'session_ids' ? $1
         for update skip locked",
    )
    .bind(target_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    let mut fired = false;
    for mut row in rows {
        let Some(mut condition) =
            serde_json::from_value::<SessionCondition>(row.condition.clone().unwrap_or(Value::Null))
                .ok()
        else {
            continue;
        };
        if !condition.states.iter().any(|wanted| wanted == state.as_str()) {
            continue;
        }
        if condition
            .armed
            .get(&target_id.to_string())
            .is_some_and(|seen| seen == &fingerprint)
        {
            continue;
        }
        if row.only_on_timeout {
            // Сторож: условие, наступившее раньше срока, отменяет будильник молча.
            row.state = "cancelled".into();
            row.fired_reason = Some("condition_met".into());
            row.updated_at = now;
            sqlx::query("update alarms set state='cancelled',fired_reason='condition_met',updated_at=$2 where id=$1")
                .bind(row.id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
            changed_in_tx(conn, &row, correlation_id).await?;
            continue;
        }
        condition
            .armed
            .insert(target_id.to_string(), fingerprint.clone());
        row.condition = Some(serde_json::to_value(&condition).unwrap_or(Value::Null));
        row.collected = json!({"state": state.as_str(), "session_id": target_id});
        fire_in_tx(conn, &mut row, "condition", now, now, correlation_id, true).await?;
        fired = true;
    }
    Ok(fired)
}

/// Цель удалена: условие с `deleted` срабатывает, остальные ждут срока.
pub async fn on_target_deleted_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='session_state'
           and condition->'session_ids' ? $1
         for update skip locked",
    )
    .bind(target_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    for mut row in rows {
        let Some(condition) =
            serde_json::from_value::<SessionCondition>(row.condition.clone().unwrap_or(Value::Null))
                .ok()
        else {
            continue;
        };
        if !condition.states.iter().any(|state| state == "deleted") || row.only_on_timeout {
            continue;
        }
        row.collected = json!({"state": "deleted", "session_id": target_id});
        fire_in_tx(conn, &mut row, "condition", now, now, Uuid::now_v7(), true).await?;
    }
    Ok(())
}

/// Переход в `fired`: состояние, причина, задание доставки и событие для PWA.
async fn fire_in_tx(
    conn: &mut PgConnection,
    row: &mut AlarmRow,
    reason: &str,
    fired_at: DateTime<Utc>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    schedule: bool,
) -> Result<(), sqlx::Error> {
    let condition = row.condition.clone();
    sqlx::query(
        "update alarms set state='fired',fired_reason=$2,fired_at=$3,collected=$4,
           updated_at=$5 where id=$1",
    )
    .bind(row.id)
    .bind(reason)
    .bind(fired_at)
    .bind(&row.collected)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    row.state = "fired".into();
    row.fired_reason = Some(reason.into());
    row.fired_at = Some(fired_at);
    row.updated_at = now;
    if condition.is_some() {
        // Условие уже сработало: задание срока больше не нужно.
        cancel_timeout_job_in_tx(conn, row.id, "alarm_settled").await?;
    }
    if schedule {
        insert_job_in_tx(
            conn,
            row.owner_id,
            row.conversation_id,
            now + COALESCE,
            row.id,
            false,
        )
        .await?;
    }
    changed_in_tx(conn, row, correlation_id).await?;
    Ok(())
}

/// `alarm_set`: одна транзакция с очередью вызывающего, пределом и заменой по `key`.
pub async fn set(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SetArgs = parse_arguments(arguments)?;
    if !context.alarms_enabled {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Alarms are disabled on this hub",
        ));
    }
    validate_key(args.key.as_deref())?;
    if args
        .note
        .as_deref()
        .is_some_and(|note| note.len() > MAX_NOTE_BYTES)
    {
        return Err(ToolError::new(ToolErrorCode::TooLarge));
    }
    if args.only_on_timeout == Some(true) || args.batch == Some(true) {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Watchdog and batch alarms are not supported yet",
        ));
    }
    let time_sources = usize::from(args.at.is_some())
        + usize::from(args.in_seconds.is_some())
        + usize::from(args.when.is_some());
    if time_sources != 1 {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }

    let now = Utc::now();
    let mut tx = context.pool.begin().await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    if let Some(stored) = sqlx::query_scalar::<_, Value>(
        "select response from alarms where owner_id=$1 and call_id=$2",
    )
    .bind(caller.owner_id)
    .bind(caller.call_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(stored);
    }
    let waiter = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;

    let replaced = if let Some(key) = args.key.as_deref() {
        let old: Option<Uuid> = sqlx::query_scalar(
            "select id from alarms where conversation_id=$1 and key=$2 and state='armed'
             for update",
        )
        .bind(caller.conversation_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(old) = old {
            cancel_in_tx(&mut tx, old, "replaced", now, caller.call_id).await?;
        }
        old
    } else {
        None
    };
    let armed: i64 =
        sqlx::query_scalar("select count(*) from alarms where conversation_id=$1 and state='armed'")
            .bind(caller.conversation_id)
            .fetch_one(&mut *tx)
            .await?;
    if armed >= MAX_ARMED {
        return Err(ToolError::new(ToolErrorCode::TooManyAlarms));
    }

    let (kind, fire_at, deadline_at, condition, response, run_at, timeout) =
        if let Some(at) = args.at.as_deref() {
            let at = DateTime::parse_from_rfc3339(at)
                .map_err(|_| ToolError::new(ToolErrorCode::InvalidArguments))?
                .with_timezone(&Utc);
            let seconds = (at - now).num_seconds();
            if !(MIN_AT_SECONDS..=MAX_AT_SECONDS).contains(&seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let condition = None;
            let response = json!({
                "state": "armed",
                "alarm_id": Uuid::now_v7(),
                "fires_at": time(at),
                "condition": format!("at {}", clock(at)),
                "replaced": replaced,
                "hint": HINT_ARMED,
            });
            (
                "at",
                Some(at),
                None,
                condition,
                response,
                at + COALESCE,
                false,
            )
        } else if let Some(seconds) = args.in_seconds {
            if !(MIN_AT_SECONDS..=MAX_AT_SECONDS).contains(&seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let at = now + TimeDelta::seconds(seconds);
            let response = json!({
                "state": "armed",
                "alarm_id": Uuid::now_v7(),
                "fires_at": time(at),
                "condition": format!("at {}", clock(at)),
                "replaced": replaced,
                "hint": HINT_ARMED,
            });
            (
                "at",
                Some(at),
                None,
                None,
                response,
                at + COALESCE,
                false,
            )
        } else {
            let when = args
                .when
                .as_ref()
                .ok_or_else(|| ToolError::new(ToolErrorCode::InvalidArguments))?;
            let (mut condition, states) = parse_condition(when)?;
            let deadline_seconds = args.deadline_seconds.unwrap_or(DEFAULT_DEADLINE_SECONDS);
            if !(MIN_DEADLINE_SECONDS..=MAX_DEADLINE_SECONDS).contains(&deadline_seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let deadline = now + TimeDelta::seconds(deadline_seconds);
            let target = condition.session_ids[0];
            if target == caller.conversation_id {
                return Err(ToolError::new(ToolErrorCode::SessionNotFound));
            }
            access::target_in_tx(&mut tx, &context.config, &waiter, caller.owner_id, target).await?;
            if let Some(current) =
                subs::current_in_tx(&mut tx, target, now, &context.settle).await?
            {
                if states.iter().any(|state| state == current.state.as_str()) {
                    // Условие уже истинно: будильник не взводится, но вызов запоминается.
                    let response = json!({
                        "state": "satisfied",
                        "current": current.state.as_str(),
                        "condition": condition_text(&condition, deadline, false),
                        "hint": HINT_SATISFIED,
                    });
                    let id = Uuid::now_v7();
                    insert_alarm_in_tx(
                        &mut tx,
                        id,
                        caller.owner_id,
                        caller.conversation_id,
                        "agent",
                        args.key.as_deref(),
                        "condition",
                        None,
                        Some(deadline),
                        false,
                        false,
                        Some(condition.as_value()),
                        args.note.as_deref(),
                        0,
                        "cancelled",
                        Some("satisfied"),
                        None,
                        caller.call_id,
                        &response,
                        now,
                    )
                    .await?;
                    tx.commit().await?;
                    return Ok(response);
                }
                condition
                    .armed
                    .insert(target.to_string(), current.fingerprint.clone());
            }
            let response = json!({
                "state": "armed",
                "alarm_id": Uuid::now_v7(),
                "deadline_at": time(deadline),
                "condition": condition_text(&condition, deadline, false),
                "replaced": replaced,
                "hint": HINT_ARMED,
            });
            (
                "condition",
                None,
                Some(deadline),
                Some(condition),
                response,
                deadline + COALESCE,
                true,
            )
        };

    let id = response["alarm_id"]
        .as_str()
        .and_then(|value| value.parse::<Uuid>().ok())
        .unwrap_or_else(Uuid::now_v7);
    let chain_depth = access::current_depth(&mut tx, caller.conversation_id).await?;
    insert_alarm_in_tx(
        &mut tx,
        id,
        caller.owner_id,
        caller.conversation_id,
        "agent",
        args.key.as_deref(),
        kind,
        fire_at,
        deadline_at,
        args.only_on_timeout.unwrap_or(false),
        args.batch.unwrap_or(false),
        condition.as_ref().map(SessionCondition::as_value),
        args.note.as_deref(),
        chain_depth,
        "armed",
        None,
        None,
        caller.call_id,
        &response,
        now,
    )
    .await?;
    insert_job_in_tx(
        &mut tx,
        caller.owner_id,
        caller.conversation_id,
        run_at,
        id,
        timeout,
    )
    .await?;
    if let Some(row) = row_in_tx(&mut tx, id).await? {
        changed_in_tx(&mut tx, &row, caller.call_id).await?;
    }
    tx.commit().await?;
    crate::scheduler::notify_pool(&context.pool);
    Ok(response)
}

impl SessionCondition {
    fn as_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_alarm_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    owner_id: Uuid,
    conversation_id: Uuid,
    created_by: &str,
    key: Option<&str>,
    kind: &str,
    fire_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
    only_on_timeout: bool,
    batch: bool,
    condition: Option<Value>,
    note: Option<&str>,
    chain_depth: i32,
    state: &str,
    fired_reason: Option<&str>,
    failure_reason: Option<&str>,
    call_id: Uuid,
    response: &Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,key,kind,condition,fire_at,
           deadline_at,only_on_timeout,batch,note,chain_depth,state,fired_reason,failure_reason,
           call_id,response,created_at,updated_at)
         values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$19)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(conversation_id)
    .bind(created_by)
    .bind(key)
    .bind(kind)
    .bind(condition)
    .bind(fire_at)
    .bind(deadline_at)
    .bind(only_on_timeout)
    .bind(batch)
    .bind(note)
    .bind(chain_depth)
    .bind(state)
    .bind(fired_reason)
    .bind(failure_reason)
    .bind(call_id)
    .bind(response)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

async fn cancel_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let row: Option<AlarmRow> = sqlx::query_as(
        "update alarms set state='cancelled',fired_reason=$2,updated_at=$3
         where id=$1 and state='armed' returning *",
    )
    .bind(id)
    .bind(reason)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    cancel_jobs_in_tx(conn, id, "alarm_cancelled").await?;
    if let Some(row) = row {
        changed_in_tx(conn, &row, correlation_id).await?;
    }
    Ok(())
}

/// `alarm_cancel`: идемпотентно по id или имени.
pub async fn cancel(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: CancelArgs = parse_arguments(arguments)?;
    if args.alarm_id.is_some() == args.key.is_some() {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    let now = Utc::now();
    let mut tx = context.pool.begin().await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    let row: Option<AlarmRow> = if let Some(id) = args.alarm_id {
        sqlx::query_as("select * from alarms where id=$1 and conversation_id=$2 for update")
            .bind(id)
            .bind(caller.conversation_id)
            .fetch_optional(&mut *tx)
            .await?
    } else {
        sqlx::query_as(
            "select * from alarms where conversation_id=$1 and key=$2 and state='armed'
             for update",
        )
        .bind(caller.conversation_id)
        .bind(args.key.as_deref())
        .fetch_optional(&mut *tx)
        .await?
    };
    let Some(row) = row else {
        return Err(ToolError::new(ToolErrorCode::AlarmNotFound));
    };
    let result = match row.state.as_str() {
        "armed" => {
            cancel_in_tx(&mut tx, row.id, "cancelled", now, caller.call_id).await?;
            "cancelled"
        }
        "cancelled" => "already_cancelled",
        _ => "already_fired",
    };
    let updated = row_in_tx(&mut tx, row.id).await?;
    tx.commit().await?;
    Ok(json!({
        "result": result,
        "alarm": updated.as_ref().map(view),
    }))
}

/// Отмена владельцем из PWA: чужой будильник неотличим от несуществующего.
pub async fn cancel_owner(
    pool: &sqlx::PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    alarm_id: Uuid,
) -> Result<AlarmView, ToolError> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;
    let row: Option<AlarmRow> = sqlx::query_as(
        "select * from alarms where id=$1 and owner_id=$2 and conversation_id=$3 for update",
    )
    .bind(alarm_id)
    .bind(owner_id)
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(ToolError::new(ToolErrorCode::AlarmNotFound));
    };
    if row.state == "armed" {
        cancel_in_tx(&mut tx, row.id, "cancelled", now, Uuid::now_v7()).await?;
    }
    let updated = row_in_tx(&mut tx, row.id).await?.unwrap_or(row);
    tx.commit().await?;
    Ok(view(&updated))
}

/// `alarm_list`: только свои будильники; заметка не отдаётся, её уже знает модель.
pub async fn list(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ListArgs = parse_arguments(arguments)?;
    let include_finished = args.include_finished.unwrap_or(false);
    let mut conn = context.pool.acquire().await?;
    access::caller_in_tx(&mut conn, caller.owner_id, caller.conversation_id).await?;
    let rows = list_rows(
        &mut conn,
        caller.owner_id,
        caller.conversation_id,
        include_finished,
        20,
    )
    .await?;
    Ok(json!({
        "alarms": rows.iter().map(view).collect::<Vec<_>>(),
    }))
}

pub async fn list_rows(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    include_finished: bool,
    limit: i64,
) -> Result<Vec<AlarmRow>, sqlx::Error> {
    if include_finished {
        sqlx::query_as(
            "select * from alarms where owner_id=$1 and conversation_id=$2
             order by created_at desc limit $3",
        )
        .bind(owner_id)
        .bind(conversation_id)
        .bind(limit.min(100))
        .fetch_all(conn)
        .await
    } else {
        sqlx::query_as(
            "select * from alarms where owner_id=$1 and conversation_id=$2 and state='armed'
             order by coalesce(fire_at,deadline_at) limit $3",
        )
        .bind(owner_id)
        .bind(conversation_id)
        .bind(limit.min(100))
        .fetch_all(conn)
        .await
    }
}

/// Снимок разговора: взведённые и недавние завершившиеся будильники.
pub async fn snapshot_rows(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Vec<AlarmRow>, sqlx::Error> {
    sqlx::query_as(
        "select * from alarms
         where conversation_id=$1
           and (state='armed' or (state in ('fired','failed','expired')
                and updated_at > clock_timestamp() - interval '30 days'))
         order by state='armed' desc, coalesce(fire_at,deadline_at) nulls last, created_at
         limit 50",
    )
    .bind(conversation_id)
    .fetch_all(conn)
    .await
}

pub fn snapshot_views(rows: &[AlarmRow]) -> Vec<AlarmView> {
    rows.iter().map(view).collect()
}

/// Задание доставки: сливает все сработавшие и не доставленные будильники получателя
/// в один ввод. Дедупликация идёт по будильнику: у него один `delivered_input_id`.
pub async fn deliver_job_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<crate::crunch::JobOutcome, SubmitError> {
    let Some(conversation) = job.conversation_id else {
        return Ok(crate::crunch::JobOutcome::Cancelled("invalid_payload"));
    };
    let mut rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms where conversation_id=$1 and (
             (state='fired' and delivered_input_id is null)
             or (state='armed' and kind='at' and fire_at <= $2)
             or (state='armed' and kind='condition' and deadline_at <= $2)
         )
         order by coalesce(fire_at,deadline_at), id
         for update skip locked",
    )
    .bind(conversation)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(crate::crunch::JobOutcome::Cancelled("alarm_settled"));
    }
    for row in &mut rows {
        if row.state != "armed" {
            continue;
        }
        let (reason, fired_at, state) = if row.kind == "at" {
            ("time", row.fire_at.unwrap_or(now), None)
        } else {
            let state: Option<String> = sqlx::query_scalar(
                "select activity_state from conversation_derived where conversation_id=$1",
            )
            .bind(conversation)
            .fetch_optional(&mut *conn)
            .await?;
            ("timed_out", row.deadline_at.unwrap_or(now), state)
        };
        if let Some(state) = state {
            row.collected = json!({"state": state});
        }
        fire_in_tx(conn, row, reason, fired_at, now, correlation_id, false).await?;
    }
    let text = render(&rows, now);
    let key = format!("alarm:{}", job.id);
    let existing: Option<Uuid> =
        sqlx::query_scalar("select id from inputs where owner_id=$1 and idempotency_key=$2")
            .bind(job.owner_id)
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    let input_id = if let Some(input_id) = existing {
        input_id
    } else {
        let first = &rows[0];
        let submitted = submit_input_in_tx(
            conn,
            &SubmitInput {
                owner_id: job.owner_id,
                conversation_id: conversation,
                idempotency_key: key,
                content: vec![ContentBlock::Text { text }],
                received_at: now,
                correlation_id,
                origin: Some(json!({
                    "kind": "alarm",
                    "alarm_id": first.id,
                    "chain_depth": first.chain_depth,
                    "event": origin::HubEvent {
                        kind: "alarm".into(),
                        alarm_id: Some(first.id),
                        alarm_key: first.key.clone(),
                        fired_reason: first.fired_reason.clone(),
                        ..Default::default()
                    },
                })),
            },
        )
        .await?;
        submitted.input_id
    };
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    sqlx::query(
        "update alarms set delivered_input_id=$2,delivered_at=$3,updated_at=$3 where id=any($1)",
    )
    .bind(&ids)
    .bind(input_id)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(crate::crunch::JobOutcome::Done(json!({
        "input_id": input_id,
        "alarms": ids,
    })))
}

/// Задание доставки исчерпало попытки: будильник становится `failed` с причиной в PWA.
pub async fn abandon_deliver_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(id) = payload
        .get("alarm_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Uuid>().ok())
    else {
        return Ok(());
    };
    let row: Option<AlarmRow> = sqlx::query_as(
        "update alarms set state='failed',failure_reason='delivery_failed',updated_at=$2
         where id=$1 and conversation_id=$3 and delivered_input_id is null
           and state in ('armed','fired')
         returning *",
    )
    .bind(id)
    .bind(now)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = row {
        changed_in_tx(conn, &row, Uuid::now_v7()).await?;
    }
    Ok(())
}

fn render(rows: &[AlarmRow], now: DateTime<Utc>) -> String {
    let mut text = String::new();
    if rows.len() == 1 {
        text.push_str(&render_one(&rows[0], now));
    } else {
        text.push_str(&format!("[canterlot] {} alarms fired.\n", rows.len()));
        for row in rows {
            text.push_str(&format!("\n- {}", render_one(row, now).trim_start_matches("[canterlot] ")));
        }
    }
    text.push_str("\n\n");
    text.push_str(AUTOMATION);
    text
}

fn render_one(row: &AlarmRow, now: DateTime<Utc>) -> String {
    let name = name_of(row.key.as_deref());
    let fired_at = row.fired_at.unwrap_or(now);
    let late = now - fired_at;
    let late_text = if late >= TimeDelta::minutes(1) {
        format!(", late by {}", coarse(late))
    } else {
        String::new()
    };
    let mut text = match row.kind.as_str() {
        "at" => format!(
            "[canterlot] Alarm {name}fired at {}{late_text} (set {} ago).",
            clock(fired_at),
            coarse(fired_at - row.created_at)
        ),
        _ => {
            let state = row
                .collected
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let target = row
                .condition
                .as_ref()
                .and_then(|value| value.get("session_ids"))
                .and_then(Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(Value::as_str)
                .unwrap_or_default();
            if row.fired_reason.as_deref() == Some("timed_out") {
                format!(
                    "[canterlot] Alarm {name}timed out after {}: session {target} is still {state}{late_text}.",
                    coarse(fired_at - row.created_at)
                )
            } else {
                format!(
                    "[canterlot] Alarm {name}fired: session {target} is {state}{late_text}."
                )
            }
        }
    };
    if let Some(note) = row.note.as_deref().filter(|note| !note.is_empty()) {
        text.push_str("\nNote: ");
        text.push_str(note);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_args_from_json() {
        let at = (Utc::now() + TimeDelta::seconds(60)).to_rfc3339();
        let value = json!({"at": at, "key": "gate", "note": "wave C"});
        let parsed: SetArgs = serde_json::from_value(value).unwrap();
        assert!(parsed.at.is_some());
    }

    #[test]
    fn condition_text_names_target_states_and_deadline() {
        let id = Uuid::now_v7();
        let condition = SessionCondition {
            kind: "session_state".into(),
            session_ids: vec![id],
            states: vec!["idle".into(), "failed".into()],
            all: false,
            armed: BTreeMap::new(),
        };
        let text = condition_text(&condition, Utc::now(), false);
        assert!(text.contains(&id.to_string()));
        assert!(text.contains("idle, failed"));
        assert!(text.contains("deadline"));
    }

    #[test]
    fn render_single_alarm_keeps_note_and_automation_line() {
        let now = Utc::now();
        let id = Uuid::now_v7();
        let row = AlarmRow {
            id,
            owner_id: Uuid::now_v7(),
            conversation_id: Uuid::now_v7(),
            key: Some("gate".into()),
            kind: "at".into(),
            condition: None,
            fire_at: Some(now - TimeDelta::minutes(2)),
            deadline_at: None,
            only_on_timeout: false,
            batch: false,
            collected: Value::Null,
            note: Some("wave C".into()),
            chain_depth: 0,
            state: "fired".into(),
            fired_reason: Some("time".into()),
            failure_reason: None,
            call_id: Uuid::now_v7(),
            response: Value::Null,
            delivered_input_id: None,
            delivered_at: None,
            created_at: now - TimeDelta::hours(2),
            updated_at: now,
            fired_at: Some(now - TimeDelta::minutes(2)),
        };
        let text = render(&[row], now);
        assert!(text.starts_with("[canterlot] Alarm \"gate\" fired at"));
        assert!(text.contains("Note: wave C"));
        assert!(text.ends_with(AUTOMATION));
    }
}
