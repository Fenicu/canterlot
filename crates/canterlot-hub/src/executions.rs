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
    #[error("idempotency conflict")]
    IdempotencyConflict,
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
impl From<crate::idempotency::IdempotencyConflict> for ExecutionError {
    fn from(_: crate::idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

pub async fn start_with_key(
    pool: &PgPool,
    owner: Uuid,
    id: Uuid,
    key: &str,
    options: (bool, bool),
    correlation: Uuid,
    #[cfg(feature = "fault-injection")] faults: &crate::fault_injection::Faults,
) -> Result<StartedExecution, ExecutionError> {
    let (resume, continue_queue) = options;
    let owned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(owner)
    .fetch_one(pool)
    .await?;
    if !owned {
        return Err(ExecutionError::ConversationNotFound);
    }
    #[cfg(feature = "fault-injection")]
    let faults = faults.clone();
    let operation = if resume { "resume" } else { "start_execution" };
    let request = json!({"conversation_id":id,"continue_queue":continue_queue});
    Ok(crate::idempotency::run_in_tx(
        pool,
        owner,
        key,
        operation,
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                let started =
                    start_in_tx(conn, owner, id, &StartOptions { resume }, correlation).await?;
                #[cfg(feature = "fault-injection")]
                if resume {
                    faults
                        .pause(crate::fault_injection::Point::ResumeAfterStart)
                        .await;
                    if faults.take_drop(
                        crate::fault_injection::Point::ResumeAfterStart,
                        &id.to_string(),
                    ) {
                        return Err(ExecutionError::Db(sqlx::Error::InvalidArgument(
                            "resume fault".into(),
                        )));
                    }
                }
                if continue_queue {
                    crate::delivery::control::continue_queue_in_tx(conn, owner, id, correlation)
                        .await
                        .map_err(|e| match e {
                            crate::delivery::control::ControlError::Db(e) => ExecutionError::Db(e),
                            _ => ExecutionError::ConversationNotFound,
                        })?;
                }
                Ok::<_, ExecutionError>(started)
            })
        },
    )
    .await?
    .value)
}

pub async fn start(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<StartedExecution, ExecutionError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            start_in_tx(
                &mut *conn,
                owner_id,
                conversation_id,
                &StartOptions { resume: false },
                correlation_id,
            )
            .await
        })
    })
    .await
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
    let harness: Option<String> =
        sqlx::query_scalar("select harness from conversations where id=$1 and owner_id=$2")
            .bind(conversation_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
    // Строка провайдера раньше очереди: возобновление и вход в резерв сериализуются.
    let provider = harness
        .as_deref()
        .and_then(canterlot_proto::harness::provider_of);
    if let Some(provider) = provider {
        crate::limits::share_provider(conn, provider).await?;
    }
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
    let preset_ref: String = row.try_get("instruction_profile")?;
    let preset_revision: Option<i64> = row.try_get("instruction_preset_revision")?;
    // Сессия, созданная без старта, закрепляет версию первым запуском, а не созданием.
    let first_start: bool =
        sqlx::query_scalar("select not exists(select 1 from executions where conversation_id=$1)")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let resolved = crate::presets::resolve_in_tx(
        &mut *conn,
        owner_id,
        &crate::presets::PresetRef::parse(&preset_ref)
            .map_err(|_| ExecutionError::ConversationNotFound)?,
        preset_revision.filter(|_| !first_start),
    )
    .await
    .map_err(|_| ExecutionError::ConversationNotFound)?;
    let instructions = serde_json::json!({
        "mode": resolved.mode,
        "preset_id": resolved.preset_id.map(|id| id.to_string()).unwrap_or_default(),
        "revision": resolved.revision.unwrap_or(0),
        "title": resolved.title,
        "text": resolved.text.unwrap_or_default(),
    });
    let native: Option<String> = if options.resume {
        // Беседа записана только после хода: без него CLI отказывает в --resume.
        sqlx::query_scalar("select e.native_conversation_id from executions e where e.conversation_id=$1 and e.native_conversation_id is not null and exists(select 1 from inputs i join commands c on c.id=i.command_id where c.execution_id=e.id and i.state='accepted') order by e.generation desc limit 1")
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
    if resolved.revision != preset_revision {
        crate::conversations::pin_preset_revision_in_tx(
            conn,
            conversation_id,
            resolved.revision,
            correlation_id,
        )
        .await?;
    }
    crate::conversations::promote_pending_for_launch_in_tx(conn, conversation_id, correlation_id)
        .await?;
    let (model, effort, permission_mode): (String, String, String) =
        sqlx::query_as("select model, effort, permission_mode from conversations where id = $1")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
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
        "model":model,"effort":effort,"permission_mode":permission_mode,"instructions":instructions,
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
    let overridden = row
        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("reserve_override_until")?
        .is_some_and(|until| until > chrono::Utc::now());
    if let Some(provider) = provider.filter(|_| !overridden) {
        for hold in crate::crunch::initial_holds_in_tx(conn, provider).await? {
            if !queue.hold_reasons.0.iter().any(|h| {
                matches!((h, &hold), (HoldReason::WeeklyReserve { provider: a, .. }, HoldReason::WeeklyReserve { provider: b, .. }) if a == b)
            }) {
                queue.hold_reasons.0.push(hold);
            }
        }
    }
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
    let outcome = outcome.map(str::to_owned);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            set_state_in_tx(
                &mut *conn,
                execution_id,
                to,
                outcome.as_deref(),
                correlation_id,
            )
            .await
        })
    })
    .await
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
    if matches!(
        to,
        ExecutionState::Stopped | ExecutionState::Failed | ExecutionState::OwnerLost
    ) {
        crate::requests::close_for_ended_execution_in_tx(
            conn,
            queue.conversation_id,
            execution_id,
            if to == ExecutionState::OwnerLost {
                "owner_lost"
            } else {
                "execution_ended"
            },
            correlation_id,
        )
        .await?;
        let open: i64 = sqlx::query_scalar(crate::requests::LIVE_OPEN_REQUESTS)
            .bind(queue.conversation_id)
            .fetch_one(&mut *conn)
            .await?;
        if open == 0 {
            queue
                .hold_reasons
                .0
                .retain(|reason| *reason != HoldReason::AwaitingUserRequest);
        }
    }
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
