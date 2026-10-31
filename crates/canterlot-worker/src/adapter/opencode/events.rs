//! Перевод `/api/event` в события адаптера. Переводчик синхронен и не паникует;
//! события чужих сессий отбрасываются, неизвестный тип даёт одну диагностику.

use super::super::{AdapterEvent, BlockKind, ModelUsage, Phase};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};
use uuid::Uuid;

const MAIN_AGENT: &str = "orchestrator";
const MAX_UNKNOWN_TYPES: usize = 64;
const BLOCK_NAMESPACE: Uuid = Uuid::from_u128(0x7c1e_52a0_9d3b_4f6e_8a21_3b5d_c04e_91f7);

// Служебные события и то, что разбирают следующие срезы адаптера.
const KNOWN: &[&str] = &[
    "agent.updated",
    "form.cancelled",
    "form.created",
    "form.replied",
    "integration.updated",
    "mcp.resources.changed",
    "mcp.status.changed",
    "mcp.tools.changed",
    "permission.asked",
    "permission.replied",
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
    "session.execution.started",
    "session.inbox.cancelled",
    "session.instructions.updated",
    "session.model.selected",
    "session.permissions",
    "session.retry.scheduled",
    "session.step.streamed",
    "session.synthetic",
    "session.tool.called",
    "session.tool.failed",
    "session.tool.input.ended",
    "session.tool.input.started",
    "session.tool.progress",
    "session.tool.success",
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

#[derive(Debug)]
pub enum Translated {
    Event(AdapterEvent),
    /// Ввод с этим идентификатором записан сервером.
    InboxSeen(String),
    RunEnded(RunOutcome),
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

    pub fn begin_run(&mut self, command: Uuid) {
        self.command = Some(command);
        self.usage.clear();
        self.context_tokens = 0;
    }

    pub fn finish_run(&mut self, command: Uuid) -> AdapterEvent {
        self.command = None;
        self.steps.clear();
        self.open.clear();
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
            "session.inbox.enqueued" | "session.inbox.delivered" => {
                vec![Translated::InboxSeen(
                    string_field(data, "inboxID").to_owned(),
                )]
            }
            "session.step.started" => self.on_step_started(data),
            "session.step.ended" | "session.step.failed" => self.on_step_finished(data),
            "session.text.started" => self.on_started(TextKind::Text, data),
            "session.text.delta" => self.on_delta(TextKind::Text, data),
            "session.text.ended" => self.on_ended(TextKind::Text, data),
            "session.reasoning.started" => self.on_started(TextKind::Reasoning, data),
            "session.reasoning.delta" => self.on_delta(TextKind::Reasoning, data),
            "session.reasoning.ended" => self.on_ended(TextKind::Reasoning, data),
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
        if self.unknown.len() >= MAX_UNKNOWN_TYPES || !self.unknown.insert(kind.to_owned()) {
            return Vec::new();
        }
        vec![Self::diagnostic("unknown_event", kind, false)]
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
