use serde_json::json;
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    delivery::LockedQueue,
    events::{self, NewEvent, Scope},
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
};

#[derive(Debug, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct StartedExecution {
    pub execution_id: Uuid,
    pub generation: i64,
    pub command_id: Uuid,
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("execution {execution_id} is already live")]
    AlreadyLive { execution_id: Uuid },
    #[error("conversation not found")]
    ConversationNotFound,
    #[error("conversation closed")]
    ConversationClosed,
    #[error("invalid execution transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: ExecutionState,
        to: ExecutionState,
    },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn start(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<StartedExecution, ExecutionError> {
    let mut tx = pool.begin().await?;
    let started = start_in_tx(
        &mut tx,
        owner_id,
        conversation_id,
        &StartOptions { resume: false },
        correlation_id,
    )
    .await?;
    tx.commit().await?;
    Ok(started)
}

pub struct StartOptions {
    pub resume: bool,
}

pub async fn start_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    options: &StartOptions,
    correlation_id: Uuid,
) -> Result<StartedExecution, ExecutionError> {
    let mut queue: LockedQueue = sqlx::query_as(
        "select * from queue_state
         where conversation_id = $1 for update",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ExecutionError::ConversationNotFound)?;
    let row = sqlx::query("select * from conversations where id=$1 and owner_id=$2")
        .bind(conversation_id)
        .bind(owner_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(ExecutionError::ConversationNotFound)?;
    use sqlx::Row;
    let machine_id: Uuid = row.try_get("machine_id")?;
    let native: Option<String> = if options.resume {
        sqlx::query_scalar("select native_conversation_id from executions where conversation_id=$1 order by generation desc limit 1")
            .bind(conversation_id).fetch_optional(&mut *conn).await?.flatten()
    } else {
        None
    };
    if queue.closed {
        return Err(ExecutionError::ConversationClosed);
    }
    if let Some((execution_id, _, _)) = live(conn, conversation_id).await? {
        return Err(ExecutionError::AlreadyLive { execution_id });
    }
    let generation: i64 = sqlx::query_scalar(
        "select coalesce(max(generation), 0) + 1 from executions where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    let started = StartedExecution {
        execution_id: Uuid::now_v7(),
        generation,
        command_id: Uuid::now_v7(),
    };
    sqlx::query(
        "insert into executions (id, conversation_id, generation, state)
         values ($1, $2, $3, 'created')",
    )
    .bind(started.execution_id)
    .bind(conversation_id)
    .bind(generation)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                               execution_generation, kind, payload, delivery_state)
         values ($1, $2, $3, $4, $5, 'start_execution', $6, 'pending')",
    )
    .bind(started.command_id)
    .bind(machine_id)
    .bind(conversation_id)
    .bind(started.execution_id)
    .bind(generation)
    .bind(json!({"harness": row.try_get::<String,_>("harness")?, "workdir": row.try_get::<String,_>("workdir")?,
        "model":row.try_get::<String,_>("model")?,"effort":row.try_get::<String,_>("effort")?,
        "permission_mode":row.try_get::<String,_>("permission_mode")?,"instruction_profile":row.try_get::<String,_>("instruction_profile")?,
        "native_conversation_id":native}))
    .execute(&mut *conn)
    .await?;
    sqlx::query("select pg_notify('canterlot_outbox', $1::text)")
        .bind(machine_id.to_string())
        .execute(&mut *conn)
        .await?;
    queue
        .hold_reasons
        .0
        .retain(|r| *r != HoldReason::NoLiveExecution);
    add_once(&mut queue.hold_reasons.0, HoldReason::ExecutionStarting);
    queue
        .save(
            conn,
            &queue.slot()?,
            Some(started.execution_id),
            correlation_id,
        )
        .await?;
    events::append(
        conn,
        state_event(
            conversation_id,
            started.execution_id,
            generation,
            ExecutionState::Created,
            None,
            correlation_id,
        ),
    )
    .await?;
    Ok(started)
}

pub async fn set_state(
    pool: &PgPool,
    execution_id: Uuid,
    to: ExecutionState,
    outcome: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), ExecutionError> {
    let mut tx = pool.begin().await?;
    set_state_in_tx(&mut tx, execution_id, to, outcome, correlation_id).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_state_in_tx(
    conn: &mut PgConnection,
    execution_id: Uuid,
    to: ExecutionState,
    outcome: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), ExecutionError> {
    let mut queue: LockedQueue = sqlx::query_as(
        "select q.* from queue_state q join executions e on e.conversation_id = q.conversation_id
         where e.id = $1 for update of q",
    )
    .bind(execution_id)
    .fetch_one(&mut *conn)
    .await?;
    let (state, generation): (String, i64) =
        sqlx::query_as("select state, generation from executions where id = $1")
            .bind(execution_id)
            .fetch_one(&mut *conn)
            .await?;
    let from = state
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    if !legal_transition(from, to, outcome) {
        return Err(ExecutionError::InvalidTransition { from, to });
    }
    let mut slot = queue.slot()?;
    match to {
        ExecutionState::Running => queue.hold_reasons.0.retain(|reason| {
            !matches!(
                reason,
                HoldReason::ExecutionStarting | HoldReason::NoLiveExecution
            )
        }),
        ExecutionState::Stopped | ExecutionState::Failed => {
            if !matches!(
                outcome,
                Some("user_stop" | "start_cancelled" | "force_stopped")
            ) {
                add_once(
                    &mut queue.pause_reasons.0,
                    if to == ExecutionState::Stopped {
                        PauseReason::ProcessExited
                    } else {
                        PauseReason::FatalError
                    },
                );
            }
            queue
                .hold_reasons
                .0
                .retain(|reason| *reason != HoldReason::ExecutionStarting);
            add_once(&mut queue.hold_reasons.0, HoldReason::NoLiveExecution);
            if matches!(slot, RunSlot::Running { .. }) {
                slot = RunSlot::Free;
                queue
                    .hold_reasons
                    .0
                    .retain(|reason| *reason != HoldReason::RunInProgress);
            }
        }
        ExecutionState::OwnerLost => add_once(&mut queue.pause_reasons.0, PauseReason::OwnerLost),
        _ => {}
    }
    sqlx::query("update executions set state = $2, outcome = $3, phase = case when $2 in ('stopped', 'failed') then null else phase end, phase_at = case when $2 in ('stopped', 'failed') then null else phase_at end, phase_detail=case when $2 in ('stopped','failed') then null else phase_detail end, updated_at = now() where id = $1")
        .bind(execution_id)
        .bind(to.as_str())
        .bind(outcome)
        .execute(&mut *conn)
        .await?;
    queue
        .save(conn, &slot, Some(execution_id), correlation_id)
        .await?;
    events::append(
        conn,
        state_event(
            queue.conversation_id,
            execution_id,
            generation,
            to,
            outcome,
            correlation_id,
        ),
    )
    .await?;
    Ok(())
}

pub async fn live(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<(Uuid, i64, ExecutionState)>, sqlx::Error> {
    let row: Option<(Uuid, i64, String)> = sqlx::query_as(
        "select id, generation, state from executions
         where conversation_id = $1 and state not in ('stopped', 'failed')",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await?;
    row.map(|(id, generation, state)| {
        state
            .parse()
            .map(|state| (id, generation, state))
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))
    })
    .transpose()
}

fn legal_transition(from: ExecutionState, to: ExecutionState, outcome: Option<&str>) -> bool {
    use ExecutionState::*;
    matches!(
        (from, to, outcome),
        (Created, Created | Starting | OwnerLost, None)
            | (Created, Stopped, Some("start_cancelled"))
            | (Created, Failed, Some("start_rejected"))
            | (Starting, Running | Stopping | OwnerLost, None)
            | (Running, Stopping | OwnerLost, None)
            | (Stopping, OwnerLost, None)
            | (
                Starting | Running | Stopping | OwnerLost,
                Stopped,
                Some("machine_rebooted")
            )
            | (
                Stopping,
                Stopped,
                Some("user_stop" | "normal_exit" | "start_cancelled" | "force_stopped")
            )
            | (
                Stopping,
                Failed,
                Some(
                    "start_rejected"
                        | "start_failed"
                        | "cli_incompatible"
                        | "auth_required"
                        | "crashed"
                        | "protocol_incompatible"
                        | "storage_failure"
                        | "supervisor_died"
                        | "context_exhausted"
                        | "admin_resolved"
                )
            )
            | (
                OwnerLost,
                Failed,
                Some("supervisor_died" | "admin_resolved")
            )
    )
}

fn add_once<T: PartialEq>(reasons: &mut Vec<T>, reason: T) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn state_event(
    conversation_id: Uuid,
    execution_id: Uuid,
    generation: i64,
    state: ExecutionState,
    outcome: Option<&str>,
    correlation_id: Uuid,
) -> NewEvent {
    NewEvent {
        scope: Scope::Conversation {
            id: conversation_id,
        },
        kind: "execution_state_changed",
        conversation_id: Some(conversation_id),
        execution_id: Some(execution_id),
        payload: json!({"state": state, "generation": generation, "outcome": outcome}),
        correlation_id,
    }
}
