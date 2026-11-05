//! Черновик поля ввода: одна серверная копия на разговор, общая для устройств владельца.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::events::{self, NewEvent, Scope};

/// Предел текста черновика в байтах UTF-8.
pub const MAX_TEXT_BYTES: usize = 65_536;

#[derive(Debug, Clone, serde::Serialize, ToSchema)]
pub struct DraftView {
    pub text: String,
    pub revision: i64,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutDraft {
    pub text: String,
    #[serde(default)]
    pub base_revision: i64,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug)]
pub enum DraftError {
    NotFound,
    TooLong,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DraftError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

fn view(row: (String, i64, DateTime<Utc>)) -> DraftView {
    DraftView {
        text: row.0,
        revision: row.1,
        updated_at: Some(crate::conversations::time(row.2)),
    }
}

fn empty() -> DraftView {
    DraftView {
        text: String::new(),
        revision: 0,
        updated_at: None,
    }
}

async fn owned(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(conn)
    .await
}

pub async fn get(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<DraftView, DraftError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if !owned(conn, owner_id, conversation_id).await? {
                return Err(DraftError::NotFound);
            }
            let row: Option<(String, i64, DateTime<Utc>)> = sqlx::query_as(
                "select text, revision, updated_at from composer_drafts where conversation_id=$1",
            )
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
            Ok(row.map(view).unwrap_or_else(empty))
        })
    })
    .await
}

/// Полная замена текста: `base_revision` принимается для совместимости клиента,
/// но не участвует в слиянии. Побеждает последняя запись, ответ несёт ревизию хаба.
pub async fn put(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    body: PutDraft,
    correlation_id: Uuid,
) -> Result<DraftView, DraftError> {
    if body.text.len() > MAX_TEXT_BYTES {
        return Err(DraftError::TooLong);
    }
    let PutDraft {
        text,
        base_revision: _,
        device_id,
    } = body;
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Строка разговора сериализует записи черновика и удаление разговора.
            let exists: Option<Uuid> =
                sqlx::query_scalar("select id from conversations where id=$1 and owner_id=$2 for update")
                    .bind(conversation_id)
                    .bind(owner_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            if exists.is_none() {
                return Err(DraftError::NotFound);
            }
            if text.is_empty() {
                let removed: Option<i64> = sqlx::query_scalar(
                    "delete from composer_drafts where conversation_id=$1 returning revision",
                )
                .bind(conversation_id)
                .fetch_optional(&mut *conn)
                .await?;
                let Some(_) = removed else {
                    return Ok(empty());
                };
                let revision: i64 =
                    sqlx::query_scalar("select nextval('composer_draft_revision_seq')")
                        .fetch_one(&mut *conn)
                        .await?;
                publish(conn, conversation_id, revision, "", device_id.as_deref(), correlation_id)
                    .await?;
                return Ok(DraftView {
                    text: String::new(),
                    revision,
                    updated_at: None,
                });
            }
            let revision: i64 = sqlx::query_scalar("select nextval('composer_draft_revision_seq')")
                .fetch_one(&mut *conn)
                .await?;
            let updated_at: DateTime<Utc> = sqlx::query_scalar(
                "insert into composer_drafts(conversation_id,owner_id,text,revision,updated_at,device_id)
                 values($1,$2,$3,$4,clock_timestamp(),$5)
                 on conflict (conversation_id) do update
                 set owner_id=excluded.owner_id, text=excluded.text,
                     revision=excluded.revision, updated_at=excluded.updated_at,
                     device_id=excluded.device_id
                 returning updated_at",
            )
            .bind(conversation_id)
            .bind(owner_id)
            .bind(&text)
            .bind(revision)
            .bind(device_id.as_deref())
            .fetch_one(&mut *conn)
            .await?;
            publish(conn, conversation_id, revision, &text, device_id.as_deref(), correlation_id)
                .await?;
            Ok(DraftView {
                text,
                revision,
                updated_at: Some(crate::conversations::time(updated_at)),
            })
        })
    })
    .await
}

/// Удаляет черновик в уже открытой транзакции приёма ввода или планирования.
/// Событие с пустым текстом очищает поле на остальных устройствах.
pub async fn clear_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    device_id: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let removed: Option<i64> = sqlx::query_scalar(
        "delete from composer_drafts where conversation_id=$1 returning revision",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    if removed.is_none() {
        return Ok(());
    }
    let revision: i64 = sqlx::query_scalar("select nextval('composer_draft_revision_seq')")
        .fetch_one(&mut *conn)
        .await?;
    publish(conn, conversation_id, revision, "", device_id, correlation_id).await?;
    Ok(())
}

/// Снимок уже проверил владельца и читает черновик в своей транзакции.
pub async fn in_snapshot(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<DraftView>, sqlx::Error> {
    let row: Option<(String, i64, DateTime<Utc>)> = sqlx::query_as(
        "select text, revision, updated_at from composer_drafts where conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(view))
}

async fn publish(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    revision: i64,
    text: &str,
    device_id: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "draft_changed",
            conversation_id: Some(conversation_id),
            execution_id: None,
            payload: json!({
                "conversation_id": conversation_id,
                "revision": revision,
                "text": text,
                "device_id": device_id,
            }),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
