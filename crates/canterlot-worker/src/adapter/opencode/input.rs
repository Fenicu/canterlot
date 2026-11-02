//! Ввод OpenCode: тело `POST .../prompt`, проверка повтора и поздняя сверка доставки.
//! Всегда `delivery: "queue"`: значение по умолчанию `steer` влило бы обычный ввод
//! в идущий ход. Повторной отправки нет (D-6): она к тому же будит пустой период работы.

use super::http::{Http, HttpError};
use crate::adapter::{LocalFile, Transmission, codex::input::attachment_lines};
use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub const PROOF_KIND: &str = "opencode_inbox_admitted";
pub const PROOF_KIND_HISTORY: &str = "opencode_history";
/// Сервер отвергает вложение крупнее.
const MAX_NATIVE_FILE_BYTES: u64 = 20 * 1024 * 1024;
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

fn native(file: &LocalFile) -> Result<Value, &'static str> {
    let path = std::path::absolute(&file.path).map_err(|_| "input_build_failed")?;
    let uri = url::Url::from_file_path(path).map_err(|()| "input_build_failed")?;
    Ok(json!({"uri": uri.as_str(), "name": file.name}))
}

/// Путь в тексте есть у каждого файла: агент работает с ним инструментами. Картинка в пределах
/// модели вдобавок идёт вложением. Только вложением нельзя: двоичный файл в `files` молча
/// исчезает, а картинка у модели без такого ввода становится текстом ошибки.
pub fn prompt_body(input: &Transmission, images: bool) -> Result<Value, &'static str> {
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
    let mut body = json!({
        "id": message_id(input.command_id),
        "text": text,
        "delivery": "queue",
        "metadata": {"canterlot_command": input.command_id.to_string()},
    });
    let attached: Vec<&LocalFile> = input
        .files
        .iter()
        .filter(|file| images && file.image && file.size <= MAX_NATIVE_FILE_BYTES)
        .collect();
    if !attached.is_empty() {
        body["files"] = attached
            .into_iter()
            .map(native)
            .collect::<Result<Vec<_>, _>>()?
            .into();
    }
    Ok(body)
}

/// Тело `POST .../prompt` для вливания в идущий ход: `delivery: "steer"`
/// прерывает текущий шаг и перенаправляет сессию сразу, а не следующим периодом.
/// Хаб показывает действие с подписью про прерывание шага.
pub fn prompt_body_steer(input: &Transmission, images: bool) -> Result<Value, &'static str> {
    let mut body = prompt_body(input, images)?;
    body["delivery"] = "steer".into();
    Ok(body)
}

/// Напоминание модели после выхода из `read_only`. Лежит только в тексте для сервера: лента,
/// журнал и хаб хранят ввод как набран.
pub const READ_ONLY_EXIT_NOTE: &str = "<system-reminder>\nRead-only mode is over. Your tools `shell`, `question`, `write`, `edit` and the others are available again and are called directly by name, not through `execute`. Ignore any Code Mode wording that says tools cannot be called directly: it applies only to the tools listed in its catalog.\n</system-reminder>";

pub fn add_note(body: &mut Value, note: &str) {
    let text = body["text"].as_str().unwrap_or_default();
    body["text"] = if text.is_empty() {
        note.into()
    } else {
        format!("{note}\n\n{text}").into()
    };
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
    use std::path::Path;

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
            prompt_body(&input("я".repeat(MAX_INPUT_TEXT_BYTES)), true),
            Err("input_too_long")
        );
    }

    fn file(name: &str, mime: &str, size: u64, image: bool) -> LocalFile {
        LocalFile {
            path: Path::new("/data/вложения").join(name),
            name: name.into(),
            mime: mime.into(),
            size,
            image,
        }
    }

    fn with_files(files: Vec<LocalFile>) -> Transmission {
        let mut input = input("смотри".into());
        input.files = files;
        input
    }

    // Картинка в пределах модели идёт вложением, путь с кириллицей кодируется как URI.
    #[test]
    fn picture_within_the_model_goes_as_a_file() {
        let body = prompt_body(
            &with_files(vec![file("кот.png", "image/png", 10, true)]),
            true,
        )
        .unwrap();
        assert_eq!(
            body["files"],
            json!([{
                "uri": "file:///data/%D0%B2%D0%BB%D0%BE%D0%B6%D0%B5%D0%BD%D0%B8%D1%8F/%D0%BA%D0%BE%D1%82.png",
                "name": "кот.png",
            }])
        );
    }

    // Вложением идёт только то, что модель прочитает; путь в тексте есть всегда.
    #[test]
    fn everything_else_goes_only_as_a_path_in_the_text() {
        for (model_sees_pictures, attached) in [
            (false, file("кот.png", "image/png", 10, true)),
            (
                true,
                file("большой.png", "image/png", MAX_NATIVE_FILE_BYTES + 1, true),
            ),
            (true, file("отчёт.pdf", "application/pdf", 10, false)),
        ] {
            let name = attached.name.clone();
            let body = prompt_body(&with_files(vec![attached]), model_sees_pictures).unwrap();
            assert!(body.get("files").is_none(), "{name}");
            let text = body["text"].as_str().unwrap();
            assert!(text.starts_with("смотри\n\n"), "{text}");
            assert!(text.contains(&format!("/data/вложения/{name}")), "{text}");
        }
    }

    #[test]
    fn mixed_input_attaches_only_the_picture() {
        let body = prompt_body(
            &with_files(vec![
                file("кот.png", "image/png", 10, true),
                file("данные.bin", "application/octet-stream", 10, false),
            ]),
            true,
        )
        .unwrap();
        assert_eq!(body["files"].as_array().unwrap().len(), 1);
        let text = body["text"].as_str().unwrap();
        assert!(
            text.contains("кот.png") && text.contains("данные.bin"),
            "{text}"
        );
    }

    #[test]
    fn id_has_the_recorded_shape() {
        let id = message_id(Uuid::from_u128(0xc185021b336546deaa453515fb8d4fb2));
        assert_eq!(id, "msg_c185021b336546deaa453515fb8d4fb2");
    }
}
