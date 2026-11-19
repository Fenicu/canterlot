//! Инструменты общей памяти: `memory_search`, `memory_get`, `memory_write`, `memory_update`,
//! `memory_delete`. Область берётся из строки вызывающего разговора, а не из аргументов;
//! у разговора без домена каждый инструмент отвечает `memory_unavailable`.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, parse_arguments};
use crate::db;
use crate::memory::store::{self, Actor, Kind, Level, MemoryError, MemoryScope, NewEntry, Patch};

fn code_of(error: &MemoryError) -> ToolErrorCode {
    match error {
        MemoryError::LooksLikeSecret { .. } => ToolErrorCode::LooksLikeSecret,
        MemoryError::TooLarge { .. } => ToolErrorCode::TooLarge,
        MemoryError::SubjectExists { .. } => ToolErrorCode::SubjectExists,
        MemoryError::VersionConflict { .. } => ToolErrorCode::VersionConflict,
        MemoryError::NotFound => ToolErrorCode::NotFound,
        MemoryError::RateLimited { .. } => ToolErrorCode::RateLimited,
        MemoryError::DomainFull { .. } => ToolErrorCode::DomainFull,
        MemoryError::BadLevel => ToolErrorCode::BadLevel,
        MemoryError::Invalid { .. } => ToolErrorCode::Invalid,
        MemoryError::MemoryUnavailable => ToolErrorCode::MemoryUnavailable,
        MemoryError::Db(_) => ToolErrorCode::Internal,
    }
}

/// Отказ с метаданными в тексте: `{"id": …, "version": …}` и подобное. Значения секретов
/// и тексты записей сюда не попадают.
fn fail(error: MemoryError) -> ToolError {
    if let MemoryError::Db(error) = error {
        return error.into();
    }
    let code = code_of(&error);
    let metadata = error.metadata();
    let empty = metadata.as_object().is_none_or(|object| object.is_empty());
    if empty {
        ToolError::new(code)
    } else {
        ToolError::with_message(code, format!("{} {metadata}", code.default_message()))
    }
}

impl From<MemoryError> for ToolError {
    fn from(error: MemoryError) -> Self {
        fail(error)
    }
}

fn parse_id(text: &str) -> Result<Uuid, ToolError> {
    text.parse()
        .map_err(|_| fail(MemoryError::Invalid { field: "id" }))
}

fn parse_kind(text: &str) -> Result<Kind, ToolError> {
    Kind::parse(text).ok_or_else(|| fail(MemoryError::Invalid { field: "kind" }))
}

fn parse_level(text: &str) -> Result<Level, ToolError> {
    Level::parse(text).ok_or_else(|| fail(MemoryError::Invalid { field: "level" }))
}

fn parse_time(text: &str) -> Result<DateTime<Utc>, ToolError> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|_| {
            fail(MemoryError::Invalid {
                field: "valid_until",
            })
        })
}

async fn scope_of(conn: &mut PgConnection, caller: &Caller) -> Result<MemoryScope, ToolError> {
    MemoryScope::of_caller(conn, caller.conversation_id)
        .await?
        .ok_or_else(|| fail(MemoryError::MemoryUnavailable))
}

async fn actor_of(conn: &mut PgConnection, caller: &Caller) -> Result<Actor, ToolError> {
    let row: Option<(String, String)> =
        sqlx::query_as("select harness, model from conversations where id = $1")
            .bind(caller.conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
    let (harness, model) = row.unwrap_or_default();
    Ok(Actor::Agent {
        conversation_id: caller.conversation_id,
        harness,
        model,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    #[serde(default)]
    query: String,
    kind: Option<String>,
    #[serde(default)]
    include_other_projects: bool,
    limit: Option<u32>,
}

pub async fn search(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SearchArgs = parse_arguments("memory_search", arguments)?;
    let kind = args.kind.as_deref().map(parse_kind).transpose()?;
    let mut tx = db::begin(&context.pool).await?;
    let scope = scope_of(&mut tx, caller).await?;
    let rows = store::search(
        &mut tx,
        &scope,
        &args.query,
        kind,
        args.include_other_projects,
        args.limit.unwrap_or(context.memory.search_limit),
        &context.memory,
    )
    .await?;
    tx.commit().await?;
    Ok(json!({
        "count": rows.len(),
        "results": rows.iter().map(store::SearchRow::to_json).collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    id: String,
}

pub async fn get(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: GetArgs = parse_arguments("memory_get", arguments)?;
    let id = parse_id(&args.id)?;
    let mut tx = db::begin(&context.pool).await?;
    let scope = scope_of(&mut tx, caller).await?;
    // Запись, найденная поиском по всем проектам домена, читается целиком.
    let entry = store::get(&mut tx, &scope, id, true).await?;
    tx.commit().await?;
    entry
        .map(|entry| entry.to_json())
        .ok_or_else(|| fail(MemoryError::NotFound))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    kind: String,
    subject: String,
    body: String,
    level: Option<String>,
    valid_until: Option<String>,
}

pub async fn write(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: WriteArgs = parse_arguments("memory_write", arguments)?;
    let new = NewEntry {
        kind: parse_kind(&args.kind)?,
        subject: args.subject,
        body: args.body,
        level: args
            .level
            .as_deref()
            .map_or(Ok(Level::Project), parse_level)?,
        valid_until: args.valid_until.as_deref().map(parse_time).transpose()?,
    };
    let mut tx = db::begin(&context.pool).await?;
    let scope = scope_of(&mut tx, caller).await?;
    let actor = actor_of(&mut tx, caller).await?;
    let written = store::write(&mut tx, &scope, actor, new, &context.memory).await?;
    tx.commit().await?;
    Ok(written.to_json())
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    id: String,
    expected_version: i32,
    kind: Option<String>,
    subject: Option<String>,
    body: Option<String>,
    level: Option<String>,
    /// Отсутствует: не трогать; `null`: снять срок.
    #[serde(default, deserialize_with = "double_option")]
    valid_until: Option<Option<String>>,
}

pub async fn update(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: UpdateArgs = parse_arguments("memory_update", arguments)?;
    let id = parse_id(&args.id)?;
    let patch = Patch {
        kind: args.kind.as_deref().map(parse_kind).transpose()?,
        subject: args.subject,
        body: args.body,
        level: args.level.as_deref().map(parse_level).transpose()?,
        valid_until: match args.valid_until {
            None => None,
            Some(None) => Some(None),
            Some(Some(text)) => Some(Some(parse_time(&text)?)),
        },
    };
    let mut tx = db::begin(&context.pool).await?;
    let scope = scope_of(&mut tx, caller).await?;
    let actor = actor_of(&mut tx, caller).await?;
    let written = store::update(
        &mut tx,
        &scope,
        actor,
        id,
        args.expected_version,
        patch,
        &context.memory,
    )
    .await?;
    tx.commit().await?;
    Ok(written.to_json())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    id: String,
    expected_version: i32,
}

/// Для агента удаление — архивирование: вернуть запись может только владелец.
pub async fn delete(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: DeleteArgs = parse_arguments("memory_delete", arguments)?;
    let id = parse_id(&args.id)?;
    let mut tx = db::begin(&context.pool).await?;
    let scope = scope_of(&mut tx, caller).await?;
    let actor = actor_of(&mut tx, caller).await?;
    let written = store::archive(
        &mut tx,
        &scope,
        actor,
        id,
        args.expected_version,
        &context.memory,
    )
    .await?;
    tx.commit().await?;
    Ok(json!({"id": written.id, "status": "archived", "version": written.version}))
}
