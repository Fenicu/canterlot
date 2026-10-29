use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    executions,
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
};

#[derive(Debug, Clone)]
pub struct GateInputs {
    pub machine_available: bool,
    pub machine_reconciled: bool,
    pub session_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    Closed,
    Paused(Vec<PauseReason>),
    Held(Vec<HoldReason>),
    SlotBusy,
    NoLiveRunningExecution,
    MachineUnavailable,
    MachineNotReconciled,
    CapabilityMissing { input_id: Uuid },
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

/// Флаг резерва читается без блокировки: между commit наблюдения и правилом ход не начнётся.
async fn derived_reserve(
    conn: &mut sqlx::PgConnection,
    conversation_id: Uuid,
) -> Result<Option<HoldReason>, sqlx::Error> {
    let (harness, overridden): (String, bool) = sqlx::query_as(
        "select harness, coalesce(reserve_override_until > clock_timestamp(), false) from conversations where id = $1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    let Some(provider) = canterlot_proto::harness::provider_of(&harness).filter(|_| !overridden)
    else {
        return Ok(None);
    };
    let reserve: Option<Option<chrono::DateTime<chrono::Utc>>> = sqlx::query_scalar(
        "select coalesce(weekly_reserve_resets_at, weekly_resets_at) from provider_limits
         where provider = $1 and weekly_reserve_active",
    )
    .bind(provider)
    .fetch_optional(conn)
    .await?;
    Ok(reserve.map(|resets_at| HoldReason::WeeklyReserve {
        provider: provider.into(),
        resets_at,
    }))
}

pub async fn dispatch_next(
    pool: &PgPool,
    conversation_id: Uuid,
    gate: GateInputs,
    correlation_id: Uuid,
) -> Result<DispatchOutcome, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let slot = queue.slot()?;
    let live = executions::live(&mut *conn, conversation_id).await?;
    let mut reasons = Vec::new();
    if queue.closed {
        reasons.push(BlockReason::Closed);
    }
    if !queue.pause_reasons.0.is_empty() {
        reasons.push(BlockReason::Paused(queue.pause_reasons.0.clone()));
    }
    let mut holds: Vec<_> = queue
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
    if let Some(reserve) = derived_reserve(&mut *conn, conversation_id).await?
        && !holds.iter().any(|h| matches!((h, &reserve), (HoldReason::WeeklyReserve { provider: a, .. }, HoldReason::WeeklyReserve { provider: b, .. }) if a == b))
    {
        holds.push(reserve);
    }
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
    } else if !gate.machine_reconciled {
        reasons.push(BlockReason::MachineNotReconciled);
    }
    if !reasons.is_empty() {
        return Ok(DispatchOutcome::Blocked(reasons));
    }
    let head: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select id, command_id from inputs where conversation_id = $1 and state = 'queued'
         order by input_seq limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((input_id, command_id)) = head else {
        return Ok(DispatchOutcome::Empty);
    };
    let (pending, rev) =
        crate::conversations::revalidate_pending_in_tx(&mut *conn, conversation_id, correlation_id)
            .await?;
    // Ссылки на вложения и настройки понимает только сеанс с `pilot_v1`.
    let carries_refs: bool = sqlx::query_scalar(
        "select jsonb_array_length(coalesce(payload->'attachment_ids','[]'::jsonb)) > 0 from commands where id = $1",
    )
    .bind(command_id)
    .fetch_one(&mut *conn)
    .await?;
    if (carries_refs || pending.as_object().is_some_and(|v| !v.is_empty()))
        && !gate.session_capabilities.iter().any(|v| v == "pilot_v1")
    {
        return Ok(DispatchOutcome::Blocked(vec![
            BlockReason::CapabilityMissing { input_id },
        ]));
    }
    sqlx::query("update commands set payload=case when $2::jsonb='{}'::jsonb then payload-'settings'-'settings_rev' else (payload-'settings'-'settings_rev') || jsonb_build_object('settings',$2::jsonb,'settings_rev',$3::bigint) end where id=$1")
        .bind(command_id).bind(&pending).bind(rev).execute(&mut *conn).await?;
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
    .fetch_one(&mut *conn)
    .await?;
    let reset: Vec<Uuid> = sqlx::query_scalar(
        "update input_attachments set machine_state='pending', machine_reason=null,
                retryable=null, updated_at=clock_timestamp()
         where input_id=$1 and (machine_state,machine_reason,retryable)
             is distinct from ('pending',null,null) returning attachment_id",
    )
    .bind(input_id)
    .fetch_all(&mut *conn)
    .await?;
    for attachment_id in reset {
        events::append(
            &mut *conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: conversation_id,
                },
                kind: "attachment_state_changed",
                conversation_id: Some(conversation_id),
                execution_id: Some(execution_id),
                payload: json!({"input_id":input_id,"attachment_id":attachment_id,
                    "machine_state":"pending","machine_reason":null}),
                correlation_id,
            },
        )
        .await?;
    }
    queue
        .save(
            &mut *conn,
            &RunSlot::Dispatching { input_id, attempt },
            Some(execution_id),
            correlation_id,
        )
        .await?;
    sqlx::query("update inputs set state = 'dispatched' where id = $1")
        .bind(input_id)
        .execute(&mut *conn)
        .await?;
    events::append(
        &mut *conn,
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
        .execute(&mut *conn)
        .await?;
    Ok(DispatchOutcome::Dispatched {
        input_id,
        command_id,
        attempt,
        machine_id,
        execution_id,
        generation,
    })
        })
    })
    .await
}
