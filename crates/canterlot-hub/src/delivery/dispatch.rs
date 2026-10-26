use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    executions,
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
};

#[derive(Debug, Clone, Copy)]
pub struct GateInputs {
    pub machine_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    Closed,
    Paused(Vec<PauseReason>),
    Held(Vec<HoldReason>),
    SlotBusy,
    NoLiveRunningExecution,
    MachineUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    Dispatched {
        input_id: Uuid,
        command_id: Uuid,
        attempt: i32,
        machine_id: Uuid,
        execution_id: Uuid,
        generation: i64,
    },
    Blocked(Vec<BlockReason>),
    Empty,
}

pub async fn dispatch_next(
    pool: &PgPool,
    conversation_id: Uuid,
    gate: GateInputs,
    correlation_id: Uuid,
) -> Result<DispatchOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *tx)
            .await?;
    let slot = queue.slot()?;
    let live = executions::live(&mut tx, conversation_id).await?;
    let mut reasons = Vec::new();
    if queue.closed {
        reasons.push(BlockReason::Closed);
    }
    if !queue.pause_reasons.0.is_empty() {
        reasons.push(BlockReason::Paused(queue.pause_reasons.0.clone()));
    }
    let holds: Vec<_> = queue
        .hold_reasons
        .0
        .iter()
        .filter(|reason| {
            !matches!(
                reason,
                HoldReason::RunInProgress
                    | HoldReason::WorkdirBusy { .. }
                    | HoldReason::AttachmentsPending { .. }
            )
        })
        .cloned()
        .collect();
    if !holds.is_empty() {
        reasons.push(BlockReason::Held(holds));
    }
    if slot != RunSlot::Free {
        reasons.push(BlockReason::SlotBusy);
    }
    if !matches!(live, Some((_, _, ExecutionState::Running))) {
        reasons.push(BlockReason::NoLiveRunningExecution);
    }
    if !gate.machine_available {
        reasons.push(BlockReason::MachineUnavailable);
    }
    if !reasons.is_empty() {
        tx.commit().await?;
        return Ok(DispatchOutcome::Blocked(reasons));
    }
    let head: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select id, command_id from inputs where conversation_id = $1 and state = 'queued'
         order by input_seq limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((input_id, command_id)) = head else {
        tx.commit().await?;
        return Ok(DispatchOutcome::Empty);
    };
    let (execution_id, generation, _) = live.ok_or(sqlx::Error::RowNotFound)?;
    let (attempt, machine_id): (i32, Uuid) = sqlx::query_as(
        "update commands set delivery_state = 'pending', dispatch_attempt = dispatch_attempt + 1,
                execution_id = $2, execution_generation = $3, last_status = null,
                last_status_rank = 0, result = null, stored_at = null, machine_stored_at_ms = null, completed_at = null
         where id = $1 returning dispatch_attempt, machine_id",
    )
    .bind(command_id)
    .bind(execution_id)
    .bind(generation)
    .fetch_one(&mut *tx)
    .await?;
    queue
        .save(
            &mut tx,
            &RunSlot::Dispatching { input_id, attempt },
            Some(execution_id),
            correlation_id,
        )
        .await?;
    sqlx::query("update inputs set state = 'dispatched' where id = $1")
        .bind(input_id)
        .execute(&mut *tx)
        .await?;
    events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "input_state_changed",
            conversation_id: Some(conversation_id),
            execution_id: Some(execution_id),
            payload: json!({"input_id": input_id, "state": "dispatched", "attempt": attempt}),
            correlation_id,
        },
    )
    .await?;
    sqlx::query("select pg_notify('canterlot_outbox', $1::text)")
        .bind(machine_id.to_string())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(DispatchOutcome::Dispatched {
        input_id,
        command_id,
        attempt,
        machine_id,
        execution_id,
        generation,
    })
}
