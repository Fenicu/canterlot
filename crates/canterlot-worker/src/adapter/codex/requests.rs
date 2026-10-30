//! Запросы сервера Codex: разрешения на команды и правки, структурированные
//! вопросы, elicitation MCP и остальные методы. Ответ строится только из явного
//! решения владельца: авто-allow нет, при сбросе очередь умирает без ответов.
use super::super::{AdapterEvent, valid_decision};
use super::rpc::{RpcClient, RpcId};
use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

pub struct RouteCtx<'a> {
    pub agent: &'a str,
    pub item: Option<&'a Value>,
}

impl fmt::Debug for RouteCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouteCtx")
            .field("agent", &self.agent)
            .field("has_item", &self.item.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PermKind {
    Command,
    FileChange,
    Permissions,
    Elicitation,
}

struct Pending {
    rpc_id: RpcId,
    kind: String,
    perm: Option<PermKind>,
    payload: Value,
    item_id: String,
    available_decisions: Option<Value>,
    question_map: Vec<(String, String)>,
    requested: Value,
    elicitation_url_mode: bool,
    answered: bool,
}

impl fmt::Debug for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending")
            .field("rpc_id", &self.rpc_id)
            .field("kind", &self.kind)
            .field("item_id", &self.item_id)
            .field("answered", &self.answered)
            .finish_non_exhaustive()
    }
}

pub enum Intake {
    Open(AdapterEvent),
    Reply(Value),
}

impl fmt::Debug for Intake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Intake::Open(event) => f.debug_tuple("Open").field(event).finish(),
            // Ответ может нести текст решений: не показываем.
            Intake::Reply(_) => f.write_str("Reply"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Ours { request_id: String },
    Cleared { request_id: String },
}

pub struct Requests {
    generation: u64,
    pending: HashMap<String, Pending>,
    rpc_index: HashMap<RpcId, String>,
    diagnostics: Vec<AdapterEvent>,
    seen_unsupported: HashSet<String>,
}

impl fmt::Debug for Requests {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Requests")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

// Проверка решения по контракту 1.3c (для `deliver_decision` задачи 7).
pub fn check_decision(kind: &str, payload: &Value, decision: &DecisionPayload) -> bool {
    valid_decision(kind, payload, decision)
}

// «Разрешено правилами CLI»: пункт без запроса при политике не `never`
// и песочнице не `dangerFullAccess`. Вызывает задача 7 для завершившихся
// commandExecution/fileChange с `approved_by_cli`.
pub fn cli_policy_mark(approval_policy: &Value, sandbox: &Value) -> Option<&'static str> {
    match approval_policy.as_str() {
        Some("never") | None => None,
        Some(_) => {
            if is_danger_full_access(sandbox) {
                None
            } else {
                Some("cli_policy")
            }
        }
    }
}

fn is_danger_full_access(sandbox: &Value) -> bool {
    match sandbox {
        Value::String(mode) => normalize_policy(mode) == "dangerfullaccess",
        Value::Object(map) => map
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|mode| normalize_policy(mode) == "dangerfullaccess"),
        _ => false,
    }
}

fn normalize_policy(mode: &str) -> String {
    mode.to_lowercase()
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .collect()
}

fn rpc_id_string(id: &RpcId) -> String {
    match id {
        RpcId::Int(n) => n.to_string(),
        RpcId::Str(s) => s.clone(),
    }
}

fn request_id(item_id: &str, id: &RpcId) -> String {
    format!("{item_id}/{}", rpc_id_string(id))
}

impl Requests {
    /// Новый app-server: его id серверных запросов снова с нуля.
    pub fn next_generation(&mut self) {
        self.generation += 1;
    }

    pub fn new() -> Self {
        Self {
            generation: 0,
            pending: HashMap::new(),
            rpc_index: HashMap::new(),
            diagnostics: Vec::new(),
            seen_unsupported: HashSet::new(),
        }
    }

    pub fn is_pending(&self, request_id: &str) -> bool {
        self.pending.contains_key(request_id)
    }

    // Вид, груз и элемент для `deliver_decision` задачи 7.
    pub fn pending_info(&self, request_id: &str) -> Option<(String, Value, String)> {
        let pending = self.pending.get(request_id)?;
        Some((
            pending.kind.clone(),
            pending.payload.clone(),
            pending.item_id.clone(),
        ))
    }

    fn track(&mut self, request_id: String, pending: Pending) {
        let rpc_id = pending.rpc_id.clone();
        self.pending.insert(request_id.clone(), pending);
        self.rpc_index.insert(rpc_id, request_id);
    }

    pub fn on_server_request(
        &mut self,
        id: RpcId,
        method: &str,
        params: &Value,
        ctx: &RouteCtx,
    ) -> Intake {
        match method {
            "item/commandExecution/requestApproval" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request_id = request_id(item_id, &id);
                let payload = command_payload(params, item_id, ctx.agent);
                self.track(
                    request_id.clone(),
                    Pending {
                        rpc_id: id,
                        kind: "permission".into(),
                        payload: payload.clone(),
                        perm: Some(PermKind::Command),
                        item_id: item_id.to_owned(),
                        available_decisions: params.get("availableDecisions").cloned(),
                        question_map: Vec::new(),
                        requested: Value::Null,
                        elicitation_url_mode: false,
                        answered: false,
                    },
                );
                Intake::Open(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "permission".into(),
                    payload: payload.clone(),
                })
            }
            "item/fileChange/requestApproval" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request_id = request_id(item_id, &id);
                let payload = file_change_payload(params, item_id, ctx);
                self.track(
                    request_id.clone(),
                    Pending {
                        rpc_id: id,
                        kind: "permission".into(),
                        payload: payload.clone(),
                        perm: Some(PermKind::FileChange),
                        item_id: item_id.to_owned(),
                        available_decisions: None,
                        question_map: Vec::new(),
                        requested: Value::Null,
                        elicitation_url_mode: false,
                        answered: false,
                    },
                );
                Intake::Open(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "permission".into(),
                    payload: payload.clone(),
                })
            }
            "item/permissions/requestApproval" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or(method);
                let request_id = request_id(item_id, &id);
                let payload = permissions_payload(params, item_id, ctx.agent);
                self.track(
                    request_id.clone(),
                    Pending {
                        rpc_id: id,
                        kind: "permission".into(),
                        payload: payload.clone(),
                        perm: Some(PermKind::Permissions),
                        item_id: item_id.to_owned(),
                        available_decisions: None,
                        question_map: Vec::new(),
                        requested: params.get("permissions").cloned().unwrap_or(Value::Null),
                        elicitation_url_mode: false,
                        answered: false,
                    },
                );
                Intake::Open(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "permission".into(),
                    payload: payload.clone(),
                })
            }
            "item/tool/requestUserInput" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request_id = request_id(item_id, &id);
                let (payload, map) = question_payload(params, item_id, ctx.agent);
                self.track(
                    request_id.clone(),
                    Pending {
                        rpc_id: id,
                        kind: "question".into(),
                        payload: payload.clone(),
                        perm: None,
                        item_id: item_id.to_owned(),
                        available_decisions: None,
                        question_map: map,
                        requested: Value::Null,
                        elicitation_url_mode: false,
                        answered: false,
                    },
                );
                Intake::Open(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "question".into(),
                    payload,
                })
            }
            "mcpServer/elicitation/request" => {
                // Без itemId (схема McpServerElicitationRequestParams) id запроса
                // различает процессы поколением: id JSON-RPC повторяются.
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("{method}@{}", self.generation));
                let item_id = item_id.as_str();
                let request_id = request_id(item_id, &id);
                let url_mode = params.get("mode").and_then(Value::as_str) == Some("url");
                let payload = elicitation_payload(params, item_id, ctx.agent);
                self.track(
                    request_id.clone(),
                    Pending {
                        rpc_id: id,
                        kind: "permission".into(),
                        payload: payload.clone(),
                        perm: Some(PermKind::Elicitation),
                        item_id: item_id.to_owned(),
                        available_decisions: None,
                        question_map: Vec::new(),
                        requested: Value::Null,
                        elicitation_url_mode: url_mode,
                        answered: false,
                    },
                );
                Intake::Open(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "permission".into(),
                    payload: payload.clone(),
                })
            }
            _ => {
                if self.seen_unsupported.insert(method.to_owned()) {
                    self.diagnostics.push(AdapterEvent::Diagnostic {
                        kind: "unsupported_server_request",
                        event_type: method.to_owned(),
                        required: false,
                    });
                }
                Intake::Reply(RpcClient::error(&id, -32601, "unsupported server request"))
            }
        }
    }

    // Строка ответа для записи в app-server; ожидание `resolved` — в задаче 7.
    pub fn answer(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> Result<Value, &'static str> {
        let pending = self
            .pending
            .get_mut(request_id)
            .filter(|pending| !pending.answered)
            .ok_or("request_not_pending")?;
        let result = match (pending.perm, decision) {
            (Some(perm), DecisionPayload::Permission { behavior, .. }) => {
                permission_answer(perm, pending, behavior.clone())
            }
            (None, DecisionPayload::Question { answers }) => question_answer(pending, answers),
            _ => return Err("decision_mismatch"),
        };
        pending.answered = true;
        Ok(RpcClient::result(&pending.rpc_id, result))
    }

    pub fn on_resolved(&mut self, rpc_id: &RpcId) -> Option<Resolved> {
        let request_id = self.rpc_index.remove(rpc_id)?;
        let pending = self.pending.remove(&request_id)?;
        if pending.answered {
            Some(Resolved::Ours { request_id })
        } else {
            Some(Resolved::Cleared { request_id })
        }
    }

    // Прерывание и конец хода: хозяин уже закрыл запросы, серверу — ничего.
    pub fn drop_all(&mut self) {
        self.pending.clear();
        self.rpc_index.clear();
    }

    pub fn drain_diagnostics(&mut self) -> Vec<AdapterEvent> {
        std::mem::take(&mut self.diagnostics)
    }
}

impl Default for Requests {
    fn default() -> Self {
        Self::new()
    }
}

// Отказ без предложенного `decline` прерывает и ход: помечаем карточку.
fn decline_offered(available: &Option<Value>) -> bool {
    match available {
        None => true,
        Some(Value::Array(decisions)) => decisions.iter().any(|decision| decision == "decline"),
        Some(_) => true,
    }
}

fn input_without_ids(params: &Value) -> Value {
    match params.as_object() {
        Some(map) => map
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "threadId" | "turnId" | "itemId"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<String, Value>>()
            .into(),
        None => Value::Null,
    }
}

fn command_payload(params: &Value, item_id: &str, agent: &str) -> Value {
    let available = params.get("availableDecisions").cloned();
    serde_json::json!({
        "tool_name": "shell",
        "command": params.get("command").cloned().unwrap_or(Value::Null),
        "cwd": params.get("cwd").cloned().unwrap_or(Value::Null),
        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
        "command_actions": params.get("commandActions").cloned().unwrap_or(Value::Null),
        "available_decisions": available.clone().unwrap_or(Value::Null),
        "network": params.get("networkApprovalContext").cloned().unwrap_or(Value::Null),
        "input": input_without_ids(params),
        "tool_use_id": item_id,
        "agent": agent,
        "deny_also_interrupts": !decline_offered(&available),
    })
}

fn file_change_payload(params: &Value, item_id: &str, ctx: &RouteCtx) -> Value {
    serde_json::json!({
        "tool_name": "apply_patch",
        "changes": ctx.item.and_then(|item| item.get("changes").cloned()).unwrap_or(Value::Null),
        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
        "grant_root": params.get("grantRoot").cloned().unwrap_or(Value::Null),
        "tool_use_id": item_id,
        "agent": ctx.agent,
        "deny_also_interrupts": false,
    })
}

fn permissions_payload(params: &Value, item_id: &str, agent: &str) -> Value {
    serde_json::json!({
        "tool_name": "permissions",
        "permissions": params.get("permissions").cloned().unwrap_or(Value::Null),
        "reason": params.get("reason").cloned().unwrap_or(Value::Null),
        "cwd": params.get("cwd").cloned().unwrap_or(Value::Null),
        "tool_use_id": item_id,
        "agent": agent,
        "deny_also_interrupts": false,
    })
}

fn elicitation_payload(params: &Value, item_id: &str, agent: &str) -> Value {
    serde_json::json!({
        "tool_name": "mcp_elicitation",
        "server": params.get("server").or_else(|| params.get("serverName")).cloned().unwrap_or(Value::Null),
        "message": params.get("message").cloned().unwrap_or(Value::Null),
        "mode": params.get("mode").cloned().unwrap_or(Value::Null),
        "url": params.get("url").cloned().unwrap_or(Value::Null),
        "schema": params
            .get("requestedSchema")
            .or_else(|| params.get("schema"))
            .cloned()
            .unwrap_or(Value::Null),
        "tool_use_id": item_id,
        "agent": agent,
    })
}

fn question_payload(params: &Value, item_id: &str, agent: &str) -> (Value, Vec<(String, String)>) {
    let mut map = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let questions: Vec<Value> = params
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .enumerate()
                .map(|(index, question)| {
                    let codex_id = question.get("id").and_then(Value::as_str).unwrap_or_default();
                    let canterlot_id = if !codex_id.is_empty() && seen.insert(codex_id.to_owned()) {
                        codex_id.to_owned()
                    } else {
                        format!("q{index}")
                    };
                    map.push((canterlot_id.clone(), codex_id.to_owned()));
                    let options = match question.get("options") {
                        Some(Value::Array(options)) => Value::Array(
                            options
                                .iter()
                                .map(|option| {
                                    serde_json::json!({
                                        "label": option.get("label").cloned().unwrap_or(Value::Null),
                                        "description": option.get("description").cloned().unwrap_or(Value::Null),
                                    })
                                })
                                .collect(),
                        ),
                        _ => Value::Null,
                    };
                    let has_options = question
                        .get("options")
                        .and_then(Value::as_array)
                        .is_some();
                    serde_json::json!({
                        "id": canterlot_id,
                        "text": question.get("question").cloned().unwrap_or(Value::Null),
                        "header": question.get("header").cloned().unwrap_or(Value::Null),
                        "options": options,
                        "multi_select": false,
                        "allow_custom": question.get("isOther").and_then(Value::as_bool).unwrap_or(false)
                            || !has_options,
                        "is_secret": question.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (
        serde_json::json!({
            "questions": questions,
            "tool_use_id": item_id,
            "agent": agent,
        }),
        map,
    )
}

fn permission_answer(perm: PermKind, pending: &Pending, behavior: PermissionBehavior) -> Value {
    match (perm, behavior) {
        (PermKind::Command, PermissionBehavior::Allow) => {
            serde_json::json!({"decision": "accept"})
        }
        (PermKind::FileChange, PermissionBehavior::Allow) => {
            serde_json::json!({"decision": "accept"})
        }
        (PermKind::Command, PermissionBehavior::Deny) => {
            if decline_offered(&pending.available_decisions) {
                serde_json::json!({"decision": "decline"})
            } else {
                serde_json::json!({"decision": "cancel"})
            }
        }
        (PermKind::FileChange, PermissionBehavior::Deny) => {
            serde_json::json!({"decision": "decline"})
        }
        (PermKind::Permissions, PermissionBehavior::Allow) => {
            serde_json::json!({"permissions": pending.requested, "scope": "turn"})
        }
        (PermKind::Permissions, PermissionBehavior::Deny) => {
            serde_json::json!({"permissions": {}, "scope": "turn"})
        }
        (PermKind::Elicitation, PermissionBehavior::Allow) => {
            if pending.elicitation_url_mode {
                serde_json::json!({"action": "accept", "content": null, "_meta": null})
            } else {
                serde_json::json!({"action": "accept", "content": {}, "_meta": null})
            }
        }
        (PermKind::Elicitation, PermissionBehavior::Deny) => {
            serde_json::json!({"action": "decline", "content": null, "_meta": null})
        }
    }
}

fn question_answer(pending: &Pending, answers: &BTreeMap<String, AnswerValue>) -> Value {
    let mut out = serde_json::Map::new();
    for (canterlot_id, codex_id) in &pending.question_map {
        let values: Vec<String> = match answers.get(canterlot_id) {
            Some(AnswerValue::Single(value)) => vec![value.clone()],
            Some(AnswerValue::Multiple(values)) => values.clone(),
            None => continue,
        };
        out.insert(codex_id.clone(), serde_json::json!({"answers": values}));
    }
    serde_json::json!({"answers": out})
}
