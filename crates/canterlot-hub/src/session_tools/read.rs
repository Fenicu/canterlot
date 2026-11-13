//! `sessions_list` и `session_inspect`. Модель видит текст сообщений, поэтому всё усечено,
//! а решения по запросам и рассуждения чужой модели сюда не попадают совсем.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, access, parse_arguments};
use crate::{
    activity::{ActivityState, extract_request_summary},
    conversations::{self, AgentListQuery, ListItem},
    history::{self, HistoryCursor, HistoryError, InputContentBlock, TimelineItem},
    requests::RequestKind,
};

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;
const DEFAULT_MESSAGES: i64 = 5;
const MAX_MESSAGES: i64 = 20;
/// Запросы и рассуждения не показываются, поэтому страница берётся с запасом.
const PAGE_FACTOR: i64 = 3;
const MESSAGE_BYTES: usize = 4096;
const TOTAL_BYTES: usize = 64 * 1024;
/// Одна страница блока: длинный результат читается по `next_offset`.
const BLOCK_PAGE_BYTES: i64 = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    state: Option<ActivityState>,
    #[serde(default)]
    include_archived: bool,
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectArgs {
    session_id: Uuid,
    messages: Option<i64>,
    block_id: Option<Uuid>,
    offset: Option<i64>,
}

fn bounded(value: Option<i64>, default: i64, min: i64, max: i64) -> Result<i64, ToolError> {
    let value = value.unwrap_or(default);
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(ToolError::new(ToolErrorCode::InvalidArguments))
    }
}

fn session_row(item: &ListItem, caller: Uuid) -> Value {
    let a = &item.attributes;
    json!({
        "id": a.id,
        "title": a.title,
        "harness": a.harness.id,
        "model": item.model,
        "workdir": a.workdir,
        "is_self": a.id == caller,
        "parent_id": a.parent_id,
        "activity": {
            "state": item.activity.state,
            "evidence": item.activity.evidence,
            "since": item.activity.since,
        },
        "queued": item.queue.queued,
        "worktree": a.worktree,
    })
}

pub async fn list(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ListArgs = parse_arguments("sessions_list", arguments)?;
    let limit = bounded(args.limit, DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    let mut tx = crate::db::begin(&context.pool).await?;
    let session = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let family = access::family_in_tx(&mut tx, &context.config, &session, caller.owner_id).await?;
    let items = conversations::agent_list(
        &mut tx,
        &AgentListQuery {
            owner_id: caller.owner_id,
            family,
            state: args.state,
            include_archived: args.include_archived,
            limit,
        },
    )
    .await?;
    let queued_spawns =
        crate::session_tools::spawn_queue::waiting_summary(&mut tx, caller.conversation_id).await?;
    tx.commit().await?;
    let sessions: Vec<Value> = items
        .iter()
        .map(|item| session_row(item, caller.conversation_id))
        .collect();
    Ok(json!({"sessions": sessions, "queued_spawns": queued_spawns}))
}

/// Обрезка по границе символа; `total_bytes` говорит о полном размере.
fn clipped(text: &str, total_bytes: i64, truncated_before: bool) -> Value {
    let cut = history::prefix(text, MESSAGE_BYTES);
    json!({
        "text": cut,
        "truncated": truncated_before || cut.len() < text.len(),
        "total_bytes": total_bytes,
    })
}

/// Продолжение усечённого блока; у ввода его нет, он хранится не в блоках.
fn next_offset(row: &Value) -> Option<usize> {
    let truncated = row["truncated"].as_bool().unwrap_or(false);
    truncated.then(|| row["text"].as_str().map_or(0, str::len))
}

fn message(item: &TimelineItem) -> Option<Value> {
    match item {
        TimelineItem::Request(_) => None,
        // Служебная строка ленты для PWA; в сообщениях агенту её нет, как и запросов.
        TimelineItem::Compaction(_) => None,
        TimelineItem::Block(block) if block.kind == "reasoning" => None,
        TimelineItem::Block(block) => {
            let mut row = clipped(&block.content, block.content_bytes, block.content_truncated);
            row["next_offset"] = json!(next_offset(&row));
            row["id"] = json!(block.id);
            row["kind"] = json!("block");
            row["block_kind"] = json!(block.kind);
            row["agent"] = json!(block.agent);
            row["model"] = json!(block.model);
            row["tool_name"] = json!(block.tool_name);
            row["summary"] = json!(block.summary);
            row["state"] = json!(block.state);
            row["created_at"] = json!(block.created_at);
            Some(row)
        }
        TimelineItem::Input(input) => {
            let text = input
                .content
                .iter()
                .filter_map(|block| match block {
                    InputContentBlock::Text { text } => Some(text.as_str()),
                    InputContentBlock::Attachment(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let attachments: Vec<&str> =
                input.attachments.iter().map(|a| a.name.as_str()).collect();
            let mut row = clipped(&text, text.len() as i64, false);
            row["id"] = json!(input.id);
            row["kind"] = json!("input");
            row["from_owner"] = json!(input.origin.is_none());
            row["attachments"] = json!(attachments);
            row["state"] = json!(input.state);
            row["created_at"] = json!(input.created_at);
            Some(row)
        }
    }
}

/// Новые важнее старых: бюджет тратится с конца ленты, порядок ответа прежний.
fn within_budget(mut rows: Vec<Value>, count: usize) -> (Vec<Value>, usize) {
    let skip = rows.len().saturating_sub(count);
    rows.drain(..skip);
    let total = rows.len();
    let mut spent = 0;
    let mut kept = Vec::new();
    for row in rows.into_iter().rev() {
        spent += row["text"].as_str().map_or(0, str::len);
        if spent > TOTAL_BYTES {
            break;
        }
        kept.push(row);
    }
    kept.reverse();
    let omitted = total - kept.len();
    (kept, omitted)
}

async fn processes(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    let metrics: Option<(Value, DateTime<Utc>)> = sqlx::query_as(
        "select mm.executions,mm.observed_at from machine_metrics mm
         join conversations c on c.machine_id=mm.machine_id where c.id=$1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((executions, observed_at)) = metrics else {
        return Ok(Vec::new());
    };
    let own: Vec<String> =
        sqlx::query_scalar("select id::text from executions where conversation_id=$1")
            .bind(id)
            .fetch_all(conn)
            .await?;
    Ok(executions
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["execution_id"]
                .as_str()
                .is_some_and(|execution| own.iter().any(|id| id == execution))
        })
        .map(|row| {
            json!({
                "execution_id": row["execution_id"],
                "alive": row["alive"],
                "cpu_permille": row["cpu_permille"],
                "rss_bytes": row["rss_bytes"],
                "observed_at": observed_at,
            })
        })
        .collect())
}

async fn last_run(conn: &mut PgConnection, id: Uuid) -> Result<Value, sqlx::Error> {
    let row = sqlx::query(
        "select r.started_at,r.finished_at,r.duration_ms,r.git,r.tests,
                coalesce(sum(u.input_tokens),0)::bigint input_tokens,
                coalesce(sum(u.output_tokens),0)::bigint output_tokens
         from runs r left join run_model_usage u on u.input_command_id=r.input_command_id
         where r.conversation_id=$1 group by r.input_command_id
         order by r.started_at desc limit 1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else {
        return Ok(Value::Null);
    };
    let git: Option<Value> = row.try_get("git")?;
    Ok(json!({
        "started_at": row.try_get::<DateTime<Utc>, _>("started_at")?,
        "finished_at": row.try_get::<Option<DateTime<Utc>>, _>("finished_at")?,
        "duration_ms": row.try_get::<Option<i64>, _>("duration_ms")?,
        "input_tokens": row.try_get::<i64, _>("input_tokens")?,
        "output_tokens": row.try_get::<i64, _>("output_tokens")?,
        "git": git.map(|git| json!({
            "available": git["available"],
            "head_before": git["head_before"],
            "head_after": git["head_after"],
            "dirty": git["dirty"],
            "truncated": git["truncated"],
            "commits": git["commits"].as_array().map_or(0, Vec::len),
            "files": git["files"].as_array().map_or(0, Vec::len),
        })),
        "tests": row.try_get::<Value, _>("tests")?,
    }))
}

async fn open_requests(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Value>, sqlx::Error> {
    let rows: Vec<(Uuid, String, Value)> = sqlx::query_as(
        "select r.id,r.kind,r.request_payload from requests r
         join executions e on e.id=r.execution_id
         where r.conversation_id=$1 and r.state='open'
           and e.state not in ('stopped','failed','owner_lost')
         order by r.created_at,r.id",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, payload)| {
            let summary = kind
                .parse::<RequestKind>()
                .ok()
                .and_then(|parsed| extract_request_summary(parsed, &payload));
            json!({"id": id, "kind": kind, "summary": summary})
        })
        .collect())
}

/// Страница одного блока цели. Рассуждения остаются закрыты и здесь: для модели такой блок
/// неотличим от несуществующего.
async fn block_page(
    conn: &mut PgConnection,
    owner_id: Uuid,
    target: Uuid,
    block_id: Uuid,
    offset: i64,
) -> Result<Value, ToolError> {
    let invalid = || ToolError::new(ToolErrorCode::InvalidArguments);
    let kind: Option<String> = sqlx::query_scalar(
        "select kind from blocks where block_id=$1 and conversation_id=$2 and kind<>'reasoning'",
    )
    .bind(block_id)
    .bind(target)
    .fetch_optional(&mut *conn)
    .await?;
    let kind = kind.ok_or_else(invalid)?;
    let page = history::block_content(conn, owner_id, target, block_id, offset, BLOCK_PAGE_BYTES)
        .await
        .map_err(|error| match error {
            HistoryError::Db(error) => error.into(),
            HistoryError::InvalidOffset | HistoryError::InvalidLimit => invalid(),
        })?
        .ok_or_else(invalid)?;
    Ok(json!({
        "id": block_id,
        "block_kind": kind,
        "text": page.content,
        "offset": page.offset,
        "next_offset": page.next_offset,
        "total_bytes": page.total_bytes,
        "state": page.state,
    }))
}

pub async fn inspect(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: InspectArgs = parse_arguments("session_inspect", arguments)?;
    let count = bounded(args.messages, DEFAULT_MESSAGES, 0, MAX_MESSAGES)?;
    if match args.block_id {
        Some(_) => args.messages.is_some(),
        None => args.offset.is_some(),
    } {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    let mut tx = crate::db::begin(&context.pool).await?;
    let session = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let target = access::target_in_tx(
        &mut tx,
        &context.config,
        &session,
        caller.owner_id,
        args.session_id,
    )
    .await?;
    if let Some(block_id) = args.block_id {
        let offset = args.offset.unwrap_or(0);
        let block = block_page(&mut tx, caller.owner_id, target.id, block_id, offset).await?;
        tx.commit().await?;
        return Ok(json!({"session_id": target.id, "block": block}));
    }
    let item = conversations::list_item(&mut tx, caller.owner_id, target.id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    let (messages, omitted) = if count == 0 {
        (Vec::new(), 0)
    } else {
        let page = history::page(
            &mut tx,
            caller.owner_id,
            target.id,
            HistoryCursor::Tail,
            count * PAGE_FACTOR,
        )
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
        let rows = page.items.iter().filter_map(message).collect();
        within_budget(rows, count as usize)
    };
    let processes = processes(&mut tx, target.id).await?;
    let run = last_run(&mut tx, target.id).await?;
    let requests = open_requests(&mut tx, target.id).await?;
    let nudge = super::nudge::snapshot_rule(&mut tx, target.id).await?;
    let nudge_events = super::nudge::snapshot_events(&mut tx, target.id, 5).await?;
    tx.commit().await?;
    let a = &item.attributes;
    Ok(json!({
        "session": {
            "id": a.id,
            "title": a.title,
            "harness": a.harness.id,
            "model": item.model,
            "workdir": a.workdir,
            "is_self": a.id == caller.conversation_id,
            "parent_id": a.parent_id,
        },
        "activity": {
            "state": item.activity.state,
            "evidence": item.activity.evidence,
            "since": item.activity.since,
        },
        "queued": item.queue.queued,
        "worktree": a.worktree,
        "messages": messages,
        "messages_omitted": omitted,
        "processes": processes,
        "last_run": run,
        "open_requests": requests,
        "nudge": nudge,
        "nudge_events": nudge_events,
    }))
}
