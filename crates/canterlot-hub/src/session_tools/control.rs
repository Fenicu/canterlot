//! `session_interrupt` и `session_stop`: только прямой родитель управляет остановкой
//! запущенной им сессии. Рабочая копия и ветка остаются: это не приём результата.

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, access, parse_arguments};
use crate::{delivery::control, executions};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    session_id: Uuid,
}

/// Цель обязана быть прямым потомком вызывающего: чужая и корневая сессия неразличимы
/// с несуществующей, а сессия без родителя — с неделегированной.
async fn owned_in_tx(
    conn: &mut PgConnection,
    context: &ToolContext,
    caller: &Caller,
    target_id: Uuid,
) -> Result<access::Session, ToolError> {
    let me = access::caller_in_tx(conn, caller.owner_id, caller.conversation_id).await?;
    if target_id == me.id {
        return Err(ToolError::new(ToolErrorCode::SessionNotFound));
    }
    let target =
        access::target_in_tx(conn, &context.config, &me, caller.owner_id, target_id).await?;
    let parent: Option<Uuid> = sqlx::query_scalar(
        "select parent_conversation_id from conversations where id=$1 and owner_id=$2",
    )
    .bind(target.id)
    .bind(caller.owner_id)
    .fetch_one(&mut *conn)
    .await?;
    match parent {
        None => return Err(ToolError::new(ToolErrorCode::NotDelegated)),
        Some(parent) if parent != me.id => return Err(ToolError::new(ToolErrorCode::NotParent)),
        Some(_) => {}
    }
    Ok(target)
}

fn idle() -> Value {
    json!({"status": "nothing_running", "hint": "The session has no live process."})
}

pub async fn interrupt(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: Args = parse_arguments(arguments)?;
    let mut tx = context.pool.begin().await?;
    let target = owned_in_tx(&mut tx, context, caller, args.session_id).await?;
    access::check_read_only(
        &access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?,
        &target,
    )?;
    let live = executions::live(&mut tx, target.id).await?.is_some();
    tx.commit().await?;
    if !live {
        return Ok(idle());
    }
    let outcome = control::request_stop(
        &context.pool,
        caller.owner_id,
        target.id,
        &format!("agenti:{}", caller.call_id),
        caller.call_id,
    )
    .await
    .map_err(control_error)?;
    let control::StopOutcome::Requested {
        interrupt_command_id,
        ..
    } = outcome;
    Ok(json!({
        "status": "interrupted",
        "session_id": target.id,
        "command_id": interrupt_command_id,
    }))
}

pub async fn stop(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: Args = parse_arguments(arguments)?;
    let mut tx = context.pool.begin().await?;
    let target = owned_in_tx(&mut tx, context, caller, args.session_id).await?;
    access::check_read_only(
        &access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?,
        &target,
    )?;
    let live = executions::live(&mut tx, target.id).await?.is_some();
    tx.commit().await?;
    if !live {
        return Ok(idle());
    }
    let outcome = control::request_stop_execution(
        &context.pool,
        caller.owner_id,
        target.id,
        false,
        &format!("agentstop:{}", caller.call_id),
        caller.call_id,
    )
    .await
    .map_err(control_error)?;
    let (status, command_id) = match outcome {
        control::StopExecutionOutcome::Requested { command_id, .. } => {
            ("stopping", Some(command_id))
        }
        control::StopExecutionOutcome::AlreadyRequested { command_id } => {
            ("already_stopping", Some(command_id))
        }
        control::StopExecutionOutcome::NothingToStop => ("nothing_running", None),
        control::StopExecutionOutcome::OwnerLost => ("owner_lost", None),
    };
    let mut answer = json!({
        "status": status,
        "session_id": target.id,
        "worktree_kept": true,
    });
    if let Some(id) = command_id {
        answer["command_id"] = json!(id);
    }
    Ok(answer)
}

fn control_error(error: control::ControlError) -> ToolError {
    use control::ControlError as E;
    match error {
        E::NotFound => ToolError::new(ToolErrorCode::SessionNotFound),
        E::IdempotencyConflict | E::InvalidRequest { .. } => {
            ToolError::new(ToolErrorCode::InvalidArguments)
        }
        E::Db(error) => error.into(),
    }
}
