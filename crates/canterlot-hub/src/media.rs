//! Картинки, на которые смотрел агент (pilot.md §5.7): приём от воркера в staging, проверенная
//! надёжная публикация, затем строка `block_media`; ссылка из блока и событие появляются только
//! по записи `BlockMedia` журнала сопровождения, которую воркер пишет после `MediaStored`.

use std::{fmt, io};

use bytes::Bytes;
use canterlot_proto::{
    limits::{
        MAX_MEDIA_BYTES, MAX_SHARE_BYTES, MAX_SHARE_NOTE_BYTES, MAX_SHARE_TITLE_BYTES, MEDIA_MIMES,
    },
    worker::v1::{BlockFile, BlockMedia, MediaHeader},
};
use futures_util::stream::BoxStream;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::io::AsyncReadExt;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    attachments::{BlobError, BlobStore},
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

/// Файл, который агент отдаёт владельцу: часть заголовка `PutMedia` с `share`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub title: String,
    pub note: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Проверенный заголовок `PutMedia`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub id: Uuid,
    pub execution_id: Uuid,
    pub block_id: Uuid,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub share: Option<Share>,
}

pub enum MediaError {
    Invalid,
    /// Объявленный размер больше потолка (`MAX_SHARE_BYTES`, `MAX_MEDIA_BYTES`).
    TooLarge,
    /// Суммарный объём медиа машины с этой загрузкой превысил предел.
    Quota,
    /// Имя файла не проходит нормализацию вложений.
    InvalidName,
    /// Тип не принимается: неизвестный MIME, сигнатура не совпала с картинкой,
    /// пустой файл с типом картинки или видео.
    InvalidType,
    /// Заголовок или подпись длиннее предела либо с управляющими символами.
    InvalidText,
    NotFound,
    Conflict,
    Busy,
    ChecksumMismatch,
    Interrupted,
    Db(sqlx::Error),
    Blob(BlobError),
}

impl fmt::Debug for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Db(error) => f
                .debug_tuple("Db")
                .field(&crate::dberr::describe(error))
                .finish(),
            other => f.write_str(other.kind()),
        }
    }
}

impl MediaError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Invalid => "media_invalid",
            Self::TooLarge => "media_too_large",
            Self::Quota => "media_quota_exceeded",
            Self::InvalidName => "media_invalid_name",
            Self::InvalidType => "media_invalid_type",
            Self::InvalidText => "media_invalid_text",
            Self::NotFound => "media_not_found",
            Self::Conflict => "media_conflict",
            Self::Busy => "media_upload_in_progress",
            Self::ChecksumMismatch => "media_checksum_mismatch",
            Self::Interrupted => "media_body_interrupted",
            Self::Db(_) => "media_db_unavailable",
            Self::Blob(_) => "media_store_unavailable",
        }
    }
}

impl From<sqlx::Error> for MediaError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<BlobError> for MediaError {
    fn from(error: BlobError) -> Self {
        Self::Blob(error)
    }
}

impl From<io::Error> for MediaError {
    fn from(error: io::Error) -> Self {
        Self::Blob(BlobError::Io(error))
    }
}

fn uuid(value: &str) -> Result<Uuid, MediaError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(MediaError::Invalid)
}

fn optional_dimension(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

pub fn parse_header(raw: &MediaHeader) -> Result<Header, MediaError> {
    let share = if raw.share {
        let name =
            crate::attachments::normalize_name(&raw.name).map_err(|_| MediaError::InvalidName)?;
        // Пустой файл законен (.gitkeep, пустой журнал): blob нулевой длины с SHA-256 пустого ввода.
        if raw.size > MAX_SHARE_BYTES {
            return Err(MediaError::TooLarge);
        }
        if raw.title.len() > MAX_SHARE_TITLE_BYTES
            || raw.title.chars().any(char::is_control)
            || raw.note.len() > MAX_SHARE_NOTE_BYTES
        {
            return Err(MediaError::InvalidText);
        }
        Some(Share {
            name,
            title: raw.title.clone(),
            note: raw.note.clone(),
            width: optional_dimension(raw.width),
            height: optional_dimension(raw.height),
        })
    } else {
        None
    };
    let mime = match &share {
        Some(_) => crate::attachments::normalize_mime(Some(&raw.mime))
            .map_err(|_| MediaError::InvalidType)?,
        None => MEDIA_MIMES
            .iter()
            .copied()
            .find(|mime| *mime == raw.mime)
            .ok_or(MediaError::InvalidType)?
            .to_owned(),
    };
    // Пустой файл не бывает картинкой или видео: лента не должна открывать его плеером.
    if raw.size == 0 && (mime.starts_with("image/") || mime.starts_with("video/")) {
        return Err(MediaError::InvalidType);
    }
    let sha256 = raw.sha256.as_str();
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(MediaError::Invalid);
    }
    if !raw.share && raw.size > MAX_MEDIA_BYTES {
        return Err(MediaError::TooLarge);
    }
    Ok(Header {
        id: uuid(&raw.media_id)?,
        execution_id: uuid(&raw.execution_id)?,
        block_id: uuid(&raw.block_id)?,
        mime,
        size: raw.size,
        sha256: sha256.to_owned(),
        share,
    })
}

/// Сигнатура файла должна совпадать с объявленным типом: иначе тип не отдаётся браузеру.
pub fn signature_matches(mime: &str, head: &[u8]) -> bool {
    match mime {
        "image/png" => head.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => head.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a"),
        "image/webp" => head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP",
        _ => false,
    }
}

#[derive(sqlx::FromRow)]
struct Stored {
    execution_id: Uuid,
    block_id: Uuid,
    mime: String,
    size_bytes: i64,
    sha256: String,
}

impl Stored {
    fn same(&self, header: &Header) -> bool {
        self.execution_id == header.execution_id
            && self.block_id == header.block_id
            && self.mime == header.mime
            && self.size_bytes == header.size as i64
            && self.sha256 == header.sha256
    }
}

async fn stored(pool: &PgPool, id: Uuid) -> Result<Option<Stored>, sqlx::Error> {
    sqlx::query_as(
        "select execution_id, block_id, mime, size_bytes, sha256 from block_media where id=$1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Ответ на повтор: та же запись - успех, другая - конфликт.
fn replay(row: Option<Stored>, header: &Header) -> Option<Result<(), MediaError>> {
    row.map(|row| {
        if row.same(header) {
            Ok(())
        } else {
            Err(MediaError::Conflict)
        }
    })
}

async fn read_head(store: &dyn BlobStore, id: Uuid) -> Result<Vec<u8>, MediaError> {
    let (file, _) = store.open(id, 0).await?;
    let mut head = Vec::with_capacity(12);
    file.take(12).read_to_end(&mut head).await?;
    Ok(head)
}

/// Принимает тело в staging, проверяет длину и SHA-256, публикует (fsync файла, rename, fsync
/// каталогов), сверяет сигнатуру и только затем фиксирует строку `block_media`. Сбой между
/// публикацией и commit оставляет blob без строки; повтор с тем же ID и checksum проверяет
/// опубликованный blob без копирования и фиксирует метаданные.
#[derive(sqlx::FromRow)]
struct StoredShare {
    execution_id: Uuid,
    block_id: Uuid,
    mime: String,
    size_bytes: i64,
    sha256: String,
    name: String,
    title: String,
    note: String,
}

impl StoredShare {
    fn same(&self, header: &Header, share: &Share) -> bool {
        self.execution_id == header.execution_id
            && self.block_id == header.block_id
            && self.mime == header.mime
            && self.size_bytes == header.size as i64
            && self.sha256 == header.sha256
            && self.name == share.name
            && self.title == share.title
            && self.note == share.note
    }
}

async fn stored_share(pool: &PgPool, id: Uuid) -> Result<Option<StoredShare>, sqlx::Error> {
    sqlx::query_as(
        "select execution_id, block_id, mime, size_bytes, sha256, name, title, note
         from block_files where id=$1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

async fn insert_share(
    pool: &PgPool,
    conversation_id: Uuid,
    header: &Header,
    share: &Share,
) -> Result<(), MediaError> {
    let header = header.clone();
    let share = share.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let owner_id: Uuid = sqlx::query_scalar(
                "select owner_id from conversations where id=$1",
            )
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
            sqlx::query(
                "insert into attachments (id, owner_id, conversation_id, name, mime, size_bytes, sha256, state, ready_at)
                 values ($1, $2, $3, $4, $5, $6, $7, 'ready', clock_timestamp())
                 on conflict (id) do nothing",
            )
            .bind(header.id)
            .bind(owner_id)
            .bind(conversation_id)
            .bind(&share.name)
            .bind(&header.mime)
            .bind(header.size as i64)
            .bind(&header.sha256)
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "insert into block_files (id, execution_id, conversation_id, block_id, attachment_id, name, mime, size_bytes, sha256, title, note, width, height)
                 values ($1, $2, $3, $4, $1, $5, $6, $7, $8, $9, $10, $11, $12)
                 on conflict (id) do nothing",
            )
            .bind(header.id)
            .bind(header.execution_id)
            .bind(conversation_id)
            .bind(header.block_id)
            .bind(&share.name)
            .bind(&header.mime)
            .bind(header.size as i64)
            .bind(&header.sha256)
            .bind(&share.title)
            .bind(&share.note)
            .bind(share.width.map(|v| v as i32))
            .bind(share.height.map(|v| v as i32))
            .execute(&mut *conn)
            .await?;
            let row: Option<StoredShare> = sqlx::query_as(
                "select execution_id, block_id, mime, size_bytes, sha256, name, title, note
                 from block_files where id=$1",
            )
            .bind(header.id)
            .fetch_optional(&mut *conn)
            .await?;
            match row {
                Some(row) if row.same(&header, &share) => Ok(()),
                _ => Err(MediaError::Conflict),
            }
        })
    })
    .await
}

/// Оплаченный диском объём медиа машины: строки `block_media` и `block_files` держат blob
/// до удаления разговора или уборки брошенной записи.
async fn used_bytes(pool: &PgPool, machine_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum(size_bytes), 0)::bigint from (
           select m.size_bytes from block_media m
             join conversations c on c.id=m.conversation_id
            where c.machine_id=$1
           union all
           select f.size_bytes from block_files f
             join conversations c on c.id=f.conversation_id
            where c.machine_id=$1
         ) media",
    )
    .bind(machine_id)
    .fetch_one(pool)
    .await
}

/// `max_bytes = 0` выключает общий предел; он не заменяет пределы одного файла.
async fn check_quota(
    pool: &PgPool,
    machine_id: Uuid,
    size: u64,
    max_bytes: u64,
) -> Result<(), MediaError> {
    if max_bytes == 0 {
        return Ok(());
    }
    let used = i128::from(used_bytes(pool, machine_id).await?);
    if used + i128::from(size) > i128::from(max_bytes) {
        return Err(MediaError::Quota);
    }
    Ok(())
}

pub async fn put(
    pool: &PgPool,
    store: &dyn BlobStore,
    machine_id: Uuid,
    header: &Header,
    max_bytes: u64,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
) -> Result<(), MediaError> {
    let conversation_id: Uuid = sqlx::query_scalar(
        "select e.conversation_id from executions e join conversations c on c.id=e.conversation_id
         where e.id=$1 and c.machine_id=$2",
    )
    .bind(header.execution_id)
    .bind(machine_id)
    .fetch_optional(pool)
    .await?
    .ok_or(MediaError::NotFound)?;
    if let Some(share) = &header.share {
        if let Some(stored) = stored_share(pool, header.id).await? {
            return if stored.same(header, share) {
                Ok(())
            } else {
                Err(MediaError::Conflict)
            };
        }
        let foreign: bool =
            sqlx::query_scalar("select exists(select 1 from attachments where id=$1)")
                .bind(header.id)
                .fetch_one(pool)
                .await?;
        if foreign {
            return Err(MediaError::Conflict);
        }
    } else {
        let foreign: bool =
            sqlx::query_scalar("select exists(select 1 from attachments where id=$1)")
                .bind(header.id)
                .fetch_one(pool)
                .await?;
        if foreign {
            return Err(MediaError::Conflict);
        }
        if let Some(result) = replay(stored(pool, header.id).await?, header) {
            return result;
        }
    }
    // Повтор уже сохранённого выше не тратит квоту второй раз.
    check_quota(pool, machine_id, header.size, max_bytes).await?;
    let Some(locked) = store.try_lock(header.id)? else {
        return Err(MediaError::Busy);
    };
    let store = &*locked;
    if let Some(result) = replay(stored(pool, header.id).await?, header) {
        return result;
    }
    let id = header.id;
    match store.open(id, 0).await {
        // Повтор после сбоя между публикацией и commit: тело не нужно, blob проверяется заново.
        Ok(_) => match store.publish(id, header.size, &header.sha256).await {
            Ok(()) => {}
            Err(BlobError::ChecksumMismatch | BlobError::LengthMismatch { .. }) => {
                return Err(MediaError::Conflict);
            }
            Err(error) => return Err(error.into()),
        },
        Err(BlobError::NotFound) => {
            store.delete_staging(id).await?;
            let received = receive(store, header, body).await;
            if let Err(error) = received {
                if let Err(error) = store.delete_staging(id).await {
                    tracing::warn!(kind = "media_staging_delete_failed", media_id = %id, error = %error.kind(), "staging delete failed");
                }
                return Err(error);
            }
        }
        Err(error) => return Err(error.into()),
    }
    let share = header.share.clone();
    let head = read_head(store, id).await?;
    let image = header.mime.starts_with("image/");
    if (header.share.is_none() || image) && !signature_matches(&header.mime, &head) {
        store.delete_blob(id).await?;
        return Err(MediaError::InvalidType);
    }
    if let Some(share) = share {
        return insert_share(pool, conversation_id, header, &share).await;
    }
    let header = header.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query(
                "insert into block_media (id, execution_id, conversation_id, block_id, mime, size_bytes, sha256)
                 values ($1, $2, $3, $4, $5, $6, $7) on conflict (id) do nothing",
            )
            .bind(id)
            .bind(header.execution_id)
            .bind(conversation_id)
            .bind(header.block_id)
            .bind(header.mime.clone())
            .bind(header.size as i64)
            .bind(header.sha256.clone())
            .execute(&mut *conn)
            .await?;
            let row: Stored = sqlx::query_as(
                "select execution_id, block_id, mime, size_bytes, sha256 from block_media where id=$1",
            )
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
            if !row.same(&header) {
                return Err(MediaError::Conflict);
            }
            Ok(())
        })
    })
    .await
}

async fn receive(
    store: &dyn BlobStore,
    header: &Header,
    body: BoxStream<'static, Result<Bytes, io::Error>>,
) -> Result<(), MediaError> {
    let written = match store.append_staging(header.id, 0, body, header.size).await {
        Ok(written) => written,
        Err(BlobError::TooLarge { .. }) => return Err(MediaError::Invalid),
        Err(BlobError::BodyInterrupted { .. }) => return Err(MediaError::Interrupted),
        Err(error) => return Err(error.into()),
    };
    if written != header.size {
        return Err(MediaError::Invalid);
    }
    match store.publish(header.id, header.size, &header.sha256).await {
        Ok(()) => Ok(()),
        Err(BlobError::ChecksumMismatch) => Err(MediaError::ChecksumMismatch),
        Err(BlobError::LengthMismatch { .. }) => Err(MediaError::Invalid),
        Err(error) => Err(error.into()),
    }
}

/// Запись `BlockMedia`: ссылка появляется только на опубликованную строку того же исполнения,
/// блока, типа, размера и checksum. Иначе запись пропускается (`true`), курсор журнала идёт дальше.
pub async fn attach_in_tx(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockMedia,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let parse = |value: &str| {
        Uuid::parse_str(value)
            .ok()
            .filter(|id| !id.is_nil())
            .ok_or_else(|| sqlx::Error::InvalidArgument("invalid media reference".into()))
    };
    let id = parse(&fact.media_id)?;
    let block_id = parse(&fact.block_id)?;
    let row: Option<(Uuid, String, i64, String, bool)> = sqlx::query_as(
        "select block_id, mime, size_bytes, sha256, attached_at is not null from block_media
         where id=$1 and execution_id=$2 and conversation_id=$3 for update",
    )
    .bind(id)
    .bind(run.execution_id)
    .bind(run.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((stored_block, mime, size, sha256, attached)) = row else {
        return Ok(true);
    };
    if attached
        || stored_block != block_id
        || mime != fact.mime
        || size != fact.size as i64
        || sha256 != fact.sha256
    {
        return Ok(true);
    }
    let block: bool = sqlx::query_scalar(
        "select exists(select 1 from blocks where block_id=$1 and execution_id=$2)",
    )
    .bind(block_id)
    .bind(run.execution_id)
    .fetch_one(&mut *conn)
    .await?;
    if !block {
        return Ok(true);
    }
    let dimension = |value: u32| i32::try_from(value).ok().filter(|v| *v > 0);
    let (width, height) = (dimension(fact.width), dimension(fact.height));
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "block_media_attached",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({"block_id": block_id, "media_id": id, "mime": mime, "size": size,
                "width": width, "height": height}),
            correlation_id,
        },
    )
    .await?;
    sqlx::query(
        "update block_media set attached_at=$2, attached_seq=$3, width=$4, height=$5 where id=$1",
    )
    .bind(id)
    .bind(event.committed_at)
    .bind(event.scope_seq)
    .bind(width)
    .bind(height)
    .execute(conn)
    .await?;
    Ok(false)
}

/// Файл, который агент отдал владельцу, в моделях чтения блока.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct FileView {
    pub attachment_id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub title: String,
    pub note: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Файлы блоков страницы одним запросом.
pub async fn files_for_blocks(
    conn: &mut PgConnection,
    block_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<FileView>>, sqlx::Error> {
    let rows = sqlx::query(
        "select block_id, attachment_id, name, mime, size_bytes size, title, note, width, height
         from block_files where block_id=any($1) and attached_at is not null
         order by block_id, attached_seq, id",
    )
    .bind(block_ids)
    .fetch_all(conn)
    .await?;
    let mut files: std::collections::HashMap<Uuid, Vec<FileView>> = Default::default();
    for row in &rows {
        use sqlx::{FromRow, Row};
        files
            .entry(row.try_get("block_id")?)
            .or_default()
            .push(FileView::from_row(row)?);
    }
    Ok(files)
}

/// Запись `BlockFile`: ссылка появляется только на опубликованную строку того же исполнения,
/// блока, имени, типа, размера и checksum.
pub async fn attach_file_in_tx(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockFile,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let parse = |value: &str| {
        Uuid::parse_str(value)
            .ok()
            .filter(|id| !id.is_nil())
            .ok_or_else(|| sqlx::Error::InvalidArgument("invalid file reference".into()))
    };
    let id = parse(&fact.attachment_id)?;
    let block_id = parse(&fact.block_id)?;
    // Сборщик освобождает вложение под блокировкой его строки, а затем удаляет строку файла:
    // порядок тот же, освобождённое вложение уже без blob.
    let ready: bool = sqlx::query_scalar(
        "select coalesce((select state='ready' from attachments where id=$1 for update), false)",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if !ready {
        return Ok(true);
    }
    let row: Option<(String, String, i64, String, String, String, bool)> = sqlx::query_as(
        "select mime, name, size_bytes, sha256, title, note, attached_at is not null
         from block_files where id=$1 and execution_id=$2 and conversation_id=$3
           and block_id=$4 for update",
    )
    .bind(id)
    .bind(run.execution_id)
    .bind(run.conversation_id)
    .bind(block_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((mime, name, size, sha256, title, note, attached)) = row else {
        return Ok(true);
    };
    if attached
        || mime != fact.mime
        || name != fact.name
        || size != fact.size as i64
        || sha256 != fact.sha256
        || title != fact.title
        || note != fact.note
    {
        return Ok(true);
    }
    let block: bool = sqlx::query_scalar(
        "select exists(select 1 from blocks where block_id=$1 and execution_id=$2)",
    )
    .bind(block_id)
    .bind(run.execution_id)
    .fetch_one(&mut *conn)
    .await?;
    if !block {
        return Ok(true);
    }
    let dimension = |value: u32| i32::try_from(value).ok().filter(|v| *v > 0);
    let (width, height) = (dimension(fact.width), dimension(fact.height));
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "block_file_attached",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({
                "block_id": block_id,
                "attachment_id": id,
                "name": name,
                "mime": mime,
                "size": size,
                "title": title,
                "note": note,
                "width": width,
                "height": height,
            }),
            correlation_id,
        },
    )
    .await?;
    sqlx::query(
        "update block_files set attached_at=$2, attached_seq=$3, width=coalesce($4,width), height=coalesce($5,height) where id=$1",
    )
    .bind(id)
    .bind(event.committed_at)
    .bind(event.scope_seq)
    .bind(width)
    .bind(height)
    .execute(&mut *conn)
    .await?;
    Ok(false)
}

/// Картинка в моделях чтения блока.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct MediaView {
    pub media_id: Uuid,
    pub mime: String,
    pub size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Картинки блоков страницы одним запросом.
pub async fn for_blocks(
    conn: &mut PgConnection,
    block_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<MediaView>>, sqlx::Error> {
    let rows = sqlx::query(
        "select block_id, id media_id, mime, size_bytes size, width, height from block_media
         where block_id=any($1) and attached_at is not null order by block_id, attached_seq, id",
    )
    .bind(block_ids)
    .fetch_all(conn)
    .await?;
    let mut media: std::collections::HashMap<Uuid, Vec<MediaView>> = Default::default();
    for row in &rows {
        use sqlx::{FromRow, Row};
        media
            .entry(row.try_get("block_id")?)
            .or_default()
            .push(MediaView::from_row(row)?);
    }
    Ok(media)
}

/// Связанная с блоком картинка владельца для выдачи браузеру.
pub struct Download {
    pub mime: String,
    pub size: u64,
    pub sha256: String,
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
) -> Result<Download, MediaError> {
    let (mime, size, sha256): (String, i64, String) = sqlx::query_as(
        "select m.mime, m.size_bytes, m.sha256 from block_media m
         join conversations c on c.id=m.conversation_id
         where m.id=$1 and c.owner_id=$2 and m.attached_at is not null",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?
    .ok_or(MediaError::NotFound)?;
    let (file, len) = match store.open(id, 0).await {
        Ok(opened) => opened,
        Err(BlobError::NotFound) => {
            tracing::warn!(kind = "media_blob_missing", media_id = %id, "referenced media blob is missing");
            return Err(MediaError::NotFound);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Download {
        mime,
        size: len.min(size as u64),
        sha256,
        file,
    })
}
