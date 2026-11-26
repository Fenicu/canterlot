//! Напоминания агентам служебной строкой `[canterlot]` в начале следующего ввода:
//! устаревшие задачи плана оркестратора и список задач, который агент перестал вести.
//!
//! Напоминание не начинает ход: это служебная заметка (`submit_service_note_in_tx`), которую
//! выдача приклеивает к вводу. Обе включаются и выключаются настройкой владельца.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

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
