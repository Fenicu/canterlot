//! Адаптер Antigravity: `agy acp` (JSON-RPC 2.0 в stdio) на исполнение.
//! Задача 1: запуск, resume и каталог; задача 2: доставка, steer, логический ход,
//! события, прерывание и сжатие.

pub mod catalog;
pub mod events;
pub mod input;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod rpc;
pub mod session;
pub mod version;

pub use self::catalog::AntigravityCatalog;

use self::{
    events::Translator,
    input::{Effect, PromptKind, Target, Tracker},
    process::{PrepareError, Process, StdoutLine},
    rpc::{Incoming, RpcClient, RpcError, RpcId},
    session::{SessionError, SessionInfo},
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, Capabilities, CloseMode,
        CompactOutcome, DecisionOutcome, InstructionSpec, InterruptOutcome, ModelUsage, Phase,
        Proof, StartSpec, Started, SteerOutcome, Support, Transmission, TransmitOutcome,
    },
    config::AntigravityConfig,
    containment::Containment,
};
use canterlot_proto::requests::DecisionPayload;
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

pub struct AntigravityAdapter {
    cfg: AntigravityConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    ready_deadline: Duration,
    stop_grace: Duration,
    interrupt_deadline: Duration,
    turn_stall_deadline: Duration,
    proc: Option<Process>,
    rpc: RpcClient,
    events: VecDeque<AdapterEvent>,
    session: Option<SessionInfo>,
    instructions: InstructionSpec,
    workdir: PathBuf,
    native_conversation_id: String,
    translator: Option<Translator>,
    tracker: Tracker,
    responses: HashMap<i64, Result<Value, RpcError>>,
    current_model: String,
    current_effort: String,
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
            responses: HashMap::new(),
            current_model: String::new(),
            current_effort: String::new(),
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

    /// Сколько ждать ответа после `Undetermined`, прежде чем закрыть процесс силой.
    pub fn with_turn_stall_deadline(mut self, deadline: Duration) -> Self {
        self.turn_stall_deadline = deadline;
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
        let Some(mut proc) = self.proc.take() else {
            return;
        };
        proc.close_stdin();
        let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
        let _ = timeout(KILL_WAIT, proc.wait()).await;
        self.exited = true;
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
                session::resume(proc, &mut self.rpc, native_id, &self.instructions, timeout)
                    .await
                    .map_err(Self::session_error)?
            }
            None => session::new_session(
                proc,
                &mut self.rpc,
                &self.workdir,
                &self.instructions,
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
        match self.proc.as_mut() {
            Some(proc) => proc.write_message(&message).await.is_ok(),
            None => false,
        }
    }

    async fn pump_once(&mut self) -> bool {
        if self.exit_reported || self.eof {
            return false;
        }
        let Some(proc) = self.proc.as_mut() else {
            self.exit_reported = true;
            return false;
        };
        let line = proc.next_line().await;
        self.dispatch_line(line).await
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
                Incoming::ServerRequest { id, method, .. } => {
                    self.handle_server_request(&id, &method).await;
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
        let fed = match self.translator.as_mut() {
            Some(translator) => translator.feed(update),
            None => return,
        };
        let cancel = fed.cancel;
        self.events.extend(fed.events);
        if cancel {
            self.send_cancel().await;
        }
    }

    async fn send_cancel(&mut self) {
        let notification = input::cancel_notification(&self.session_id());
        let _ = self.write_message(notification).await;
    }

    async fn handle_server_request(&mut self, id: &RpcId, method: &str) {
        if method == "session/request_permission" {
            // Задача 3 откроет настоящий запрос владельцу; сейчас отказ, чтобы ход не завис.
            self.diagnostic("antigravity_permission_denied", false);
            let reply = json!({
                "jsonrpc": "2.0",
                "id": id.to_value(),
                "result": {"outcome": {"outcome": "selected", "optionId": "deny"}},
            });
            let _ = self.write_message(reply).await;
        } else {
            self.diagnostic("unsupported_server_request", false);
            let mut reply = RpcClient::error(id, -32601, "Method not found");
            reply["jsonrpc"] = json!("2.0");
            let _ = self.write_message(reply).await;
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
        self.stall_at = None;
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
        if let Some(message) = failed {
            self.error_block(message, Some(command_id));
        }
        let context_tokens = usage.last_input;
        let usage: Vec<ModelUsage> = usage.model_usage(&self.current_model).into_iter().collect();
        self.events.push_back(AdapterEvent::RunFinished {
            input_command_id: command_id,
            usage,
            context_tokens,
            context_window_tokens: 0,
        });
        self.events.push_back(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
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
        use std::os::unix::process::ExitStatusExt;
        self.eof = true;
        let status = match self.proc.as_mut() {
            Some(proc) => proc.wait().await.ok(),
            None => None,
        };
        self.exited = true;
        self.exit_reported = true;
        self.stall_at = None;
        if let Some(Effect::CompactDone { ok }) = self.tracker.on_eof() {
            self.events.push_back(AdapterEvent::Compacted { ok });
        }
        let (code, signal) = status.map_or((None, None), |status| (status.code(), status.signal()));
        self.events
            .push_back(AdapterEvent::ProcessExited { code, signal });
    }

    async fn check_stall(&mut self) {
        if !self.stall_at.is_some_and(|at| Instant::now() >= at) {
            return;
        }
        use std::os::unix::process::ExitStatusExt;
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
        self.exited = true;
        self.exit_reported = true;
        if let Some(Effect::CompactDone { ok }) = self.tracker.on_eof() {
            self.events.push_back(AdapterEvent::Compacted { ok });
        }
        let (code, signal) = status.map_or((None, None), |status| (status.code(), status.signal()));
        self.events
            .push_back(AdapterEvent::ProcessExited { code, signal });
    }

    // --- настройки хода -----------------------------------------------------

    async fn settings_call(
        &mut self,
        method: &'static str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let (id, message) = self.rpc.request_v2(method, params);
        if !self.write_message(message).await {
            self.rpc.forget(id);
            return Err(RpcError {
                code: -32000,
                message: "process exited".into(),
                data: None,
            });
        }
        let limit = Instant::now() + self.deadline;
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
        if change.permission_mode.is_some()
            || change
                .instructions
                .as_ref()
                .is_some_and(|next| !next.same_effect(&self.instructions))
        {
            // Смена режима и пресета — перезапуск между ходами (задача 4).
            self.diagnostic("settings_restart_required", false);
        }
        Ok(())
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

enum EchoEnd {
    Accepted,
    Rejected(String),
    Undetermined,
}

#[async_trait::async_trait]
impl Adapter for AntigravityAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Unavailable("antigravity: task 3"),
            questions: Support::Unavailable("antigravity: task 3"),
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
        if matches!(pinned.version, version::CliVersion::UntestedNewer(_)) {
            self.diagnostic("untested_cli_version", false);
        }
        self.instructions = spec.instructions.clone();
        self.workdir = spec.workdir.clone();
        // Аргументы проверяются до spawn: неверный режим не оставляет процесс.
        if process::launch_args(&self.exec_dir, &spec.permission_mode).is_err() {
            return Err(AdapterError::StartFailed("unknown_permission_mode"));
        }
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

    async fn transmit(
        &mut self,
        input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.proc.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        if self.tracker.has_turn() {
            return TransmitOutcome::Rejected("turn_active".into());
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
        self.tracker.expect_echo(input.command_id, &text);
        self.tracker.await_echo(input.command_id);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(Some(input.command_id));
        }
        if !self.write_message(message).await {
            self.tracker.forget_command(input.command_id);
            self.rpc.forget(id);
            return TransmitOutcome::Undetermined;
        }
        let limit = Instant::now() + self.deadline;
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
        _request_id: &str,
        _decision: &DecisionPayload,
    ) -> DecisionOutcome {
        DecisionOutcome::NotApplied("antigravity: task 3".into())
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        let Some(target) = self.tracker.target() else {
            return InterruptOutcome::NotApplicable;
        };
        self.send_cancel().await;
        let limit = Instant::now() + self.interrupt_deadline;
        loop {
            let done = match target {
                Target::Turn => !self.tracker.has_turn(),
                Target::Compact => !self.tracker.has_compaction(),
            };
            if done {
                return match target {
                    Target::Turn if self.tracker.last_stop() == Some("cancelled") => {
                        InterruptOutcome::Acknowledged
                    }
                    Target::Compact if self.tracker.last_compact_ok() == Some(false) => {
                        InterruptOutcome::Acknowledged
                    }
                    _ => InterruptOutcome::NotApplicable,
                };
            }
            if self.eof || self.exited {
                return InterruptOutcome::Undetermined;
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
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
        self.tracker.expect_echo(input.command_id, &text);
        self.tracker.await_echo(input.command_id);
        if !self.write_message(message).await {
            self.tracker.forget_command(input.command_id);
            self.rpc.forget(id);
            return SteerOutcome::Undetermined;
        }
        let limit = Instant::now() + self.deadline;
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
        self.tracker.expect_echo(command, input::COMPACT_TEXT);
        self.tracker.await_echo(command);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(None);
        }
        if !self.write_message(message).await {
            self.tracker.forget_command(command);
            self.rpc.forget(id);
            return CompactOutcome::Failed;
        }
        let limit = Instant::now() + self.deadline;
        match self.wait_echo(command, limit).await {
            EchoEnd::Accepted => CompactOutcome::Accepted,
            EchoEnd::Rejected(_) | EchoEnd::Undetermined => {
                self.tracker.forget_command(command);
                CompactOutcome::Failed
            }
        }
    }

    async fn agent_call_result(&mut self, _call_id: Uuid, _result: AgentCallResult) {}

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            self.check_stall().await;
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            if self.exit_reported {
                return std::future::pending().await;
            }
            match self.stall_at {
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
        self.stall_at = None;
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
    fn capabilities_reserve_requests_for_task_3() {
        let adapter = AntigravityAdapter::new(
            AntigravityConfig::default(),
            PathBuf::from("/tmp"),
            Duration::from_millis(100),
        );
        let capabilities = adapter.capabilities();
        assert_eq!(capabilities.streaming, Support::Supported);
        assert_eq!(capabilities.resume, Support::Supported);
        assert_eq!(capabilities.interrupt, Support::Supported);
        assert_eq!(
            capabilities.approvals,
            Support::Unavailable("antigravity: task 3")
        );
        assert_eq!(
            capabilities.questions,
            Support::Unavailable("antigravity: task 3")
        );
        assert_eq!(
            capabilities.background_work,
            Support::Unavailable("antigravity: P8")
        );
        assert!(capabilities.interrupt_keeps_process);
        assert!(adapter.holds_input_during_compaction());
    }
}
