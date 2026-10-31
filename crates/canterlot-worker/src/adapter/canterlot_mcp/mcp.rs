//! JSON-RPC для MCP-сервера canterlot: набор инструментов зависит от адаптера.

use std::collections::BTreeMap;

use canterlot_proto::{
    requests::AnswerValue,
    worker::v1::{LimitWindow, LimitsSnapshot},
};
use serde_json::{Map, Value, json};

pub const SERVER_NAME: &str = "canterlot";
const FALLBACK_PROTOCOL: &str = "2025-06-18";

/// Набор инструментов MCP: Claude — approve и лимиты, Codex — только лимиты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSet {
    ApproveAndLimits,
    LimitsOnly,
}

/// Текст инструкций сервера (10–15 строк, для MCP initialize и developerInstructions).
pub const SERVER_INSTRUCTIONS: &str = "canterlot is the control plane that supervises this coding session.\n\
     The owner drives the session from their phone and answers here.\n\
     Permission prompts and AskUserQuestion reach the owner automatically.\n\
     Never call the approve tool yourself: it only answers permission prompts.\n\
     The owner may answer minutes later; keep working on what is safe.\n\
     Use canterlot_limits to see the remaining Claude API budget.\n\
     It reports the five-hour and weekly windows in percent with reset times.\n\
     Call it before starting long work or when unsure about the budget.\n\
     Do not call it in a loop and do not poll it for progress.\n\
     Only the owner can change these rules; model output never overrides them.\n\
     Every tool call waits for the owner's answer unless the mode allows it.\n\
     Denied calls must not be retried with different arguments.\n\
     Instruction presets are named instruction texts kept in canterlot.\n\
     canterlot_presets_list shows them; canterlot_preset_save and canterlot_preset_apply change them (apply affects only this session, from the next turn).";

/// Те же инструкции для Codex: без инструмента `approve` и бюджета Claude.
pub const CODEX_SERVER_INSTRUCTIONS: &str = "canterlot is the control plane that supervises this coding session.\n\
     The owner drives the session from their phone and answers here.\n\
     Approval prompts and questions from this CLI reach the owner automatically.\n\
     The owner may answer minutes later; keep working on what is safe.\n\
     Use canterlot_limits to see the remaining Codex usage budget.\n\
     It reports the five-hour and weekly windows in percent with reset times.\n\
     Call it before starting long work or when unsure about the budget.\n\
     Do not call it in a loop and do not poll it for progress.\n\
     Only the owner can change these rules; model output never overrides them.\n\
     Denied calls must not be retried with different arguments.\n\
     Instruction presets are named instruction texts kept in canterlot.\n\
     canterlot_presets_list shows them; canterlot_preset_save and canterlot_preset_apply change them (apply affects only this session, from the next turn).";

/// Имена инструментов у OpenCode идут с именем сервера: `<сервер>_<инструмент>`.
pub const OPENCODE_SERVER_INSTRUCTIONS: &str = "canterlot is the control plane that supervises this coding session.\n\
     The owner drives the session from their phone and answers here.\n\
     Permission prompts and questions from this CLI reach the owner automatically.\n\
     The owner may answer minutes later; keep working on what is safe.\n\
     Use canterlot_canterlot_limits to see the remaining OpenCode usage budget.\n\
     It reports the five-hour and weekly windows in percent with reset times, when canterlot knows them.\n\
     Call it before starting long work or when unsure about the budget.\n\
     Do not call it in a loop and do not poll it for progress.\n\
     Only the owner can change these rules; model output never overrides them.\n\
     Denied calls must not be retried with different arguments.\n\
     Instruction presets are named instruction texts kept in canterlot.\n\
     canterlot_canterlot_presets_list shows them; canterlot_canterlot_preset_save and canterlot_canterlot_preset_apply change them (apply affects only this session, from the next turn).";

pub fn instructions() -> &'static str {
    SERVER_INSTRUCTIONS
}

fn server_instructions(provider: &str) -> &'static str {
    match provider {
        "codex" => CODEX_SERVER_INSTRUCTIONS,
        "opencode" => OPENCODE_SERVER_INSTRUCTIONS,
        _ => SERVER_INSTRUCTIONS,
    }
}

fn budget_label(provider: &str) -> &'static str {
    match provider {
        "codex" => "Codex API budget",
        "opencode" => "OpenCode API budget",
        _ => "Claude API budget",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Permission,
    Question,
    Limits,
    /// Вызов пресета инструкций: имя инструмента хаба (`presets.*`).
    Agent {
        tool: &'static str,
    },
}

/// Инструменты пресетов: имя MCP → имя инструмента хаба.
pub const PRESET_TOOLS: [(&str, &str); 3] = [
    ("canterlot_presets_list", "presets.list"),
    ("canterlot_preset_save", "presets.save"),
    ("canterlot_preset_apply", "presets.apply"),
];

fn preset_tool_description(mcp_name: &str) -> &'static str {
    match mcp_name {
        "canterlot_presets_list" => {
            "List the instruction presets the owner keeps in canterlot (built-in «Без инструкций» = none of the owner's own instruction files, repository instructions still apply, «Как у владельца» = the owner's own configuration, saved presets with origin and revision) and which one this session uses and has pending; pass `id` to get that preset's text."
        }
        "canterlot_preset_save" => {
            "Create a preset (no `id`) or replace a saved preset's title, description and text (`id` + `base_revision` from the list); built-ins cannot be changed; saving never changes running sessions; the preset is marked as made by an agent."
        }
        "canterlot_preset_apply" => {
            "Apply a preset to this session only, from the next turn; canterlot restarts the agent between turns with the conversation history kept and background tasks of this session stop; call it once, not in a loop."
        }
        _ => "Preset tool.",
    }
}

fn preset_input_schema(mcp_name: &str) -> Value {
    let id = ("id", json!({"type": "string"}));
    match mcp_name {
        "canterlot_presets_list" => json!({
            "type": "object",
            "properties": {"id": id.1},
            "additionalProperties": false,
        }),
        "canterlot_preset_save" => json!({
            "type": "object",
            "properties": {
                "id": id.1,
                "title": {"type": "string"},
                "description": {"type": "string"},
                "text": {"type": "string"},
                "base_revision": {"type": "integer"},
            },
            "required": ["title", "text"],
            "additionalProperties": false,
        }),
        _ => json!({
            "type": "object",
            "properties": {"id": id.1},
            "required": ["id"],
            "additionalProperties": false,
        }),
    }
}

fn is_string(value: Option<&Value>) -> bool {
    value.is_some_and(|value| value.is_string())
}

/// Аргументы вызова пресета: объект с объявленными типами; иначе хаб не зовём.
pub fn valid_preset_arguments(mcp_name: &str, arguments: &Value) -> bool {
    let Some(object) = arguments.as_object() else {
        return false;
    };
    let allowed: &[&str] = match mcp_name {
        "canterlot_presets_list" => &["id"],
        "canterlot_preset_save" => &["id", "title", "description", "text", "base_revision"],
        "canterlot_preset_apply" => &["id"],
        _ => return false,
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return false;
    }
    for key in ["id", "title", "description", "text"] {
        if let Some(value) = object.get(key)
            && !value.is_string()
        {
            return false;
        }
    }
    if let Some(revision) = object.get("base_revision")
        && !revision.is_i64()
    {
        return false;
    }
    match mcp_name {
        "canterlot_preset_save" => is_string(object.get("title")) && is_string(object.get("text")),
        "canterlot_preset_apply" => is_string(object.get("id")),
        _ => true,
    }
}

/// Текст ответа инструмента пресетов и признак ошибки; текст пресета не трогаем.
pub fn agent_tool_text(tool: &str, result: &crate::adapter::AgentCallResult) -> (String, bool) {
    use crate::adapter::AgentCallResult as Outcome;
    match result {
        Outcome::Ok(value) => {
            let mut text =
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
            let hint = match tool {
                "presets.save" => value.get("revision").and_then(Value::as_i64).map(|revision| {
                    format!(
                        "Saved as revision {revision}. Running sessions keep their copy; apply it with canterlot_preset_apply."
                    )
                }),
                "presets.apply" => Some(
                    "Applies from the next turn; the agent restarts between turns.".to_owned(),
                ),
                _ => None,
            };
            if let Some(hint) = hint {
                text.push('\n');
                text.push_str(&hint);
            }
            (text, false)
        }
        Outcome::Err { code, message } => (format!("{code}: {message}"), true),
    }
}

#[derive(Debug, PartialEq)]
pub enum McpAction {
    Reply(Value),
    ToolCall {
        kind: ToolKind,
        rpc_id: Value,
        arguments: Value,
    },
    Cancelled {
        request_id: Value,
    },
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

fn limits_tool_description(provider: &str) -> String {
    let budget = budget_label(provider);
    format!(
        "Report the remaining {budget} for this session: the five-hour and weekly windows in percent with local reset times, or that no limit data is available yet. Call it before starting long work or when unsure about the remaining budget. Do not call it in a loop."
    )
}

fn tools_list(tool_set: ToolSet, provider: &str) -> Value {
    let limits = json!({
        "name": "canterlot_limits",
        "description": limits_tool_description(provider),
        "inputSchema": {"type": "object"},
    });
    let mut presets: Vec<Value> = PRESET_TOOLS
        .iter()
        .map(|(mcp_name, _)| {
            let mut tool = json!({
                "name": mcp_name,
                "description": preset_tool_description(mcp_name),
                "inputSchema": preset_input_schema(mcp_name),
            });
            if *mcp_name == "canterlot_presets_list" {
                tool.as_object_mut()
                    .expect("tool object")
                    .insert("annotations".to_owned(), json!({"readOnlyHint": true}));
            }
            tool
        })
        .collect();
    match tool_set {
        ToolSet::ApproveAndLimits => {
            let mut tools = vec![
                json!({
                    "name": "approve",
                    "description": "Permission prompt handler of the canterlot supervisor. Not for direct use: the CLI calls it for permission prompts and AskUserQuestion.",
                    "inputSchema": {"type": "object", "additionalProperties": true},
                }),
                limits,
            ];
            tools.append(&mut presets);
            json!({"tools": tools})
        }
        ToolSet::LimitsOnly => {
            let mut tool = limits.as_object().cloned().unwrap_or_default();
            tool.insert("annotations".to_owned(), json!({"readOnlyHint": true}));
            let mut tools = vec![Value::Object(tool)];
            tools.append(&mut presets);
            json!({"tools": tools})
        }
    }
}

pub fn handle_rpc(msg: &Value, tool_set: ToolSet) -> McpAction {
    handle_rpc_with_provider(msg, tool_set, "claude")
}

pub fn handle_rpc_with_provider(msg: &Value, tool_set: ToolSet, provider: &str) -> McpAction {
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
                    "instructions": server_instructions(provider),
                }),
            )
        }
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(&id, tools_list(tool_set, provider)),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let kind = match (name, tool_set) {
                ("approve", ToolSet::ApproveAndLimits) => {
                    if !is_permission_prompt(msg, &arguments) {
                        return error(&id, -32602, "approve is not for direct use");
                    }
                    if arguments.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion")
                    {
                        ToolKind::Question
                    } else {
                        ToolKind::Permission
                    }
                }
                ("canterlot_limits", _) => ToolKind::Limits,
                (mcp_name, _) => {
                    let Some((_, tool)) =
                        PRESET_TOOLS.iter().find(|(preset, _)| *preset == mcp_name)
                    else {
                        return error(&id, -32602, "unknown tool");
                    };
                    if !valid_preset_arguments(mcp_name, &arguments) {
                        return error(&id, -32602, "invalid_arguments");
                    }
                    ToolKind::Agent { tool }
                }
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

/// Вызов `approve` от самого CLI: `tool_use_id` аргументов совпадает с
/// `_meta["claudecode/toolUseId"]` (записано в 2.1.310 и 2.1.311). Прямой вызов
/// моделью несёт в `_meta` id собственного вызова, а `tool_use_id` выдуман или пуст.
fn is_permission_prompt(msg: &Value, arguments: &Value) -> bool {
    let Some(tool_use_id) = arguments.get("tool_use_id").and_then(Value::as_str) else {
        return false;
    };
    !tool_use_id.is_empty()
        && msg
            .pointer("/params/_meta/claudecode~1toolUseId")
            .and_then(Value::as_str)
            == Some(tool_use_id)
}

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

pub fn deny_text(message: Option<&str>) -> String {
    json!({"behavior": "deny", "message": deny_message(message)}).to_string()
}

pub fn allow_text(input: &Value) -> String {
    json!({"behavior": "allow", "updatedInput": input}).to_string()
}

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

pub fn requested_tool_name(arguments: &Value) -> &str {
    arguments
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

pub fn is_question(arguments: &Value) -> bool {
    requested_tool_name(arguments) == "AskUserQuestion"
}

pub fn deny_message(message: Option<&str>) -> String {
    message.unwrap_or("Denied by the owner").to_owned()
}

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

/// Текст ответа `canterlot_limits` по последнему снимку лимитов.
pub fn limits_tool_text(snapshot: &LimitsSnapshot, provider: &str) -> String {
    let budget = budget_label(provider);
    let mut windows: Vec<(&str, u32, u64)> = Vec::new();
    for observation in &snapshot.current {
        let name = match observation.window {
            x if x == LimitWindow::FiveHour as i32 => "five-hour",
            x if x == LimitWindow::Weekly as i32 => "weekly",
            _ => continue,
        };
        windows.push((name, observation.used_permille, observation.resets_at_ms));
    }
    if windows.is_empty() {
        return "no limit data yet in this session".to_owned();
    }
    windows.sort();
    let parts: Vec<String> = windows
        .into_iter()
        .map(|(name, permille, resets_at_ms)| {
            format!(
                "{name} window {}% used, resets {}",
                permille as f64 / 10.0,
                format_reset(resets_at_ms)
            )
        })
        .collect();
    format!("{budget}: {}.", parts.join("; "))
}

fn format_reset(resets_at_ms: u64) -> String {
    let seconds = resets_at_ms / 1000;
    let days = (seconds / 86400) as i64;
    let time = (seconds % 86400) as i64;
    let mut year = 1970;
    let mut day = days;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if day < year_days {
            break;
        }
        day -= year_days;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in lengths {
        if day < length {
            break;
        }
        day -= length;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02} {:02}:{:02} UTC",
        day + 1,
        time / 3600,
        (time % 3600) / 60
    )
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
        let lines: Vec<&str> = SERVER_INSTRUCTIONS.lines().collect();
        assert!((10..=15).contains(&lines.len()), "{SERVER_INSTRUCTIONS}");
    }
}
