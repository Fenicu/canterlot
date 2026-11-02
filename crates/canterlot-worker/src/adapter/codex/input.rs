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
#[derive(Clone, Debug)]
pub struct ModelEntry {
    pub id: String,
    pub title: String,
    pub efforts: Vec<String>,
    pub images: bool,
    pub default: bool,
}

// Страница ответа model/list: `data` по схеме ModelListResponse.
pub fn model_entries(result: &Value) -> Vec<ModelEntry> {
    result
        .get("data")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .map(|model| {
                    let id = model.get("id").and_then(Value::as_str).unwrap_or_default();
                    ModelEntry {
                        id: id.to_owned(),
                        title: model
                            .get("displayName")
                            .or_else(|| model.get("description"))
                            .and_then(Value::as_str)
                            .unwrap_or(id)
                            .to_owned(),
                        efforts: model_efforts(model),
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

// Курсор следующей страницы model/list; пустой или null — страниц больше нет.
pub fn next_cursor(result: &Value) -> Option<String> {
    result
        .get("nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_owned)
}

// Параметры model/list: страница по курсору.
pub fn model_list_params(cursor: Option<&str>) -> Value {
    let mut params = json!({"includeHidden": false, "limit": 100});
    if let Some(cursor) = cursor {
        params["cursor"] = cursor.into();
    }
    params
}

// Предел страниц model/list: защита от зацикленного курсора.
pub const MODEL_LIST_MAX_PAGES: usize = 20;

// `efforts` в краткой сводке или `supportedReasoningEfforts[].reasoningEffort` в схеме.
fn model_efforts(model: &Value) -> Vec<String> {
    if let Some(efforts) = model.get("efforts").and_then(Value::as_array) {
        return efforts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .as_str()
                        .or_else(|| entry.get("reasoningEffort").and_then(Value::as_str))
                })
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

// Проверка смены настроек до отправки (для `validate_transmission`).
pub fn validate_settings(
    change: &SettingsChange,
    models: &[ModelEntry],
    current_model: Option<&str>,
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
        // Усилие без смены модели относится к текущей модели сессии.
        let entry = current_model
            .and_then(|current| models.iter().find(|entry| entry.id == current))
            .or_else(|| models.iter().find(|entry| entry.default))
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

/// Параметры `turn/steer`: тот же текст и картинки, что у `turn/start`, но без
/// ходовых переопределений — steer их не принимает. `clientUserMessageId`
/// возвращается эхом как `clientId` элемента `userMessage` и служит
/// доказательством, как у обычного хода.
pub fn build_turn_steer(
    thread_id: &str,
    expected_turn_id: &str,
    t: &Transmission,
) -> std::io::Result<Value> {
    let mut parts = Vec::new();
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
    Ok(json!({
        "threadId": thread_id,
        "expectedTurnId": expected_turn_id,
        "clientUserMessageId": t.command_id.to_string(),
        "input": input,
    }))
}

// Файлового ввода у Codex нет: путь в тексте, агент читает сам.
pub(crate) fn attachment_lines(files: &[LocalFile]) -> std::io::Result<String> {
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

// Id хода из ответа turn/start.
pub fn started_turn_id(result: &Value) -> Option<String> {
    result
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
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
    history_turn(result, client_id).map(|(id, _)| id)
}

// Ход с этим clientId и его `status` (completed|interrupted|failed|inProgress).
pub fn history_turn(result: &Value, client_id: &str) -> Option<(String, Option<String>)> {
    let turns = result.get("data")?.as_array()?;
    turns.iter().find_map(|turn| {
        let items = turn.get("items")?.as_array()?;
        let found = items
            .iter()
            .any(|item| item.get("clientId").and_then(Value::as_str) == Some(client_id));
        if found {
            let id = turn.get("id")?.as_str()?.to_owned();
            let status = turn
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some((id, status))
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
#[derive(Clone)]
pub struct ActiveTurn {
    pub turn_id: String,
    pub command_id: Uuid,
}

pub struct Outbox {
    active: Option<ActiveTurn>,
    // Ходы из ответов turn/start: повтор такого id и есть слияние.
    known_turns: HashSet<String>,
    // Ходы из turn/started: порядок с ответом схема не задаёт, слиянием не считаются.
    announced_turns: HashSet<String>,
    adopted: Option<String>,
}

impl fmt::Debug for Outbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Outbox")
            .field("has_active", &self.active.is_some())
            .field("known_turns", &self.known_turns.len())
            .field("announced_turns", &self.announced_turns.len())
            .finish_non_exhaustive()
    }
}

impl Outbox {
    pub fn new() -> Self {
        Self {
            active: None,
            known_turns: HashSet::new(),
            announced_turns: HashSet::new(),
            adopted: None,
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
                field_names: Vec::new(),
            });
        }
        self.active = Some(ActiveTurn {
            turn_id: turn_id.to_owned(),
            command_id,
        });
        None
    }

    // Ход, о котором этот процесс сообщал (ответ turn/start или turn/started).
    pub fn knows(&self, turn_id: &str) -> bool {
        self.known_turns.contains(turn_id) || self.announced_turns.contains(turn_id)
    }

    pub fn on_turn_started(&mut self, turn_id: &str) {
        self.announced_turns.insert(turn_id.to_owned());
    }

    // turn/started раньше ответа turn/start: ход активен, ответ только подтвердит его.
    pub fn adopt(&mut self, turn_id: &str, command_id: Uuid) {
        self.adopted = Some(turn_id.to_owned());
        self.active = Some(ActiveTurn {
            turn_id: turn_id.to_owned(),
            command_id,
        });
    }

    // Ход принят по turn/started и уже кончился: ответ turn/start ничего не решает.
    pub fn adopted_turn_ended(&self) -> bool {
        self.adopted.is_some() && self.active.is_none()
    }

    // Ответ подтвердил принятый ход (он мог уже и закончиться).
    pub fn take_adopted(&mut self, turn_id: &str) -> bool {
        let adopted = self.adopted.take().is_some_and(|id| id == turn_id);
        if adopted {
            self.known_turns.insert(turn_id.to_owned());
        }
        adopted
    }

    // Привязка к живому ходу после проверки истории: без диагностики слияния.
    pub fn attach(&mut self, turn_id: &str, command_id: Uuid) {
        self.known_turns.insert(turn_id.to_owned());
        self.active = Some(ActiveTurn {
            turn_id: turn_id.to_owned(),
            command_id,
        });
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
