//! Запросы сервера: разрешения и формы. Живут только в его памяти, поэтому решение уходит
//! один раз, а адрес ответа берётся из запроса: у потомка своя сессия.

use super::{
    super::{AdapterEvent, DecisionProof, Phase},
    events::{PendingRequest, RequestKind},
    http::{Http, HttpError},
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use serde_json::{Value, json};
use std::{collections::HashMap, fmt, time::Duration};

const REPLY_WAIT: Duration = Duration::from_secs(5);
/// Отказ без текста кончает ход, поэтому текст есть всегда.
const DECLINED: &str = "The owner declined this action. Continue without it.";
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

struct Pending {
    kind: RequestKind,
    session: String,
    item: Option<String>,
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
        "agent": request.agent(),
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
        let pending = Pending {
            kind: request.kind,
            session: request.session.clone(),
            item: request.item.clone(),
            answered: false,
        };
        match request.kind {
            RequestKind::Permission => {
                let payload = permission_payload(&request);
                self.pending.insert(request.id.clone(), pending);
                Intake::Open(
                    AdapterEvent::RequestOpened {
                        request_id: request.id,
                        kind: "permission".into(),
                        payload,
                    },
                    Phase::AwaitingPermission,
                )
            }
            RequestKind::Form => Intake::Refuse(
                refusal(&pending, &request.id, REFUSED),
                "opencode_form_refused",
            ),
        }
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
            _ => return Err("decision_invalid"),
        };
        pending.answered = true;
        Ok(reply)
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
            child: false,
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
        child.child = true;
        assert_eq!(permission_payload(&child)["agent"], "subagent");
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
