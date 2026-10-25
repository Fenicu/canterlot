use std::fmt;

use canterlot_proto::limits::{MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    idempotency::{self, Begin},
    model::{CommandKind, DeliveryState, InputState},
};

const OPERATION: &str = "submit_input";
pub const QUEUE_LIMIT: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
}

#[derive(Debug, Clone)]
pub struct SubmitInput {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub idempotency_key: String,
    pub content: Vec<ContentBlock>,
    pub received_at: DateTime<Utc>,
    pub correlation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmittedInput {
    pub input_id: Uuid,
    pub input_seq: i64,
    pub command_id: Uuid,
    pub state: InputState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    InputText,
    InputMeta,
}

pub enum SubmitError {
    IdempotencyConflict,
    KeyRaceLost,
    InvalidContent {
        reason: &'static str,
    },
    QueueFull {
        limit: i64,
    },
    ResourceLimit {
        kind: LimitKind,
        limit: usize,
        actual: usize,
    },
    ConversationNotFound,
    ConversationClosed,
    Db(sqlx::Error),
}

fn fmt_db_error(error: &sqlx::Error, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if let sqlx::Error::Database(error) = error {
        return match error.code() {
            Some(code) => f.write_str(&code),
            None => write!(f, "{:?}", error.kind()),
        };
    }
    f.write_str(match error {
        sqlx::Error::Configuration(_) => "Configuration",
        sqlx::Error::InvalidArgument(_) => "InvalidArgument",
        sqlx::Error::Io(_) => "Io",
        sqlx::Error::Tls(_) => "Tls",
        sqlx::Error::Protocol(_) => "Protocol",
        sqlx::Error::RowNotFound => "RowNotFound",
        sqlx::Error::TypeNotFound { .. } => "TypeNotFound",
        sqlx::Error::ColumnIndexOutOfBounds { .. } => "ColumnIndexOutOfBounds",
        sqlx::Error::ColumnNotFound(_) => "ColumnNotFound",
        sqlx::Error::ColumnDecode { .. } => "ColumnDecode",
        sqlx::Error::Encode(_) => "Encode",
        sqlx::Error::Decode(_) => "Decode",
        sqlx::Error::AnyDriverError(_) => "AnyDriverError",
        sqlx::Error::PoolTimedOut => "PoolTimedOut",
        sqlx::Error::PoolClosed => "PoolClosed",
        sqlx::Error::WorkerCrashed => "WorkerCrashed",
        sqlx::Error::Migrate(_) => "Migrate",
        sqlx::Error::InvalidSavePointStatement => "InvalidSavePointStatement",
        sqlx::Error::BeginFailed => "BeginFailed",
        sqlx::Error::ConfigFile(_) => "ConfigFile",
        _ => "Unknown",
    })
}

impl fmt::Debug for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdempotencyConflict => f.write_str("IdempotencyConflict"),
            Self::KeyRaceLost => f.write_str("KeyRaceLost"),
            Self::InvalidContent { reason } => f
                .debug_struct("InvalidContent")
                .field("reason", reason)
                .finish(),
            Self::QueueFull { limit } => f.debug_struct("QueueFull").field("limit", limit).finish(),
            Self::ResourceLimit {
                kind,
                limit,
                actual,
            } => f
                .debug_struct("ResourceLimit")
                .field("kind", kind)
                .field("limit", limit)
                .field("actual", actual)
                .finish(),
            Self::ConversationNotFound => f.write_str("ConversationNotFound"),
            Self::ConversationClosed => f.write_str("ConversationClosed"),
            Self::Db(error) => {
                f.write_str("Db(")?;
                fmt_db_error(error, f)?;
                f.write_str(")")
            }
        }
    }
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdempotencyConflict => f.write_str("idempotency conflict"),
            Self::KeyRaceLost => f.write_str("idempotency key race lost"),
            Self::InvalidContent { reason } => write!(f, "invalid content: {reason}"),
            Self::QueueFull { limit } => write!(f, "queue limit exceeded: {limit}"),
            Self::ResourceLimit {
                kind,
                limit,
                actual,
            } => write!(
                f,
                "resource limit exceeded: {kind:?}, limit {limit}, actual {actual}"
            ),
            Self::ConversationNotFound => f.write_str("conversation not found"),
            Self::ConversationClosed => f.write_str("conversation closed"),
            Self::Db(error) => {
                f.write_str("database error: ")?;
                fmt_db_error(error, f)
            }
        }
    }
}

impl std::error::Error for SubmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Db(error) => error.source(),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for SubmitError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

fn validate_content(req: &SubmitInput) -> Result<(), SubmitError> {
    if req.content.iter().any(|block| match block {
        ContentBlock::Text { text } => text.contains('\0'),
    }) {
        return Err(SubmitError::InvalidContent {
            reason: "nul_character",
        });
    }
    Ok(())
}

enum Attempt {
    Submitted(SubmittedInput),
    LostRace,
}

fn request_hash(req: &SubmitInput) -> Vec<u8> {
    idempotency::request_hash(
        OPERATION,
        &json!({"conversation_id": req.conversation_id, "content": req.content}),
    )
}

fn replay(response: serde_json::Value) -> Result<SubmittedInput, SubmitError> {
    serde_json::from_value(response).map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}

async fn resolve_race(
    conn: &mut PgConnection,
    req: &SubmitInput,
    hash: &[u8],
) -> Result<SubmittedInput, SubmitError> {
    match idempotency::lookup(conn, req.owner_id, &req.idempotency_key, OPERATION, hash).await? {
        Begin::Replay(response) => replay(response),
        Begin::New | Begin::Conflict => Err(SubmitError::IdempotencyConflict),
    }
}

pub async fn submit_input(pool: &PgPool, req: SubmitInput) -> Result<SubmittedInput, SubmitError> {
    validate_content(&req)?;
    let hash = request_hash(&req);
    let mut tx = pool.begin().await?;
    match submit_attempt(&mut tx, &req, &hash).await {
        Ok(Attempt::Submitted(submitted)) => {
            tx.commit().await?;
            Ok(submitted)
        }
        Ok(Attempt::LostRace) => {
            tx.rollback().await?;
            let mut conn = pool.acquire().await?;
            resolve_race(&mut conn, &req, &hash).await
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Вызывать внутри транзакции; при KeyRaceLost вызывающий обязан откатить всю транзакцию.
pub async fn submit_input_in_tx(
    tx: &mut PgConnection,
    req: &SubmitInput,
) -> Result<SubmittedInput, SubmitError> {
    validate_content(req)?;
    if !tx.is_in_transaction() {
        return Err(sqlx::Error::InvalidArgument("an outer transaction is required".into()).into());
    }
    let hash = request_hash(req);
    match submit_attempt(tx, req, &hash).await? {
        Attempt::Submitted(submitted) => Ok(submitted),
        Attempt::LostRace => Err(SubmitError::KeyRaceLost),
    }
}

async fn submit_attempt(
    conn: &mut PgConnection,
    req: &SubmitInput,
    hash: &[u8],
) -> Result<Attempt, SubmitError> {
    match idempotency::lookup(conn, req.owner_id, &req.idempotency_key, OPERATION, hash).await? {
        Begin::Replay(response) => return replay(response).map(Attempt::Submitted),
        Begin::Conflict => return Err(SubmitError::IdempotencyConflict),
        Begin::New => {}
    }

    let (input_seq, closed, machine_id): (i64, bool, Uuid) = sqlx::query_as(
        "select q.next_input_seq, q.closed, c.machine_id from queue_state q
         join conversations c on c.id = q.conversation_id
         where q.conversation_id = $1 and c.owner_id = $2 for update of q",
    )
    .bind(req.conversation_id)
    .bind(req.owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(SubmitError::ConversationNotFound)?;
    match idempotency::lookup(conn, req.owner_id, &req.idempotency_key, OPERATION, hash).await? {
        Begin::Replay(response) => return replay(response).map(Attempt::Submitted),
        Begin::Conflict => return Err(SubmitError::IdempotencyConflict),
        Begin::New => {}
    }
    if closed {
        return Err(SubmitError::ConversationClosed);
    }

    let text_bytes: usize = req
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
        })
        .sum();
    if text_bytes > MAX_INPUT_TEXT_BYTES {
        return Err(SubmitError::ResourceLimit {
            kind: LimitKind::InputText,
            limit: MAX_INPUT_TEXT_BYTES,
            actual: text_bytes,
        });
    }
    let content_bytes = serde_json::to_vec(&req.content)
        .map_err(|error| sqlx::Error::Encode(Box::new(error)))?
        .len();
    let meta_bytes = content_bytes - text_bytes;
    if meta_bytes > MAX_INPUT_META_BYTES {
        return Err(SubmitError::ResourceLimit {
            kind: LimitKind::InputMeta,
            limit: MAX_INPUT_META_BYTES,
            actual: meta_bytes,
        });
    }
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id = $1
         and state in ('queued', 'dispatched', 'on_machine')",
    )
    .bind(req.conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if queued >= QUEUE_LIMIT {
        return Err(SubmitError::QueueFull { limit: QUEUE_LIMIT });
    }

    sqlx::query(
        "update queue_state set next_input_seq = next_input_seq + 1 where conversation_id = $1",
    )
    .bind(req.conversation_id)
    .execute(&mut *conn)
    .await?;
    let submitted = SubmittedInput {
        input_id: Uuid::now_v7(),
        input_seq,
        command_id: Uuid::now_v7(),
        state: InputState::Queued,
    };
    let content =
        serde_json::to_value(&req.content).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, kind, delivery_state, dispatch_attempt, payload)
         values ($1, $2, $3, $4, $5, 0, $6)",
    )
    .bind(submitted.command_id)
    .bind(machine_id)
    .bind(req.conversation_id)
    .bind(CommandKind::DeliverInput.as_str())
    .bind(DeliveryState::Held.as_str())
    .bind(json!({"input_id": submitted.input_id, "conversation_id": req.conversation_id, "content": content}))
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "insert into inputs (id, conversation_id, owner_id, input_seq, idempotency_key,
                             content, state, command_id, received_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(submitted.input_id)
    .bind(req.conversation_id)
    .bind(req.owner_id)
    .bind(input_seq)
    .bind(&req.idempotency_key)
    .bind(&content)
    .bind(submitted.state.as_str())
    .bind(submitted.command_id)
    .bind(req.received_at)
    .execute(&mut *conn)
    .await?;

    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: req.conversation_id,
            },
            kind: "input_queued",
            conversation_id: Some(req.conversation_id),
            execution_id: None,
            payload: json!({
                "input_id": submitted.input_id,
                "input_seq": input_seq,
                "idempotency_key": req.idempotency_key,
                "state": submitted.state,
                "content": content,
            }),
            correlation_id: req.correlation_id,
        },
    )
    .await?;

    let response =
        serde_json::to_value(&submitted).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    if idempotency::record(
        conn,
        req.owner_id,
        &req.idempotency_key,
        OPERATION,
        hash,
        &response,
        Some(req.conversation_id),
    )
    .await?
    {
        Ok(Attempt::Submitted(submitted))
    } else {
        Ok(Attempt::LostRace)
    }
}
