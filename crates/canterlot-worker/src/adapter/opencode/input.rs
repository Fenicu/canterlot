//! Ввод OpenCode: тело `POST .../prompt`, проверка повтора и поздняя сверка доставки.
//! Всегда `delivery: "queue"`: значение по умолчанию `steer` влило бы обычный ввод
//! в идущий ход. Повторной отправки нет (D-6): она к тому же будит пустой период работы.

use super::http::{Http, HttpError};
use crate::adapter::{Transmission, codex::input::attachment_lines};
use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub const PROOF_KIND: &str = "opencode_inbox_admitted";
pub const PROOF_KIND_HISTORY: &str = "opencode_history";
const LOOKUP_WAIT: Duration = Duration::from_secs(5);
/// Паузы перед чтением проекций: запрос мог ещё обрабатываться.
const LOOKUP_DELAYS: [Duration; 4] = [
    Duration::from_millis(100),
    Duration::from_millis(400),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

pub fn message_id(command_id: Uuid) -> String {
    format!("msg_{}", command_id.simple())
}

/// Вложения пока идут путями в тексте: двоичный файл в `files` молча исчезает.
pub fn prompt_body(input: &Transmission) -> Result<Value, &'static str> {
    let mut parts = Vec::new();
    if !input.text.is_empty() {
        parts.push(input.text.clone());
    }
    if !input.files.is_empty() {
        parts.push(attachment_lines(&input.files).map_err(|_| "input_build_failed")?);
    }
    let text = parts.join("\n\n");
    if text.len() > MAX_INPUT_TEXT_BYTES {
        return Err("input_too_long");
    }
    Ok(json!({
        "id": message_id(input.command_id),
        "text": text,
        "delivery": "queue",
        "metadata": {"canterlot_command": input.command_id.to_string()},
    }))
}

pub fn accepted_id(reply: &Value) -> Option<&str> {
    reply.pointer("/data/id").and_then(Value::as_str)
}

/// Ввод виден в очереди или в истории; чтение ничего не меняет на сервере.
pub async fn find_input(http: &Http, session: &str, id: &str) -> Result<bool, HttpError> {
    let inbox = http
        .get(&format!("/api/session/{session}/inbox"), LOOKUP_WAIT)
        .await?;
    let queued = inbox
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.get("id").and_then(Value::as_str) == Some(id))
        });
    if queued {
        return Ok(true);
    }
    match http
        .get(&format!("/api/session/{session}/message/{id}"), LOOKUP_WAIT)
        .await
    {
        Ok(_) => Ok(true),
        Err(HttpError::Status { status: 404, .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Сверка после `Undetermined`: запись может появиться чуть позже обрыва.
pub async fn reconcile(http: Http, session: String, id: String) -> bool {
    for delay in LOOKUP_DELAYS {
        tokio::time::sleep(delay).await;
        if matches!(find_input(&http, &session, &id).await, Ok(true)) {
            return true;
        }
    }
    false
}

/// Отказ сервера на приёме: ввод точно не записан.
pub fn is_rejection(status: u16) -> bool {
    matches!(status, 400 | 404 | 409)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: String) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text,
            files: Vec::new(),
            settings: None,
        }
    }

    #[test]
    fn input_over_the_limit_is_refused_before_the_request() {
        assert_eq!(
            prompt_body(&input("я".repeat(MAX_INPUT_TEXT_BYTES))),
            Err("input_too_long")
        );
    }

    #[test]
    fn id_has_the_recorded_shape() {
        let id = message_id(Uuid::from_u128(0xc185021b336546deaa453515fb8d4fb2));
        assert_eq!(id, "msg_c185021b336546deaa453515fb8d4fb2");
    }
}
