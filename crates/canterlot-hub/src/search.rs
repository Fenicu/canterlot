//! Полнотекстовый поиск по истории: ответы агента (`blocks` вида `text`) и сообщения ленты
//! (`inputs` с `feed_seq`). Общее ядро для `POST /api/v1/search` и инструмента
//! `history.search`; область видимости задаёт вызывающий.
//!
//! Запрос и фрагменты — данные уровня сообщения: ни в журнал, ни в `Debug`, ни в метрики.
//! Порядок по времени, а не по рангу: индекс GIN отвечает только «совпало», ранг требовал бы
//! разбирать каждый совпавший документ заново. Фрагмент `ts_headline` строится только для строк
//! страницы. Документ и частичные предикаты в запросе совпадают с индексами миграции 0075
//! буква в букву, иначе планировщик их не возьмёт.

use std::{sync::LazyLock, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row};
use tokio::sync::Semaphore;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::conversations::time;

/// Длина запроса в символах.
pub const MAX_QUERY_CHARS: usize = 200;
pub const DEFAULT_LIMIT: i64 = 20;
pub const MAX_LIMIT: i64 = 50;
/// Сколько совпадений считается точно; больше — «больше 200».
pub const TOTAL_CAP: i64 = 200;
/// Длина фрагмента в символах после склейки пробелов.
pub const SNIPPET_MAX_CHARS: usize = 600;
/// Срок одного поиска в базе и в очереди за слотом.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Поиск дороже любого другого чтения: одновременно не больше двух на процесс, HTTP и MCP
/// вместе.
static SLOTS: Semaphore = Semaphore::const_new(2);

/// Отметки начала и конца совпадения от `ts_headline`. Из исходного текста их вырезают
/// заранее, поэтому чужой текст сдвинуть разметку не может.
const START: char = '\u{E000}';
const STOP: char = '\u{E001}';

/// Что искать: ответы агента, сообщения ленты или (без поля) и то и другое.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Answers,
    Messages,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Answer,
    Message,
}

impl HitKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Message => "message",
        }
    }
}

/// Кто написал найденное сообщение: владелец, другая сессия, бриф запуска или итог делегата.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Owner,
    Message,
    Spawn,
    Result,
}

// Текст — данные уровня сообщения, поэтому `Debug` у фрагмента и совпадений нет.
/// Кусок фрагмента; `hit` — совпавшее слово, клиент его подсвечивает. HTML сервер не шлёт.
#[derive(Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SnippetSegment {
    pub text: String,
    pub hit: bool,
}

/// Позиция в порядке `created_at desc, kind, id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    at: DateTime<Utc>,
    kind: HitKind,
    id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        let kind = match self.kind {
            HitKind::Answer => 'a',
            HitKind::Message => 'm',
        };
        URL_SAFE_NO_PAD.encode(format!(
            "{}:{kind}:{}",
            self.at.timestamp_micros(),
            self.id.simple()
        ))
    }

    pub fn decode(value: &str) -> Option<Self> {
        let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(value).ok()?).ok()?;
        let mut parts = raw.splitn(3, ':');
        let at = DateTime::from_timestamp_micros(parts.next()?.parse().ok()?)?;
        let kind = match parts.next()? {
            "a" => HitKind::Answer,
            "m" => HitKind::Message,
            _ => return None,
        };
        let id = Uuid::parse_str(parts.next()?).ok()?;
        Some(Self { at, kind, id })
    }
}

/// Найденный ответ или сообщение. `seq` — место в ленте (`created_seq` блока, `feed_seq`
/// ввода); `jump_seq` — место верхнего предка вложенного блока субагента, иначе `seq`.
#[derive(Clone, Serialize, ToSchema)]
pub struct Hit {
    pub conversation_id: Uuid,
    pub conversation_title: String,
    pub harness: String,
    pub model: String,
    /// Сессия в архиве (или принятый делегат до уборки).
    pub archived: bool,
    pub kind: HitKind,
    #[schema(required = true)]
    pub origin_kind: Option<OriginKind>,
    #[schema(required = true)]
    pub block_id: Option<Uuid>,
    #[schema(required = true)]
    pub input_id: Option<Uuid>,
    pub seq: i64,
    pub jump_seq: i64,
    pub created_at: String,
    pub snippet: Vec<SnippetSegment>,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) key: Option<Cursor>,
}

impl Hit {
    /// Курсор продолжения сразу после этого совпадения.
    pub fn cursor(&self) -> Option<String> {
        self.key.as_ref().map(Cursor::encode)
    }
}

#[derive(Clone, Serialize, ToSchema)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Число совпадений, не больше 200.
    pub total: i64,
    /// Совпадений больше, чем `total`.
    pub total_capped: bool,
    /// В запросе нет ни одного искомого слова (одни служебные слова или только исключения).
    #[serde(skip)]
    #[schema(ignore)]
    pub no_terms: bool,
}

impl SearchPage {
    fn empty(no_terms: bool) -> Self {
        Self {
            hits: Vec::new(),
            next_cursor: None,
            total: 0,
            total_capped: false,
            no_terms,
        }
    }
}

/// Проверенный запрос. Текст в `Debug` не попадает.
pub struct SearchQuery {
    pub owner_id: Uuid,
    pub text: String,
    /// Семья делегированной сессии; `None` — все сессии владельца.
    pub family: Option<Vec<Uuid>>,
    pub conversation_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub harness: Option<String>,
    pub kind: Option<SearchKind>,
    pub cursor: Option<Cursor>,
    pub limit: i64,
}

impl std::fmt::Debug for SearchQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchQuery")
            .field("query_chars", &self.text.chars().count())
            .field("family", &self.family.as_ref().map(Vec::len))
            .field("conversation_id", &self.conversation_id)
            .field("kind", &self.kind)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum SearchError {
    /// Пустой или слишком длинный запрос, неверный курсор, размер страницы или период.
    Invalid,
    /// Срок вышел: в очереди за слотом или в базе.
    Timeout,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for SearchError {
    fn from(error: sqlx::Error) -> Self {
        let canceled =
            matches!(&error, sqlx::Error::Database(db) if db.code().as_deref() == Some("57014"));
        if canceled {
            Self::Timeout
        } else {
            Self::Db(error)
        }
    }
}

/// Поля запроса до проверки: общая часть тела HTTP и аргументов MCP.
pub struct RawQuery {
    pub text: String,
    pub conversation_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub harness: Option<String>,
    pub kind: Option<SearchKind>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl RawQuery {
    pub fn check(
        self,
        owner_id: Uuid,
        family: Option<Vec<Uuid>>,
    ) -> Result<SearchQuery, SearchError> {
        let text = self.text.trim();
        let chars = text.chars().count();
        if chars == 0 || chars > MAX_QUERY_CHARS || text.contains('\0') {
            return Err(SearchError::Invalid);
        }
        let limit = self.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(SearchError::Invalid);
        }
        if let (Some(since), Some(until)) = (self.since, self.until)
            && since >= until
        {
            return Err(SearchError::Invalid);
        }
        let harness = match self.harness {
            Some(harness) if harness.is_empty() || harness.len() > 64 || harness.contains('\0') => {
                return Err(SearchError::Invalid);
            }
            harness => harness,
        };
        let cursor = match self.cursor {
            Some(cursor) => Some(Cursor::decode(&cursor).ok_or(SearchError::Invalid)?),
            None => None,
        };
        Ok(SearchQuery {
            owner_id,
            text: text.to_owned(),
            family,
            conversation_id: self.conversation_id,
            since: self.since,
            until: self.until,
            harness,
            kind: self.kind,
            cursor,
            limit,
        })
    }
}

/// Условия одной ветви без курсора. `$1` владелец, `$2` запрос, `$3` семья, `$4` сессия,
/// `$5`/`$6` период, `$7` харнесс, `$8`/`$9` нужны ли ответы и сообщения.
const BLOCK_WHERE: &str = "b.kind = 'text' and b.state <> 'streaming'
   and canterlot_block_doc(b.content) @@ websearch_to_tsquery('pg_catalog.russian', $2)
   and c.owner_id = $1 and c.trashed_at is null
   and ($3::uuid[] is null or b.conversation_id = any($3::uuid[]))
   and ($4::uuid is null or b.conversation_id = $4::uuid)
   and ($5::timestamptz is null or b.created_at >= $5::timestamptz)
   and ($6::timestamptz is null or b.created_at < $6::timestamptz)
   and ($7::text is null or c.harness = $7::text)
   and $8::boolean";

/// Сообщения ленты: владелец, другая сессия, бриф запуска и итог делегата. Будильники,
/// автопинок, служебные сообщения хаба и вычищенные события удалённого отправителя — нет.
const INPUT_WHERE: &str = "i.feed_seq is not null
   and canterlot_input_doc(i.content) @@ websearch_to_tsquery('pg_catalog.russian', $2)
   and (i.origin is null or i.origin->>'kind' in ('message','spawn')
        or (i.origin->>'kind' = 'event' and i.origin->>'from' is not null
            and i.origin->'sender_deleted' is distinct from 'true'::jsonb))
   and c.owner_id = $1 and c.trashed_at is null
   and ($3::uuid[] is null or i.conversation_id = any($3::uuid[]))
   and ($4::uuid is null or i.conversation_id = $4::uuid)
   and ($5::timestamptz is null or i.created_at >= $5::timestamptz)
   and ($6::timestamptz is null or i.created_at < $6::timestamptz)
   and ($7::text is null or c.harness = $7::text)
   and $9::boolean";

/// Текст ввода для фрагмента: та же выборка, что в `canterlot_input_doc`.
const INPUT_TEXT: &str = "array_to_string(array(select jsonb_array_elements_text(jsonb_path_query_array(i.content, 'lax $[*] ? (@.type == \"text\").text'))), E'\\n')";

/// Страница: `$10`–`$12` курсор, `$13` строк с запасом в одну.
pub static PAGE_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "with page as (
           select * from (
             (select 'answer'::text kind, b.block_id id, b.conversation_id, b.created_at
              from blocks b join conversations c on c.id = b.conversation_id
              where {BLOCK_WHERE}
                and ($10::timestamptz is null or b.created_at < $10::timestamptz
                     or (b.created_at = $10::timestamptz
                         and ('answer' > $11::text or ('answer' = $11::text and b.block_id > $12::uuid))))
              order by b.created_at desc, b.block_id limit $13)
             union all
             (select 'message'::text kind, i.id, i.conversation_id, i.created_at
              from inputs i join conversations c on c.id = i.conversation_id
              where {INPUT_WHERE}
                and ($10::timestamptz is null or i.created_at < $10::timestamptz
                     or (i.created_at = $10::timestamptz
                         and ('message' > $11::text or ('message' = $11::text and i.id > $12::uuid))))
              order by i.created_at desc, i.id limit $13)
           ) found
           order by created_at desc, kind, id limit $13)
         select p.kind, p.id, p.conversation_id, p.created_at,
                c.title, c.harness, c.model, c.archived_at is not null archived,
                coalesce(b.created_seq, i.feed_seq) seq, b.parent_block_id, i.origin,
                ts_headline('pg_catalog.russian',
                  left(replace(replace(case when p.kind = 'answer' then b.content else {INPUT_TEXT} end,
                                       U&'\\E000', ''), U&'\\E001', ''), 32768),
                  websearch_to_tsquery('pg_catalog.russian', $2),
                  U&'StartSel=\\E000,StopSel=\\E001,MaxFragments=2,MaxWords=24,MinWords=8,FragmentDelimiter=\" … \"') snippet
         from page p
         join conversations c on c.id = p.conversation_id
         left join blocks b on p.kind = 'answer' and b.block_id = p.id
         left join inputs i on p.kind = 'message' and i.id = p.id
         order by p.created_at desc, p.kind, p.id"
    )
});

/// Число совпадений до `$10` строк в каждой ветви.
pub static TOTAL_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "select count(*) from (
           (select 1 from blocks b join conversations c on c.id = b.conversation_id
            where {BLOCK_WHERE} limit $10)
           union all
           (select 1 from inputs i join conversations c on c.id = i.conversation_id
            where {INPUT_WHERE} limit $10)
         ) found"
    )
});

/// Верхние предки вложенных блоков: до 32 шагов, при обрыве цепочки — самый верхний найденный.
const JUMP_SQL: &str = "with recursive up(start, id, parent, seq, step) as (
   select block_id, block_id, parent_block_id, created_seq, 0 from blocks where block_id = any($1)
   union all
   select u.start, x.block_id, x.parent_block_id, x.created_seq, u.step + 1
   from blocks x join up u on x.block_id = u.parent where u.step < 32)
 select distinct on (start) start, seq from up order by start, step desc";

/// Привязка общих условий `$1`–`$9`.
macro_rules! bind_filters {
    ($query:expr, $q:expr) => {
        $query
            .bind($q.owner_id)
            .bind(&$q.text)
            .bind($q.family.as_deref())
            .bind($q.conversation_id)
            .bind($q.since)
            .bind($q.until)
            .bind($q.harness.as_deref())
            .bind($q.kind != Some(SearchKind::Messages))
            .bind($q.kind != Some(SearchKind::Answers))
    };
}

/// Поиск под слотом и сроком. Пустой по смыслу запрос (одни служебные слова, только
/// исключения) — не ошибка, а пустая страница с `no_terms`.
pub async fn search(pool: &PgPool, query: &SearchQuery) -> Result<SearchPage, SearchError> {
    let started = std::time::Instant::now();
    let _slot = tokio::time::timeout(TIMEOUT, SLOTS.acquire())
        .await
        .map_err(|_| SearchError::Timeout)?
        .expect("search semaphore is never closed");
    let mut tx = crate::db::begin(pool).await?;
    let page = run(&mut tx, query).await?;
    tx.commit().await?;
    tracing::debug!(
        kind = "history_search",
        hits = page.hits.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "history search"
    );
    Ok(page)
}

async fn run(conn: &mut PgConnection, query: &SearchQuery) -> Result<SearchPage, SearchError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "set local statement_timeout = {}",
        TIMEOUT.as_millis()
    )))
    .execute(&mut *conn)
    .await?;
    // Запрос без лексем даёт NOTICE: до клиента и в журнал sqlx он не доходит.
    sqlx::query("set local client_min_messages = warning")
        .execute(&mut *conn)
        .await?;
    let tree: String =
        sqlx::query_scalar("select querytree(websearch_to_tsquery('pg_catalog.russian', $1))")
            .bind(&query.text)
            .fetch_one(&mut *conn)
            .await?;
    if tree.is_empty() || tree == "T" {
        return Ok(SearchPage::empty(true));
    }
    let cursor = query.cursor;
    let rows = bind_filters!(sqlx::query(sqlx::AssertSqlSafe(PAGE_SQL.as_str())), query)
        .bind(cursor.map(|c| c.at))
        .bind(cursor.map(|c| c.kind.as_str()))
        .bind(cursor.map(|c| c.id))
        .bind(query.limit + 1)
        .fetch_all(&mut *conn)
        .await?;
    let more = rows.len() as i64 > query.limit;
    let mut hits = Vec::with_capacity(rows.len());
    let mut nested = Vec::new();
    for row in rows.iter().take(query.limit as usize) {
        let kind = if row.try_get::<String, _>("kind")? == "answer" {
            HitKind::Answer
        } else {
            HitKind::Message
        };
        let id: Uuid = row.try_get("id")?;
        let at: DateTime<Utc> = row.try_get("created_at")?;
        let seq: i64 = row.try_get("seq")?;
        let origin: Option<serde_json::Value> = row.try_get("origin")?;
        if kind == HitKind::Answer && row.try_get::<Option<Uuid>, _>("parent_block_id")?.is_some() {
            nested.push(id);
        }
        let snippet: String = row.try_get("snippet")?;
        hits.push(Hit {
            conversation_id: row.try_get("conversation_id")?,
            conversation_title: row.try_get("title")?,
            harness: row.try_get("harness")?,
            model: row.try_get("model")?,
            archived: row.try_get("archived")?,
            kind,
            origin_kind: (kind == HitKind::Message).then(|| origin_kind(origin.as_ref())),
            block_id: (kind == HitKind::Answer).then_some(id),
            input_id: (kind == HitKind::Message).then_some(id),
            seq,
            jump_seq: seq,
            created_at: time(at),
            snippet: segments(&snippet),
            key: Some(Cursor { at, kind, id }),
        });
    }
    if !nested.is_empty() {
        let jumps: Vec<(Uuid, i64)> = sqlx::query_as(JUMP_SQL)
            .bind(&nested)
            .fetch_all(&mut *conn)
            .await?;
        for hit in &mut hits {
            if let Some((_, seq)) = jumps.iter().find(|(id, _)| Some(*id) == hit.block_id) {
                hit.jump_seq = *seq;
            }
        }
    }
    let counted: i64 = bind_filters!(
        sqlx::query_scalar(sqlx::AssertSqlSafe(TOTAL_SQL.as_str())),
        query
    )
    .bind(TOTAL_CAP + 1)
    .fetch_one(&mut *conn)
    .await?;
    let next_cursor = if more {
        hits.last().and_then(Hit::cursor)
    } else {
        None
    };
    Ok(SearchPage {
        hits,
        next_cursor,
        total: counted.min(TOTAL_CAP),
        total_capped: counted > TOTAL_CAP,
        no_terms: false,
    })
}

fn origin_kind(origin: Option<&serde_json::Value>) -> OriginKind {
    match origin
        .and_then(|origin| origin.get("kind"))
        .and_then(serde_json::Value::as_str)
    {
        None => OriginKind::Owner,
        Some("message") => OriginKind::Message,
        Some("spawn") => OriginKind::Spawn,
        Some(_) => OriginKind::Result,
    }
}

/// Разметка `ts_headline` в куски: пробелы и переводы строк склеены в один пробел, длина
/// ограничена [`SNIPPET_MAX_CHARS`].
pub fn segments(headline: &str) -> Vec<SnippetSegment> {
    let mut out: Vec<SnippetSegment> = Vec::new();
    let mut hit = false;
    let mut current = String::new();
    let mut chars = 0;
    let mut space = false;
    let flush = |out: &mut Vec<SnippetSegment>, current: &mut String, hit: bool| {
        if current.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.hit == hit => last.text.push_str(current),
            _ => out.push(SnippetSegment {
                text: std::mem::take(current),
                hit,
            }),
        }
        current.clear();
    };
    for c in headline.chars() {
        match c {
            START | STOP => {
                // Пробел перед отметкой остаётся в предыдущем куске, а не в подсветке.
                if space && chars > 0 && chars + 1 < SNIPPET_MAX_CHARS {
                    current.push(' ');
                    chars += 1;
                }
                space = false;
                flush(&mut out, &mut current, hit);
                hit = c == START;
            }
            c if c.is_whitespace() => space = true,
            c => {
                if chars >= SNIPPET_MAX_CHARS {
                    break;
                }
                if space && (chars > 0) {
                    if chars + 1 >= SNIPPET_MAX_CHARS {
                        break;
                    }
                    current.push(' ');
                    chars += 1;
                }
                space = false;
                current.push(c);
                chars += 1;
            }
        }
    }
    flush(&mut out, &mut current, hit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headline_becomes_segments_with_collapsed_whitespace() {
        let text = format!("  Ошибка\n\nпри {START}сборке{STOP}:\t{START}сборка{STOP} упала ");
        assert!(
            segments(&text)
                == vec![
                    SnippetSegment {
                        text: "Ошибка при ".into(),
                        hit: false
                    },
                    SnippetSegment {
                        text: "сборке".into(),
                        hit: true
                    },
                    SnippetSegment {
                        text: ": ".into(),
                        hit: false
                    },
                    SnippetSegment {
                        text: "сборка".into(),
                        hit: true
                    },
                    SnippetSegment {
                        text: " упала".into(),
                        hit: false
                    },
                ]
        );
        assert!(segments("").is_empty());
        let long = "слово ".repeat(1000);
        let total: usize = segments(&long).iter().map(|s| s.text.chars().count()).sum();
        assert!(total <= SNIPPET_MAX_CHARS, "{total}");
    }

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        let cursor = Cursor {
            at: DateTime::from_timestamp_micros(1_762_600_000_123_456).unwrap(),
            kind: HitKind::Message,
            id: Uuid::now_v7(),
        };
        assert!(Cursor::decode(&cursor.encode()) == Some(cursor));
        for bad in ["", "!!", "MTox", &URL_SAFE_NO_PAD.encode("1:x:abc")] {
            assert!(Cursor::decode(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn query_debug_has_no_text() {
        let query = RawQuery {
            text: "секретное слово".into(),
            conversation_id: None,
            since: None,
            until: None,
            harness: None,
            kind: None,
            cursor: None,
            limit: None,
        }
        .check(Uuid::nil(), None)
        .unwrap();
        let debug = format!("{query:?}");
        assert!(!debug.contains("секрет"), "{debug}");
        assert!(debug.contains("query_chars"), "{debug}");
    }
}
