//! Перевод `/api/event` в события адаптера. Переводчик синхронен и не паникует;
//! события чужих сессий отбрасываются, неизвестный тип даёт одну диагностику.

use super::{
    super::{AdapterEvent, BlockKind, ModelUsage, Phase},
    requests::GLOBAL_SESSION,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};
use uuid::Uuid;

const MAIN_AGENT: &str = "orchestrator";
const MAX_UNKNOWN_TYPES: usize = 64;
const MAX_TYPE_CHARS: usize = 64;
const SUMMARY_BYTES: usize = 240;
const BLOCK_NAMESPACE: Uuid = Uuid::from_u128(0x7c1e_52a0_9d3b_4f6e_8a21_3b5d_c04e_91f7);

// Служебные события и то, что разбирают следующие срезы адаптера.
const KNOWN: &[&str] = &[
    "agent.updated",
    "integration.updated",
    "mcp.resources.changed",
    "mcp.status.changed",
    "mcp.tools.changed",
    "plugin.updated",
    "project.updated",
    "provider.updated",
    "server.connected",
    "session.agent.selected",
    "session.compaction.delta",
    "session.compaction.ended",
    "session.compaction.failed",
    "session.compaction.started",
    "session.created",
    "session.deleted",
    "session.inbox.cancelled",
    "session.instructions.updated",
    "session.model.selected",
    "session.permissions",
    "session.retry.scheduled",
    "session.step.streamed",
    "session.synthetic",
    "session.tool.input.ended",
    "session.usage.updated",
    "shell.created",
    "shell.deleted",
    "shell.exited",
];

/// Стабильный идентификатор блока: живые события и сверка по проекции дают один и тот же.
pub fn block_id(session: &str, message: &str, kind: &str, part: &str) -> Uuid {
    Uuid::new_v5(
        &BLOCK_NAMESPACE,
        format!("{session}\0{message}\0{kind}\0{part}").as_bytes(),
    )
}

pub struct RunFailure {
    pub kind: String,
    pub message: String,
}

impl fmt::Debug for RunFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunFailure")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Succeeded,
    Failed(RunFailure),
    Interrupted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Permission,
    Form,
}

/// Запрос сервера. Ответ идёт на путь его сессии: у потомка она своя.
pub struct PendingRequest {
    pub kind: RequestKind,
    pub id: String,
    pub session: String,
    pub item: Option<String>,
    pub child: bool,
    /// Кадр запроса: команды, файлы и текст вопросов.
    pub raw: Value,
}

impl PendingRequest {
    pub fn agent(&self) -> &'static str {
        if self.child { "subagent" } else { MAIN_AGENT }
    }
}

impl fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingRequest")
            .field("kind", &self.kind)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Translated {
    Event(AdapterEvent),
    /// Ввод с этим идентификатором записан сервером; `delivered`: стал видимым сообщением.
    InboxSeen {
        id: String,
        delivered: bool,
    },
    RunStarted,
    RunEnded(RunOutcome),
    /// Начало вызова: адаптер решает, ждать ли пометку «Разрешено правилами CLI».
    ToolCall {
        item: String,
        block: Uuid,
        event: AdapterEvent,
    },
    /// Инструмент уже работает: запроса разрешения по нему больше не будет.
    ToolRunning(String),
    ToolDone(String),
    Request(PendingRequest),
    /// Сервер закрыл запрос с этим идентификатором.
    RequestSettled(String),
}

#[derive(Clone, Copy)]
enum TextKind {
    Text,
    Reasoning,
}

impl TextKind {
    fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "reasoning",
        }
    }

    fn block(self) -> BlockKind {
        match self {
            Self::Text => BlockKind::Text,
            Self::Reasoning => BlockKind::Reasoning,
        }
    }
}

/// Имя есть только у `session.tool.input.started`, вход — у `session.tool.called`.
struct ToolState {
    name: String,
    block: Uuid,
    input: Value,
    open: bool,
}

#[derive(Default)]
struct Tally {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

pub struct Translator {
    root: String,
    children: HashSet<String>,
    command: Option<Uuid>,
    /// Модель шага: `session.step.started` единственный источник.
    steps: HashMap<String, String>,
    model: String,
    open: HashSet<Uuid>,
    /// `id` вызова уникален только в шаге, поэтому ключ — пара с сообщением.
    tools: HashMap<String, ToolState>,
    usage: BTreeMap<String, Tally>,
    context_tokens: u64,
    windows: HashMap<String, u64>,
    unknown: HashSet<String>,
}

fn string_field<'a>(data: &'a Value, key: &str) -> &'a str {
    data.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn model_label(model: &Value) -> Option<String> {
    let provider = model.get("providerID")?.as_str()?;
    let id = model.get("id")?.as_str()?;
    Some(format!("{provider}/{id}"))
}

pub fn tool_key(message: &str, id: &str) -> String {
    format!("{message}/{id}")
}

fn tool_summary(name: &str, input: &Value) -> String {
    let argument = [
        "command",
        "path",
        "filePath",
        "pattern",
        "query",
        "url",
        "description",
    ]
    .iter()
    .find_map(|key| input.get(key).and_then(Value::as_str));
    let mut summary = match argument {
        Some(argument) if !argument.is_empty() => format!("{name}: {argument}"),
        _ => name.to_owned(),
    };
    summary.truncate(summary.floor_char_boundary(SUMMARY_BYTES));
    summary
}

// Текст вызова: команда как есть, остальное сырым входом.
fn call_text(input: &Value) -> String {
    match input.get("command").and_then(Value::as_str) {
        Some(command) => command.to_owned(),
        None if input.is_null() => String::new(),
        None => input.to_string(),
    }
}

// Картинки из результата разбирает срез вложений.
fn content_text(data: &Value) -> String {
    data.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ordinal(data: &Value) -> String {
    data.get("ordinal")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .to_string()
}

impl Translator {
    pub fn new(root: &str, windows: HashMap<String, u64>) -> Self {
        Self {
            root: root.to_owned(),
            children: HashSet::new(),
            command: None,
            steps: HashMap::new(),
            model: String::new(),
            open: HashSet::new(),
            tools: HashMap::new(),
            usage: BTreeMap::new(),
            context_tokens: 0,
            windows,
            unknown: HashSet::new(),
        }
    }

    /// Своя сессия или её потомок; ответы на запросы идут на путь этой сессии.
    pub fn owns(&self, session: &str) -> bool {
        session == self.root || self.children.contains(session)
    }

    /// Потомки сессии: у каждого своя копия правил.
    pub fn children(&self) -> impl Iterator<Item = &str> {
        self.children.iter().map(String::as_str)
    }

    /// Команда следующих блоков. Расход не сбрасывается: запуск без ввода, в который влился
    /// ввод, считается вместе с ним.
    pub fn begin_run(&mut self, command: Option<Uuid>) {
        self.command = command;
    }

    pub fn finish_run(&mut self, command: Uuid) -> AdapterEvent {
        self.command = None;
        self.steps.clear();
        self.open.clear();
        self.tools.clear();
        let usage = std::mem::take(&mut self.usage)
            .into_iter()
            .map(|(model, tally)| ModelUsage {
                model,
                agent: MAIN_AGENT.into(),
                input_tokens: tally.input,
                output_tokens: tally.output,
                cache_read_tokens: tally.cache_read,
                cache_write_tokens: tally.cache_write,
            })
            .collect();
        AdapterEvent::RunFinished {
            input_command_id: command,
            usage,
            context_tokens: std::mem::take(&mut self.context_tokens),
            context_window_tokens: self.windows.get(&self.model).copied().unwrap_or_default(),
        }
    }

    pub fn feed(&mut self, event: &Value) -> Vec<Translated> {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return vec![Self::diagnostic("unparsed_event", "unparsed_event", false)];
        };
        let empty = Value::Null;
        let data = event.get("data").unwrap_or(&empty);
        if kind == "session.created" {
            self.on_created(data);
            return Vec::new();
        }
        if kind == "permission.asked" {
            return self.on_permission(data);
        }
        if kind == "form.created" {
            return self.on_form(data);
        }
        if matches!(
            kind,
            "permission.replied" | "form.replied" | "form.cancelled"
        ) {
            return self.on_settled(kind, data);
        }
        let session = data.get("sessionID").and_then(Value::as_str);
        let terminal = kind.starts_with("session.execution.");
        if terminal && session.is_none() {
            return vec![Self::diagnostic("unknown_event", kind, true)];
        }
        if session.is_some_and(|session| session != self.root) {
            return Vec::new();
        }
        match kind {
            "session.execution.succeeded" => {
                vec![Translated::RunEnded(RunOutcome::Succeeded)]
            }
            "session.execution.interrupted" => {
                vec![Translated::RunEnded(RunOutcome::Interrupted)]
            }
            "session.execution.failed" => {
                vec![Translated::RunEnded(RunOutcome::Failed(RunFailure {
                    kind: data
                        .pointer("/error/type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    message: data
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }))]
            }
            "session.execution.started" => vec![Translated::RunStarted],
            "session.inbox.enqueued" | "session.inbox.delivered" => {
                vec![Translated::InboxSeen {
                    id: string_field(data, "inboxID").to_owned(),
                    delivered: kind == "session.inbox.delivered",
                }]
            }
            "session.step.started" => self.on_step_started(data),
            "session.step.ended" | "session.step.failed" => self.on_step_finished(data),
            "session.text.started" => self.on_started(TextKind::Text, data),
            "session.text.delta" => self.on_delta(TextKind::Text, data),
            "session.text.ended" => self.on_ended(TextKind::Text, data),
            "session.reasoning.started" => self.on_started(TextKind::Reasoning, data),
            "session.reasoning.delta" => self.on_delta(TextKind::Reasoning, data),
            "session.reasoning.ended" => self.on_ended(TextKind::Reasoning, data),
            "session.tool.input.started" => self.on_tool_declared(data),
            "session.tool.called" => self.on_tool_called(data),
            "session.tool.progress" => self.on_tool_progress(data),
            "session.tool.success" => self.on_tool_success(data),
            "session.tool.failed" => self.on_tool_failed(data),
            _ if KNOWN.contains(&kind) => Vec::new(),
            _ if terminal => vec![Self::diagnostic("unknown_event", kind, true)],
            _ => self.on_unknown(kind),
        }
    }

    fn diagnostic(kind: &'static str, event_type: &str, required: bool) -> Translated {
        Translated::Event(AdapterEvent::Diagnostic {
            kind,
            event_type: event_type.to_owned(),
            required,
        })
    }

    fn on_unknown(&mut self, kind: &str) -> Vec<Translated> {
        let name: String = kind.chars().take(MAX_TYPE_CHARS).collect();
        if self.unknown.len() >= MAX_UNKNOWN_TYPES || !self.unknown.insert(name.clone()) {
            return Vec::new();
        }
        vec![Self::diagnostic("unknown_event", &name, false)]
    }

    fn on_created(&mut self, data: &Value) {
        let parent = string_field(data, "parentID");
        if !parent.is_empty() && self.owns(parent) {
            self.children
                .insert(string_field(data, "sessionID").to_owned());
        }
    }

    fn on_step_started(&mut self, data: &Value) -> Vec<Translated> {
        if let Some(model) = data.get("model").and_then(model_label) {
            self.steps.insert(
                string_field(data, "assistantMessageID").to_owned(),
                model.clone(),
            );
            self.model = model;
        }
        vec![Translated::Event(AdapterEvent::Phase {
            phase: Phase::Generating,
            tool_name: None,
            summary: None,
        })]
    }

    // `input` уже без кэша, `output` без рассуждений: так их записывает сервер.
    fn on_step_finished(&mut self, data: &Value) -> Vec<Translated> {
        let Some(tokens) = data.get("tokens").filter(|tokens| tokens.is_object()) else {
            return Vec::new();
        };
        let count = |pointer: &str| tokens.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
        let model = self
            .steps
            .get(string_field(data, "assistantMessageID"))
            .unwrap_or(&self.model)
            .clone();
        let tally = self.usage.entry(model).or_default();
        tally.input += count("/input");
        tally.output += count("/output") + count("/reasoning");
        tally.cache_read += count("/cache/read");
        tally.cache_write += count("/cache/write");
        self.context_tokens = count("/input") + count("/cache/read") + count("/cache/write");
        Vec::new()
    }

    // Без идентификатора ответить нельзя, а вызов будет ждать вечно.
    fn request(
        &self,
        event: &str,
        kind: RequestKind,
        raw: &Value,
        item: Option<String>,
    ) -> Vec<Translated> {
        let session = string_field(raw, "sessionID");
        let id = string_field(raw, "id");
        if session.is_empty() || id.is_empty() {
            return vec![Self::diagnostic("unparsed_event", event, true)];
        }
        let global = kind == RequestKind::Form && session == GLOBAL_SESSION;
        if !global && !self.owns(session) {
            return Vec::new();
        }
        vec![Translated::Request(PendingRequest {
            kind,
            id: id.to_owned(),
            session: session.to_owned(),
            item,
            child: session != self.root,
            raw: raw.clone(),
        })]
    }

    fn on_permission(&mut self, data: &Value) -> Vec<Translated> {
        let item = data.get("source").map(|source| {
            tool_key(
                string_field(source, "messageID"),
                string_field(source, "id"),
            )
        });
        self.request("permission.asked", RequestKind::Permission, data, item)
    }

    fn on_form(&mut self, data: &Value) -> Vec<Translated> {
        let empty = Value::Null;
        let form = data.get("form").unwrap_or(&empty);
        let item = form
            .pointer("/metadata/tool")
            .map(|tool| tool_key(string_field(tool, "messageID"), string_field(tool, "id")));
        self.request("form.created", RequestKind::Form, form, item)
    }

    fn on_settled(&self, kind: &str, data: &Value) -> Vec<Translated> {
        let key = if kind == "permission.replied" {
            "requestID"
        } else {
            "id"
        };
        if !self.owns(string_field(data, "sessionID")) {
            return Vec::new();
        }
        vec![Translated::RequestSettled(
            string_field(data, key).to_owned(),
        )]
    }

    fn tool_of(&mut self, data: &Value) -> (String, &mut ToolState) {
        let message = string_field(data, "assistantMessageID");
        let id = string_field(data, "id");
        let key = tool_key(message, id);
        let block = block_id(&self.root, message, "tool", id);
        let state = self.tools.entry(key.clone()).or_insert_with(|| ToolState {
            name: "tool".to_owned(),
            block,
            input: Value::Null,
            open: false,
        });
        (key, state)
    }

    fn on_tool_declared(&mut self, data: &Value) -> Vec<Translated> {
        let (_, state) = self.tool_of(data);
        if let Some(name) = data.get("name").and_then(Value::as_str) {
            state.name = name.to_owned();
        }
        Vec::new()
    }

    // Начало блока ждёт входа: сводка без него пуста.
    fn open_tool(&mut self, data: &Value, out: &mut Vec<Translated>) {
        let model = self
            .steps
            .get(string_field(data, "assistantMessageID"))
            .unwrap_or(&self.model)
            .clone();
        let command = self.command;
        let (item, state) = self.tool_of(data);
        if std::mem::replace(&mut state.open, true) {
            return;
        }
        let block = state.block;
        let event = AdapterEvent::BlockStarted {
            block_id: block,
            kind: BlockKind::ToolCall,
            input_command_id: command,
            parent: None,
            agent: MAIN_AGENT.into(),
            model,
            tool_name: Some(state.name.clone()),
            summary: Some(tool_summary(&state.name, &state.input)),
            approved_by: None,
        };
        out.push(Translated::ToolCall { item, block, event });
    }

    fn on_tool_called(&mut self, data: &Value) -> Vec<Translated> {
        let (_, state) = self.tool_of(data);
        state.input = data.get("input").cloned().unwrap_or(Value::Null);
        let (name, summary) = (state.name.clone(), tool_summary(&state.name, &state.input));
        let mut out = Vec::new();
        self.open_tool(data, &mut out);
        out.push(Translated::Event(AdapterEvent::Phase {
            phase: Phase::RunningTool,
            tool_name: Some(name),
            summary: Some(summary),
        }));
        out
    }

    fn on_tool_progress(&mut self, data: &Value) -> Vec<Translated> {
        let (item, _) = self.tool_of(data);
        vec![Translated::ToolRunning(item)]
    }

    fn finish_tool(&mut self, data: &Value, text: String, status: &str) -> Vec<Translated> {
        let mut out = Vec::new();
        self.open_tool(data, &mut out);
        let model = self
            .steps
            .get(string_field(data, "assistantMessageID"))
            .unwrap_or(&self.model)
            .clone();
        let message = string_field(data, "assistantMessageID");
        let id = string_field(data, "id");
        let item = tool_key(message, id);
        let result = block_id(&self.root, message, "tool_result", id);
        let Some(state) = self.tools.remove(&item) else {
            return out;
        };
        out.push(Translated::ToolDone(item));
        out.push(Translated::Event(AdapterEvent::BlockStarted {
            block_id: result,
            kind: BlockKind::ToolResult,
            input_command_id: self.command,
            parent: Some(state.block),
            agent: MAIN_AGENT.into(),
            model,
            tool_name: None,
            summary: None,
            approved_by: None,
        }));
        out.push(Translated::Event(AdapterEvent::BlockFinal {
            block_id: result,
            text,
            summary: None,
        }));
        let mut summary = tool_summary(&state.name, &state.input);
        summary.push_str(", ");
        summary.push_str(status);
        out.push(Translated::Event(AdapterEvent::BlockFinal {
            block_id: state.block,
            text: call_text(&state.input),
            summary: Some(summary),
        }));
        out.push(Translated::Event(AdapterEvent::Phase {
            phase: Phase::Generating,
            tool_name: None,
            summary: None,
        }));
        out
    }

    // Упавшая команда приходит как `success` с `metadata.exit`.
    fn on_tool_success(&mut self, data: &Value) -> Vec<Translated> {
        let status = match data.pointer("/metadata/exit").and_then(Value::as_i64) {
            Some(code) => format!("exit {code}"),
            None => "completed".to_owned(),
        };
        self.finish_tool(data, content_text(data), &status)
    }

    fn on_tool_failed(&mut self, data: &Value) -> Vec<Translated> {
        let status = match data.pointer("/error/type").and_then(Value::as_str) {
            Some("permission.rejected") => "declined",
            Some("aborted") => "interrupted",
            _ => "failed",
        };
        let message = data
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content = content_text(data);
        let text = if content.is_empty() {
            message.to_owned()
        } else {
            format!("{content}\n{message}")
        };
        self.finish_tool(data, text, status)
    }

    fn id_of(&self, kind: TextKind, data: &Value) -> Uuid {
        block_id(
            &self.root,
            string_field(data, "assistantMessageID"),
            kind.name(),
            &ordinal(data),
        )
    }

    // Блоки не вложены: `reasoning.ended` приходит после `text.started`, поэтому у каждого
    // своё состояние, а не стек.
    fn ensure_open(&mut self, kind: TextKind, data: &Value, out: &mut Vec<Translated>) -> Uuid {
        let block_id = self.id_of(kind, data);
        if self.open.insert(block_id) {
            let model = self
                .steps
                .get(string_field(data, "assistantMessageID"))
                .unwrap_or(&self.model)
                .clone();
            out.push(Translated::Event(AdapterEvent::BlockStarted {
                block_id,
                kind: kind.block(),
                input_command_id: self.command,
                parent: None,
                agent: MAIN_AGENT.into(),
                model,
                tool_name: None,
                summary: None,
                approved_by: None,
            }));
        }
        block_id
    }

    fn on_started(&mut self, kind: TextKind, data: &Value) -> Vec<Translated> {
        let mut out = Vec::new();
        self.ensure_open(kind, data, &mut out);
        out
    }

    fn on_delta(&mut self, kind: TextKind, data: &Value) -> Vec<Translated> {
        let mut out = Vec::new();
        let block_id = self.ensure_open(kind, data, &mut out);
        out.push(Translated::Event(AdapterEvent::BlockDelta {
            block_id,
            append: string_field(data, "delta").to_owned(),
        }));
        out
    }

    fn on_ended(&mut self, kind: TextKind, data: &Value) -> Vec<Translated> {
        let mut out = Vec::new();
        let block_id = self.ensure_open(kind, data, &mut out);
        self.open.remove(&block_id);
        out.push(Translated::Event(AdapterEvent::BlockFinal {
            block_id,
            text: string_field(data, "text").to_owned(),
            summary: None,
        }));
        out
    }
}
