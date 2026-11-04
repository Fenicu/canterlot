//! Адаптер Antigravity: `agy acp` (JSON-RPC 2.0 в stdio) на исполнение.
//! Задача 1: запуск, resume и каталог; задача 2: доставка, steer, логический ход,
//! события, прерывание и сжатие.

pub mod catalog;
pub mod events;
pub mod input;
pub mod limits;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod requests;
pub mod rpc;
pub mod session;
pub mod version;

pub use self::catalog::AntigravityCatalog;

use self::{
    events::{QuestionCall, Translator},
    input::{Effect, PromptKind, Target, Tracker},
    process::{PrepareError, Process, StdoutLine},
    requests::{Answer, PendingKind, Requests},
    rpc::{Incoming, RpcClient, RpcError, RpcId},
    session::{SessionError, SessionInfo},
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, Capabilities, CloseMode,
        CompactOutcome, DecisionOutcome, DecisionProof, InstructionSpec, InterruptOutcome,
        ModelUsage, Phase, Proof, SettingsChange, StartSpec, Started, SteerOutcome, Support,
        Transmission, TransmitOutcome,
        canterlot_mcp::{Broker, BrokerEvent, ToolSet},
    },
    config::AntigravityConfig,
    containment::Containment,
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};
use uuid::Uuid;

const STEP: Duration = Duration::from_secs(20);
const KILL_WAIT: Duration = Duration::from_secs(2);
const INTERRUPT_WAIT: Duration = Duration::from_secs(5);
const TURN_STALL: Duration = Duration::from_secs(60);
const ERROR_TEXT_BYTES: usize = 512;

/// Вопросы отвечаются обычным prompt внутри логического хода (agy 1.5.0).
const QUESTIONS_CAPABILITY: Support = Support::Supported;

/// Квота из JSON-RPC ошибки prompt: форма §11 не подтверждена живым кадром, до неё
/// `Limit(Reached)` приходит только из `/usage` (§8). Смена значения — единственное,
/// что нужно после живой проверки.
const QUOTA_FROM_PROMPT_ERROR: bool = false;

fn questions_enabled_by_default() -> bool {
    !matches!(QUESTIONS_CAPABILITY, Support::Unavailable(_))
}

pub struct AntigravityAdapter {
    cfg: AntigravityConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    ready_deadline: Duration,
    stop_grace: Duration,
    interrupt_deadline: Duration,
    turn_stall_deadline: Duration,
    proc: Option<Process>,
    /// Копия, выбранная при первом запуске исполнения; перезапуск не ищет новую.
    pinned: Option<process::Pinned>,
    rpc: RpcClient,
    events: VecDeque<AdapterEvent>,
    session: Option<SessionInfo>,
    instructions: InstructionSpec,
    workdir: PathBuf,
    native_conversation_id: String,
    translator: Option<Translator>,
    tracker: Tracker,
    requests: Requests,
    responses: HashMap<i64, Result<Value, RpcError>>,
    /// MCP canterlot: сокет сессии и stdio-relay, который запускает сам CLI.
    broker: Option<Broker>,
    relay_executable: Option<PathBuf>,
    mcp_servers: Value,
    limits_calls: HashMap<Uuid, String>,
    current_model: String,
    current_effort: String,
    permission_mode: String,
    spec: Option<StartSpec>,
    generation: u64,
    questions_enabled: bool,
    limit_reported: bool,
    /// Срок, к которому отменённый prompt должен ответить, иначе — сторож хода.
    cancel_at: Option<Instant>,
    stall_at: Option<Instant>,
    eof: bool,
    exited: bool,
    exit_reported: bool,
    start_attempted: bool,
}

impl AntigravityAdapter {
    pub fn new(cfg: AntigravityConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            ready_deadline: STEP,
            stop_grace: Duration::from_secs(5),
            interrupt_deadline: INTERRUPT_WAIT,
            turn_stall_deadline: TURN_STALL,
            proc: None,
            pinned: None,
            rpc: RpcClient::new(),
            events: VecDeque::new(),
            session: None,
            instructions: InstructionSpec {
                mode: crate::adapter::InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            workdir: PathBuf::new(),
            native_conversation_id: String::new(),
            translator: None,
            tracker: Tracker::new(),
            requests: Requests::new(0),
            responses: HashMap::new(),
            broker: None,
            relay_executable: None,
            mcp_servers: json!([]),
            limits_calls: HashMap::new(),
            current_model: String::new(),
            current_effort: String::new(),
            permission_mode: String::new(),
            spec: None,
            generation: 0,
            questions_enabled: questions_enabled_by_default(),
            limit_reported: false,
            cancel_at: None,
            stall_at: None,
            eof: false,
            exited: false,
            exit_reported: false,
            start_attempted: false,
        }
    }

    pub fn with_ready_deadline(mut self, ready_deadline: Duration) -> Self {
        self.ready_deadline = ready_deadline;
        self
    }

    /// Срок ожидания `cancelled` после `session/cancel` (спека: 5 с).
    pub fn with_interrupt_deadline(mut self, deadline: Duration) -> Self {
        self.interrupt_deadline = deadline;
        self
    }

    /// Бинарник relay `canterlot-worker mcp-relay`; по умолчанию текущий процесс.
    pub fn with_relay_executable(mut self, executable: PathBuf) -> Self {
        self.relay_executable = Some(executable);
        self
    }

    /// Сколько ждать ответа после `Undetermined`, прежде чем закрыть процесс силой.
    pub fn with_turn_stall_deadline(mut self, deadline: Duration) -> Self {
        self.turn_stall_deadline = deadline;
        self
    }

    /// Тестовый тумблер пути вопросов; на 1.4.12 старт всё равно его выключает.
    pub fn with_questions_enabled(mut self, enabled: bool) -> Self {
        self.questions_enabled = enabled;
        self
    }

    fn diagnostic(&mut self, kind: &'static str, required: bool) {
        self.events.push_back(AdapterEvent::Diagnostic {
            kind,
            event_type: kind.into(),
            required,
            field_names: Vec::new(),
        });
    }

    fn has_diagnostic(&self, kind: &str) -> bool {
        self.events.iter().any(|event| {
            matches!(event, AdapterEvent::Diagnostic { kind: event_kind, .. } if *event_kind == kind)
        })
    }

    fn session_id(&self) -> String {
        self.session
            .as_ref()
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| self.native_conversation_id.clone())
    }

    async fn abandon(&mut self, group: &dyn Containment) {
        if let Some(mut broker) = self.broker.take() {
            broker.shutdown().await;
        }
        let Some(mut proc) = self.proc.take() else {
            return;
        };
        proc.close_stdin();
        let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
        let _ = timeout(KILL_WAIT, proc.wait()).await;
        self.exited = true;
    }

    /// Сокет MCP сессии и параметры relay для `session/new`/`session/resume`.
    async fn bind_broker(&mut self) -> Result<(), AdapterError> {
        let broker = Broker::bind_with(&self.exec_dir, ToolSet::LimitsOnly, "antigravity")
            .await
            .map_err(|_| AdapterError::StartFailed("mcp_bind_failed"))?;
        let relay = self
            .relay_executable
            .clone()
            .or_else(|| std::env::current_exe().ok())
            .ok_or(AdapterError::StartFailed("relay_unavailable"))?;
        self.mcp_servers = session::mcp_servers(&relay, &self.exec_dir, broker.relay_secret());
        broker.arm();
        self.broker = Some(broker);
        Ok(())
    }

    fn session_error(error: SessionError) -> AdapterError {
        AdapterError::StartFailed(match error {
            SessionError::Rpc(_) => "acp_request_rejected",
            SessionError::Eof => "process_exited",
            SessionError::Timeout => "acp_timeout",
            SessionError::Protocol => "invalid_response",
            SessionError::Io(_) => "cli_io_failed",
        })
    }

    async fn handshake(&mut self, spec: &StartSpec) -> Result<SessionInfo, AdapterError> {
        let timeout = self.ready_deadline.max(STEP);
        let proc = self
            .proc
            .as_mut()
            .ok_or(AdapterError::StartFailed("cli_not_started"))?;
        let initialized = session::initialize(proc, &mut self.rpc, timeout)
            .await
            .map_err(Self::session_error)?;
        let agent_version = initialized["agentInfo"]["version"]
            .as_str()
            .unwrap_or_default();
        if !version::is_verified(agent_version) && !self.has_diagnostic("untested_cli_version") {
            self.diagnostic("untested_cli_version", false);
        }
        let proc = self.proc.as_mut().expect("process checked above");
        let info = match &spec.native_conversation_id {
            Some(native_id) => {
                let found = session::contains_session(proc, &mut self.rpc, native_id, timeout)
                    .await
                    .map_err(Self::session_error)?;
                if !found {
                    return Err(AdapterError::StartFailed("conversation not found"));
                }
                session::resume(
                    proc,
                    &mut self.rpc,
                    native_id,
                    &self.instructions,
                    &self.mcp_servers,
                    timeout,
                )
                .await
                .map_err(Self::session_error)?
            }
            None => session::new_session(
                proc,
                &mut self.rpc,
                &self.workdir,
                &self.instructions,
                &self.mcp_servers,
                timeout,
            )
            .await
            .map_err(Self::session_error)?,
        };
        if !matches!(spec.model.as_str(), "" | "default") {
            session::set_model(proc, &mut self.rpc, &info.session_id, &spec.model, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("model_rejected"))?;
        }
        if !spec.effort.is_empty() {
            session::set_effort(proc, &mut self.rpc, &info.session_id, &spec.effort, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("effort_rejected"))?;
        }
        Ok(info)
    }

    pub fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }

    // --- чтение потока ------------------------------------------------------

    async fn write_message(&mut self, message: Value) -> bool {
        let deadline = Instant::now() + self.deadline;
        self.write_message_until(message, deadline).await
    }

    /// Запись в срок операции. Таймаут означает, что CLI перестал читать stdin:
    /// половина строки уже в канале, поэтому соединение закрывается, а supervisor
    /// получает `ProcessExited`, как при любом выходе процесса.
    async fn write_message_until(&mut self, message: Value, deadline: Instant) -> bool {
        let written = match self.proc.as_mut() {
            Some(proc) => proc.write_message_until(&message, deadline).await,
            None => return false,
        };
        match written {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                self.retire_unwritable().await;
                false
            }
            Err(_) => false,
        }
    }

    async fn retire_unwritable(&mut self) {
        let status = match self.proc.take() {
            Some(mut proc) => {
                proc.kill_now();
                timeout(KILL_WAIT, proc.wait())
                    .await
                    .ok()
                    .and_then(Result::ok)
            }
            None => None,
        };
        self.finish_process(status);
    }

    /// Терминальный исход процесса: общие события для EOF, стража хода и записи.
    fn finish_process(&mut self, status: Option<std::process::ExitStatus>) {
        use std::os::unix::process::ExitStatusExt;
        self.exited = true;
        self.exit_reported = true;
        self.cancel_at = None;
        self.stall_at = None;
        self.requests.drop_open();
        if let Some(Effect::CompactDone { ok }) = self.tracker.on_eof() {
            self.events.push_back(AdapterEvent::Compacted { ok });
        }
        let (code, signal) = status.map_or((None, None), |status| (status.code(), status.signal()));
        self.events
            .push_back(AdapterEvent::ProcessExited { code, signal });
    }

    async fn pump_once(&mut self) -> bool {
        if self.exit_reported || self.eof {
            return false;
        }
        if self.proc.is_none() {
            self.exit_reported = true;
            return false;
        }
        enum Pumped {
            Line(StdoutLine),
            Broker(BrokerEvent),
        }
        // Вызов MCP не ждёт кадра stdout: иначе ответ владельца застрял бы до конца хода.
        let pumped = {
            let proc = self.proc.as_mut().expect("process checked above");
            match self.broker.as_mut() {
                Some(broker) => tokio::select! {
                    line = proc.next_line() => Pumped::Line(line),
                    event = broker.next() => Pumped::Broker(event),
                },
                None => Pumped::Line(proc.next_line().await),
            }
        };
        match pumped {
            Pumped::Line(line) => self.dispatch_line(line).await,
            Pumped::Broker(event) => {
                self.on_broker(event);
                true
            }
        }
    }

    fn on_broker(&mut self, event: BrokerEvent) {
        match event {
            BrokerEvent::AgentTool {
                call_id,
                tool,
                arguments,
                ..
            } => self.events.push_back(AdapterEvent::AgentCall {
                call_id,
                tool: tool.to_owned(),
                arguments,
            }),
            BrokerEvent::ShareFile { call_id, arguments } => {
                self.events
                    .push_back(AdapterEvent::ShareFile { call_id, arguments });
            }
            BrokerEvent::LimitsQuery { call_id } => {
                self.limits_calls
                    .insert(call_id, "no limit data yet in this session".to_owned());
                self.events.push_back(AdapterEvent::Limits { call_id });
            }
            _ => {}
        }
    }

    async fn dispatch_line(&mut self, line: StdoutLine) -> bool {
        match line {
            StdoutLine::Line(text) => match rpc::classify(&text) {
                Incoming::Notification { method, params } => {
                    if method == "session/update" {
                        self.handle_update(&params).await;
                    }
                    true
                }
                Incoming::ServerRequest { id, method, params } => {
                    self.handle_server_request(&id, &method, &params).await;
                    true
                }
                Incoming::Response { id, result } => {
                    self.handle_response(id, result);
                    true
                }
                Incoming::Invalid => {
                    self.diagnostic("invalid_frame", true);
                    true
                }
            },
            StdoutLine::Oversized(_) => {
                self.diagnostic("oversized_frame", true);
                true
            }
            StdoutLine::Eof => {
                self.handle_eof().await;
                false
            }
        }
    }

    async fn handle_update(&mut self, params: &Value) {
        let update = &params["update"];
        if !update.is_object() {
            self.diagnostic("malformed_session_update", true);
            return;
        }
        if update["sessionUpdate"] == "user_message_chunk"
            && let Some(text) = update["content"]["text"].as_str()
            && let Some(effect) = self.tracker.on_echo(text)
            && let Effect::LateProof(command_id) = effect
        {
            self.events
                .push_back(AdapterEvent::InputAccepted { command_id });
        }
        if let Some(tool_call_id) = update.get("toolCallId").and_then(Value::as_str) {
            let status = update.get("status").and_then(Value::as_str);
            self.requests.note_tool_frame(tool_call_id, status);
        }
        let fed = match self.translator.as_mut() {
            Some(translator) => translator.feed(update),
            None => return,
        };
        let events::Fed { events, question } = fed;
        // Текст модели не признак квоты: обычный ответ может обсуждать лимиты.
        // `Limit(Reached)` приходит из `/usage` и, после живой проверки, из ошибки prompt.
        self.events.extend(events);
        if let Some(question) = question {
            self.open_question(question).await;
        }
    }

    /// Единый путь отмены: срок ответа тот же, что у `interrupt`; если prompt
    /// не ответил, `next_event` закрывает процесс сторожем хода.
    async fn cancel_with_watch(&mut self) {
        let notification = input::cancel_notification(&self.session_id());
        let sent = self.write_message(notification).await;
        if sent && !self.eof && !self.exited {
            self.cancel_at = Some(Instant::now() + self.interrupt_deadline);
        }
    }

    async fn handle_server_request(&mut self, id: &RpcId, method: &str, params: &Value) {
        if method == "session/request_permission" {
            match self
                .requests
                .open_permission(id.clone(), params, "orchestrator")
            {
                Ok((request_id, payload)) => {
                    if let Some(translator) = self.translator.as_mut() {
                        translator.permission_open = true;
                    }
                    self.events.push_back(AdapterEvent::RequestOpened {
                        request_id,
                        kind: "permission".into(),
                        payload,
                    });
                    self.events.push_back(AdapterEvent::Phase {
                        phase: Phase::AwaitingPermission,
                        tool_name: None,
                        summary: None,
                    });
                }
                Err(_) => {
                    // Без toolCallId карточки нет: отказ, чтобы ход не завис.
                    self.diagnostic("antigravity_permission_invalid", false);
                    let _ = self.write_message(permission_reply(id, "deny")).await;
                }
            }
        } else {
            self.diagnostic("unsupported_server_request", false);
            let mut reply = RpcClient::error(id, -32601, "Method not found");
            reply["jsonrpc"] = json!("2.0");
            let _ = self.write_message(reply).await;
        }
    }

    async fn open_question(&mut self, call: QuestionCall) {
        if !self.questions_enabled {
            // До живой проверки cancel + служебный prompt вопрос не показывается.
            self.diagnostic("antigravity_questions_unavailable", false);
            self.cancel_with_watch().await;
            return;
        }
        match self
            .requests
            .open_question(&call.tool_call_id, &call.raw_input, "orchestrator")
        {
            Ok((request_id, payload)) => {
                // 1.5.0 отвечает `end_turn` одновременно с вопросом. Ход держим
                // открытым, пока карточка не закрыта ответом или отказом.
                self.tracker.expect_continuation(true);
                self.events.push_back(AdapterEvent::RequestOpened {
                    request_id,
                    kind: "question".into(),
                    payload,
                });
                self.events.push_back(AdapterEvent::Phase {
                    phase: Phase::AwaitingAnswer,
                    tool_name: None,
                    summary: None,
                });
            }
            Err(_) => {
                // Пустой или неразбираемый questions[]: карточка не открывается,
                // ход прерывается, чтобы не висеть без stopReason.
                self.diagnostic("antigravity_question_invalid", false);
                self.cancel_with_watch().await;
            }
        }
    }

    fn handle_response(&mut self, id: RpcId, result: Result<Value, RpcError>) {
        let RpcId::Int(id) = id else {
            return;
        };
        let _ = self.rpc.resolve(&RpcId::Int(id));
        if !self.tracker.is_prompt(id) {
            self.responses.insert(id, result);
            return;
        }
        let stalled = self.stall_at.is_some();
        match self.tracker.on_response(id, &result) {
            Some(Effect::TurnClosed {
                command_id,
                usage,
                failed,
                stop: _,
            }) => self.close_turn(command_id, usage, failed),
            Some(Effect::CompactDone { ok }) => {
                self.close_blocks();
                self.events.push_back(AdapterEvent::Compacted { ok });
            }
            _ => {}
        }
        // Сторож отмены снимается только когда ответили все prompt-ы хода:
        // ответ одного RPC не отвечает за молчащий второй.
        if self.cancel_at.is_some() && self.tracker.outstanding() == 0 {
            self.cancel_at = None;
        }
        if stalled && self.tracker.target().is_none() {
            self.stall_at = None;
        }
    }

    fn close_blocks(&mut self) {
        if let Some(translator) = self.translator.as_mut() {
            let mut events = Vec::new();
            translator.end_turn(&mut events);
            translator.begin_turn(None);
            self.events.extend(events);
        }
    }

    fn close_turn(&mut self, command_id: Uuid, usage: input::Usage, failed: Option<String>) {
        self.close_blocks();
        self.close_open_requests("turn_ended");
        if let Some(message) = &failed {
            if QUOTA_FROM_PROMPT_ERROR
                && !self.limit_reported
                && let Some(fact) = limits::reached_from_text(message, crate::machine::now_ms())
            {
                self.limit_reported = true;
                self.events.push_back(AdapterEvent::Limit(fact));
            }
            self.error_block(message.clone(), Some(command_id));
        }
        let context_tokens = usage.last_input;
        let usage: Vec<ModelUsage> = usage.model_usage(&self.current_model).into_iter().collect();
        self.events.push_back(AdapterEvent::RunFinished {
            input_command_id: command_id,
            usage,
            context_tokens,
            context_window_tokens: limits::context_window(&self.current_model),
        });
        self.events.push_back(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
    }

    fn close_open_requests(&mut self, reason: &str) {
        for request_id in self.requests.expire() {
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id,
                reason: reason.into(),
            });
        }
        if let Some(translator) = self.translator.as_mut() {
            translator.permission_open = false;
        }
    }

    fn error_block(&mut self, text: String, command_id: Option<Uuid>) {
        let block_id = Uuid::now_v7();
        let text: String = text.chars().take(ERROR_TEXT_BYTES).collect();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::Error,
            input_command_id: command_id,
            parent: None,
            agent: "orchestrator".into(),
            model: self.current_model.clone(),
            tool_name: None,
            summary: None,
            approved_by: None,
        });
        self.events.push_back(AdapterEvent::BlockFinal {
            block_id,
            text,
            summary: None,
        });
    }

    async fn handle_eof(&mut self) {
        self.eof = true;
        let status = match self.proc.as_mut() {
            Some(proc) => proc.wait().await.ok(),
            None => None,
        };
        self.finish_process(status);
    }

    /// Отмена не дождалась ответа prompt до срока: включается тот же сторож,
    /// что у `interrupt`, и `next_event` закрывает процесс.
    fn check_cancel_watch(&mut self) {
        let Some(at) = self.cancel_at else {
            return;
        };
        if self.tracker.outstanding() == 0 {
            self.cancel_at = None;
            return;
        }
        if Instant::now() >= at {
            self.cancel_at = None;
            self.stall_at = Some(Instant::now() + self.turn_stall_deadline);
        }
    }

    async fn check_stall(&mut self) {
        self.check_cancel_watch();
        if !self.stall_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        self.stall_at = None;
        let status = match self.proc.take() {
            Some(mut proc) => {
                proc.kill_now();
                timeout(KILL_WAIT, proc.wait())
                    .await
                    .ok()
                    .and_then(Result::ok)
            }
            None => None,
        };
        self.finish_process(status);
    }

    // --- настройки хода -----------------------------------------------------

    async fn settings_call(
        &mut self,
        method: &'static str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let (id, message) = self.rpc.request_v2(method, params);
        let limit = Instant::now() + self.deadline;
        if !self.write_message_until(message, limit).await {
            self.rpc.forget(id);
            return Err(RpcError {
                code: -32000,
                message: "process exited".into(),
                data: None,
            });
        }
        loop {
            if let Some(result) = self.responses.remove(&id) {
                return result;
            }
            if self.eof || self.exited {
                return Err(RpcError {
                    code: -32000,
                    message: "process exited".into(),
                    data: None,
                });
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                self.rpc.forget(id);
                return Err(RpcError {
                    code: -32000,
                    message: "timeout".into(),
                    data: None,
                });
            }
        }
    }

    async fn apply_settings(
        &mut self,
        change: Option<&crate::adapter::SettingsChange>,
    ) -> Result<(), String> {
        let Some(change) = change else {
            return Ok(());
        };
        if let Some(model) = change.model.as_deref()
            && !matches!(model, "" | "default")
            && model != self.current_model
        {
            let params = json!({"sessionId": self.session_id(), "modelId": model});
            self.settings_call("session/set_model", params)
                .await
                .map_err(|_| "model_rejected".to_string())?;
            self.current_model = model.to_owned();
            if let Some(translator) = self.translator.as_mut() {
                translator.set_model(model);
            }
        }
        if let Some(effort) = change.effort.as_deref()
            && !effort.is_empty()
            && effort != self.current_effort
        {
            let params = json!({
                "sessionId": self.session_id(),
                "configId": "thought_level",
                "value": effort,
            });
            self.settings_call("session/set_config_option", params)
                .await
                .map_err(|_| "effort_rejected".to_string())?;
            self.current_effort = effort.to_owned();
        }
        Ok(())
    }

    async fn restart_for_settings(
        &mut self,
        change: &SettingsChange,
        group: &dyn Containment,
    ) -> Result<(), &'static str> {
        let Some(mut spec) = self.spec.clone() else {
            return Err("settings_unavailable");
        };
        let native = self.session_id();
        let previous = (
            self.permission_mode.clone(),
            self.instructions.clone(),
            self.current_model.clone(),
            self.current_effort.clone(),
        );
        let next_mode = change
            .permission_mode
            .clone()
            .unwrap_or_else(|| previous.0.clone());
        if process::launch_args(&self.exec_dir, &next_mode).is_err() {
            return Err("settings_unavailable");
        }
        let next_instructions = change
            .instructions
            .clone()
            .unwrap_or_else(|| previous.1.clone());
        let next_model = change.model.clone().unwrap_or_else(|| previous.2.clone());
        let next_effort = change.effort.clone().unwrap_or_else(|| previous.3.clone());
        self.stop_for_restart().await;
        // Дети ушедшего CLI (инструменты) не должны пережить замену: второй CLI
        // рядом с живыми инструментами не запускается.
        if !matches!(
            group.terminate(self.stop_grace).await,
            Ok(crate::containment::Termination::Empty)
        ) {
            tracing::warn!(
                kind = "antigravity_restart_descendants",
                "the retiring CLI left live descendants; the execution stops"
            );
            if let Some(mut broker) = self.broker.take() {
                broker.shutdown().await;
            }
            self.reset_connection();
            self.diagnostic("restart_failed", true);
            return Err("settings_restart_failed");
        }
        self.reset_connection();
        // Новый процесс получает новый relay со своим секретом.
        if let Some(mut broker) = self.broker.take() {
            broker.shutdown().await;
        }
        if self.bind_broker().await.is_err() {
            self.diagnostic("restart_failed", true);
            return Err("settings_restart_failed");
        }
        self.instructions = next_instructions.clone();
        self.permission_mode = next_mode.clone();
        match self
            .launch_resumed(
                &native,
                &next_mode,
                &next_instructions,
                &next_model,
                &next_effort,
                group,
            )
            .await
        {
            Ok(()) => {
                spec.native_conversation_id = Some(native);
                spec.permission_mode = next_mode;
                spec.instructions = next_instructions;
                spec.model = next_model;
                spec.effort = next_effort;
                self.spec = Some(spec);
                let proc = self.proc.as_ref().expect("restart spawned a process");
                self.events.push_back(AdapterEvent::CliRestarted {
                    cli_pid: proc.pid,
                    cli_start_ticks: proc.start_ticks,
                });
                Ok(())
            }
            Err(error) => {
                // Прежние настройки не подставляются: после переключения в `ask`
                // молчаливый откат к `bypass` оставил бы владельца без запросов
                // разрешений. Исполнение останавливается с причиной запуска.
                tracing::warn!(
                    kind = "antigravity_restart_failed",
                    reason = ?error,
                    "antigravity restart failed; the execution stops"
                );
                self.stop_for_restart().await;
                self.reset_connection();
                self.diagnostic("restart_failed", true);
                Err("settings_restart_failed")
            }
        }
    }

    async fn stop_for_restart(&mut self) {
        let Some(mut proc) = self.proc.take() else {
            return;
        };
        proc.close_stdin();
        if timeout(self.stop_grace, proc.wait()).await.is_err() {
            proc.kill_now();
            let _ = timeout(KILL_WAIT, proc.wait()).await;
        }
    }

    fn reset_connection(&mut self) {
        self.rpc = RpcClient::new();
        self.responses.clear();
        self.requests = Requests::new(self.generation);
        self.translator = None;
        self.eof = false;
        self.exited = false;
        self.exit_reported = false;
        self.cancel_at = None;
        self.stall_at = None;
        self.limit_reported = false;
    }

    async fn launch_resumed(
        &mut self,
        native: &str,
        mode: &str,
        instructions: &InstructionSpec,
        model: &str,
        effort: &str,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        let executable = self
            .pinned
            .as_ref()
            .filter(|pinned| pinned.executable.is_file())
            .map(|pinned| pinned.executable.clone())
            .ok_or(AdapterError::StartFailed("cli_not_found"))?;
        self.proc = Some(
            Process::spawn(&executable, &self.exec_dir, &self.workdir, mode, group)
                .map_err(|_| AdapterError::StartFailed("cli_launch_failed"))?,
        );
        let timeout = self.ready_deadline.max(STEP);
        let proc = self.proc.as_mut().expect("spawned above");
        session::initialize(proc, &mut self.rpc, timeout)
            .await
            .map_err(Self::session_error)?;
        let proc = self.proc.as_mut().expect("process checked above");
        let found = session::contains_session(proc, &mut self.rpc, native, timeout)
            .await
            .map_err(Self::session_error)?;
        if !found {
            return Err(AdapterError::StartFailed("conversation not found"));
        }
        let proc = self.proc.as_mut().expect("process checked above");
        let info = session::resume(
            proc,
            &mut self.rpc,
            native,
            instructions,
            &self.mcp_servers,
            timeout,
        )
        .await
        .map_err(Self::session_error)?;
        if !matches!(model, "" | "default") {
            let proc = self.proc.as_mut().expect("process checked above");
            session::set_model(proc, &mut self.rpc, &info.session_id, model, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("model_rejected"))?;
        }
        if !effort.is_empty() {
            let proc = self.proc.as_mut().expect("process checked above");
            session::set_effort(proc, &mut self.rpc, &info.session_id, effort, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("effort_rejected"))?;
        }
        self.native_conversation_id = native.to_owned();
        self.current_model = info
            .current_model
            .clone()
            .filter(|_| matches!(model, "" | "default"))
            .unwrap_or_else(|| model.to_owned());
        self.current_effort = if effort.is_empty() {
            info.current_effort.clone().unwrap_or_default()
        } else {
            effort.to_owned()
        };
        self.translator = Some(Translator::new(&self.current_model));
        self.session = Some(info);
        Ok(())
    }

    /// Довести неизвестное сжатие до терминального исхода: отмена и ожидание ответа
    /// в срок `interrupt`, иначе процесс закрывается (`ProcessExited`).
    async fn settle_compaction(&mut self) {
        if !self.tracker.has_compaction() {
            return;
        }
        self.cancel_with_watch().await;
        let limit = self
            .cancel_at
            .unwrap_or_else(|| Instant::now() + self.interrupt_deadline);
        loop {
            if self.eof || self.exited {
                return;
            }
            if !self.tracker.has_compaction() {
                return;
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                let status = match self.proc.take() {
                    Some(mut proc) => {
                        proc.kill_now();
                        timeout(KILL_WAIT, proc.wait())
                            .await
                            .ok()
                            .and_then(Result::ok)
                    }
                    None => None,
                };
                self.finish_process(status);
                return;
            }
        }
    }

    // --- доказательство приёма ---------------------------------------------

    async fn wait_echo(&mut self, command_id: Uuid, limit: Instant) -> EchoEnd {
        loop {
            if self.tracker.take_proved(command_id) {
                self.tracker.stop_waiting();
                return EchoEnd::Accepted;
            }
            if let Some(message) = self.tracker.early_rejection(command_id) {
                self.tracker.stop_waiting();
                return EchoEnd::Rejected(message);
            }
            if self.tracker.take_answered_unproven(command_id) {
                self.tracker.stop_waiting();
                return EchoEnd::Undetermined;
            }
            if self.eof || self.exited {
                self.tracker.stop_waiting();
                return EchoEnd::Undetermined;
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                self.tracker.stop_waiting();
                self.tracker.mark_unconfirmed(command_id);
                return EchoEnd::Undetermined;
            }
        }
    }
}

fn permission_reply(id: &RpcId, option: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.to_value(),
        "result": {"outcome": {"outcome": "selected", "optionId": option}},
    })
}

fn permission_applied(option: &str) -> DecisionOutcome {
    DecisionOutcome::Applied(DecisionProof {
        proof_kind: "agy_acp_permission_frame".into(),
        payload: json!({"option": option}),
    })
}

fn deny_decision(message: &str) -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Deny,
        message: Some(message.to_owned()),
    }
}

enum EchoEnd {
    Accepted,
    Rejected(String),
    Undetermined,
}

#[async_trait::async_trait]
impl Adapter for AntigravityAdapter {
    /// MCP сессии задаёт только ACP-клиент: в `session/new` уходит единственный
    /// сервер canterlot. Список и настройки MCP владельца адаптер не читает и
    /// не передаёт; перекрытие глобального списка `agy` — открытая проба P16
    /// (`docs/stage0/antigravity.md`).
    fn isolates_mcp(&self) -> bool {
        true
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Supported,
            questions: if self.questions_enabled {
                QUESTIONS_CAPABILITY.clone()
            } else {
                Support::Unavailable("agy_1_4_12")
            },
            background_work: Support::Unavailable("antigravity: P8"),
            interrupt_keeps_process: true,
        }
    }

    /// `/compact` идёт обычным prompt: итог чужого хода приписался бы вводу.
    fn holds_input_during_compaction(&self) -> bool {
        true
    }

    async fn start(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        if self.start_attempted {
            return Err(AdapterError::AlreadyStarted);
        }
        self.start_attempted = true;
        if !self.cfg.enabled {
            return Err(AdapterError::StartFailed("harness_disabled"));
        }
        let pinned = match process::prepare_pinned(&self.cfg).await {
            Ok(pinned) => pinned,
            Err(PrepareError::Version(version::VersionError::NotFound)) => {
                return Err(AdapterError::StartFailed("cli_not_found"));
            }
            Err(PrepareError::Version(version::VersionError::BelowMinimum(_))) => {
                return Err(AdapterError::StartFailed("cli_too_old"));
            }
            Err(PrepareError::Version(_)) => {
                return Err(AdapterError::StartFailed("cli_version_failed"));
            }
            Err(PrepareError::MissingBinary) => {
                return Err(AdapterError::StartFailed("cli_not_found"));
            }
            Err(PrepareError::MissingDataDir) => {
                return Err(AdapterError::StartFailed("data_dir_missing"));
            }
            Err(PrepareError::Io(_)) => {
                return Err(AdapterError::StartFailed("cli_pin_failed"));
            }
        };
        // Делегированная сессия не должна видеть MCP-серверы владельца. Пока P16
        // не закрыт, единственное доказуемое состояние — пустой глобальный список:
        // его отдаёт сам CLI, файлы `~/.gemini` адаптер не открывает.
        if spec.isolate_mcp {
            let data_dir = self
                .cfg
                .data_dir
                .as_deref()
                .ok_or(AdapterError::StartFailed("data_dir_missing"))?;
            match process::owner_mcp(&pinned.executable, data_dir).await {
                Ok(process::OwnerMcp::Empty) => {}
                Ok(process::OwnerMcp::Configured) => {
                    return Err(AdapterError::StartFailed("owner_mcp_not_isolated"));
                }
                Err(_) => return Err(AdapterError::StartFailed("mcp_list_failed")),
            }
        }
        // Версия и копия фиксируются на всё исполнение: перезапуск настроек
        // не должен подхватить обновившийся или откатившийся системный `agy`.
        self.pinned = Some(pinned.clone());
        if let Some(previous) = &pinned.previous {
            tracing::warn!(
                kind = "antigravity_cli_version_changed",
                previous,
                current = pinned.version.label(),
                "the configured agy changed; a new pinned copy is used"
            );
            self.diagnostic("antigravity_cli_version_changed", false);
        }
        if matches!(pinned.version, version::CliVersion::UntestedNewer(_)) {
            self.diagnostic("untested_cli_version", false);
        }
        // На 1.4.12 ответ на вопрос через cancel + prompt ломает сессию:
        // карточка не показывается, `open_question` отменяет ход.
        if !version::supports_questions(&pinned.version) {
            self.questions_enabled = false;
        }
        self.tracker
            .set_thoughts_in_output(version::thoughts_in_output(&pinned.version));
        self.instructions = spec.instructions.clone();
        self.workdir = spec.workdir.clone();
        self.permission_mode = spec.permission_mode.clone();
        self.generation = spec.generation;
        self.requests = Requests::new(spec.generation);
        self.spec = Some(spec.clone());
        self.limit_reported = false;
        // Аргументы проверяются до spawn: неверный режим не оставляет процесс.
        if process::launch_args(&self.exec_dir, &spec.permission_mode).is_err() {
            return Err(AdapterError::StartFailed("unknown_permission_mode"));
        }
        // Сокет и relay — до CLI: `session/new` уже перечисляет canterlot.
        self.bind_broker().await?;
        self.proc = Some(
            Process::spawn(
                &pinned.executable,
                &self.exec_dir,
                &spec.workdir,
                &spec.permission_mode,
                group,
            )
            .map_err(|error| {
                AdapterError::StartFailed(match error {
                    process::LaunchError::UnknownPermissionMode => "unknown_permission_mode",
                    process::LaunchError::WorkdirNotDir => "workdir_not_dir",
                    process::LaunchError::CliNotFoundInContainment
                    | process::LaunchError::SpawnFailed(_) => "cli_launch_failed",
                    process::LaunchError::DataDirMissing
                    | process::LaunchError::SourceNotFound
                    | process::LaunchError::Io(_) => "cli_launch_failed",
                })
            })?,
        );
        match self.handshake(spec).await {
            Ok(info) => {
                self.native_conversation_id = info.session_id.clone();
                self.current_model = if !matches!(spec.model.as_str(), "" | "default") {
                    spec.model.clone()
                } else {
                    info.current_model.clone().unwrap_or_default()
                };
                self.current_effort = if spec.effort.is_empty() {
                    info.current_effort.clone().unwrap_or_default()
                } else {
                    spec.effort.clone()
                };
                self.translator = Some(Translator::new(&self.current_model));
                let proc = self.proc.as_ref().expect("spawned before handshake");
                let started = Started {
                    native_conversation_id: info.session_id.clone(),
                    cli_pid: proc.pid,
                    cli_start_ticks: proc.start_ticks,
                };
                self.session = Some(info);
                Ok(started)
            }
            Err(error) => {
                self.abandon(group).await;
                Err(error)
            }
        }
    }

    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        input::prompt_text(input).map(|_| ())
    }

    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome {
        if self.proc.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        if self.tracker.has_turn() {
            return TransmitOutcome::Rejected("turn_active".into());
        }
        if let Some(change) = input.settings.as_ref()
            && (change
                .permission_mode
                .as_deref()
                .is_some_and(|mode| mode != self.permission_mode)
                || change
                    .instructions
                    .as_ref()
                    .is_some_and(|next| !next.same_effect(&self.instructions)))
            && let Err(reason) = self.restart_for_settings(change, group).await
        {
            return TransmitOutcome::Rejected(reason.into());
        }
        if let Err(reason) = self.apply_settings(input.settings.as_ref()).await {
            return TransmitOutcome::Rejected(reason);
        }
        let text = match input::prompt_text(input) {
            Ok(text) => text,
            Err(reason) => return TransmitOutcome::Rejected(reason.into()),
        };
        let params = input::prompt_params(&self.session_id(), &text);
        let (id, message) = self.rpc.request_v2("session/prompt", params);
        if !self.tracker.begin(input.command_id) {
            self.rpc.forget(id);
            return TransmitOutcome::Rejected("turn_active".into());
        }
        self.tracker
            .register(id, input.command_id, PromptKind::Turn);
        self.tracker.expect_echo(id, input.command_id, &text);
        self.tracker.await_echo(input.command_id);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(Some(input.command_id));
        }
        let limit = Instant::now() + self.deadline;
        if !self.write_message_until(message, limit).await {
            self.tracker.forget_command(input.command_id);
            self.rpc.forget(id);
            return TransmitOutcome::Undetermined;
        }
        match self.wait_echo(input.command_id, limit).await {
            EchoEnd::Accepted => TransmitOutcome::Accepted(Proof {
                command_id: input.command_id,
                kind: input::PROOF_KIND,
            }),
            EchoEnd::Rejected(message) => TransmitOutcome::Rejected(message),
            EchoEnd::Undetermined => TransmitOutcome::Undetermined,
        }
    }

    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        let Some(pending) = self.requests.pending(request_id) else {
            return if self.requests.was_expired(request_id) {
                DecisionOutcome::NotApplied("question expired".into())
            } else {
                DecisionOutcome::NotApplied("request_not_pending".into())
            };
        };
        if pending.answered {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        }
        let answer = match self.requests.answer(request_id, decision) {
            Ok(answer) => answer,
            Err(reason) => return DecisionOutcome::NotApplied(reason.into()),
        };
        if self.eof || self.exited || self.proc.is_none() {
            return DecisionOutcome::Undetermined;
        }
        match answer {
            Answer::Permission { rpc_id, option } => {
                if !self.write_message(permission_reply(&rpc_id, option)).await {
                    return DecisionOutcome::Undetermined;
                }
                if let Some(translator) = self.translator.as_mut() {
                    translator.permission_open = false;
                }
                let limit = Instant::now() + self.deadline;
                loop {
                    if self.requests.take_resolved(request_id) {
                        return permission_applied(option);
                    }
                    if self.eof || self.exited {
                        return DecisionOutcome::Undetermined;
                    }
                    // Ответ prompt закрыл ход: он тоже доказывает, что ответ дошёл.
                    if !self.tracker.has_turn() {
                        return permission_applied(option);
                    }
                    if timeout_at(limit, self.pump_once()).await.is_err() {
                        return DecisionOutcome::Undetermined;
                    }
                }
            }
            Answer::Question { text } => {
                let Some(command_id) = self.tracker.turn_command() else {
                    return DecisionOutcome::NotApplied("question expired".into());
                };
                // 1.5.0: ответ уходит обычным prompt без cancel. Эха у него нет,
                // поэтому приём доказывает следующий кадр `ask_question`.
                self.tracker.expect_continuation(false);
                let params = input::prompt_params(&self.session_id(), &text);
                let (id, message) = self.rpc.request_v2("session/prompt", params);
                self.tracker.register(id, command_id, PromptKind::Turn);
                let limit = Instant::now() + self.deadline;
                if !self.write_message_until(message, limit).await {
                    self.tracker.forget_command(command_id);
                    self.rpc.forget(id);
                    return DecisionOutcome::Undetermined;
                }
                loop {
                    if self.requests.take_resolved(request_id) {
                        return DecisionOutcome::Applied(DecisionProof {
                            proof_kind: "agy_acp_question_tool_frame".into(),
                            payload: json!({"tool_call_id": request_id, "text_bytes": text.len()}),
                        });
                    }
                    if self.eof || self.exited {
                        return DecisionOutcome::Undetermined;
                    }
                    if !self.tracker.has_turn() {
                        return DecisionOutcome::NotApplied("question_not_applied".into());
                    }
                    if timeout_at(limit, self.pump_once()).await.is_err() {
                        return DecisionOutcome::Undetermined;
                    }
                }
            }
        }
    }

    async fn refuse_request(&mut self, request_id: &str, message: &str) {
        let Some(pending) = self.requests.pending(request_id) else {
            return;
        };
        if pending.answered {
            return;
        }
        let kind = pending.kind.clone();
        match kind {
            PendingKind::Permission { .. } => {
                let Ok(Answer::Permission { rpc_id, option }) =
                    self.requests.answer(request_id, &deny_decision(message))
                else {
                    return;
                };
                let _ = self.write_message(permission_reply(&rpc_id, option)).await;
                self.requests.remove(request_id);
            }
            PendingKind::Question { .. } => {
                self.requests.remove(request_id);
                // Отказ вопроса только отменяет живой ход; prompt не отправляется.
                if self.tracker.has_turn() && !self.eof && !self.exited {
                    self.cancel_with_watch().await;
                }
                // 1.5.0 уже ответил `end_turn` на исходный prompt: если других
                // ответов не ждём, удержанный ход закрываем сами.
                self.tracker.expect_continuation(false);
                if let Some(Effect::TurnClosed {
                    command_id,
                    usage,
                    failed,
                    stop: _,
                }) = self.tracker.close_if_idle()
                {
                    self.close_turn(command_id, usage, failed);
                }
            }
        }
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        let Some(target) = self.tracker.target() else {
            return InterruptOutcome::NotApplicable;
        };
        let open = self.requests.open_ids();
        let had_question = open.iter().any(|request_id| {
            matches!(
                self.requests
                    .pending(request_id)
                    .map(|pending| &pending.kind),
                Some(PendingKind::Question { .. })
            )
        });
        for request_id in open {
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id,
                reason: "interrupted".into(),
            });
        }
        self.requests.drop_open();
        if let Some(translator) = self.translator.as_mut() {
            translator.permission_open = false;
        }
        self.cancel_with_watch().await;
        // Вопрос 1.5.0 держит ход после уже пришедшего `end_turn`: cancel его
        // закрывает, а ответа prompt, который закрыл бы ход, не будет.
        self.tracker.expect_continuation(false);
        if let Some(Effect::TurnClosed {
            command_id,
            usage,
            failed,
            stop: _,
        }) = self.tracker.close_if_idle()
        {
            self.cancel_at = None;
            self.close_turn(command_id, usage, failed);
            return InterruptOutcome::Acknowledged;
        }
        let limit = self
            .cancel_at
            .unwrap_or_else(|| Instant::now() + self.interrupt_deadline);
        loop {
            if self.eof || self.exited {
                return InterruptOutcome::Undetermined;
            }
            let done = match target {
                Target::Turn => !self.tracker.has_turn(),
                Target::Compact => !self.tracker.has_compaction(),
            };
            if done {
                self.cancel_at = None;
                return match target {
                    Target::Turn
                        if had_question || self.tracker.last_stop() == Some("cancelled") =>
                    {
                        InterruptOutcome::Acknowledged
                    }
                    Target::Compact if self.tracker.last_compact_ok() == Some(false) => {
                        InterruptOutcome::Acknowledged
                    }
                    _ => InterruptOutcome::NotApplicable,
                };
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                self.cancel_at = None;
                self.stall_at = Some(Instant::now() + self.turn_stall_deadline);
                return InterruptOutcome::Undetermined;
            }
        }
    }

    async fn steer(&mut self, input: &Transmission) -> SteerOutcome {
        if self.tracker.has_compaction() {
            return SteerOutcome::NotApplicable("compaction".into());
        }
        if self
            .translator
            .as_ref()
            .is_some_and(|translator| translator.permission_open)
        {
            return SteerOutcome::NotApplicable("permission_open".into());
        }
        if !self.tracker.has_turn() {
            return SteerOutcome::NotApplicable("no_turn".into());
        }
        if let Err(reason) = self.apply_settings(input.settings.as_ref()).await {
            return SteerOutcome::Rejected(reason);
        }
        let text = match input::prompt_text(input) {
            Ok(text) => text,
            Err(reason) => return SteerOutcome::Rejected(reason.into()),
        };
        let params = input::prompt_params(&self.session_id(), &text);
        let (id, message) = self.rpc.request_v2("session/prompt", params);
        self.tracker
            .register(id, input.command_id, PromptKind::Turn);
        self.tracker.expect_echo(id, input.command_id, &text);
        self.tracker.await_echo(input.command_id);
        let limit = Instant::now() + self.deadline;
        if !self.write_message_until(message, limit).await {
            self.tracker.forget_command(input.command_id);
            self.rpc.forget(id);
            return SteerOutcome::Undetermined;
        }
        match self.wait_echo(input.command_id, limit).await {
            EchoEnd::Accepted => SteerOutcome::Accepted(Proof {
                command_id: input.command_id,
                kind: input::PROOF_KIND,
            }),
            EchoEnd::Rejected(message) => SteerOutcome::Rejected(message),
            EchoEnd::Undetermined => SteerOutcome::Undetermined,
        }
    }

    async fn compact(&mut self, command: Uuid) -> CompactOutcome {
        if self.tracker.has_turn() || self.tracker.has_compaction() {
            return CompactOutcome::NotApplicable;
        }
        if self.proc.is_none() || self.exited || self.eof {
            return CompactOutcome::Failed;
        }
        let params = input::prompt_params(&self.session_id(), input::COMPACT_TEXT);
        let (id, message) = self.rpc.request_v2("session/prompt", params);
        self.tracker.register(id, command, PromptKind::Compact);
        self.tracker.expect_echo(id, command, input::COMPACT_TEXT);
        self.tracker.await_echo(command);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(None);
        }
        let limit = Instant::now() + self.deadline;
        if !self.write_message_until(message, limit).await {
            self.tracker.forget_command(command);
            self.rpc.forget(id);
            return CompactOutcome::Failed;
        }
        match self.wait_echo(command, limit).await {
            EchoEnd::Accepted => CompactOutcome::Accepted,
            EchoEnd::Rejected(_) => {
                self.tracker.forget_command(command);
                CompactOutcome::Failed
            }
            EchoEnd::Undetermined => {
                // Исход неизвестен: ввод нельзя отпускать, пока prompt сжатия в полёте,
                // иначе следующий ход перемешается со сжатием, а поздний ответ потеряется.
                self.settle_compaction().await;
                CompactOutcome::Failed
            }
        }
    }

    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        // Итог вызова возвращается запросившему relay; чужой итог брокер отбросит.
        let result = crate::adapter::limits_fallback(&mut self.limits_calls, call_id, result);
        if let Some(broker) = self.broker.as_mut() {
            broker.answer_agent_tool(call_id, &result).await;
        }
    }

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            self.check_stall().await;
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            if self.exit_reported {
                return std::future::pending().await;
            }
            // Будимся по ближайшему сроку: отмена ждёт ответа, затем сторож хода
            // получает свой срок в `check_cancel_watch`.
            let deadline = self.stall_at.into_iter().chain(self.cancel_at).min();
            match deadline {
                Some(at) => {
                    let _ = timeout_at(at, self.pump_once()).await;
                }
                None => {
                    self.pump_once().await;
                }
            }
        }
    }

    fn set_stop_grace(&mut self, grace: Duration) {
        self.stop_grace = grace;
    }

    async fn close(
        &mut self,
        mode: CloseMode,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        self.cancel_at = None;
        self.stall_at = None;
        self.requests.drop_open();
        if let Some(mut broker) = self.broker.take() {
            broker.shutdown().await;
        }
        let Some(mut proc) = self.proc.take() else {
            return Ok(());
        };
        proc.close_stdin();
        if !self.exited {
            if matches!(mode, CloseMode::Force) {
                let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
                timeout(KILL_WAIT, proc.wait())
                    .await
                    .map_err(|_| AdapterError::Deadline)??;
            } else if timeout(self.stop_grace, proc.wait()).await.is_err() {
                let _ = group.signal_member(&proc.member(), rustix::process::Signal::TERM);
                if timeout(self.stop_grace.min(KILL_WAIT), proc.wait())
                    .await
                    .is_err()
                {
                    let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
                    timeout(KILL_WAIT, proc.wait())
                        .await
                        .map_err(|_| AdapterError::Deadline)??;
                }
            }
            self.exited = true;
        }
        Ok(())
    }
}

/// Профиль показывается только при таблице `[antigravity]`; каталог — после пробы.
pub fn catalog_entry(catalog: &AntigravityCatalog) -> crate::adapter::HarnessCatalog {
    catalog.harness()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_expose_the_live_verified_features() {
        let adapter = AntigravityAdapter::new(
            AntigravityConfig::default(),
            PathBuf::from("/tmp"),
            Duration::from_millis(100),
        );
        let capabilities = adapter.capabilities();
        assert_eq!(capabilities.streaming, Support::Supported);
        assert_eq!(capabilities.resume, Support::Supported);
        assert_eq!(capabilities.interrupt, Support::Supported);
        assert_eq!(capabilities.approvals, Support::Supported);
        assert_eq!(capabilities.questions, Support::Supported);
        assert_eq!(
            capabilities.background_work,
            Support::Unavailable("antigravity: P8")
        );
        assert!(capabilities.interrupt_keeps_process);
        assert!(adapter.holds_input_during_compaction());
    }
}
