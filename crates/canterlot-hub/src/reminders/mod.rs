//! Напоминания агентам служебной строкой `[canterlot]` в начале следующего ввода:
//! устаревшие задачи плана оркестратора и список задач, который агент перестал вести.
//!
//! Напоминание не начинает ход: это служебная заметка (`submit_service_note_in_tx`), которую
//! выдача приклеивает к вводу. Обе включаются и выключаются настройкой владельца.

pub mod plan;
pub mod todo;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use tracing::warn;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::delivery::{ContentBlock, SubmitError, SubmitInput, submit_service_note_in_tx};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentReminderSettings {
    /// Напоминать оркестратору об устаревших задачах его плана.
    pub plan_enabled: bool,
    /// Напоминать агенту о списке задач, который давно не менялся.
    pub todo_enabled: bool,
}

impl Default for AgentReminderSettings {
    fn default() -> Self {
        Self {
            plan_enabled: true,
            todo_enabled: true,
        }
    }
}

pub async fn settings(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<AgentReminderSettings, sqlx::Error> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "select plan_enabled, todo_enabled from agent_reminder_settings where owner_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(conn)
    .await?;
    Ok(
        row.map_or_else(Default::default, |(plan_enabled, todo_enabled)| {
            AgentReminderSettings {
                plan_enabled,
                todo_enabled,
            }
        }),
    )
}

/// Действует со следующей выдачи: уже приклеенные заметки не отзываются.
pub async fn replace_settings_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    next: AgentReminderSettings,
) -> Result<AgentReminderSettings, sqlx::Error> {
    sqlx::query(
        "insert into agent_reminder_settings (owner_id, plan_enabled, todo_enabled)
         values ($1, $2, $3)
         on conflict (owner_id) do update
         set plan_enabled = excluded.plan_enabled, todo_enabled = excluded.todo_enabled,
             updated_at = clock_timestamp()",
    )
    .bind(owner_id)
    .bind(next.plan_enabled)
    .bind(next.todo_enabled)
    .execute(conn)
    .await?;
    Ok(next)
}

/// Перед выдачей ввода разговору: положить нужные напоминания служебными заметками, чтобы
/// `with_service_notes` приклеил их к этому же вводу. Вызывающий держит очередь разговора.
pub async fn prepare_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some((owner_id, side)): Option<(Uuid, bool)> = sqlx::query_as(
        "select owner_id, side_of is not null from conversations
         where id = $1 and trashed_at is null",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    let settings = settings(&mut *conn, owner_id).await?;
    if settings.plan_enabled
        && let Some((text, numbers)) = plan::take_in_tx(&mut *conn, conversation_id).await?
    {
        let origin = event_origin(json!({"kind": "plan_reminder", "plan_tasks": numbers}));
        note_in_tx(
            &mut *conn,
            owner_id,
            conversation_id,
            "plan_reminder",
            text,
            origin,
            correlation_id,
        )
        .await?;
    }
    // Боковой чат — короткий разговор владельца о родителе: список задач в нём не ведут.
    if settings.todo_enabled
        && !side
        && let Some((text, revision)) = todo::take_in_tx(&mut *conn, conversation_id).await?
    {
        let origin = event_origin(json!({"kind": "todo_reminder", "todo_revision": revision}));
        note_in_tx(
            conn,
            owner_id,
            conversation_id,
            "todo_reminder",
            text,
            origin,
            correlation_id,
        )
        .await?;
    }
    Ok(())
}

fn event_origin(event: Value) -> Value {
    json!({"kind": "event", "chain_depth": 0, "event": event})
}

/// Отказ проверки содержимого ничего не записал и выдачу не останавливает; ошибка базы
/// прерывает транзакцию выдачи целиком.
async fn note_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    kind: &str,
    text: String,
    origin: Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    match submit_service_note_in_tx(
        conn,
        &SubmitInput {
            owner_id,
            conversation_id,
            idempotency_key: format!("{kind}:{conversation_id}:{}", Uuid::now_v7()),
            content: vec![ContentBlock::Text { text }],
            received_at: Utc::now(),
            correlation_id,
            origin: Some(origin),
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(SubmitError::Db(error)) => Err(error),
        Err(error) => {
            warn!(kind = "reminder_note_failed", reminder = kind, %conversation_id, error = %error, "reminder note was not written");
            Ok(())
        }
    }
}
