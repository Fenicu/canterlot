use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    executions,
    idempotency::{self, Begin},
    model::{InputState, PauseReason, RunSlot},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    CancelRequested { cancel_command_id: Uuid },
    TooLate { state: InputState },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    Requested {
        interrupt_command_id: Option<Uuid>,
        withdraw_command_id: Option<Uuid>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinueOutcome {
    pub removed: Vec<PauseReason>,
    pub remaining: Vec<PauseReason>,
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("input or conversation not found")]
    NotFound,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub async fn cancel_input(
    pool: &PgPool,
    owner_id: Uuid,
    input_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<CancelOutcome, ControlError> {
    const OPERATION: &str = "cancel_input";
    let hash = idempotency::request_hash(OPERATION, &json!({"input_id": input_id}));
    let mut tx = pool.begin().await?;
    let queue: LockedQueue = sqlx::query_as(
        "select q.* from queue_state q
         join inputs i on i.conversation_id = q.conversation_id
         join conversations c on c.id = q.conversation_id
         where i.id = $1 and i.owner_id = $2 and c.owner_id = $2 for update of q",
    )
    .bind(input_id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ControlError::NotFound)?;
    if let Some(outcome) = lookup(&mut tx, owner_id, idempotency_key, OPERATION, &hash).await? {
        tx.commit().await?;
        return Ok(outcome);
    }
    let (state, command_id, execution_id): (String, Uuid, Option<Uuid>) =
        sqlx::query_as("select i.state, i.command_id, c.execution_id from inputs i join commands c on c.id = i.command_id where i.id = $1")
            .bind(input_id)
            .fetch_one(&mut *tx)
            .await?;
    let state = state
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    let outcome = match state {
        InputState::Queued => {
            sqlx::query(
                "update inputs set state = 'cancelled', cancel_requested = false where id = $1",
            )
            .bind(input_id)
            .execute(&mut *tx)
            .await?;
            events::append(
                &mut tx,
                NewEvent {
                    scope: Scope::Conversation {
                        id: queue.conversation_id,
                    },
                    kind: "input_state_changed",
                    conversation_id: Some(queue.conversation_id),
                    execution_id,
                    payload: json!({"input_id": input_id, "state": "cancelled"}),
                    correlation_id,
                },
            )
            .await?;
            CancelOutcome::Cancelled
        }
        InputState::Dispatched | InputState::OnMachine => {
            sqlx::query("update inputs set cancel_requested = true where id = $1")
                .bind(input_id)
                .execute(&mut *tx)
                .await?;
            let cancel_command_id = target_command(&mut tx, command_id, "cancel_input").await?;
            events::append(
                &mut tx,
                NewEvent {
                    scope: Scope::Conversation {
                        id: queue.conversation_id,
                    },
                    kind: "input_state_changed",
                    conversation_id: Some(queue.conversation_id),
                    execution_id,
                    payload: json!({"input_id": input_id, "state": state, "cancel_requested": true}),
                    correlation_id,
                },
            )
            .await?;
            CancelOutcome::CancelRequested { cancel_command_id }
        }
        InputState::Cancelled => CancelOutcome::Cancelled,
        state => CancelOutcome::TooLate { state },
    };
    record(
        &mut tx,
        owner_id,
        idempotency_key,
        OPERATION,
        &hash,
        queue.conversation_id,
        &outcome,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

pub async fn request_stop(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<StopOutcome, ControlError> {
    const OPERATION: &str = "request_stop";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let mut tx = pool.begin().await?;
    let mut queue = lock_conversation(&mut tx, owner_id, conversation_id).await?;
    if let Some(outcome) = lookup(&mut tx, owner_id, idempotency_key, OPERATION, &hash).await? {
        tx.commit().await?;
        return Ok(outcome);
    }
    if !queue.pause_reasons.0.contains(&PauseReason::UserStop) {
        queue.pause_reasons.0.push(PauseReason::UserStop);
    }
    let slot = queue.slot()?;
    queue.save(&mut tx, &slot).await?;
    let live = executions::live(&mut tx, conversation_id).await?;
    let interrupt_command_id = if let Some((execution_id, generation, _)) = live {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "select id from commands where conversation_id = $1 and kind = 'interrupt'
             and execution_id = $2 and execution_generation = $3 and delivery_state <> 'completed'
             order by created_at, id limit 1",
        )
        .bind(conversation_id)
        .bind(execution_id)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await?;
        Some(if let Some(existing) = existing {
            existing
        } else {
            let command_id = Uuid::now_v7();
            let machine_id: Uuid = sqlx::query_scalar(
                "insert into commands (id, machine_id, conversation_id, execution_id,
                                       execution_generation, kind, payload, delivery_state)
                 select $1, machine_id, id, $3, $4, 'interrupt', $5, 'pending'
                 from conversations where id = $2 returning machine_id",
            )
            .bind(command_id)
            .bind(conversation_id)
            .bind(execution_id)
            .bind(generation)
            .bind(json!({"execution_id": execution_id, "execution_generation": generation}))
            .fetch_one(&mut *tx)
            .await?;
            notify_outbox(&mut tx, machine_id).await?;
            command_id
        })
    } else {
        None
    };
    let withdraw_command_id = if let RunSlot::Dispatching { input_id, .. } = slot {
        let command_id: Option<Uuid> = sqlx::query_scalar(
            "select command_id from inputs where id = $1 and state in ('dispatched', 'on_machine')",
        )
        .bind(input_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(command_id) = command_id {
            Some(target_command(&mut tx, command_id, "withdraw_input").await?)
        } else {
            None
        }
    } else {
        None
    };
    events::append(
        &mut tx,
        queue_event(
            &queue,
            "queue_paused",
            live.map(|(id, _, _)| id),
            correlation_id,
        ),
    )
    .await?;
    let outcome = StopOutcome::Requested {
        interrupt_command_id,
        withdraw_command_id,
    };
    record(
        &mut tx,
        owner_id,
        idempotency_key,
        OPERATION,
        &hash,
        conversation_id,
        &outcome,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

pub async fn continue_queue(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<ContinueOutcome, ControlError> {
    const OPERATION: &str = "continue_queue";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let mut tx = pool.begin().await?;
    let mut queue = lock_conversation(&mut tx, owner_id, conversation_id).await?;
    if let Some(outcome) = lookup(&mut tx, owner_id, idempotency_key, OPERATION, &hash).await? {
        tx.commit().await?;
        return Ok(outcome);
    }
    let (remaining, removed): (Vec<_>, Vec<_>) =
        queue.pause_reasons.0.into_iter().partition(|reason| {
            matches!(reason, PauseReason::OwnerLost | PauseReason::RestorePending)
        });
    let outcome = ContinueOutcome { removed, remaining };
    queue.pause_reasons.0 = outcome.remaining.clone();
    sqlx::query("update queue_state set pause_reasons = $2 where conversation_id = $1")
        .bind(conversation_id)
        .bind(&queue.pause_reasons)
        .execute(&mut *tx)
        .await?;
    events::append(
        &mut tx,
        queue_event(
            &queue,
            if outcome.remaining.is_empty() {
                "queue_resumed"
            } else {
                "queue_paused"
            },
            None,
            correlation_id,
        ),
    )
    .await?;
    record(
        &mut tx,
        owner_id,
        idempotency_key,
        OPERATION,
        &hash,
        conversation_id,
        &outcome,
    )
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn lock_conversation(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<LockedQueue, ControlError> {
    sqlx::query_as(
        "select q.* from queue_state q join conversations c on c.id = q.conversation_id
         where q.conversation_id = $1 and c.owner_id = $2 for update of q",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(conn)
    .await?
    .ok_or(ControlError::NotFound)
}

async fn lookup<T: DeserializeOwned>(
    conn: &mut PgConnection,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    hash: &[u8],
) -> Result<Option<T>, ControlError> {
    match idempotency::lookup(conn, owner_id, key, operation, hash).await? {
        Begin::New => Ok(None),
        Begin::Conflict => Err(ControlError::IdempotencyConflict),
        Begin::Replay(response) => serde_json::from_value(response)
            .map(Some)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)).into()),
    }
}

async fn record<T: Serialize>(
    conn: &mut PgConnection,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    hash: &[u8],
    conversation_id: Uuid,
    outcome: &T,
) -> Result<(), ControlError> {
    let response =
        serde_json::to_value(outcome).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    if idempotency::record(
        conn,
        owner_id,
        key,
        operation,
        hash,
        &response,
        Some(conversation_id),
    )
    .await?
    {
        Ok(())
    } else {
        Err(ControlError::IdempotencyConflict)
    }
}

async fn target_command(
    conn: &mut PgConnection,
    target_command_id: Uuid,
    kind: &str,
) -> Result<Uuid, sqlx::Error> {
    let command_id = Uuid::now_v7();
    let machine_id: Uuid = sqlx::query_scalar(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                               execution_generation, kind, payload, target_command_id, delivery_state)
         select $1, machine_id, conversation_id, execution_id, execution_generation, $3,
                jsonb_build_object('target_command_id', id, 'dispatch_attempt', dispatch_attempt), id, 'pending'
         from commands where id = $2 and kind = 'deliver_input' returning machine_id",
    )
    .bind(command_id)
    .bind(target_command_id)
    .bind(kind)
    .fetch_one(&mut *conn)
    .await?;
    notify_outbox(conn, machine_id).await?;
    Ok(command_id)
}

async fn notify_outbox(conn: &mut PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify('canterlot_outbox', $1::text)")
        .bind(machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

fn queue_event(
    queue: &LockedQueue,
    kind: &'static str,
    execution_id: Option<Uuid>,
    correlation_id: Uuid,
) -> NewEvent {
    NewEvent {
        scope: Scope::Conversation {
            id: queue.conversation_id,
        },
        kind,
        conversation_id: Some(queue.conversation_id),
        execution_id,
        payload: json!({"pause_reasons": queue.pause_reasons.0}),
        correlation_id,
    }
}
