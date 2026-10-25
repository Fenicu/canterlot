//! Типы JSON-RPC сообщений `codex app-server` (0.164.0), которые использует прототип.
//!
//! Типы написаны вручную по сгенерированной схеме (`generate-json-schema`):
//! из ~280 типов нужен небольшой набор, а неизвестные поля и варианты должны
//! переживать обновления CLI. Сырые значения всегда сохраняются в evidence.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// ID запроса JSON-RPC: сервер может присылать числа или строки.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Int(i64),
    Str(String),
}

impl RequestId {
    pub fn to_value(&self) -> Value {
        match self {
            RequestId::Int(i) => json!(i),
            RequestId::Str(s) => json!(s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// Входящее сообщение после классификации.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response {
        id: RequestId,
        result: Result<Value, RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: RequestId,
        method: String,
        params: Value,
    },
    Invalid {
        reason: String,
    },
}

/// Классифицирует строку протокола: ответ, уведомление или запрос сервера.
/// Поле `"jsonrpc"` на проводе отсутствует, поэтому опираемся на `id`/`method`.
pub fn classify(v: Value) -> Incoming {
    let Value::Object(mut map) = v else {
        return Incoming::Invalid {
            reason: "not a JSON object".into(),
        };
    };
    let method = match map.remove("method") {
        Some(Value::String(m)) => Some(m),
        Some(_) => {
            return Incoming::Invalid {
                reason: "method is not a string".into(),
            };
        }
        None => None,
    };
    let id = match map.remove("id") {
        None | Some(Value::Null) => None,
        Some(raw) => match serde_json::from_value::<RequestId>(raw) {
            Ok(id) => Some(id),
            Err(_) => {
                return Incoming::Invalid {
                    reason: "id is neither integer nor string".into(),
                };
            }
        },
    };
    let params = map.remove("params").unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => Incoming::ServerRequest { id, method, params },
        (Some(method), None) => Incoming::Notification { method, params },
        (None, Some(id)) => {
            if let Some(err) = map.remove("error") {
                match serde_json::from_value::<RpcError>(err) {
                    Ok(e) => Incoming::Response { id, result: Err(e) },
                    Err(_) => Incoming::Invalid {
                        reason: "malformed error object".into(),
                    },
                }
            } else if let Some(result) = map.remove("result") {
                Incoming::Response {
                    id,
                    result: Ok(result),
                }
            } else {
                Incoming::Invalid {
                    reason: "response without result or error".into(),
                }
            }
        }
        (None, None) => Incoming::Invalid {
            reason: "neither method nor id".into(),
        },
    }
}

pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "method": method, "params": params })
}

pub fn response(id: &RequestId, result: Value) -> Value {
    json!({ "id": id.to_value(), "result": result })
}

/// Сопоставление исходящих запросов и ответов по `id`.
#[derive(Default)]
pub struct Correlator {
    pending: HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Delivered,
    /// Ответ на неизвестный/уже закрытый запрос или со строковым id.
    Unmatched,
}

impl Correlator {
    pub fn register(&mut self, id: i64) -> oneshot::Receiver<Result<Value, RpcError>> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        rx
    }

    pub fn resolve(&mut self, id: &RequestId, result: Result<Value, RpcError>) -> Resolution {
        let RequestId::Int(n) = id else {
            return Resolution::Unmatched;
        };
        match self.pending.remove(n) {
            // получатель мог уже отвалиться по таймауту: это не ошибка протокола
            Some(tx) => {
                let _ = tx.send(result);
                Resolution::Delivered
            }
            None => Resolution::Unmatched,
        }
    }

    pub fn forget(&mut self, id: i64) {
        self.pending.remove(&id);
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Закрыть все ожидания (процесс умер): получатели увидят ошибку канала.
    pub fn fail_all(&mut self) {
        self.pending.clear();
    }
}

// ---- результаты запросов ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub status: Option<Value>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResult {
    pub thread: Thread,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub approval_policy: Option<Value>,
    #[serde(default)]
    pub sandbox: Option<Value>,
    #[serde(default)]
    pub instruction_sources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<Value>,
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub status: TurnStatus,
    #[serde(default)]
    pub items: Vec<Value>,
    #[serde(default)]
    pub error: Option<TurnError>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TurnStartResult {
    pub turn: Turn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResult {
    pub turn_id: String,
}

// ---- уведомления ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub error: TurnError,
    #[serde(default)]
    pub will_retry: bool,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolved {
    pub thread_id: String,
    pub request_id: RequestId,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageUpdated {
    pub thread_id: String,
    pub turn_id: String,
    pub token_usage: Value,
}

/// Элемент разговора. Неизвестные типы не ломают разбор.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename_all = "camelCase")]
    UserMessage {
        id: String,
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        content: Vec<Value>,
    },
    #[serde(rename_all = "camelCase")]
    AgentMessage {
        id: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        phase: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: String,
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        status: String,
        #[serde(default)]
        exit_code: Option<i64>,
        #[serde(default)]
        aggregated_output: Option<String>,
        #[serde(default)]
        process_id: Option<String>,
        #[serde(default)]
        duration_ms: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    FileChange {
        id: String,
        #[serde(default)]
        changes: Vec<Value>,
        status: String,
    },
    #[serde(rename_all = "camelCase")]
    Plan {
        id: String,
        #[serde(default)]
        text: String,
    },
    Reasoning {
        id: String,
    },
    #[serde(other)]
    Other,
}

impl ThreadItem {
    pub fn id(&self) -> Option<&str> {
        match self {
            ThreadItem::UserMessage { id, .. }
            | ThreadItem::AgentMessage { id, .. }
            | ThreadItem::CommandExecution { id, .. }
            | ThreadItem::FileChange { id, .. }
            | ThreadItem::Plan { id, .. }
            | ThreadItem::Reasoning { id } => Some(id),
            ThreadItem::Other => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ThreadItem::UserMessage { .. } => "userMessage",
            ThreadItem::AgentMessage { .. } => "agentMessage",
            ThreadItem::CommandExecution { .. } => "commandExecution",
            ThreadItem::FileChange { .. } => "fileChange",
            ThreadItem::Plan { .. } => "plan",
            ThreadItem::Reasoning { .. } => "reasoning",
            ThreadItem::Other => "other",
        }
    }
}

// ---- запросы сервера к клиенту ----

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub kind: Option<Value>,
    #[serde(default)]
    pub available_decisions: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub grant_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    #[serde(default)]
    pub options: Option<Vec<UserInputOption>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub questions: Vec<UserInputQuestion>,
    #[serde(default)]
    pub is_blocking: bool,
}

/// Ответ на `item/tool/requestUserInput`: id вопроса -> выбранные ответы.
pub fn user_input_answer(answers: &[(&str, Vec<String>)]) -> Value {
    let mut map = serde_json::Map::new();
    for (qid, values) in answers {
        map.insert((*qid).to_string(), json!({ "answers": values }));
    }
    json!({ "answers": map })
}

pub fn approval_decision(decision: &str) -> Value {
    json!({ "decision": decision })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_response_notification_and_server_request() {
        let r = classify(json!({"id": 3, "result": {"turn": {"id": "t"}}}));
        assert!(matches!(
            r,
            Incoming::Response {
                id: RequestId::Int(3),
                result: Ok(_)
            }
        ));

        let e = classify(json!({"id": 4, "error": {"code": -32600, "message": "bad"}}));
        match e {
            Incoming::Response {
                result: Err(err), ..
            } => assert_eq!(err.code, -32600),
            other => panic!("unexpected {other:?}"),
        }

        let n = classify(json!({"method": "turn/started", "params": {"x": 1}}));
        assert!(matches!(n, Incoming::Notification { ref method, .. } if method == "turn/started"));

        let s = classify(
            json!({"id": 0, "method": "item/commandExecution/requestApproval", "params": {}}),
        );
        assert!(matches!(
            s,
            Incoming::ServerRequest {
                id: RequestId::Int(0),
                ..
            }
        ));

        let s2 = classify(json!({"id": "abc", "method": "x", "params": {}}));
        assert!(matches!(
            s2,
            Incoming::ServerRequest {
                id: RequestId::Str(_),
                ..
            }
        ));
    }

    #[test]
    fn rejects_malformed_messages() {
        assert!(matches!(classify(json!([1, 2])), Incoming::Invalid { .. }));
        assert!(matches!(
            classify(json!({"id": 1})),
            Incoming::Invalid { .. }
        ));
        assert!(matches!(
            classify(json!({"foo": 1})),
            Incoming::Invalid { .. }
        ));
        assert!(matches!(
            classify(json!({"id": 1.5, "result": {}})),
            Incoming::Invalid { .. }
        ));
    }

    #[tokio::test]
    async fn correlator_matches_out_of_order_responses() {
        let mut c = Correlator::default();
        let rx1 = c.register(1);
        let rx2 = c.register(2);
        assert_eq!(
            c.resolve(&RequestId::Int(2), Ok(json!("two"))),
            Resolution::Delivered
        );
        assert_eq!(
            c.resolve(&RequestId::Int(1), Ok(json!("one"))),
            Resolution::Delivered
        );
        assert_eq!(rx1.await.expect("rx1"), Ok(json!("one")));
        assert_eq!(rx2.await.expect("rx2"), Ok(json!("two")));
        assert_eq!(c.pending_count(), 0);
    }

    #[tokio::test]
    async fn correlator_reports_unmatched_and_duplicate_responses() {
        let mut c = Correlator::default();
        let rx = c.register(7);
        assert_eq!(
            c.resolve(&RequestId::Int(8), Ok(json!(null))),
            Resolution::Unmatched
        );
        assert_eq!(
            c.resolve(&RequestId::Str("7".into()), Ok(json!(null))),
            Resolution::Unmatched
        );
        assert_eq!(
            c.resolve(&RequestId::Int(7), Ok(json!(1))),
            Resolution::Delivered
        );
        // повторный ответ на тот же id уже не сопоставляется
        assert_eq!(
            c.resolve(&RequestId::Int(7), Ok(json!(2))),
            Resolution::Unmatched
        );
        assert_eq!(rx.await.expect("rx"), Ok(json!(1)));
    }

    #[tokio::test]
    async fn correlator_fail_all_closes_waiters() {
        let mut c = Correlator::default();
        let rx = c.register(1);
        c.fail_all();
        assert!(rx.await.is_err());
    }

    #[test]
    fn thread_item_tolerates_unknown_types_and_fields() {
        let item: ThreadItem =
            serde_json::from_value(json!({"type": "hookPrompt", "id": "x", "fragments": []}))
                .expect("unknown variant");
        assert!(matches!(item, ThreadItem::Other));
        let item: ThreadItem = serde_json::from_value(json!({
            "type": "agentMessage", "id": "m1", "text": "hi", "phase": null,
            "memoryCitation": null, "delivery": null, "questions": null, "newField": 1
        }))
        .expect("agent message");
        assert_eq!(item.id(), Some("m1"));
    }

    #[test]
    fn builds_user_input_answer() {
        let v = user_input_answer(&[("color", vec!["Blue".into()])]);
        assert_eq!(v, json!({"answers": {"color": {"answers": ["Blue"]}}}));
    }
}
