//! `session_wait`: блокирующее ожидание до 300 секунд. Основной путь остаётся подпиской,
//! ожидание нужно для коротких пауз внутри хода. Между опросами соединение с базой не держится:
//! на каждый опрос берётся короткое соединение и сразу возвращается в пул.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use tokio::time::Instant;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, parse_arguments, subs, summary,
};

const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 300;
const POLL: Duration = Duration::from_secs(1);
const HINT_SUBSCRIBED: &str = "You stay subscribed; end your turn.";
const HINT_NOT_SUBSCRIBED: &str =
    "Not settled yet and you are not subscribed; call session_subscribe, then end your turn.";
const DELETED_TEXT: &str = "[canterlot] The session was deleted.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    session_id: Uuid,
    timeout_seconds: Option<i64>,
}

/// Что показал опрос.
enum Poll {
    /// Цель осела; отпечаток тот же, что посчитает проектор.
    Settled(subs::Current),
    /// Цель ещё работает; в поле её последнее состояние.
    Pending(String),
    Deleted,
}

pub async fn wait(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: WaitArgs = parse_arguments("session_wait", arguments)?;
    let seconds = args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT as i64);
    if !(1..=MAX_TIMEOUT as i64).contains(&seconds) {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    check_visible(context, caller, args.session_id).await?;
    let turn = {
        let mut conn = context.pool.acquire().await?;
        caller_turn(&mut conn, caller.conversation_id).await?
    };
    let deadline = Instant::now() + Duration::from_secs(seconds as u64);
    loop {
        // Домен цели мог измениться, пока вызов ждал: итог показывается только при
        // действующей видимости, иначе отказ тот же, что у несуществующей сессии.
        // Удалённая цель сюда не попадает: её исход отдаёт опрос.
        {
            let mut conn = context.pool.acquire().await?;
            let hidden =
                access::hidden_from_in_tx(&mut conn, caller.conversation_id, &[args.session_id])
                    .await?;
            if !hidden.is_empty() {
                return Err(ToolError::new(ToolErrorCode::SessionNotFound));
            }
        }
        match poll(context, caller.owner_id, args.session_id).await? {
            Poll::Settled(current) => {
                return settled(context, caller, &turn, args.session_id, &current).await;
            }
            Poll::Deleted => {
                return Ok(json!({
                    "state": "deleted",
                    "summary": DELETED_TEXT,
                    "timed_out": false,
                    "event_suppressed": false,
                }));
            }
            Poll::Pending(state) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return timed_out(context, caller, args.session_id, state).await;
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL.min(left)) => {}
                    _ = context.shutdown.cancelled() => {
                        return Err(ToolError::with_message(
                            ToolErrorCode::Internal,
                            "The hub is shutting down, try again later",
                        ));
                    }
                }
            }
        }
    }
}

/// Та же видимость, что у остальных инструментов: чужая сессия неотличима от несуществующей.
async fn check_visible(
    context: &ToolContext,
    caller: &Caller,
    target: Uuid,
) -> Result<(), ToolError> {
    let mut tx = crate::db::begin(&context.pool).await?;
    let waiter = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if target == waiter.id {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    access::target_in_tx(&mut tx, &context.config, &waiter, caller.owner_id, target).await?;
    tx.commit().await?;
    Ok(())
}

async fn poll(context: &ToolContext, owner_id: Uuid, target: Uuid) -> Result<Poll, ToolError> {
    let mut conn = context.pool.acquire().await?;
    let exists: Option<Uuid> =
        sqlx::query_scalar("select id from conversations where id=$1 and owner_id=$2")
            .bind(target)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
    if exists.is_none() {
        return Ok(Poll::Deleted);
    }
    Ok(
        match subs::current_in_tx(&mut conn, target, Utc::now(), &context.settle).await? {
            Some(current) => Poll::Settled(current),
            None => Poll::Pending(activity_state(&mut conn, target).await?),
        },
    )
}

async fn activity_state(conn: &mut PgConnection, target: Uuid) -> Result<String, sqlx::Error> {
    let state: Option<String> = sqlx::query_scalar(
        "select activity_state from conversation_derived where conversation_id=$1",
    )
    .bind(target)
    .fetch_optional(conn)
    .await?;
    Ok(state.unwrap_or_else(|| "idle".to_owned()))
}

/// Ход вызывающего на момент вызова: ввод в слоте очереди и номер его попытки. Свободный слот
/// тоже ход: агент может звать инструмент из фоновой работы, когда хаб хода не видит.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallerTurn {
    input_id: Option<Uuid>,
    attempt: Option<i32>,
}

pub async fn caller_turn(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<CallerTurn, sqlx::Error> {
    let slot: Option<(Option<Uuid>, Option<i32>)> = sqlx::query_as(
        "select run_slot_input_id, run_slot_attempt from queue_state where conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await?;
    let (input_id, attempt) = slot.unwrap_or_default();
    Ok(CallerTurn { input_id, attempt })
}

/// Ожидание забирает событие подписки себе: ставит отпечаток и, если итог конечный, закрывает
/// подписку. Условие в самом `update` решает гонку с проектором: кто первым взял строку, тот и
/// говорит.
///
/// Забирать можно, только пока ответ вызова есть кому прочитать. Воркер вызов не отменяет:
/// после прерывания хода или смерти CLI хаб продолжает ждать, а ответ уходит в никуда. Поэтому
/// исполнение вызывающего должно быть живым, а слот его очереди занят тем же вводом, что в
/// начале вызова; иначе событие остаётся проектору и приходит сообщением.
pub async fn claim_event(
    conn: &mut PgConnection,
    caller: &Caller,
    turn: &CallerTurn,
    target: Uuid,
    current: &subs::Current,
) -> Result<bool, sqlx::Error> {
    let hit = sqlx::query(
        "update session_subscriptions set last_fingerprint=$3,
           closed_at=case when $4 then clock_timestamp() end,
           closed_reason=case when $4 then $5 end
         where subscriber_id=$1 and target_id=$2 and closed_at is null
           and last_fingerprint is distinct from $3
           and exists(select 1 from executions e
                       where e.id=$6 and e.conversation_id=$1
                         and e.state not in ('stopped','failed'))
           and (select q.run_slot_input_id from queue_state q where q.conversation_id=$1)
               is not distinct from $7
           and (select q.run_slot_attempt from queue_state q where q.conversation_id=$1)
               is not distinct from $8",
    )
    .bind(caller.conversation_id)
    .bind(target)
    .bind(&current.fingerprint)
    .bind(current.closes)
    .bind(current.state.as_str())
    .bind(caller.execution_id)
    .bind(turn.input_id)
    .bind(turn.attempt)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(hit > 0)
}

/// Результат увиден здесь: если проектор ещё не отправил событие, оно больше не нужно.
async fn settled(
    context: &ToolContext,
    caller: &Caller,
    turn: &CallerTurn,
    target: Uuid,
    current: &subs::Current,
) -> Result<Value, ToolError> {
    let state = current.state;
    let mut conn = context.pool.acquire().await?;
    // Последняя проверка видимости прямо перед чтением итога.
    if !access::hidden_from_in_tx(&mut conn, caller.conversation_id, &[target])
        .await?
        .is_empty()
    {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    let armed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select armed_at from session_subscriptions where subscriber_id=$1 and target_id=$2",
    )
    .bind(caller.conversation_id)
    .bind(target)
    .fetch_optional(&mut *conn)
    .await?;
    let text = summary::event_text(&mut conn, caller.owner_id, target, state.as_str(), armed_at)
        .await?
        .unwrap_or_else(|| DELETED_TEXT.to_owned());
    let hit = claim_event(&mut conn, caller, turn, target, current).await?;
    Ok(json!({
        "state": state.as_str(),
        "summary": text,
        "timed_out": false,
        "event_suppressed": hit,
    }))
}

async fn timed_out(
    context: &ToolContext,
    caller: &Caller,
    target: Uuid,
    state: String,
) -> Result<Value, ToolError> {
    let subscribed: bool = sqlx::query_scalar(
        "select exists(select 1 from session_subscriptions
         where subscriber_id=$1 and target_id=$2 and closed_at is null)",
    )
    .bind(caller.conversation_id)
    .bind(target)
    .fetch_one(&context.pool)
    .await?;
    Ok(json!({
        "state": state,
        "summary": Value::Null,
        "timed_out": true,
        "event_suppressed": false,
        "hint": if subscribed { HINT_SUBSCRIBED } else { HINT_NOT_SUBSCRIBED },
    }))
}
