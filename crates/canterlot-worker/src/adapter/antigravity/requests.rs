//! Запросы сервера Antigravity: разрешения инструментов и вопросы `ask_question`.
//!
//! Карточка разрешения отвечает только `allow`/`deny` (никогда `allow_always`);
//! сырой JSON-RPC id живёт отдельно от id canterlot `<generation>:<toolCallId>`,
//! потому что сервер повторяет `"1"` в каждом процессе. Вопрос нормализуется в
//! обычную карточку canterlot и отвечается служебным prompt внутри логического хода.

use super::rpc::RpcId;
use crate::adapter::valid_decision;
use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// `valid_decision` не принимает больше четырёх вопросов в карточке.
pub const MAX_QUESTIONS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub options: Vec<String>,
    pub multi_select: bool,
}

#[derive(Clone, Debug)]
pub enum PendingKind {
    Permission {
        tool_call_id: String,
    },
    Question {
        tool_call_id: String,
        questions: Vec<Question>,
    },
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub rpc_id: Option<RpcId>,
    pub payload: Value,
    pub kind: PendingKind,
    pub answered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    NoToolCall,
    InvalidQuestions,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Permission { rpc_id: RpcId, option: &'static str },
    Question { text: String },
}

pub struct Requests {
    generation: u64,
    pending: HashMap<String, Pending>,
    by_tool: HashMap<String, String>,
    resolved: HashSet<String>,
    expired: HashSet<String>,
}

impl Requests {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            pending: HashMap::new(),
            by_tool: HashMap::new(),
            resolved: HashSet::new(),
            expired: HashSet::new(),
        }
    }

    fn request_id(&self, tool_call_id: &str) -> String {
        format!("{}:{tool_call_id}", self.generation)
    }

    fn insert(&mut self, request_id: String, pending: Pending, tool_call_id: &str) {
        self.by_tool
            .insert(tool_call_id.to_owned(), request_id.clone());
        self.pending.insert(request_id, pending);
    }

    pub fn open_permission(
        &mut self,
        rpc_id: RpcId,
        params: &Value,
        agent: &str,
    ) -> Result<(String, Value), OpenError> {
        let tool = params.get("toolCall").unwrap_or(params);
        let tool_call_id = tool
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(OpenError::NoToolCall)?;
        let request_id = self.request_id(tool_call_id);
        if self.pending.contains_key(&request_id) {
            return Err(OpenError::Duplicate);
        }
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
        let raw = tool.get("rawInput").cloned().unwrap_or(Value::Null);
        let payload = json!({
            "tool_name": name,
            "title": tool.get("title").cloned().unwrap_or(Value::Null),
            "input": raw,
            "command": raw.get("CommandLine").cloned().unwrap_or(Value::Null),
            "tool_use_id": tool_call_id,
            "agent": agent,
            "options": params.get("options").cloned().unwrap_or(Value::Null),
            // deny в Antigravity завершает ход без текста (t5-deny).
            "deny_also_interrupts": true,
        });
        self.insert(
            request_id.clone(),
            Pending {
                rpc_id: Some(rpc_id),
                payload: payload.clone(),
                kind: PendingKind::Permission {
                    tool_call_id: tool_call_id.to_owned(),
                },
                answered: false,
            },
            tool_call_id,
        );
        Ok((request_id, payload))
    }

    pub fn open_question(
        &mut self,
        tool_call_id: &str,
        raw_input: &Value,
        agent: &str,
    ) -> Result<(String, Value), OpenError> {
        let questions = parse_questions(raw_input)?;
        let request_id = self.request_id(tool_call_id);
        if self.pending.contains_key(&request_id) {
            return Err(OpenError::Duplicate);
        }
        let payload = question_payload(tool_call_id, &questions, agent);
        self.insert(
            request_id.clone(),
            Pending {
                rpc_id: None,
                payload: payload.clone(),
                kind: PendingKind::Question {
                    tool_call_id: tool_call_id.to_owned(),
                    questions,
                },
                answered: false,
            },
            tool_call_id,
        );
        Ok((request_id, payload))
    }

    pub fn pending(&self, request_id: &str) -> Option<&Pending> {
        self.pending.get(request_id)
    }

    pub fn was_expired(&self, request_id: &str) -> bool {
        self.expired.contains(request_id)
    }

    /// Решение владельца: у вопроса текст продолжения, у разрешения — optionId.
    pub fn answer(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> Result<Answer, &'static str> {
        let pending = self
            .pending
            .get_mut(request_id)
            .filter(|pending| !pending.answered)
            .ok_or("request_not_pending")?;
        let answer = match (&pending.kind, decision) {
            (PendingKind::Permission { .. }, DecisionPayload::Permission { behavior, .. }) => {
                let option = match behavior {
                    PermissionBehavior::Allow => "allow",
                    PermissionBehavior::Deny => "deny",
                };
                let rpc_id = pending.rpc_id.clone().ok_or("decision_mismatch")?;
                Answer::Permission { rpc_id, option }
            }
            (PendingKind::Question { questions, .. }, DecisionPayload::Question { answers }) => {
                if !valid_decision("question", &pending.payload, decision) {
                    return Err("invalid_decision");
                }
                Answer::Question {
                    text: continuation_text(questions, answers).ok_or("invalid_decision")?,
                }
            }
            _ => return Err("decision_mismatch"),
        };
        pending.answered = true;
        Ok(answer)
    }

    /// Следующий кадр инструмента после ответа — доказательство `Applied`.
    /// Для вопроса ждём терминальный статус: `pending`/`in_progress`, отправленные
    /// до ответа, могли прийти после него и ничего не доказывают.
    pub fn note_tool_frame(&mut self, tool_call_id: &str, status: Option<&str>) {
        let Some(request_id) = self.by_tool.get(tool_call_id) else {
            return;
        };
        let Some(pending) = self
            .pending
            .get(request_id)
            .filter(|pending| pending.answered)
        else {
            return;
        };
        let terminal = matches!(status, Some("completed" | "failed"));
        if terminal || matches!(pending.kind, PendingKind::Permission { .. }) {
            self.resolved.insert(request_id.clone());
        }
    }

    pub fn take_resolved(&mut self, request_id: &str) -> bool {
        self.resolved.remove(request_id)
    }

    /// Конец хода: открытые карточки закрываются, поздний ответ на вопрос истекает.
    /// Отвеченные карточки просто забываются: `RequestClosed` для них уже не нужен.
    pub fn expire(&mut self) -> Vec<String> {
        let open: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, pending)| !pending.answered)
            .map(|(request_id, _)| request_id.clone())
            .collect();
        for request_id in &open {
            if let Some(pending) = self.pending.remove(request_id)
                && matches!(pending.kind, PendingKind::Question { .. })
            {
                self.expired.insert(request_id.clone());
            }
        }
        self.pending.retain(|_, pending| !pending.answered);
        self.by_tool.clear();
        self.resolved.clear();
        open
    }

    /// Cancel и прерывание: карточки закрываются, ответить на них уже нельзя.
    pub fn drop_open(&mut self) -> Vec<String> {
        self.expire()
    }

    /// Убрать карточку без отметки «истекла»: отказ уже случился.
    pub fn remove(&mut self, request_id: &str) -> Option<Pending> {
        let pending = self.pending.remove(request_id)?;
        match &pending.kind {
            PendingKind::Permission { tool_call_id }
            | PendingKind::Question { tool_call_id, .. } => {
                if self
                    .by_tool
                    .get(tool_call_id)
                    .is_some_and(|id| id == request_id)
                {
                    self.by_tool.remove(tool_call_id);
                }
            }
        }
        self.resolved.remove(request_id);
        Some(pending)
    }

    pub fn open_ids(&self) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, pending)| !pending.answered)
            .map(|(request_id, _)| request_id.clone())
            .collect()
    }
}

/// Нормализация `rawInput.questions[]`: `q<index>`, варианты, multi-select.
pub fn parse_questions(raw: &Value) -> Result<Vec<Question>, OpenError> {
    let list = raw
        .get("questions")
        .and_then(Value::as_array)
        .ok_or(OpenError::InvalidQuestions)?;
    if list.is_empty() || list.len() > MAX_QUESTIONS {
        return Err(OpenError::InvalidQuestions);
    }
    list.iter()
        .enumerate()
        .map(|(index, question)| {
            let text = question
                .get("question")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .ok_or(OpenError::InvalidQuestions)?;
            let options = match question.get("options") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(options)) => options
                    .iter()
                    .map(|option| {
                        option
                            .as_str()
                            .or_else(|| option.get("label").and_then(Value::as_str))
                            .filter(|option| !option.is_empty())
                            .map(str::to_owned)
                            .ok_or(OpenError::InvalidQuestions)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(OpenError::InvalidQuestions),
            };
            Ok(Question {
                id: format!("q{index}"),
                text: text.to_owned(),
                options,
                multi_select: question
                    .get("is_multi_select")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn question_payload(tool_call_id: &str, questions: &[Question], agent: &str) -> Value {
    let shown: Vec<Value> = questions
        .iter()
        .map(|question| {
            json!({
                "id": question.id,
                "text": question.text,
                "header": Value::Null,
                "options": question.options.iter().map(|option| json!({
                    "label": option,
                    "description": Value::Null,
                })).collect::<Vec<_>>(),
                "multi_select": question.multi_select,
                "allow_custom": true,
                "is_secret": false,
            })
        })
        .collect();
    json!({
        "questions": shown,
        "tool_use_id": tool_call_id,
        "agent": agent,
    })
}

/// Строки служебного prompt: по одной на вопрос, multi-select — через запятую.
pub fn continuation_text(
    questions: &[Question],
    answers: &BTreeMap<String, AnswerValue>,
) -> Option<String> {
    let mut lines = Vec::new();
    for question in questions {
        let answer = match answers.get(&question.id)? {
            AnswerValue::Single(answer) => answer.clone(),
            AnswerValue::Multiple(values) => values.join(", "),
        };
        lines.push(format!(
            "Ответ владельца на вопрос \"{}\": {}",
            question.text, answer
        ));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(questions: Value) -> Value {
        json!({"questions": questions})
    }

    fn single(id: &str, value: &str) -> DecisionPayload {
        let mut answers = BTreeMap::new();
        answers.insert(id.to_owned(), AnswerValue::Single(value.to_owned()));
        DecisionPayload::Question { answers }
    }

    #[test]
    fn question_ids_options_and_multi_select_are_normalised() {
        let questions = parse_questions(&raw(json!([
            {"question": "Цвет?", "options": ["Синий", "Красный"], "is_multi_select": true},
            {"question": "Комментарий?"},
        ])))
        .unwrap();
        assert_eq!(questions[0].id, "q0");
        assert_eq!(questions[0].text, "Цвет?");
        assert_eq!(questions[0].options, vec!["Синий", "Красный"]);
        assert!(questions[0].multi_select);
        assert_eq!(questions[1].id, "q1");
        assert!(questions[1].options.is_empty());
        assert!(!questions[1].multi_select);
    }

    #[test]
    fn empty_or_broken_questions_are_refused() {
        for broken in [
            json!({}),
            raw(json!([])),
            raw(json!([{"options": ["a"]}])),
            raw(json!([{"question": "  "}])),
            raw(json!([{"question": "q", "options": "a"}])),
            raw(json!(
                (0..5)
                    .map(|i| json!({"question": format!("q{i}")}))
                    .collect::<Vec<_>>()
            )),
        ] {
            assert_eq!(parse_questions(&broken), Err(OpenError::InvalidQuestions));
        }
    }

    #[test]
    fn question_payload_passes_the_contract_and_allows_free_answers() {
        let (_, payload) = Requests::new(7)
            .open_question(
                "call_1",
                &raw(json!([
                    {"question": "Цвет?", "options": ["Синий", "Красный"], "is_multi_select": false}
                ])),
                "orchestrator",
            )
            .unwrap();
        assert_eq!(payload["tool_use_id"], "call_1");
        assert_eq!(payload["questions"][0]["id"], "q0");
        assert_eq!(payload["questions"][0]["options"][0]["label"], "Синий");
        assert_eq!(payload["questions"][0]["allow_custom"], true);
        // Свободный ответ допустим при allow_custom.
        let mut requests = Requests::new(7);
        let (request_id, _) = requests
            .open_question(
                "call_1",
                &raw(json!([{"question": "Цвет?"}])),
                "orchestrator",
            )
            .unwrap();
        assert_eq!(
            requests.answer(&request_id, &single("q0", "свой ответ")),
            Ok(Answer::Question {
                text: "Ответ владельца на вопрос \"Цвет?\": свой ответ".into()
            })
        );
        assert_eq!(
            requests.answer(&request_id, &single("q0", "ещё")),
            Err("request_not_pending")
        );
    }

    #[test]
    fn multi_select_answers_are_joined_by_comma() {
        let raw = raw(json!([
            {"question": "Что взять?", "options": ["a", "b", "c"], "is_multi_select": true}
        ]));
        let mut requests = Requests::new(1);
        let (request_id, _) = requests
            .open_question("call_2", &raw, "orchestrator")
            .unwrap();
        let mut answers = BTreeMap::new();
        answers.insert(
            "q0".into(),
            AnswerValue::Multiple(vec!["a".into(), "c".into()]),
        );
        assert_eq!(
            requests.answer(&request_id, &DecisionPayload::Question { answers }),
            Ok(Answer::Question {
                text: "Ответ владельца на вопрос \"Что взять?\": a, c".into()
            })
        );
    }

    #[test]
    fn permission_id_is_generation_and_tool_call_and_the_raw_id_is_separate() {
        let mut requests = Requests::new(4);
        let params = json!({
            "toolCall": {"toolCallId": "call_9", "name": "run_command",
                         "rawInput": {"CommandLine": "echo hi"}},
            "options": [{"optionId": "allow"}, {"optionId": "deny"}],
        });
        let (request_id, payload) = requests
            .open_permission(RpcId::Str("1".into()), &params, "orchestrator")
            .unwrap();
        assert_eq!(request_id, "4:call_9");
        assert_eq!(payload["tool_name"], "run_command");
        assert_eq!(payload["tool_use_id"], "call_9");
        assert_eq!(
            requests.answer(
                &request_id,
                &DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    message: None,
                }
            ),
            Ok(Answer::Permission {
                rpc_id: RpcId::Str("1".into()),
                option: "allow"
            })
        );
        assert_eq!(
            requests.answer(
                &request_id,
                &DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    message: None,
                }
            ),
            Err("request_not_pending")
        );
    }

    #[test]
    fn a_tool_frame_proves_an_answered_permission() {
        let mut requests = Requests::new(1);
        let params = json!({"toolCall": {"toolCallId": "call_1", "name": "run_command"}});
        let (request_id, _) = requests
            .open_permission(RpcId::Int(1), &params, "orchestrator")
            .unwrap();
        assert!(!requests.take_resolved(&request_id));
        requests
            .answer(
                &request_id,
                &DecisionPayload::Permission {
                    behavior: PermissionBehavior::Deny,
                    message: None,
                },
            )
            .unwrap();
        requests.note_tool_frame("call_1", Some("in_progress"));
        assert!(requests.take_resolved(&request_id));
        assert!(!requests.take_resolved(&request_id));
    }

    #[test]
    fn a_stale_pending_frame_does_not_prove_a_question_answer() {
        let mut requests = Requests::new(1);
        let (request_id, _) = requests
            .open_question("call_1", &raw(json!([{"question": "Q?"}])), "orchestrator")
            .unwrap();
        requests.answer(&request_id, &single("q0", "x")).unwrap();
        requests.note_tool_frame("call_1", Some("pending"));
        assert!(!requests.take_resolved(&request_id));
        requests.note_tool_frame("call_1", Some("in_progress"));
        assert!(!requests.take_resolved(&request_id));
        requests.note_tool_frame("call_1", Some("completed"));
        assert!(requests.take_resolved(&request_id));
    }

    #[test]
    fn expiry_marks_a_question_for_a_late_answer() {
        let mut requests = Requests::new(1);
        let (request_id, _) = requests
            .open_question("call_1", &raw(json!([{"question": "Q?"}])), "orchestrator")
            .unwrap();
        assert_eq!(requests.expire(), vec![request_id.clone()]);
        assert!(requests.was_expired(&request_id));
        assert_eq!(
            requests.answer(&request_id, &single("q0", "x")),
            Err("request_not_pending")
        );
    }
}
