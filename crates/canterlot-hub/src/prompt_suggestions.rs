//! Подсказка следующего сообщения от Claude Code: хаб хранит только последнюю на разговор.
//!
//! Подсказки получают только сессии владельца на Claude Code: не делегаты и не другие CLI.
//! Настройка владельца выключает их сразу: запись больше не сохраняется, сохранённые стираются,
//! а флаг CLI уходит при следующем запуске.

use canterlot_proto::worker::v1::PromptSuggested;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

pub const HARNESS: &str = "claude_code";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct PromptSuggestionView {
    /// Предсказанное следующее сообщение владельца, не длиннее 2 КиБ.
    pub text: String,
    pub execution_id: Uuid,
    #[serde(with = "crate::model::utc_time")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptSuggestionSettings {
    /// Подсказки следующего сообщения в пустом поле ввода сессий Claude Code.
    pub enabled: bool,
}

pub async fn settings(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<PromptSuggestionSettings, sqlx::Error> {
    let enabled: Option<bool> =
        sqlx::query_scalar("select enabled from prompt_suggestion_settings where owner_id = $1")
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    Ok(PromptSuggestionSettings {
        enabled: enabled.unwrap_or(true),
    })
}

/// Сохраняет настройку; выключение стирает подсказки всех разговоров владельца с событием.
pub async fn replace_settings_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    next: PromptSuggestionSettings,
    correlation_id: Uuid,
) -> Result<PromptSuggestionSettings, sqlx::Error> {
    sqlx::query(
        "insert into prompt_suggestion_settings (owner_id, enabled) values ($1, $2)
         on conflict (owner_id) do update
         set enabled = excluded.enabled, updated_at = clock_timestamp()",
    )
    .bind(owner_id)
    .bind(next.enabled)
    .execute(&mut *conn)
    .await?;
    if !next.enabled {
        let cleared: Vec<Uuid> = sqlx::query_scalar(
            "delete from prompt_suggestions s using conversations c
             where c.id = s.conversation_id and c.owner_id = $1
             returning s.conversation_id",
        )
        .bind(owner_id)
        .fetch_all(&mut *conn)
        .await?;
        for conversation_id in cleared {
            publish(conn, conversation_id, None, correlation_id).await?;
        }
    }
    Ok(next)
}

/// Включать ли `--prompt-suggestions` при запуске: Claude Code, не делегат, настройка включена.
pub async fn enabled_for_start(
    conn: &mut PgConnection,
    owner_id: Uuid,
    harness: &str,
    delegated: bool,
) -> Result<bool, sqlx::Error> {
    if harness != HARNESS || delegated {
        return Ok(false);
    }
    Ok(settings(conn, owner_id).await?.enabled)
}

/// `true`, если подсказка не сохранена: устарела, разговор её не получает или ввод уже ждёт.
pub async fn apply(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &PromptSuggested,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some(text) = canterlot_proto::limits::bound_prompt_suggestion(&fact.text) else {
        return Ok(true);
    };
    let generation = i64::try_from(fact.generation)
        .map_err(|_| sqlx::Error::InvalidArgument("invalid prompt_suggested: generation".into()))?;
    // Ввод в очереди или в передаче означает новый ход: подсказка к прошлому ему не нужна.
    let stored: Option<PromptSuggestionView> = sqlx::query_as(
        "insert into prompt_suggestions as s (conversation_id, execution_id, generation, text)
         select c.id, $2, $3, $4 from conversations c
         where c.id = $1 and c.harness = $5
           and c.parent_conversation_id is null and c.spawn_depth = 0
           and coalesce((select p.enabled from prompt_suggestion_settings p
                         where p.owner_id = c.owner_id), true)
           and not exists (select 1 from inputs i where i.conversation_id = c.id
                           and i.state in ('queued', 'dispatched', 'on_machine', 'transmitting'))
         on conflict (conversation_id) do update
         set execution_id = excluded.execution_id, generation = excluded.generation,
             text = excluded.text, created_at = clock_timestamp()
         where s.generation <= excluded.generation
         returning text, execution_id, created_at",
    )
    .bind(run.conversation_id)
    .bind(run.execution_id)
    .bind(generation)
    .bind(&text)
    .bind(HARNESS)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(view) = stored else {
        return Ok(true);
    };
    publish(conn, run.conversation_id, Some(&view), correlation_id).await?;
    Ok(false)
}

/// Новый ввод или новый ход: подсказка больше не к месту.
pub async fn clear_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let cleared = sqlx::query("delete from prompt_suggestions where conversation_id = $1")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if cleared == 0 {
        return Ok(false);
    }
    publish(conn, conversation_id, None, correlation_id).await?;
    Ok(true)
}

pub async fn in_snapshot(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<PromptSuggestionView>, sqlx::Error> {
    sqlx::query_as(
        "select text, execution_id, created_at from prompt_suggestions where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await
}

async fn publish(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    suggestion: Option<&PromptSuggestionView>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "prompt_suggestion_changed",
            conversation_id: Some(conversation_id),
            execution_id: suggestion.map(|view| view.execution_id),
            payload: json!({
                "conversation_id": conversation_id,
                "suggestion": suggestion,
            }),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
