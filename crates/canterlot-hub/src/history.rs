use std::collections::HashMap;

use crate::{blocks, conversations::time};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Row, postgres::PgRow};
use utoipa::ToSchema;
use uuid::Uuid;

pub const TIMELINE_KEYS_SQL: &str = "select * from (
 (select block_id id, 'block' item_kind, created_seq seq from blocks
  where conversation_id=$1 and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq desc limit $5)
 union all
 (select id, 'compaction' item_kind, created_seq seq from compactions
  where conversation_id=$1 and created_seq is not null and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq desc limit $5)
 union all
 (select id, 'input' item_kind, feed_seq seq from inputs
  where conversation_id=$1 and feed_seq is not null and (not $3 or feed_seq>$2) and (not $4 or feed_seq<$2)
  order by feed_seq desc limit $5)
 union all
 (select id, 'request' item_kind, created_seq seq from requests
  where conversation_id=$1 and created_seq is not null and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq desc limit $5)
) timeline order by seq desc limit $5";
const FORWARD_KEYS_SQL: &str = "select * from (
 (select block_id id, 'block' item_kind, created_seq seq from blocks
  where conversation_id=$1 and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq asc limit $5)
 union all
 (select id, 'compaction' item_kind, created_seq seq from compactions
  where conversation_id=$1 and created_seq is not null and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq asc limit $5)
 union all
 (select id, 'input' item_kind, feed_seq seq from inputs
  where conversation_id=$1 and feed_seq is not null and (not $3 or feed_seq>$2) and (not $4 or feed_seq<$2)
  order by feed_seq asc limit $5)
 union all
 (select id, 'request' item_kind, created_seq seq from requests
  where conversation_id=$1 and created_seq is not null and (not $3 or created_seq>$2) and (not $4 or created_seq<$2)
  order by created_seq asc limit $5)
) timeline order by seq asc limit $5";

#[derive(Serialize, ToSchema)]
pub struct InputView {
    pub id: Uuid,
    pub idempotency_key: String,
    pub content: Vec<InputContentBlock>,
    pub attachments: Vec<AttachmentView>,
    pub state: String,
    /// Ревизия текста и вложений: правка ожидающего ввода идёт сравнением-и-заменой.
    pub revision: i64,
    pub created_seq: String,
    pub feed_seq: Option<String>,
    pub created_at: String,
    pub retry_of: Option<Uuid>,
    /// От какой сессии пришёл ввод; у ввода владельца пусто.
    #[schema(value_type = Option<crate::session_tools::origin::InputOrigin>)]
    pub origin: Option<Value>,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct AttachmentView {
    pub attachment_id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub sha256: String,
    pub image: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    Text { text: String },
    Attachment(AttachmentView),
}
#[derive(Serialize, ToSchema)]
pub struct BlockView {
    pub id: Uuid,
    #[serde(rename = "block_kind")]
    pub kind: String,
    pub agent: String,
    pub model: String,
    pub parent_block_id: Option<Uuid>,
    pub input_command_id: Option<Uuid>,
    pub tool_name: Option<String>,
    pub summary: Option<String>,
    /// Set to `"cli_policy"` when the tool ran without an `approve` prompt
    /// while the effective mode was not `bypassPermissions`.
    pub approved_by: Option<String>,
    pub state: String,
    pub created_seq: String,
    pub created_at: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub line_count: i64,
    pub content: String,
    pub content_truncated: bool,
    pub content_bytes: i64,
    pub rev: i64,
    pub revision_gap: bool,
    pub incomplete_reason: Option<String>,
    /// Картинки, на которые смотрел агент; появляются после `block_media_attached`.
    pub media: Vec<crate::media::MediaView>,
    /// Файлы, которые агент отдал владельцу; появляются после `block_file_attached`.
    pub files: Vec<crate::media::FileView>,
}
/// Вопрос или разрешение на том месте ленты, где агент его задал: открытый запрос
/// стоит у поля ввода, решённый остаётся здесь и после перезагрузки.
#[derive(Serialize, ToSchema)]
pub struct RequestItemView {
    pub id: Uuid,
    pub request_kind: String,
    pub state: String,
    pub request_payload: Value,
    pub decision_payload: Option<Value>,
    pub created_seq: String,
    pub created_at: String,
}
#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineItem {
    Input(Box<InputView>),
    Block(Box<BlockView>),
    Request(Box<RequestItemView>),
    Compaction(Box<crate::compactions::CompactionView>),
}
#[derive(Serialize, ToSchema)]
pub struct HistoryPage {
    pub items: Vec<TimelineItem>,
    pub next_before: Option<String>,
    pub next_after: Option<String>,
}
/// Одно сообщение владельца для списка перехода по своим сообщениям.
#[derive(Serialize, ToSchema)]
pub struct OwnerInputView {
    pub id: Uuid,
    /// Место ввода в ленте (`feed_seq`): по нему клиент прыгает к сообщению.
    pub seq: String,
    pub created_at: String,
    /// Первая часть текста без разбора: строку списка выбирает клиент.
    pub preview: String,
    pub attachments_count: i64,
    /// От какой сессии или службы пришёл ввод; у ввода владельца пусто.
    #[schema(value_type = Option<crate::session_tools::origin::InputOrigin>)]
    pub origin: Option<Value>,
}
#[derive(Serialize, ToSchema)]
pub struct OwnerInputsPage {
    pub items: Vec<OwnerInputView>,
    pub next_before: Option<String>,
}
#[derive(Clone, Copy)]
pub enum HistoryCursor {
    Tail,
    Before(i64),
    After(i64),
    /// Окно вокруг места в ленте: цель и соседи с обеих сторон.
    Around(i64),
}

pub(crate) async fn owned(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from conversations where id=$1 and owner_id=$2)")
        .bind(id)
        .bind(owner)
        .fetch_one(conn)
        .await
}
pub(crate) fn input_view(r: PgRow) -> Result<InputView, sqlx::Error> {
    let content: Vec<InputContentBlock> = serde_json::from_value(r.try_get::<Value, _>("content")?)
        .map_err(|_| sqlx::Error::Protocol("invalid stored input content".into()))?;
    let attachments = content
        .iter()
        .filter_map(|b| match b {
            InputContentBlock::Attachment(a) => Some(a.clone()),
            _ => None,
        })
        .collect();
    Ok(InputView {
        id: r.try_get("id")?,
        idempotency_key: r.try_get("idempotency_key")?,
        content,
        attachments,
        state: r.try_get("state")?,
        revision: r.try_get::<Option<i64>, _>("revision")?.unwrap_or(1),
        created_seq: r.try_get::<i64, _>("created_seq")?.to_string(),
        feed_seq: r
            .try_get::<Option<i64>, _>("feed_seq")?
            .map(|n| n.to_string()),
        created_at: time(r.try_get("created_at")?),
        retry_of: r.try_get("retry_of")?,
        origin: r.try_get("origin")?,
    })
}
pub(crate) fn prefix(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
async fn request_views(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, RequestItemView>, sqlx::Error> {
    // Как в снимке: незавершённому запросу остановленного исполнения ответа уже не будет.
    let rows = sqlx::query(
        "select r.id,r.kind,case when r.state in ('open','decision_saved','decision_transmitting') and e.state in ('stopped','failed','owner_lost') then 'closed_without_decision' else r.state end state,r.request_payload,r.decision_payload,r.created_seq,r.created_at from requests r join executions e on e.id=r.execution_id where r.id=any($1)",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?;
    let mut views = HashMap::with_capacity(rows.len());
    for r in rows {
        let id: Uuid = r.try_get("id")?;
        views.insert(
            id,
            RequestItemView {
                id,
                request_kind: r.try_get("kind")?,
                state: r.try_get("state")?,
                request_payload: r.try_get("request_payload")?,
                decision_payload: r.try_get("decision_payload")?,
                created_seq: r.try_get::<i64, _>("created_seq")?.to_string(),
                created_at: time(r.try_get("created_at")?),
            },
        );
    }
    Ok(views)
}
/// Предел показываемого текста вызова и вывода инструмента; остальное читается по запросу.
const TOOL_PREVIEW_BYTES: usize = 4096;
/// Текст блока читается на стороне базы: у вывода инструмента наружу уходит только начало
/// (символов не меньше, чем байт в пределе), а размер и число строк считаются там же.
const BLOCK_VIEWS_SQL: &str = "select b.block_id,b.kind,b.agent,b.model,b.parent_block_id,b.input_command_id,
 b.tool_name,b.summary,b.approved_by,b.state,b.created_seq,b.created_at,b.finished_at,b.rev,b.revision_gap,b.incomplete_reason,
 octet_length(t.text)::bigint content_bytes,
 (length(t.text)-length(replace(t.text,E'\\n',''))+(t.text<>'' and right(t.text,1)<>E'\\n')::int)::bigint line_count,
 case when b.kind in ('tool_call','tool_result') then left(t.text,$2) else t.text end content
 from blocks b cross join lateral (
  select case when b.state='streaming'
   then coalesce((select string_agg(c.text,'' order by c.seq) from block_chunks c where c.block_id=b.block_id),'')
   else b.content end text offset 0) t
 where b.block_id=any($1)";
async fn block_views(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, BlockView>, sqlx::Error> {
    let rows = sqlx::query(BLOCK_VIEWS_SQL)
        .bind(ids)
        .bind(TOOL_PREVIEW_BYTES as i32)
        .fetch_all(&mut *conn)
        .await?;
    let mut media = crate::media::for_blocks(conn, ids).await?;
    let mut files = crate::media::files_for_blocks(conn, ids).await?;
    let mut views = HashMap::with_capacity(rows.len());
    for r in rows {
        let id: Uuid = r.try_get("block_id")?;
        let kind: String = r.try_get("kind")?;
        let content: String = r.try_get("content")?;
        let content_bytes: i64 = r.try_get("content_bytes")?;
        let content = if matches!(kind.as_str(), "tool_call" | "tool_result") {
            prefix(&content, TOOL_PREVIEW_BYTES).to_owned()
        } else {
            content
        };
        let at = time(r.try_get("created_at")?);
        views.insert(
            id,
            BlockView {
                id,
                kind,
                agent: r.try_get("agent")?,
                model: r.try_get("model")?,
                parent_block_id: r.try_get("parent_block_id")?,
                input_command_id: r.try_get("input_command_id")?,
                tool_name: r.try_get("tool_name")?,
                summary: r.try_get("summary")?,
                approved_by: r.try_get("approved_by")?,
                state: r.try_get("state")?,
                created_seq: r.try_get::<i64, _>("created_seq")?.to_string(),
                created_at: at.clone(),
                started_at: at,
                finished_at: r
                    .try_get::<Option<DateTime<Utc>>, _>("finished_at")?
                    .map(time),
                line_count: r.try_get("line_count")?,
                content_truncated: (content.len() as i64) < content_bytes,
                content,
                content_bytes,
                rev: r.try_get("rev")?,
                revision_gap: r.try_get("revision_gap")?,
                incomplete_reason: r.try_get("incomplete_reason")?,
                media: media.remove(&id).unwrap_or_default(),
                files: files.remove(&id).unwrap_or_default(),
            },
        );
    }
    Ok(views)
}
fn missing(kind: &str) -> sqlx::Error {
    sqlx::Error::Protocol(format!("timeline {kind} disappeared"))
}
pub async fn page(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    cursor: HistoryCursor,
    limit: i64,
) -> Result<Option<HistoryPage>, sqlx::Error> {
    if !(1..=200).contains(&limit) {
        return Err(sqlx::Error::InvalidArgument("invalid history limit".into()));
    }
    if !owned(conn, owner_id, conversation_id).await? {
        return Ok(None);
    }
    if let HistoryCursor::Around(center) = cursor {
        return around(conn, owner_id, conversation_id, center, limit).await;
    }
    let (key, after, before) = match cursor {
        HistoryCursor::Tail => (0, false, false),
        HistoryCursor::Before(n) => (n, false, true),
        HistoryCursor::After(n) => (n, true, false),
        HistoryCursor::Around(_) => unreachable!("handled above"),
    };
    let sql = if after {
        FORWARD_KEYS_SQL
    } else {
        TIMELINE_KEYS_SQL
    };
    let mut keys = sqlx::query(sql)
        .bind(conversation_id)
        .bind(key)
        .bind(after)
        .bind(before)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
    let more = keys.len() > limit as usize;
    keys.truncate(limit as usize);
    if !after {
        keys.reverse();
    }
    let first = keys.first().map(|r| r.get::<i64, _>("seq"));
    let last = keys.last().map(|r| r.get::<i64, _>("seq"));
    let mut wanted: HashMap<&str, Vec<Uuid>> = HashMap::new();
    for k in &keys {
        wanted
            .entry(k.try_get("item_kind")?)
            .or_default()
            .push(k.try_get("id")?);
    }
    let ids = |kind: &str| wanted.get(kind).map(Vec::as_slice).unwrap_or_default();
    let mut blocks = block_views(conn, ids("block")).await?;
    let mut inputs = HashMap::new();
    if !ids("input").is_empty() {
        for r in sqlx::query("select * from inputs where id=any($1)")
            .bind(ids("input"))
            .fetch_all(&mut *conn)
            .await?
        {
            let view = input_view(r)?;
            inputs.insert(view.id, view);
        }
    }
    let mut requests = if ids("request").is_empty() {
        HashMap::new()
    } else {
        request_views(conn, ids("request")).await?
    };
    let mut compactions = if ids("compaction").is_empty() {
        HashMap::new()
    } else {
        crate::compactions::compaction_views(conn, ids("compaction")).await?
    };
    let mut items = Vec::with_capacity(keys.len());
    for k in &keys {
        let id: Uuid = k.try_get("id")?;
        items.push(match k.try_get::<&str, _>("item_kind")? {
            "input" => TimelineItem::Input(Box::new(
                inputs.remove(&id).ok_or_else(|| missing("input"))?,
            )),
            "request" => TimelineItem::Request(Box::new(
                requests.remove(&id).ok_or_else(|| missing("request"))?,
            )),
            "compaction" => TimelineItem::Compaction(Box::new(
                compactions
                    .remove(&id)
                    .ok_or_else(|| missing("compaction"))?,
            )),
            _ => TimelineItem::Block(Box::new(
                blocks.remove(&id).ok_or_else(|| missing("block"))?,
            )),
        });
    }
    Ok(Some(HistoryPage {
        items,
        next_before: if more && !after {
            first.map(|n| n.to_string())
        } else {
            None
        },
        next_after: if more && after {
            last.map(|n| n.to_string())
        } else {
            None
        },
    }))
}
/// Окно вокруг одного места ленты: немного старше цели и остальное новее.
/// Прыжок в старое сообщение подгружает только этот кусок, а не всю историю.
async fn around(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    center: i64,
    limit: i64,
) -> Result<Option<HistoryPage>, sqlx::Error> {
    let older_limit = if limit <= 2 { 1 } else { (limit / 4).max(1) };
    let newer_limit = (limit - older_limit).max(1);
    let before = Box::pin(page(
        conn,
        owner_id,
        conversation_id,
        HistoryCursor::Before(center.saturating_add(1)),
        older_limit,
    ))
    .await?;
    let Some(before) = before else {
        return Ok(None);
    };
    let after = Box::pin(page(
        conn,
        owner_id,
        conversation_id,
        HistoryCursor::After(center),
        newer_limit,
    ))
    .await?;
    let Some(after) = after else {
        return Ok(None);
    };
    let mut items = before.items;
    items.extend(after.items);
    Ok(Some(HistoryPage {
        items,
        next_before: before.next_before,
        next_after: after.next_after,
    }))
}
/// Сколько символов текста попадает в превью списка сообщений.
const PREVIEW_CHARS: usize = 200;
fn preview(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
/// Доставленные вводы разговора от имени владельца, страницами от новых к старым.
/// Ожидающие доставки вводы (`feed_seq is null`) в список не попадают.
pub async fn owner_inputs(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    before: Option<i64>,
    limit: i64,
) -> Result<Option<OwnerInputsPage>, sqlx::Error> {
    if !(1..=2000).contains(&limit) {
        return Err(sqlx::Error::InvalidArgument("invalid inputs limit".into()));
    }
    if !owned(conn, owner_id, conversation_id).await? {
        return Ok(None);
    }
    let mut rows = sqlx::query(
        "select i.id,i.feed_seq,i.created_at,i.origin,
         coalesce((select string_agg(b->>'text','' order by ordinality)
                   from jsonb_array_elements(i.content) with ordinality as e(b,ordinality)
                   where b->>'type'='text'),'') preview,
         (select count(*) from jsonb_array_elements(i.content) as e(b) where b->>'type'='attachment') attachments_count
         from inputs i join conversations c on c.id=i.conversation_id
         where i.conversation_id=$1 and c.owner_id=$2 and i.feed_seq is not null
           and (not $3 or i.feed_seq<$4)
         order by i.feed_seq desc limit $5",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .bind(before.is_some())
    .bind(before.unwrap_or_default())
    .bind(limit + 1)
    .fetch_all(&mut *conn)
    .await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    rows.reverse();
    let next_before = if more {
        rows.first()
            .map(|row| row.get::<i64, _>("feed_seq").to_string())
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|row| {
            let text: String = row.try_get("preview")?;
            Ok(OwnerInputView {
                id: row.try_get("id")?,
                seq: row.try_get::<i64, _>("feed_seq")?.to_string(),
                created_at: time(row.try_get("created_at")?),
                preview: preview(&text, PREVIEW_CHARS),
                attachments_count: row.try_get("attachments_count")?,
                origin: row.try_get("origin")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Some(OwnerInputsPage { items, next_before }))
}
#[derive(Serialize, ToSchema)]
pub struct BlockPage {
    pub content: String,
    pub offset: i64,
    pub next_offset: Option<i64>,
    pub total_bytes: i64,
    pub state: String,
    pub rev: i64,
    pub checksum: Option<String>,
}
#[derive(thiserror::Error)]
pub enum HistoryError {
    #[error("invalid offset")]
    InvalidOffset,
    #[error("invalid limit")]
    InvalidLimit,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}
impl std::fmt::Debug for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
pub async fn block_content(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    block_id: Uuid,
    offset: i64,
    limit: i64,
) -> Result<Option<BlockPage>, HistoryError> {
    let r=sqlx::query("select b.state,b.rev,b.checksum from blocks b join conversations c on c.id=b.conversation_id where b.block_id=$1 and c.id=$2 and c.owner_id=$3").bind(block_id).bind(conversation_id).bind(owner_id).fetch_optional(&mut *conn).await?;
    let Some(r) = r else { return Ok(None) };
    if !(1..=262144).contains(&limit) {
        return Err(HistoryError::InvalidLimit);
    }
    let text = blocks::read_text(conn, block_id).await?;
    let offset = usize::try_from(offset)
        .ok()
        .filter(|n| *n <= text.len() && text.is_char_boundary(*n))
        .ok_or(HistoryError::InvalidOffset)?;
    let rest = &text[offset..];
    let mut cut = prefix(rest, limit as usize);
    // Малый лимит всё равно возвращает хотя бы один целый символ.
    if cut.is_empty() && !rest.is_empty() {
        cut = &rest[..rest.chars().next().expect("nonempty text").len_utf8()];
    }
    let end = offset + cut.len();
    Ok(Some(BlockPage {
        content: cut.into(),
        offset: offset as i64,
        next_offset: (end < text.len()).then_some(end as i64),
        total_bytes: text.len() as i64,
        state: r.try_get("state")?,
        rev: r.try_get("rev")?,
        checksum: r.try_get("checksum")?,
    }))
}
#[derive(Serialize, ToSchema)]
pub struct ContextInfo {
    pub tokens: i64,
    pub window_tokens: Option<i64>,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct ModelUsageView {
    pub model: String,
    pub agent: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}
#[derive(Serialize, ToSchema)]
pub struct RunLimit {
    pub provider: String,
    pub weekly_delta_permille: i32,
    pub exact: bool,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct PartView {
    pub block_id: Uuid,
    pub agent: String,
    pub model: String,
    pub kind: String,
}
#[derive(Serialize, ToSchema)]
pub struct RunInfo {
    pub run_id: Uuid,
    pub input_id: Uuid,
    #[serde(with = "crate::model::utc_time")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::model::optional_utc_time")]
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub context: Option<ContextInfo>,
    pub usage: Vec<ModelUsageView>,
    pub limit: Option<RunLimit>,
    pub parts: Vec<PartView>,
    pub compactions: Option<u32>,
    /// Скорость ответа по моделям основного агента; пусто, если её нельзя выделить честно.
    pub speed: Vec<crate::speed::ModelSpeed>,
}
pub async fn run_info(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    run_id: Uuid,
) -> Result<Option<RunInfo>, sqlx::Error> {
    let r=sqlx::query("select r.*,i.id input_id from runs r join conversations c on c.id=r.conversation_id join inputs i on i.command_id=r.input_command_id and i.conversation_id=c.id where c.owner_id=$1 and c.id=$2 and r.input_command_id=$3").bind(owner_id).bind(conversation_id).bind(run_id).fetch_optional(&mut *conn).await?;
    let Some(r) = r else { return Ok(None) };
    let before: Option<i32> = r.try_get("weekly_before_permille")?;
    let after: Option<i32> = r.try_get("weekly_after_permille")?;
    let provider: Option<String> = r.try_get("provider")?;
    let limit = provider
        .zip(before.zip(after))
        .filter(|(_, (b, a))| a >= b)
        .map(|(provider, (b, a))| RunLimit {
            provider,
            weekly_delta_permille: a - b,
            exact: r.get::<bool, _>("exact")
                && !r.get::<bool, _>("overlapped")
                && a > b
                && (a % 10 != 0 || b % 10 != 0),
        });
    let usage=sqlx::query_as("select model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens from run_model_usage where input_command_id=$1 order by model,agent").bind(run_id).fetch_all(&mut *conn).await?;
    let parts=sqlx::query_as("select block_id,agent,model,kind from blocks where conversation_id=$1 and input_command_id=$2 order by created_seq,block_id").bind(conversation_id).bind(run_id).fetch_all(&mut *conn).await?;
    let speed = crate::speed::for_runs(conn, &[run_id])
        .await?
        .remove(&run_id)
        .unwrap_or_default();
    Ok(Some(RunInfo {
        run_id,
        input_id: r.try_get("input_id")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
        duration_ms: r.try_get("duration_ms")?,
        context: r
            .try_get::<Option<i64>, _>("context_tokens")?
            .map(|tokens| ContextInfo {
                tokens,
                window_tokens: r.get("context_window_tokens"),
            }),
        usage,
        limit,
        parts,
        compactions: None,
        speed,
    }))
}
/// Страница ленты одного субагента: корень (вызов) и его потомки по любой глубине.
#[derive(Serialize, ToSchema)]
pub struct LanePage {
    pub root: BlockView,
    /// Потомки корня по возрастанию `created_seq`; сам корень сюда не входит.
    pub items: Vec<BlockView>,
    pub next_before: Option<String>,
}
const LANE_KEYS_SQL: &str = "with recursive lane(block_id, created_seq) as (
 select block_id, created_seq from blocks where conversation_id=$1 and parent_block_id=$2
 union
 select b.block_id, b.created_seq from blocks b join lane l on b.parent_block_id=l.block_id where b.conversation_id=$1
)
select block_id, created_seq from lane
 where block_id<>$2 and (not $3 or created_seq<$4)
 order by created_seq desc limit $5";
/// Один субагент целиком, страницами от нового к старому: лента разговора держит только хвост,
/// а старые ветки в ней не лежат.
pub async fn lane(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    root_id: Uuid,
    before: Option<i64>,
    limit: i64,
) -> Result<Option<LanePage>, sqlx::Error> {
    if !(1..=200).contains(&limit) {
        return Err(sqlx::Error::InvalidArgument("invalid lane limit".into()));
    }
    if !owned(conn, owner_id, conversation_id).await? {
        return Ok(None);
    }
    let known: bool = sqlx::query_scalar(
        "select exists(select 1 from blocks where block_id=$1 and conversation_id=$2)",
    )
    .bind(root_id)
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if !known {
        return Ok(None);
    }
    let mut keys = sqlx::query(LANE_KEYS_SQL)
        .bind(conversation_id)
        .bind(root_id)
        .bind(before.is_some())
        .bind(before.unwrap_or_default())
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await?;
    let more = keys.len() > limit as usize;
    keys.truncate(limit as usize);
    keys.reverse();
    let first = keys.first().map(|r| r.get::<i64, _>("created_seq"));
    let mut ids = keys
        .iter()
        .map(|key| key.try_get("block_id"))
        .collect::<Result<Vec<Uuid>, _>>()?;
    ids.push(root_id);
    let mut views = block_views(conn, &ids).await?;
    let root = views.remove(&root_id).ok_or_else(|| missing("block"))?;
    ids.pop();
    let items = ids
        .iter()
        .map(|id| views.remove(id).ok_or_else(|| missing("block")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(LanePage {
        root,
        items,
        next_before: if more {
            first.map(|n| n.to_string())
        } else {
            None
        },
    }))
}
#[derive(Serialize, ToSchema)]
pub struct SubagentRef {
    pub name: String,
    pub model: String,
}
#[derive(Serialize, ToSchema)]
pub struct DelegationNode {
    pub block_id: Uuid,
    pub parent_block_id: Option<Uuid>,
    pub subagent: Option<SubagentRef>,
    pub tool_name: Option<String>,
    pub task: Option<String>,
    pub state: String,
    #[serde(with = "crate::model::utc_time")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::model::optional_utc_time")]
    pub finished_at: Option<DateTime<Utc>>,
}
/// Вызов субагента финален сразу после записи входа: его конец говорит только о том, что вызов
/// сделан. Состояние даёт фоновая задача, а без неё настоящий результат вызова.
fn subagent_state(
    r: &sqlx::postgres::PgRow,
    call: &'static str,
    finished_at: Option<DateTime<Utc>>,
) -> Result<(&'static str, Option<DateTime<Utc>>), sqlx::Error> {
    let live: bool = r.try_get("live")?;
    if let Some(task) = r.try_get::<Option<&str>, _>("task_state")? {
        let state = match task {
            "running" if live => return Ok(("running", None)),
            "completed" => "done",
            "failed" => "failed",
            _ => "stopped",
        };
        let ended = r.try_get::<Option<DateTime<Utc>>, _>("task_ended_at")?;
        return Ok((state, ended.or(finished_at)));
    }
    if let Some(result) = r.try_get::<Option<&str>, _>("result_state")? {
        let summary = r.try_get::<Option<&str>, _>("result_summary")?;
        let state = match (result, summary) {
            ("streaming", _) => return Ok(("running", None)),
            ("final", Some("error")) => "failed",
            ("final", Some("interrupted")) => "stopped",
            ("final", _) => "done",
            _ => "incomplete",
        };
        let ended = r.try_get::<Option<DateTime<Utc>>, _>("result_finished_at")?;
        return Ok((state, ended.or(finished_at)));
    }
    Ok(match call {
        "incomplete" => ("incomplete", finished_at),
        _ if live => ("running", None),
        _ => ("incomplete", finished_at),
    })
}

pub async fn delegation(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<Option<Vec<DelegationNode>>, sqlx::Error> {
    if !owned(conn, owner_id, conversation_id).await? {
        return Ok(None);
    }
    let rows=sqlx::query("select b.*,child.agent child_agent,child.model child_model,
            c.harness harness,
            b.execution_id=live.id is true live,
            t.state task_state,t.ended_at task_ended_at,
            result.state result_state,result.summary result_summary,result.finished_at result_finished_at
        from blocks b
        join conversations c on c.id=b.conversation_id
        left join lateral (select agent,model from blocks c where c.parent_block_id=b.block_id and c.conversation_id=b.conversation_id and c.agent<>b.agent order by c.created_seq,c.block_id limit 1) child on true
        left join lateral (select e.id from executions e where e.conversation_id=b.conversation_id and e.state in ('created','starting','running','stopping') limit 1) live on true
        left join background_tasks t on t.conversation_id=b.conversation_id and t.block_id=b.block_id
        left join lateral (select r.state,r.summary,r.finished_at from blocks r where r.parent_block_id=b.block_id and r.conversation_id=b.conversation_id and r.kind='tool_result' and r.summary is distinct from 'async_launched' order by r.created_seq desc,r.block_id limit 1) result on true
        where b.conversation_id=$1 and b.kind='tool_call' and (lower(b.tool_name) in ('agent','task','spawn_agent','delegate') or exists(select 1 from blocks c where c.parent_block_id=b.block_id and c.conversation_id=b.conversation_id and c.agent<>b.agent)) order by b.created_seq,b.block_id").bind(conversation_id).fetch_all(conn).await?;
    rows.into_iter()
        .map(|r| {
            let tool_name: Option<String> = r.try_get("tool_name")?;
            let call = match r.try_get::<&str, _>("state")? {
                "streaming" => "running",
                "final" => "done",
                _ => "incomplete",
            };
            let finished_at: Option<DateTime<Utc>> = r.try_get("finished_at")?;
            let name = tool_name.as_deref().unwrap_or_default().to_lowercase();
            let subagent_call = matches!(
                (r.try_get::<&str, _>("harness")?, name.as_str()),
                ("claude_code", "agent" | "task") | ("codex", "spawn_agent")
            );
            let (state, finished_at) = if subagent_call {
                subagent_state(&r, call, finished_at)?
            } else {
                (call, finished_at)
            };
            Ok(DelegationNode {
                block_id: r.try_get("block_id")?,
                parent_block_id: r.try_get("parent_block_id")?,
                subagent: r
                    .try_get::<Option<String>, _>("child_agent")?
                    .map(|name| SubagentRef {
                        name,
                        model: r.get("child_model"),
                    }),
                tool_name,
                task: r.try_get("summary")?,
                state: state.into(),
                started_at: r.try_get("created_at")?,
                finished_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}
