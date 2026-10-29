//! Ввод Codex: строго один ход за раз, текст с путями вложений и картинками
//! `localImage`, доказательство доставки эхом `clientId`, проверка повтора
//! по истории, смена настроек ходом, пометка порога и прерывание ходом.
use super::rpc::RpcError;
use super::thread::{OwnerDefaults, PermissionMode, turn_policy};
use crate::adapter::{AdapterEvent, LocalFile, SettingsChange, Transmission, limit_note};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

pub const PROOF_KIND_MESSAGE: &str = "codex_user_message_item";
pub const PROOF_KIND_HISTORY: &str = "codex_history_client_id";
// Ответа на turn/interrupt нет дольше — исход не определён (сигналов нет).
pub const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(5);
// Вложения-картинки: крышка canterlot, предел самого Codex открыт.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

// Действующие настройки хода в словаре canterlot.
pub struct CurrentSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: PermissionMode,
}

// Запись model/list для проверки настроек.
pub struct ModelEntry {
    pub id: String,
    pub title: String,
    pub efforts: Vec<String>,
    pub images: bool,
    pub default: bool,
}

pub fn model_entries(result: &Value) -> Vec<ModelEntry> {
    result
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .map(|model| {
                    let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
                    ModelEntry {
                        id: id.to_owned(),
                        title: model
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                        efforts: model
                            .get("efforts")
                            .and_then(Value::as_array)
                            .map(|efforts| {
                                efforts
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                        images: model
                            .get("inputModalities")
                            .and_then(Value::as_array)
                            .is_some_and(|modalities| {
                                modalities.iter().any(|m| m.as_str() == Some("image"))
                            }),
                        default: model
                            .get("isDefault")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// Проверка смены настроек до отправки (для `validate_transmission`).
pub fn validate_settings(
    change: &SettingsChange,
    models: &[ModelEntry],
) -> Result<(), &'static str> {
    if let Some(mode) = change.permission_mode.as_deref()
        && PermissionMode::parse(mode).is_err()
    {
        return Err("settings_unavailable");
    }
    if let Some(id) = change.model.as_deref() {
        let entry = models
            .iter()
            .find(|entry| entry.id == id)
            .ok_or("settings_unavailable")?;
        if let Some(effort) = change.effort.as_deref()
            && !entry.efforts.iter().any(|known| known == effort)
        {
            return Err("settings_unavailable");
        }
    } else if let Some(effort) = change.effort.as_deref() {
        // Усилие без смены модели относится к модели по умолчанию.
        let entry = models
            .iter()
            .find(|entry| entry.default)
            .ok_or("settings_unavailable")?;
        if !entry.efforts.iter().any(|known| known == effort) {
            return Err("settings_unavailable");
        }
    }
    Ok(())
}

// Собранный запрос turn/start: параметры, команда и её вложения.
pub struct TurnRequest {
    pub params: Value,
    pub command_id: Uuid,
    pub files: Vec<LocalFile>,
}

impl fmt::Debug for TurnRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnRequest")
            .field("command_id", &self.command_id)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

pub fn build_turn_request(
    thread_id: &str,
    t: &Transmission,
    current: &CurrentSettings,
    owner: &OwnerDefaults,
    note: Option<&str>,
) -> std::io::Result<TurnRequest> {
    Ok(TurnRequest {
        params: build_turn_start(thread_id, t, current, owner, note)?,
        command_id: t.command_id,
        files: t.files.clone(),
    })
}

pub fn build_turn_start(
    thread_id: &str,
    t: &Transmission,
    current: &CurrentSettings,
    owner: &OwnerDefaults,
    note: Option<&str>,
) -> std::io::Result<Value> {
    let mut parts = Vec::new();
    if let Some(note) = note {
        parts.push(note.to_owned());
    }
    if !t.text.is_empty() {
        parts.push(t.text.clone());
    }
    if !t.files.is_empty() {
        parts.push(attachment_lines(&t.files)?);
    }
    let text = parts.join("\n\n");
    if text.len() > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES {
        return Err(std::io::Error::other("input_too_long"));
    }
    let mut input = vec![json!({"type": "text", "text": text, "text_elements": []})];
    for file in &t.files {
        if is_local_image(file) {
            input.push(json!({"type": "localImage", "path": file.path.to_string_lossy()}));
        }
    }
    let mut params = serde_json::Map::new();
    params.insert("threadId".to_owned(), thread_id.into());
    params.insert(
        "clientUserMessageId".to_owned(),
        t.command_id.to_string().into(),
    );
    params.insert("input".to_owned(), Value::Array(input));
    if let Some(effort) = t
        .settings
        .as_ref()
        .and_then(|change| change.effort.clone())
        .or_else(|| current.effort.clone())
    {
        params.insert("effort".to_owned(), effort.into());
    }
    if let Some(model) = t.settings.as_ref().and_then(|change| change.model.clone()) {
        params.insert("model".to_owned(), model.into());
    }
    if let Some(mode) = t
        .settings
        .as_ref()
        .and_then(|change| change.permission_mode.as_deref())
    {
        let mode =
            PermissionMode::parse(mode).map_err(|_| std::io::Error::other("unknown_mode"))?;
        let (approval, sandbox) = turn_policy(mode, owner);
        if let Some(approval) = approval {
            params.insert("approvalPolicy".to_owned(), approval);
        }
        if let Some(sandbox) = sandbox {
            params.insert("sandboxPolicy".to_owned(), sandbox);
        }
        params.insert("approvalsReviewer".to_owned(), "user".into());
    }
    Ok(Value::Object(params))
}

fn is_local_image(file: &LocalFile) -> bool {
    file.image && IMAGE_MIMES.contains(&file.mime.as_str()) && file.size <= MAX_IMAGE_BYTES
}

// Файлового ввода у Codex нет: путь в тексте, агент читает сам.
fn attachment_lines(files: &[LocalFile]) -> std::io::Result<String> {
    let mut list = String::new();
    for file in files {
        let path = std::path::absolute(&file.path)?;
        if !list.is_empty() {
            list.push('\n');
        }
        list.push_str(&format!(
            "{} ({}, {} bytes): {}",
            file.name,
            file.mime,
            file.size,
            path.display()
        ));
    }
    Ok(list)
}

// Ошибка JSON-RPC на turn/start — отказ с обрезанным сообщением.
pub fn error_rejection(error: &RpcError) -> String {
    error.message.chars().take(128).collect()
}

// Параметры thread/turns/list для проверки повтора (новые — сверху).
pub fn history_lookup_params(thread_id: &str) -> Value {
    json!({"threadId": thread_id, "itemsView": "full", "sortDirection": "desc", "limit": 20})
}

// Ход, где ввод с этим clientId уже принят.
pub fn history_contains(result: &Value, client_id: &str) -> Option<String> {
    let turns = result.get("data")?.as_array()?;
    turns.iter().find_map(|turn| {
        let items = turn.get("items")?.as_array()?;
        let found = items
            .iter()
            .any(|item| item.get("clientId").and_then(Value::as_str) == Some(client_id));
        if found {
            turn.get("id")?.as_str().map(str::to_owned)
        } else {
            None
        }
    })
}

// Пометка недельного порога: не чаще раза на порог за исполнение.
pub fn threshold_note(
    weekly: Option<(u16, Option<u64>)>,
    last_noted: &mut Option<u16>,
) -> Option<String> {
    let (threshold, text) = limit_note::next_threshold(weekly, last_noted.unwrap_or(0), "Codex")?;
    *last_noted = Some(threshold);
    Some(text)
}

// Активный и виденные ходы: строгий FIFO и ловля слияния ходов.
pub struct ActiveTurn {
    pub turn_id: String,
    pub command_id: Uuid,
}

pub struct Outbox {
    active: Option<ActiveTurn>,
    known_turns: HashSet<String>,
}

impl fmt::Debug for Outbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outbox")
            .field("has_active", &self.active.is_some())
            .field("known_turns", &self.known_turns.len())
            .finish_non_exhaustive()
    }
}

impl Outbox {
    pub fn new() -> Self {
        Self {
            active: None,
            known_turns: HashSet::new(),
        }
    }

    pub fn active(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }

    // Ответ turn/start: знакомый turn.id значит слияние — очередь останавливается.
    // `Some` — диагностика `turn_merged`, `None` — ход принят и активен.
    pub fn on_turn_response(&mut self, command_id: Uuid, turn_id: &str) -> Option<AdapterEvent> {
        if !self.known_turns.insert(turn_id.to_owned()) {
            return Some(AdapterEvent::Diagnostic {
                kind: "turn_merged",
                event_type: "turn_merged".into(),
                required: true,
            });
        }
        self.active = Some(ActiveTurn {
            turn_id: turn_id.to_owned(),
            command_id,
        });
        None
    }

    pub fn on_turn_started(&mut self, turn_id: &str) {
        self.known_turns.insert(turn_id.to_owned());
    }

    // Завершение главного хода гасит активный; чужой ход не трогает.
    pub fn on_turn_completed(&mut self, turn_id: &str) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|turn| turn.turn_id == turn_id)
        {
            self.active = None;
            true
        } else {
            false
        }
    }
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

pub fn interrupt_params(thread_id: &str, turn_id: &str) -> Value {
    json!({"threadId": thread_id, "turnId": turn_id})
}

pub fn background_terminals_list_params(thread_id: &str) -> Value {
    json!({"threadId": thread_id})
}

pub fn background_terminal_terminate_params(thread_id: &str, process_id: &str) -> Value {
    json!({"threadId": thread_id, "processId": process_id})
}

// processId терминалов, чьи itemId из прерванного хода; чужие фонды живут дальше.
pub fn terminals_to_terminate(list: &Value, turn_items: &HashSet<String>) -> Vec<String> {
    list.get("data")
        .and_then(Value::as_array)
        .map(|terminals| {
            terminals
                .iter()
                .filter(|terminal| {
                    terminal
                        .get("itemId")
                        .and_then(Value::as_str)
                        .is_some_and(|item| turn_items.contains(item))
                })
                .filter_map(|terminal| {
                    terminal
                        .get("processId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}
