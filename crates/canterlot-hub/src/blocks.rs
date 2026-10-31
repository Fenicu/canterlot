use std::borrow::Cow;

use canterlot_proto::limits::{BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES};
use canterlot_proto::worker::v1::{
    BlockDelta, BlockFinal, BlockIncomplete, BlockStarted, JournalClosed,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::events::{self, NewEvent, Scope};

pub struct ExecutionRef {
    pub execution_id: Uuid,
    pub conversation_id: Uuid,
    pub machine_id: Uuid,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockOutcome {
    Applied,
    Ignored(BlockIgnored),
    Diagnosed(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlockIgnored {
    StaleRevision,
    AfterFinal,
    UnknownBlock,
}

pub fn checksum(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

pub(crate) fn replace_nul(text: &mut String) {
    if let Cow::Owned(replaced) = without_nul(text) {
        *text = replaced;
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// PostgreSQL не принимает U+0000 ни в `text`, ни в `jsonb`, а отказ в одной записи
/// повторялся бы вечно и держал поток данных всей машины.
pub(crate) fn without_nul(text: &str) -> Cow<'_, str> {
    if text.contains('\0') {
        Cow::Owned(text.replace('\0', "\u{fffd}"))
    } else {
        Cow::Borrowed(text)
    }
}

fn id(value: &str) -> Result<Uuid, sqlx::Error> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid block reference".into()))
}
fn optional_id(value: &str) -> Result<Option<Uuid>, sqlx::Error> {
    if value.is_empty() {
        Ok(None)
    } else {
        id(value).map(Some)
    }
}
fn rev(value: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(value).map_err(|_| sqlx::Error::InvalidArgument("invalid block revision".into()))
}

#[derive(sqlx::FromRow)]
struct Block {
    block_id: Uuid,
    rev: i64,
    state: String,
    revision_gap: bool,
    checksum: Option<String>,
}
async fn load(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    block_id: Uuid,
) -> Result<Option<Block>, sqlx::Error> {
    sqlx::query_as("select b.block_id, b.rev, b.state, b.revision_gap, b.checksum from blocks b join executions e on e.id = b.execution_id join conversations c on c.id = e.conversation_id where b.block_id = $1 and b.execution_id = $2 and b.conversation_id = $3 and c.machine_id = $4 for update of b")
        .bind(block_id).bind(run.execution_id).bind(run.conversation_id).bind(run.machine_id).fetch_optional(conn).await
}
async fn emit(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    kind: &'static str,
    payload: Value,
    correlation_id: Uuid,
) -> Result<events::StoredEvent, sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind,
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload,
            correlation_id,
        },
    )
    .await
}

pub(crate) fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

pub async fn start_block(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockStarted,
    correlation_id: Uuid,
) -> Result<BlockOutcome, sqlx::Error> {
    let block_id = id(&fact.block_id)?;
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from blocks where block_id = $1)")
            .bind(block_id)
            .fetch_one(&mut *conn)
            .await?;
    if exists {
        return Ok(BlockOutcome::Ignored(BlockIgnored::UnknownBlock));
    }
    if !matches!(
        fact.kind.as_str(),
        "text" | "reasoning" | "tool_call" | "tool_result" | "warning" | "error"
    ) {
        return Err(sqlx::Error::InvalidArgument("invalid block kind".into()));
    }
    let input = optional_id(&fact.input_command_id)?;
    let parent = optional_id(&fact.parent_block_id)?;
    let inserted = sqlx::query("insert into blocks (block_id, execution_id, conversation_id, kind, state, rev, input_command_id, parent_block_id, agent, model, tool_name, summary, approved_by, line_count) select $1, e.id, e.conversation_id, $4, 'streaming', $5, $6, $7, $9, $10, nullif($11,''), nullif($12,''), nullif($13,''), case when $4='tool_result' then 0 end from executions e join conversations c on c.id = e.conversation_id where e.id = $2 and c.id = $3 and c.machine_id = $8 and ($6::uuid is null or exists (select 1 from commands cmd where cmd.id = $6 and cmd.execution_id = e.id and cmd.machine_id = $8 and cmd.kind = 'deliver_input')) and ($7::uuid is null or exists (select 1 from blocks b where b.block_id = $7 and b.conversation_id = e.conversation_id)) on conflict (block_id) do nothing")
        .bind(block_id).bind(run.execution_id).bind(run.conversation_id).bind(&fact.kind).bind(rev(fact.rev)?).bind(input).bind(parent).bind(run.machine_id).bind(if fact.agent.is_empty(){"main"}else{&fact.agent}).bind(&fact.model).bind(&fact.tool_name).bind(&fact.summary).bind(&fact.approved_by).execute(&mut *conn).await?.rows_affected();
    if inserted == 0 {
        return Ok(BlockOutcome::Ignored(BlockIgnored::UnknownBlock));
    }
    let event = emit(conn, run, "block_started", json!({"block_id": block_id, "rev": fact.rev, "kind": fact.kind, "input_command_id": input, "parent_block_id": parent, "agent": if fact.agent.is_empty(){"main"}else{&fact.agent}, "model": fact.model, "tool_name": non_empty(&fact.tool_name), "summary": non_empty(&fact.summary), "approved_by": non_empty(&fact.approved_by)}), correlation_id).await?;
    sqlx::query("update blocks set created_seq=$2,created_at=$3 where block_id=$1")
        .bind(block_id)
        .bind(event.scope_seq)
        .bind(event.committed_at)
        .execute(&mut *conn)
        .await?;
    Ok(BlockOutcome::Applied)
}

pub async fn apply_delta(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockDelta,
    correlation_id: Uuid,
) -> Result<BlockOutcome, sqlx::Error> {
    if fact.append.len() > MAX_BLOCK_DELTA_BYTES {
        return Err(sqlx::Error::InvalidArgument("block delta limit".into()));
    }
    let block_id = id(&fact.block_id)?;
    let Some(block) = load(conn, run, block_id).await? else {
        return Ok(BlockOutcome::Ignored(BlockIgnored::UnknownBlock));
    };
    if block.state != "streaming" {
        return Ok(BlockOutcome::Ignored(BlockIgnored::AfterFinal));
    }
    let next = rev(fact.rev)?;
    if next <= block.rev {
        return Ok(BlockOutcome::Ignored(BlockIgnored::StaleRevision));
    }
    if rev(fact.base_rev)? != block.rev {
        sqlx::query("update blocks set revision_gap = true where block_id = $1")
            .bind(block_id)
            .execute(&mut *conn)
            .await?;
        emit(conn, run, "block_revision_gap", json!({"block_id": block_id, "current_rev": block.rev, "base_rev": fact.base_rev, "rev": fact.rev}), correlation_id).await?;
        return Ok(BlockOutcome::Diagnosed("block_revision_gap"));
    }
    let append = without_nul(&fact.append);
    let raw = matches!(append, Cow::Owned(_)).then_some(fact.append.as_bytes());
    sqlx::query("insert into block_chunks (block_id, seq, text, raw) values ($1, $2, $3, $4)")
        .bind(block_id)
        .bind(next)
        .bind(append.as_ref())
        .bind(raw)
        .execute(&mut *conn)
        .await?;
    sqlx::query("update blocks set rev = $2, line_count = case when kind='tool_result' then coalesce(line_count,0)+$3 else line_count end, updated_at = clock_timestamp() where block_id = $1")
        .bind(block_id)
        .bind(next)
        .bind(append.bytes().filter(|b| *b==b'\n').count() as i32)
        .execute(&mut *conn)
        .await?;
    emit(
        conn,
        run,
        "block_delta",
        json!({"block_id": block_id, "base_rev": fact.base_rev, "rev": fact.rev, "append": append}),
        correlation_id,
    )
    .await?;
    Ok(BlockOutcome::Applied)
}

pub async fn finalize(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockFinal,
    correlation_id: Uuid,
) -> Result<BlockOutcome, sqlx::Error> {
    if fact.text.len() > BLOCK_FINAL_INLINE_MAX_BYTES {
        return Err(sqlx::Error::InvalidArgument("block final limit".into()));
    }
    let block_id = id(&fact.block_id)?;
    let Some(block) = load(conn, run, block_id).await? else {
        return Ok(BlockOutcome::Ignored(BlockIgnored::UnknownBlock));
    };
    let text = without_nul(&fact.text);
    let accumulated = read_text(conn, block_id).await?;
    let content: &str = if fact.text_omitted {
        &accumulated
    } else {
        &text
    };
    let expected = stored_checksum(conn, block_id, fact, content).await?;
    if block.state == "final" {
        if block.checksum.as_deref() == Some(expected.as_str()) {
            return Ok(BlockOutcome::Ignored(BlockIgnored::AfterFinal));
        }
        emit(
            conn,
            run,
            "block_conflicting_final",
            json!({"block_id": block_id, "rev": fact.rev}),
            correlation_id,
        )
        .await?;
        return Ok(BlockOutcome::Diagnosed("block_conflicting_final"));
    }
    let mismatch = checksum(&accumulated) != expected || checksum(content) != expected;
    if fact.text_omitted && (block.revision_gap || mismatch) {
        sqlx::query("update blocks set content = $2, rev = $3, state = 'incomplete', checksum = null, incomplete_reason = 'revision_gap', updated_at = clock_timestamp() where block_id = $1")
            .bind(block_id).bind(content).bind(rev(fact.rev)?).execute(&mut *conn).await?;
        if mismatch {
            emit(
                conn,
                run,
                "block_checksum_mismatch",
                json!({"block_id": block_id, "rev": fact.rev}),
                correlation_id,
            )
            .await?;
        }
        let event = emit(
            conn,
            run,
            "block_incomplete",
            json!({"block_id": block_id, "rev": fact.rev, "reason": "revision_gap"}),
            correlation_id,
        )
        .await?;
        sqlx::query("update blocks set finished_at=$2,summary=coalesce(nullif($3,''),summary),line_count=case when kind='tool_result' then $4 else line_count end where block_id=$1").bind(block_id).bind(event.committed_at).bind(&fact.summary).bind(content.bytes().filter(|b|*b==b'\n').count() as i32).execute(&mut *conn).await?;
        return Ok(BlockOutcome::Diagnosed(if mismatch {
            "block_checksum_mismatch"
        } else {
            "block_revision_gap"
        }));
    }
    sqlx::query("update blocks set content = $2, rev = $3, state = 'final', checksum = $4, incomplete_reason = null, updated_at = clock_timestamp() where block_id = $1")
        .bind(block_id).bind(content).bind(rev(fact.rev)?).bind(&expected).execute(&mut *conn).await?;
    let event = emit(conn, run, "block_final", json!({"block_id": block_id, "rev": fact.rev, "text": text, "checksum": expected, "text_omitted": fact.text_omitted, "summary": non_empty(&fact.summary)}), correlation_id).await?;
    sqlx::query("update blocks set finished_at=$2,summary=coalesce(nullif($3,''),summary),line_count=case when kind='tool_result' then $4 else line_count end where block_id=$1").bind(block_id).bind(event.committed_at).bind(&fact.summary).bind(content.bytes().filter(|b|*b==b'\n').count() as i32).execute(&mut *conn).await?;
    if mismatch {
        emit(
            conn,
            run,
            "block_checksum_mismatch",
            json!({"block_id": block_id, "rev": fact.rev}),
            correlation_id,
        )
        .await?;
        return Ok(BlockOutcome::Diagnosed("block_checksum_mismatch"));
    }
    Ok(BlockOutcome::Applied)
}

/// Воркер считает сумму по своему тексту, а хаб хранит текст без NUL, и браузер сверяет его.
/// Верная сумма оригинала становится суммой сохранённого текста, неверная остаётся расхождением.
async fn stored_checksum(
    conn: &mut PgConnection,
    block_id: Uuid,
    fact: &BlockFinal,
    stored: &str,
) -> Result<String, sqlx::Error> {
    let original = if fact.text_omitted {
        raw_chunks_checksum(conn, block_id).await?
    } else {
        fact.text.contains('\0').then(|| checksum(&fact.text))
    };
    Ok(if original.as_deref() == Some(fact.checksum.as_str()) {
        checksum(stored)
    } else {
        fact.checksum.clone()
    })
}

/// Сумма дельт в том виде, как их прислал воркер; `None`, если ни одна не менялась.
async fn raw_chunks_checksum(
    conn: &mut PgConnection,
    block_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let chunks: Vec<Vec<u8>> = sqlx::query_scalar(
        "select coalesce(c.raw, convert_to(c.text, 'UTF8')) from block_chunks c
         where c.block_id = $1
           and exists (select 1 from block_chunks r where r.block_id = $1 and r.raw is not null)
         order by c.seq",
    )
    .bind(block_id)
    .fetch_all(conn)
    .await?;
    if chunks.is_empty() {
        return Ok(None);
    }
    let mut digest = Sha256::new();
    for chunk in &chunks {
        digest.update(chunk);
    }
    Ok(Some(hex(&digest.finalize())))
}

pub async fn mark_incomplete(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BlockIncomplete,
    correlation_id: Uuid,
) -> Result<BlockOutcome, sqlx::Error> {
    let block_id = id(&fact.block_id)?;
    let Some(block) = load(conn, run, block_id).await? else {
        return Ok(BlockOutcome::Ignored(BlockIgnored::UnknownBlock));
    };
    if block.state != "streaming" {
        return Ok(BlockOutcome::Ignored(BlockIgnored::AfterFinal));
    }
    sqlx::query("update blocks set content = coalesce((select string_agg(text, '' order by seq) from block_chunks where block_id = $1), ''), state = 'incomplete', rev = $2, incomplete_reason = $3, updated_at = clock_timestamp() where block_id = $1")
        .bind(block_id).bind(rev(fact.rev)?).bind(&fact.reason).execute(&mut *conn).await?;
    let event = emit(
        conn,
        run,
        "block_incomplete",
        json!({"block_id": block_id, "rev": fact.rev, "reason": fact.reason}),
        correlation_id,
    )
    .await?;
    sqlx::query("update blocks set finished_at = $2 where block_id = $1")
        .bind(block_id)
        .bind(event.committed_at)
        .execute(&mut *conn)
        .await?;
    Ok(BlockOutcome::Applied)
}

pub async fn close_journal(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &JournalClosed,
    correlation_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let blocks: Vec<Block> = sqlx::query_as("select b.block_id, b.rev, b.state, b.revision_gap, b.checksum from blocks b join conversations c on c.id = b.conversation_id where b.execution_id = $1 and b.conversation_id = $2 and c.machine_id = $3 and b.state = 'streaming' order by b.block_id for update of b")
        .bind(run.execution_id).bind(run.conversation_id).bind(run.machine_id).fetch_all(&mut *conn).await?;
    let count = blocks.len() as u64;
    for block in blocks {
        mark_incomplete(
            conn,
            run,
            &BlockIncomplete {
                block_id: block.block_id.to_string(),
                rev: block.rev as u64,
                reason: fact.reason.clone(),
            },
            correlation_id,
        )
        .await?;
    }
    Ok(count)
}

pub async fn read_text(conn: &mut PgConnection, block_id: Uuid) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("select case when state = 'streaming' then coalesce((select string_agg(text, '' order by seq) from block_chunks where block_id = $1), '') else content end from blocks where block_id = $1")
        .bind(block_id).fetch_one(conn).await
}
