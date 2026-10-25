//! Сообщения stream-json Claude Code: разбор вывода и построение ввода.
//!
//! Формы взяты из официального справочника Agent SDK (SDKMessage и др.);
//! неизвестные типы не роняют разбор, а классифицируются как Other.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    SystemInit,
    System(String),
    StreamEvent,
    Assistant,
    User,
    Result,
    ControlRequest,
    ControlResponse,
    Other(String),
}

pub fn classify(v: &Value) -> Kind {
    let ty = v.get("type").and_then(Value::as_str);
    let subtype = v.get("subtype").and_then(Value::as_str);
    match (ty, subtype) {
        (Some("system"), Some("init")) => Kind::SystemInit,
        (Some("system"), sub) => Kind::System(sub.unwrap_or("<none>").to_owned()),
        (Some("stream_event"), _) => Kind::StreamEvent,
        (Some("assistant"), _) => Kind::Assistant,
        (Some("user"), _) => Kind::User,
        (Some("result"), _) => Kind::Result,
        (Some("control_request"), _) => Kind::ControlRequest,
        (Some("control_response"), _) => Kind::ControlResponse,
        (Some(other), _) => Kind::Other(other.to_owned()),
        (None, _) => Kind::Other("<no type>".to_owned()),
    }
}

/// Пользовательский ввод (SDKUserMessage) со своим UUID.
pub fn user_message(uuid: &str, text: &str) -> Value {
    user_message_blocks(uuid, Value::String(text.to_owned()))
}

/// Пользовательский ввод с произвольными блоками содержимого (например, изображение).
pub fn user_message_blocks(uuid: &str, blocks: Value) -> Value {
    json!({
        "type": "user",
        "uuid": uuid,
        "message": {"role": "user", "content": blocks},
        "parent_tool_use_id": null,
    })
}

/// UNDOCUMENTED (частично): конверт control_request описан в справочнике SDK,
/// а ключ `subtype` во вложенном запросе выведен по аналогии и не описан явно.
pub fn control_interrupt(request_id: &str, cancel_queued: bool) -> Value {
    let mut request = json!({"subtype": "interrupt"});
    if cancel_queued {
        request["cancel_queued"] = Value::Bool(true);
    }
    json!({"type": "control_request", "request_id": request_id, "request": request})
}

pub fn session_id(v: &Value) -> Option<&str> {
    v.get("session_id").and_then(Value::as_str)
}

pub fn uuid(v: &Value) -> Option<&str> {
    v.get("uuid").and_then(Value::as_str)
}

pub fn is_replay(v: &Value) -> bool {
    v.get("isReplay").and_then(Value::as_bool).unwrap_or(false)
}

/// UUID пользовательских сообщений, на которые отвечает кадр:
/// `user_message_uuids`, а при его отсутствии `user_message_uuid`.
pub fn answered_uuids(v: &Value) -> Vec<String> {
    if let Some(list) = v.get("user_message_uuids").and_then(Value::as_array) {
        return list
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    v.get("user_message_uuid")
        .and_then(Value::as_str)
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default()
}

/// Текстовая дельта из stream_event (content_block_delta / text_delta).
pub fn text_delta(v: &Value) -> Option<&str> {
    let event = v.get("event")?;
    if event.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type")?.as_str()? != "text_delta" {
        return None;
    }
    delta.get("text")?.as_str()
}

/// Склеенный текст блоков `text` из assistant-сообщения.
pub fn assistant_text(v: &Value) -> String {
    content_blocks(v)
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub is_error: bool,
    pub text: String,
}

pub fn tool_uses(v: &Value) -> Vec<ToolUse> {
    content_blocks(v)
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|b| ToolUse {
            id: str_field(b, "id"),
            name: str_field(b, "name"),
            input: b.get("input").cloned().unwrap_or(Value::Null),
        })
        .collect()
}

pub fn tool_results(v: &Value) -> Vec<ToolResult> {
    content_blocks(v)
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|b| ToolResult {
            tool_use_id: str_field(b, "tool_use_id"),
            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            text: result_text(b.get("content")),
        })
        .collect()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Сборка входа инструмента из `input_json_delta`: до `content_block_stop`
/// частичный JSON не считается готовым объектом (AT-B-07).
#[derive(Default)]
pub struct ToolInputAssembler {
    open: std::collections::HashMap<u64, OpenInput>,
    pub done: Vec<AssembledInput>,
}

struct OpenInput {
    id: String,
    name: String,
    buf: String,
    pieces: usize,
}

#[derive(Debug, Clone)]
pub struct AssembledInput {
    pub tool_use_id: String,
    pub name: String,
    pub pieces: usize,
    pub input: Result<Value, String>,
}

impl ToolInputAssembler {
    pub fn observe(&mut self, v: &Value) {
        if classify(v) != Kind::StreamEvent {
            return;
        }
        let Some(event) = v.get("event") else { return };
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => self.open.clear(),
            Some("content_block_start") => {
                let block = &event["content_block"];
                if block["type"] == "tool_use" {
                    self.open.insert(
                        index,
                        OpenInput {
                            id: str_field(block, "id"),
                            name: str_field(block, "name"),
                            buf: String::new(),
                            pieces: 0,
                        },
                    );
                }
            }
            Some("content_block_delta") => {
                if event["delta"]["type"] == "input_json_delta"
                    && let Some(open) = self.open.get_mut(&index)
                {
                    open.buf
                        .push_str(event["delta"]["partial_json"].as_str().unwrap_or_default());
                    open.pieces += 1;
                }
            }
            Some("content_block_stop") => {
                if let Some(open) = self.open.remove(&index) {
                    let input = if open.buf.is_empty() {
                        Ok(json!({}))
                    } else {
                        serde_json::from_str(&open.buf).map_err(|e| e.to_string())
                    };
                    self.done.push(AssembledInput {
                        tool_use_id: open.id,
                        name: open.name,
                        pieces: open.pieces,
                        input,
                    });
                }
            }
            _ => {}
        }
    }

    /// Блоки, которые начались, но ещё не завершились.
    pub fn pending(&self) -> usize {
        self.open.len()
    }
}

fn content_blocks(v: &Value) -> &[Value] {
    v.pointer("/message/content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("json")
    }

    #[test]
    fn classifies_documented_message_types() {
        assert_eq!(
            classify(&parse(
                r#"{"type":"system","subtype":"init","session_id":"s"}"#
            )),
            Kind::SystemInit
        );
        assert_eq!(
            classify(&parse(r#"{"type":"system","subtype":"api_retry"}"#)),
            Kind::System("api_retry".into())
        );
        assert_eq!(
            classify(&parse(r#"{"type":"stream_event","event":{}}"#)),
            Kind::StreamEvent
        );
        assert_eq!(
            classify(&parse(r#"{"type":"assistant","message":{}}"#)),
            Kind::Assistant
        );
        assert_eq!(
            classify(&parse(r#"{"type":"user","message":{}}"#)),
            Kind::User
        );
        assert_eq!(
            classify(&parse(r#"{"type":"result","subtype":"success"}"#)),
            Kind::Result
        );
        assert_eq!(
            classify(&parse(r#"{"type":"control_response","response":{}}"#)),
            Kind::ControlResponse
        );
        assert_eq!(
            classify(&parse(r#"{"type":"control_request","request":{}}"#)),
            Kind::ControlRequest
        );
        assert_eq!(
            classify(&parse(r#"{"type":"rate_limit_event"}"#)),
            Kind::Other("rate_limit_event".into())
        );
        assert_eq!(
            classify(&parse(r#"[1,2]"#)),
            Kind::Other("<no type>".into())
        );
    }

    #[test]
    fn builds_user_message_with_uuid() {
        let v = user_message("11111111-1111-4111-8111-111111111111", "привет");
        assert_eq!(v["type"], "user");
        assert_eq!(v["uuid"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "привет");
        assert!(v["parent_tool_use_id"].is_null());
        let line = serde_json::to_string(&v).expect("serialize");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn builds_interrupt_control_request() {
        let v = control_interrupt("req-1", true);
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "req-1");
        assert_eq!(v["request"]["subtype"], "interrupt");
        assert_eq!(v["request"]["cancel_queued"], true);
        let plain = control_interrupt("req-2", false);
        assert!(plain["request"].get("cancel_queued").is_none());
    }

    #[test]
    fn answered_uuids_prefers_list_and_falls_back_to_single() {
        let both = parse(r#"{"user_message_uuid":"b","user_message_uuids":["a","b"]}"#);
        assert_eq!(answered_uuids(&both), vec!["a", "b"]);
        let single = parse(r#"{"user_message_uuid":"c"}"#);
        assert_eq!(answered_uuids(&single), vec!["c"]);
        assert!(answered_uuids(&parse("{}")).is_empty());
    }

    #[test]
    fn extracts_text_delta_only_from_text_deltas() {
        let delta = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"при"}}}"#,
        );
        assert_eq!(text_delta(&delta), Some("при"));
        let json_delta = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"co"}}}"#,
        );
        assert_eq!(text_delta(&json_delta), None);
    }

    #[test]
    fn extracts_assistant_text_and_tool_uses() {
        let v = parse(
            r#"{"type":"assistant","message":{"content":[
                {"type":"text","text":"Run "},
                {"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}},
                {"type":"text","text":"now"}]}}"#,
        );
        assert_eq!(assistant_text(&v), "Run now");
        assert_eq!(
            tool_uses(&v),
            vec![ToolUse {
                id: "toolu_1".into(),
                name: "Bash".into(),
                input: json!({"command":"ls"})
            }]
        );
    }

    #[test]
    fn extracts_tool_results_with_error_flag_and_text() {
        let v = parse(
            r#"{"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"toolu_1","content":"a.txt\nb.txt"},
                {"type":"tool_result","tool_use_id":"toolu_2","is_error":true,
                 "content":[{"type":"text","text":"File does not exist."}]}]}}"#,
        );
        let got = tool_results(&v);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].tool_use_id, "toolu_1");
        assert!(!got[0].is_error);
        assert_eq!(got[0].text, "a.txt\nb.txt");
        assert!(got[1].is_error);
        assert_eq!(got[1].text, "File does not exist.");
    }

    #[test]
    fn assembles_tool_input_only_at_block_stop() {
        let frames = [
            r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_9","name":"Bash","input":{}}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"and\":\"ls -a\"}"}}}"#,
        ];
        let mut asm = ToolInputAssembler::default();
        for f in frames {
            asm.observe(&parse(f));
        }
        assert!(asm.done.is_empty(), "incomplete input must not be emitted");
        assert_eq!(asm.pending(), 1);
        asm.observe(&parse(
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        ));
        assert_eq!(asm.done.len(), 1);
        let d = &asm.done[0];
        assert_eq!(
            (d.tool_use_id.as_str(), d.name.as_str(), d.pieces),
            ("toolu_9", "Bash", 2)
        );
        assert_eq!(
            d.input.as_ref().expect("json"),
            &json!({"command": "ls -a"})
        );
    }

    #[test]
    fn truncated_tool_input_is_an_error_not_an_object() {
        let mut asm = ToolInputAssembler::default();
        asm.observe(&parse(r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"Read","input":{}}}}"#));
        asm.observe(&parse(r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"file_pa"}}}"#));
        asm.observe(&parse(
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
        ));
        assert!(asm.done[0].input.is_err());
    }

    #[test]
    fn replay_flag() {
        assert!(is_replay(&parse(r#"{"type":"user","isReplay":true}"#)));
        assert!(!is_replay(&parse(r#"{"type":"user"}"#)));
    }
}
