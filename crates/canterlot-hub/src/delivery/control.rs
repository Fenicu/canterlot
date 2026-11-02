use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
    executions,
    idempotency::{self, Begin},
    model::{CompactionProgress, ExecutionState, HoldReason, InputState, PauseReason, RunSlot},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Cancelled,
    CancelRequested { cancel_command_id: Uuid },
    TooLate { state: InputState },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    Requested {
        interrupt_command_id: Option<Uuid>,
        withdraw_command_id: Option<Uuid>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopExecutionOutcome {
    Requested {
        command_id: Uuid,
        force: bool,
    },
    AlreadyRequested {
        command_id: Uuid,
    },
    NothingToStop,
    /// Сопровождения нет: выход только через `resolve-owner-lost`, команда остановки не придёт.
    OwnerLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolveOwnerLostOutcome {
    Requested { command_id: Uuid },
    AlreadyRequested { command_id: Uuid },
    NotOwnerLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactOutcome {
    Requested { command_id: Uuid },
    AlreadyRequested { command_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: &'static str },
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
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "cancel_input";
    let hash = idempotency::request_hash(OPERATION, &json!({"input_id": input_id}));
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    let queue: LockedQueue = sqlx::query_as(
        "select q.* from queue_state q
         join inputs i on i.conversation_id = q.conversation_id
         join conversations c on c.id = q.conversation_id
         where i.id = $1 and i.owner_id = $2 and c.owner_id = $2 for update of q",
    )
    .bind(input_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ControlError::NotFound)?;
    if let Some(outcome) = lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await? {
        return Ok(outcome);
    }
    let (state, command_id, execution_id): (String, Uuid, Option<Uuid>) =
        sqlx::query_as("select i.state, i.command_id, c.execution_id from inputs i join commands c on c.id = i.command_id where i.id = $1")
            .bind(input_id)
            .fetch_one(&mut *conn)
            .await?;
    let state = state
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    let outcome = match state {
        InputState::Queued => {
            // «Отправить сейчас» мог уже создать вливание: отмена ввода должна
            // погасить его в той же транзакции, иначе отменённый текст всё равно
            // уедет в идущий ход. Ушедшее на машину вливание отменяем только
            // отказом: вслепую помечать его завершённым нельзя.
            let steers: Vec<(Uuid, String, bool)> = sqlx::query_as(
                "select id, delivery_state, dispatch_claimed_at is not null from commands
                 where kind = 'steer_input' and target_command_id = $1
                   and delivery_state <> 'completed'
                 order by created_at, id",
            )
            .bind(command_id)
            .fetch_all(&mut *conn)
            .await?;
            if steers
                .iter()
                .any(|(_, state, claimed)| state != "pending" || *claimed)
            {
                return Err(ControlError::InvalidRequest {
                    reason: "steer_in_flight",
                });
            }
            if !steers.is_empty() {
                let ids: Vec<Uuid> = steers.into_iter().map(|(id, _, _)| id).collect();
                sqlx::query(
                    "update commands set delivery_state = 'completed',
                            completed_at = coalesce(completed_at, now()),
                            result = jsonb_build_object('cancelled', true)
                     where id = any($1)",
                )
                .bind(&ids)
                .execute(&mut *conn)
                .await?;
            }
            sqlx::query(
                "update inputs set state = 'cancelled', cancel_requested = false where id = $1",
            )
            .bind(input_id)
            .execute(&mut *conn)
            .await?;
            events::append(
                &mut *conn,
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
                .execute(&mut *conn)
                .await?;
            let cancel_command_id = target_command(&mut *conn, command_id, "cancel_input").await?;
            events::append(
                &mut *conn,
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
        &mut *conn,
        owner_id,
        &idempotency_key,
        OPERATION,
        &hash,
        queue.conversation_id,
        &outcome,
    )
    .await?;
    Ok(outcome)
        })
    })
    .await
}

pub async fn request_stop(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<StopOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "request_stop";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let mut queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            mark_owner_stop(&mut *conn, conversation_id, false).await?;
            crate::crunch::clear_resume_in_tx(&mut *conn, conversation_id, None, correlation_id)
                .await?;
            if !queue.pause_reasons.0.contains(&PauseReason::UserStop) {
                queue.pause_reasons.0.push(PauseReason::UserStop);
            }
            let slot = queue.slot()?;
            queue.save(&mut *conn, &slot, None, correlation_id).await?;
            let live = executions::live(&mut *conn, conversation_id).await?;
            let interrupt_command_id = if let Some((execution_id, generation, _)) = live {
                let existing: Option<Uuid> = sqlx::query_scalar(
                    "select id from commands where conversation_id = $1 and kind = 'interrupt'
             and execution_id = $2 and execution_generation = $3 and delivery_state <> 'completed'
             order by created_at, id limit 1",
                )
                .bind(conversation_id)
                .bind(execution_id)
                .bind(generation)
                .fetch_optional(&mut *conn)
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
                    .fetch_one(&mut *conn)
                    .await?;
                    notify_outbox(&mut *conn, machine_id).await?;
                    command_id
                })
            } else {
                None
            };
            let withdraw_command_id = withdraw_dispatch(&mut *conn, &slot).await?;
            events::append(
                &mut *conn,
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
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

fn set_compaction(queue: &mut LockedQueue, wanted: Option<CompactionProgress>) -> bool {
    let mut found = false;
    for reason in &mut queue.pause_reasons.0 {
        if let PauseReason::SessionStuck { compaction } = reason {
            *compaction = wanted;
            found = true;
        }
    }
    found
}

/// Сжатие тратит токены, поэтому только по действию владельца: между ходами живого
/// исполнения, в застрявшей сессии с пометкой прогресса в причине паузы.
pub async fn request_compaction(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<CompactOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "request_compaction";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let mut queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let slot = queue.slot()?;
            if slot != RunSlot::Free {
                return Err(ControlError::InvalidRequest {
                    reason: "run_in_progress",
                });
            }
            let Some((execution_id, generation, ExecutionState::Running)) =
                executions::live(&mut *conn, conversation_id).await?
            else {
                return Err(ControlError::InvalidRequest {
                    reason: "no_live_execution",
                });
            };
            // Кнопка доступна и вне застрявшей паузы; в ней помечаем прогресс в причине.
            set_compaction(&mut queue, Some(CompactionProgress::Requested));
            let existing: Option<Uuid> = sqlx::query_scalar(
                "select id from commands where conversation_id = $1 and kind = 'compact_session'
                 and execution_id = $2 and execution_generation = $3
                 and delivery_state <> 'completed' order by created_at, id limit 1",
            )
            .bind(conversation_id)
            .bind(execution_id)
            .bind(generation)
            .fetch_optional(&mut *conn)
            .await?;
            let outcome = if let Some(command_id) = existing {
                CompactOutcome::AlreadyRequested { command_id }
            } else {
                let command_id = Uuid::now_v7();
                let machine_id: Uuid = sqlx::query_scalar(
                    "insert into commands (id, machine_id, conversation_id, execution_id,
                                   execution_generation, kind, payload, delivery_state)
                     select $1, machine_id, id, $3, $4, 'compact_session', $5, 'pending'
                     from conversations where id = $2 returning machine_id",
                )
                .bind(command_id)
                .bind(conversation_id)
                .bind(execution_id)
                .bind(generation)
                .bind(json!({"execution_id": execution_id, "execution_generation": generation}))
                .fetch_one(&mut *conn)
                .await?;
                notify_outbox(&mut *conn, machine_id).await?;
                queue
                    .save(&mut *conn, &slot, Some(execution_id), correlation_id)
                    .await?;
                CompactOutcome::Requested { command_id }
            };
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

async fn lock_run(conn: &mut PgConnection, run: &ExecutionRef) -> Result<LockedQueue, sqlx::Error> {
    sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
        .bind(run.conversation_id)
        .fetch_one(conn)
        .await
}

/// Факт воркера: один и тот же отказ подряд. Очередь встаёт, иначе ввод ушёл бы в тот же отказ.
pub async fn on_session_stuck_in_tx(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut queue = lock_run(conn, run).await?;
    let stuck = |reason: &PauseReason| matches!(reason, PauseReason::SessionStuck { .. });
    if queue.pause_reasons.0.iter().any(stuck) {
        return Ok(());
    }
    queue
        .pause_reasons
        .0
        .push(PauseReason::SessionStuck { compaction: None });
    let slot = queue.slot()?;
    queue
        .save(conn, &slot, Some(run.execution_id), correlation_id)
        .await?;
    events::append(
        conn,
        queue_event(
            &queue,
            "queue_paused",
            Some(run.execution_id),
            correlation_id,
        ),
    )
    .await?;
    Ok(())
}

/// Итог сжатия: удача снимает застрявшую паузу и очередь идёт дальше, отказ оставляет
/// карточку. Вне паузы очередь не трогаем: лишнее `queue_resumed` дёрнуло бы ленту.
pub async fn on_compaction_done_in_tx(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    ok: bool,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut queue = lock_run(conn, run).await?;
    if !queue
        .pause_reasons
        .0
        .iter()
        .any(|reason| matches!(reason, PauseReason::SessionStuck { .. }))
    {
        return Ok(());
    }
    if ok {
        queue
            .pause_reasons
            .0
            .retain(|reason| !matches!(reason, PauseReason::SessionStuck { .. }));
    } else {
        set_compaction(&mut queue, Some(CompactionProgress::Failed));
    }
    let slot = queue.slot()?;
    queue
        .save(conn, &slot, Some(run.execution_id), correlation_id)
        .await?;
    events::append(
        conn,
        queue_event(
            &queue,
            if queue.pause_reasons.0.is_empty() {
                "queue_resumed"
            } else {
                "queue_paused"
            },
            Some(run.execution_id),
            correlation_id,
        ),
    )
    .await?;
    Ok(())
}

pub async fn request_stop_execution(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    force: bool,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<StopExecutionOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "request_stop_execution";
    let hash = idempotency::request_hash(
        OPERATION,
        &json!({"conversation_id": conversation_id, "force": force}),
    );
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let outcome =
                stop_locked_in_tx(&mut *conn, queue, conversation_id, force, correlation_id)
                    .await?;
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

/// Остановка исполнения в чужой транзакции, без ключа идемпотентности: повтор ничего не меняет.
pub(crate) async fn stop_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    force: bool,
    correlation_id: Uuid,
) -> Result<StopExecutionOutcome, ControlError> {
    let queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
    stop_locked_in_tx(conn, queue, conversation_id, force, correlation_id).await
}

async fn stop_locked_in_tx(
    conn: &mut PgConnection,
    mut queue: LockedQueue,
    conversation_id: Uuid,
    force: bool,
    correlation_id: Uuid,
) -> Result<StopExecutionOutcome, ControlError> {
    if let Some((_, _, ExecutionState::OwnerLost)) =
        executions::live(&mut *conn, conversation_id).await?
    {
        return Ok(StopExecutionOutcome::OwnerLost);
    }
    mark_owner_stop(&mut *conn, conversation_id, true).await?;
    crate::crunch::clear_resume_in_tx(&mut *conn, conversation_id, None, correlation_id).await?;
    if !queue.pause_reasons.0.contains(&PauseReason::UserStop) {
        queue.pause_reasons.0.push(PauseReason::UserStop);
    }
    let slot = queue.slot()?;
    queue.save(&mut *conn, &slot, None, correlation_id).await?;
    let live = executions::live(&mut *conn, conversation_id).await?;
    let outcome = if let Some((execution_id, generation, _)) = live {
        enqueue_stop(&mut *conn, conversation_id, execution_id, generation, force).await?
    } else {
        StopExecutionOutcome::NothingToStop
    };
    withdraw_dispatch(&mut *conn, &slot).await?;
    events::append(
        &mut *conn,
        queue_event(
            &queue,
            "queue_paused",
            live.map(|(id, _, _)| id),
            correlation_id,
        ),
    )
    .await?;
    Ok(outcome)
}

/// Решение владельца по потерянному исполнению. Воркер сначала проверяет, не живо ли
/// сопровождение, и только потом завершает остаток группы.
pub async fn request_resolve_owner_lost(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
) -> Result<ResolveOwnerLostOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "resolve_owner_lost";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let outcome = match executions::live(&mut *conn, conversation_id).await? {
                Some((execution_id, generation, ExecutionState::OwnerLost)) => {
                    enqueue_resolve(&mut *conn, conversation_id, execution_id, generation).await?
                }
                _ => ResolveOwnerLostOutcome::NotOwnerLost,
            };
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

async fn enqueue_resolve(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    generation: i64,
) -> Result<ResolveOwnerLostOutcome, sqlx::Error> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from commands where conversation_id = $1 and kind = 'resolve_owner_lost'
             and execution_id = $2 and execution_generation = $3 and delivery_state <> 'completed'
         order by created_at, id limit 1",
    )
    .bind(conversation_id)
    .bind(execution_id)
    .bind(generation)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(command_id) = existing {
        return Ok(ResolveOwnerLostOutcome::AlreadyRequested { command_id });
    }
    let command_id = Uuid::now_v7();
    let machine_id: Uuid = sqlx::query_scalar(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                               execution_generation, kind, payload, delivery_state)
         select $1, machine_id, id, $3, $4, 'resolve_owner_lost', '{}', 'pending'
         from conversations where id = $2 returning machine_id",
    )
    .bind(command_id)
    .bind(conversation_id)
    .bind(execution_id)
    .bind(generation)
    .fetch_one(&mut *conn)
    .await?;
    notify_outbox(&mut *conn, machine_id).await?;
    Ok(ResolveOwnerLostOutcome::Requested { command_id })
}

pub(crate) async fn withdraw_dispatch(
    conn: &mut PgConnection,
    slot: &RunSlot,
) -> Result<Option<Uuid>, sqlx::Error> {
    if let RunSlot::Dispatching { input_id, .. } = slot {
        let command_id: Option<Uuid> = sqlx::query_scalar(
            "select command_id from inputs where id = $1 and state in ('dispatched', 'on_machine')",
        )
        .bind(input_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(command_id) = command_id {
            return Ok(Some(
                target_command(conn, command_id, "withdraw_input").await?,
            ));
        }
    }
    Ok(None)
}

pub async fn continue_queue(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
) -> Result<ContinueOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "continue_queue";
    let hash = idempotency::request_hash(OPERATION, &json!({"conversation_id": conversation_id}));
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let _queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let outcome =
                continue_queue_in_tx(&mut *conn, owner_id, conversation_id, correlation_id).await?;
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

pub async fn continue_queue_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<ContinueOutcome, ControlError> {
    let mut queue = lock_conversation(conn, owner_id, conversation_id).await?;
    let owner_lost: bool = sqlx::query_scalar(
        "select exists(select 1 from executions where conversation_id = $1 and state = 'owner_lost')",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    let (remaining, removed): (Vec<_>, Vec<_>) = std::mem::take(&mut queue.pause_reasons.0)
        .into_iter()
        .partition(|reason| {
            matches!(reason, PauseReason::RestorePending)
                || (owner_lost && *reason == PauseReason::OwnerLost)
        });
    let outcome = ContinueOutcome { removed, remaining };
    queue.pause_reasons.0 = outcome.remaining.clone();
    queue
        .save(conn, &queue.slot()?, None, correlation_id)
        .await?;
    events::append(
        conn,
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

async fn enqueue_stop(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    generation: i64,
    force: bool,
) -> Result<StopExecutionOutcome, sqlx::Error> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from commands where conversation_id = $1 and kind = 'stop_execution'
             and execution_id = $2 and execution_generation = $3 and delivery_state <> 'completed'
             and payload->>'force' = $4 order by created_at, id limit 1",
    )
    .bind(conversation_id)
    .bind(execution_id)
    .bind(generation)
    .bind(if force { "true" } else { "false" })
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(command_id) = existing {
        return Ok(StopExecutionOutcome::AlreadyRequested { command_id });
    }
    let command_id = Uuid::now_v7();
    let machine_id: Uuid = sqlx::query_scalar(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                                       execution_generation, kind, payload, delivery_state)
                 select $1, machine_id, id, $3, $4, 'stop_execution', $5, 'pending'
                 from conversations where id = $2 returning machine_id",
    )
    .bind(command_id)
    .bind(conversation_id)
    .bind(execution_id)
    .bind(generation)
    .bind(json!({"force": force}))
    .fetch_one(&mut *conn)
    .await?;
    notify_outbox(&mut *conn, machine_id).await?;
    Ok(StopExecutionOutcome::Requested { command_id, force })
}

/// Хаб отказался от записи журнала этого исполнения: очередь встаёт с видимой причиной,
/// а исполнение останавливается обычным путём, и остаток журнала применяется как всегда.
pub(crate) async fn stop_after_refused_record_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let Some((live, generation, state)) = executions::live(&mut *conn, conversation_id).await?
    else {
        return Ok(());
    };
    if live != execution_id || state == ExecutionState::OwnerLost {
        return Ok(());
    }
    if !queue.pause_reasons.0.contains(&PauseReason::FatalError) {
        queue.pause_reasons.0.push(PauseReason::FatalError);
        let slot = queue.slot()?;
        queue
            .save(&mut *conn, &slot, Some(execution_id), correlation_id)
            .await?;
        events::append(
            &mut *conn,
            queue_event(&queue, "queue_paused", Some(execution_id), correlation_id),
        )
        .await?;
    }
    enqueue_stop(conn, conversation_id, execution_id, generation, true).await?;
    Ok(())
}

async fn mark_owner_stop(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_stop: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("update executions e set owner_stop_requested=owner_stop_requested or $2,owner_stop_input_seq=greatest(owner_stop_input_seq,coalesce((select max(i.input_seq) from inputs i join commands c on c.id=i.command_id where c.execution_id=e.id),0)) where e.conversation_id=$1 and e.state not in ('stopped','failed','owner_lost')")
        .bind(conversation_id).bind(execution_stop).execute(conn).await?;
    Ok(())
}

async fn target_command(
    conn: &mut PgConnection,
    target_command_id: Uuid,
    kind: &str,
) -> Result<Uuid, sqlx::Error> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select c.id from commands c join commands target on target.id = c.target_command_id
         where c.target_command_id = $1 and c.kind = $2 and c.delivery_state <> 'completed'
           and c.payload->>'target_dispatch_attempt' = target.dispatch_attempt::text
         order by c.created_at, c.id limit 1",
    )
    .bind(target_command_id)
    .bind(kind)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(existing) = existing {
        return Ok(existing);
    }
    let command_id = Uuid::now_v7();
    let machine_id: Uuid = sqlx::query_scalar(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                               execution_generation, kind, payload, target_command_id, delivery_state)
         select $1, machine_id, conversation_id, execution_id, execution_generation, $3,
                jsonb_build_object('target_command_id', id, 'target_dispatch_attempt', dispatch_attempt), id, 'pending'
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SendNowOutcome {
    Requested { steer_command_id: Uuid },
    AlreadyRequested { steer_command_id: Uuid },
}

/// «Отправить сейчас»: влить ожидающий ввод в идущий ход, не открывая нового.
/// Действителен, только пока ввод ещё в очереди, а ход идёт; иначе 400 с
/// причиной, которую PWA показывает подписью. Идемпотентен по ключу, а при
/// другом ключе на тот же ввод возвращает существующую команду.
pub async fn request_send_now(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    input_id: Uuid,
    idempotency_key: &str,
    _correlation_id: Uuid,
    machine_capabilities: &[String],
) -> Result<SendNowOutcome, ControlError> {
    idempotency::validate_key(idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "request_send_now";
    let hash = idempotency::request_hash(
        OPERATION,
        &json!({"conversation_id": conversation_id, "input_id": input_id}),
    );
    let idempotency_key = idempotency_key.to_owned();
    let machine_capabilities = machine_capabilities.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let input: Option<(String, Uuid, Value)> = sqlx::query_as(
                "select i.state, i.command_id, i.content from inputs i
                 where i.id = $1 and i.conversation_id = $2",
            )
            .bind(input_id)
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
            let (state, deliver_command_id, content) = input.ok_or(ControlError::NotFound)?;
            if state != InputState::Queued.as_str() {
                return Err(ControlError::InvalidRequest {
                    reason: "input_not_queued",
                });
            }
            // Ход идёт, только если слот занят им и исполнение живое.
            let live = executions::live(&mut *conn, conversation_id).await?;
            let (execution_id, generation, exec_state) = live.ok_or(ControlError::InvalidRequest {
                reason: "no_running_turn",
            })?;
            if !matches!(exec_state, ExecutionState::Running) {
                return Err(ControlError::InvalidRequest {
                    reason: "no_running_turn",
                });
            }
            if !matches!(queue.slot()?, RunSlot::Running { .. }) {
                return Err(ControlError::InvalidRequest {
                    reason: "no_running_turn",
                });
            }
            let running_input_id = match queue.slot()? {
                RunSlot::Running { input_id } => input_id,
                _ => {
                    return Err(ControlError::InvalidRequest {
                        reason: "no_running_turn",
                    });
                }
            };
            let running_command_id: Uuid =
                sqlx::query_scalar("select command_id from inputs where id = $1")
                    .bind(running_input_id)
                    .fetch_one(&mut *conn)
                    .await?;
            let (harness, machine_id): (String, Uuid) =
                sqlx::query_as("select harness, machine_id from conversations where id = $1")
                    .bind(conversation_id)
                    .fetch_one(&mut *conn)
                    .await?;
            if !matches!(harness.as_str(), "claude_code" | "codex" | "opencode") {
                return Err(ControlError::InvalidRequest {
                    reason: "unsupported_harness",
                });
            }
            if !machine_capabilities
                .iter()
                .any(|capability| capability == crate::session::CAPABILITY_SEND_NOW_V1)
            {
                return Err(ControlError::InvalidRequest {
                    reason: "capability_missing",
                });
            }
            // Открытый запрос или сжатие: вливать некуда, отказ с подписью.
            let open_requests: i64 = sqlx::query_scalar(crate::requests::LIVE_OPEN_REQUESTS)
                .bind(conversation_id)
                .fetch_one(&mut *conn)
                .await?;
            let awaiting: bool = sqlx::query_scalar::<_, Option<bool>>(
                "select phase in ('awaiting_permission', 'awaiting_answer') from executions
                 where id = $1",
            )
            .bind(execution_id)
            .fetch_one(&mut *conn)
            .await?
            .unwrap_or(false);
            if open_requests > 0 || awaiting {
                return Err(ControlError::InvalidRequest {
                    reason: "request_open",
                });
            }
            if queue.hold_reasons.0.contains(&HoldReason::ContextMaintenance) {
                return Err(ControlError::InvalidRequest {
                    reason: "compacting",
                });
            }
            let compacting: bool = sqlx::query_scalar(
                "select exists(select 1 from commands where conversation_id = $1
                 and kind = 'compact_session' and execution_id = $2 and delivery_state <> 'completed')",
            )
            .bind(conversation_id)
            .bind(execution_id)
            .fetch_one(&mut *conn)
            .await?;
            if compacting {
                return Err(ControlError::InvalidRequest {
                    reason: "compacting",
                });
            }
            // Повтор другим ключом: та же команда, нового хода нет.
            let existing: Option<Uuid> = sqlx::query_scalar(
                "select id from commands where kind = 'steer_input' and target_command_id = $1
                 and delivery_state <> 'completed' order by created_at, id limit 1",
            )
            .bind(deliver_command_id)
            .fetch_optional(&mut *conn)
            .await?;
            let outcome = if let Some(existing) = existing {
                SendNowOutcome::AlreadyRequested {
                    steer_command_id: existing,
                }
            } else {
                let attachment_ids: Value = sqlx::query_scalar(
                    "select coalesce(payload->'attachment_ids', '[]'::jsonb) from commands where id = $1",
                )
                .bind(deliver_command_id)
                .fetch_optional(&mut *conn)
                .await?
                .unwrap_or(Value::Array(Vec::new()));
                let steer_command_id = Uuid::now_v7();
                sqlx::query(
                    "insert into commands (id, machine_id, conversation_id, execution_id,
                                   execution_generation, kind, payload, target_command_id, delivery_state,
                                   dispatch_attempt)
                 values ($1, $2, $3, $4, $5, 'steer_input', $6, $7, 'pending', 0)",
                )
                .bind(steer_command_id)
                .bind(machine_id)
                .bind(conversation_id)
                .bind(execution_id)
                .bind(generation)
                .bind(json!({
                    "input_id": input_id,
                    "conversation_id": conversation_id,
                    "content": content,
                    "attachment_ids": attachment_ids,
                    "expected_running_command_id": running_command_id.to_string(),
                }))
                .bind(deliver_command_id)
                .execute(&mut *conn)
                .await?;
                notify_outbox(&mut *conn, machine_id).await?;
                SendNowOutcome::Requested { steer_command_id }
            };
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct EditInputOutcome {
    pub input_id: Uuid,
    pub revision: i64,
}

/// Параметры правки ожидающего ввода.
pub struct EditInput {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub input_id: Uuid,
    pub text: String,
    pub attachment_ids: Vec<Uuid>,
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub correlation_id: Uuid,
}

/// Правка ожидающего ввода: замена текста и списка вложений сравнением-и-заменой
/// по ревизии. Действует, только пока ввод ещё в `queued`: выдача, передача,
/// приём и отмена идут мимо. Решение принимается под тем же замком очереди,
/// что и выдача, поэтому правка никогда не гоняется с диспетчером: либо новое
/// содержимое едет следующим ходом целиком, либо правка отклоняется.
pub async fn request_edit_input(
    pool: &PgPool,
    edit: EditInput,
) -> Result<EditInputOutcome, ControlError> {
    let EditInput {
        owner_id,
        conversation_id,
        input_id,
        text,
        attachment_ids,
        expected_revision,
        idempotency_key,
        correlation_id,
    } = edit;
    idempotency::validate_key(&idempotency_key)
        .map_err(|reason| ControlError::InvalidRequest { reason })?;
    const OPERATION: &str = "edit_input";
    let hash = idempotency::request_hash(
        OPERATION,
        &json!({"conversation_id": conversation_id, "input_id": input_id, "text": text, "attachment_ids": attachment_ids, "expected_revision": expected_revision}),
    );
    let idempotency_key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let queue = lock_conversation(&mut *conn, owner_id, conversation_id).await?;
            if let Some(outcome) =
                lookup(&mut *conn, owner_id, &idempotency_key, OPERATION, &hash).await?
            {
                return Ok(outcome);
            }
            let input: Option<(String, i64, Uuid)> = sqlx::query_as(
                "select i.state, i.revision, i.command_id from inputs i
                 where i.id = $1 and i.conversation_id = $2",
            )
            .bind(input_id)
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
            let (state, revision, deliver_command_id) = input.ok_or(ControlError::NotFound)?;
            if state != InputState::Queued.as_str() {
                // Та же граница, что у «Отправить сейчас» и выдачи: выданное,
                // переданное, принятое и отменённое правится только отзывом.
                return Err(ControlError::InvalidRequest {
                    reason: "input_not_queued",
                });
            }
            if revision != expected_revision {
                return Err(ControlError::InvalidRequest {
                    reason: "revision_conflict",
                });
            }
            // Outbox уже выбрал вливание или обычную выдачу: payload мог уйти
            // на машину, поэтому правка закрыта до квитанции.
            let steer: Option<(bool, bool)> = sqlx::query_as(
                "select dispatch_claimed_at is not null,
                        delivery_state = 'stored_on_machine'
                 from commands where kind = 'steer_input'
                   and target_command_id = $1 and delivery_state <> 'completed'
                 order by created_at, id limit 1",
            )
            .bind(deliver_command_id)
            .fetch_optional(&mut *conn)
            .await?;
            let deliver_claimed: bool = sqlx::query_scalar(
                "select delivery_state = 'pending' and dispatch_claimed_at is not null
                 from commands where id = $1",
            )
            .bind(deliver_command_id)
            .fetch_one(&mut *conn)
            .await?;
            if deliver_claimed || steer.is_some_and(|(claimed, stored)| claimed || stored) {
                return Err(ControlError::InvalidRequest {
                    reason: "steer_in_flight",
                });
            }
            if text.trim().is_empty() && attachment_ids.is_empty() {
                return Err(ControlError::InvalidRequest {
                    reason: "empty_content",
                });
            }
            let mut blocks = vec![crate::delivery::submit::ContentBlock::Text { text }];
            if !attachment_ids.is_empty() {
                if crate::attachments::check_ids(&attachment_ids).is_err() {
                    return Err(ControlError::InvalidRequest {
                        reason: "attachment_unavailable",
                    });
                }
                let (catalog, machine) =
                    crate::attachments::gate_for(&mut *conn, conversation_id).await?;
                sqlx::query("delete from input_attachments where input_id = $1")
                    .bind(input_id)
                    .execute(&mut *conn)
                    .await?;
                let refs = crate::attachments::link_in_tx(
                    &mut *conn,
                    owner_id,
                    conversation_id,
                    input_id,
                    &attachment_ids,
                    &catalog,
                    &machine,
                )
                .await
                .map_err(|error| match error {
                    crate::attachments::AttachmentError::NotReady => {
                        ControlError::InvalidRequest {
                            reason: "attachment_not_ready",
                        }
                    }
                    crate::attachments::AttachmentError::NotFound => {
                        ControlError::InvalidRequest {
                            reason: "attachment_not_found",
                        }
                    }
                    _ => ControlError::InvalidRequest {
                        reason: "attachment_unavailable",
                    },
                })?;
                for reference in &refs {
                    blocks.push(
                        crate::delivery::submit::ContentBlock::from_ref(reference)
                            .map_err(|_| ControlError::InvalidRequest {
                                reason: "attachment_unavailable",
                            })?,
                    );
                }
            } else {
                sqlx::query("delete from input_attachments where input_id = $1")
                    .bind(input_id)
                    .execute(&mut *conn)
                    .await?;
            }
            crate::delivery::submit::check_bounds(&blocks).map_err(|_| {
                ControlError::InvalidRequest {
                    reason: "content_too_long",
                }
            })?;
            let content = serde_json::to_value(&blocks)
                .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
            // Единственный источник текста для выдачи — строка команды: воркер
            // читает её при каждой выдаче и повторе, старого текста у него нет.
            sqlx::query("update inputs set content = $2, revision = revision + 1 where id = $1")
                .bind(input_id)
                .bind(&content)
                .execute(&mut *conn)
                .await?;
            sqlx::query(
                "update commands set payload = payload || jsonb_build_object('content', $2::jsonb, 'attachment_ids', $3::jsonb)
                 where id = $1",
            )
            .bind(deliver_command_id)
            .bind(&content)
            .bind(json!(attachment_ids))
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "update commands set payload = payload || jsonb_build_object('content', $2::jsonb, 'attachment_ids', $3::jsonb)
                 where kind = 'steer_input' and target_command_id = $1 and delivery_state = 'pending'",
            )
            .bind(deliver_command_id)
            .bind(&content)
            .bind(json!(attachment_ids))
            .execute(&mut *conn)
            .await?;
            let outcome = EditInputOutcome {
                input_id,
                revision: revision + 1,
            };
            events::append(
                &mut *conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: queue.conversation_id,
                    },
                    kind: "input_state_changed",
                    conversation_id: Some(queue.conversation_id),
                    execution_id: None,
                    payload: json!({
                        "input_id": input_id,
                        "state": "queued",
                        "attempt": 0,
                        "cause": "edited",
                        "revision": revision + 1,
                        "content": content,
                    }),
                    correlation_id,
                },
            )
            .await?;
            record(
                &mut *conn,
                owner_id,
                &idempotency_key,
                OPERATION,
                &hash,
                conversation_id,
                &outcome,
            )
            .await?;
            Ok(outcome)
        })
    })
    .await
}
