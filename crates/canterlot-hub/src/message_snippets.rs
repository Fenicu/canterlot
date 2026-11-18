//! Заготовки сообщений владельца: глобальные и привязанные к проекту (канонический workdir).
//!
//! Проекта как отдельной сущности в хабе нет: ключ проекта — workdir сессии, откуда
//! заготовка была сохранена. Одна ревизия на строку: каждая правка меняет её на единицу,
//! устаревшая ревизия получает 409. Текст заготовки — данные владельца: в `Debug` только длина.

use crate::{conversations, idempotency};
use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};
use utoipa::ToSchema;
use uuid::Uuid;

pub const MAX_TITLE_CHARS: usize = 80;
pub const MAX_BODY_BYTES: usize = MAX_INPUT_TEXT_BYTES;
pub const MAX_SNIPPETS_PER_OWNER: i64 = 100;
pub const SCOPE_GLOBAL: &str = "global";
pub const SCOPE_PROJECT: &str = "project";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct MessageSnippet {
    pub id: Uuid,
    /// `global` или `project`.
    pub scope: String,
    /// Канонический workdir для `project`, пустая строка для `global`.
    pub project_key: String,
    pub title: String,
    pub body: String,
    pub position: i32,
    pub revision: i64,
    #[serde(with = "crate::model::utc_time")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::model::utc_time")]
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for MessageSnippet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageSnippet")
            .field("id", &self.id)
            .field("scope", &self.scope)
            .field("project_key", &self.project_key)
            .field("title", &self.title)
            .field("body_bytes", &self.body.len())
            .field("position", &self.position)
            .field("revision", &self.revision)
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewSnippet {
    pub scope: String,
    #[serde(default)]
    pub project_key: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub position: Option<i32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SnippetPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub position: Option<i32>,
    pub base_revision: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReorderSnippets {
    pub scope: String,
    #[serde(default)]
    pub project_key: String,
    pub ids: Vec<Uuid>,
}

#[derive(Debug)]
pub enum MessageSnippetError {
    NotFound,
    RevisionConflict { current: i64 },
    TitleTaken,
    LimitReached,
    Invalid { field: &'static str },
    IdempotencyConflict,
    Db(sqlx::Error),
}

impl From<idempotency::IdempotencyConflict> for MessageSnippetError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

impl From<sqlx::Error> for MessageSnippetError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

fn scope_of(scope: &str, project_key: &str) -> Result<(), MessageSnippetError> {
    match scope {
        SCOPE_GLOBAL if project_key.is_empty() => Ok(()),
        SCOPE_PROJECT
            if !project_key.is_empty()
                && project_key.len() <= canterlot_proto::limits::MAX_WORKDIR_BYTES =>
        {
            Ok(())
        }
        _ => Err(MessageSnippetError::Invalid { field: "scope" }),
    }
}

fn validate_title(title: &str) -> Result<(), MessageSnippetError> {
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(MessageSnippetError::Invalid { field: "title" });
    }
    Ok(())
}

fn validate_body(body: &str) -> Result<(), MessageSnippetError> {
    if body.is_empty() || body.len() > MAX_BODY_BYTES {
        return Err(MessageSnippetError::Invalid { field: "body" });
    }
    Ok(())
}

async fn next_position(
    conn: &mut PgConnection,
    owner: Uuid,
    scope: &str,
    project_key: &str,
) -> Result<i32, sqlx::Error> {
    let row = sqlx::query(
        "select coalesce(max(position), -1) + 1 as next from message_snippets
         where owner_id=$1 and scope=$2 and project_key=$3 and deleted_at is null",
    )
    .bind(owner)
    .bind(scope)
    .bind(project_key)
    .fetch_one(conn)
    .await?;
    row.try_get::<i32, _>("next")
}

async fn publish(
    conn: &mut PgConnection,
    owner: Uuid,
    snippet: Option<&MessageSnippet>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    conversations::append(
        conn,
        crate::events::Scope::Conversations { owner_id: owner },
        "message_snippet_changed",
        None,
        json!({"snippet": snippet, "deleted": snippet.is_none()}),
        correlation_id,
    )
    .await
}

pub async fn list(
    conn: &mut PgConnection,
    owner: Uuid,
    scope: &str,
    project_key: &str,
) -> Result<Vec<MessageSnippet>, MessageSnippetError> {
    scope_of(scope, project_key)?;
    Ok(sqlx::query_as(
        "select id, scope, project_key, title, body, position, revision, created_at, updated_at
         from message_snippets
         where owner_id=$1 and scope=$2 and project_key=$3 and deleted_at is null
         order by position, created_at",
    )
    .bind(owner)
    .bind(scope)
    .bind(project_key)
    .fetch_all(conn)
    .await?)
}

async fn create_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    body: NewSnippet,
    correlation_id: Uuid,
) -> Result<MessageSnippet, MessageSnippetError> {
    conversations::lock_owner(&mut *conn, owner).await?;
    scope_of(&body.scope, &body.project_key)?;
    validate_title(&body.title)?;
    validate_body(&body.body)?;
    let total: i64 = sqlx::query_scalar(
        "select count(*) from message_snippets where owner_id=$1 and deleted_at is null",
    )
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    if total >= MAX_SNIPPETS_PER_OWNER {
        return Err(MessageSnippetError::LimitReached);
    }
    let taken: bool = sqlx::query_scalar(
        "select exists(select 1 from message_snippets where owner_id=$1 and scope=$2
          and project_key=$3 and lower(title)=lower($4) and deleted_at is null)",
    )
    .bind(owner)
    .bind(&body.scope)
    .bind(&body.project_key)
    .bind(&body.title)
    .fetch_one(&mut *conn)
    .await?;
    if taken {
        return Err(MessageSnippetError::TitleTaken);
    }
    let position = match body.position {
        Some(value) if value >= 0 => value,
        Some(_) => return Err(MessageSnippetError::Invalid { field: "position" }),
        None => next_position(&mut *conn, owner, &body.scope, &body.project_key).await?,
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into message_snippets (id, owner_id, scope, project_key, title, body, position, revision)
         values ($1,$2,$3,$4,$5,$6,$7,1)",
    )
    .bind(id)
    .bind(owner)
    .bind(&body.scope)
    .bind(&body.project_key)
    .bind(&body.title)
    .bind(&body.body)
    .bind(position)
    .execute(&mut *conn)
    .await?;
    let snippet = get(&mut *conn, owner, id).await?;
    publish(&mut *conn, owner, Some(&snippet), correlation_id).await?;
    Ok(snippet)
}

pub async fn get(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<MessageSnippet, MessageSnippetError> {
    sqlx::query_as(
        "select id, scope, project_key, title, body, position, revision, created_at, updated_at
         from message_snippets where owner_id=$1 and id=$2 and deleted_at is null",
    )
    .bind(owner)
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(MessageSnippetError::NotFound)
}

pub async fn create(
    pool: &PgPool,
    owner: Uuid,
    key: &str,
    body: NewSnippet,
    correlation_id: Uuid,
) -> Result<MessageSnippet, MessageSnippetError> {
    let request = json!({
        "scope": body.scope,
        "project_key": body.project_key,
        "title": body.title,
        "body": body.body,
        "position": body.position,
    });
    let outcome = idempotency::run_in_tx(
        pool,
        owner,
        key,
        "create_message_snippet",
        &request,
        None,
        move |conn| Box::pin(create_in_tx(conn, owner, body, correlation_id)),
    )
    .await?;
    Ok(outcome.value)
}

async fn update_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    patch: SnippetPatch,
    correlation_id: Uuid,
) -> Result<MessageSnippet, MessageSnippetError> {
    conversations::lock_owner(&mut *conn, owner).await?;
    let current = get(&mut *conn, owner, id).await?;
    if patch.base_revision != current.revision {
        return Err(MessageSnippetError::RevisionConflict {
            current: current.revision,
        });
    }
    let title = patch.title.unwrap_or_else(|| current.title.clone());
    let body = patch.body.unwrap_or_else(|| current.body.clone());
    let position = patch.position.unwrap_or(current.position);
    validate_title(&title)?;
    validate_body(&body)?;
    if position < 0 {
        return Err(MessageSnippetError::Invalid { field: "position" });
    }
    let taken: bool = sqlx::query_scalar(
        "select exists(select 1 from message_snippets where owner_id=$1 and scope=$2
          and project_key=$3 and lower(title)=lower($4) and id<>$5 and deleted_at is null)",
    )
    .bind(owner)
    .bind(&current.scope)
    .bind(&current.project_key)
    .bind(&title)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if taken {
        return Err(MessageSnippetError::TitleTaken);
    }
    sqlx::query(
        "update message_snippets set title=$3, body=$4, position=$5, revision=revision+1,
          updated_at=clock_timestamp() where owner_id=$1 and id=$2 and deleted_at is null",
    )
    .bind(owner)
    .bind(id)
    .bind(&title)
    .bind(&body)
    .bind(position)
    .execute(&mut *conn)
    .await?;
    let snippet = get(&mut *conn, owner, id).await?;
    publish(&mut *conn, owner, Some(&snippet), correlation_id).await?;
    Ok(snippet)
}

pub async fn update(
    pool: &PgPool,
    owner: Uuid,
    id: Uuid,
    key: &str,
    patch: SnippetPatch,
    correlation_id: Uuid,
) -> Result<MessageSnippet, MessageSnippetError> {
    let request = json!({
        "id": id,
        "title": patch.title,
        "body": patch.body,
        "position": patch.position,
        "base_revision": patch.base_revision,
    });
    let outcome = idempotency::run_in_tx(
        pool,
        owner,
        key,
        "update_message_snippet",
        &request,
        None,
        move |conn| Box::pin(update_in_tx(conn, owner, id, patch, correlation_id)),
    )
    .await?;
    Ok(outcome.value)
}

async fn delete_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<(), MessageSnippetError> {
    conversations::lock_owner(&mut *conn, owner).await?;
    get(&mut *conn, owner, id).await?;
    sqlx::query(
        "update message_snippets set deleted_at=clock_timestamp(), position=-1,
          updated_at=clock_timestamp() where owner_id=$1 and id=$2 and deleted_at is null",
    )
    .bind(owner)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    publish(&mut *conn, owner, None::<&MessageSnippet>, correlation_id).await?;
    Ok(())
}

pub async fn delete(
    pool: &PgPool,
    owner: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), MessageSnippetError> {
    idempotency::run_in_tx(
        pool,
        owner,
        key,
        "delete_message_snippet",
        &id,
        None,
        move |conn| Box::pin(delete_in_tx(conn, owner, id, correlation_id)),
    )
    .await?;
    Ok(())
}

async fn reorder_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    body: ReorderSnippets,
    correlation_id: Uuid,
) -> Result<Vec<MessageSnippet>, MessageSnippetError> {
    conversations::lock_owner(&mut *conn, owner).await?;
    scope_of(&body.scope, &body.project_key)?;
    if body.ids.is_empty() || body.ids.len() > MAX_SNIPPETS_PER_OWNER as usize {
        return Err(MessageSnippetError::Invalid { field: "ids" });
    }
    let found: Vec<Uuid> = sqlx::query_scalar(
        "select id from message_snippets where owner_id=$1 and scope=$2 and project_key=$3
          and deleted_at is null",
    )
    .bind(owner)
    .bind(&body.scope)
    .bind(&body.project_key)
    .fetch_all(&mut *conn)
    .await?;
    let mut unique = std::collections::BTreeSet::new();
    for id in &body.ids {
        if !found.contains(id) || !unique.insert(id) {
            return Err(MessageSnippetError::NotFound);
        }
    }
    for (position, id) in body.ids.iter().enumerate() {
        sqlx::query(
            "update message_snippets set position=$4, revision=revision+1,
              updated_at=clock_timestamp() where owner_id=$1 and id=$2 and scope=$3",
        )
        .bind(owner)
        .bind(id)
        .bind(&body.scope)
        .bind(position as i32)
        .execute(&mut *conn)
        .await?;
    }
    let items = list(&mut *conn, owner, &body.scope, &body.project_key).await?;
    for snippet in &items {
        publish(&mut *conn, owner, Some(snippet), correlation_id).await?;
    }
    Ok(items)
}

pub async fn reorder(
    pool: &PgPool,
    owner: Uuid,
    key: &str,
    body: ReorderSnippets,
    correlation_id: Uuid,
) -> Result<Vec<MessageSnippet>, MessageSnippetError> {
    let request = json!({
        "scope": body.scope,
        "project_key": body.project_key,
        "ids": body.ids,
    });
    let outcome = idempotency::run_in_tx(
        pool,
        owner,
        key,
        "reorder_message_snippets",
        &request,
        None,
        move |conn| Box::pin(reorder_in_tx(conn, owner, body, correlation_id)),
    )
    .await?;
    Ok(outcome.value)
}
