//! `history_search`: поиск по истории сессий, которые видит вызывающий. Корневая сессия видит
//! все сессии владельца, делегированная — предков и потомков, как `sessions_list`. Только
//! чтение, поэтому проверки `read_only` нет.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, access, parse_arguments};
use crate::search::{self, Hit, HitKind, OriginKind, RawQuery, SearchError, SearchKind};

/// Бюджет ответа модели, как у `session_inspect`.
const TOTAL_BYTES: usize = 64 * 1024;

const NO_TERMS_HINT: &str = "No words to search for: the query holds only stop words or only exclusions. Use content words; whole words match in any Russian or English form, substrings and partial identifiers do not.";
const NOTHING_HINT: &str = "Nothing found. Whole words match in any Russian or English form; substrings and partial identifiers (paths, code fragments) do not match.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    session_id: Option<Uuid>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    harness: Option<String>,
    kind: Option<SearchKind>,
    limit: Option<i64>,
    cursor: Option<String>,
}

pub async fn search(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SearchArgs = parse_arguments(arguments)?;
    let mut tx = crate::db::begin(&context.pool).await?;
    let session = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if let Some(target) = args.session_id {
        access::target_in_tx(&mut tx, &context.config, &session, caller.owner_id, target).await?;
    }
    let family = access::family_in_tx(&mut tx, &context.config, &session, caller.owner_id).await?;
    tx.commit().await?;
    let query = RawQuery {
        text: args.query,
        conversation_id: args.session_id,
        since: args.since,
        until: args.until,
        harness: args.harness,
        kind: args.kind,
        cursor: args.cursor,
        limit: args.limit,
    }
    .check(caller.owner_id, family)
    .map_err(tool_error)?;
    let mut page = search::search(&context.pool, &query)
        .await
        .map_err(tool_error)?;
    let mut hits = Vec::with_capacity(page.hits.len());
    let mut lines = Vec::with_capacity(page.hits.len());
    let mut used = 0;
    let mut cut = false;
    for hit in &page.hits {
        let row = hit_json(hit);
        let line = hit_line(hit);
        let size = row.to_string().len() + line.len();
        if used + size > TOTAL_BYTES && !hits.is_empty() {
            cut = true;
            break;
        }
        used += size;
        hits.push(row);
        lines.push(line);
    }
    if cut {
        page.next_cursor = page.hits.get(hits.len() - 1).and_then(Hit::cursor);
    }
    let mut text = if hits.is_empty() {
        if page.no_terms {
            NO_TERMS_HINT.to_owned()
        } else {
            NOTHING_HINT.to_owned()
        }
    } else {
        let total = if page.total_capped {
            format!("more than {}", page.total)
        } else {
            page.total.to_string()
        };
        format!("{} of {total} matches, newest first.", hits.len())
    };
    for line in &lines {
        text.push('\n');
        text.push_str(line);
    }
    if let Some(cursor) = &page.next_cursor {
        text.push_str(&format!("\nMore: call again with cursor \"{cursor}\"."));
    }
    Ok(json!({
        "text": text,
        "hits": hits,
        "next_cursor": page.next_cursor,
        "total": page.total,
        "total_capped": page.total_capped,
    }))
}

fn tool_error(error: SearchError) -> ToolError {
    match error {
        SearchError::Invalid => ToolError::new(ToolErrorCode::InvalidArguments),
        SearchError::Timeout => ToolError::with_message(
            ToolErrorCode::Internal,
            "The search took too long; narrow it with session_id, since or more words",
        ),
        SearchError::Db(error) => error.into(),
    }
}

fn origin_label(hit: &Hit) -> &'static str {
    match (hit.kind, hit.origin_kind) {
        (HitKind::Answer, _) => "answer",
        (_, Some(OriginKind::Message)) => "message from a session",
        (_, Some(OriginKind::Spawn)) => "spawn brief",
        (_, Some(OriginKind::Result)) => "session result",
        _ => "owner message",
    }
}

fn snippet_text(hit: &Hit) -> String {
    hit.snippet
        .iter()
        .map(|segment| {
            if segment.hit {
                format!("**{}**", segment.text)
            } else {
                segment.text.clone()
            }
        })
        .collect()
}

/// Настоящий ID блока, а не верхнего предка: `session_inspect {block_id}` читает его целиком.
fn hit_json(hit: &Hit) -> Value {
    json!({
        "session_id": hit.conversation_id,
        "session_title": hit.conversation_title,
        "harness": hit.harness,
        "model": hit.model,
        "archived": hit.archived,
        "kind": hit.kind,
        "origin_kind": hit.origin_kind,
        "block_id": hit.block_id,
        "input_id": hit.input_id,
        "seq": hit.seq,
        "created_at": hit.created_at,
        "snippet": snippet_text(hit),
    })
}

fn hit_line(hit: &Hit) -> String {
    let id = match (hit.block_id, hit.input_id) {
        (Some(block), _) => format!("block_id {block}"),
        (_, Some(input)) => format!("input_id {input}"),
        _ => String::new(),
    };
    let archived = if hit.archived { " (archived)" } else { "" };
    format!(
        "- {} · \"{}\"{archived} · {} · session_id {} · {id}\n  {}",
        hit.created_at,
        super::origin::clean_title(&hit.conversation_title),
        origin_label(hit),
        hit.conversation_id,
        snippet_text(hit),
    )
}
