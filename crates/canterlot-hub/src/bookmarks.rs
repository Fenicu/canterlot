//! Избранные сообщения владельца. Закладка хранит только ссылку на ввод или верхний блок
//! ленты: фрагмент читается из цели при выдаче, поэтому правка и сжатие истории не оставляют
//! устаревший текст.

use std::fmt;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    conversations::time,
    events::{self, NewEvent, Scope},
    search::{HitKind, OriginKind},
};

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 100;
pub const CONVERSATION_LIMIT: usize = 500;
pub const EXCERPT_CHARS: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Input,
    Block,
}

impl TargetKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "input" => Some(Self::Input),
            "block" => Some(Self::Block),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Block => "block",
        }
    }
}

/// Закладка с полями перехода, как у совпадения поиска: `block_id` или `input_id` и `seq`
/// открывают ленту на нужном месте, `jump_seq` — место верхнего предка (у закладки это
/// всегда сама цель: вложенные блоки не бронируются).
#[derive(Clone, Serialize, ToSchema)]
pub struct BookmarkView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub target_kind: TargetKind,
    pub target_id: Uuid,
    pub created_at: String,
    pub target_created_at: String,
    pub kind: HitKind,
    #[schema(required = true)]
    pub origin_kind: Option<OriginKind>,
    #[schema(required = true)]
    pub block_id: Option<Uuid>,
    #[schema(required = true)]
    pub input_id: Option<Uuid>,
    pub seq: i64,
    pub jump_seq: i64,
    pub harness: String,
    pub model: String,
    pub archived: bool,
    /// `owner` у ввода и `agent` у блока; имя агента — в `agent`.
    pub role: String,
    #[schema(required = true)]
    pub agent: Option<String>,
    pub excerpt: String,
    pub conversation_title: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) key: Option<Cursor>,
}

impl BookmarkView {
    fn cursor(&self) -> Option<String> {
        self.key.as_ref().map(Cursor::encode)
    }
}

#[derive(Clone, Serialize, ToSchema)]
pub struct BookmarkPage {
    pub bookmarks: Vec<BookmarkView>,
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    at: DateTime<Utc>,
    id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!(
            "{}:{}",
            self.at.timestamp_micros(),
            self.id.simple()
        ))
    }

    pub fn decode(value: &str) -> Option<Self> {
        let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(value).ok()?).ok()?;
        let (at, id) = raw.split_once(':')?;
        Some(Self {
            at: DateTime::from_timestamp_micros(at.parse().ok()?)?,
            id: Uuid::parse_str(id).ok()?,
        })
    }
}

#[derive(Debug)]
pub enum BookmarkError {
    NotFound,
    InvalidKind,
    Invalid,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for BookmarkError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

const ENTRY_COLUMNS: &str = "select b.id,b.conversation_id,b.target_kind,b.target_id,b.created_at,
        coalesce(i.created_at,bl.created_at) target_created_at,
        coalesce(i.feed_seq,i.created_seq,bl.created_seq,0)::bigint seq,
        i.id input_id,bl.block_id,i.origin,bl.agent,
        c.title,c.harness,coalesce(nullif(bl.model,''),c.model) model,c.archived_at,
        case when b.target_kind='input' then i.content end input_content,
        case when b.target_kind='block' then
          case when bl.state='streaming'
            then coalesce((select string_agg(ch.text,'' order by ch.seq)
                           from block_chunks ch where ch.block_id=bl.block_id),'')
            else bl.content end
        end block_content";

const ENTRY_FROM: &str = " from bookmarks b
    join conversations c on c.id=b.conversation_id
    left join inputs i on b.target_kind='input' and i.id=b.target_id
         and i.conversation_id=b.conversation_id
    left join blocks bl on b.target_kind='block' and bl.block_id=b.target_id
         and bl.conversation_id=b.conversation_id
    where (i.id is not null or bl.block_id is not null)";

const ORPHANS: &str = "delete from bookmarks b
    where b.owner_id=$1
      and not exists (select 1 from inputs i
                       where b.target_kind='input' and i.id=b.target_id
                         and i.conversation_id=b.conversation_id)
      and not exists (select 1 from blocks bl
                       where b.target_kind='block' and bl.block_id=b.target_id
                         and bl.conversation_id=b.conversation_id)";

const ORPHANS_IN_CONVERSATION: &str = "delete from bookmarks b
    where b.owner_id=$1 and b.conversation_id=$2
      and not exists (select 1 from inputs i
                       where b.target_kind='input' and i.id=b.target_id
                         and i.conversation_id=b.conversation_id)
      and not exists (select 1 from blocks bl
                       where b.target_kind='block' and bl.block_id=b.target_id
                         and bl.conversation_id=b.conversation_id)";

fn view(row: sqlx::postgres::PgRow) -> Result<BookmarkView, BookmarkError> {
    let target_kind = TargetKind::parse(row.try_get::<String, _>("target_kind")?.as_str())
        .ok_or_else(|| sqlx::Error::Protocol("invalid stored bookmark kind".into()))?;
    let origin: Option<Value> = row.try_get("origin")?;
    let excerpt = match target_kind {
        TargetKind::Input => input_text(row.try_get::<Option<Value>, _>("input_content")?),
        TargetKind::Block => row
            .try_get::<Option<String>, _>("block_content")?
            .unwrap_or_default(),
    };
    let at: DateTime<Utc> = row.try_get("created_at")?;
    let seq: i64 = row.try_get("seq")?;
    Ok(BookmarkView {
        id: row.try_get("id")?,
        conversation_id: row.try_get("conversation_id")?,
        target_kind,
        target_id: row.try_get("target_id")?,
        created_at: time(at),
        target_created_at: time(row.try_get("target_created_at")?),
        kind: match target_kind {
            TargetKind::Input => HitKind::Message,
            TargetKind::Block => HitKind::Answer,
        },
        origin_kind: (target_kind == TargetKind::Input).then(|| origin_kind(origin.as_ref())),
        block_id: row.try_get("block_id")?,
        input_id: row.try_get("input_id")?,
        seq,
        jump_seq: seq,
        harness: row.try_get("harness")?,
        model: row.try_get("model")?,
        archived: row
            .try_get::<Option<DateTime<Utc>>, _>("archived_at")?
            .is_some(),
        role: match target_kind {
            TargetKind::Input => "owner".into(),
            TargetKind::Block => "agent".into(),
        },
        agent: row.try_get("agent")?,
        excerpt: snippet(&excerpt),
        conversation_title: row.try_get("title")?,
        key: Some(Cursor {
            at,
            id: row.try_get("id")?,
        }),
    })
}

fn origin_kind(origin: Option<&Value>) -> OriginKind {
    match origin
        .and_then(|origin| origin.get("kind"))
        .and_then(Value::as_str)
    {
        None => OriginKind::Owner,
        Some("message") => OriginKind::Message,
        Some("spawn") => OriginKind::Spawn,
        Some(_) => OriginKind::Result,
    }
}

fn input_text(content: Option<Value>) -> String {
    content
        .as_ref()
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Текст без разметки: пробелы и переводы строк склеены, длина ограничена
/// [`EXCERPT_CHARS`] символами.
pub fn snippet(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(EXCERPT_CHARS)
        .collect()
}

async fn target_exists(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    kind: TargetKind,
    target_id: Uuid,
) -> Result<bool, sqlx::Error> {
    match kind {
        TargetKind::Input => {
            sqlx::query_scalar(
                "select exists(select 1 from inputs where id=$1 and conversation_id=$2)",
            )
            .bind(target_id)
            .bind(conversation_id)
            .fetch_one(conn)
            .await
        }
        TargetKind::Block => {
            sqlx::query_scalar(
                "select exists(select 1 from blocks
                           where block_id=$1 and conversation_id=$2 and parent_block_id is null)",
            )
            .bind(target_id)
            .bind(conversation_id)
            .fetch_one(conn)
            .await
        }
    }
}

async fn entry(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    kind: TargetKind,
    target_id: Uuid,
) -> Result<BookmarkView, BookmarkError> {
    let sql = format!(
        "{ENTRY_COLUMNS}{ENTRY_FROM} and b.owner_id=$1 and b.conversation_id=$2
         and b.target_kind=$3 and b.target_id=$4"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(conversation_id)
        .bind(kind.as_str())
        .bind(target_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(BookmarkError::NotFound)?;
    view(row)
}

/// Добавляет закладку; повтор возвращает существующую и событие не публикует.
pub async fn add(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    kind: TargetKind,
    target_id: Uuid,
    correlation_id: Uuid,
) -> Result<(BookmarkView, bool), BookmarkError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let exists: Option<Uuid> = sqlx::query_scalar(
                "select id from conversations
                 where id=$1 and owner_id=$2 and trashed_at is null for update",
            )
            .bind(conversation_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            if exists.is_none() {
                return Err(BookmarkError::NotFound);
            }
            if !target_exists(conn, conversation_id, kind, target_id).await? {
                return Err(BookmarkError::NotFound);
            }
            let inserted: Option<Uuid> = sqlx::query_scalar(
                "insert into bookmarks(id,owner_id,conversation_id,target_kind,target_id)
                 values($1,$2,$3,$4,$5)
                 on conflict (conversation_id,target_kind,target_id) do nothing
                 returning id",
            )
            .bind(Uuid::now_v7())
            .bind(owner_id)
            .bind(conversation_id)
            .bind(kind.as_str())
            .bind(target_id)
            .fetch_optional(&mut *conn)
            .await?;
            let bookmark = entry(conn, owner_id, conversation_id, kind, target_id).await?;
            if inserted.is_some() {
                publish(conn, owner_id, &bookmark, true, correlation_id).await?;
            }
            Ok((bookmark, inserted.is_some()))
        })
    })
    .await
}

/// Удаляет закладку; отсутствие строки не ошибка и события не даёт.
pub async fn remove(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    kind: TargetKind,
    target_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), BookmarkError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let exists: Option<Uuid> = sqlx::query_scalar(
                "select id from conversations
                 where id=$1 and owner_id=$2 and trashed_at is null for update",
            )
            .bind(conversation_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            if exists.is_none() {
                return Err(BookmarkError::NotFound);
            }
            let removed: Option<Uuid> = sqlx::query_scalar(
                "delete from bookmarks
                 where owner_id=$1 and conversation_id=$2 and target_kind=$3 and target_id=$4
                 returning id",
            )
            .bind(owner_id)
            .bind(conversation_id)
            .bind(kind.as_str())
            .bind(target_id)
            .fetch_optional(&mut *conn)
            .await?;
            if removed.is_some() {
                let payload = serde_json::json!({
                    "conversation_id": conversation_id,
                    "target_kind": kind.as_str(),
                    "target_id": target_id,
                    "bookmarked": false,
                });
                publish_payload(conn, owner_id, conversation_id, payload, correlation_id).await?;
            }
            Ok(())
        })
    })
    .await
}

pub async fn list_conversation(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<Vec<BookmarkView>, BookmarkError> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let owned: bool = sqlx::query_scalar(
                "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
            )
            .bind(conversation_id)
            .bind(owner_id)
            .fetch_one(&mut *conn)
            .await?;
            if !owned {
                return Err(BookmarkError::NotFound);
            }
            sqlx::query(ORPHANS_IN_CONVERSATION)
                .bind(owner_id)
                .bind(conversation_id)
                .execute(&mut *conn)
                .await?;
            let sql = format!(
                "{ENTRY_COLUMNS}{ENTRY_FROM} and b.owner_id=$1 and b.conversation_id=$2
                 order by seq,b.created_at,b.id limit $3"
            );
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(owner_id)
                .bind(conversation_id)
                .bind(CONVERSATION_LIMIT as i64)
                .fetch_all(&mut *conn)
                .await?
                .into_iter()
                .map(view)
                .collect()
        })
    })
    .await
}

pub async fn list_owner(
    pool: &PgPool,
    owner_id: Uuid,
    limit: Option<i64>,
    cursor: Option<String>,
) -> Result<BookmarkPage, BookmarkError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(BookmarkError::Invalid);
    }
    let cursor = match cursor {
        Some(value) => Some(Cursor::decode(&value).ok_or(BookmarkError::Invalid)?),
        None => None,
    };
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query(ORPHANS)
                .bind(owner_id)
                .execute(&mut *conn)
                .await?;
            let sql = format!(
                "{ENTRY_COLUMNS}{ENTRY_FROM}
                 and b.owner_id=$1 and c.trashed_at is null
                 and ($2::timestamptz is null or b.created_at < $2
                      or (b.created_at = $2 and b.id > $3))
                 order by b.created_at desc,b.id limit $4"
            );
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(owner_id)
                .bind(cursor.map(|c| c.at))
                .bind(cursor.map(|c| c.id))
                .bind(limit + 1)
                .fetch_all(&mut *conn)
                .await?;
            let more = rows.len() as i64 > limit;
            let mut bookmarks = rows
                .into_iter()
                .take(limit as usize)
                .map(view)
                .collect::<Result<Vec<_>, _>>()?;
            let next_cursor = more
                .then(|| bookmarks.last().and_then(BookmarkView::cursor))
                .flatten();
            Ok(BookmarkPage {
                bookmarks: std::mem::take(&mut bookmarks),
                next_cursor,
            })
        })
    })
    .await
}

async fn publish(
    conn: &mut PgConnection,
    owner_id: Uuid,
    bookmark: &BookmarkView,
    bookmarked: bool,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let payload = serde_json::json!({
        "conversation_id": bookmark.conversation_id,
        "target_kind": bookmark.target_kind.as_str(),
        "target_id": bookmark.target_id,
        "bookmarked": bookmarked,
        "bookmark": bookmark,
    });
    publish_payload(
        conn,
        owner_id,
        bookmark.conversation_id,
        payload,
        correlation_id,
    )
    .await
}

async fn publish_payload(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    payload: Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversations { owner_id },
            kind: "bookmark_changed",
            conversation_id: Some(conversation_id),
            execution_id: None,
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

impl fmt::Debug for BookmarkView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BookmarkView")
            .field("id", &self.id)
            .field("conversation_id", &self.conversation_id)
            .field("target_kind", &self.target_kind)
            .field("target_id", &self.target_id)
            .finish_non_exhaustive()
    }
}
