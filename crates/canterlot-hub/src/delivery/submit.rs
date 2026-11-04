use std::fmt;

use canterlot_proto::{
    limits::{
        LimitError, MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES, MAX_MESSAGE_BYTES,
        check_deliver_input,
    },
    worker::v1::{
        AttachmentRef, Command, ContentBlock as ProtoContentBlock, DeliverInput, TextBlock,
        command, content_block,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    attachments::{self, AttachmentError},
    events::{self, NewEvent, Scope},
    idempotency::{self, Begin},
    model::{CommandKind, DeliveryState, InputState, PauseReason},
};

const OPERATION: &str = "submit_input";
pub const QUEUE_LIMIT: i64 = 100;

/// Блок содержимого ввода. Владелец присылает только `text`; блоки `attachment` хаб добавляет
/// сам после текста при привязке готовых вложений.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Attachment {
        attachment_id: Uuid,
        name: String,
        mime: String,
        size: u64,
        sha256: String,
        image: bool,
    },
}

impl ContentBlock {
    pub fn from_ref(reference: &AttachmentRef) -> Result<Self, sqlx::Error> {
        Ok(Self::Attachment {
            attachment_id: Uuid::parse_str(&reference.attachment_id)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            name: reference.name.clone(),
            mime: reference.mime.clone(),
            size: reference.size,
            sha256: reference.sha256.clone(),
            image: reference.image,
        })
    }

    pub fn to_proto(&self) -> ProtoContentBlock {
        ProtoContentBlock {
            block: Some(match self {
                Self::Text { text } => content_block::Block::Text(TextBlock { text: text.clone() }),
                Self::Attachment {
                    attachment_id,
                    name,
                    mime,
                    size,
                    sha256,
                    image,
                } => content_block::Block::Attachment(AttachmentRef {
                    attachment_id: attachment_id.to_string(),
                    name: name.clone(),
                    mime: mime.clone(),
                    size: *size,
                    sha256: sha256.clone(),
                    image: *image,
                }),
            }),
        }
    }
}

#[derive(Clone)]
pub struct SubmitInput {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub idempotency_key: String,
    pub content: Vec<ContentBlock>,
    pub received_at: DateTime<Utc>,
    pub correlation_id: Uuid,
    /// Откуда пришёл ввод другой сессии; у ввода владельца пусто.
    pub origin: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
    Message,
    InstructionsText,
}

pub enum SubmitError {
    IdempotencyConflict,
    KeyRaceLost,
    InvalidRequest {
        reason: &'static str,
    },
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
    Attachment(AttachmentError),
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
            Self::InvalidRequest { reason } => f
                .debug_struct("InvalidRequest")
                .field("reason", reason)
                .finish(),
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
            Self::Attachment(error) => f.debug_tuple("Attachment").field(error).finish(),
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
            Self::InvalidRequest { reason } => write!(f, "invalid request: {reason}"),
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
            Self::Attachment(error) => write!(f, "attachment: {error}"),
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

impl From<AttachmentError> for SubmitError {
    fn from(error: AttachmentError) -> Self {
        match error {
            AttachmentError::Db(error) => Self::Db(error),
            other => Self::Attachment(other),
        }
    }
}

fn validate_content(req: &SubmitInput, has_attachments: bool) -> Result<(), SubmitError> {
    idempotency::validate_key(&req.idempotency_key)
        .map_err(|reason| SubmitError::InvalidRequest { reason })?;
    validate_message_content(&req.content, has_attachments)
}

/// Содержимое владельца: только текст; без вложений хотя бы один непустой блок.
pub(crate) fn validate_message_content(
    content: &[ContentBlock],
    has_attachments: bool,
) -> Result<(), SubmitError> {
    let mut texts = Vec::with_capacity(content.len());
    for block in content {
        match block {
            ContentBlock::Text { text } => texts.push(text),
            ContentBlock::Attachment { .. } => {
                return Err(SubmitError::InvalidContent {
                    reason: "attachment_block",
                });
            }
        }
    }
    if !has_attachments && texts.iter().all(|text| text.is_empty()) {
        return Err(SubmitError::InvalidContent { reason: "empty" });
    }
    if texts.iter().any(|text| text.contains('\0')) {
        return Err(SubmitError::InvalidContent {
            reason: "nul_character",
        });
    }
    check_bounds(content)
}

/// Резервируем заголовок будущей выдачи до привязки исполнения.
pub(crate) fn check_bounds(content: &[ContentBlock]) -> Result<(), SubmitError> {
    let command = Command {
        command_id: Uuid::nil().to_string(),
        dispatch_attempt: i32::MAX as u32,
        execution_id: Uuid::nil().to_string(),
        execution_generation: i64::MAX as u64,
        kind: Some(command::Kind::DeliverInput(DeliverInput {
            input_id: Uuid::nil().to_string(),
            conversation_id: Uuid::nil().to_string(),
            content: content.iter().map(ContentBlock::to_proto).collect(),
            ..Default::default()
        })),
    };
    check_deliver_input(&command).map_err(|error| {
        let (kind, limit, actual) = match error {
            LimitError::InputText { actual } => {
                (LimitKind::InputText, MAX_INPUT_TEXT_BYTES, actual)
            }
            LimitError::InputMeta { actual } => {
                (LimitKind::InputMeta, MAX_INPUT_META_BYTES, actual)
            }
            LimitError::InstructionsText { actual } => (
                LimitKind::InstructionsText,
                canterlot_proto::limits::MAX_PRESET_TEXT_BYTES,
                actual,
            ),
            LimitError::Message { actual } => (LimitKind::Message, MAX_MESSAGE_BYTES, actual),
            LimitError::EventBatch { .. } => {
                unreachable!("command check cannot return an event batch limit")
            }
        };
        SubmitError::ResourceLimit {
            kind,
            limit,
            actual,
        }
    })?;
    Ok(())
}

enum Attempt {
    Submitted(SubmittedInput),
    LostRace,
}

fn request_hash(req: &SubmitInput, attachments: &[Uuid], retry_of: Option<Uuid>) -> Vec<u8> {
    if attachments.is_empty() && retry_of.is_none() {
        return idempotency::request_hash(
            OPERATION,
            &json!({"conversation_id":req.conversation_id,"content":req.content}),
        );
    }
    idempotency::request_hash(
        OPERATION,
        &json!({"conversation_id": req.conversation_id, "content": req.content, "attachment_ids": attachments, "retry_of": retry_of}),
    )
}

fn replay(response: serde_json::Value) -> Result<SubmittedInput, SubmitError> {
    serde_json::from_value(response).map_err(|error| sqlx::Error::Decode(Box::new(error)).into())
}

async fn lookup_submission(
    conn: &mut PgConnection,
    req: &SubmitInput,
    hash: &[u8],
) -> Result<Option<SubmittedInput>, SubmitError> {
    #[derive(sqlx::FromRow)]
    struct Stored {
        id: Uuid,
        conversation_id: Uuid,
        input_seq: i64,
        command_id: Uuid,
        submission_hash: Option<Vec<u8>>,
        content: sqlx::types::Json<Vec<ContentBlock>>,
        attachment_ids: sqlx::types::Json<Vec<Uuid>>,
        retry_of: Option<Uuid>,
    }
    let stored: Option<Stored> = sqlx::query_as("select i.id,i.conversation_id,i.input_seq,i.command_id,i.submission_hash,i.content,i.retry_of,coalesce(c.payload->'attachment_ids',(select jsonb_agg(attachment_id order by position) from input_attachments where input_id=i.id),'[]'::jsonb) attachment_ids from inputs i join commands c on c.id=i.command_id where i.owner_id=$1 and i.client_idempotency_key=$2")
        .bind(req.owner_id).bind(&req.idempotency_key).fetch_optional(conn).await?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let stored_hash = stored.submission_hash.unwrap_or_else(|| {
        // Старые ключи могли быть удалены до миграции; исходный запрос восстанавливается из ввода.
        let original = SubmitInput {
            conversation_id: stored.conversation_id,
            content: stored
                .content
                .0
                .into_iter()
                .filter(|b| matches!(b, ContentBlock::Text { .. }))
                .collect(),
            ..req.clone()
        };
        request_hash(&original, &stored.attachment_ids, stored.retry_of)
    });
    if stored.conversation_id != req.conversation_id || stored_hash != hash {
        return Err(SubmitError::IdempotencyConflict);
    }
    Ok(Some(SubmittedInput {
        input_id: stored.id,
        input_seq: stored.input_seq,
        command_id: stored.command_id,
        state: InputState::Queued,
    }))
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
    validate_content(&req, false)?;
    let hash = request_hash(&req, &[], None);
    let mut tx = crate::db::begin(pool).await?;
    match submit_attempt(&mut tx, &req, &hash, None, &[], None, false).await {
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
    validate_content(req, false)?;
    if !tx.is_in_transaction() {
        return Err(sqlx::Error::InvalidArgument("an outer transaction is required".into()).into());
    }
    let hash = request_hash(req, &[], None);
    match submit_attempt(tx, req, &hash, None, &[], None, false).await? {
        Attempt::Submitted(submitted) => Ok(submitted),
        Attempt::LostRace => Err(SubmitError::KeyRaceLost),
    }
}

pub(crate) async fn submit_message_input_in_tx(
    tx: &mut PgConnection,
    req: &SubmitInput,
    attachments: &[Uuid],
    retry_of: Option<Uuid>,
) -> Result<SubmittedInput, SubmitError> {
    submit_message_input_scoped_in_tx(tx, req, attachments, retry_of, None).await
}

/// Вложения другого разговора вызывающего: источник проверяется вместо целевого.
pub async fn submit_forwarded_message_input_in_tx(
    tx: &mut PgConnection,
    req: &SubmitInput,
    attachments: &[Uuid],
    retry_of: Option<Uuid>,
    source_conversation_id: Uuid,
) -> Result<SubmittedInput, SubmitError> {
    submit_message_input_scoped_in_tx(tx, req, attachments, retry_of, Some(source_conversation_id))
        .await
}

async fn submit_message_input_scoped_in_tx(
    tx: &mut PgConnection,
    req: &SubmitInput,
    attachments: &[Uuid],
    retry_of: Option<Uuid>,
    source_conversation_id: Option<Uuid>,
) -> Result<SubmittedInput, SubmitError> {
    if !tx.is_in_transaction() {
        return Err(sqlx::Error::InvalidArgument("an outer transaction is required".into()).into());
    }
    let hash = request_hash(req, attachments, retry_of);
    idempotency::validate_key(&req.idempotency_key)
        .map_err(|reason| SubmitError::InvalidRequest { reason })?;
    match idempotency::lookup(tx, req.owner_id, &req.idempotency_key, OPERATION, &hash).await? {
        Begin::Replay(response) => return replay(response),
        Begin::Conflict => return Err(SubmitError::IdempotencyConflict),
        Begin::New => {}
    }
    if let Some(submitted) = lookup_submission(tx, req, &hash).await? {
        return Ok(submitted);
    }
    attachments::check_ids(attachments)?;
    validate_content(req, !attachments.is_empty())?;
    match submit_attempt(
        tx,
        req,
        &hash,
        retry_of,
        attachments,
        source_conversation_id,
        true,
    )
    .await?
    {
        Attempt::Submitted(submitted) => Ok(submitted),
        Attempt::LostRace => Err(SubmitError::KeyRaceLost),
    }
}

async fn submit_attempt(
    conn: &mut PgConnection,
    req: &SubmitInput,
    hash: &[u8],
    retry_of: Option<Uuid>,
    attachment_ids: &[Uuid],
    source_conversation_id: Option<Uuid>,
    auto_title: bool,
) -> Result<Attempt, SubmitError> {
    match idempotency::lookup(conn, req.owner_id, &req.idempotency_key, OPERATION, hash).await? {
        Begin::Replay(response) => return replay(response).map(Attempt::Submitted),
        Begin::Conflict => return Err(SubmitError::IdempotencyConflict),
        Begin::New => {}
    }
    if let Some(submitted) = lookup_submission(conn, req, hash).await? {
        return Ok(Attempt::Submitted(submitted));
    }
    let (input_seq, closed, machine_id): (i64, bool, Uuid) = sqlx::query_as(
        "select q.next_input_seq, q.closed, c.machine_id from queue_state q join conversations c on c.id = q.conversation_id where q.conversation_id = $1 and c.owner_id = $2 for update of q")
        .bind(req.conversation_id).bind(req.owner_id).fetch_optional(&mut *conn).await?.ok_or(SubmitError::ConversationNotFound)?;
    match idempotency::lookup(conn, req.owner_id, &req.idempotency_key, OPERATION, hash).await? {
        Begin::Replay(response) => return replay(response).map(Attempt::Submitted),
        Begin::Conflict => return Err(SubmitError::IdempotencyConflict),
        Begin::New => {}
    }
    if let Some(submitted) = lookup_submission(conn, req, hash).await? {
        return Ok(Attempt::Submitted(submitted));
    }
    if closed {
        return Err(SubmitError::ConversationClosed);
    }
    if let Some(id) = retry_of {
        let valid: bool = sqlx::query_scalar("select exists(select 1 from inputs where id=$1 and conversation_id=$2 and state='acceptance_unconfirmed')").bind(id).bind(req.conversation_id).fetch_one(&mut *conn).await?;
        if !valid {
            return Err(SubmitError::InvalidRequest {
                reason: "invalid_retry",
            });
        }
    }
    let submitted = SubmittedInput {
        input_id: Uuid::now_v7(),
        input_seq,
        command_id: Uuid::now_v7(),
        state: InputState::Queued,
    };
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

    // Ввод владельца сам становится продолжением после лимита.
    let resumed: Option<(bool, Option<DateTime<Utc>>)> = sqlx::query_as(
        "update conversations set resume_pending = false where id = $1 and resume_pending
         returning crunch_enabled, reserve_override_until",
    )
    .bind(req.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    sqlx::query(
        "update queue_state set next_input_seq = next_input_seq + 1 where conversation_id = $1",
    )
    .bind(req.conversation_id)
    .execute(&mut *conn)
    .await?;
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
    let inserted = sqlx::query(
        "insert into inputs (id, conversation_id, owner_id, input_seq, idempotency_key,
                             content, state, command_id, received_at, retry_of,client_idempotency_key,submission_hash,origin)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $5, $11, $12)
         on conflict (owner_id,client_idempotency_key) do nothing",
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
    .bind(retry_of)
    .bind(hash)
    .bind(&req.origin)
    .execute(&mut *conn)
    .await?.rows_affected();
    if inserted == 0 {
        return Ok(Attempt::LostRace);
    }
    // Повтор как новое — решение владельца по неподтверждённому вводу: его
    // пауза больше не держит очередь.
    if let Some(id) = retry_of {
        let mut queue: super::LockedQueue =
            sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
                .bind(req.conversation_id)
                .fetch_one(&mut *conn)
                .await?;
        queue
            .pause_reasons
            .0
            .retain(|reason| *reason != PauseReason::UnconfirmedDelivery { input_id: id });
        let slot = queue.slot()?;
        queue
            .save(&mut *conn, &slot, None, req.correlation_id)
            .await?;
    }

    let title_changed = if auto_title && input_seq == 1 {
        crate::conversations::auto_title_in_tx(
            conn,
            req.owner_id,
            req.conversation_id,
            &req.content,
        )
        .await?
    } else {
        false
    };

    // Порядок блокировок: строка разговора выше, затем записи вложений `for share`.
    let content = if attachment_ids.is_empty() {
        content
    } else {
        let (catalog, machine) = attachments::gate_for(conn, req.conversation_id).await?;
        let refs = match source_conversation_id {
            Some(source) => {
                attachments::link_forwarded_in_tx(
                    conn,
                    req.owner_id,
                    source,
                    submitted.input_id,
                    attachment_ids,
                    &catalog,
                    &machine,
                )
                .await?
            }
            None => {
                attachments::link_in_tx(
                    conn,
                    req.owner_id,
                    req.conversation_id,
                    submitted.input_id,
                    attachment_ids,
                    &catalog,
                    &machine,
                )
                .await?
            }
        };
        let mut blocks = req.content.clone();
        for reference in &refs {
            blocks.push(ContentBlock::from_ref(reference)?);
        }
        check_bounds(&blocks)?;
        let content =
            serde_json::to_value(&blocks).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        sqlx::query("update commands set payload = payload || jsonb_build_object('content', $2::jsonb, 'attachment_ids', $3::jsonb) where id = $1")
            .bind(submitted.command_id)
            .bind(&content)
            .bind(json!(attachment_ids))
            .execute(&mut *conn)
            .await?;
        sqlx::query("update inputs set content = $2 where id = $1")
            .bind(submitted.input_id)
            .bind(&content)
            .execute(&mut *conn)
            .await?;
        content
    };

    let mut payload = json!({
        "input_id": submitted.input_id,
        "input_seq": input_seq,
        "idempotency_key": req.idempotency_key,
        "state": submitted.state,
        "content": content,
    });
    if let Some(origin) = &req.origin {
        payload["origin"] = origin.clone();
    }
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: req.conversation_id,
            },
            kind: "input_queued",
            conversation_id: Some(req.conversation_id),
            execution_id: None,
            payload,
            correlation_id: req.correlation_id,
        },
    )
    .await?;

    if let Some((enabled, until)) = resumed {
        events::append(
            conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: req.conversation_id,
                },
                kind: "crunch_changed",
                conversation_id: Some(req.conversation_id),
                execution_id: None,
                payload: json!({
                    "enabled": enabled,
                    "resume_pending": false,
                    "reserve_override_until": until
                        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                }),
                correlation_id: req.correlation_id,
            },
        )
        .await?;
    }

    if title_changed {
        crate::conversations::emit_attributes(
            conn,
            req.owner_id,
            req.conversation_id,
            req.correlation_id,
        )
        .await?;
    }

    sqlx::query("update inputs set created_seq=$2,created_at=$3 where id=$1")
        .bind(submitted.input_id)
        .bind(event.scope_seq)
        .bind(event.committed_at)
        .execute(&mut *conn)
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

impl fmt::Debug for ContentBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { text } => f
                .debug_struct("Text")
                .field("text_bytes", &text.len())
                .finish(),
            Self::Attachment {
                attachment_id,
                size,
                ..
            } => f
                .debug_struct("Attachment")
                .field("attachment_id", attachment_id)
                .field("size", size)
                .finish_non_exhaustive(),
        }
    }
}

impl fmt::Debug for SubmitInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubmitInput")
            .field("owner_id", &self.owner_id)
            .field("conversation_id", &self.conversation_id)
            .field("idempotency_key_bytes", &self.idempotency_key.len())
            .field("content", &self.content)
            .field("received_at", &self.received_at)
            .field("correlation_id", &self.correlation_id)
            .field("from_session", &self.origin.is_some())
            .finish()
    }
}
