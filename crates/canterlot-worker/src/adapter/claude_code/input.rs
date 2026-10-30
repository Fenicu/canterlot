//! Ввод Claude Code: строго один ход за раз, сообщения пользователя с вложениями,
//! доказательство доставки кадром ответа, смена модели и режима между ходами,
//! прерывание только SIGINT (решение владельца OD-2).

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};
use tokio::time::Instant;
use uuid::Uuid;

use super::parser::{Parsed, Parser};
use super::process::{CliProcess, Signal, StdoutLine};
use crate::adapter::{
    AdapterEvent, InterruptOutcome, LocalFile, Proof, SettingsChange, Transmission,
    TransmitOutcome, limit_note,
};

pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
pub const PROOF_KIND: &str = "claude_code_answer_frame";
const MAX_NATIVE_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
const NATIVE_IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Модель, усилие и режим разрешений в словаре canterlot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchSettings {
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveTurn {
    pub command_id: Uuid,
    pub attempt: u32,
    pub delivery_proven: bool,
}

pub enum Next {
    /// Одна строка NDJSON для stdin.
    Write(Value),
    Idle,
}

impl fmt::Debug for Next {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(line) => f
                .debug_struct("Write")
                .field("type", &line["type"])
                .finish_non_exhaustive(),
            Self::Idle => f.write_str("Idle"),
        }
    }
}

struct Queued {
    command_id: Uuid,
    attempt: u32,
    text: String,
    files: Vec<LocalFile>,
    settings: Option<SettingsChange>,
}

enum ControlKind {
    Model(String),
    PermissionMode(String),
}

struct PendingControl {
    request_id: String,
    command_id: Uuid,
    kind: ControlKind,
    deadline: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interrupt {
    None,
    Sent,
    Acknowledged,
}

pub struct InputQueue {
    launch_mode: String,
    effective: LaunchSettings,
    owner_cli_mode: Option<String>,
    mode_changed: bool,
    bypass_allowed: bool,
    control_timeout: Duration,
    fifo: VecDeque<Queued>,
    active: Option<ActiveTurn>,
    control: Option<PendingControl>,
    rejected: Vec<(Uuid, &'static str)>,
    proven: HashSet<Uuid>,
    weekly_noted: u16,
    interrupt: Interrupt,
}

impl fmt::Debug for InputQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputQueue")
            .field("queued", &self.fifo.len())
            .field("active", &self.active)
            .field("control_pending", &self.control.is_some())
            .field("weekly_noted", &self.weekly_noted)
            .finish_non_exhaustive()
    }
}

impl InputQueue {
    pub fn new(launch: LaunchSettings) -> Self {
        Self {
            launch_mode: launch.permission_mode.clone(),
            bypass_allowed: launch.permission_mode == "bypass",
            effective: launch,
            owner_cli_mode: None,
            mode_changed: false,
            control_timeout: CONTROL_TIMEOUT,
            fifo: VecDeque::new(),
            active: None,
            control: None,
            rejected: Vec::new(),
            proven: HashSet::new(),
            weekly_noted: 0,
            interrupt: Interrupt::None,
        }
    }

    pub fn with_control_timeout(mut self, timeout: Duration) -> Self {
        self.control_timeout = timeout;
        self
    }

    pub fn effective(&self) -> &LaunchSettings {
        &self.effective
    }

    /// Применить настройки перезапущенного CLI: флаги уже переданы при запуске,
    /// управляющие запросы для этой смены не нужны.
    pub fn apply_restarted(&mut self, model: &str, effort: &str, permission_mode: &str) {
        self.effective.model = model.to_owned();
        self.effective.effort = effort.to_owned();
        self.effective.permission_mode = permission_mode.to_owned();
        if permission_mode == "bypass" {
            self.bypass_allowed = true;
        }
    }

    pub fn active(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }

    pub fn is_active(&self, command_id: Uuid) -> bool {
        self.active
            .is_some_and(|turn| turn.command_id == command_id)
    }

    pub fn is_proven(&self, command_id: Uuid) -> bool {
        self.proven.contains(&command_id)
    }

    pub fn control_deadline(&self) -> Option<Instant> {
        self.control.as_ref().map(|control| control.deadline)
    }

    /// Проверка смены настроек до постановки ввода (для `validate_transmission`).
    pub fn validate(&self, change: Option<&SettingsChange>) -> Result<(), &'static str> {
        let Some(change) = change else {
            return Ok(());
        };
        if change
            .effort
            .as_ref()
            .is_some_and(|effort| *effort != self.effective.effort)
        {
            return Err("effort_change_unsupported");
        }
        if let Some(mode) = change
            .permission_mode
            .as_ref()
            .filter(|mode| **mode != self.effective.permission_mode)
        {
            self.cli_permission_mode(mode)?;
        }
        Ok(())
    }

    fn cli_permission_mode(&self, mode: &str) -> Result<String, &'static str> {
        match mode {
            "ask" => Ok("default".into()),
            "read_only" => Ok("plan".into()),
            "bypass" if self.bypass_allowed => Ok("bypassPermissions".into()),
            "bypass" => Err("permission_mode_needs_restart"),
            "owner" => match &self.owner_cli_mode {
                Some(cli) if self.launch_mode == "owner" => Ok(cli.clone()),
                _ => Err("permission_mode_needs_restart"),
            },
            _ => Err("settings_unavailable"),
        }
    }

    pub fn enqueue(&mut self, t: &Transmission) {
        if self.proven.contains(&t.command_id) {
            return;
        }
        self.fifo.retain(|queued| queued.command_id != t.command_id);
        self.fifo.push_back(Queued {
            command_id: t.command_id,
            attempt: t.attempt,
            text: t.text.clone(),
            files: t.files.clone(),
            settings: t.settings.clone(),
        });
    }

    /// Убрать ещё не записанный ввод (ход не начат).
    pub fn withdraw(&mut self, command_id: Uuid) {
        self.fifo.retain(|queued| queued.command_id != command_id);
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.command_id == command_id)
        {
            self.control = None;
        }
    }

    pub fn take_rejection(&mut self, command_id: Uuid) -> Option<&'static str> {
        let index = self.rejected.iter().position(|(id, _)| *id == command_id)?;
        Some(self.rejected.remove(index).1)
    }

    fn reject_front(&mut self, reason: &'static str) {
        if let Some(queued) = self.fifo.pop_front() {
            self.rejected.push((queued.command_id, reason));
        }
    }

    /// Следующая строка для stdin; `Idle`, пока идёт ход или ждёт ответа управляющий запрос.
    pub fn next(&mut self, weekly: Option<(u16, Option<u64>)>) -> Next {
        if self.active.is_some() || self.control.is_some() {
            return Next::Idle;
        }
        while let Some(front) = self.fifo.front() {
            let command_id = front.command_id;
            if self.proven.contains(&command_id) {
                self.fifo.pop_front();
                continue;
            }
            let change = front.settings.clone().unwrap_or_default();
            if let Err(reason) = self.validate(Some(&change)) {
                self.reject_front(reason);
                continue;
            }
            if let Some(model) = change.model.filter(|model| *model != self.effective.model) {
                let request = json!({"subtype": "set_model", "model": model});
                return self.control_request(command_id, ControlKind::Model(model), request);
            }
            if let Some(mode) = change
                .permission_mode
                .filter(|mode| *mode != self.effective.permission_mode)
            {
                match self.cli_permission_mode(&mode) {
                    Ok(cli) => {
                        let request = json!({"subtype": "set_permission_mode", "mode": cli});
                        return self.control_request(
                            command_id,
                            ControlKind::PermissionMode(mode),
                            request,
                        );
                    }
                    Err(reason) => {
                        self.reject_front(reason);
                        continue;
                    }
                }
            }
            let note = self.weekly_note(weekly);
            let Some(queued) = self.fifo.pop_front() else {
                break;
            };
            match build_message(
                queued.command_id,
                &queued.text,
                &queued.files,
                note.as_ref().map(|(_, text)| text.as_str()),
            ) {
                Ok(message) => {
                    if let Some((threshold, _)) = note {
                        self.weekly_noted = threshold;
                    }
                    self.active = Some(ActiveTurn {
                        command_id: queued.command_id,
                        attempt: queued.attempt,
                        delivery_proven: false,
                    });
                    self.interrupt = Interrupt::None;
                    return Next::Write(message);
                }
                Err(_) => self
                    .rejected
                    .push((queued.command_id, "attachment_unreadable")),
            }
        }
        Next::Idle
    }

    fn control_request(&mut self, command_id: Uuid, kind: ControlKind, request: Value) -> Next {
        let request_id = format!("canterlot-{}", Uuid::now_v7());
        self.control = Some(PendingControl {
            request_id: request_id.clone(),
            command_id,
            kind,
            deadline: Instant::now() + self.control_timeout,
        });
        Next::Write(json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request,
        }))
    }

    fn finish_control(&mut self, control: PendingControl, success: bool) {
        match (control.kind, success) {
            (ControlKind::Model(model), true) => self.effective.model = model,
            (ControlKind::PermissionMode(mode), true) => {
                self.effective.permission_mode = mode;
                self.mode_changed = true;
            }
            (kind, false) => {
                if self
                    .fifo
                    .front()
                    .is_some_and(|queued| queued.command_id == control.command_id)
                {
                    self.reject_front(match kind {
                        ControlKind::Model(_) => "model_change_failed",
                        ControlKind::PermissionMode(_) => "permission_mode_change_failed",
                    });
                }
            }
        }
    }

    /// Ответа на управляющий запрос нет в срок: смена не применена, ввод не отправляется.
    pub fn control_timed_out(&mut self) {
        if let Some(control) = self.control.take() {
            self.finish_control(control, false);
        }
    }

    fn weekly_note(&self, weekly: Option<(u16, Option<u64>)>) -> Option<(u16, String)> {
        limit_note::next_threshold(weekly, self.weekly_noted, "Claude")
    }

    /// Доказательство доставки, конец хода, ответы на управляющие запросы.
    pub fn on_parsed(&mut self, parsed: &Parsed) -> Option<Proof> {
        match parsed {
            Parsed::AnswerFrame { user_message_uuids } => {
                let turn = self.active.as_mut()?;
                if turn.delivery_proven || !user_message_uuids.contains(&turn.command_id) {
                    return None;
                }
                turn.delivery_proven = true;
                self.proven.insert(turn.command_id);
                return Some(Proof {
                    command_id: turn.command_id,
                    kind: PROOF_KIND,
                });
            }
            Parsed::Event(AdapterEvent::RunFinished {
                input_command_id, ..
            }) => {
                if self.is_active(*input_command_id) {
                    self.active = None;
                }
            }
            Parsed::ControlResponse {
                request_id,
                success,
            } => {
                if let Some(control) = self
                    .control
                    .take_if(|control| control.request_id == *request_id)
                {
                    self.finish_control(control, *success);
                }
            }
            Parsed::Init {
                permission_mode, ..
            } => {
                if permission_mode == "bypassPermissions" {
                    self.bypass_allowed = true;
                }
                if self.owner_cli_mode.is_none()
                    && !self.mode_changed
                    && !permission_mode.is_empty()
                {
                    self.owner_cli_mode = Some(permission_mode.clone());
                }
            }
            Parsed::TurnError {
                terminal_reason: Some(reason),
                ..
            } if is_interrupt_reason(reason) && self.interrupt == Interrupt::Sent => {
                self.interrupt = Interrupt::Acknowledged;
            }
            _ => {}
        }
        None
    }

    /// Отметить отправку SIGINT; `false`, если хода нет и прерывать нечего.
    pub fn interrupt_sent(&mut self) -> bool {
        if self.active.is_none() {
            return false;
        }
        self.interrupt = Interrupt::Sent;
        true
    }

    pub fn interrupt_acknowledged(&self) -> bool {
        self.interrupt == Interrupt::Acknowledged
    }
}

/// Конец хода после SIGINT: прерван вызов инструмента (`aborted_tools`)
/// или поток ответа модели (`aborted_streaming`, записан CLI 2.1.311).
pub fn is_interrupt_reason(reason: &str) -> bool {
    matches!(reason, "aborted_tools" | "aborted_streaming")
}

/// Сообщение пользователя stream-json: строка, если есть только текст владельца;
/// иначе блоки картинок, пометка canterlot, текст и перечень вложений с путями.
pub fn user_message(t: &Transmission, note: Option<&str>) -> std::io::Result<Value> {
    build_message(t.command_id, &t.text, &t.files, note)
}

fn build_message(
    command_id: Uuid,
    text: &str,
    files: &[LocalFile],
    note: Option<&str>,
) -> std::io::Result<Value> {
    let content = if note.is_none() && files.is_empty() {
        Value::String(text.to_owned())
    } else {
        let mut blocks = Vec::new();
        for file in files {
            if let Some(block) = native_image(file)? {
                blocks.push(block);
            }
        }
        if let Some(note) = note {
            blocks.push(text_block(note));
        }
        if !text.is_empty() {
            blocks.push(text_block(text));
        }
        if !files.is_empty() {
            blocks.push(text_block(&attachment_list(files)?));
        }
        Value::Array(blocks)
    };
    Ok(json!({
        "type": "user",
        "uuid": command_id.to_string(),
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
    }))
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

fn native_image(file: &LocalFile) -> std::io::Result<Option<Value>> {
    if !file.image || !NATIVE_IMAGE_MIMES.contains(&file.mime.as_str()) {
        return Ok(None);
    }
    if std::fs::metadata(&file.path)?.len() > MAX_NATIVE_IMAGE_BYTES {
        return Ok(None);
    }
    let bytes = std::fs::read(&file.path)?;
    if bytes.len() as u64 > MAX_NATIVE_IMAGE_BYTES {
        return Ok(None);
    }
    Ok(Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": file.mime,
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        },
    })))
}

fn attachment_list(files: &[LocalFile]) -> std::io::Result<String> {
    let mut list = String::from("Attached files:");
    for file in files {
        let path = std::path::absolute(&file.path)?;
        list.push_str(&format!(
            "\n{} ({}, {} bytes): {}",
            file.name,
            file.mime,
            file.size,
            path.display()
        ));
    }
    Ok(list)
}

/// Очередь ввода вместе с разборщиком потока: запись в stdin и чтение stdout
/// для `transmit` и `interrupt` адаптера. Разобранные кадры копятся в порядке
/// чтения и отдаются через `next_parsed`.
pub struct InputDriver {
    pub queue: InputQueue,
    pub parser: Parser,
    parsed: VecDeque<Parsed>,
    eof: bool,
}

impl fmt::Debug for InputDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputDriver")
            .field("queue", &self.queue)
            .field("parsed", &self.parsed.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

fn proof(command_id: Uuid) -> TransmitOutcome {
    TransmitOutcome::Accepted(Proof {
        command_id,
        kind: PROOF_KIND,
    })
}

impl InputDriver {
    pub fn new(queue: InputQueue, parser: Parser) -> Self {
        Self {
            queue,
            parser,
            parsed: VecDeque::new(),
            eof: false,
        }
    }

    pub fn eof(&self) -> bool {
        self.eof
    }

    pub fn next_parsed(&mut self) -> Option<Parsed> {
        self.parsed.pop_front()
    }

    pub fn feed(&mut self, line: StdoutLine) {
        match line {
            StdoutLine::Line(line) => {
                for parsed in self.parser.feed(&line) {
                    self.queue.on_parsed(&parsed);
                    self.parsed.push_back(parsed);
                }
            }
            StdoutLine::Oversized(_) => {
                self.parsed
                    .push_back(Parsed::Event(AdapterEvent::Diagnostic {
                        kind: "oversized_frame",
                        event_type: String::new(),
                        required: false,
                    }))
            }
            StdoutLine::Eof => self.eof = true,
        }
    }

    /// Прочитать и разобрать одну строку stdout; отмена безопасна.
    pub async fn pump(&mut self, cli: &mut CliProcess) {
        let line = cli.next_line().await;
        self.feed(line);
    }

    /// Ввод уходит в stdin только между ходами; `Accepted` только по кадру ответа с его UUID.
    /// `deadline` отсчитывается от записи сообщения, управляющий запрос ждёт свой срок.
    pub async fn transmit(
        &mut self,
        cli: &mut CliProcess,
        input: &Transmission,
        weekly: Option<(u16, Option<u64>)>,
        deadline: Duration,
    ) -> TransmitOutcome {
        let command_id = input.command_id;
        if self.queue.is_proven(command_id) {
            return proof(command_id);
        }
        if let Err(reason) = self.queue.validate(input.settings.as_ref()) {
            return TransmitOutcome::Rejected(reason.into());
        }
        self.queue.enqueue(input);
        let waiting_since = Instant::now();
        let mut written: Option<Instant> = None;
        loop {
            if self.queue.is_proven(command_id) {
                return proof(command_id);
            }
            if let Some(reason) = self.queue.take_rejection(command_id) {
                return TransmitOutcome::Rejected(reason.into());
            }
            if written.is_some() && !self.queue.is_active(command_id) {
                return TransmitOutcome::Undetermined;
            }
            if self.eof {
                self.queue.withdraw(command_id);
                return TransmitOutcome::Undetermined;
            }
            if let Next::Write(line) = self.queue.next(weekly) {
                if let Some(turn) = self.queue.active() {
                    self.parser.set_input_command(turn.command_id);
                    if turn.command_id == command_id && written.is_none() {
                        written = Some(Instant::now());
                    }
                }
                let limit = written.map_or(waiting_since + deadline, |at| at + deadline);
                match tokio::time::timeout_at(limit, cli.write_line(&line)).await {
                    Ok(Ok(())) => continue,
                    _ => {
                        self.queue.withdraw(command_id);
                        return TransmitOutcome::Undetermined;
                    }
                }
            }
            let limit = match (written, self.queue.control_deadline()) {
                (Some(at), _) => at + deadline,
                (None, Some(control)) => control,
                (None, None) => waiting_since + deadline,
            };
            if tokio::time::timeout_at(limit, self.pump(cli))
                .await
                .is_err()
            {
                if self.queue.control_deadline().is_some() {
                    self.queue.control_timed_out();
                    continue;
                }
                if written.is_some() {
                    return TransmitOutcome::Undetermined;
                }
                self.queue.withdraw(command_id);
                return TransmitOutcome::Rejected("turn_active".into());
            }
        }
    }

    /// SIGINT процессу CLI и ничего больше: подтверждение = `result` с `aborted_tools` или `aborted_streaming`,
    /// после него CLI выходит с кодом 0 (исполнение заканчивается `stopped(user_stop)`).
    pub async fn interrupt(
        &mut self,
        cli: &mut CliProcess,
        deadline: Duration,
    ) -> InterruptOutcome {
        if !self.queue.interrupt_sent() {
            return InterruptOutcome::NotApplicable;
        }
        if cli.signal(Signal::Int).is_err() {
            return InterruptOutcome::Undetermined;
        }
        let limit = Instant::now() + deadline;
        loop {
            if self.queue.interrupt_acknowledged() {
                return InterruptOutcome::Acknowledged;
            }
            if self.eof {
                return InterruptOutcome::Undetermined;
            }
            if tokio::time::timeout_at(limit, self.pump(cli))
                .await
                .is_err()
            {
                return InterruptOutcome::Undetermined;
            }
        }
    }
}
