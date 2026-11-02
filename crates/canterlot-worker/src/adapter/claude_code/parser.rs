//! Stream-JSON CLI output: NDJSON frames, tool input assembler, phases,
//! per-model usage deltas and subagent attribution.
//!
//! Never panics on unknown input and never ends the session: known frames
//! without a reaction are `Ignored`, unknown ones leave one non-required
//! diagnostic per name, lines that are not JSON become an `unparsed_line`
//! diagnostic. Partial tool JSON stays buffered until `content_block_stop`.

use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

use super::super::frame_names::object_field_names;
use super::super::labels::{command_label, cut_end, path_tail};
use super::super::{
    AdapterEvent, BackgroundKind, BackgroundState, BackgroundTask, BlockKind, ModelUsage, Phase,
    TodoItem, TodoStatus,
};

/// Summary of a tool call: tool name plus the identifying argument.
const TOOL_CALL_SUMMARY_LIMIT: usize = 120;

/// Сводка результата, завершившегося ошибкой: у результата своей сводки нет, отметка
/// доходит до ленты без новых полей в протоколе, журнале и истории.
const TOOL_FAILED_SUMMARY: &str = "error";

/// Сводка результата запуска фоновой задачи: настоящий результат придёт позже. Хаб и лента
/// не считают такой блок результатом вызова.
const TOOL_LAUNCHED_SUMMARY: &str = "async_launched";

/// Потолки диагностик неизвестных кадров те же, что у OpenCode.
const MAX_UNKNOWN_TYPES: usize = 64;
const MAX_TYPE_CHARS: usize = 64;

const IGNORED_SYSTEM: [&str; 5] = [
    "api_retry",
    "task_progress",
    "task_updated",
    "thinking_tokens",
    "commands_changed",
];
const IGNORED_STREAM_EVENTS: [&str; 1] = ["message_stop"];
const IGNORED_DELTAS: [&str; 1] = ["signature_delta"];
const IGNORED_USER_BLOCKS: [&str; 2] = ["text", "image"];

/// One open content block, keyed by stream index.
struct OpenBlock {
    block_id: Uuid,
    kind: BlockKind,
    text: String,
    tool_use_id: Option<String>,
    tool_name: Option<String>,
}

/// Buffered `input_json_delta` fragments of one tool call.
struct OpenInput {
    id: String,
    buf: String,
}

struct AssembledInput {
    input: Result<serde_json::Value, String>,
    raw: String,
}

/// Tool input assembler: `input_json_delta` fragments are buffered until
/// `content_block_stop`; only then is the object parsed once.
#[derive(Default)]
struct ToolInputAssembler {
    open: HashMap<u64, OpenInput>,
}

impl ToolInputAssembler {
    fn start(&mut self, index: u64, id: &str) {
        self.open.insert(
            index,
            OpenInput {
                id: id.to_owned(),
                buf: String::new(),
            },
        );
    }

    fn push(&mut self, index: u64, fragment: &str) {
        if let Some(open) = self.open.get_mut(&index) {
            open.buf.push_str(fragment);
        }
    }

    fn stop(&mut self, index: u64) -> Option<(String, AssembledInput)> {
        let open = self.open.remove(&index)?;
        let input = if open.buf.is_empty() {
            Ok(serde_json::Value::Object(Default::default()))
        } else {
            serde_json::from_str(&open.buf).map_err(|error| error.to_string())
        };
        Some((
            open.id,
            AssembledInput {
                input,
                raw: open.buf,
            },
        ))
    }
}

/// Last-message usage of the main agent, for context accounting.
#[derive(Default, Clone, Copy)]
struct MessageUsage {
    input: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Шаг живого расхода контекста между записями `ContextProgress`.
const CONTEXT_PROGRESS_STEP: u64 = 10_000;

/// Фоновая задача CLI. Ключ — `task_id`: завершившийся субагент может запуститься снова
/// с тем же `task_id` и новым `run_id`, последнее событие главнее.
struct TrackedTask {
    kind: BackgroundKind,
    parent_task_id: Option<String>,
    description: String,
    state: BackgroundState,
    /// Конец ещё не сообщён хабу.
    ended_unsent: bool,
}

/// Квитанция прерывания: снимок очереди CLI, взятый в момент обработки
/// управляющего запроса (приходит раньше `result` прерванного хода).
/// Итог хода читают вместе с ней, очередь после итога уже другая.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InterruptReceipt {
    pub still_queued: Vec<Uuid>,
    pub cancelled: Vec<Uuid>,
}

/// Parsed output of one NDJSON line; several events may come out of order.
pub enum Parsed {
    Event(AdapterEvent),
    Init {
        session_id: String,
        model: String,
        permission_mode: String,
        version: String,
    },
    AnswerFrame {
        user_message_uuids: Vec<Uuid>,
    },
    Replay {
        uuid: Uuid,
    },
    /// Недокументированный, но записанный кадр CLI о судьбе нашего UUID: `queued`, `started`,
    /// `completed` или `cancelled`. Раньше первого кадра модели доказывает приём.
    CommandLifecycle {
        command_id: Uuid,
        state: String,
    },
    ToolCallComplete {
        block_id: Uuid,
        tool_use_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        is_error: bool,
    },
    RateLimit(serde_json::Value),
    TurnError {
        status: Option<u16>,
        error: Option<String>,
        terminal_reason: Option<String>,
    },
    ControlResponse {
        request_id: String,
        success: bool,
        /// Квитанция `interrupt`: снимок очереди CLI в момент прерывания
        /// (`still_queued`/`cancelled`), только у успешных ответов на прерывание.
        interrupt: Option<InterruptReceipt>,
    },
    /// Служебный итог `/compact` потреблён без открытого хода: команда сжатия закрыта.
    CompactResultConsumed {
        ok: bool,
    },
    Ignored,
}

impl std::fmt::Debug for Parsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event(event) => f.debug_tuple("Event").field(event).finish(),
            Self::Init {
                session_id,
                model,
                permission_mode,
                version,
            } => f
                .debug_struct("Init")
                .field("session_id", session_id)
                .field("model", model)
                .field("permission_mode", permission_mode)
                .field("version", version)
                .finish(),
            Self::AnswerFrame { user_message_uuids } => f
                .debug_struct("AnswerFrame")
                .field("user_message_uuids", user_message_uuids)
                .finish(),
            Self::Replay { uuid } => f.debug_struct("Replay").field("uuid", uuid).finish(),
            Self::CommandLifecycle { command_id, state } => f
                .debug_struct("CommandLifecycle")
                .field("command_id", command_id)
                .field("state", state)
                .finish(),
            Self::ToolCallComplete {
                block_id,
                tool_use_id,
                tool_name,
                ..
            } => f
                .debug_struct("ToolCallComplete")
                .field("block_id", block_id)
                .field("tool_use_id", tool_use_id)
                .field("tool_name", tool_name)
                .finish(),
            Self::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => f
                .debug_struct("ToolResult")
                .field("tool_use_id", tool_use_id)
                .field("is_error", is_error)
                .finish(),
            Self::RateLimit(_) => f.write_str("RateLimit"),
            Self::TurnError {
                status,
                terminal_reason,
                ..
            } => f
                .debug_struct("TurnError")
                .field("status", status)
                .field("terminal_reason", terminal_reason)
                .finish(),
            Self::ControlResponse {
                request_id,
                success,
                interrupt,
            } => f
                .debug_struct("ControlResponse")
                .field("request_id", request_id)
                .field("success", success)
                .field("interrupt", interrupt)
                .finish(),
            Self::CompactResultConsumed { ok } => f
                .debug_struct("CompactResultConsumed")
                .field("ok", ok)
                .finish(),
            Self::Ignored => f.write_str("Ignored"),
        }
    }
}

/// Незнакомое состояние не должно терять пункт: он показывается как ожидающий.
fn todo_item(todo: &serde_json::Value) -> Option<TodoItem> {
    Some(TodoItem {
        text: todo.get("content")?.as_str()?.to_owned(),
        status: match todo.get("status").and_then(serde_json::Value::as_str) {
            Some("in_progress") => TodoStatus::InProgress,
            Some("completed") => TodoStatus::Completed,
            _ => TodoStatus::Pending,
        },
    })
}

pub struct Parser {
    input_command: Option<Uuid>,
    session_model: Option<String>,
    current_model: String,
    prev_usage: HashMap<String, ModelUsage>,
    model_agent: HashMap<String, String>,
    blocks: HashMap<u64, OpenBlock>,
    tool_blocks: HashMap<String, Uuid>,
    tool_agents: HashMap<String, String>,
    /// Сообщения субагента, пришедшие потоком: их целый кадр ничего нового не несёт.
    streamed_messages: HashSet<String>,
    assembler: ToolInputAssembler,
    last_usage: Option<MessageUsage>,
    /// Ручное сжатие по кнопке ждёт своей границы: адаптер выставил флаг через `compact`.
    compact_manual: bool,
    /// Сжатие уже объявлено началом: повторный `status=compacting` не дублирует эпизод.
    compact_active: bool,
    /// `/compact`, записанный адаптером по кнопке: только его итог не принадлежит ходу.
    compact_command: Option<Uuid>,
    /// Граница этого `/compact` уже прошла.
    compact_boundary_seen: bool,
    /// Кадр последней границы сжатия: CLI дублирует границу, повторный эпизод не открывает.
    last_compact_boundary: Option<String>,
    /// Живой расход контекста, уже отданный `ContextProgress`: реже журнала, не чаще каждых
    /// `CONTEXT_PROGRESS_STEP` токенов, иначе болтливый ход зальёт журнал и хаб.
    progress_sent: u64,
    /// Имена уже названных неизвестных кадров: на исполнение один раз на имя.
    unknown: HashSet<String>,
    /// Записанная команда ждёт своего `result`.
    command_open: bool,
    /// Пробуждающий ход приписан этой команде: CLI начал его сам.
    wake: Option<Uuid>,
    /// CLI начал ход без ввода, первого кадра модели ещё могло не быть.
    cli_turn: bool,
    background: BTreeMap<String, TrackedTask>,
    /// Вызов каждой задачи: `tool_use_id` есть только в `task_started`, а задача, ушедшая
    /// в фон на ходу, попадает в список позже него.
    task_calls: HashMap<String, String>,
    /// Последнее сообщённое: работающие задачи и признак пробуждающего хода.
    background_sent: (Vec<BackgroundTask>, bool),
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            input_command: None,
            session_model: None,
            current_model: String::new(),
            prev_usage: HashMap::new(),
            model_agent: HashMap::new(),
            blocks: HashMap::new(),
            tool_blocks: HashMap::new(),
            tool_agents: HashMap::new(),
            streamed_messages: HashSet::new(),
            assembler: ToolInputAssembler::default(),
            last_usage: None,
            compact_manual: false,
            compact_active: false,
            compact_command: None,
            compact_boundary_seen: false,
            last_compact_boundary: None,
            progress_sent: 0,
            unknown: HashSet::new(),
            command_open: false,
            wake: None,
            cli_turn: false,
            background: BTreeMap::new(),
            task_calls: HashMap::new(),
            background_sent: (Vec::new(), false),
        }
    }

    /// Неизвестное имя кадра: одна необязательная диагностика на имя, не больше
    /// `MAX_UNKNOWN_TYPES` имён, сессию и ход это не трогает.
    fn note_unknown(
        &mut self,
        out: &mut Vec<Parsed>,
        scope: &str,
        kind: Option<&str>,
        shape: Option<&serde_json::Value>,
    ) {
        let name = match kind.filter(|kind| !kind.is_empty()) {
            Some(kind) if scope.is_empty() => kind.to_owned(),
            Some(kind) => format!("{scope}/{kind}"),
            None => scope.to_owned(),
        };
        let name: String = name.chars().take(MAX_TYPE_CHARS).collect();
        if self.unknown.len() >= MAX_UNKNOWN_TYPES || !self.unknown.insert(name.clone()) {
            return;
        }
        out.push(Parsed::Event(AdapterEvent::Diagnostic {
            kind: "unknown_event",
            event_type: name,
            required: false,
            field_names: shape.map(object_field_names).unwrap_or_default(),
        }));
    }

    pub fn set_input_command(&mut self, id: Uuid) {
        self.input_command = Some(id);
        self.command_open = true;
    }

    /// Кнопка «Сжать контекст» отправила `/compact`: ближайшая граница — ручная.
    pub fn mark_compact_manual(&mut self, command: Uuid) {
        self.compact_manual = true;
        self.compact_command = Some(command);
        self.compact_boundary_seen = false;
    }

    /// Эпизод сжатия начат статусом и ещё не закрыт границей или отказом.
    pub fn compaction_open(&self) -> bool {
        self.compact_active
    }

    /// Адаптер больше не ждёт `/compact`: его поздний итог идёт обычным путём.
    pub fn forget_compact(&mut self) {
        self.compact_manual = false;
        self.compact_command = None;
        self.compact_boundary_seen = false;
    }

    /// CLI занят ходом, который начал сам: ввод ему не нужен, перезапуск его оборвал бы.
    pub fn wake_turn_open(&self) -> bool {
        self.wake.is_some() || self.cli_turn
    }

    /// Ход без ввода идёт от модели: для сопровождения это ход последней команды. Кадр
    /// `system/init` для этого не годится: его одного мало, чтобы держать очередь.
    fn adopt_wake_turn(&mut self, out: &mut Vec<Parsed>) {
        if self.command_open || self.wake.is_some() {
            return;
        }
        let Some(command) = self.input_command else {
            return;
        };
        self.wake = Some(command);
        out.push(Parsed::Event(AdapterEvent::RunAdopted {
            input_command_id: command,
        }));
        self.report_background(out);
    }

    /// Сколько фоновых задач работает по последнему списку CLI.
    pub fn background_running(&self) -> usize {
        self.background
            .values()
            .filter(|task| task.state == BackgroundState::Running)
            .count()
    }

    fn background_view(&self, task_id: &str, task: &TrackedTask) -> BackgroundTask {
        BackgroundTask {
            task_id: task_id.to_owned(),
            kind: task.kind,
            block_id: self
                .task_calls
                .get(task_id)
                .and_then(|call| self.tool_blocks.get(call))
                .copied(),
            parent_task_id: task.parent_task_id.clone(),
            description: task.description.clone(),
            state: task.state,
        }
    }

    fn report_background(&mut self, out: &mut Vec<Parsed>) {
        let tasks: Vec<BackgroundTask> = self
            .background
            .iter()
            .filter(|(_, task)| task.state == BackgroundState::Running || task.ended_unsent)
            .map(|(id, task)| self.background_view(id, task))
            .collect();
        let running: Vec<BackgroundTask> = tasks
            .iter()
            .filter(|task| task.state == BackgroundState::Running)
            .cloned()
            .collect();
        let wake_turn = self.wake.is_some();
        let ended = tasks.len() != running.len();
        if !ended && self.background_sent == (running.clone(), wake_turn) {
            return;
        }
        for task in self.background.values_mut() {
            task.ended_unsent = false;
        }
        self.background_sent = (running, wake_turn);
        out.push(Parsed::Event(AdapterEvent::BackgroundWork {
            tasks,
            wake_turn,
        }));
    }

    /// Полный список CLI. Задача, пропавшая из него без уведомления, остановлена;
    /// уведомление, если придёт позже, уточнит состояние.
    fn feed_background_list(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let list = value
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut listed = HashSet::new();
        for task in list {
            let Some(id) = task.get("task_id").and_then(serde_json::Value::as_str) else {
                continue;
            };
            listed.insert(id.to_owned());
            self.background.insert(
                id.to_owned(),
                TrackedTask {
                    kind: task_kind(task),
                    parent_task_id: task
                        .get("parent_task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    description: task
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    state: BackgroundState::Running,
                    ended_unsent: false,
                },
            );
        }
        for (id, task) in &mut self.background {
            if task.state == BackgroundState::Running && !listed.contains(id) {
                task.state = BackgroundState::Stopped;
                task.ended_unsent = true;
            }
        }
        self.report_background(out);
    }

    /// `task_started` есть и у синхронного субагента, и у обычной команды: в набор идёт
    /// только `is_backgrounded`, в том числе задача субагента, её CLI тоже считает.
    fn feed_task_started(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let text = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
        let Some(id) = text("task_id") else {
            return;
        };
        if let Some(call) = text("tool_use_id") {
            self.task_calls.insert(id.to_owned(), call.to_owned());
        }
        if value
            .get("is_backgrounded")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return;
        }
        let task = TrackedTask {
            kind: task_kind(value),
            parent_task_id: text("parent_task_id").map(str::to_owned),
            description: text("description").unwrap_or_default().to_owned(),
            state: BackgroundState::Running,
            ended_unsent: false,
        };
        self.background.insert(id.to_owned(), task);
        self.report_background(out);
    }

    fn feed_task_notification(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let Some(task) = value
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| self.background.get_mut(id))
        else {
            return;
        };
        task.state = match value.get("status").and_then(serde_json::Value::as_str) {
            Some("completed") => BackgroundState::Completed,
            Some("failed") => BackgroundState::Failed,
            _ => BackgroundState::Stopped,
        };
        task.ended_unsent = true;
        self.report_background(out);
    }

    pub fn reset_process(&mut self) {
        self.command_open = false;
        self.wake = None;
        self.cli_turn = false;
        self.compact_manual = false;
        self.compact_active = false;
        self.compact_command = None;
        self.compact_boundary_seen = false;
        self.progress_sent = 0;
        self.background.clear();
        self.task_calls.clear();
        self.background_sent = (Vec::new(), false);
        self.prev_usage.clear();
        self.tool_blocks.clear();
        self.tool_agents.clear();
        self.streamed_messages.clear();
        self.blocks.clear();
    }

    fn agent_of(&self, parent_tool_use_id: Option<&str>) -> (String, Option<Uuid>) {
        match parent_tool_use_id {
            Some(id) => {
                let agent = self
                    .tool_agents
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "subagent".to_owned());
                (agent, self.tool_blocks.get(id).copied())
            }
            None => ("orchestrator".to_owned(), None),
        }
    }

    fn answer_frame(value: &serde_json::Value) -> Option<Parsed> {
        let mut uuids: Vec<Uuid> = value
            .get("user_message_uuids")
            .and_then(serde_json::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(|s| Uuid::parse_str(s).ok())
                    .collect()
            })
            .unwrap_or_default();
        if uuids.is_empty()
            && let Some(id) = value
                .get("user_message_uuid")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
        {
            uuids.push(id);
        }
        if uuids.is_empty() {
            None
        } else {
            Some(Parsed::AnswerFrame {
                user_message_uuids: uuids,
            })
        }
    }

    fn note_model(&mut self, model: Option<&str>, parent_tool_use_id: Option<&str>) {
        // `<synthetic>` — кадры самого CLI (ошибки API, вывод локальных команд), не модель.
        let Some(model) = model.filter(|m| !m.is_empty() && *m != "<synthetic>") else {
            return;
        };
        self.current_model = model.to_owned();
        match parent_tool_use_id {
            Some(id) => {
                self.model_agent.entry(model.to_owned()).or_insert_with(|| {
                    self.tool_agents
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| "subagent".to_owned())
                });
            }
            None => {
                self.session_model.get_or_insert_with(|| model.to_owned());
            }
        }
    }

    fn note_usage(
        &mut self,
        usage: &serde_json::Value,
        parent_tool_use_id: Option<&str>,
        out: &mut Vec<Parsed>,
    ) {
        if parent_tool_use_id.is_some() {
            return;
        }
        let get = |key: &str| {
            usage
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let total = get("input_tokens")
            + get("cache_read_input_tokens")
            + get("cache_creation_input_tokens");
        self.last_usage = Some(MessageUsage {
            input: get("input_tokens"),
            cache_read: get("cache_read_input_tokens"),
            cache_write: get("cache_creation_input_tokens"),
        });
        // Живой расход в ходу: та же сумма, что в `RunFinished`, но реже журнала.
        if total >= self.progress_sent + CONTEXT_PROGRESS_STEP {
            self.progress_sent = total;
            out.push(Parsed::Event(AdapterEvent::ContextProgress {
                context_tokens: total,
                context_window_tokens: 0,
            }));
        }
    }

    fn start_text_block(
        &mut self,
        out: &mut Vec<Parsed>,
        index: u64,
        kind: BlockKind,
        parent_tool_use_id: Option<&str>,
    ) {
        let (agent, parent) = self.agent_of(parent_tool_use_id);
        let block_id = Uuid::now_v7();
        self.blocks.insert(
            index,
            OpenBlock {
                block_id,
                kind: kind.clone(),
                text: String::new(),
                tool_use_id: None,
                tool_name: None,
            },
        );
        out.push(Parsed::Event(AdapterEvent::BlockStarted {
            block_id,
            kind,
            input_command_id: self.input_command,
            parent,
            agent,
            model: self.current_model.clone(),
            tool_name: None,
            summary: None,
            approved_by: None,
        }));
    }

    pub fn feed(&mut self, line: &str) -> Vec<Parsed> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return vec![Parsed::Event(AdapterEvent::Diagnostic {
                kind: "unparsed_line",
                event_type: String::new(),
                required: false,
                field_names: Vec::new(),
            })];
        };
        let Some(frame) = value.as_object() else {
            return vec![Parsed::Ignored];
        };
        let mut out = Vec::new();
        match frame.get("type").and_then(serde_json::Value::as_str) {
            Some("system") => self.feed_system(&value, &mut out),
            Some("stream_event") => self.feed_stream(&value, &mut out),
            Some("assistant") => self.feed_assistant(&value, &mut out),
            Some("user") => self.feed_user(&value, &mut out),
            Some("result") => self.feed_result(&value, &mut out),
            Some("rate_limit_event") => {
                if let Some(info) = value.get("rate_limit_info").cloned() {
                    out.push(Parsed::RateLimit(info));
                }
            }
            Some("control_response") => self.feed_control_response(&value, &mut out),
            Some("command_lifecycle") => {
                self.feed_command_lifecycle(&value, &mut out);
            }
            Some(kind) => {
                self.note_unknown(&mut out, "", Some(kind), Some(&value));
                if out.is_empty() {
                    out.push(Parsed::Ignored);
                }
            }
            _ => out.push(Parsed::Ignored),
        }
        out
    }

    fn feed_command_lifecycle(&self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let command_id = value
            .get("command_uuid")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok());
        let state = value
            .get("state")
            .and_then(serde_json::Value::as_str)
            .filter(|state| !state.is_empty());
        match (command_id, state) {
            (Some(command_id), Some(state)) => out.push(Parsed::CommandLifecycle {
                command_id,
                state: state.to_owned(),
            }),
            _ => out.push(Parsed::Ignored),
        }
    }

    fn feed_system(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        match value.get("subtype").and_then(serde_json::Value::as_str) {
            Some("init") => {
                let string = |key: &str| {
                    value
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let model = string("model");
                if !model.is_empty() {
                    self.current_model = model.clone();
                    self.session_model.get_or_insert(model.clone());
                }
                self.cli_turn = !self.command_open
                    && self.compact_command.is_none()
                    && self.input_command.is_some();
                out.push(Parsed::Init {
                    session_id: string("session_id"),
                    model,
                    permission_mode: string("permissionMode"),
                    version: string("claude_code_version"),
                });
            }
            Some("status") => {
                if value.get("status").and_then(serde_json::Value::as_str) == Some("compacting") {
                    out.push(Parsed::Event(AdapterEvent::Phase {
                        phase: Phase::ContextMaintenance,
                        tool_name: None,
                        summary: None,
                    }));
                    // Повторный статус одного сжатия эпизод не дублирует.
                    if !self.compact_active {
                        self.compact_active = true;
                        out.push(Parsed::Event(AdapterEvent::CompactionStarted {
                            manual: self.compact_manual,
                        }));
                    }
                } else {
                    out.push(Parsed::Ignored);
                }
            }
            Some("compact_boundary") => {
                let uuid = value
                    .get("uuid")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                // Повторный кадр той же границы эпизод не дублирует.
                if !uuid.is_empty() && self.last_compact_boundary.as_deref() == Some(uuid) {
                    out.push(Parsed::Ignored);
                    return;
                }
                self.last_compact_boundary = Some(uuid.to_owned());
                let meta = value
                    .get("compact_metadata")
                    .unwrap_or(&serde_json::Value::Null);
                let manual = self.compact_manual
                    || meta.get("trigger").and_then(serde_json::Value::as_str) == Some("manual");
                let pre_tokens = meta
                    .get("pre_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let post_tokens = meta
                    .get("post_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                self.compact_active = false;
                self.compact_manual = false;
                // Автоматическая граница итог хода не трогает: он остаётся ходу.
                if self.compact_command.is_some() {
                    self.compact_boundary_seen = true;
                }
                // После сжатия расход начинается заново: старое значение не должно
                // пережить границу ни в ходу, ни в индикаторе.
                self.last_usage = None;
                self.progress_sent = 0;
                out.push(Parsed::Event(AdapterEvent::CompactionFinished {
                    manual,
                    ok: true,
                    pre_tokens,
                    post_tokens,
                }));
            }
            Some("background_tasks_changed") => {
                self.feed_background_list(value, out);
                if out.is_empty() {
                    out.push(Parsed::Ignored);
                }
            }
            Some("task_started") => {
                self.feed_task_started(value, out);
                if out.is_empty() {
                    out.push(Parsed::Ignored);
                }
            }
            Some("task_notification") => {
                self.feed_task_notification(value, out);
                if out.is_empty() {
                    out.push(Parsed::Ignored);
                }
            }
            Some(kind) if !IGNORED_SYSTEM.contains(&kind) => {
                self.note_unknown(out, "system", Some(kind), Some(value));
                if out.is_empty() {
                    out.push(Parsed::Ignored);
                }
            }
            _ => out.push(Parsed::Ignored),
        }
    }

    fn feed_stream(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let parent = value
            .get("parent_tool_use_id")
            .and_then(serde_json::Value::as_str);
        let event = value.get("event");
        let event_type = event
            .and_then(|event| event.get("type"))
            .and_then(serde_json::Value::as_str);
        match event_type {
            Some("message_start") => {
                // Поток модели во время `/compact` — само сжатие, а не пробуждающий ход.
                if parent.is_none() && self.compact_command.is_none() {
                    self.adopt_wake_turn(out);
                }
                let message = event.and_then(|event| event.get("message"));
                if parent.is_some()
                    && let Some(id) = message
                        .and_then(|message| message.get("id"))
                        .and_then(serde_json::Value::as_str)
                {
                    self.streamed_messages.insert(id.to_owned());
                }
                self.note_model(
                    message
                        .and_then(|message| message.get("model"))
                        .and_then(serde_json::Value::as_str),
                    parent,
                );
                if let Some(usage) = message.and_then(|message| message.get("usage")) {
                    self.note_usage(usage, parent, out);
                }
                if let Some(answer) = Self::answer_frame(value) {
                    out.push(answer);
                }
                out.push(Parsed::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
            Some("content_block_start") => {
                let block = event.and_then(|event| event.get("content_block"));
                let index = event
                    .and_then(|event| event.get("index"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                match block
                    .and_then(|block| block.get("type"))
                    .and_then(serde_json::Value::as_str)
                {
                    Some("text") => self.start_text_block(out, index, BlockKind::Text, parent),
                    Some("thinking") => {
                        self.start_text_block(out, index, BlockKind::Reasoning, parent)
                    }
                    Some("tool_use") => {
                        let id = block
                            .and_then(|block| block.get("id"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let name = block
                            .and_then(|block| block.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let (agent, parent) = self.agent_of(parent);
                        let block_id = Uuid::now_v7();
                        self.blocks.insert(
                            index,
                            OpenBlock {
                                block_id,
                                kind: BlockKind::ToolCall,
                                text: String::new(),
                                tool_use_id: Some(id.to_owned()),
                                tool_name: Some(name.to_owned()),
                            },
                        );
                        self.tool_blocks.insert(id.to_owned(), block_id);
                        self.assembler.start(index, id);
                        out.push(Parsed::Event(AdapterEvent::BlockStarted {
                            block_id,
                            kind: BlockKind::ToolCall,
                            input_command_id: self.input_command,
                            parent,
                            agent,
                            model: self.current_model.clone(),
                            tool_name: Some(name.to_owned()),
                            summary: None,
                            approved_by: None,
                        }));
                        out.push(Parsed::Event(AdapterEvent::Phase {
                            phase: Phase::RunningTool,
                            tool_name: Some(name.to_owned()),
                            summary: None,
                        }));
                    }
                    other => self.note_unknown(out, "content_block", other, block),
                }
            }
            Some("content_block_delta") => {
                let delta = event.and_then(|event| event.get("delta"));
                let index = event
                    .and_then(|event| event.get("index"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                match delta
                    .and_then(|delta| delta.get("type"))
                    .and_then(serde_json::Value::as_str)
                {
                    Some("text_delta") => {
                        if let Some(text) = delta
                            .and_then(|delta| delta.get("text"))
                            .and_then(serde_json::Value::as_str)
                            && let Some(block) = self.blocks.get_mut(&index)
                            && matches!(block.kind, BlockKind::Text | BlockKind::Reasoning)
                        {
                            block.text.push_str(text);
                            out.push(Parsed::Event(AdapterEvent::BlockDelta {
                                block_id: block.block_id,
                                append: text.to_owned(),
                            }));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta
                            .and_then(|delta| delta.get("thinking"))
                            .and_then(serde_json::Value::as_str)
                            && let Some(block) = self.blocks.get_mut(&index)
                            && matches!(block.kind, BlockKind::Reasoning)
                        {
                            block.text.push_str(text);
                            out.push(Parsed::Event(AdapterEvent::BlockDelta {
                                block_id: block.block_id,
                                append: text.to_owned(),
                            }));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(fragment) = delta
                            .and_then(|delta| delta.get("partial_json"))
                            .and_then(serde_json::Value::as_str)
                        {
                            self.assembler.push(index, fragment);
                        }
                    }
                    Some(kind) if IGNORED_DELTAS.contains(&kind) => {}
                    other => self.note_unknown(out, "delta", other, delta),
                }
            }
            Some("content_block_stop") => {
                let index = event
                    .and_then(|event| event.get("index"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let Some(block) = self.blocks.remove(&index) else {
                    return;
                };
                match block.kind {
                    BlockKind::Text => {
                        out.push(Parsed::Event(AdapterEvent::BlockFinal {
                            block_id: block.block_id,
                            text: block.text,
                            summary: None,
                        }));
                    }
                    BlockKind::Reasoning => {
                        if !block.text.is_empty() {
                            out.push(Parsed::Event(AdapterEvent::BlockFinal {
                                block_id: block.block_id,
                                text: block.text,
                                summary: None,
                            }));
                        }
                    }
                    BlockKind::ToolCall => {
                        let tool_use_id = block.tool_use_id.unwrap_or_default();
                        let tool_name = block.tool_name.unwrap_or_default();
                        if let Some((_, assembled)) = self.assembler.stop(index) {
                            match assembled.input {
                                Ok(input) => self.complete_tool(
                                    out,
                                    block.block_id,
                                    tool_use_id,
                                    tool_name,
                                    input,
                                ),
                                Err(_) => {
                                    out.push(Parsed::Event(AdapterEvent::BlockStarted {
                                        block_id: Uuid::now_v7(),
                                        kind: BlockKind::Error,
                                        input_command_id: self.input_command,
                                        parent: None,
                                        agent: self
                                            .tool_agents
                                            .get(&tool_use_id)
                                            .cloned()
                                            .unwrap_or_else(|| "orchestrator".to_owned()),
                                        model: self.current_model.clone(),
                                        tool_name: Some(tool_name),
                                        summary: Some("invalid tool input".to_owned()),
                                        approved_by: None,
                                    }));
                                    out.push(Parsed::Event(AdapterEvent::BlockFinal {
                                        block_id: block.block_id,
                                        text: assembled.raw,
                                        summary: Some("invalid tool input".to_owned()),
                                    }));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                if let Some(usage) = event.and_then(|event| event.get("usage")) {
                    self.note_usage(usage, parent, out);
                }
            }
            Some(kind) if IGNORED_STREAM_EVENTS.contains(&kind) => {}
            other => self.note_unknown(out, "stream_event", other, event),
        }
    }

    fn complete_tool(
        &mut self,
        out: &mut Vec<Parsed>,
        block_id: Uuid,
        tool_use_id: String,
        tool_name: String,
        input: serde_json::Value,
    ) {
        if tool_name == "Task" || tool_name == "Agent" {
            self.tool_agents
                .insert(tool_use_id.clone(), subagent_name(&input));
        }
        out.push(Parsed::Event(AdapterEvent::BlockFinal {
            block_id,
            text: call_text(&input),
            summary: Some(tool_summary(&tool_name, &input)),
        }));
        out.push(Parsed::ToolCallComplete {
            block_id,
            tool_use_id,
            tool_name,
            input,
        });
    }

    /// Субагент не шлёт потоковых кадров (CLI 2.1.311): его вызовы и текст приходят
    /// только целыми кадрами `assistant` вперемешку с потоком основного агента.
    fn feed_subagent_message(
        &mut self,
        lane: &str,
        message: Option<&serde_json::Value>,
        out: &mut Vec<Parsed>,
    ) {
        let streamed = message
            .and_then(|message| message.get("id"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| self.streamed_messages.contains(id));
        if streamed {
            return;
        }
        let content = message
            .and_then(|message| message.get("content"))
            .and_then(serde_json::Value::as_array);
        for block in content.into_iter().flatten() {
            let text = |key: &str| {
                block
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            };
            match block.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => self.subagent_tool(lane, block, out),
                Some("text") => self.subagent_text(lane, BlockKind::Text, text("text"), out),
                Some("thinking") => {
                    self.subagent_text(lane, BlockKind::Reasoning, text("thinking"), out);
                }
                other => self.note_unknown(out, "assistant_block", other, Some(block)),
            }
        }
    }

    fn subagent_text(&self, lane: &str, kind: BlockKind, text: &str, out: &mut Vec<Parsed>) {
        if text.is_empty() {
            return;
        }
        let (agent, parent) = self.agent_of(Some(lane));
        let block_id = Uuid::now_v7();
        out.push(Parsed::Event(AdapterEvent::BlockStarted {
            block_id,
            kind,
            input_command_id: self.input_command,
            parent,
            agent,
            model: self.current_model.clone(),
            tool_name: None,
            summary: None,
            approved_by: None,
        }));
        out.push(Parsed::Event(AdapterEvent::BlockFinal {
            block_id,
            text: text.to_owned(),
            summary: None,
        }));
    }

    fn subagent_tool(&mut self, lane: &str, block: &serde_json::Value, out: &mut Vec<Parsed>) {
        let string = |key: &str| {
            block
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let (tool_use_id, tool_name) = (string("id"), string("name"));
        if self.tool_blocks.contains_key(&tool_use_id) {
            return;
        }
        let (agent, parent) = self.agent_of(Some(lane));
        let block_id = Uuid::now_v7();
        self.tool_blocks.insert(tool_use_id.clone(), block_id);
        out.push(Parsed::Event(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolCall,
            input_command_id: self.input_command,
            parent,
            agent,
            model: self.current_model.clone(),
            tool_name: Some(tool_name.clone()),
            summary: None,
            approved_by: None,
        }));
        let input = block.get("input").cloned().unwrap_or_default();
        self.complete_tool(out, block_id, tool_use_id, tool_name, input);
    }

    fn feed_assistant(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let parent = value
            .get("parent_tool_use_id")
            .and_then(serde_json::Value::as_str);
        let message = value.get("message");
        self.note_model(
            message
                .and_then(|message| message.get("model"))
                .and_then(serde_json::Value::as_str),
            parent,
        );
        if let Some(usage) = message.and_then(|message| message.get("usage")) {
            self.note_usage(usage, parent, out);
        }
        if let Some(answer) = Self::answer_frame(value) {
            out.push(answer);
        }
        // Ошибка упора несёт `rate_limit_info` отклонённого запроса: из него время сброса.
        if let Some(info) = value
            .get("api_error_params")
            .and_then(|params| params.get("rate_limit_info"))
            .filter(|info| info.is_object())
        {
            out.push(Parsed::RateLimit(info.clone()));
        }
        if let Some(lane) = parent {
            self.feed_subagent_message(lane, message, out);
        }
        // Вывод локальной команды (`/context`, `/cost`) приходит одним кадром
        // `assistant` с `local_command_source` и без потоковых кадров (CLI 2.1.311).
        if value.get("local_command_source").is_some() {
            let texts = message
                .and_then(|message| message.get("content"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                })
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str));
            for text in texts {
                let (agent, parent) = self.agent_of(parent);
                let block_id = Uuid::now_v7();
                out.push(Parsed::Event(AdapterEvent::BlockStarted {
                    block_id,
                    kind: BlockKind::Text,
                    input_command_id: self.input_command,
                    parent,
                    agent,
                    model: self.current_model.clone(),
                    tool_name: None,
                    summary: None,
                    approved_by: None,
                }));
                out.push(Parsed::Event(AdapterEvent::BlockFinal {
                    block_id,
                    text: text.to_owned(),
                    summary: None,
                }));
            }
        }
    }

    fn feed_user(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        if value.get("isReplay").and_then(serde_json::Value::as_bool) == Some(true) {
            if let Some(uuid) = value
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                out.push(Parsed::Replay { uuid });
            }
            return;
        }
        let content = value
            .get("message")
            .and_then(|message| message.get("content"));
        let blocks: Vec<&serde_json::Value> = match content {
            Some(serde_json::Value::Array(blocks)) => blocks.iter().collect(),
            _ => Vec::new(),
        };
        let launched = value.get("tool_use_result").is_some_and(|result| {
            result.get("status").and_then(serde_json::Value::as_str) == Some("async_launched")
                || result
                    .get("backgroundTaskId")
                    .is_some_and(serde_json::Value::is_string)
        });
        for block in blocks {
            let kind = block.get("type").and_then(serde_json::Value::as_str);
            if kind != Some("tool_result") {
                if !kind.is_some_and(|kind| IGNORED_USER_BLOCKS.contains(&kind)) {
                    self.note_unknown(out, "user_block", kind, Some(block));
                }
                continue;
            }
            let tool_use_id = block
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let is_error = block
                .get("is_error")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let content = block.get("content").cloned().unwrap_or_default();
            let text = tool_result_text(&content);
            let parent = self.tool_blocks.get(&tool_use_id).copied();
            let block_id = Uuid::now_v7();
            let (agent, _) = self.agent_of(
                value
                    .get("parent_tool_use_id")
                    .and_then(serde_json::Value::as_str),
            );
            out.push(Parsed::Event(AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolResult,
                input_command_id: self.input_command,
                parent,
                agent,
                model: self.current_model.clone(),
                tool_name: None,
                summary: None,
                approved_by: None,
            }));
            out.push(Parsed::Event(AdapterEvent::BlockFinal {
                block_id,
                text,
                summary: if is_error {
                    Some(TOOL_FAILED_SUMMARY.to_owned())
                } else {
                    launched.then(|| TOOL_LAUNCHED_SUMMARY.to_owned())
                },
            }));
            out.push(Parsed::ToolResult {
                tool_use_id,
                content,
                is_error,
            });
        }
        self.feed_todos(value, out);
    }

    /// `newTodos` есть только у успешного `TodoWrite`, и в нём весь список. Вызовы субагента
    /// приходят без потоковых кадров, поэтому источник один на все дорожки — этот кадр.
    fn feed_todos(&self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let Some(todos) = value
            .get("tool_use_result")
            .and_then(|result| result.get("newTodos"))
            .and_then(serde_json::Value::as_array)
        else {
            return;
        };
        let lane = match value
            .get("parent_tool_use_id")
            .and_then(serde_json::Value::as_str)
        {
            None => None,
            Some(id) => match self.tool_blocks.get(id) {
                Some(block) => Some(*block),
                None => {
                    out.push(Parsed::Event(AdapterEvent::Diagnostic {
                        kind: "todo_lane_unknown",
                        event_type: String::new(),
                        required: false,
                        field_names: Vec::new(),
                    }));
                    return;
                }
            },
        };
        out.push(Parsed::Event(AdapterEvent::TodoUpdated {
            lane,
            items: todos.iter().filter_map(todo_item).collect(),
        }));
    }

    /// `result` пробуждающего хода не заканчивает записанную команду. Такой ход узнаётся по
    /// `origin` или по `user_message_uuids` без этой команды. Сообщение, записанное
    /// раньше его конца, CLI держит у себя, и его собственный `result` идёт следом: тогда
    /// кадр ничего не завершает, а расход достаётся этому `result` как разница счётчиков.
    fn result_command(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) -> Option<Uuid> {
        let foreign = matches!(
            Self::answer_frame(value),
            Some(Parsed::AnswerFrame { user_message_uuids })
                if !self.input_command.is_some_and(|id| user_message_uuids.contains(&id))
        );
        let waking = foreign
            || value
                .pointer("/origin/kind")
                .and_then(serde_json::Value::as_str)
                == Some("task-notification");
        self.cli_turn = false;
        if !waking && (self.command_open || self.wake.is_none()) {
            self.command_open = false;
            self.wake = None;
            return Some(self.input_command.unwrap_or_default());
        }
        if self.wake.is_none() && !self.command_open {
            self.adopt_wake_turn(out);
            if self.wake.is_none() {
                out.push(Parsed::Event(AdapterEvent::Diagnostic {
                    kind: "autonomous_run_unattributed",
                    event_type: String::new(),
                    required: false,
                    field_names: Vec::new(),
                }));
            }
        }
        self.wake.take()
    }

    /// Итог `/compact` кнопки узнаётся по его UUID в `user_message_uuids`. Без списка
    /// им может быть только итог без открытого и пробуждающего хода: `/compact` уходит
    /// лишь в простой CLI.
    fn is_compact_result(&self, value: &serde_json::Value) -> bool {
        let Some(compact) = self.compact_command else {
            return false;
        };
        if let Some(Parsed::AnswerFrame { user_message_uuids }) = Self::answer_frame(value) {
            return user_message_uuids.contains(&compact);
        }
        !self.command_open
            && self.wake.is_none()
            && value
                .pointer("/origin/kind")
                .and_then(serde_json::Value::as_str)
                != Some("task-notification")
    }

    fn feed_result(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let is_error = value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true);
        if self.is_compact_result(value) {
            // Итог служебного `/compact`: ходу прошлой команды он не принадлежит, расход
            // уже ушёл в границу. Без границы сжатия не было: это отказ.
            let ok = self.compact_boundary_seen && !is_error;
            if self.compact_active {
                self.compact_active = false;
                out.push(Parsed::Event(AdapterEvent::CompactionFinished {
                    manual: self.compact_manual,
                    ok: false,
                    pre_tokens: 0,
                    post_tokens: 0,
                }));
            }
            self.forget_compact();
            self.cli_turn = false;
            self.last_usage = None;
            self.progress_sent = 0;
            out.push(Parsed::Event(AdapterEvent::Phase {
                phase: Phase::AwaitingInput,
                tool_name: None,
                summary: None,
            }));
            out.push(Parsed::CompactResultConsumed { ok });
            self.report_background(out);
            return;
        }
        // Автоматическое сжатие оборвалось ошибкой: граница не придёт, эпизод закрывает
        // отказ, а итог остаётся ходу.
        if self.compact_active && is_error {
            self.compact_active = false;
            let manual = std::mem::take(&mut self.compact_manual);
            self.last_usage = None;
            self.progress_sent = 0;
            out.push(Parsed::Event(AdapterEvent::CompactionFinished {
                manual,
                ok: false,
                pre_tokens: 0,
                post_tokens: 0,
            }));
        }
        if let Some(answer) = Self::answer_frame(value) {
            out.push(answer);
        }
        let finished = self.result_command(value, out);
        if !self.command_open {
            out.push(Parsed::Event(AdapterEvent::Phase {
                phase: Phase::AwaitingInput,
                tool_name: None,
                summary: None,
            }));
        }
        if let Some(input_command_id) = finished {
            let usage = self.turn_usage(value);
            let main = self
                .session_model
                .clone()
                .unwrap_or_else(|| self.current_model.clone());
            let last = self.last_usage.unwrap_or_default();
            // Живой учёт следующего хода начинается заново: итог ушёл в `RunFinished`.
            self.progress_sent = 0;
            out.push(Parsed::Event(AdapterEvent::RunFinished {
                input_command_id,
                usage,
                context_tokens: last.input + last.cache_read + last.cache_write,
                context_window_tokens: value
                    .get("modelUsage")
                    .and_then(|usage| usage.get(&main))
                    .and_then(|model| model.get("contextWindow"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            }));
        }
        if value.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) {
            if is_context_overflow(value) {
                out.push(Parsed::Event(AdapterEvent::ContextExhausted));
            } else {
                out.push(Parsed::TurnError {
                    status: value
                        .get("api_error_status")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|status| u16::try_from(status).ok()),
                    error: turn_error_text(value),
                    terminal_reason: value
                        .get("terminal_reason")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
        self.report_background(out);
    }

    fn turn_usage(&mut self, value: &serde_json::Value) -> Vec<ModelUsage> {
        let mut usage = Vec::new();
        let Some(models) = value
            .get("modelUsage")
            .and_then(serde_json::Value::as_object)
        else {
            return usage;
        };
        let main = self
            .session_model
            .clone()
            .unwrap_or_else(|| self.current_model.clone());
        let mut names: Vec<(&String, &serde_json::Value)> = models.iter().collect();
        names.sort_by(|a, b| a.0.cmp(b.0));
        for (name, model) in names {
            let get = |key: &str| {
                model
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            let seen = ModelUsage {
                model: name.clone(),
                agent: String::new(),
                input_tokens: get("inputTokens"),
                output_tokens: get("outputTokens"),
                cache_read_tokens: get("cacheReadInputTokens"),
                cache_write_tokens: get("cacheCreationInputTokens"),
            };
            let prev = self.prev_usage.insert(name.clone(), seen.clone());
            let delta = |now: u64, was: u64| now.saturating_sub(was);
            let was = prev.unwrap_or_else(|| ModelUsage {
                model: name.clone(),
                agent: String::new(),
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
            let agent = if *name == main {
                "orchestrator".to_owned()
            } else {
                self.model_agent
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| "orchestrator".to_owned())
            };
            usage.push(ModelUsage {
                model: name.clone(),
                agent,
                input_tokens: delta(seen.input_tokens, was.input_tokens),
                output_tokens: delta(seen.output_tokens, was.output_tokens),
                cache_read_tokens: delta(seen.cache_read_tokens, was.cache_read_tokens),
                cache_write_tokens: delta(seen.cache_write_tokens, was.cache_write_tokens),
            });
        }
        usage
    }

    fn feed_control_response(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        let response = value.get("response");
        let request_id = response
            .and_then(|response| response.get("request_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if request_id.is_empty() {
            out.push(Parsed::Ignored);
            return;
        }
        let success = response
            .and_then(|response| response.get("subtype"))
            .and_then(serde_json::Value::as_str)
            == Some("success");
        // Квитанцию несёт только ответ на `interrupt`: у смены модели и режима
        // этих ключей нет. Пустые списки тоже квитанция: очередь была пуста.
        let inner = success
            .then(|| response.and_then(|response| response.get("response")))
            .flatten();
        let interrupt = inner.and_then(|inner| {
            let queued = inner.get("still_queued").is_some();
            let cancelled = inner.get("cancelled").is_some();
            (queued || cancelled).then(|| InterruptReceipt {
                still_queued: inner.get("still_queued").map(uuid_list).unwrap_or_default(),
                cancelled: inner.get("cancelled").map(uuid_list).unwrap_or_default(),
            })
        });
        out.push(Parsed::ControlResponse {
            request_id,
            success,
            interrupt,
        });
    }
}

/// Список UUID из кадра, мусор пропускается, длина ограничена.
fn uuid_list(value: &serde_json::Value) -> Vec<Uuid> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .filter_map(|id| Uuid::parse_str(id).ok())
                .take(128)
                .collect()
        })
        .unwrap_or_default()
}

impl std::fmt::Debug for Parser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parser")
            .field("input_command_id", &self.input_command)
            .field("model", &self.current_model)
            .field("open_blocks", &self.blocks.len())
            .field("tracked_tool_calls", &self.tool_blocks.len())
            .finish_non_exhaustive()
    }
}

fn tool_result_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn tool_argument(tool_name: &str, input: &serde_json::Value) -> String {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    if let Some(command) = text("command") {
        return command_label(command).to_owned();
    }
    if let Some(path) = text("file_path").or_else(|| text("notebook_path")) {
        // Двоеточие и пробел после имени инструмента тоже занимают место в сводке.
        let room = TOOL_CALL_SUMMARY_LIMIT.saturating_sub(tool_name.len() + 2);
        return path_tail(path, room);
    }
    let question = input
        .pointer("/questions/0/question")
        .and_then(serde_json::Value::as_str);
    ["pattern", "skill", "query", "url", "description"]
        .iter()
        .find_map(|key| text(key))
        .or(question)
        .unwrap_or_default()
        .to_owned()
}

/// Текст вызова: команда как есть, остальное сырым входом.
fn call_text(input: &serde_json::Value) -> String {
    match input.get("command").and_then(serde_json::Value::as_str) {
        Some(command) => command.to_owned(),
        None if input.is_null() => String::new(),
        None => input.to_string(),
    }
}

fn tool_summary(tool_name: &str, input: &serde_json::Value) -> String {
    let argument = tool_argument(tool_name, input);
    let summary = if argument.is_empty() {
        tool_name.to_owned()
    } else {
        format!("{tool_name}: {argument}")
    };
    cut_end(&summary, TOOL_CALL_SUMMARY_LIMIT)
}

fn task_kind(task: &serde_json::Value) -> BackgroundKind {
    match task.get("task_type").and_then(serde_json::Value::as_str) {
        Some("local_agent") => BackgroundKind::Agent,
        _ => BackgroundKind::Bash,
    }
}

fn subagent_name(input: &serde_json::Value) -> String {
    input
        .get("subagent_type")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            input
                .get("description")
                .and_then(serde_json::Value::as_str)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or("subagent")
        .to_owned()
}

fn is_context_overflow(value: &serde_json::Value) -> bool {
    let mut texts = Vec::new();
    if let Some(text) = value.get("result").and_then(serde_json::Value::as_str) {
        texts.push(text.to_owned());
    }
    if let Some(errors) = value.get("errors").and_then(serde_json::Value::as_array) {
        texts.extend(
            errors
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned),
        );
    }
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        texts.push(error.to_owned());
    }
    texts
        .iter()
        .any(|text| text.to_lowercase().contains("too long"))
}

fn turn_error_text(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.get("result").and_then(serde_json::Value::as_str)
        && !text.is_empty()
    {
        return Some(text.to_owned());
    }
    value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}
