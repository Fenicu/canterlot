//! Ввод Antigravity: текст с путями вложений, ожидающие настройки, доказательство
//! приёма эхом `user_message_chunk` и логический ход из нескольких `session/prompt`.
//!
//! Ход открывает исходная команда; steer добавляет в него второй prompt. Ответы
//! приходят в любом порядке, поэтому счётчик неотвеченных RPC живёт здесь, а не
//! в порядке кадров. Закрывает ход ровно один `RunFinished` (§4).

use super::rpc::RpcError;
use crate::adapter::{LocalFile, ModelUsage, Transmission};
use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

pub const PROOF_KIND: &str = "agy_acp_user_echo";
pub const COMPACT_TEXT: &str = "/compact";

/// Текст prompt: сам ввод и пути вложений. Картинки ACP молча выбрасывает (P22),
/// поэтому вложением не идёт ничего — только путь в тексте.
pub fn prompt_text(input: &Transmission) -> Result<String, &'static str> {
    let mut parts = Vec::new();
    if !input.text.is_empty() {
        parts.push(input.text.clone());
    }
    if !input.files.is_empty() {
        parts.push(attachment_lines(&input.files)?);
    }
    let text = parts.join("\n\n");
    if text.len() > MAX_INPUT_TEXT_BYTES {
        return Err("input_too_long");
    }
    Ok(text)
}

fn attachment_lines(files: &[LocalFile]) -> Result<String, &'static str> {
    let mut list = String::new();
    for file in files {
        let path = std::path::absolute(&file.path).map_err(|_| "attachment_missing")?;
        if !path.is_file() {
            return Err("attachment_missing");
        }
        if !list.is_empty() {
            list.push('\n');
        }
        list.push_str(&format!(
            "{} ({}, {} bytes): {}",
            file.name,
            file.mime,
            file.size,
            path.display()
        ));
    }
    Ok(list)
}

pub fn prompt_params(session_id: &str, text: &str) -> Value {
    json!({
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": text}],
    })
}

/// `session/cancel` — уведомление, ответа у него нет.
pub fn cancel_notification(session_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session_id},
    })
}

/// Расход одного ответа prompt. `output_tokens` включает мышление: у Antigravity
/// `thoughtTokens` идут отдельным полем, у canterlot это часть генерации (§6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub seen: bool,
    pub input: u64,
    pub output: u64,
    pub thought: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Вход последнего ответа: размер контекста для индикатора (§8).
    pub last_input: u64,
}

impl Usage {
    pub fn parse(result: &Value) -> Option<Self> {
        let usage = result.get("usage")?;
        if !usage.is_object() {
            return None;
        }
        let number = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        Some(Self {
            seen: true,
            input: number("inputTokens"),
            output: number("outputTokens"),
            thought: number("thoughtTokens"),
            cache_read: number("cachedReadTokens"),
            cache_write: number("cachedWriteTokens"),
            last_input: number("inputTokens"),
        })
    }

    pub fn add(&mut self, other: Option<Self>) {
        let Some(other) = other else {
            return;
        };
        if !self.seen {
            *self = other;
            return;
        }
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.thought = self.thought.saturating_add(other.thought);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.last_input = other.last_input;
    }

    pub fn model_usage(&self, model: &str) -> Option<ModelUsage> {
        self.seen.then(|| ModelUsage {
            model: model.to_owned(),
            agent: "orchestrator".into(),
            input_tokens: self.input,
            output_tokens: self.output.saturating_add(self.thought),
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// Prompt логического хода: исходный или steer.
    Turn,
    /// `session/prompt` со `/compact`, итог — `Compacted`.
    Compact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Turn,
    Compact,
}

#[derive(Debug)]
pub enum Effect {
    Proof(Uuid),
    LateProof(Uuid),
    TurnClosed {
        command_id: Uuid,
        usage: Usage,
        failed: Option<String>,
        stop: Option<String>,
    },
    CompactDone {
        ok: bool,
    },
}

struct Turn {
    command_id: Uuid,
    outstanding: usize,
    usage: Usage,
    /// Хотя бы один prompt хода доказан эхом или ответил успешно.
    accepted: bool,
    failed: Option<String>,
    stop: Option<String>,
    /// Продолжение после вопроса (§6, задача 3): ход ждёт служебный prompt.
    continuation: bool,
}

struct PromptMeta {
    command_id: Uuid,
    kind: PromptKind,
    proved: bool,
}

struct EchoWait {
    /// Prompt, которому принадлежит ожидание: ответ снимает его, чужое эхо не продлевает.
    rpc_id: i64,
    command_id: Uuid,
    text: String,
}

/// Состояние логического хода. Один экземпляр на адаптер, потому что ход один.
#[derive(Default)]
pub struct Tracker {
    turn: Option<Turn>,
    prompts: HashMap<i64, PromptMeta>,
    echoes: VecDeque<EchoWait>,
    waiting: Option<Uuid>,
    proved: HashSet<Uuid>,
    early_rejects: HashMap<Uuid, String>,
    answered_unproven: HashSet<Uuid>,
    unconfirmed: HashSet<Uuid>,
    compaction: Option<Uuid>,
    compact_proved: bool,
    last_stop: Option<String>,
    last_compact_ok: Option<bool>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_turn(&self) -> bool {
        self.turn.is_some()
    }

    pub fn turn_command(&self) -> Option<Uuid> {
        self.turn.as_ref().map(|turn| turn.command_id)
    }

    /// Сколько prompt-ответов хода ещё не пришло.
    pub fn outstanding(&self) -> usize {
        self.turn.as_ref().map_or(0, |turn| turn.outstanding)
    }

    /// Вопрос отвечён: ход ждёт служебный prompt и не закрывается ответом cancel.
    pub fn expect_continuation(&mut self, expected: bool) {
        if let Some(turn) = self.turn.as_mut() {
            turn.continuation = expected;
        }
    }

    pub fn target(&self) -> Option<Target> {
        if self.turn.is_some() {
            Some(Target::Turn)
        } else if self.compaction.is_some() {
            Some(Target::Compact)
        } else {
            None
        }
    }

    pub fn has_compaction(&self) -> bool {
        self.compaction.is_some()
    }

    pub fn last_stop(&self) -> Option<&str> {
        self.last_stop.as_deref()
    }

    pub fn last_compact_ok(&self) -> Option<bool> {
        self.last_compact_ok
    }

    /// Открыть ход исходной командой; второй ход поверх первого не открывается.
    pub fn begin(&mut self, command_id: Uuid) -> bool {
        if self.turn.is_some() {
            return false;
        }
        self.turn = Some(Turn {
            command_id,
            outstanding: 0,
            usage: Usage::default(),
            accepted: false,
            failed: None,
            stop: None,
            continuation: false,
        });
        true
    }

    pub fn register(&mut self, id: i64, command_id: Uuid, kind: PromptKind) {
        self.prompts.insert(
            id,
            PromptMeta {
                command_id,
                kind,
                proved: false,
            },
        );
        match kind {
            PromptKind::Turn => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.outstanding += 1;
                }
            }
            PromptKind::Compact => {
                self.compaction = Some(command_id);
                self.compact_proved = false;
            }
        }
    }

    pub fn is_prompt(&self, id: i64) -> bool {
        self.prompts.contains_key(&id)
    }

    /// Ожидание эха текста: чанк с чужим текстом доказательством не считается.
    pub fn expect_echo(&mut self, rpc_id: i64, command_id: Uuid, text: &str) {
        self.echoes.push_back(EchoWait {
            rpc_id,
            command_id,
            text: text.to_owned(),
        });
    }

    pub fn await_echo(&mut self, command_id: Uuid) {
        self.waiting = Some(command_id);
        self.proved.remove(&command_id);
    }

    pub fn stop_waiting(&mut self) {
        self.waiting = None;
    }

    pub fn take_proved(&mut self, command_id: Uuid) -> bool {
        self.proved.remove(&command_id)
    }

    pub fn early_rejection(&mut self, command_id: Uuid) -> Option<String> {
        self.early_rejects.remove(&command_id)
    }

    /// Ответ пришёл раньше эха: ждать доказательство бессмысленно.
    pub fn take_answered_unproven(&mut self, command_id: Uuid) -> bool {
        self.answered_unproven.remove(&command_id)
    }

    pub fn mark_unconfirmed(&mut self, command_id: Uuid) {
        self.unconfirmed.insert(command_id);
    }

    /// Позднее эхо после `Undetermined` становится `InputAccepted`.
    pub fn on_echo(&mut self, text: &str) -> Option<Effect> {
        // Эхо доказывает приём только живому prompt: ожидание отвечённого prompt
        // иначе съело бы эхо следующего ввода с тем же текстом.
        let prompts = &self.prompts;
        self.echoes
            .retain(|wait| prompts.contains_key(&wait.rpc_id));
        let position = self.echoes.iter().position(|wait| wait.text == text)?;
        let wait = self.echoes.remove(position)?;
        if let Some(meta) = self
            .prompts
            .values_mut()
            .find(|meta| meta.command_id == wait.command_id)
        {
            meta.proved = true;
        }
        if let Some(turn) = self.turn.as_mut()
            && turn.command_id == wait.command_id
        {
            turn.accepted = true;
        }
        if self.compaction == Some(wait.command_id) {
            self.compact_proved = true;
            self.last_compact_ok = None;
        }
        if self.waiting == Some(wait.command_id) {
            self.proved.insert(wait.command_id);
            return Some(Effect::Proof(wait.command_id));
        }
        if self.unconfirmed.remove(&wait.command_id) {
            return Some(Effect::LateProof(wait.command_id));
        }
        None
    }

    /// Ответ prompt: вклад в ход, закрытие хода или итог сжатия.
    pub fn on_response(&mut self, id: i64, result: &Result<Value, RpcError>) -> Option<Effect> {
        let meta = self.prompts.remove(&id)?;
        // Ответ закрывает ожидание эха своего prompt: чужой ввод его не наследует.
        self.echoes.retain(|wait| wait.rpc_id != id);
        let awaiting_echo = !meta.proved && self.waiting == Some(meta.command_id);
        if awaiting_echo {
            self.waiting = None;
            self.answered_unproven.insert(meta.command_id);
        }
        self.proved.remove(&meta.command_id);
        match meta.kind {
            PromptKind::Compact => {
                self.compaction = None;
                let stop = result
                    .as_ref()
                    .ok()
                    .and_then(|value| value["stopReason"].as_str())
                    .map(str::to_owned);
                if !meta.proved {
                    self.unconfirmed.remove(&meta.command_id);
                    if let Err(error) = result {
                        self.early_rejects
                            .insert(meta.command_id, error.message.clone());
                        return None;
                    }
                }
                let ok = stop.as_deref() == Some("end_turn");
                self.last_compact_ok = Some(ok);
                Some(Effect::CompactDone { ok })
            }
            PromptKind::Turn => {
                let turn = self.turn.as_mut()?;
                turn.outstanding = turn.outstanding.saturating_sub(1);
                match result {
                    Ok(value) => {
                        turn.usage.add(Usage::parse(value));
                        turn.accepted = true;
                        if let Some(stop) = value["stopReason"].as_str() {
                            turn.stop = Some(stop.to_owned());
                        }
                    }
                    Err(error) => {
                        self.unconfirmed.remove(&meta.command_id);
                        // Отказ после срока (`Undetermined`) больше некому вручить:
                        // без терминального исхода ход молча пропал бы, а supervisor
                        // остался бы занят. Ранний отказ ждёт `transmit` в `early_rejects`.
                        if meta.proved || !awaiting_echo {
                            turn.accepted = true;
                            turn.failed = Some(error.message.clone());
                        } else {
                            self.early_rejects
                                .insert(meta.command_id, error.message.clone());
                        }
                    }
                }
                if turn.outstanding > 0 || turn.continuation {
                    return None;
                }
                let turn = self.turn.take()?;
                self.echoes
                    .retain(|wait| wait.command_id != turn.command_id);
                self.unconfirmed.remove(&turn.command_id);
                self.last_stop = turn.stop.clone();
                if !turn.accepted {
                    return None;
                }
                Some(Effect::TurnClosed {
                    command_id: turn.command_id,
                    usage: turn.usage,
                    failed: turn.failed,
                    stop: turn.stop,
                })
            }
        }
    }

    /// Смерть процесса: открытый ход закрывает supervisor, сжатие — `Compacted`.
    pub fn on_eof(&mut self) -> Option<Effect> {
        let compact = self.compaction.take();
        let proved = std::mem::take(&mut self.compact_proved);
        self.turn = None;
        self.prompts.clear();
        self.echoes.clear();
        self.waiting = None;
        self.proved.clear();
        self.early_rejects.clear();
        self.answered_unproven.clear();
        self.unconfirmed.clear();
        if compact.is_some() && proved {
            self.last_compact_ok = Some(false);
            Some(Effect::CompactDone { ok: false })
        } else {
            None
        }
    }

    /// Сжатие не подтвердилось до срока: ответ уже не припишется к команде.
    pub fn forget_command(&mut self, command_id: Uuid) {
        let removed: Vec<PromptKind> = self
            .prompts
            .values()
            .filter(|meta| meta.command_id == command_id)
            .map(|meta| meta.kind)
            .collect();
        for kind in removed {
            if kind == PromptKind::Turn
                && let Some(turn) = self.turn.as_mut()
            {
                turn.outstanding = turn.outstanding.saturating_sub(1);
            }
        }
        self.prompts.retain(|_, meta| meta.command_id != command_id);
        self.echoes.retain(|wait| wait.command_id != command_id);
        self.unconfirmed.remove(&command_id);
        self.early_rejects.remove(&command_id);
        self.answered_unproven.remove(&command_id);
        if self
            .turn
            .as_ref()
            .is_some_and(|turn| turn.command_id == command_id && turn.outstanding == 0)
        {
            self.turn = None;
        }
        if self.compaction == Some(command_id) {
            self.compaction = None;
            self.compact_proved = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn input(text: &str) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: text.into(),
            files: Vec::new(),
            settings: None,
        }
    }

    fn file(path: &str, image: bool) -> LocalFile {
        LocalFile {
            path: PathBuf::from(path),
            name: "файл.txt".into(),
            mime: if image {
                "image/png".into()
            } else {
                "text/plain".into()
            },
            size: 3,
            image,
        }
    }

    #[test]
    fn prompt_is_one_text_block_with_attachment_paths() {
        let path = std::env::temp_dir().join(format!("agy-input-{}", Uuid::now_v7()));
        std::fs::write(&path, b"hi").unwrap();
        let mut message = input("смотри");
        message.files = vec![
            file(path.to_str().unwrap(), true),
            file(path.to_str().unwrap(), false),
        ];
        let text = prompt_text(&message).unwrap();
        assert!(text.starts_with("смотри\n\n"));
        assert_eq!(text.matches(path.to_str().unwrap()).count(), 2);
        let params = prompt_params("session", &text);
        assert_eq!(params["sessionId"], "session");
        assert_eq!(params["prompt"].as_array().unwrap().len(), 1);
        assert_eq!(params["prompt"][0]["type"], "text");
        assert!(params["prompt"][0].get("data").is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_missing_attachment_is_refused() {
        let mut message = input("смотри");
        message.files = vec![file("/definitely/not/here.txt", false)];
        assert_eq!(prompt_text(&message), Err("attachment_missing"));
    }

    #[test]
    fn usage_maps_thought_into_output_and_keeps_cache() {
        let result = json!({"stopReason": "end_turn", "usage": {
            "inputTokens": 100, "outputTokens": 7, "thoughtTokens": 5,
            "cachedReadTokens": 3, "cachedWriteTokens": 2, "totalTokens": 115,
        }});
        let usage = Usage::parse(&result).unwrap();
        let mapped = usage.model_usage("gemini-3.8-flash-low").unwrap();
        assert_eq!(mapped.input_tokens, 100);
        assert_eq!(mapped.output_tokens, 12);
        assert_eq!(mapped.cache_read_tokens, 3);
        assert_eq!(mapped.cache_write_tokens, 2);
        assert_eq!(mapped.model, "gemini-3.8-flash-low");
        assert_eq!(mapped.agent, "orchestrator");
    }

    #[test]
    fn missing_usage_is_allowed() {
        assert_eq!(Usage::parse(&json!({"stopReason": "end_turn"})), None);
        let usage = Usage::default();
        assert!(usage.model_usage("m").is_none());
    }

    fn closed(effect: Option<Effect>) -> (Uuid, Usage, Option<String>, Option<String>) {
        match effect {
            Some(Effect::TurnClosed {
                command_id,
                usage,
                failed,
                stop,
            }) => (command_id, usage, failed, stop),
            other => panic!("not a turn close: {other:?}"),
        }
    }

    fn usage(input: u64, output: u64, thought: u64) -> Value {
        json!({"stopReason": "end_turn", "usage": {
            "inputTokens": input, "outputTokens": output, "thoughtTokens": thought,
            "cachedReadTokens": 0, "cachedWriteTokens": 0,
        }})
    }

    // t7-cancel-p3-steer: ответ второго prompt приходит раньше первого.
    #[test]
    fn reverse_response_order_closes_one_turn_with_summed_usage() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let mut tracker = Tracker::new();
        assert!(tracker.begin(first));
        tracker.register(4, first, PromptKind::Turn);
        tracker.expect_echo(4, first, "one");
        tracker.await_echo(first);
        assert!(matches!(tracker.on_echo("one"), Some(Effect::Proof(id)) if id == first));
        assert!(tracker.take_proved(first));
        tracker.stop_waiting();

        tracker.register(5, second, PromptKind::Turn);
        tracker.expect_echo(5, second, "two");
        tracker.await_echo(second);
        assert!(matches!(tracker.on_echo("two"), Some(Effect::Proof(id)) if id == second));
        assert!(tracker.take_proved(second));
        tracker.stop_waiting();

        assert!(tracker.on_response(5, &Ok(usage(7538, 25, 0))).is_none());
        let (command, usage, failed, stop) =
            closed(tracker.on_response(4, &Ok(serde_json::json!({"stopReason": "end_turn"}))));
        assert_eq!(command, first);
        assert_eq!(usage.input, 7538);
        assert_eq!(usage.output, 25);
        assert_eq!(failed, None);
        assert_eq!(stop.as_deref(), Some("end_turn"));
        assert!(!tracker.has_turn());
    }

    #[test]
    fn forward_response_order_closes_the_same_turn() {
        let first = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(first);
        tracker.register(4, first, PromptKind::Turn);
        tracker.expect_echo(4, first, "one");
        tracker.on_echo("one");
        tracker.register(5, first, PromptKind::Turn);
        tracker.expect_echo(5, first, "two");
        tracker.on_echo("two");
        assert!(tracker.on_response(4, &Ok(usage(10, 1, 2))).is_none());
        let (command, usage, ..) = closed(tracker.on_response(5, &Ok(usage(20, 3, 4))));
        assert_eq!(command, first);
        assert_eq!(usage.input, 30);
        assert_eq!(usage.output, 4);
        assert_eq!(usage.thought, 6);
    }

    #[test]
    fn an_echo_of_another_text_is_no_proof() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(2, command, PromptKind::Turn);
        tracker.expect_echo(2, command, "our text");
        tracker.await_echo(command);
        assert!(tracker.on_echo("foreign").is_none());
        assert!(!tracker.take_proved(command));
    }

    #[test]
    fn a_late_echo_after_the_deadline_becomes_a_late_proof() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(2, command, PromptKind::Turn);
        tracker.expect_echo(2, command, "text");
        tracker.await_echo(command);
        tracker.stop_waiting();
        tracker.mark_unconfirmed(command);
        assert!(matches!(tracker.on_echo("text"), Some(Effect::LateProof(id)) if id == command));
    }

    #[test]
    fn an_error_before_the_echo_rejects_without_a_run_finished() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(2, command, PromptKind::Turn);
        tracker.expect_echo(2, command, "text");
        tracker.await_echo(command);
        assert!(
            tracker
                .on_response(
                    2,
                    &Err(RpcError {
                        code: -32603,
                        message: "trajectory not found".into(),
                        data: None,
                    }),
                )
                .is_none()
        );
        assert_eq!(
            tracker.early_rejection(command).as_deref(),
            Some("trajectory not found")
        );
        assert!(!tracker.has_turn());
    }

    #[test]
    fn an_error_after_the_echo_fails_the_turn() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(2, command, PromptKind::Turn);
        tracker.expect_echo(2, command, "text");
        tracker.on_echo("text");
        let (id, _, failed, _) = closed(tracker.on_response(
            2,
            &Err(RpcError {
                code: -32603,
                message: "quota".into(),
                data: None,
            }),
        ));
        assert_eq!(id, command);
        assert_eq!(failed.as_deref(), Some("quota"));
    }

    #[test]
    fn a_response_before_the_echo_does_not_wait_for_the_deadline() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(2, command, PromptKind::Turn);
        tracker.expect_echo(2, command, "text");
        tracker.await_echo(command);
        assert!(tracker.on_response(2, &Ok(usage(1, 1, 0))).is_some());
        assert!(tracker.take_answered_unproven(command));
    }

    #[test]
    fn a_compact_error_before_the_echo_is_a_failed_command_not_an_event() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.register(5, command, PromptKind::Compact);
        tracker.expect_echo(5, command, COMPACT_TEXT);
        tracker.await_echo(command);
        assert!(
            tracker
                .on_response(
                    5,
                    &Err(RpcError {
                        code: -32603,
                        message: "no".into(),
                        data: None,
                    }),
                )
                .is_none()
        );
        assert_eq!(tracker.early_rejection(command).as_deref(), Some("no"));
        assert!(!tracker.has_compaction());
    }

    #[test]
    fn a_proven_compact_ends_with_end_turn() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.register(5, command, PromptKind::Compact);
        tracker.expect_echo(5, command, COMPACT_TEXT);
        tracker.on_echo(COMPACT_TEXT);
        assert!(tracker.has_compaction());
        assert!(matches!(
            tracker.on_response(5, &Ok(usage(1, 1, 0))),
            Some(Effect::CompactDone { ok: true })
        ));
        assert_eq!(tracker.last_compact_ok(), Some(true));
    }

    #[test]
    fn forgetting_a_steer_keeps_the_original_turn() {
        let first = Uuid::now_v7();
        let steer = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(first);
        tracker.register(4, first, PromptKind::Turn);
        tracker.register(5, steer, PromptKind::Turn);
        tracker.forget_command(steer);
        assert!(tracker.has_turn());
        let (command, ..) = closed(tracker.on_response(4, &Ok(usage(1, 1, 0))));
        assert_eq!(command, first);
    }

    #[test]
    fn forgetting_the_original_prompt_closes_the_turn() {
        let first = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(first);
        tracker.register(4, first, PromptKind::Turn);
        tracker.forget_command(first);
        assert!(!tracker.has_turn());
    }

    // §6: ответ на вопрос ждёт cancel, затем служебный prompt того же хода.
    #[test]
    fn a_question_continuation_keeps_the_turn_open_until_the_service_prompt() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.begin(command);
        tracker.register(4, command, PromptKind::Turn);
        tracker.expect_echo(4, command, "ask");
        tracker.on_echo("ask");
        tracker.expect_continuation(true);
        assert!(
            tracker
                .on_response(4, &Ok(json!({"stopReason": "cancelled"})))
                .is_none()
        );
        assert!(tracker.has_turn());
        assert_eq!(tracker.outstanding(), 0);
        tracker.expect_continuation(false);
        tracker.register(5, command, PromptKind::Turn);
        assert_eq!(tracker.outstanding(), 1);
        let (closed, ..) = closed(tracker.on_response(5, &Ok(usage(2, 1, 0))));
        assert_eq!(closed, command);
        assert!(!tracker.has_turn());
    }

    #[test]
    fn eof_reports_failed_compaction_only_after_its_echo() {
        let command = Uuid::now_v7();
        let mut tracker = Tracker::new();
        tracker.register(5, command, PromptKind::Compact);
        tracker.expect_echo(5, command, COMPACT_TEXT);
        assert!(tracker.on_eof().is_none());
        assert_eq!(tracker.last_compact_ok(), None);

        let command = Uuid::now_v7();
        tracker.register(5, command, PromptKind::Compact);
        tracker.expect_echo(5, command, COMPACT_TEXT);
        tracker.on_echo(COMPACT_TEXT);
        assert!(matches!(
            tracker.on_eof(),
            Some(Effect::CompactDone { ok: false })
        ));
    }
}
