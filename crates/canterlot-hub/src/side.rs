//! Боковой чат: read-only копия сессии, форкнутая от её нативной записи.
//!
//! Копия не видна в списках, не шлёт пушей и не трогает очередь родителя. Строка
//! `conversations.side_of` — единственная связь; удаление идёт приложением, без каскада.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    delivery::LockedQueue,
    executions,
    model::{HoldReason, PauseReason},
    session::{CAPABILITY_SIDE_CHAT_V1, Registry},
};

/// Инструкция, которая уходит агенту при первом старте и каждом свежем форке.
pub const SIDE_INSTRUCTION: &str = "You are a read-only side assistant forked from the session described below. Answer the owner's questions about it. Do not modify files, do not run commands that change state, do not start other work.";

/// Инструкция копии уходит режимом пресета: режимы `clean` и `owner` не принимают текст.
const SIDE_PRESET_ID: &str = "00000000-0000-4000-8000-00000000a1de";

const PREFACE_LIMIT: usize = 2000;
const PREFACE_ENTRY_LIMIT: usize = 600;

#[derive(Debug, Error)]
pub enum SideError {
    #[error("conversation not found")]
    NotFound,
    #[error("side chat unsupported for this harness")]
    UnsupportedHarness,
    #[error("worker capability missing")]
    CapabilityMissing,
    #[error("conversation is already a side chat")]
    ParentIsSide,
    #[error("parent has no native session yet")]
    NoNativeId,
    #[error("parent session is busy")]
    ParentBusy,
    #[error("side execution is live")]
    Live,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

/// Строка ответа на создание: `created=false` значит, что копия уже была.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct SideView {
    pub conversation_id: Uuid,
    pub created: bool,
}

/// Нативная запись родителя, которую форкает копия.
pub async fn current_native(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    Ok(sqlx::query_scalar(
        "select native_conversation_id from executions
          where conversation_id=$1 and native_conversation_id is not null
          order by generation desc limit 1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten())
}

/// Родитель занят ходом. OpenCode форкают только на простое: второй сервер на общей
/// базе при активном ходе не проверен и может запустить восстановление чужого хода.
pub async fn parent_busy(conn: &mut PgConnection, parent_id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(String, String)> = sqlx::query_as(
        "select c.harness, coalesce(d.activity_state,'idle')
           from conversations c
           left join conversation_derived d on d.conversation_id=c.id
          where c.id=$1",
    )
    .bind(parent_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.is_some_and(|(harness, state)| harness == "opencode" && state != "idle"))
}

fn assemble_instruction(text: String) -> String {
    if text.trim().is_empty() {
        SIDE_INSTRUCTION.to_owned()
    } else {
        format!("{text}\n\n{SIDE_INSTRUCTION}")
    }
}

/// Общая часть старта копии: поля полезной нагрузки, граница форка и повтор инструкции.
/// Вызывается из `executions` в той же транзакции, что и вставка исполнения.
pub(crate) async fn prepare_start_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    parent_id: Uuid,
    harness: &str,
    first_start: bool,
    instructions: &mut Value,
    payload: &mut Value,
) -> Result<(), sqlx::Error> {
    let parent_native = current_native(conn, parent_id).await?.unwrap_or_default();
    payload["isolate_mcp"] = json!(true);
    payload["canterlot_mcp"] = json!("side");
    payload["fork_from_native"] = json!(parent_native);
    let repeat: bool =
        sqlx::query_scalar("select side_reinstruct or $2 or $3 from conversations where id=$1")
            .bind(conversation_id)
            .bind(harness == "codex")
            .bind(first_start)
            .fetch_one(&mut *conn)
            .await?;
    if repeat {
        let text = instructions
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        instructions["mode"] = json!("preset");
        instructions["preset_id"] = json!(SIDE_PRESET_ID);
        instructions["revision"] = json!(1);
        instructions["title"] = json!("Боковой чат");
        instructions["text"] = json!(assemble_instruction(text));
    }
    // Граница — место родителя в его журнале и в нумерации ленты. Всё, что появится
    // после неё, попадёт в сводку; блоки, ещё стримящиеся сейчас, тоже считаются после.
    let journal: Option<i64> = sqlx::query_scalar(
        "select committed_seq from journal_cursors j
          join executions e on e.id=j.execution_id
         where e.conversation_id=$1 order by j.committed_seq desc limit 1",
    )
    .bind(parent_id)
    .fetch_optional(&mut *conn)
    .await?;
    let seq: Option<i64> = sqlx::query_scalar(
        "select greatest(
           coalesce((select max(created_seq) from inputs where conversation_id=$1),0),
           coalesce((select max(created_seq) from blocks where conversation_id=$1),0))",
    )
    .bind(parent_id)
    .fetch_one(&mut *conn)
    .await?;
    let boundary = json!({"journal_seq": journal, "seq": seq.unwrap_or(0)});
    sqlx::query("update conversations set side_boundary=$2, side_reinstruct=false where id=$1")
        .bind(conversation_id)
        .bind(boundary)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn create(
    pool: &PgPool,
    owner_id: Uuid,
    parent_id: Uuid,
    correlation_id: Uuid,
    registry: &Registry,
) -> Result<SideView, SideError> {
    let capability = sqlx::query_scalar::<_, Uuid>(
        "select machine_id from conversations where id=$1 and owner_id=$2",
    )
    .bind(parent_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?
    .is_some_and(|machine| registry.has_capability(machine, CAPABILITY_SIDE_CHAT_V1));
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Строка родителя — общий замок создания, удаления и пересоздания.
            let parent = sqlx::query(
                "select machine_id,harness,workdir,model,effort,title,side_of
                   from conversations
                  where id=$1 and owner_id=$2 and trashed_at is null
                  for update",
            )
            .bind(parent_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(SideError::NotFound)?;
            let harness: String = parent.try_get("harness")?;
            if harness == "antigravity" {
                return Err(SideError::UnsupportedHarness);
            }
            if parent.try_get::<Option<Uuid>, _>("side_of")?.is_some() {
                return Err(SideError::ParentIsSide);
            }
            let machine_id: Uuid = parent.try_get("machine_id")?;
            if !capability {
                return Err(SideError::CapabilityMissing);
            }
            if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
                "select id from conversations where side_of=$1 and trashed_at is null",
            )
            .bind(parent_id)
            .fetch_optional(&mut *conn)
            .await?
            {
                return Ok(SideView {
                    conversation_id: existing,
                    created: false,
                });
            }
            if current_native(&mut *conn, parent_id).await?.is_none() {
                return Err(SideError::NoNativeId);
            }
            if parent_busy(&mut *conn, parent_id).await? {
                return Err(SideError::ParentBusy);
            }
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into conversations
                   (id,owner_id,machine_id,harness,workdir,title,title_source,model,effort,
                    permission_mode,instruction_profile,instruction_preset_revision,
                    crunch_enabled,parent_conversation_id,spawn_depth,side_of)
                 values($1,$2,$3,$4,$5,$6,'auto',$7,$8,'read_only','clean',null,
                        false,null,0,$9)",
            )
            .bind(id)
            .bind(owner_id)
            .bind(machine_id)
            .bind(&harness)
            .bind(parent.try_get::<String, _>("workdir")?)
            .bind(parent.try_get::<String, _>("title")?)
            .bind(parent.try_get::<String, _>("model")?)
            .bind(parent.try_get::<String, _>("effort")?)
            .bind(parent_id)
            .execute(&mut *conn)
            .await?;
            sqlx::query("insert into queue_state(conversation_id,hold_reasons) values($1,$2)")
                .bind(id)
                .bind(sqlx::types::Json(vec![HoldReason::NoLiveExecution]))
                .execute(&mut *conn)
                .await?;
            sqlx::query("insert into conversation_derived(conversation_id,effective_model,effective_effort,effective_permission_mode,crunch_enabled,hold_reasons,activity_evidence) select id,model,effort,permission_mode,crunch_enabled,q.hold_reasons,'{\"kind\":\"idle\",\"last_turn_at\":null,\"background_work\":null}' from conversations c join queue_state q on q.conversation_id=c.id where c.id=$1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            let _ = correlation_id;
            Ok(SideView {
                conversation_id: id,
                created: true,
            })
        })
    })
    .await
}

/// Удаление копии: сначала остановка, затем обычный путь удаления. Ответ `false` —
/// исполнение ещё живо, его остановку заказали, строки уберёт следующий проход.
pub(crate) async fn stop_and_delete_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, crate::deletion::DeleteError> {
    if executions::live(&mut *conn, id).await?.is_some() {
        let _ =
            crate::delivery::control::stop_in_tx(&mut *conn, owner_id, id, false, correlation_id)
                .await;
        return Ok(false);
    }
    crate::deletion::delete_side_in_tx(&mut *conn, owner_id, id, correlation_id).await?;
    Ok(true)
}

/// Остановка и удаление копии родителя. Возвращает `false`, если копия ещё остановится.
pub(crate) async fn stop_and_delete_for_parent_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent_id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, crate::deletion::DeleteError> {
    let side: Option<Uuid> =
        sqlx::query_scalar("select id from conversations where side_of=$1 for update")
            .bind(parent_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(side) = side else { return Ok(true) };
    stop_and_delete_in_tx(&mut *conn, owner_id, side, correlation_id).await
}

pub async fn remove(
    pool: &PgPool,
    owner_id: Uuid,
    parent_id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, SideError> {
    let removed = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let parent: Option<Uuid> = sqlx::query_scalar(
                "select id from conversations where id=$1 and owner_id=$2 for update",
            )
            .bind(parent_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            if parent.is_none() {
                return Err(SideError::NotFound);
            }
            let side: Option<Uuid> =
                sqlx::query_scalar("select id from conversations where side_of=$1 for update")
                    .bind(parent_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            let Some(side) = side else { return Ok(false) };
            stop_and_delete_in_tx(&mut *conn, owner_id, side, correlation_id)
                .await
                .map_err(|error| match error {
                    crate::deletion::DeleteError::Live => SideError::Live,
                    crate::deletion::DeleteError::NotFound => SideError::NotFound,
                    crate::deletion::DeleteError::Db(error) => SideError::Db(error),
                    _ => SideError::Live,
                })
        })
    })
    .await?;
    Ok(removed)
}

/// Ввод в боковой чат сам поднимает исполнение: следующая копия форкается заново.
/// OpenCode ждёт простоя родителя и держит ввод с понятной причиной.
pub(crate) async fn start_for_input_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), SideError> {
    let side: Option<Uuid> = sqlx::query_scalar("select side_of from conversations where id=$1")
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    let Some(parent_id) = side else {
        return Ok(());
    };
    if executions::live(&mut *conn, conversation_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    if parent_busy(&mut *conn, parent_id).await? {
        let mut queue: LockedQueue =
            sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
                .bind(conversation_id)
                .fetch_one(&mut *conn)
                .await?;
        if !queue.hold_reasons.0.contains(&HoldReason::SideParentBusy) {
            queue.hold_reasons.0.push(HoldReason::SideParentBusy);
            let slot = queue.slot()?;
            queue.save(&mut *conn, &slot, None, correlation_id).await?;
        }
        return Ok(());
    }
    clear_hold(&mut *conn, conversation_id).await?;
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    queue
        .pause_reasons
        .0
        .retain(|reason| *reason != PauseReason::UserStop);
    let slot = queue.slot()?;
    queue.save(&mut *conn, &slot, None, correlation_id).await?;
    match executions::start_in_tx(conn, owner_id, conversation_id, correlation_id).await {
        Ok(_) => Ok(()),
        Err(executions::ExecutionError::AlreadyLive { .. }) => Ok(()),
        Err(executions::ExecutionError::Db(error)) => Err(SideError::Db(error)),
        Err(_) => Ok(()),
    }
}

async fn clear_hold(conn: &mut PgConnection, conversation_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update queue_state set hold_reasons=(
           select coalesce(jsonb_agg(value),'[]'::jsonb)
           from jsonb_array_elements(hold_reasons) value
           where value->>'reason' <> 'side_parent_busy')
         where conversation_id=$1",
    )
    .bind(conversation_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Сводка о сессии для агента. Составляется один раз на выдачу ввода и хранится в
/// полезной нагрузке команды, поэтому повтор отправки не меняет текст.
pub async fn compose_preface(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query(
        "select c.side_of, c.side_boundary, p.title,
                coalesce(d.activity_state,'idle') state, d.activity_evidence evidence
           from conversations c
           join conversations p on p.id=c.side_of
           left join conversation_derived d on d.conversation_id=p.id
          where c.id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let parent_id: Uuid = row.try_get("side_of")?;
    let boundary: Value = row
        .try_get::<Option<Value>, _>("side_boundary")?
        .unwrap_or_else(|| json!({"seq": 0}));
    let after: i64 = boundary["seq"].as_i64().unwrap_or(0);
    let title: String = row.try_get("title")?;
    let state: String = row.try_get("state")?;
    let evidence: Value = row
        .try_get::<Option<Value>, _>("evidence")?
        .unwrap_or_else(|| json!({}));
    let todos: Option<Value> = sqlx::query_scalar(
        "select items from agent_todos where conversation_id=$1 and lane_block_id is null",
    )
    .bind(parent_id)
    .fetch_optional(&mut *conn)
    .await?;
    let owner_rows: Vec<(i64, Value)> = sqlx::query_as(
        "select created_seq, content from inputs
          where conversation_id=$1 and created_seq>$2
            and state not in ('cancelled','rejected')
          order by created_seq",
    )
    .bind(parent_id)
    .bind(after)
    .fetch_all(&mut *conn)
    .await?;
    let agent_rows: Vec<(i64, String)> = sqlx::query_as(
        "select created_seq, content from blocks
          where conversation_id=$1 and kind='text' and parent_block_id is null
            and created_seq>$2 and btrim(content)<>''
          order by created_seq",
    )
    .bind(parent_id)
    .bind(after)
    .fetch_all(&mut *conn)
    .await?;
    let mut entries: Vec<(i64, char, String)> = Vec::new();
    for (seq, content) in owner_rows {
        let text = input_text(&content);
        if !text.trim().is_empty() {
            entries.push((seq, 'В', clip_entry(&text)));
        }
    }
    for (seq, text) in agent_rows {
        entries.push((seq, 'А', clip_agent(&text)));
    }
    entries.sort_by_key(|(seq, _, _)| *seq);
    let mut tail = String::new();
    for (_, who, text) in entries.into_iter().rev() {
        let line = format!("{who}: {text}\n");
        if !tail.is_empty() && tail.len() + line.len() + 400 > PREFACE_LIMIT {
            break;
        }
        tail.insert_str(0, &line);
    }
    let mut preface = format!(
        "Сессия: {title}\nСостояние: {state} ({})\n",
        compact(&evidence)
    );
    if let Some(items) = todos.filter(|items| items.as_array().is_some_and(|a| !a.is_empty())) {
        preface.push_str(&format!("Задачи: {}\n", compact(&items)));
    }
    if !tail.is_empty() {
        preface.push_str("После границы, свежее в конце:\n");
        preface.push_str(&tail);
    }
    if preface.len() > PREFACE_LIMIT {
        let mut cut = PREFACE_LIMIT;
        while !preface.is_char_boundary(cut) {
            cut -= 1;
        }
        preface.truncate(cut);
    }
    Ok(Some(preface))
}

fn input_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn clip_entry(text: &str) -> String {
    let text = text.trim();
    if text.len() <= PREFACE_ENTRY_LIMIT {
        return text.to_owned();
    }
    let mut cut = PREFACE_ENTRY_LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// Длинный ответ агента сокращается до первой и последней строки: середина в сводке
/// всё равно не поместится, а конец обычно и есть итог.
fn clip_agent(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() > 2 || text.len() > 240 {
        let first = lines.first().copied().unwrap_or_default();
        let last = lines.last().copied().unwrap_or_default();
        clip_entry(&format!("{first} … {last}"))
    } else {
        clip_entry(text)
    }
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.len() <= 300 {
        return text;
    }
    let mut cut = 300;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// Уборка боковых чатов: отпускает удержанные вводы после простоя родителя, гасит
/// копию без хода и убирает строки, чей родитель уже удалён.
pub async fn sweep(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    release_waiting(pool).await?;
    stop_idle(pool, now).await?;
    purge_orphans(pool).await?;
    Ok(())
}

async fn release_waiting(pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select c.id, c.owner_id, c.side_of
           from conversations c
           join conversations p on p.id=c.side_of
           join queue_state q on q.conversation_id=c.id
           left join conversation_derived d on d.conversation_id=p.id
          where q.hold_reasons @> '[{\"reason\":\"side_parent_busy\"}]'::jsonb
            and exists(select 1 from inputs i where i.conversation_id=c.id and i.state='queued')
            and (p.harness<>'opencode' or coalesce(d.activity_state,'idle')='idle')",
    )
    .fetch_all(pool)
    .await?;
    let had_rows = !rows.is_empty();
    for (id, owner_id, _parent) in rows {
        let mut tx = crate::db::begin(pool).await?;
        clear_hold(&mut tx, id).await?;
        let result = executions::start_in_tx(&mut tx, owner_id, id, Uuid::now_v7()).await;
        match result {
            Ok(_) | Err(executions::ExecutionError::AlreadyLive { .. }) => tx.commit().await?,
            Err(executions::ExecutionError::Db(error)) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
            Err(_) => tx.commit().await?,
        }
    }
    if had_rows {
        crate::scheduler::notify_pool(pool);
    }
    Ok(())
}

async fn stop_idle(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let cutoff = now - chrono::TimeDelta::minutes(30);
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select c.id, c.owner_id
           from conversations c
           join queue_state q on q.conversation_id=c.id
           left join conversation_derived d on d.conversation_id=c.id
          where c.side_of is not null and c.trashed_at is null
            and exists(select 1 from executions e
                        where e.conversation_id=c.id
                          and e.state in ('created','starting','running','stopping'))
            and q.run_slot='free' and coalesce(d.queue_queued,0)=0
            and coalesce(d.activity_state,'idle')='idle'
            and not exists(select 1 from requests r
                            where r.conversation_id=c.id and r.state='open')
            and coalesce(d.last_activity_at,
                         (select max(e.created_at) from executions e
                           where e.conversation_id=c.id),
                         '-infinity'::timestamptz) < $1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    for (id, owner_id) in rows {
        let mut conn = pool.acquire().await?;
        if let Err(error) =
            crate::delivery::control::stop_in_tx(&mut conn, owner_id, id, false, Uuid::now_v7())
                .await
        {
            tracing::warn!(kind = "side_idle_stop_failed", conversation_id = %id, error = %error, "side idle stop failed");
        }
    }
    Ok(())
}

async fn purge_orphans(pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select c.id, c.owner_id from conversations c
          where c.side_of is not null
            and (c.trashed_at is not null
                 or exists(select 1 from conversations p
                            where p.id=c.side_of and p.trashed_at is not null)
                 or not exists(select 1 from conversations p where p.id=c.side_of))
            and not exists(select 1 from executions e
                            where e.conversation_id=c.id
                              and e.state in ('created','starting','running','stopping'))",
    )
    .fetch_all(pool)
    .await?;
    for (id, owner_id) in rows {
        let mut tx = crate::db::begin(pool).await?;
        match crate::deletion::delete_side_in_tx(&mut tx, owner_id, id, Uuid::now_v7()).await {
            Ok(()) => tx.commit().await?,
            Err(crate::deletion::DeleteError::Live)
            | Err(crate::deletion::DeleteError::NotFound) => {
                let _ = tx.rollback().await;
            }
            Err(crate::deletion::DeleteError::Db(error)) => {
                let _ = tx.rollback().await;
                return Err(error);
            }
            Err(_) => {
                let _ = tx.rollback().await;
            }
        }
    }
    Ok(())
}
