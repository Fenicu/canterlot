//! Сопоставление: наши вводы ↔ эхо/ответы CLI, tool_use ↔ tool_result.

use std::collections::HashMap;

use serde_json::Value;

use crate::protocol::{self, Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delivery {
    /// Байты записаны в stdin, подтверждения нет.
    Written,
    /// CLI вернул эхо (`isReplay`) с нашим UUID.
    Echoed,
    /// Первый кадр ответа несёт наш UUID в `user_message_uuid(s)`.
    Answering,
    /// Итог хода (`result`) несёт наш UUID.
    Answered,
}

#[derive(Debug, Clone)]
pub struct InputEntry {
    pub uuid: String,
    pub written_ms: u64,
    pub echo_ms: Option<u64>,
    pub first_reply_ms: Option<u64>,
    pub result_ms: Option<u64>,
}

impl InputEntry {
    pub fn delivery(&self) -> Delivery {
        if self.result_ms.is_some() {
            Delivery::Answered
        } else if self.first_reply_ms.is_some() {
            Delivery::Answering
        } else if self.echo_ms.is_some() {
            Delivery::Echoed
        } else {
            Delivery::Written
        }
    }
}

#[derive(Default)]
pub struct InputLedger {
    entries: Vec<InputEntry>,
    foreign_echoes: Vec<String>,
}

impl InputLedger {
    pub fn written(&mut self, uuid: &str, t_ms: u64) {
        self.entries.push(InputEntry {
            uuid: uuid.to_owned(),
            written_ms: t_ms,
            echo_ms: None,
            first_reply_ms: None,
            result_ms: None,
        });
    }

    pub fn observe(&mut self, t_ms: u64, v: &Value) {
        match protocol::classify(v) {
            Kind::User if protocol::is_replay(v) => {
                let Some(id) = protocol::uuid(v) else { return };
                match self.entries.iter_mut().find(|e| e.uuid == id) {
                    Some(e) => {
                        e.echo_ms.get_or_insert(t_ms);
                    }
                    None => {
                        if !self.foreign_echoes.iter().any(|f| f == id) {
                            self.foreign_echoes.push(id.to_owned());
                        }
                    }
                }
            }
            Kind::StreamEvent | Kind::Assistant => {
                for id in protocol::answered_uuids(v) {
                    if let Some(e) = self.entries.iter_mut().find(|e| e.uuid == id) {
                        e.first_reply_ms.get_or_insert(t_ms);
                    }
                }
            }
            Kind::Result => {
                for id in protocol::answered_uuids(v) {
                    if let Some(e) = self.entries.iter_mut().find(|e| e.uuid == id) {
                        e.result_ms.get_or_insert(t_ms);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn get(&self, uuid: &str) -> Option<&InputEntry> {
        self.entries.iter().find(|e| e.uuid == uuid)
    }

    /// Эхо с UUID, которых мы не отправляли.
    pub fn foreign_echoes(&self) -> &[String] {
        &self.foreign_echoes
    }
}

#[derive(Default)]
pub struct ToolLedger {
    uses: Vec<protocol::ToolUse>,
    results: HashMap<String, Vec<protocol::ToolResult>>,
}

impl ToolLedger {
    pub fn observe(&mut self, v: &Value) {
        match protocol::classify(v) {
            Kind::Assistant => self.uses.extend(protocol::tool_uses(v)),
            Kind::User => {
                for r in protocol::tool_results(v) {
                    self.results
                        .entry(r.tool_use_id.clone())
                        .or_default()
                        .push(r);
                }
            }
            _ => {}
        }
    }

    pub fn uses(&self) -> &[protocol::ToolUse] {
        &self.uses
    }

    pub fn result_for(&self, tool_use_id: &str) -> Option<&protocol::ToolResult> {
        self.results.get(tool_use_id).and_then(|r| r.first())
    }

    /// tool_use без результата.
    pub fn unmatched_uses(&self) -> Vec<String> {
        self.uses
            .iter()
            .filter(|u| !self.results.contains_key(&u.id))
            .map(|u| u.id.clone())
            .collect()
    }

    /// Результаты, для которых не было tool_use, и повторные результаты.
    pub fn orphan_or_duplicate_results(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .results
            .iter()
            .filter(|(id, rs)| rs.len() > 1 || !self.uses.iter().any(|u| &u.id == *id))
            .map(|(id, _)| id.clone())
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delivery_progresses_through_echo_reply_and_result() {
        let mut l = InputLedger::default();
        l.written("u1", 10);
        assert_eq!(
            l.get("u1").map(InputEntry::delivery),
            Some(Delivery::Written)
        );
        l.observe(12, &json!({"type":"user","isReplay":true,"uuid":"u1"}));
        assert_eq!(
            l.get("u1").map(InputEntry::delivery),
            Some(Delivery::Echoed)
        );
        l.observe(20, &json!({"type":"stream_event","user_message_uuid":"u1","event":{"type":"message_start"}}));
        l.observe(
            25,
            &json!({"type":"assistant","user_message_uuid":"u1","message":{"content":[]}}),
        );
        let e = l.get("u1").expect("entry");
        assert_eq!(e.delivery(), Delivery::Answering);
        assert_eq!(e.first_reply_ms, Some(20));
        l.observe(30, &json!({"type":"result","user_message_uuids":["u1"]}));
        let e = l.get("u1").expect("entry");
        assert_eq!(e.delivery(), Delivery::Answered);
        assert_eq!((e.echo_ms, e.result_ms), (Some(12), Some(30)));
    }

    #[test]
    fn merged_turn_marks_all_listed_inputs() {
        let mut l = InputLedger::default();
        l.written("a", 1);
        l.written("b", 2);
        l.observe(
            9,
            &json!({"type":"result","user_message_uuid":"b","user_message_uuids":["a","b"]}),
        );
        assert_eq!(l.get("a").and_then(|e| e.result_ms), Some(9));
        assert_eq!(l.get("b").and_then(|e| e.result_ms), Some(9));
    }

    #[test]
    fn foreign_echo_is_tracked_and_first_timestamps_are_kept() {
        let mut l = InputLedger::default();
        l.written("u1", 1);
        l.observe(5, &json!({"type":"user","isReplay":true,"uuid":"zzz"}));
        l.observe(6, &json!({"type":"user","isReplay":true,"uuid":"u1"}));
        l.observe(7, &json!({"type":"user","isReplay":true,"uuid":"u1"}));
        assert_eq!(l.foreign_echoes(), ["zzz".to_owned()]);
        assert_eq!(l.get("u1").and_then(|e| e.echo_ms), Some(6));
    }

    #[test]
    fn non_replay_user_frames_are_not_echoes() {
        let mut l = InputLedger::default();
        l.written("u1", 1);
        l.observe(
            5,
            &json!({"type":"user","uuid":"u1","message":{"content":[]}}),
        );
        assert_eq!(
            l.get("u1").map(InputEntry::delivery),
            Some(Delivery::Written)
        );
    }

    #[test]
    fn tool_ledger_links_results_and_flags_anomalies() {
        let mut t = ToolLedger::default();
        t.observe(&json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}},
            {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"x"}},
            {"type":"tool_use","id":"t3","name":"Read","input":{"file_path":"y"}}]}}));
        t.observe(&json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"a"},
            {"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"missing"},
            {"type":"tool_result","tool_use_id":"t2","content":"again"},
            {"type":"tool_result","tool_use_id":"tx","content":"orphan"}]}}));
        assert_eq!(t.uses().len(), 3);
        assert!(t.result_for("t2").is_some_and(|r| r.is_error));
        assert_eq!(t.unmatched_uses(), vec!["t3".to_owned()]);
        assert_eq!(
            t.orphan_or_duplicate_results(),
            vec!["t2".to_owned(), "tx".to_owned()]
        );
    }
}
