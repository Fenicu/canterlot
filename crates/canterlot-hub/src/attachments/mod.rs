//! Вложения хаба: staging → проверенная надёжная публикация → commit метаданных (§13.1).

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENT_NAME_BYTES, MAX_ATTACHMENTS_PER_INPUT},
    worker::v1::AttachmentRef,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    idempotency::{self, Begin, IdempotencyConflict},
};

pub mod blobstore;
pub mod gc;
pub mod upload;

pub use blobstore::{
    BlobError, BlobStore, FileKind, LocalBlobStore, NoTrace, PublishStep, PublishTrace, StoredFile,
};

/// Одно тело `PATCH`; объявляется в `GET /api/v1/config` как `limits.upload_chunk_bytes`.
pub const UPLOAD_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
/// Форматы, которые отдаются как изображение и передаются агенту нативным блоком.
pub const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
const DEFAULT_MIME: &str = "application/octet-stream";

/// Признак нативного изображения для ссылки агенту.
pub fn native_image(mime: &str) -> bool {
    IMAGE_MIMES.contains(&mime)
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewAttachment {
    pub conversation_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub mime: Option<String>,
    pub size: u64,
    pub sha256: String,
}

impl fmt::Debug for NewAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewAttachment")
            .field("conversation_id", &self.conversation_id)
            .field("name_bytes", &self.name.len())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = AttachmentRecord)]
pub struct AttachmentView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub state: String,
    pub fail_reason: Option<String>,
    pub uploaded_bytes: Option<u64>,
}

impl fmt::Debug for AttachmentView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentView")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("size", &self.size)
            .field("uploaded_bytes", &self.uploaded_bytes)
            .finish_non_exhaustive()
    }
}

/// Харнесс разговора: принимает ли он изображения.
#[derive(Clone, Copy, Debug)]
pub struct HarnessBrief {
    pub images: bool,
}

/// Сохранённый отчёт машины: `pilot_v1` и совпадение лимитов вложений.
#[derive(Clone, Copy, Debug)]
pub struct MachineGate {
    pub pilot: bool,
    pub limits_ok: bool,
}

pub enum AttachmentError {
    NotFound,
    TooLarge,
    ExcessBytes { current: u64 },
    TooMany,
    InvalidName,
    Invalid,
    ChecksumMismatch,
    OffsetMismatch { current: u64 },
    UploadInProgress { current: u64 },
    BodyInterrupted { current: u64 },
    NotUploadable,
    NotReady,
    Unavailable,
    Unsupported,
    Conflict,
    InvalidState,
    TooManyUploads,
    CapabilityMissing,
    LimitsMismatch,
    IdempotencyConflict,
    Db(sqlx::Error),
    Blob(BlobError),
}

impl AttachmentError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::TooLarge => "too_large",
            Self::ExcessBytes { .. } => "excess_bytes",
            Self::TooMany => "too_many",
            Self::InvalidName => "invalid_name",
            Self::Invalid => "invalid",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::OffsetMismatch { .. } => "offset_mismatch",
            Self::UploadInProgress { .. } => "upload_in_progress",
            Self::BodyInterrupted { .. } => "body_interrupted",
            Self::NotUploadable => "not_uploadable",
            Self::NotReady => "not_ready",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::Conflict => "conflict",
            Self::InvalidState => "invalid_state",
            Self::TooManyUploads => "too_many_uploads",
            Self::CapabilityMissing => "capability_missing",
            Self::LimitsMismatch => "limits_mismatch",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::Db(_) => "db",
            Self::Blob(_) => "blob",
        }
    }

    /// Текущее смещение загрузки для заголовка `Upload-Offset`.
    pub fn upload_offset(&self) -> Option<u64> {
        match self {
            Self::ExcessBytes { current }
            | Self::OffsetMismatch { current }
            | Self::UploadInProgress { current }
            | Self::BodyInterrupted { current } => Some(*current),
            _ => None,
        }
    }
}

impl fmt::Debug for AttachmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(error) => f
                .debug_tuple("Db")
                .field(&crate::dberr::describe(error))
                .finish(),
            Self::Blob(error) => f.debug_tuple("Blob").field(error).finish(),
            other => match other.upload_offset() {
                Some(current) => write!(f, "{}(current={current})", other.kind()),
                None => f.write_str(other.kind()),
            },
        }
    }
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for AttachmentError {}

impl From<sqlx::Error> for AttachmentError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<IdempotencyConflict> for AttachmentError {
    fn from(_: IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

impl From<BlobError> for AttachmentError {
    fn from(error: BlobError) -> Self {
        Self::Blob(error)
    }
}

impl From<std::io::Error> for AttachmentError {
    fn from(error: std::io::Error) -> Self {
        Self::Blob(BlobError::Io(error))
    }
}

/// Имя сводится к последнему компоненту пути; NUL и управляющие символы запрещены.
pub fn normalize_name(raw: &str) -> Result<String, AttachmentError> {
    if raw.chars().any(char::is_control) {
        return Err(AttachmentError::InvalidName);
    }
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    if last.is_empty() || last == "." || last == ".." || last.len() > MAX_ATTACHMENT_NAME_BYTES {
        return Err(AttachmentError::InvalidName);
    }
    Ok(last.to_owned())
}

/// Тип без параметров, в нижнем регистре, из символов токена RFC 6838.
pub fn normalize_mime(raw: Option<&str>) -> Result<String, AttachmentError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_MIME.into());
    };
    let mime = raw
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let token = |part: &str| {
        !part.is_empty()
            && part.len() <= 63
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$&^_.+-".contains(&b))
    };
    match mime.split_once('/') {
        Some((kind, subtype)) if token(kind) && token(subtype) => Ok(mime),
        _ => Err(AttachmentError::Invalid),
    }
}

pub fn normalize_sha256(raw: &str) -> Result<String, AttachmentError> {
    let sha = raw.to_ascii_lowercase();
    if sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(AttachmentError::Invalid)
    }
}

fn valid_key(key: &str) -> Result<(), AttachmentError> {
    if idempotency::validate_key(key).is_err()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AttachmentError::Invalid);
    }
    Ok(())
}

fn view(row: &PgRow, uploaded: Option<u64>) -> Result<AttachmentView, sqlx::Error> {
    Ok(AttachmentView {
        id: row.try_get("id")?,
        conversation_id: row.try_get("conversation_id")?,
        name: row.try_get("name")?,
        mime: row.try_get("mime")?,
        size: row.try_get::<i64, _>("size_bytes")? as u64,
        sha256: row.try_get("sha256")?,
        state: row.try_get("state")?,
        fail_reason: row.try_get("fail_reason")?,
        uploaded_bytes: uploaded,
    })
}

pub(crate) async fn state_event(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    id: Uuid,
    state: &str,
    reason: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "attachment_state_changed",
            conversation_id: Some(conversation_id),
            execution_id: None,
            payload: json!({"attachment_id": id, "state": state, "reason": reason}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Создаёт запись `created`; объявленный размер служит и потолком загрузки.
pub async fn create(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    req: NewAttachment,
    correlation_id: Uuid,
) -> Result<AttachmentView, AttachmentError> {
    valid_key(key)?;
    let name = normalize_name(&req.name)?;
    let mime = normalize_mime(req.mime.as_deref())?;
    let sha256 = normalize_sha256(&req.sha256)?;
    if req.size > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::TooLarge);
    }
    let conversation_id = req.conversation_id;
    let size = req.size;
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "create_attachment",
        &req,
        Some(conversation_id),
        move |conn| {
            Box::pin(async move {
                let owned: bool = sqlx::query_scalar(
                    "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
                )
                .bind(conversation_id)
                .bind(owner_id)
                .fetch_one(&mut *conn)
                .await?;
                if !owned {
                    return Err(AttachmentError::NotFound);
                }
                let row = sqlx::query(
                    "insert into attachments (id, owner_id, conversation_id, name, mime, size_bytes, sha256)
                     values ($1, $2, $3, $4, $5, $6, $7) returning *",
                )
                .bind(Uuid::now_v7())
                .bind(owner_id)
                .bind(conversation_id)
                .bind(&name)
                .bind(&mime)
                .bind(size as i64)
                .bind(&sha256)
                .fetch_one(&mut *conn)
                .await?;
                let created = view(&row, Some(0))?;
                state_event(conn, conversation_id, created.id, "created", None, correlation_id)
                    .await?;
                Ok(created)
            })
        },
    )
    .await?;
    Ok(outcome.value)
}

/// Чтение владельцем; `uploaded_bytes` для незавершённой загрузки берётся из staging.
pub async fn get(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
) -> Result<AttachmentView, AttachmentError> {
    let row = sqlx::query("select * from attachments where id=$1 and owner_id=$2")
        .bind(id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await?
        .ok_or(AttachmentError::NotFound)?;
    let state: String = row.try_get("state")?;
    let size = row.try_get::<i64, _>("size_bytes")? as u64;
    let uploaded = match state.as_str() {
        "created" | "uploading" => Some(upload::received(store, id, size).await?),
        "ready" => Some(size),
        _ => None,
    };
    Ok(view(&row, uploaded)?)
}

async fn fail(
    pool: &PgPool,
    id: Uuid,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let reason = reason.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let failed: Option<Uuid> = sqlx::query_scalar(
                "update attachments set state='failed', fail_reason=$2, updated_at=clock_timestamp()
                 where id=$1 and state in ('created','uploading') returning conversation_id",
            )
            .bind(id)
            .bind(&reason)
            .fetch_optional(&mut *conn)
            .await?;
            if let Some(conversation_id) = failed {
                state_event(
                    &mut *conn,
                    conversation_id,
                    id,
                    "failed",
                    Some(reason.as_str()),
                    correlation_id,
                )
                .await?;
            }
            Ok(())
        })
    })
    .await
}

/// Завершение загрузки. Ключ проверяется первым, затем вне транзакций: отметка активности,
/// проверка длины и SHA-256, fsync, rename и fsync каталогов; только потом короткая транзакция
/// переводит запись в `ready`. Сбой между публикацией и commit оставляет `uploading` и blob,
/// повтор с тем же ID и checksum не копирует содержимое и фиксирует метаданные.
pub async fn complete(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
    sha256: Option<&str>,
    key: &str,
    correlation_id: Uuid,
) -> Result<AttachmentView, AttachmentError> {
    valid_key(key)?;
    let sha256 = sha256.map(normalize_sha256).transpose()?;
    let body = json!({"attachment_id": id, "sha256": sha256});
    let hash = idempotency::request_hash("complete_attachment", &body);
    {
        let mut conn = pool.acquire().await?;
        match idempotency::lookup(&mut conn, owner_id, key, "complete_attachment", &hash).await? {
            Begin::Replay(value) => {
                return serde_json::from_value(value)
                    .map_err(|e| AttachmentError::Db(sqlx::Error::Decode(Box::new(e))));
            }
            Begin::Conflict => return Err(AttachmentError::IdempotencyConflict),
            Begin::New => {}
        }
    }
    let size: i64 =
        sqlx::query_scalar("select size_bytes from attachments where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(pool)
            .await?
            .ok_or(AttachmentError::NotFound)?;
    let Some(locked) = store.try_lock(id)? else {
        let current = upload::received(store, id, size as u64).await?;
        return Err(AttachmentError::UploadInProgress { current });
    };
    let store = &*locked;
    let active: Option<(Uuid, i64, String)> = sqlx::query_as(
        "update attachments set updated_at=clock_timestamp()
         where id=$1 and owner_id=$2 and state in ('created','uploading')
         returning conversation_id, size_bytes, sha256",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;
    let conversation_id = match active {
        Some((conversation_id, size, declared)) => {
            if sha256.as_ref().is_some_and(|sha| *sha != declared) {
                return Err(AttachmentError::Conflict);
            }
            match store.publish(id, size as u64, &declared).await {
                Ok(()) => {}
                Err(BlobError::LengthMismatch { actual }) => {
                    return Err(AttachmentError::OffsetMismatch { current: actual });
                }
                Err(BlobError::ChecksumMismatch) => {
                    fail(pool, id, "checksum_mismatch", correlation_id).await?;
                    if let Err(error) = store.delete_staging(id).await {
                        tracing::warn!(kind = "attachment_staging_delete_failed", attachment_id = %id, error = %error.kind(), "staging delete failed");
                    }
                    return Err(AttachmentError::ChecksumMismatch);
                }
                Err(error) => return Err(error.into()),
            }
            conversation_id
        }
        None => {
            let current: (Uuid, String, String) = sqlx::query_as(
                "select conversation_id, state, sha256 from attachments where id=$1 and owner_id=$2",
            )
            .bind(id)
            .bind(owner_id)
            .fetch_optional(pool)
            .await?
            .ok_or(AttachmentError::NotFound)?;
            let (conversation_id, state, declared) = current;
            if state != "ready" {
                return Err(AttachmentError::NotUploadable);
            }
            if sha256.as_ref().is_some_and(|sha| *sha != declared) {
                return Err(AttachmentError::Conflict);
            }
            conversation_id
        }
    };
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "complete_attachment",
        &body,
        Some(conversation_id),
        move |conn| {
            Box::pin(async move {
                let published = sqlx::query(
                    "update attachments set state='ready', ready_at=clock_timestamp(), updated_at=clock_timestamp()
                     where id=$1 and owner_id=$2 and state in ('created','uploading') returning *",
                )
                .bind(id)
                .bind(owner_id)
                .fetch_optional(&mut *conn)
                .await?;
                let row = match published {
                    Some(row) => {
                        state_event(conn, conversation_id, id, "ready", None, correlation_id)
                            .await?;
                        row
                    }
                    None => {
                        let row = sqlx::query("select * from attachments where id=$1 and owner_id=$2")
                            .bind(id)
                            .bind(owner_id)
                            .fetch_one(&mut *conn)
                            .await?;
                        if row.try_get::<String, _>("state")? != "ready" {
                            return Err(AttachmentError::NotUploadable);
                        }
                        row
                    }
                };
                let size = row.try_get::<i64, _>("size_bytes")? as u64;
                Ok(view(&row, Some(size))?)
            })
        },
    )
    .await?;
    Ok(outcome.value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CancelEffect {
    Cancelled,
    Released,
    Unchanged,
}

/// Отмена загрузки или удаление неиспользованного готового вложения; файлы удаляются после commit.
pub async fn cancel(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), AttachmentError> {
    valid_key(key)?;
    let body = json!({"attachment_id": id});
    let hash = idempotency::request_hash("cancel_attachment", &body);
    {
        let mut conn = pool.acquire().await?;
        if matches!(
            idempotency::lookup(&mut conn, owner_id, key, "cancel_attachment", &hash).await?,
            Begin::Conflict
        ) {
            return Err(AttachmentError::IdempotencyConflict);
        }
    }
    let size: i64 =
        sqlx::query_scalar("select size_bytes from attachments where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(pool)
            .await?
            .ok_or(AttachmentError::NotFound)?;
    let Some(locked) = store.try_lock(id)? else {
        let current = upload::received(store, id, size as u64).await?;
        return Err(AttachmentError::UploadInProgress { current });
    };
    let store = &*locked;
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "cancel_attachment",
        &body,
        None,
        move |conn| {
            Box::pin(async move {
                let (conversation_id, state): (Uuid, String) = sqlx::query_as(
                    "select conversation_id, state from attachments where id=$1 and owner_id=$2 for update",
                )
                .bind(id)
                .bind(owner_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or(AttachmentError::NotFound)?;
                let effect = match state.as_str() {
                    "created" | "uploading" => {
                        sqlx::query("update attachments set state='cancelled', updated_at=clock_timestamp() where id=$1")
                            .bind(id)
                            .execute(&mut *conn)
                            .await?;
                        state_event(conn, conversation_id, id, "cancelled", None, correlation_id)
                            .await?;
                        CancelEffect::Cancelled
                    }
                    "ready" => {
                        if linked(conn, id).await? {
                            return Err(AttachmentError::InvalidState);
                        }
                        sqlx::query("update attachments set state='released', updated_at=clock_timestamp() where id=$1")
                            .bind(id)
                            .execute(&mut *conn)
                            .await?;
                        state_event(conn, conversation_id, id, "released", None, correlation_id)
                            .await?;
                        CancelEffect::Released
                    }
                    _ => CancelEffect::Unchanged,
                };
                Ok(effect)
            })
        },
    )
    .await?;
    if outcome.value != CancelEffect::Unchanged {
        for result in [store.delete_staging(id).await, store.delete_blob(id).await] {
            if let Err(error) = result {
                tracing::warn!(kind = "attachment_file_delete_failed", attachment_id = %id, error = %error.kind(), "file delete failed; the GC retries");
            }
        }
    }
    Ok(())
}

/// Есть ли ссылка из ввода или отложенного сообщения; вызывать после блокировки строки.
pub(crate) async fn linked(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from input_attachments where attachment_id=$1)
             or exists(select 1 from scheduled_job_attachments where attachment_id=$1)
             or exists(select 1 from composer_drafts where attachment_ids ? $1::text)
             or exists(select 1 from block_files where attachment_id=$1 and attached_at is not null)",
    )
    .bind(id)
    .fetch_one(conn)
    .await
}

/// Харнесс и сохранённый отчёт машины разговора; обычное чтение без блокировок.
pub async fn gate_for(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<(HarnessBrief, MachineGate), sqlx::Error> {
    let (images, pilot, limits_ok): (bool, bool, bool) = sqlx::query_as(
        "select coalesce(h.images, false), 'pilot_v1' = any(m.capabilities), m.limits_ok
         from conversations c join machines m on m.id = c.machine_id
         left join machine_harnesses h on h.machine_id = c.machine_id and h.harness = c.harness
         where c.id = $1",
    )
    .bind(conversation_id)
    .fetch_one(conn)
    .await?;
    Ok((HarnessBrief { images }, MachineGate { pilot, limits_ok }))
}

/// Число и уникальность ссылок одного ввода.
pub fn check_ids(ids: &[Uuid]) -> Result<(), AttachmentError> {
    if ids.len() > MAX_ATTACHMENTS_PER_INPUT {
        return Err(AttachmentError::TooMany);
    }
    let mut seen = HashSet::with_capacity(ids.len());
    if !ids.iter().all(|id| seen.insert(*id)) {
        return Err(AttachmentError::Invalid);
    }
    Ok(())
}

/// Блокирует записи `for share` по возрастанию id и требует владельца, разговор и `ready`;
/// сериализуется со сборщиком, который берёт `for update`. Ссылки идут в порядке запроса.
pub async fn lock_ready_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    attachment_ids: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    lock_checked_in_tx(
        conn,
        owner_id,
        attachment_ids,
        catalog,
        machine,
        |_, conversation| conversation == conversation_id,
    )
    .await
}

/// Вложения из разговора вызывающего, передаваемые другому разговору без повторной загрузки:
/// владелец и `ready` те же, но исходный разговор проверяется отдельно от целевого.
pub async fn lock_forwarded_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    source_conversation_id: Uuid,
    attachment_ids: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    lock_checked_in_tx(
        conn,
        owner_id,
        attachment_ids,
        catalog,
        machine,
        |_, conversation| conversation == source_conversation_id,
    )
    .await
}

async fn lock_checked_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    attachment_ids: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
    allowed: impl Fn(Uuid, Uuid) -> bool,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    if !machine.pilot {
        return Err(AttachmentError::CapabilityMissing);
    }
    if !machine.limits_ok {
        return Err(AttachmentError::LimitsMismatch);
    }
    check_ids(attachment_ids)?;
    let rows = sqlx::query(
        "select id, owner_id, conversation_id, name, mime, size_bytes, sha256, state
         from attachments where id = any($1) order by id for share",
    )
    .bind(attachment_ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_id = HashMap::with_capacity(rows.len());
    for row in rows {
        by_id.insert(row.try_get::<Uuid, _>("id")?, row);
    }
    let mut refs = Vec::with_capacity(attachment_ids.len());
    for id in attachment_ids {
        let Some(row) = by_id.get(id) else {
            return Err(AttachmentError::NotReady);
        };
        if row.try_get::<Uuid, _>("owner_id")? != owner_id
            || !allowed(*id, row.try_get::<Uuid, _>("conversation_id")?)
            || row.try_get::<String, _>("state")? != "ready"
        {
            return Err(AttachmentError::NotReady);
        }
        let mime: String = row.try_get("mime")?;
        if mime.starts_with("image/") && !catalog.images {
            return Err(AttachmentError::Unsupported);
        }
        refs.push(AttachmentRef {
            attachment_id: id.to_string(),
            name: row.try_get("name")?,
            image: native_image(&mime),
            mime,
            size: row.try_get::<i64, _>("size_bytes")? as u64,
            sha256: row.try_get("sha256")?,
        });
    }
    Ok(refs)
}

/// Заново связывает вложения правленого ввода. Новое вложение берётся из
/// разговора ввода; уже привязанное к вводу (`kept`) остаётся, откуда бы оно
/// ни пришло: у пересланного другой сессией ввода это разговор отправителя.
#[allow(clippy::too_many_arguments)]
pub async fn relink_edited_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    input_id: Uuid,
    attachment_ids: &[Uuid],
    kept: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    let refs = lock_checked_in_tx(
        conn,
        owner_id,
        attachment_ids,
        catalog,
        machine,
        |id, conversation| conversation == conversation_id || kept.contains(&id),
    )
    .await?;
    sqlx::query("delete from input_attachments where input_id = $1")
        .bind(input_id)
        .execute(&mut *conn)
        .await?;
    for (position, id) in attachment_ids.iter().enumerate() {
        sqlx::query(
            "insert into input_attachments (input_id, attachment_id, position) values ($1, $2, $3)",
        )
        .bind(input_id)
        .bind(id)
        .bind(position as i32)
        .execute(&mut *conn)
        .await?;
    }
    Ok(refs)
}

/// Связывает переданные вложения с вводом целевого разговора.
pub async fn link_forwarded_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    source_conversation_id: Uuid,
    input_id: Uuid,
    attachment_ids: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    let refs = lock_forwarded_in_tx(
        conn,
        owner_id,
        source_conversation_id,
        attachment_ids,
        catalog,
        machine,
    )
    .await?;
    for (position, id) in attachment_ids.iter().enumerate() {
        sqlx::query(
            "insert into input_attachments (input_id, attachment_id, position) values ($1, $2, $3)",
        )
        .bind(input_id)
        .bind(id)
        .bind(position as i32)
        .execute(&mut *conn)
        .await?;
    }
    Ok(refs)
}

/// Связывает готовые вложения с вводом в транзакции его создания.
pub async fn link_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    input_id: Uuid,
    attachment_ids: &[Uuid],
    catalog: &HarnessBrief,
    machine: &MachineGate,
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    let refs = lock_ready_in_tx(
        conn,
        owner_id,
        conversation_id,
        attachment_ids,
        catalog,
        machine,
    )
    .await?;
    for (position, id) in attachment_ids.iter().enumerate() {
        sqlx::query(
            "insert into input_attachments (input_id, attachment_id, position) values ($1, $2, $3)",
        )
        .bind(input_id)
        .bind(id)
        .bind(position as i32)
        .execute(&mut *conn)
        .await?;
    }
    Ok(refs)
}

/// Ссылки отложенного сообщения: сборщик считает их наравне со ссылками вводов.
pub async fn link_scheduled_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    job_id: Uuid,
    attachment_ids: &[Uuid],
) -> Result<Vec<AttachmentRef>, AttachmentError> {
    if attachment_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (catalog, machine) = gate_for(conn, conversation_id).await?;
    let refs = lock_ready_in_tx(
        conn,
        owner_id,
        conversation_id,
        attachment_ids,
        &catalog,
        &machine,
    )
    .await?;
    sqlx::query(
        "insert into scheduled_job_attachments (job_id, attachment_id) select $1, unnest($2::uuid[]) on conflict do nothing",
    )
    .bind(job_id)
    .bind(attachment_ids)
    .execute(conn)
    .await?;
    Ok(refs)
}

/// Снимает ссылки заданий, которые отменены, отправлены или завершились ошибкой.
pub async fn unlink_scheduled_in_tx(
    conn: &mut PgConnection,
    job_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from scheduled_job_attachments where job_id = any($1)")
        .bind(job_ids)
        .execute(conn)
        .await?;
    Ok(())
}

/// Готовое вложение владельца для выдачи браузеру.
pub struct Download {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub file: tokio::fs::File,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

pub async fn open_for_download(
    pool: &PgPool,
    store: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Download, AttachmentError> {
    let (name, mime, size, state): (String, String, i64, String) = sqlx::query_as(
        "select name, mime, size_bytes, state from attachments where id=$1 and owner_id=$2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?
    .ok_or(AttachmentError::NotFound)?;
    match state.as_str() {
        "ready" => {}
        "created" | "uploading" => return Err(AttachmentError::NotReady),
        _ => return Err(AttachmentError::Unavailable),
    }
    let (file, len) = match store.open(id, 0).await {
        Ok(opened) => opened,
        Err(BlobError::NotFound) => return Err(AttachmentError::Unavailable),
        Err(error) => return Err(error.into()),
    };
    Ok(Download {
        name,
        mime,
        size: len.min(size as u64),
        file,
    })
}
