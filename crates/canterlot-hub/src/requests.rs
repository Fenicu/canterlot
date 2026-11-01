use std::str::FromStr;

use canterlot_proto::requests::DecisionPayload;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub execution_id: Uuid,
    pub generation: i64,
    pub kind: RequestKind,
    pub native_request_id: String,
    pub state: RequestState,
    pub request_payload: serde_json::Value,
    pub decision_payload: Option<serde_json::Value>,
    pub winner_decision_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RequestState {
    Open,
    DecisionSaved,
    DecisionTransmitting,
    DecisionApplied,
    DecisionUnconfirmed,
    DecisionNotApplied,
    ClosedWithoutDecision,
}

impl RequestState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::DecisionSaved => "decision_saved",
            Self::DecisionTransmitting => "decision_transmitting",
            Self::DecisionApplied => "decision_applied",
            Self::DecisionUnconfirmed => "decision_unconfirmed",
            Self::DecisionNotApplied => "decision_not_applied",
            Self::ClosedWithoutDecision => "closed_without_decision",
        }
    }
}

impl FromStr for RequestState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "decision_saved" => Ok(Self::DecisionSaved),
            "decision_transmitting" => Ok(Self::DecisionTransmitting),
            "decision_applied" => Ok(Self::DecisionApplied),
            "decision_unconfirmed" => Ok(Self::DecisionUnconfirmed),
            "decision_not_applied" => Ok(Self::DecisionNotApplied),
            "closed_without_decision" => Ok(Self::ClosedWithoutDecision),
            other => Err(format!("unknown request state: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RequestKind {
    Permission,
    Question,
    SpawnApproval,
}

impl RequestKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::SpawnApproval => "spawn_approval",
        }
    }
}

impl FromStr for RequestKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "permission" => Ok(Self::Permission),
            "question" => Ok(Self::Question),
            "spawn_approval" => Ok(Self::SpawnApproval),
            other => Err(format!("unknown request kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewRequest {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub execution_id: Uuid,
    pub generation: i64,
    pub kind: RequestKind,
    pub native_request_id: String,
    pub request_payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionResult {
    Committed {
        row: RequestRow,
        command_id: Uuid,
    },
    IdempotentReplay {
        row: RequestRow,
    },
    Conflict {
        winner_decision: serde_json::Value,
        current_state: RequestState,
    },
    StaleGeneration,
    AlreadyClosed {
        current_state: RequestState,
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RequestDomainError {
    #[error("request not found: {0}")]
    NotFound(Uuid),
    #[error("conversation not found: {0}")]
    ConversationNotFound(Uuid),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Значение ответа на секретный вопрос во всём, кроме команды доставки воркеру.
pub const REDACTED_ANSWER: &str = "[redacted]";

/// Копия решения без ответов на секретные вопросы (`is_secret`): её видят события,
/// снимки, ответы HTTP и хранилище ключей идемпотентности.
pub fn redact_decision(
    request_payload: &serde_json::Value,
    decision: &serde_json::Value,
) -> serde_json::Value {
    let secret = |value: &serde_json::Value| {
        ["is_secret", "isSecret"]
            .iter()
            .any(|key| value.get(key) == Some(&serde_json::Value::Bool(true)))
    };
    let everything = secret(request_payload);
    let secret_ids: std::collections::BTreeSet<&str> = request_payload
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|question| secret(question))
        .filter_map(|question| question.get("id").and_then(serde_json::Value::as_str))
        .collect();
    let mut decision = decision.clone();
    if let Some(answers) = decision
        .get_mut("answers")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (id, answer) in answers.iter_mut() {
            if everything || secret_ids.contains(id.as_str()) {
                *answer = json!(REDACTED_ANSWER);
            }
        }
    }
    decision
}

pub async fn create_request_in_tx(
    tx: &mut PgConnection,
    req: &NewRequest,
) -> Result<RequestRow, sqlx::Error> {
    sqlx::query_as::<_, RequestRow>(
        r#"
        insert into requests (
            id, conversation_id, execution_id, generation,
            kind, native_request_id, state, request_payload,
            decision_payload, winner_decision_id, idempotency_key,
            created_at, updated_at
        )
        values (
            $1, $2, $3, $4,
            $5, $6, 'open', $7,
            null, null, null,
            now(), now()
        )
        returning
            id, conversation_id, execution_id, generation,
            kind, native_request_id, state, request_payload,
            decision_payload, winner_decision_id, idempotency_key,
            created_at, updated_at
        "#,
    )
    .bind(req.id)
    .bind(req.conversation_id)
    .bind(req.execution_id)
    .bind(req.generation)
    .bind(req.kind)
    .bind(&req.native_request_id)
    .bind(&req.request_payload)
    .fetch_one(tx)
    .await
}

pub async fn save_decision_in_tx(
    tx: &mut PgConnection,
    conversation_id: Uuid,
    request_id: Uuid,
    generation: i64,
    decision: &DecisionPayload,
    idempotency_key: &str,
) -> Result<DecisionResult, RequestDomainError> {
    // 1. Порядок блокировок хаба, шаг 7: блокировка строки conversations FOR NO KEY UPDATE
    let (machine_id, owner_id): (Uuid, Uuid) = sqlx::query_as(
        "select machine_id, owner_id from conversations where id = $1 for no key update",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RequestDomainError::ConversationNotFound(conversation_id))?;

    // 2. Порядок блокировок хаба, шаг 10.5: блокировка строки requests FOR UPDATE по id
    let row: RequestRow = sqlx::query_as(
        r#"
        select
            id, conversation_id, execution_id, generation,
            kind, native_request_id, state, request_payload,
            decision_payload, winner_decision_id, idempotency_key,
            created_at, updated_at
        from requests
        where id = $1 and conversation_id = $2
        for update
        "#,
    )
    .bind(request_id)
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RequestDomainError::NotFound(request_id))?;

    // 3. Проверка поколения выполнения
    if generation != row.generation {
        return Ok(DecisionResult::StaleGeneration);
    }

    // 4. Проверка закрытия запроса или продвижения поколения выполнения
    let latest_gen: Option<i64> =
        sqlx::query_scalar("select max(generation) from executions where conversation_id = $1")
            .bind(conversation_id)
            .fetch_optional(&mut *tx)
            .await?;

    if latest_gen.unwrap_or(row.generation) > row.generation {
        return Ok(DecisionResult::AlreadyClosed {
            current_state: row.state,
            reason: "generation_advanced".into(),
        });
    }

    if row.state == RequestState::ClosedWithoutDecision {
        return Ok(DecisionResult::AlreadyClosed {
            current_state: row.state,
            reason: "closed_without_decision".into(),
        });
    }

    // 5. Правило единственного победителя (Sole Winner Rule, AT-D-01)
    if row.state == RequestState::Open {
        let winner_decision_id = Uuid::now_v7();
        let decision_json = serde_json::to_value(decision)?;
        // Полный ответ нужен только команде доставки; строка запроса хранит копию без секретов.
        let visible = redact_decision(&row.request_payload, &decision_json);

        let updated_row: RequestRow = sqlx::query_as(
            r#"
            update requests
            set
                state = 'decision_saved',
                winner_decision_id = $3,
                decision_payload = $4,
                idempotency_key = $5,
                updated_at = now()
            where id = $1 and conversation_id = $2
            returning
                id, conversation_id, execution_id, generation,
                kind, native_request_id, state, request_payload,
                decision_payload, winner_decision_id, idempotency_key,
                created_at, updated_at
            "#,
        )
        .bind(request_id)
        .bind(conversation_id)
        .bind(winner_decision_id)
        .bind(&visible)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await?;

        // Одобрение запуска выполняет хаб: на машину решение не уходит.
        let command_id = if row.kind == RequestKind::SpawnApproval {
            crate::session_tools::spawn::queue_decision_in_tx(tx, owner_id, conversation_id, row.id)
                .await?
        } else {
            let command_id = Uuid::now_v7();
            let command_payload = json!({
                "request_id": row.id.to_string(),
                "native_request_id": row.native_request_id,
                "decision_id": winner_decision_id.to_string(),
                "generation": row.generation,
                "kind": row.kind.as_str(),
                "decision_payload": decision_json,
            });

            sqlx::query(
                r#"
                insert into commands (
                    id, machine_id, conversation_id, execution_id,
                    execution_generation, kind, payload, delivery_state
                ) values (
                    $1, $2, $3, $4,
                    $5, 'deliver_decision', $6, 'pending'
                )
                "#,
            )
            .bind(command_id)
            .bind(machine_id)
            .bind(row.conversation_id)
            .bind(row.execution_id)
            .bind(row.generation)
            .bind(&command_payload)
            .execute(&mut *tx)
            .await?;

            // Уведомление outbox о появлении исходящей команды
            sqlx::query("select pg_notify('canterlot_outbox', $1)")
                .bind(machine_id.to_string())
                .execute(&mut *tx)
                .await?;

            command_id
        };

        // Шаг 12 порядка блокировок: публикация события request_decided
        crate::events::append(
            tx,
            crate::events::NewEvent {
                scope: crate::events::Scope::Conversation {
                    id: conversation_id,
                },
                kind: "request_decided",
                conversation_id: Some(conversation_id),
                execution_id: Some(row.execution_id),
                payload: json!({
                    "request_id": row.id,
                    "winner_decision_id": winner_decision_id,
                    "decision": visible,
                }),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await?;

        Ok(DecisionResult::Committed {
            row: updated_row,
            command_id,
        })
    } else {
        // Запрос уже решён
        if row.idempotency_key.as_deref() == Some(idempotency_key) {
            Ok(DecisionResult::IdempotentReplay { row })
        } else {
            Ok(DecisionResult::Conflict {
                winner_decision: row.decision_payload.unwrap_or(serde_json::Value::Null),
                current_state: row.state,
            })
        }
    }
}

/// Гасит команду доставки решения только этого запроса и решения. Погашенная команда
/// на машину больше не уходит, поэтому ответ из неё убирается.
pub(crate) async fn complete_decision_command_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    request_id: Uuid,
    decision_id: Uuid,
    result: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, clock_timestamp()),
                result = $3, payload = payload - 'decision_payload'
         where kind = 'deliver_decision' and delivery_state <> 'completed'
           and conversation_id = $4 and payload->>'request_id' = $1 and payload->>'decision_id' = $2",
    )
    .bind(request_id.to_string())
    .bind(decision_id.to_string())
    .bind(result)
    .bind(conversation_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Снимает удержание, когда у живых исполнений разговора не осталось открытых запросов.
pub(crate) async fn clear_awaiting_hold_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let open: i64 = sqlx::query_scalar(LIVE_OPEN_REQUESTS)
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    if open > 0 {
        return Ok(());
    }
    let queue: Option<crate::delivery::LockedQueue> =
        sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(mut queue) = queue else {
        return Ok(());
    };
    if queue
        .hold_reasons
        .0
        .contains(&crate::model::HoldReason::AwaitingUserRequest)
    {
        queue
            .hold_reasons
            .0
            .retain(|reason| *reason != crate::model::HoldReason::AwaitingUserRequest);
        let slot = queue.slot()?;
        queue
            .save(conn, &slot, Some(execution_id), correlation_id)
            .await?;
    }
    Ok(())
}

/// Команда решения отвергнута хабом или воркером до передачи агенту: решение уже не
/// дойдёт, и запрос не должен навсегда остаться в `decision_saved`.
pub(crate) async fn fail_decision_command_in_tx(
    conn: &mut PgConnection,
    command_id: Uuid,
    attempt: i32,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let target: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "select conversation_id, payload->>'request_id' from commands
         where id = $1 and kind = 'deliver_decision' and dispatch_attempt = $2
           and delivery_state <> 'completed'",
    )
    .bind(command_id)
    .bind(attempt)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((conversation_id, request_id)) = target else {
        return Ok(());
    };
    // Порядок хаба: очередь раньше строки запроса.
    sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let request_id = request_id.and_then(|id| Uuid::parse_str(&id).ok());
    let request: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "select id, execution_id, state from requests
         where id = $1 and conversation_id = $2 for update",
    )
    .bind(request_id)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let reason = crate::delivery::receipts::sanitize_reason(reason);
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, clock_timestamp()),
                result = $2, payload = payload - 'decision_payload'
         where id = $1",
    )
    .bind(command_id)
    .bind(json!({"rejected": reason}))
    .execute(&mut *conn)
    .await?;
    let Some((id, execution_id, state)) = request else {
        return Ok(());
    };
    if state != RequestState::DecisionSaved.as_str() {
        return Ok(());
    }
    sqlx::query("update requests set state = $2, updated_at = clock_timestamp() where id = $1")
        .bind(id)
        .bind(RequestState::DecisionNotApplied.as_str())
        .execute(&mut *conn)
        .await?;
    crate::events::append(
        conn,
        crate::events::NewEvent {
            scope: crate::events::Scope::Conversation {
                id: conversation_id,
            },
            kind: "request_closed",
            conversation_id: Some(conversation_id),
            execution_id: Some(execution_id),
            payload: json!({"request_id": id, "reason": reason}),
            correlation_id,
        },
    )
    .await?;
    clear_awaiting_hold_in_tx(conn, conversation_id, execution_id, correlation_id).await
}

/// Исполнение закончилось без `RequestClosed`: незавершённые запросы закрываются,
/// чтобы карточка и удержание очереди не жили вечно. Очередь разговора уже заблокирована.
pub(crate) async fn close_for_ended_execution_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Сохранённое решение о запуске выполнит задание: живой родитель ему не нужен (D21).
    let rows: Vec<(Uuid, String, Option<Uuid>, RequestKind, serde_json::Value)> = sqlx::query_as(
        "select id, state, winner_decision_id, kind, request_payload from requests
         where execution_id = $1 and state in ('open', 'decision_saved', 'decision_transmitting')
           and not (kind = 'spawn_approval' and state = 'decision_saved')
         order by id for update",
    )
    .bind(execution_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, state, winner, kind, payload) in rows {
        // Команда на машине могла дойти до агента, а факты сопровождения идут отдельным
        // журналом и могут опоздать; решения дальше хаба не ушло только у `pending`.
        let on_machine: bool = sqlx::query_scalar(
            "select exists(select 1 from commands
             where kind = 'deliver_decision' and delivery_state = 'stored_on_machine'
               and conversation_id = $1 and payload->>'request_id' = $2 and payload->>'decision_id' = $3)",
        )
        .bind(conversation_id)
        .bind(id.to_string())
        .bind(winner.map(|winner| winner.to_string()))
        .fetch_one(&mut *conn)
        .await?;
        let (next, event_reason, applied) = match state.as_str() {
            "open" => ("closed_without_decision", reason, json!(false)),
            "decision_saved" if !on_machine => ("decision_not_applied", reason, json!(false)),
            _ => (
                "decision_unconfirmed",
                "unconfirmed",
                serde_json::Value::Null,
            ),
        };
        sqlx::query("update requests set state = $2, updated_at = clock_timestamp() where id = $1")
            .bind(id)
            .bind(next)
            .execute(&mut *conn)
            .await?;
        if let Some(winner) = winner {
            complete_decision_command_in_tx(
                conn,
                conversation_id,
                id,
                winner,
                json!({"applied": applied, "reason": reason}),
            )
            .await?;
        }
        crate::events::append(
            conn,
            crate::events::NewEvent {
                scope: crate::events::Scope::Conversation {
                    id: conversation_id,
                },
                kind: "request_closed",
                conversation_id: Some(conversation_id),
                execution_id: Some(execution_id),
                payload: json!({"request_id": id, "reason": event_reason}),
                correlation_id,
            },
        )
        .await?;
        if kind == RequestKind::SpawnApproval {
            crate::session_tools::spawn::queue_lapsed_note_in_tx(
                conn,
                conversation_id,
                id,
                &payload,
            )
            .await?;
        }
    }
    Ok(())
}

/// Открытые запросы живых исполнений разговора; запросы завершённых исполнений очередь не держат.
/// Одобрение запуска ждёт владельца, не останавливая ход родителя.
pub(crate) const LIVE_OPEN_REQUESTS: &str = "select count(*) from requests r
     join executions e on e.id = r.execution_id
     where r.conversation_id = $1 and r.state = 'open' and r.kind <> 'spawn_approval'
       and e.state not in ('stopped', 'failed', 'owner_lost')";
