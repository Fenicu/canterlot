//! `session_send`, `session_subscribe`, `session_unsubscribe`. Подписка взводится в одной
//! транзакции с вводом и под очередью цели: цель не может осесть между ними, иначе
//! сброшенный отпечаток тут же вернул бы отправителю старый итог.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, access, origin, parse_arguments, subs};
use crate::{
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    executions,
};

const MAX_TEXT_BYTES: usize = 32 * 1024;
const DEDUP_WINDOW: chrono::TimeDelta = chrono::TimeDelta::minutes(10);
const CLOSED_BY_UNSUBSCRIBE: &str = "unsubscribed";

const HINT_SUBSCRIBED: &str = "Queued. You are subscribed: the result arrives in your queue as a [canterlot] message. Do not poll; end your turn if you have nothing else to do.";
const HINT_STOPPED: &str = "Queued, but the target session is stopped: the message waits until the owner resumes it. Do not poll; end your turn if you have nothing else to do.";
const HINT_NOT_SUBSCRIBED: &str =
    "Queued. You are not subscribed: no [canterlot] message will tell you the result.";
const HINT_SUBSCRIBE: &str = "Subscribed: when the session settles, a [canterlot] message arrives in your queue. Do not poll; end your turn if you have nothing else to do.";

fn subscribe_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    session_id: Uuid,
    text: String,
    #[serde(default = "subscribe_by_default")]
    subscribe: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    session_id: Uuid,
}

fn submit_error(error: SubmitError) -> ToolError {
    match error {
        SubmitError::QueueFull { .. } => ToolError::new(ToolErrorCode::QueueFull),
        SubmitError::ConversationClosed => ToolError::new(ToolErrorCode::ConversationClosed),
        SubmitError::ConversationNotFound => ToolError::new(ToolErrorCode::SessionNotFound),
        SubmitError::ResourceLimit { .. } => ToolError::new(ToolErrorCode::TextTooLarge),
        SubmitError::Db(error) => error.into(),
        SubmitError::InvalidContent { .. }
        | SubmitError::InvalidRequest { .. }
        | SubmitError::IdempotencyConflict => ToolError::new(ToolErrorCode::InvalidArguments),
        SubmitError::KeyRaceLost | SubmitError::Attachment(_) => {
            ToolError::new(ToolErrorCode::Internal)
        }
    }
}

/// Отправки одной сессии идут по очереди: иначе параллельные вызовы вместе прошли бы
/// и проверку частоты, и поиск такого же текста.
async fn lock_sender(conn: &mut PgConnection, sender: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('canterlot:send:'||$1::text,0))")
        .bind(sender)
        .execute(conn)
        .await?;
    Ok(())
}

async fn lock_queue(conn: &mut PgConnection, target: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(target)
        .execute(conn)
        .await?;
    Ok(())
}

/// Этот же вызов уже прошёл: повтор транспорта отвечает им же и подписку не трогает.
async fn replayed(
    conn: &mut PgConnection,
    caller: &Caller,
    key: &str,
    target: Uuid,
    sha256: &str,
) -> Result<Option<Uuid>, ToolError> {
    let stored: Option<(Uuid, Uuid, Option<Value>)> = sqlx::query_as(
        "select id,conversation_id,origin from inputs where owner_id=$1 and idempotency_key=$2",
    )
    .bind(caller.owner_id)
    .bind(key)
    .fetch_optional(conn)
    .await?;
    let Some((id, conversation, stored_origin)) = stored else {
        return Ok(None);
    };
    let same = stored_origin.is_some_and(|value| {
        value["from"].as_str() == Some(caller.conversation_id.to_string().as_str())
            && value["sha256"].as_str() == Some(sha256)
    });
    if conversation != target || !same {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    Ok(Some(id))
}

/// Тот же текст тому же адресату, пока первый ввод не закончен (D18). Закончен значит вышел из
/// очереди и не занимает слот хода: потом тот же текст уже новая просьба.
async fn duplicate(
    conn: &mut PgConnection,
    sender: Uuid,
    target: Uuid,
    sha256: &str,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select i.id from inputs i join queue_state q on q.conversation_id=i.conversation_id
         where i.origin->>'from'=$1 and i.conversation_id=$2 and i.origin->>'sha256'=$3
           and i.received_at>$4
           and (i.state in ('queued','dispatched','on_machine','transmitting')
                or q.run_slot_input_id=i.id)
         order by i.input_seq limit 1",
    )
    .bind(sender.to_string())
    .bind(target)
    .bind(sha256)
    .bind(now - DEDUP_WINDOW)
    .fetch_optional(conn)
    .await
}

async fn subscribed(
    conn: &mut PgConnection,
    subscriber: Uuid,
    target: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from session_subscriptions
                       where subscriber_id=$1 and target_id=$2 and closed_at is null)",
    )
    .bind(subscriber)
    .bind(target)
    .fetch_one(conn)
    .await
}

async fn answer(
    conn: &mut PgConnection,
    caller: &Caller,
    target: Uuid,
    input_id: Uuid,
    deduplicated: bool,
) -> Result<Value, sqlx::Error> {
    let target_stopped = executions::live(&mut *conn, target).await?.is_none();
    let subscribed = subscribed(conn, caller.conversation_id, target).await?;
    let hint = match (target_stopped, subscribed) {
        (_, false) => HINT_NOT_SUBSCRIBED,
        (true, true) => HINT_STOPPED,
        (false, true) => HINT_SUBSCRIBED,
    };
    Ok(json!({
        "input_id": input_id,
        "deduplicated": deduplicated,
        "subscribed": subscribed,
        "target_stopped": target_stopped,
        "hint": hint,
    }))
}

pub async fn send(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SendArgs = parse_arguments(arguments)?;
    if args.text.len() > MAX_TEXT_BYTES {
        return Err(ToolError::new(ToolErrorCode::TextTooLarge));
    }
    if args.text.trim().is_empty() {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    let mut tx = context.pool.begin().await?;
    lock_sender(&mut tx, caller.conversation_id).await?;
    let (result, fresh) = send_in_tx(&mut tx, context, caller, &args).await?;
    tx.commit().await?;
    if fresh && let Some(dispatcher) = &context.dispatcher {
        dispatcher.trigger(args.session_id);
    }
    Ok(result)
}

async fn send_in_tx(
    conn: &mut PgConnection,
    context: &ToolContext,
    caller: &Caller,
    args: &SendArgs,
) -> Result<(Value, bool), ToolError> {
    let target_id = args.session_id;
    let sha256 = origin::text_sha256(&args.text);
    let key = format!("agent:{}", caller.call_id);
    if let Some(input_id) = replayed(conn, caller, &key, target_id, &sha256).await? {
        return Ok((
            answer(conn, caller, target_id, input_id, false).await?,
            false,
        ));
    }
    let sender = access::caller_in_tx(conn, caller.owner_id, caller.conversation_id).await?;
    if target_id == sender.id {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    let target =
        access::target_in_tx(conn, &context.config, &sender, caller.owner_id, target_id).await?;
    if target.archived {
        return Err(ToolError::new(ToolErrorCode::ConversationClosed));
    }
    access::check_read_only(&sender, &target)?;
    lock_queue(conn, target_id).await?;
    let now = Utc::now();
    let depth = access::current_depth(conn, sender.id).await?;
    if let Some(input_id) = duplicate(conn, sender.id, target_id, &sha256, now).await? {
        if args.subscribe {
            subs::subscribe_in_tx(conn, caller.owner_id, sender.id, target_id, depth).await?;
        }
        return Ok((
            answer(conn, caller, target_id, input_id, true).await?,
            false,
        ));
    }
    access::check_depth(&context.config, depth + 1)?;
    access::check_rate(conn, &context.config, sender.id, target_id, now).await?;
    let from = sender.sender();
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: caller.owner_id,
            conversation_id: target_id,
            idempotency_key: key,
            content: vec![ContentBlock::Text {
                text: origin::message_text(&from, &args.text),
            }],
            received_at: now,
            correlation_id: caller.call_id,
            origin: Some(origin::message_origin(&from, depth + 1, &sha256)),
        },
    )
    .await
    .map_err(submit_error)?;
    if args.subscribe {
        subs::subscribe_in_tx(conn, caller.owner_id, sender.id, target_id, depth).await?;
    }
    let result = answer(conn, caller, target_id, submitted.input_id, false).await?;
    Ok((result, true))
}

pub async fn subscribe(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: TargetArgs = parse_arguments(arguments)?;
    let mut tx = context.pool.begin().await?;
    let subscriber = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if args.session_id == subscriber.id {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    let target = access::target_in_tx(
        &mut tx,
        &context.config,
        &subscriber,
        caller.owner_id,
        args.session_id,
    )
    .await?;
    lock_queue(&mut tx, target.id).await?;
    let depth = access::current_depth(&mut tx, subscriber.id).await?;
    let settled = subs::subscribe_explicit_in_tx(
        &mut tx,
        caller.owner_id,
        subscriber.id,
        target.id,
        depth,
        Utc::now(),
        &context.settle,
    )
    .await?;
    let current_state = match settled {
        Some(state) => state.as_str().to_owned(),
        None => activity_state(&mut tx, target.id).await?,
    };
    tx.commit().await?;
    Ok(json!({"subscribed": true, "current_state": current_state, "hint": HINT_SUBSCRIBE}))
}

async fn activity_state(conn: &mut PgConnection, id: Uuid) -> Result<String, sqlx::Error> {
    let state: Option<String> = sqlx::query_scalar(
        "select activity_state from conversation_derived where conversation_id=$1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(state.unwrap_or_else(|| "idle".to_owned()))
}

pub async fn unsubscribe(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: TargetArgs = parse_arguments(arguments)?;
    let mut tx = context.pool.begin().await?;
    let subscriber = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if args.session_id == subscriber.id {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    let target = access::target_in_tx(
        &mut tx,
        &context.config,
        &subscriber,
        caller.owner_id,
        args.session_id,
    )
    .await?;
    subs::unsubscribe_in_tx(&mut tx, subscriber.id, target.id, CLOSED_BY_UNSUBSCRIBE).await?;
    tx.commit().await?;
    Ok(json!({"subscribed": false}))
}
