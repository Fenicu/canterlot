//! Durable timeline сжатий контекста и живой расход для индикатора.
//!
//! Эпизод сжатия делят старт и финиш с одним `id` от воркера: повторный старт не
//! дублирует, финиш без старта (переподключение) открывает и закрывает сразу.
//! Живой расход держится только последним на исполнение; итог хода главнее.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

/// Строка ленты сжатия.
#[derive(Serialize, ToSchema)]
pub struct CompactionView {
    pub id: Uuid,
    pub trigger: String,
    pub pre_tokens: Option<i64>,
    /// `None` — сжатие ещё идёт.
    pub ok: Option<bool>,
    pub created_seq: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

fn invalid(what: &str) -> sqlx::Error {
    sqlx::Error::InvalidArgument(format!("invalid compaction fact: {what}"))
}

fn time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Последний известный расход контекста разговора для индикатора: живое исполнение
/// или свежий законченный ход. `None` — данных ещё нет, индикатор скрыт.
#[derive(Serialize, ToSchema)]
pub struct ContextSnapshot {
    pub tokens: i64,
    pub window_tokens: Option<i64>,
}

pub(crate) async fn latest_context(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<ContextSnapshot>, sqlx::Error> {
    let row: Option<(i64, Option<i64>)> = sqlx::query_as(
        "select context_tokens, context_window_tokens from executions
         where conversation_id = $1 and context_tokens is not null
         order by updated_at desc, generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(tokens, window_tokens)| ContextSnapshot {
        tokens,
        window_tokens,
    }))
}

pub(crate) async fn compaction_view(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<CompactionView, sqlx::Error> {
    let row = sqlx::query("select * from compactions where id = $1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let created: DateTime<Utc> = row.try_get("created_at")?;
    let finished: Option<DateTime<Utc>> = row.try_get("finished_at")?;
    Ok(CompactionView {
        id,
        trigger: row.try_get("trigger")?,
        pre_tokens: row.try_get("pre_tokens")?,
        ok: row.try_get("ok")?,
        created_seq: row
            .try_get::<Option<i64>, _>("created_seq")?
            .map(|seq| seq.to_string()),
        created_at: time(created),
        finished_at: finished.map(time),
    })
}

fn execution_of(fact_execution: &str, run: &ExecutionRef) -> Result<(), sqlx::Error> {
    if fact_execution
        .parse::<Uuid>()
        .map_err(|_| invalid("execution id"))?
        != run.execution_id
    {
        return Err(invalid("foreign execution"));
    }
    Ok(())
}

/// Факт воркера `CompactionEvent`: старт открывает эпизод, финиш закрывает.
/// Возвращает `true`, когда запись дублирует уже виденное и событие не нужно.
pub async fn apply_event(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &canterlot_proto::worker::v1::CompactionEvent,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let id = fact.id.parse::<Uuid>().map_err(|_| invalid("id"))?;
    execution_of(&fact.execution_id, run)?;
    let trigger = if fact.manual { "manual" } else { "auto" };
    if fact.started {
        let inserted: Option<bool> = sqlx::query_scalar(
            "insert into compactions (id, conversation_id, execution_id, trigger)
             values ($1, $2, $3, $4) on conflict (id) do nothing returning true",
        )
        .bind(id)
        .bind(run.conversation_id)
        .bind(run.execution_id)
        .bind(trigger)
        .fetch_optional(&mut *conn)
        .await?;
        if inserted.is_none() {
            return Ok(true);
        }
        let event = events::append(
            conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: run.conversation_id,
                },
                kind: "compaction_started",
                conversation_id: Some(run.conversation_id),
                execution_id: Some(run.execution_id),
                payload: serde_json::json!({"id": id, "trigger": trigger}),
                correlation_id,
            },
        )
        .await?;
        sqlx::query("update compactions set created_seq = $2 where id = $1")
            .bind(id)
            .bind(event.scope_seq)
            .execute(&mut *conn)
            .await?;
    } else {
        let pre_tokens: Option<i64> = if fact.pre_tokens > 0 {
            Some(i64::try_from(fact.pre_tokens).map_err(|_| invalid("pre tokens"))?)
        } else {
            None
        };
        let updated = sqlx::query(
            "update compactions set ok = $2,
             pre_tokens = coalesce($3, pre_tokens),
             finished_at = coalesce(finished_at, now())
             where id = $1 and ok is null",
        )
        .bind(id)
        .bind(fact.ok)
        .bind(pre_tokens)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if updated == 0 {
            // Финиш без старта (переподключение) открывает и закрывает сразу;
            // повторный финиш того же эпизода — дубль.
            let exists: bool =
                sqlx::query_scalar("select exists(select 1 from compactions where id = $1)")
                    .bind(id)
                    .fetch_one(&mut *conn)
                    .await?;
            if exists {
                return Ok(true);
            }
            sqlx::query(
                "insert into compactions
                 (id, conversation_id, execution_id, trigger, pre_tokens, ok, finished_at)
                 values ($1, $2, $3, $4, $5, $6, now())",
            )
            .bind(id)
            .bind(run.conversation_id)
            .bind(run.execution_id)
            .bind(trigger)
            .bind(pre_tokens)
            .bind(fact.ok)
            .execute(&mut *conn)
            .await?;
        }
        let event = events::append(
            conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: run.conversation_id,
                },
                kind: "compaction_finished",
                conversation_id: Some(run.conversation_id),
                execution_id: Some(run.execution_id),
                payload: serde_json::json!({"id": id, "trigger": trigger, "ok": fact.ok}),
                correlation_id,
            },
        )
        .await?;
        sqlx::query("update compactions set created_seq = coalesce(created_seq, $2) where id = $1")
            .bind(id)
            .bind(event.scope_seq)
            .execute(&mut *conn)
            .await?;
    }
    Ok(false)
}

/// Живой расход контекста: нулевой размер после сжатия гасит токены, окно при этом
/// не трогаем (сжатие окно не меняет); нулевое окно в прогрессе оставляет прежнее.
pub async fn apply_progress(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &canterlot_proto::worker::v1::ContextProgress,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    execution_of(&fact.execution_id, run)?;
    store_context(
        conn,
        run,
        u64_to_tokens(fact.context_tokens)?,
        u64_to_tokens(fact.context_window_tokens)?,
        correlation_id,
    )
    .await?;
    Ok(false)
}

/// Итог хода — авторитетный расход: зеркалит `runs` в исполнение и сообщает ленте.
/// Нулевой итог данных не несёт: значение не трогаем.
pub async fn note_turn_context(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &canterlot_proto::worker::v1::RunFinished,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if fact.context_tokens == 0 {
        return Ok(true);
    }
    store_context(
        conn,
        run,
        u64_to_tokens(fact.context_tokens)?,
        u64_to_tokens(fact.context_window_tokens)?,
        correlation_id,
    )
    .await?;
    Ok(false)
}

fn u64_to_tokens(value: u64) -> Result<Option<i64>, sqlx::Error> {
    if value == 0 {
        return Ok(None);
    }
    i64::try_from(value)
        .map(Some)
        .map_err(|_| invalid("context tokens"))
}

async fn store_context(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    tokens: Option<i64>,
    window: Option<i64>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "update executions set context_tokens = $2,
         context_window_tokens = coalesce($3, context_window_tokens),
         updated_at = now() where id = $1
         returning context_tokens, context_window_tokens",
    )
    .bind(run.execution_id)
    .bind(tokens)
    .bind(window)
    .fetch_one(&mut *conn)
    .await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "context_changed",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: serde_json::json!({"tokens": row.0, "window_tokens": row.1}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
