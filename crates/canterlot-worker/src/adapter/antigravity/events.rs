//! Транслятор `session/update` ACP в события адаптера (§6).
//!
//! Чанки текста и мыслей склеиваются в один блок; смена вида и конец хода
//! закрывают его `BlockFinal`. Начало вызова инструмента удерживается `HeldTools`
//! до запроса разрешения или старта, как у остальных адаптеров.

use super::super::{AdapterEvent, BlockKind, Phase};
use crate::adapter::held_tool::HeldTools;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

const SUMMARY_BYTES: usize = 160;
const SUBAGENT_NOTE: &str = "результат субагента придёт текстом агента";

struct OpenBlock {
    block_id: Uuid,
    kind: BlockKind,
    text: String,
}

struct ToolState {
    block_id: Uuid,
    name: String,
    summary: String,
    released: bool,
    raw_input: Value,
}

/// Вызов `ask_question`: адаптер нормализует вопросы и открывает карточку.
#[derive(Clone, Debug)]
pub struct QuestionCall {
    pub tool_call_id: String,
    pub raw_input: Value,
}

/// Что просит сделать кадр помимо событий.
#[derive(Default)]
pub struct Fed {
    pub events: Vec<AdapterEvent>,
    pub question: Option<QuestionCall>,
}

pub struct Translator {
    model: String,
    agent: String,
    current: Option<Uuid>,
    title: Option<String>,
    open: Option<OpenBlock>,
    tools: HashMap<String, ToolState>,
    held: HeldTools,
    pub permission_open: bool,
}

impl Translator {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            agent: "orchestrator".into(),
            current: None,
            title: None,
            open: None,
            tools: HashMap::new(),
            held: HeldTools::default(),
            permission_open: false,
        }
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_owned();
    }

    pub fn begin_turn(&mut self, command: Option<Uuid>) {
        self.current = command;
        self.open = None;
        self.tools.clear();
        self.permission_open = false;
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Конец хода: открытый блок закрывается, удержанные вызовы выходят без пометки.
    pub fn end_turn(&mut self, out: &mut Vec<AdapterEvent>) {
        self.close_open(out);
        let mut released = VecDeque::new();
        self.held.release_all(&mut released);
        out.extend(released);
        self.tools.clear();
        self.permission_open = false;
    }

    fn close_open(&mut self, out: &mut Vec<AdapterEvent>) {
        if let Some(open) = self.open.take() {
            out.push(AdapterEvent::BlockFinal {
                block_id: open.block_id,
                text: open.text,
                summary: None,
            });
        }
    }

    fn start_text(&mut self, kind: BlockKind, out: &mut Vec<AdapterEvent>) {
        let block_id = Uuid::now_v7();
        out.push(AdapterEvent::BlockStarted {
            block_id,
            kind: kind.clone(),
            input_command_id: self.current,
            parent: None,
            agent: self.agent.clone(),
            model: self.model.clone(),
            tool_name: None,
            summary: None,
            approved_by: None,
        });
        self.open = Some(OpenBlock {
            block_id,
            kind,
            text: String::new(),
        });
    }

    fn chunk(&mut self, value: &Value, kind: BlockKind) -> Fed {
        let mut fed = Fed::default();
        let Some(text) = value
            .get("content")
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
        else {
            fed.events.push(self.malformed(value));
            return fed;
        };
        if text.is_empty() {
            return fed;
        }
        let same = self.open.as_ref().is_some_and(|open| open.kind == kind);
        if !same {
            self.close_open(&mut fed.events);
            self.start_text(kind.clone(), &mut fed.events);
        }
        if let Some(open) = self.open.as_mut() {
            open.text.push_str(text);
            fed.events.push(AdapterEvent::BlockDelta {
                block_id: open.block_id,
                append: text.to_owned(),
            });
        }
        fed
    }

    pub fn feed(&mut self, value: &Value) -> Fed {
        let mut fed = Fed::default();
        let Some(kind) = value.get("sessionUpdate").and_then(Value::as_str) else {
            fed.events.push(self.malformed(value));
            return fed;
        };
        match kind {
            "user_message_chunk" => {}
            "agent_message_chunk" => return self.chunk(value, BlockKind::Text),
            "agent_thought_chunk" => return self.chunk(value, BlockKind::Reasoning),
            "tool_call" => self.tool_call(value, &mut fed),
            "tool_call_update" => self.tool_update(value, &mut fed),
            "session_info_update" => {
                if let Some(title) = value.get("title").and_then(Value::as_str) {
                    self.title = Some(title.to_owned());
                }
            }
            // Внутреннее состояние: ленты не касается.
            "available_commands_update" | "config_option_update" => {}
            other => fed.events.push(AdapterEvent::Diagnostic {
                kind: "unknown_session_update",
                event_type: other.to_owned(),
                required: false,
                field_names: keys(value),
            }),
        }
        fed
    }

    fn tool_call(&mut self, value: &Value, fed: &mut Fed) {
        self.close_open(&mut fed.events);
        let Some(id) = value.get("toolCallId").and_then(Value::as_str) else {
            fed.events.push(self.malformed(value));
            return;
        };
        let Some(name) = value.get("name").and_then(Value::as_str) else {
            fed.events.push(self.malformed(value));
            return;
        };
        let raw_input = value.get("rawInput").cloned().unwrap_or(Value::Null);
        let summary = tool_summary(name, value, &raw_input);
        let block_id = Uuid::now_v7();
        let started = AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolCall,
            input_command_id: self.current,
            parent: None,
            agent: self.agent.clone(),
            model: self.model.clone(),
            tool_name: Some(name.to_owned()),
            summary: Some(summary.clone()),
            approved_by: None,
        };
        let question_input = raw_input.clone();
        self.tools.insert(
            id.to_owned(),
            ToolState {
                block_id,
                name: name.to_owned(),
                summary,
                released: false,
                raw_input,
            },
        );
        self.held.hold(id.to_owned(), block_id, started);
        if name == "ask_question" {
            fed.question = Some(QuestionCall {
                tool_call_id: id.to_owned(),
                raw_input: question_input,
            });
        }
    }

    fn tool_update(&mut self, value: &Value, fed: &mut Fed) {
        let Some(id) = value.get("toolCallId").and_then(Value::as_str) else {
            fed.events.push(self.malformed(value));
            return;
        };
        let Some(status) = value.get("status").and_then(Value::as_str) else {
            fed.events.push(self.malformed(value));
            return;
        };
        if !self.tools.contains_key(id) {
            fed.events.push(AdapterEvent::Diagnostic {
                kind: "unknown_tool_update",
                event_type: "unknown_tool_update".into(),
                required: false,
                field_names: keys(value),
            });
            return;
        }
        match status {
            "pending" => {}
            "in_progress" => self.release(id, true, fed),
            "completed" | "failed" => self.finish_tool(id, status, value, fed),
            _ => fed.events.push(AdapterEvent::Diagnostic {
                kind: "unknown_tool_status",
                event_type: "unknown_tool_status".into(),
                required: false,
                field_names: keys(value),
            }),
        }
    }

    fn release(&mut self, id: &str, with_phase: bool, fed: &mut Fed) {
        let Some(state) = self.tools.get_mut(id) else {
            return;
        };
        if state.released {
            return;
        }
        let mut released = VecDeque::new();
        self.held.release(id, None, &mut released);
        fed.events.extend(released);
        let Some(state) = self.tools.get_mut(id) else {
            return;
        };
        state.released = true;
        if with_phase {
            fed.events.push(AdapterEvent::Phase {
                phase: Phase::RunningTool,
                tool_name: Some(state.name.clone()),
                summary: Some(state.summary.clone()),
            });
        }
    }

    fn finish_tool(&mut self, id: &str, status: &str, value: &Value, fed: &mut Fed) {
        self.release(id, false, fed);
        let Some(state) = self.tools.remove(id) else {
            return;
        };
        let raw_output = value.get("rawOutput");
        let output = raw_output
            .and_then(|raw| raw.get("output"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| content_text(value));
        let exit_code = raw_output
            .and_then(|raw| raw.get("exit_code"))
            .and_then(Value::as_i64);
        let failed = status == "failed" || exit_code.is_some_and(|code| code != 0);
        let result_kind = if failed {
            BlockKind::Error
        } else {
            BlockKind::ToolResult
        };
        let result_summary = match exit_code {
            Some(code) => format!("код выхода {code}"),
            None => status.to_owned(),
        };
        let result_block = Uuid::now_v7();
        fed.events.push(AdapterEvent::BlockStarted {
            block_id: result_block,
            kind: result_kind,
            input_command_id: self.current,
            parent: Some(state.block_id),
            agent: self.agent.clone(),
            model: self.model.clone(),
            tool_name: None,
            summary: Some(result_summary.clone()),
            approved_by: None,
        });
        fed.events.push(AdapterEvent::BlockFinal {
            block_id: result_block,
            text: output,
            summary: Some(result_summary),
        });
        let (text, summary) = tool_final(&state.name, status, value, &state);
        fed.events.push(AdapterEvent::BlockFinal {
            block_id: state.block_id,
            text,
            summary: Some(summary),
        });
    }

    fn malformed(&self, value: &Value) -> AdapterEvent {
        AdapterEvent::Diagnostic {
            kind: "malformed_session_update",
            event_type: "malformed_session_update".into(),
            required: true,
            field_names: keys(value),
        }
    }
}

fn keys(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn cut(text: &str) -> String {
    if text.len() <= SUMMARY_BYTES {
        return text.to_owned();
    }
    let end = text.floor_char_boundary(SUMMARY_BYTES);
    format!("{}…", &text[..end])
}

fn raw_input_field<'a>(raw: &'a Value, key: &str) -> &'a str {
    raw.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn tool_summary(name: &str, value: &Value, raw: &Value) -> String {
    match name {
        "invoke_subagent" => {
            let role = raw_input_field(raw, "Role");
            let prompt = raw_input_field(raw, "Prompt");
            cut(&format!("{role}: {prompt}"))
        }
        "manage_task" => cut(&format!("Action: {}", raw_input_field(raw, "Action"))),
        // MCP-инструменты приходят как `mcp_<server>_<tool>`; их схема кладёт
        // подпись в `rawInput.toolSummary`, а `title` может быть служебным.
        name if name.starts_with("mcp_") => raw
            .get("toolSummary")
            .and_then(Value::as_str)
            .or_else(|| value.get("title").and_then(Value::as_str))
            .unwrap_or(name)
            .to_owned(),
        _ => value
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| raw.get("toolSummary").and_then(Value::as_str))
            .unwrap_or(name)
            .to_owned(),
    }
}

fn tool_final(name: &str, status: &str, value: &Value, state: &ToolState) -> (String, String) {
    match name {
        "invoke_subagent" => {
            let prompt = raw_input_field(&state.raw_input, "Prompt").to_owned();
            if status == "completed" && value.get("rawOutput").is_none() {
                (prompt, SUBAGENT_NOTE.to_owned())
            } else {
                (prompt, status.to_owned())
            }
        }
        "manage_task" => (
            raw_input_field(&state.raw_input, "TaskId").to_owned(),
            format!("Action: {}", raw_input_field(&state.raw_input, "Action")),
        ),
        _ => {
            let text = state
                .raw_input
                .get("CommandLine")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if state.raw_input.is_null() {
                        String::new()
                    } else {
                        state.raw_input.to_string()
                    }
                });
            (text, state.summary.clone())
        }
    }
}

fn content_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| {
                    part.get("content")
                        .and_then(|inner| inner.get("text"))
                        .and_then(Value::as_str)
                        .or_else(|| part.get("text").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(kind: &str, text: &str) -> Value {
        json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
    }

    fn tool(id: &str, name: &str, raw: Value) -> Value {
        json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "name": name,
            "status": "in_progress",
            "kind": "execute",
            "title": "run",
            "rawInput": raw,
        })
    }

    fn update(id: &str, status: &str, raw_output: Option<Value>) -> Value {
        let mut value = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": status,
        });
        if let Some(output) = raw_output {
            value["rawOutput"] = output;
        }
        value
    }

    fn texts(events: &[AdapterEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::BlockFinal { text, .. } => Some(text.clone()),
                AdapterEvent::BlockDelta { append, .. } => Some(append.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn small_chunks_merge_and_kind_change_closes_the_block() {
        let mut translator = Translator::new("m");
        let command = Uuid::now_v7();
        translator.begin_turn(Some(command));
        let mut events = Vec::new();
        events.extend(translator.feed(&chunk("agent_thought_chunk", "a")).events);
        events.extend(translator.feed(&chunk("agent_thought_chunk", "b")).events);
        events.extend(translator.feed(&chunk("agent_message_chunk", "c")).events);
        translator.end_turn(&mut events);

        let reasoning: Vec<&AdapterEvent> = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AdapterEvent::BlockStarted {
                        kind: BlockKind::Reasoning,
                        ..
                    }
                )
            })
            .collect();
        let text: Vec<&AdapterEvent> = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AdapterEvent::BlockStarted {
                        kind: BlockKind::Text,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!((reasoning.len(), text.len()), (1, 1));
        let finals: Vec<&String> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::BlockFinal { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(finals, vec!["ab", "c"]);
        for event in &events {
            if let AdapterEvent::BlockStarted {
                input_command_id, ..
            } = event
            {
                assert_eq!(*input_command_id, Some(command));
            }
        }
    }

    #[test]
    fn a_tool_start_waits_for_permission_or_start() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        assert!(
            translator
                .feed(&tool(
                    "call_1",
                    "run_command",
                    json!({"CommandLine": "echo hi"})
                ))
                .events
                .is_empty()
        );
        assert!(
            translator
                .feed(&update("call_1", "pending", None))
                .events
                .is_empty()
        );

        let events = translator
            .feed(&update("call_1", "in_progress", None))
            .events;
        assert!(matches!(
            events[0],
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolCall,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            AdapterEvent::Phase {
                phase: Phase::RunningTool,
                ..
            }
        ));
    }

    #[test]
    fn a_completed_tool_reports_output_and_exit_code() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        translator.feed(&tool(
            "call_1",
            "run_command",
            json!({"CommandLine": "echo hi"}),
        ));
        translator.feed(&update("call_1", "in_progress", None));
        let events = translator.feed(&update(
            "call_1",
            "completed",
            Some(json!({"output": "hi\n", "exit_code": 0})),
        ));
        let events = events.events;
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolResult,
                ..
            }
        )));
        assert!(texts(&events).iter().any(|text| text == "hi\n"));
        assert!(!events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            }
        )));
    }

    #[test]
    fn a_non_zero_exit_code_is_an_error_result() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        translator.feed(&tool(
            "call_1",
            "run_command",
            json!({"CommandLine": "exit 3"}),
        ));
        translator.feed(&update("call_1", "in_progress", None));
        let events = translator
            .feed(&update(
                "call_1",
                "completed",
                Some(json!({"output": "boom", "exit_code": 3})),
            ))
            .events;
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { summary: Some(summary), .. } if summary == "код выхода 3"
        )));
    }

    #[test]
    fn a_subagent_without_payload_is_closed_with_a_note() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        let raw = json!({"Model": "inherit", "Role": "Shell Runner", "Prompt": "do it"});
        let events = translator
            .feed(&tool("call_1", "invoke_subagent", raw))
            .events;
        assert!(events.is_empty());
        let events = translator.feed(&update("call_1", "completed", None)).events;
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted { tool_name: Some(name), summary: Some(summary), .. }
                if name == "invoke_subagent" && summary.contains("Shell Runner") && summary.contains("do it")
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { summary: Some(summary), .. } if summary == SUBAGENT_NOTE
        )));
    }

    #[test]
    fn manage_task_is_a_collapsed_service_tool() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        translator.feed(&tool(
            "call_1",
            "manage_task",
            json!({"Action": "status", "TaskId": "s/task-2"}),
        ));
        let events = translator.feed(&update("call_1", "completed", None)).events;
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted { summary: Some(summary), .. } if summary == "Action: status"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { text, .. } if text == "s/task-2"
        )));
    }

    #[test]
    fn ask_question_is_exposed_to_the_adapter_as_a_question_call() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        let fed = translator.feed(&tool(
            "call_1",
            "ask_question",
            json!({"questions": [{"question": "q", "options": ["a"]}]}),
        ));
        let question = fed.question.expect("question call");
        assert_eq!(question.tool_call_id, "call_1");
        assert_eq!(question.raw_input["questions"][0]["question"], "q");
        assert!(!fed.events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "antigravity_questions_unavailable",
                ..
            }
        )));
    }

    #[test]
    fn an_mcp_tool_keeps_its_full_name_and_own_summary() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/stage0/evidence/antigravity/p-live-mcp1.jsonl");
        let text = std::fs::read_to_string(path).unwrap();
        let mut updates = Vec::new();
        for line in text.lines() {
            let entry: Value = serde_json::from_str(line).unwrap();
            let frame: Value = serde_json::from_str(entry["line"].as_str().unwrap()).unwrap();
            let update = &frame["params"]["update"];
            if update["toolCallId"] == "call_404323" {
                updates.push(update.clone());
            }
        }
        assert_eq!(updates.len(), 2, "tool_call and completed");
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        let mut events = translator.feed(&updates[0]).events;
        assert!(events.is_empty(), "the call is held until a status frame");
        events.extend(translator.feed(&updates[1]).events);
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted { tool_name: Some(name), summary: Some(summary), .. }
                if name == "mcp_echo-probe_echo" && summary == "Echo probe call"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { text, .. } if text.contains("pong-7391")
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AdapterEvent::Diagnostic { .. }))
        );
    }

    #[test]
    fn internal_updates_are_silent_and_keep_the_title() {
        let mut translator = Translator::new("m");
        translator.begin_turn(None);
        assert!(
            translator
                .feed(&json!({"sessionUpdate": "session_info_update", "title": "My Session"}))
                .events
                .is_empty()
        );
        assert_eq!(translator.title(), Some("My Session"));
        assert!(
            translator
                .feed(
                    &json!({"sessionUpdate": "available_commands_update", "availableCommands": []})
                )
                .events
                .is_empty()
        );
        assert!(
            translator
                .feed(&json!({"sessionUpdate": "config_option_update", "configOptions": []}))
                .events
                .is_empty()
        );
    }

    #[test]
    fn an_unknown_update_is_an_optional_diagnostic() {
        let mut translator = Translator::new("m");
        let events = translator
            .feed(&json!({"sessionUpdate": "plan", "entries": []}))
            .events;
        assert!(matches!(
            events[0],
            AdapterEvent::Diagnostic {
                kind: "unknown_session_update",
                required: false,
                ..
            }
        ));
    }

    #[test]
    fn a_malformed_required_frame_is_a_required_diagnostic() {
        let mut translator = Translator::new("m");
        assert!(matches!(
            translator
                .feed(&json!({"sessionUpdate": "agent_message_chunk"}))
                .events[0],
            AdapterEvent::Diagnostic {
                kind: "malformed_session_update",
                required: true,
                ..
            }
        ));
        assert!(matches!(
            translator
                .feed(&json!({"sessionUpdate": "tool_call", "name": "run_command"}))
                .events[0],
            AdapterEvent::Diagnostic { required: true, .. }
        ));
    }
}
