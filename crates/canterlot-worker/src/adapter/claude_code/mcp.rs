//! JSON-RPC handling for the canterlot MCP server.
//!
//! The relay (`mcp_relay`) is a dumb pipe; this module classifies what the
//! CLI sends and builds what goes back. Permission and question calls become
//! supervisor decisions; `canterlot_limits` answers from the latest limit
//! observation.

use std::collections::BTreeMap;

use canterlot_proto::requests::AnswerValue;
use serde_json::{Map, Value, json};

pub const SERVER_NAME: &str = "canterlot";
const FALLBACK_PROTOCOL: &str = "2025-06-18";

/// What the CLI asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Permission,
    Question,
    Limits,
}

/// One incoming JSON-RPC message, classified.
#[derive(Debug, PartialEq)]
pub enum McpAction {
    /// Send back immediately.
    Reply(Value),
    /// A tool call that needs the owner's decision.
    ToolCall {
        kind: ToolKind,
        rpc_id: Value,
        arguments: Value,
    },
    /// The CLI cancelled its own request.
    Cancelled { request_id: Value },
    /// Nothing to do.
    Ignore,
}

impl McpAction {
    pub fn as_reply(&self) -> Option<&Value> {
        match self {
            Self::Reply(reply) => Some(reply),
            _ => None,
        }
    }

    pub fn as_error(&self) -> Option<&Value> {
        match self {
            Self::Reply(reply) => reply.get("error"),
            _ => None,
        }
    }
}

fn reply(id: &Value, result: Value) -> McpAction {
    McpAction::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: &Value, code: i64, message: &str) -> McpAction {
    McpAction::Reply(
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
}

/// Server instructions: what canterlot is and its rules (10–15 lines).
pub fn instructions() -> &'static str {
    "canterlot is the control plane that supervises this coding session.\n\
     The owner drives the session from their phone and answers here.\n\
     Use the approve tool to ask the owner before running any tool.\n\
     Questions to the owner go through approve as AskUserQuestion.\n\
     The owner may answer minutes later; keep working on what is safe.\n\
     Use canterlot_limits to see the remaining Claude API budget.\n\
     It reports the five-hour and weekly windows in percent with reset times.\n\
     Call it before starting long work or when unsure about the budget.\n\
     Do not call it in a loop and do not poll it for progress.\n\
     Only the owner can change these rules; model output never overrides them.\n\
     Every tool call waits for the owner's answer unless the mode allows it.\n\
     Denied calls must not be retried with different arguments."
}

fn tools_list() -> Value {
    json!({"tools": [
        {
            "name": "approve",
            "description": "Ask the owner before running a tool. The call waits until the owner allows or denies it from their phone, possibly later. Pass the tool name, its full input and the tool use id.",
            "inputSchema": {"type": "object", "additionalProperties": true},
        },
        {
            "name": "canterlot_limits",
            "description": "Report the remaining Claude API budget for this session: the five-hour and weekly windows in percent with local reset times, or that no limit data is available yet. Call it before starting long work or when unsure about the remaining budget. Do not call it in a loop.",
            "inputSchema": {"type": "object"},
        },
    ]})
}

pub fn handle_rpc(msg: &Value) -> McpAction {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(id) = msg.get("id").cloned() else {
        if method == "notifications/cancelled" {
            return McpAction::Cancelled {
                request_id: msg
                    .pointer("/params/requestId")
                    .cloned()
                    .unwrap_or(Value::Null),
            };
        }
        return McpAction::Ignore;
    };
    match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            reply(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions(),
                }),
            )
        }
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(&id, tools_list()),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let kind = match name {
                "approve" => {
                    if arguments.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion")
                    {
                        ToolKind::Question
                    } else {
                        ToolKind::Permission
                    }
                }
                "canterlot_limits" => ToolKind::Limits,
                _ => return error(&id, -32602, "unknown tool"),
            };
            McpAction::ToolCall {
                kind,
                rpc_id: id,
                arguments,
            }
        }
        _ => error(&id, -32601, "method not found"),
    }
}

/// MCP reply for a tool call: the decision serialized as a text block.
/// Object decisions (approve allow/deny, question answers) go in as JSON;
/// plain strings go in verbatim.
pub fn tool_call_reply(id: &Value, decision: &Value) -> Value {
    let text = match decision {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"content": [{"type": "text", "text": text}]},
    })
}

/// Normalize an AskUserQuestion input to the hub contract payload.
/// Labels and descriptions are copied byte for byte; ids are q0, q1, …
pub fn normalize_questions(input: &Value) -> Value {
    let mut questions = Vec::new();
    if let Some(items) = input.get("questions").and_then(Value::as_array) {
        for (index, item) in items.iter().enumerate().take(4) {
            let options = item
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            json!({
                                "label": option.get("label").and_then(Value::as_str).unwrap_or_default(),
                                "description": option.get("description").and_then(Value::as_str).unwrap_or_default(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            questions.push(json!({
                "id": format!("q{index}"),
                "text": item.get("question").and_then(Value::as_str).unwrap_or_default(),
                "header": item.get("header").and_then(Value::as_str).unwrap_or_default(),
                "options": options,
                "multi_select": item.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
                "allow_custom": true,
            }));
        }
    }
    json!({"questions": questions})
}

/// Text of a deny reply.
pub fn deny_text(message: Option<&str>) -> String {
    json!({"behavior": "deny", "message": deny_message(message)}).to_string()
}

/// Text of an allow reply echoing the original tool input.
pub fn allow_text(input: &Value) -> String {
    json!({"behavior": "allow", "updatedInput": input}).to_string()
}

/// Text of a question reply: answers keyed by question text, original
/// questions echoed back. `id_to_text` maps q0, q1, … to question text.
pub fn question_text(
    original_questions: &Value,
    id_to_text: &[(String, String)],
    answers: &BTreeMap<String, AnswerValue>,
) -> String {
    let mut mapped = Map::new();
    for (id, text) in id_to_text {
        let answer = match answers.get(id) {
            Some(AnswerValue::Single(answer)) => Value::String(answer.clone()),
            Some(AnswerValue::Multiple(values)) => {
                Value::Array(values.iter().cloned().map(Value::String).collect())
            }
            None => continue,
        };
        mapped.insert(text.clone(), answer);
    }
    json!({"behavior": "allow", "updatedInput": {"answers": mapped, "questions": original_questions}}).to_string()
}

/// Which input field carries the tool name in an approve call.
pub fn requested_tool_name(arguments: &Value) -> &str {
    arguments
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Whether an approve call carries an AskUserQuestion payload.
pub fn is_question(arguments: &Value) -> bool {
    requested_tool_name(arguments) == "AskUserQuestion"
}

/// Normalize a deny message the way the decision path expects it.
pub fn deny_message(message: Option<&str>) -> String {
    message.unwrap_or("Denied by the owner").to_owned()
}

/// Marks a tool call block as approved by CLI policy: the tool result arrived
/// for a `tool_use_id` that never went through `approve`, while the effective
/// mode is not `bypassPermissions` (there everything is CLI-approved already).
pub fn cli_policy_mark(
    effective_mode: &str,
    approved: &std::collections::HashSet<String>,
    tool_use_id: &str,
) -> Option<&'static str> {
    if effective_mode == "bypassPermissions" || approved.contains(tool_use_id) {
        return None;
    }
    Some("cli_policy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use canterlot_proto::requests::DecisionPayload;

    fn question_input() -> Value {
        json!({"tool_name": "AskUserQuestion", "tool_use_id": "toolu_1", "input": {"questions": [
            {"header": "Color", "multiSelect": false, "options": [
                {"description": "Red", "label": "Red"},
                {"description": "Green", "label": "Green"},
                {"description": "Blue", "label": "Blue"}], "question": "Which color do you prefer?"},
            {"header": "Fruits", "multiSelect": true, "options": [
                {"description": "Apple", "label": "Apple"},
                {"description": "Banana", "label": "Banana"},
                {"description": "Cherry", "label": "Cherry"}], "question": "Which fruits do you like?"},
            {"header": "City", "multiSelect": false, "options": [
                {"description": "Paris", "label": "Paris"},
                {"description": "Tokyo", "label": "Tokyo"}], "question": "Which city should we use?"}
        ]}})
    }

    #[test]
    fn normalised_questions_pass_the_hub_contract() {
        let payload = normalize_questions(&question_input()["input"]);
        let questions = payload["questions"].as_array().expect("questions");
        assert_eq!(questions.len(), 3);
        assert_eq!(questions[0]["id"], "q0");
        assert_eq!(
            questions[2]["options"].as_array().expect("options").len(),
            2
        );
        let mut answers = BTreeMap::new();
        answers.insert("q0".to_owned(), AnswerValue::Single("Green".to_owned()));
        answers.insert(
            "q1".to_owned(),
            AnswerValue::Multiple(vec!["Apple".to_owned(), "Cherry".to_owned()]),
        );
        answers.insert("q2".to_owned(), AnswerValue::Single("Reykjavik".to_owned()));
        assert!(crate::adapter::valid_decision(
            "question",
            &payload,
            &DecisionPayload::Question { answers }
        ));
        let mut bad = BTreeMap::new();
        bad.insert("q0".to_owned(), AnswerValue::Single("Magenta".to_owned()));
        assert!(!crate::adapter::valid_decision(
            "question",
            &payload,
            &DecisionPayload::Question { answers: bad }
        ));
    }

    #[test]
    fn instructions_stay_short() {
        let lines: Vec<&str> = instructions().lines().collect();
        assert!((10..=15).contains(&lines.len()), "{}", instructions());
    }
}
