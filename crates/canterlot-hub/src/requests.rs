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
}

impl RequestKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
        }
    }
}

impl FromStr for RequestKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "permission" => Ok(Self::Permission),
            "question" => Ok(Self::Question),
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
    let machine_id: Uuid =
        sqlx::query_scalar("select machine_id from conversations where id = $1 for no key update")
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
        .bind(&decision_json)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await?;

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
                    "decision": decision_json,
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
