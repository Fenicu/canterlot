//! `secrets.request`: агент просит у владельца значение и сразу получает
//! идентификатор запроса. Само значение придёт позже сообщением, а не ответом
//! инструмента: у хаба его ещё нет.

use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, parse_arguments};
use crate::secrets::{self, SecretError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    /// Короткое имя секрета для карточки: 1–80 символов.
    name: String,
    /// Зачем он нужен: до 300 символов.
    purpose: String,
}

pub async fn request(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: Args = parse_arguments("secrets.request", arguments)?;
    let view = secrets::open(
        &context.pool,
        caller.owner_id,
        caller.conversation_id,
        &args.name,
        &args.purpose,
        Uuid::now_v7(),
    )
    .await
    .map_err(|error| match error {
        SecretError::Limit => ToolError::new(ToolErrorCode::TooManySecrets),
        SecretError::InvalidName => ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "secrets.request: name must be 1–80 printable characters",
        ),
        SecretError::InvalidPurpose => ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "secrets.request: purpose must be at most 300 printable characters",
        ),
        SecretError::ConversationNotFound | SecretError::NotFound => {
            ToolError::new(ToolErrorCode::ConversationClosed)
        }
        other => ToolError::from_secret(other),
    })?;
    Ok(json!({"request_id": view.id, "status": "requested"}))
}
