//! Настройка владельца «Встроенная память Claude»: `on` (по умолчанию) или `off`.
//!
//! При `off` запуски Claude Code в режиме «как у владельца» получают `autoMemoryEnabled: false`
//! и `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`. Хаб кладёт флаг в `StartExecution` на каждом запуске,
//! так что новое значение действует на сессии после их перезапуска.

use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeAutoMemory {
    On,
    Off,
}

impl ClaudeAutoMemory {
    fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaudeAutoMemorySettings {
    /// Встроенная автопамять Claude Code в сессиях режима «как у владельца».
    pub claude_auto_memory: ClaudeAutoMemory,
}

pub async fn settings(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<ClaudeAutoMemorySettings, sqlx::Error> {
    let mode: Option<String> =
        sqlx::query_scalar("select mode from claude_auto_memory_settings where owner_id = $1")
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    Ok(ClaudeAutoMemorySettings {
        claude_auto_memory: if mode.as_deref() == Some("off") {
            ClaudeAutoMemory::Off
        } else {
            ClaudeAutoMemory::On
        },
    })
}

pub async fn replace_settings_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    next: ClaudeAutoMemorySettings,
) -> Result<ClaudeAutoMemorySettings, sqlx::Error> {
    sqlx::query(
        "insert into claude_auto_memory_settings (owner_id, mode) values ($1, $2)
         on conflict (owner_id) do update
         set mode = excluded.mode, updated_at = clock_timestamp()",
    )
    .bind(owner_id)
    .bind(next.claude_auto_memory.as_str())
    .execute(conn)
    .await?;
    Ok(next)
}

/// Нужно ли выключить автопамять при запуске: только Claude Code и только при `off`.
pub async fn disabled_for_start(
    conn: &mut PgConnection,
    owner_id: Uuid,
    harness: &str,
) -> Result<bool, sqlx::Error> {
    if harness != crate::prompt_suggestions::HARNESS {
        return Ok(false);
    }
    Ok(settings(conn, owner_id).await?.claude_auto_memory == ClaudeAutoMemory::Off)
}
