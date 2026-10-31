//! Запросы сервера: разрешения и формы. Живут только в его памяти, поэтому решение уходит
//! один раз, а адрес ответа берётся из запроса: у потомка своя сессия.

use super::{
    super::{AdapterEvent, DecisionProof, Phase, valid_decision},
    events::{PendingRequest, RequestKind},
    http::{Http, HttpError},
};
use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    time::Duration,
};

const REPLY_WAIT: Duration = Duration::from_secs(5);
/// Отказ без текста кончает ход, поэтому текст есть всегда.
const DECLINED: &str = "The owner declined this action. Continue without it.";
/// Столько вопросов показывает карточка и принимает `valid_decision`.
const MAX_QUESTIONS: usize = 4;
/// Сессия форм MCP: их сервер не связывает с сессией разговора.
pub const GLOBAL_SESSION: &str = "global";
pub const REFUSED: &str = "This request cannot be answered in this session. Continue without it.";

/// Ответ серверу. Адрес и тело не попадают в журнал: тело несёт текст владельца.
pub enum Reply {
    Post { path: String, body: Value },
    Cancel { path: String, message: String },
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Post { .. } => "Post",
            Self::Cancel { .. } => "Cancel",
        })
    }
}

impl Reply {
    pub async fn send(&self, http: &Http) -> Result<(), HttpError> {
        match self {
            Self::Post { path, body } => http.post(path, body, REPLY_WAIT).await.map(drop),
            Self::Cancel { path, message } => http
                .delete(path, &[("message", message)], REPLY_WAIT)
                .await
                .map(drop),
        }
    }
}

/// Подпись видит владелец, сервер ждёт значение.
struct Question {
    id: String,
    options: Vec<(String, String)>,
}

impl Question {
    fn value(&self, answer: &str) -> String {
        self.options
            .iter()
            .find(|(label, _)| label == answer)
            .map_or_else(|| answer.to_owned(), |(_, value)| value.clone())
    }
}

struct Pending {
    kind: RequestKind,
    session: String,
    item: Option<String>,
    /// Карточка вопроса: по ней сверяется ответ. У разрешения не нужна.
    payload: Value,
    questions: Vec<Question>,
    answered: bool,
}

/// Что делать с пришедшим запросом.
#[derive(Debug)]
pub enum Intake {
    Open(AdapterEvent, Phase),
    /// Такой запрос владельцу не показать.
    Refuse(Reply, &'static str),
    Duplicate,
}

/// Итог `permission.replied`, `form.replied` и `form.cancelled`.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled {
    /// Наше решение: журнал ведёт супервизор.
    Ours,
    /// Сервер снял запрос сам, карточку закрывает адаптер.
    Cleared,
}

#[derive(Default)]
pub struct Requests {
    pending: HashMap<String, Pending>,
}

impl fmt::Debug for Requests {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Requests")
            .field("pending", &self.pending.len())
            .finish()
    }
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn change(file: &Value) -> Value {
    json!({
        "path": file.get("file").cloned().unwrap_or(Value::Null),
        "kind": match file.get("status").and_then(Value::as_str) {
            Some("added") => "add",
            Some("deleted") => "delete",
            _ => "update",
        },
        "diff": file.get("patch").cloned().unwrap_or(Value::Null),
    })
}

/// Карточка: `read` и `edit` показывают файлы, остальное идёт командой. Полный вход остаётся
/// у сервера, поэтому обрезанное супервизором превью решения не меняет.
pub fn permission_payload(request: &PendingRequest) -> Value {
    let raw = &request.raw;
    let action = raw.get("action").and_then(Value::as_str).unwrap_or("tool");
    let resources = strings(raw.get("resources").unwrap_or(&Value::Null));
    let tool_use_id = raw.pointer("/source/id").cloned().unwrap_or(Value::Null);
    let mut payload = json!({
        "tool_name": action,
        "tool_use_id": tool_use_id,
        "agent": request.agent,
        "deny_also_interrupts": false,
    });
    if let Some(message) = raw.get("message").filter(|message| message.is_string()) {
        payload["reason"] = message.clone();
    }
    let files = raw
        .pointer("/metadata/files")
        .and_then(Value::as_array)
        .filter(|files| !files.is_empty());
    match (action, files) {
        ("edit", Some(files)) => {
            payload["changes"] = files.iter().map(change).collect();
        }
        ("read" | "edit", _) => {
            payload["input"] = json!({"file_path": resources.join("\n")});
        }
        ("external_directory", _) => payload["grant_root"] = resources.join("\n").into(),
        _ => payload["command"] = resources.join("\n").into(),
    }
    payload
}

fn question(field: &Value) -> Option<(Value, Question)> {
    let id = field.get("key")?.as_str().filter(|id| !id.is_empty())?;
    let kind = field.get("type")?.as_str()?;
    if !matches!(kind, "string" | "multiselect") {
        return None;
    }
    let raw = field.get("options").and_then(Value::as_array);
    let mut options = Vec::new();
    let mut shown = Vec::new();
    for option in raw.into_iter().flatten() {
        let label = option.get("label")?.as_str()?;
        let value = option.get("value")?.as_str()?;
        options.push((label.to_owned(), value.to_owned()));
        shown.push(json!({
            "label": label,
            "description": option.get("description").cloned().unwrap_or(Value::Null),
        }));
    }
    let custom = field.get("custom").and_then(Value::as_bool) == Some(true) || options.is_empty();
    let header = field.get("title").cloned().unwrap_or(Value::Null);
    let shown_question = json!({
        "id": id,
        "text": field.get("description").unwrap_or(&header),
        "header": header,
        "options": shown,
        "multi_select": kind == "multiselect",
        "allow_custom": custom,
        "is_secret": false,
    });
    Some((
        shown_question,
        Question {
            id: id.to_owned(),
            options,
        },
    ))
}

/// Форма инструмента `question`. Формы MCP и всё, что карточка не соберёт, сюда не проходят:
/// сервер принял бы неполный ответ, а без ответа вызов ждал бы вечно.
fn question_payload(request: &PendingRequest) -> Option<(Value, Vec<Question>)> {
    let raw = &request.raw;
    if request.session == GLOBAL_SESSION
        || raw.pointer("/metadata/kind").and_then(Value::as_str) != Some("question")
    {
        return None;
    }
    let fields = raw.get("fields")?.as_array()?;
    if fields.is_empty() || fields.len() > MAX_QUESTIONS {
        return None;
    }
    let (shown, questions): (Vec<Value>, Vec<Question>) =
        fields.iter().map(question).collect::<Option<_>>()?;
    let tool_use_id = raw.pointer("/metadata/tool/id").cloned();
    let payload = json!({
        "questions": shown,
        "tool_use_id": tool_use_id.unwrap_or(Value::Null),
        "agent": request.agent,
    });
    Some((payload, questions))
}

fn answer_body(questions: &[Question], answers: &BTreeMap<String, AnswerValue>) -> Value {
    let mut out = serde_json::Map::new();
    for question in questions {
        let value = match answers.get(&question.id) {
            Some(AnswerValue::Single(answer)) => question.value(answer).into(),
            Some(AnswerValue::Multiple(values)) => values
                .iter()
                .map(|answer| Value::from(question.value(answer)))
                .collect(),
            None => continue,
        };
        out.insert(question.id.clone(), value);
    }
    json!({"answer": out})
}

fn cancel_path(session: &str, id: &str) -> String {
    format!("/api/session/{session}/form/{id}")
}

fn permission_path(session: &str, id: &str) -> String {
    format!("/api/session/{session}/permission/{id}/reply")
}

fn refusal(pending: &Pending, id: &str, message: &str) -> Reply {
    match pending.kind {
        RequestKind::Permission => Reply::Post {
            path: permission_path(&pending.session, id),
            body: json!({"decision": "reject", "message": message}),
        },
        RequestKind::Form => Reply::Cancel {
            path: cancel_path(&pending.session, id),
            message: message.to_owned(),
        },
    }
}

impl Requests {
    pub fn intake(&mut self, request: PendingRequest) -> Intake {
        if self.pending.contains_key(&request.id) {
            return Intake::Duplicate;
        }
        let mut pending = Pending {
            kind: request.kind,
            session: request.session.clone(),
            item: request.item.clone(),
            payload: Value::Null,
            questions: Vec::new(),
            answered: false,
        };
        let (kind, phase, payload) = match request.kind {
            RequestKind::Permission => (
                "permission",
                Phase::AwaitingPermission,
                permission_payload(&request),
            ),
            RequestKind::Form => {
                let Some((payload, questions)) = question_payload(&request) else {
                    return Intake::Refuse(
                        refusal(&pending, &request.id, REFUSED),
                        "opencode_form_refused",
                    );
                };
                pending.payload = payload.clone();
                pending.questions = questions;
                ("question", Phase::AwaitingAnswer, payload)
            }
        };
        self.pending.insert(request.id.clone(), pending);
        Intake::Open(
            AdapterEvent::RequestOpened {
                request_id: request.id,
                kind: kind.into(),
                payload,
            },
            phase,
        )
    }

    pub fn has_open(&self) -> bool {
        self.pending.values().any(|pending| !pending.answered)
    }

    /// Решение владельца. `answered` ставится до запроса: второго ответа не будет
    /// и при неизвестном исходе первого.
    pub fn answer(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> Result<Reply, &'static str> {
        let pending = self
            .pending
            .get_mut(request_id)
            .filter(|pending| !pending.answered)
            .ok_or("request_not_pending")?;
        let reply = match (pending.kind, decision) {
            (RequestKind::Permission, DecisionPayload::Permission { behavior, message }) => {
                let body = match behavior {
                    PermissionBehavior::Allow => json!({"decision": "once"}),
                    PermissionBehavior::Deny => {
                        let message = message
                            .as_deref()
                            .filter(|message| !message.trim().is_empty())
                            .unwrap_or(DECLINED);
                        json!({"decision": "reject", "message": message})
                    }
                };
                Reply::Post {
                    path: permission_path(&pending.session, request_id),
                    body,
                }
            }
            (RequestKind::Form, DecisionPayload::Question { answers })
                if valid_decision("question", &pending.payload, decision) =>
            {
                Reply::Post {
                    path: format!("{}/reply", cancel_path(&pending.session, request_id)),
                    body: answer_body(&pending.questions, answers),
                }
            }
            _ => return Err("decision_invalid"),
        };
        pending.answered = true;
        Ok(reply)
    }

    /// Сервер отверг ответ: запрос жив, и вызов будет ждать, пока ему не откажут.
    pub fn reopen(&mut self, request_id: &str) {
        if let Some(pending) = self.pending.get_mut(request_id) {
            pending.answered = false;
        }
    }

    /// Отказ без участия владельца; уже отвеченный запрос не трогается.
    pub fn refuse(&mut self, request_id: &str, message: &str) -> Option<Reply> {
        let pending = self.pending.remove(request_id)?;
        (!pending.answered).then(|| refusal(&pending, request_id, message))
    }

    pub fn forget(&mut self, request_id: &str) {
        self.pending.remove(request_id);
    }

    /// Сервер закрыл запрос.
    pub fn on_settled(&mut self, request_id: &str) -> Option<Settled> {
        let pending = self.pending.remove(request_id)?;
        Some(if pending.answered {
            Settled::Ours
        } else {
            Settled::Cleared
        })
    }

    /// Вызов закончился: его запрос снят без `permission.replied`.
    pub fn close_item(&mut self, item: &str) -> Vec<String> {
        let ids: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.item.as_deref() == Some(item))
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter(|id| matches!(self.on_settled(id), Some(Settled::Cleared)))
            .collect()
    }

    /// Прерывание сняло всё: отвечать некому.
    pub fn drop_all(&mut self) {
        self.pending.clear();
    }

    /// Прерывание ничего не сняло, а карточки закрыты: вызовы ждали бы вечно.
    pub fn refuse_all(&mut self, message: &str) -> Vec<Reply> {
        let ids: Vec<String> = self.pending.keys().cloned().collect();
        ids.iter()
            .filter_map(|id| self.refuse(id, message))
            .collect()
    }

    /// Открытые запросы, которых нет на сервере: сняты, пока потока не было. Отвеченный не
    /// трогается: его итог приходит своим путём.
    pub fn gone(&mut self, alive: &HashSet<&str>) -> Vec<String> {
        let ids: Vec<String> = self
            .pending
            .iter()
            .filter(|(id, pending)| !pending.answered && !alive.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            self.pending.remove(id);
        }
        ids
    }

    /// Период работы сессии кончился: её запросов на сервере больше нет.
    pub fn drop_session(&mut self, session: &str) {
        self.pending.retain(|_, pending| pending.session != session);
    }
}

pub fn proof(request_id: &str, decision: &DecisionPayload) -> DecisionProof {
    let kind = match decision {
        DecisionPayload::Permission {
            behavior: PermissionBehavior::Allow,
            ..
        } => "allow",
        DecisionPayload::Permission { .. } => "deny",
        DecisionPayload::Question { .. } => "answer",
    };
    DecisionProof {
        proof_kind: "opencode_reply_accepted".into(),
        payload: json!({"request_id": request_id, "decision_kind": kind}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(raw: Value) -> PendingRequest {
        PendingRequest {
            kind: RequestKind::Permission,
            id: "per_1".into(),
            session: "ses_1".into(),
            item: None,
            agent: "orchestrator".into(),
            raw,
        }
    }

    #[test]
    fn command_keeps_every_resource_on_its_line() {
        let payload = permission_payload(&request(json!({
            "action": "shell", "resources": ["ls", "пример"],
            "source": {"id": "call_1"}, "message": "почему"
        })));
        assert_eq!(payload["command"], "ls\nпример");
        assert_eq!(payload["tool_use_id"], "call_1");
        assert_eq!(payload["reason"], "почему");
    }

    #[test]
    fn edit_without_a_diff_names_the_file() {
        let payload =
            permission_payload(&request(json!({"action": "edit", "resources": ["w.txt"]})));
        assert_eq!(payload["input"]["file_path"], "w.txt");
        assert!(payload.get("changes").is_none());
    }

    #[test]
    fn child_request_is_labelled() {
        let mut child = request(json!({"action": "shell", "resources": ["ls"]}));
        child.agent = "general".into();
        assert_eq!(permission_payload(&child)["agent"], "general");
    }

    fn field(value: Value) -> PendingRequest {
        PendingRequest {
            kind: RequestKind::Form,
            id: "frm_1".into(),
            session: "ses_1".into(),
            item: None,
            agent: "orchestrator".into(),
            raw: json!({"metadata": {"kind": "question"}, "fields": [value]}),
        }
    }

    #[test]
    fn free_text_question_needs_no_options() {
        let (payload, _) = question_payload(&field(
            json!({"key": "q0", "type": "string", "title": "Имя"}),
        ))
        .unwrap();
        assert_eq!(payload["questions"][0]["allow_custom"], true);
        assert_eq!(payload["questions"][0]["text"], "Имя");
    }

    #[test]
    fn fields_a_card_cannot_collect_are_refused() {
        for kind in ["boolean", "number", "integer", "external"] {
            assert!(question_payload(&field(json!({"key": "q0", "type": kind}))).is_none());
        }
        assert!(question_payload(&field(json!({"key": "", "type": "string"}))).is_none());
        let empty = PendingRequest {
            raw: json!({"metadata": {"kind": "question"}, "fields": []}),
            ..field(Value::Null)
        };
        assert!(question_payload(&empty).is_none());
    }

    #[test]
    fn debug_hides_the_owner_text() {
        let mut requests = Requests::default();
        requests.intake(request(json!({"action": "shell", "resources": ["ls"]})));
        let reply = requests
            .answer(
                "per_1",
                &DecisionPayload::Permission {
                    behavior: PermissionBehavior::Deny,
                    message: Some("секрет".into()),
                },
            )
            .unwrap();
        assert!(!format!("{reply:?}").contains("секрет"));
    }
}
