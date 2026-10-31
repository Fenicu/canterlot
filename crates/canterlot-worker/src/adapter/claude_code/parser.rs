//! Stream-JSON CLI output: NDJSON frames, tool input assembler, phases,
//! per-model usage deltas and subagent attribution.
//!
//! Never panics on unknown input and never ends the session: unknown frames
//! are `Ignored`, lines that are not JSON become an `unparsed_line`
//! diagnostic. Partial tool JSON stays buffered until `content_block_stop`.

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::super::labels::{command_label, path_tail};
use super::super::{AdapterEvent, BlockKind, ModelUsage, Phase, TodoItem, TodoStatus};

/// Summary of a tool call: tool name plus the identifying argument.
const TOOL_CALL_SUMMARY_LIMIT: usize = 120;

/// Сводка результата, завершившегося ошибкой: у результата своей сводки нет, отметка
/// доходит до ленты без новых полей в протоколе, журнале и истории.
const TOOL_FAILED_SUMMARY: &str = "error";

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
            } => f
                .debug_struct("ControlResponse")
                .field("request_id", request_id)
                .field("success", success)
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
        }
    }

    pub fn set_input_command(&mut self, id: Uuid) {
        self.input_command = Some(id);
    }

    pub fn reset_process(&mut self) {
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

    fn note_usage(&mut self, usage: &serde_json::Value, parent_tool_use_id: Option<&str>) {
        if parent_tool_use_id.is_some() {
            return;
        }
        let get = |key: &str| {
            usage
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        self.last_usage = Some(MessageUsage {
            input: get("input_tokens"),
            cache_read: get("cache_read_input_tokens"),
            cache_write: get("cache_creation_input_tokens"),
        });
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
            _ => out.push(Parsed::Ignored),
        }
        out
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
                } else {
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
                    self.note_usage(usage, parent);
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
                    _ => {}
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
                    _ => {}
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
                    self.note_usage(usage, parent);
                }
            }
            _ => {}
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
                _ => {}
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
            self.note_usage(usage, parent);
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
        for block in blocks {
            if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_result") {
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
                summary: is_error.then(|| TOOL_FAILED_SUMMARY.to_owned()),
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

    fn feed_result(&mut self, value: &serde_json::Value, out: &mut Vec<Parsed>) {
        if let Some(answer) = Self::answer_frame(value) {
            out.push(answer);
        }
        out.push(Parsed::Event(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        }));
        let usage = self.turn_usage(value);
        let main = self
            .session_model
            .clone()
            .unwrap_or_else(|| self.current_model.clone());
        let last = self.last_usage.unwrap_or_default();
        out.push(Parsed::Event(AdapterEvent::RunFinished {
            input_command_id: self.input_command.unwrap_or_default(),
            usage,
            context_tokens: last.input + last.cache_read + last.cache_write,
            context_window_tokens: value
                .get("modelUsage")
                .and_then(|usage| usage.get(&main))
                .and_then(|model| model.get("contextWindow"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
        }));
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
        out.push(Parsed::ControlResponse {
            request_id,
            success,
        });
    }
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
    let mut summary = if argument.is_empty() {
        tool_name.to_owned()
    } else {
        format!("{tool_name}: {argument}")
    };
    summary.truncate(summary.floor_char_boundary(TOOL_CALL_SUMMARY_LIMIT));
    summary
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
