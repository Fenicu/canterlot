//! Все картинки разговора в порядке ленты: вложения владельца, картинки блоков и
//! картинки-файлы. Просмотрщик PWA листает их одним списком, поэтому страница
//! отдаёт соседей вокруг места и курсоры в обе стороны.

use crate::conversations::time;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::sync::LazyLock;
use utoipa::ToSchema;
use uuid::Uuid;

/// Что за картинка: от этого зависит адрес выдачи файла.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageKind {
    InputAttachment,
    BlockMedia,
    BlockFile,
}

impl ImageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InputAttachment => "input_attachment",
            Self::BlockMedia => "block_media",
            Self::BlockFile => "block_file",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "input_attachment" => Some(Self::InputAttachment),
            "block_media" => Some(Self::BlockMedia),
            "block_file" => Some(Self::BlockFile),
            _ => None,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ImageItem {
    /// `<kind>:<uuid>`; тот же адрес принимает `around`.
    pub id: String,
    pub kind: ImageKind,
    /// Ссылка на `/api/v1/media/{id}` у картинки блока.
    pub media_id: Option<Uuid>,
    /// Ссылка на `/api/v1/attachments/{id}/content` у вложения и файла.
    pub attachment_id: Option<Uuid>,
    pub name: Option<String>,
    pub mime: String,
    pub size: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub created_at: String,
    /// Место в ленте: переход к сообщению или блоку.
    pub seq: String,
    pub input_id: Option<Uuid>,
    pub block_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ImagesPage {
    /// Старые первыми.
    pub items: Vec<ImageItem>,
    pub total: i64,
    /// Сколько картинок старше `items[0]`: по нему считается номер в просмотрщике.
    pub offset: i64,
    pub next_before: Option<String>,
    pub next_after: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub seq: i64,
    pub rank: i32,
    pub id: Uuid,
}

impl Key {
    fn text(self) -> String {
        format!("{}:{}:{}", self.seq, self.rank, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImagesCursor {
    Tail,
    Before(Key),
    After(Key),
    /// Окно вокруг картинки: её `id` в виде `<kind>:<uuid>`.
    Around(ImageKind, Uuid),
}

pub fn parse_key(text: &str) -> Option<Key> {
    let mut parts = text.split(':');
    let seq = parts.next()?.parse().ok()?;
    let rank = parts.next()?.parse().ok()?;
    let id = Uuid::parse_str(parts.next()?).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Key { seq, rank, id })
}

pub fn parse_item_id(text: &str) -> Option<(ImageKind, Uuid)> {
    let (kind, id) = text.split_once(':')?;
    Some((ImageKind::parse(kind)?, Uuid::parse_str(id).ok()?))
}

const IMAGES_SQL: &str = "with images as not materialized (
 (select 'input_attachment'::text kind, a.id sort_id, i.feed_seq sort_seq, i.feed_seq seq, ia.position rank,
    i.id input_id, null::uuid block_id, a.id attachment_id, null::uuid media_id,
    a.name, a.mime, a.size_bytes, null::integer width, null::integer height, i.created_at
   from inputs i
   join input_attachments ia on ia.input_id = i.id
   join attachments a on a.id = ia.attachment_id
  where i.conversation_id = $1 and i.feed_seq is not null and a.state = 'ready'
    and (a.mime in ('image/png','image/jpeg','image/gif','image/webp')
         or (a.mime = 'application/octet-stream'
             and lower(substring(a.name from '\\.([^.]+)$')) in ('png','jpg','jpeg','gif','webp'))))
 union all
 (select 'block_media', m.id, m.attached_seq, b.created_seq, 0, null, m.block_id, null, m.id,
    null, m.mime, m.size_bytes, m.width, m.height, b.created_at
   from block_media m
   join blocks b on b.block_id = m.block_id and b.conversation_id = m.conversation_id
  where m.conversation_id = $1 and m.attached_at is not null and m.attached_seq is not null)
 union all
 (select 'block_file', f.attachment_id, f.attached_seq, b.created_seq, 0, null, f.block_id, f.attachment_id, null,
    f.name, f.mime, f.size_bytes, f.width, f.height, b.created_at
   from block_files f
   join blocks b on b.block_id = f.block_id and b.conversation_id = f.conversation_id
   join attachments a on a.id = f.attachment_id and a.state = 'ready'
  where f.conversation_id = $1 and f.attached_at is not null and f.attached_seq is not null
    and (f.mime in ('image/png','image/jpeg','image/gif','image/webp')
         or (f.mime = 'application/octet-stream'
             and lower(substring(f.name from '\\.([^.]+)$')) in ('png','jpg','jpeg','gif','webp'))))
) ";

const ITEM_COLUMNS: &str = "kind,sort_id,sort_seq,seq,rank,input_id,block_id,attachment_id,media_id,name,mime,size_bytes,width,height,created_at";

static BACKWARD_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{IMAGES_SQL}select {ITEM_COLUMNS} from images
 where (not $2 or (sort_seq,rank,sort_id) < ($3,$4,$5) or ($6 and (sort_seq,rank,sort_id) = ($3,$4,$5)))
 order by sort_seq desc, rank desc, sort_id desc limit $7"
    )
});

static FORWARD_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{IMAGES_SQL}select {ITEM_COLUMNS} from images
 where (not $2 or (sort_seq,rank,sort_id) > ($3,$4,$5))
 order by sort_seq asc, rank asc, sort_id asc limit $6"
    )
});

static RESOLVE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!("{IMAGES_SQL}select sort_seq,rank,sort_id from images where kind=$2 and sort_id=$3")
});

static COUNT_BEFORE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!("{IMAGES_SQL}select count(*) from images where (sort_seq,rank,sort_id) < ($2,$3,$4)")
});

static TOTAL_SQL: LazyLock<String> =
    LazyLock::new(|| format!("{IMAGES_SQL}select count(*) from images"));

fn item(row: &sqlx::postgres::PgRow) -> Result<ImageItem, sqlx::Error> {
    let kind = ImageKind::parse(row.try_get("kind")?)
        .ok_or_else(|| sqlx::Error::Protocol("unknown image kind".into()))?;
    let sort_id: Uuid = row.try_get("sort_id")?;
    Ok(ImageItem {
        id: format!("{}:{sort_id}", kind.as_str()),
        kind,
        media_id: row.try_get("media_id")?,
        attachment_id: row.try_get("attachment_id")?,
        name: row.try_get("name")?,
        mime: row.try_get("mime")?,
        size: row.try_get("size_bytes")?,
        width: row.try_get("width")?,
        height: row.try_get("height")?,
        created_at: time(row.try_get("created_at")?),
        seq: row.try_get::<i64, _>("seq")?.to_string(),
        input_id: row.try_get("input_id")?,
        block_id: row.try_get("block_id")?,
    })
}

struct Slice {
    rows: Vec<sqlx::postgres::PgRow>,
    more: bool,
}

async fn backward(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    upper: Option<Key>,
    inclusive: bool,
    limit: i64,
) -> Result<Slice, sqlx::Error> {
    let (has_upper, up) = (
        upper.is_some(),
        upper.unwrap_or(Key {
            seq: 0,
            rank: 0,
            id: Uuid::nil(),
        }),
    );
    let mut rows = sqlx::query(BACKWARD_SQL.as_str())
        .bind(conversation_id)
        .bind(has_upper)
        .bind(up.seq)
        .bind(up.rank)
        .bind(up.id)
        .bind(inclusive)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    rows.reverse();
    Ok(Slice { rows, more })
}

async fn forward(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    lower: Option<Key>,
    limit: i64,
) -> Result<Slice, sqlx::Error> {
    let (has_lower, low) = (
        lower.is_some(),
        lower.unwrap_or(Key {
            seq: 0,
            rank: 0,
            id: Uuid::nil(),
        }),
    );
    let mut rows = sqlx::query(FORWARD_SQL.as_str())
        .bind(conversation_id)
        .bind(has_lower)
        .bind(low.seq)
        .bind(low.rank)
        .bind(low.id)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    Ok(Slice { rows, more })
}

async fn resolve(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    kind: ImageKind,
    id: Uuid,
) -> Result<Option<Key>, sqlx::Error> {
    let row = sqlx::query(RESOLVE_SQL.as_str())
        .bind(conversation_id)
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    row.map(|row| {
        Ok(Key {
            seq: row.try_get("sort_seq")?,
            rank: row.try_get("rank")?,
            id: row.try_get("sort_id")?,
        })
    })
    .transpose()
}

async fn count_before(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    key: Key,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(COUNT_BEFORE_SQL.as_str())
        .bind(conversation_id)
        .bind(key.seq)
        .bind(key.rank)
        .bind(key.id)
        .fetch_one(&mut *conn)
        .await
}

async fn total(conn: &mut PgConnection, conversation_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(TOTAL_SQL.as_str())
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await
}

fn key_of(row: &sqlx::postgres::PgRow) -> Result<Key, sqlx::Error> {
    Ok(Key {
        seq: row.try_get("sort_seq")?,
        rank: row.try_get("rank")?,
        id: row.try_get("sort_id")?,
    })
}

/// Страница картинок; `None` — разговора нет или он чужой.
pub async fn page(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    cursor: ImagesCursor,
    limit: i64,
) -> Result<Option<ImagesPage>, sqlx::Error> {
    if !(1..=200).contains(&limit) {
        return Err(sqlx::Error::InvalidArgument("invalid images limit".into()));
    }
    if !crate::history::owned(conn, owner_id, conversation_id).await? {
        return Ok(None);
    }
    let live = total(&mut *conn, conversation_id).await?;
    let (rows, more_before, more_after) = match cursor {
        ImagesCursor::Tail => {
            let slice = backward(&mut *conn, conversation_id, None, false, limit).await?;
            (slice.rows, slice.more, false)
        }
        ImagesCursor::Before(key) => {
            let slice = backward(&mut *conn, conversation_id, Some(key), false, limit).await?;
            (slice.rows, slice.more, true)
        }
        ImagesCursor::After(key) => {
            let slice = forward(&mut *conn, conversation_id, Some(key), limit).await?;
            (slice.rows, true, slice.more)
        }
        ImagesCursor::Around(kind, id) => {
            let Some(center) = resolve(&mut *conn, conversation_id, kind, id).await? else {
                return Ok(None);
            };
            let older_limit = if limit <= 2 { 1 } else { (limit / 4).max(1) };
            let newer_limit = (limit - older_limit).max(1);
            let older =
                backward(&mut *conn, conversation_id, Some(center), true, older_limit).await?;
            let newer = forward(&mut *conn, conversation_id, Some(center), newer_limit).await?;
            let mut rows = older.rows;
            rows.extend(newer.rows);
            (rows, older.more, newer.more)
        }
    };
    let offset = match rows.first() {
        Some(row) => count_before(&mut *conn, conversation_id, key_of(row)?).await?,
        None => 0,
    };
    let next_before = if more_before {
        rows.first()
            .map(|row| key_of(row).map(Key::text))
            .transpose()?
    } else {
        None
    };
    let next_after = if more_after {
        rows.last()
            .map(|row| key_of(row).map(Key::text))
            .transpose()?
    } else {
        None
    };
    let items = rows.iter().map(item).collect::<Result<Vec<_>, _>>()?;
    Ok(Some(ImagesPage {
        items,
        total: live,
        offset,
        next_before,
        next_after,
    }))
}
