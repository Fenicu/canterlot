use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    executions,
    model::{HoldReason, InputState, PauseReason, RunSlot},
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
    cancel_requested: bool,
    dispatch_attempt: i32,
    last_status_rank: i32,
    delivery_state: String,
    execution_id: Option<Uuid>,
    execution_state: Option<String>,
    command_kind: String,
}

pub async fn apply_input_receipt(
    pool: &PgPool,
    command_id: Uuid,
    attempt: i32,
    receipt: InputReceipt,
    correlation_id: Uuid,
) -> Result<ApplyOutcome, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            apply_input_receipt_in_tx(&mut *conn, command_id, attempt, receipt, correlation_id)
                .await
        })
    })
    .await
}

pub async fn apply_input_receipt_in_tx(
    conn: &mut PgConnection,
    command_id: Uuid,
    attempt: i32,
    receipt: InputReceipt,
    correlation_id: Uuid,
) -> Result<ApplyOutcome, sqlx::Error> {
    let queue: Option<LockedQueue> = sqlx::query_as(
        "select q.* from queue_state q join commands c on c.conversation_id = q.conversation_id
         where c.id = $1 for update of q",
    )
    .bind(command_id)
    .fetch_optional(&mut *conn)
    .await?;
    let target: Option<ReceiptTarget> = sqlx::query_as(
        "select i.id as input_id, i.state, i.cancel_requested, c.dispatch_attempt, c.last_status_rank,
                c.delivery_state, c.execution_id, e.state as execution_state, c.kind as command_kind
         from commands c join inputs i on i.command_id = case when c.kind = 'steer_input'
                then c.target_command_id else c.id end
         left join executions e on e.id = c.execution_id
         where c.id = $1 and c.kind in ('deliver_input', 'steer_input')",
    ).bind(command_id).fetch_optional(&mut *conn).await?;
    let (Some(mut queue), Some(target)) = (queue, target) else {
        return Ok(ApplyOutcome::Ignored(Ignored::UnknownCommand));
    };
    // Итог steer-команды: ввод влился в идущий ход (или остался в очереди),
    // слот хода при этом не трогается — второй ход не открывается.
    let steer = target.command_kind == "steer_input";
    let state: InputState = target
        .state
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    if steer
        && matches!(
            receipt,
            InputReceipt::Withdrawn | InputReceipt::CancelWon | InputReceipt::CancelLost
        )
    {
        return Ok(ApplyOutcome::Ignored(Ignored::NotMonotonic));
    }
    if matches!(
        receipt,
        InputReceipt::Withdrawn
            | InputReceipt::DispatchDeferred { .. }
            | InputReceipt::CancelWon
            | InputReceipt::CancelLost
    ) {
        sqlx::query(
            "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now())
             where target_command_id = $1 and kind in ('cancel_input', 'withdraw_input')
               and delivery_state <> 'completed' and payload->>'target_dispatch_attempt' = $2::text",
        )
        .bind(command_id)
        .bind(attempt.to_string())
        .execute(&mut *conn)
        .await?;
    }
    let late_acceptance =
        state == InputState::AcceptanceUnconfirmed && receipt == InputReceipt::Accepted;
    let ignored = if attempt != target.dispatch_attempt {
        Some(Ignored::StaleAttempt)
    } else if receipt == InputReceipt::CancelLost {
        (!target.cancel_requested).then_some(Ignored::NotMonotonic)
    } else if receipt.rank() <= target.last_status_rank && !late_acceptance {
        Some(Ignored::NotMonotonic)
    } else if matches!(
        state,
        InputState::Accepted | InputState::Rejected | InputState::Cancelled
    ) {
        Some(Ignored::Terminal)
    } else if steer {
        // Steer действует только на ввод, который всё ещё ждёт в очереди
        // (или на запоздалое доказательство непринятого): ушедший дальше ввод
        // принадлежит обычной доставке. Хранение и начало передачи steer
        // не присылает.
        if state != InputState::Queued && !late_acceptance
            || matches!(
                receipt,
                InputReceipt::Stored | InputReceipt::TransmissionStarted
            )
        {
            Some(Ignored::NotMonotonic)
        } else {
            None
        }
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
        return Ok(ApplyOutcome::Ignored(ignored));
    }
    let mut slot = queue.slot()?;
    match &receipt {
        InputReceipt::Accepted => {
            crate::conversations::promote_settings_in_tx(conn, command_id, correlation_id).await?
        }
        InputReceipt::ExplicitRejection { reason }
        | InputReceipt::RejectedBeforeTransmission { reason }
            if crate::conversations::SETTINGS_REJECTION_REASONS.contains(&reason.as_str()) =>
        {
            crate::conversations::drop_settings_rejected_for_in_tx(
                conn,
                command_id,
                reason,
                correlation_id,
            )
            .await?
        }
        _ => {}
    }
    let mut delivery = target.delivery_state.as_str();
    let reason = receipt.reason().map(sanitize_reason);
    let mut reject_reason = None;
    let input_state = match &receipt {
        InputReceipt::Stored => {
            delivery = "stored_on_machine";
            InputState::OnMachine
        }
        InputReceipt::TransmissionStarted => {
            delivery = "stored_on_machine";
            InputState::Transmitting
        }
        InputReceipt::Accepted => {
            delivery = "completed";
            if late_acceptance {
                queue.pause_reasons.0.retain(|reason| {
                    *reason
                        != PauseReason::UnconfirmedDelivery {
                            input_id: target.input_id,
                        }
                });
                // Доказательство пришло после паузы, но ход уже начался: слот снова занят.
                // Иначе хаб до конца хода считал бы исполнение свободным.
                // Steer слот не занимает: влитый ввод принадлежит чужому ходу.
                if !steer
                    && slot == RunSlot::Free
                    && matches!(
                        target.execution_state.as_deref(),
                        Some("created" | "starting" | "running" | "stopping")
                    )
                {
                    slot = RunSlot::Running {
                        input_id: target.input_id,
                    };
                }
            } else if !steer
                && matches!(slot, RunSlot::Dispatching { input_id, .. } if input_id == target.input_id)
            {
                slot = if matches!(
                    target.execution_state.as_deref(),
                    Some("stopped" | "failed")
                ) {
                    RunSlot::Free
                } else {
                    RunSlot::Running {
                        input_id: target.input_id,
                    }
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
            // Steer не открывал ход: слот чужого хода остаётся занят.
            if !steer {
                slot = RunSlot::Free;
            }
            delivery = "completed";
            InputState::AcceptanceUnconfirmed
        }
        InputReceipt::ExplicitRejection { .. } if steer => {
            // Отказ влить ввод в идущий ход не губит ввод: он остаётся в
            // очереди и уйдёт обычным следующим ходом. Причина остаётся у
            // команды вливания и в событии.
            delivery = "completed";
            InputState::Queued
        }
        InputReceipt::ExplicitRejection { .. }
        | InputReceipt::RejectedBeforeTransmission { .. } => {
            reject_reason = reason.as_deref();
            if !steer {
                slot = RunSlot::Free;
            }
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
            if steer {
                // Ход кончился между кликом и доставкой: ввод остаётся в очереди
                // и уйдёт следующим ходом, дубля нет. Отказ воркера (запрос,
                // сжатие) — та же ветвь: ввод не потерян, подпись показала хаб.
                delivery = "completed";
                InputState::Queued
            } else {
                if let InputReceipt::DispatchDeferred { reason } = &receipt
                    && executions::live(conn, queue.conversation_id)
                        .await?
                        .is_some_and(|(id, _, _)| Some(id) == target.execution_id)
                {
                    let hold = if reason == "storage_low" {
                        HoldReason::StorageLow
                    } else {
                        HoldReason::NoLiveExecution
                    };
                    if !queue.hold_reasons.0.contains(&hold) {
                        queue.hold_reasons.0.push(hold);
                    }
                }
                slot = RunSlot::Free;
                if target.cancel_requested {
                    delivery = "completed";
                    InputState::Cancelled
                } else {
                    delivery = "held";
                    InputState::Queued
                }
            }
        }
    };
    if receipt == InputReceipt::CancelLost {
        sqlx::query("update inputs set cancel_requested = false where id = $1")
            .bind(target.input_id)
            .execute(&mut *conn)
            .await?;
    } else {
        sqlx::query(
            "update inputs set state = $2, reject_reason = coalesce($3, reject_reason),
                    agent_confirmed_at = case when $4 then coalesce(agent_confirmed_at, now()) else agent_confirmed_at end
             where id = $1",
        )
        .bind(target.input_id)
        .bind(input_state.as_str())
        .bind(reject_reason)
        .bind(receipt == InputReceipt::Accepted)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "update commands set delivery_state = $2, last_status = $3, last_status_rank = $4, result = $5,
                    stored_at = case when $6 then coalesce(stored_at, now()) else stored_at end,
                    completed_at = case when $2 = 'completed' then coalesce(completed_at, now()) else completed_at end
             where id = $1",
        )
        .bind(command_id)
        .bind(delivery)
        .bind(receipt.status())
        .bind(receipt.rank())
        .bind(reason.as_ref().map(|reason| json!({"reason": reason})))
        .bind(matches!(receipt, InputReceipt::Stored | InputReceipt::TransmissionStarted))
        .execute(&mut *conn)
        .await?;
        queue
            .save(conn, &slot, target.execution_id, correlation_id)
            .await?;
    }
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: queue.conversation_id,
            },
            kind: "input_state_changed",
            conversation_id: Some(queue.conversation_id),
            execution_id: target.execution_id,
            payload: if steer && input_state == InputState::Queued {
                let cause = if matches!(receipt, InputReceipt::ExplicitRejection { .. }) {
                    "steer_rejected"
                } else {
                    "steer_deferred"
                };
                let mut payload = json!({"input_id": target.input_id, "state": input_state, "attempt": attempt, "cause": cause, "command_id": command_id});
                if let Some(reason) = &reason {
                    payload["reason"] = json!(reason);
                }
                payload
            } else {
                json!({"input_id": target.input_id, "state": input_state, "attempt": attempt, "cause": receipt.status()})
            },
            correlation_id,
        },
    )
    .await?;
    if matches!(
        input_state,
        InputState::Transmitting
            | InputState::Accepted
            | InputState::AcceptanceUnconfirmed
            | InputState::Rejected
    ) {
        sqlx::query("update inputs set feed_seq=coalesce(feed_seq,$2) where id=$1")
            .bind(target.input_id)
            .bind(event.scope_seq)
            .execute(&mut *conn)
            .await?;
    }
    Ok(ApplyOutcome::Applied { input_state })
}

pub async fn apply_run_finished(
    pool: &PgPool,
    execution_id: Uuid,
    input_command_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            apply_run_finished_in_tx(&mut *conn, execution_id, input_command_id, correlation_id)
                .await
        })
    })
    .await
}

pub async fn apply_run_finished_in_tx(
    conn: &mut PgConnection,
    execution_id: Uuid,
    input_command_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let queue: Option<LockedQueue> = sqlx::query_as(
        "select q.* from queue_state q join commands c on c.conversation_id = q.conversation_id
         where c.id = $1 for update of q",
    )
    .bind(input_command_id)
    .fetch_optional(&mut *conn)
    .await?;
    let input_id: Option<Uuid> = sqlx::query_scalar(
        "select i.id from inputs i join commands c on c.id = i.command_id
         where c.id = $1 and c.execution_id = $2 and c.kind = 'deliver_input'",
    )
    .bind(input_command_id)
    .bind(execution_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let (Some(mut queue), Some(input_id)) = (queue, input_id)
        && queue.slot()? == (RunSlot::Running { input_id })
    {
        queue
            .save(conn, &RunSlot::Free, Some(execution_id), correlation_id)
            .await?;
        events::append(
            conn,
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
    Ok(())
}

pub(crate) fn sanitize_reason(reason: &str) -> String {
    let mut reason = reason.replace('\0', "");
    if reason.len() > 1024 {
        let mut end = 1024 - '…'.len_utf8();
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
        reason.push('…');
    }
    reason
}

pub(crate) async fn store_machine_time_in_tx(
    conn: &mut PgConnection,
    command_id: Uuid,
    attempt: i32,
    receipt: &InputReceipt,
    machine_time_ms: i64,
) -> Result<(), sqlx::Error> {
    match receipt {
        InputReceipt::Stored => {
            sqlx::query("update commands set machine_stored_at_ms = coalesce(machine_stored_at_ms, $3) where id = $1 and dispatch_attempt = $2")
                .bind(command_id).bind(attempt).bind(machine_time_ms).execute(conn).await?;
        }
        InputReceipt::TransmissionStarted | InputReceipt::Accepted => {
            sqlx::query("update inputs i set transmitted_machine_ms = case when $4 then coalesce(transmitted_machine_ms, $3) else transmitted_machine_ms end, agent_confirmed_machine_ms = case when not $4 then coalesce(agent_confirmed_machine_ms, $3) else agent_confirmed_machine_ms end from commands c where c.id = $1 and c.dispatch_attempt = $2 and i.command_id = c.id")
                .bind(command_id).bind(attempt).bind(machine_time_ms).bind(*receipt == InputReceipt::TransmissionStarted).execute(conn).await?;
        }
        _ => {}
    }
    Ok(())
}
