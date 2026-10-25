use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    model::{InputState, PauseReason, RunSlot},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputReceipt {
    Stored,
    TransmissionStarted,
    Accepted,
    AcceptanceUndetermined,
    ExplicitRejection { reason: String },
    RejectedBeforeTransmission { reason: String },
    CancelWon,
    CancelLost,
    Withdrawn,
    DispatchDeferred { reason: String },
}

impl InputReceipt {
    fn rank(&self) -> i32 {
        match self {
            Self::Stored => 1,
            Self::TransmissionStarted => 2,
            _ => 3,
        }
    }

    fn status(&self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::TransmissionStarted => "transmission_started",
            Self::Accepted => "accepted",
            Self::AcceptanceUndetermined => "acceptance_undetermined",
            Self::ExplicitRejection { .. } => "explicit_rejection",
            Self::RejectedBeforeTransmission { .. } => "rejected_before_transmission",
            Self::CancelWon => "cancel_won",
            Self::CancelLost => "cancel_lost",
            Self::Withdrawn => "withdrawn",
            Self::DispatchDeferred { .. } => "dispatch_deferred",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::ExplicitRejection { reason }
            | Self::RejectedBeforeTransmission { reason }
            | Self::DispatchDeferred { reason } => Some(reason),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    UnknownCommand,
    StaleAttempt,
    NotMonotonic,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied { input_state: InputState },
    Ignored(Ignored),
}

#[derive(sqlx::FromRow)]
struct ReceiptTarget {
    input_id: Uuid,
    state: String,
    dispatch_attempt: i32,
    last_status_rank: i32,
    delivery_state: String,
    execution_id: Option<Uuid>,
}

pub async fn apply_input_receipt(
    pool: &PgPool,
    command_id: Uuid,
    attempt: i32,
    receipt: InputReceipt,
    correlation_id: Uuid,
) -> Result<ApplyOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let queue: Option<LockedQueue> = sqlx::query_as(
        "select q.* from queue_state q join commands c on c.conversation_id = q.conversation_id
         where c.id = $1 for update of q",
    )
    .bind(command_id)
    .fetch_optional(&mut *tx)
    .await?;
    let target: Option<ReceiptTarget> = sqlx::query_as(
        "select i.id as input_id, i.state, c.dispatch_attempt, c.last_status_rank,
                c.delivery_state, c.execution_id from commands c join inputs i on i.command_id = c.id
         where c.id = $1 and c.kind = 'deliver_input'",
    ).bind(command_id).fetch_optional(&mut *tx).await?;
    let (Some(mut queue), Some(target)) = (queue, target) else {
        tx.commit().await?;
        return Ok(ApplyOutcome::Ignored(Ignored::UnknownCommand));
    };
    let state: InputState = target
        .state
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    let late_acceptance =
        state == InputState::AcceptanceUnconfirmed && receipt == InputReceipt::Accepted;
    let ignored = if attempt != target.dispatch_attempt {
        Some(Ignored::StaleAttempt)
    } else if receipt.rank() <= target.last_status_rank && !late_acceptance {
        Some(Ignored::NotMonotonic)
    } else if matches!(
        state,
        InputState::Accepted | InputState::Rejected | InputState::Cancelled
    ) {
        Some(Ignored::Terminal)
    } else if state == InputState::Queued
        || (state == InputState::Transmitting
            && matches!(
                receipt,
                InputReceipt::Withdrawn
                    | InputReceipt::DispatchDeferred { .. }
                    | InputReceipt::CancelWon
                    | InputReceipt::RejectedBeforeTransmission { .. }
            ))
    {
        Some(Ignored::NotMonotonic)
    } else {
        None
    };
    if let Some(ignored) = ignored {
        tx.commit().await?;
        return Ok(ApplyOutcome::Ignored(ignored));
    }
    let mut slot = queue.slot()?;
    let mut delivery = target.delivery_state.as_str();
    let mut reject_reason = None;
    let input_state = match &receipt {
        InputReceipt::Stored => {
            delivery = "stored_on_machine";
            InputState::OnMachine
        }
        InputReceipt::TransmissionStarted => InputState::Transmitting,
        InputReceipt::Accepted => {
            delivery = "completed";
            if late_acceptance {
                queue.pause_reasons.0.retain(|reason| {
                    *reason
                        != PauseReason::UnconfirmedDelivery {
                            input_id: target.input_id,
                        }
                });
            } else {
                slot = RunSlot::Running {
                    input_id: target.input_id,
                };
            }
            InputState::Accepted
        }
        InputReceipt::AcceptanceUndetermined => {
            let pause = PauseReason::UnconfirmedDelivery {
                input_id: target.input_id,
            };
            if !queue.pause_reasons.0.contains(&pause) {
                queue.pause_reasons.0.push(pause);
            }
            slot = RunSlot::Free;
            delivery = "completed";
            InputState::AcceptanceUnconfirmed
        }
        InputReceipt::ExplicitRejection { reason }
        | InputReceipt::RejectedBeforeTransmission { reason } => {
            reject_reason = Some(reason.as_str());
            slot = RunSlot::Free;
            delivery = "completed";
            InputState::Rejected
        }
        InputReceipt::CancelWon => {
            slot = RunSlot::Free;
            delivery = "completed";
            InputState::Cancelled
        }
        InputReceipt::CancelLost => state,
        InputReceipt::Withdrawn | InputReceipt::DispatchDeferred { .. } => {
            slot = RunSlot::Free;
            delivery = "held";
            InputState::Queued
        }
    };
    sqlx::query(
        "update inputs set state = $2, reject_reason = coalesce($3, reject_reason),
                agent_confirmed_at = case when $4 then coalesce(agent_confirmed_at, now()) else agent_confirmed_at end,
                cancel_requested = case when $5 then false else cancel_requested end where id = $1",
    )
    .bind(target.input_id).bind(input_state.as_str()).bind(reject_reason)
    .bind(receipt == InputReceipt::Accepted).bind(receipt == InputReceipt::CancelLost)
    .execute(&mut *tx).await?;
    sqlx::query(
        "update commands set delivery_state = $2, last_status = $3, last_status_rank = $4, result = $5,
                stored_at = case when $6 then coalesce(stored_at, now()) else stored_at end,
                completed_at = case when $2 = 'completed' then coalesce(completed_at, now()) else completed_at end
         where id = $1",
    )
    .bind(command_id).bind(delivery).bind(receipt.status()).bind(receipt.rank())
    .bind(receipt.reason().map(|reason| json!({"reason": reason})))
    .bind(receipt == InputReceipt::Stored).execute(&mut *tx).await?;
    queue.save(&mut tx, &slot).await?;
    events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Conversation {
                id: queue.conversation_id,
            },
            kind: "input_state_changed",
            conversation_id: Some(queue.conversation_id),
            execution_id: target.execution_id,
            payload: json!({"input_id": target.input_id, "state": input_state, "attempt": attempt}),
            correlation_id,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(ApplyOutcome::Applied { input_state })
}

pub async fn apply_run_finished(
    pool: &PgPool,
    execution_id: Uuid,
    input_command_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let queue: Option<LockedQueue> = sqlx::query_as(
        "select q.* from queue_state q join commands c on c.conversation_id = q.conversation_id
         where c.id = $1 for update of q",
    )
    .bind(input_command_id)
    .fetch_optional(&mut *tx)
    .await?;
    let input_id: Option<Uuid> = sqlx::query_scalar(
        "select i.id from inputs i join commands c on c.id = i.command_id
         where c.id = $1 and c.execution_id = $2 and c.kind = 'deliver_input'",
    )
    .bind(input_command_id)
    .bind(execution_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let (Some(mut queue), Some(input_id)) = (queue, input_id)
        && queue.slot()? == (RunSlot::Running { input_id })
    {
        queue.save(&mut tx, &RunSlot::Free).await?;
        events::append(
            &mut tx,
            NewEvent {
                scope: Scope::Conversation {
                    id: queue.conversation_id,
                },
                kind: "run_finished",
                conversation_id: Some(queue.conversation_id),
                execution_id: Some(execution_id),
                payload: json!({"input_id": input_id, "input_command_id": input_command_id}),
                correlation_id,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
