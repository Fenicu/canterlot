pub mod broker;
pub mod input;
pub mod limits;
pub mod mcp;
pub mod media;
pub mod parser;
pub mod process;
pub mod version;

use self::{
    broker::{Broker, BrokerEvent},
    input::{InputQueue, LaunchSettings, Next},
    limits::LimitTracker,
    media::ImageWatch,
    parser::{Parsed, Parser},
    process::{CliProcess, LaunchSpec, SessionArg, StdoutLine},
};
use crate::{
    adapter::*,
    config::ClaudeCodeConfig,
    containment::{Containment, Member},
};
use canterlot_proto::{
    requests::DecisionPayload,
    worker::v1::{LimitObservation, LimitsSnapshot},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};
use uuid::Uuid;

pub struct ClaudeCodeAdapter {
    cfg: ClaudeCodeConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    cli: Option<CliProcess>,
    broker: Option<Broker>,
    parser: Parser,
    queue: Option<InputQueue>,
    images: ImageWatch,
    limits: LimitTracker,
    events: VecDeque<AdapterEvent>,
    native_id: String,
    permission_mode: String,
    approved: HashSet<String>,
    tools: HashMap<Uuid, AdapterEvent>,
    tool_ids: HashMap<String, Uuid>,
    tool_finals: HashMap<Uuid, AdapterEvent>,
    requests: HashMap<String, (String, Value)>,
    initialized: bool,
    eof: bool,
    exited: bool,
    start_attempted: bool,
    relay_executable: Option<PathBuf>,
}

impl ClaudeCodeAdapter {
    pub fn new(cfg: ClaudeCodeConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            cli: None,
            broker: None,
            parser: Parser::new(),
            queue: None,
            images: ImageWatch::new(),
            limits: LimitTracker::default(),
            events: VecDeque::new(),
            native_id: String::new(),
            permission_mode: String::new(),
            approved: HashSet::new(),
            tools: HashMap::new(),
            tool_ids: HashMap::new(),
            tool_finals: HashMap::new(),
            requests: HashMap::new(),
            initialized: false,
            eof: false,
            exited: false,
            start_attempted: false,
            relay_executable: None,
        }
    }

    pub fn with_relay_executable(mut self, executable: PathBuf) -> Self {
        self.relay_executable = Some(executable);
        self
    }

    fn diagnostic(&mut self, kind: &'static str, required: bool) {
        self.events.push_back(AdapterEvent::Diagnostic {
            kind,
            event_type: kind.into(),
            required,
        });
    }

    fn error_block(&mut self, text: String) {
        let block_id = Uuid::now_v7();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::Error,
            input_command_id: self
                .queue
                .as_ref()
                .and_then(|q| q.active().map(|a| a.command_id)),
            parent: None,
            agent: "main".into(),
            model: self
                .queue
                .as_ref()
                .map(|q| q.effective().model.clone())
                .unwrap_or_default(),
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

    fn feed(&mut self, line: StdoutLine) {
        match line {
            StdoutLine::Eof => self.eof = true,
            StdoutLine::Oversized(_) => self.diagnostic("oversized_frame", false),
            StdoutLine::Line(line) => {
                // MCP status is launch metadata; the parser's Init contract stays unchanged.
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value["type"] == "system" && value["subtype"] == "init" {
                        let connected = value["mcp_servers"].as_array().is_some_and(|servers| {
                            servers
                                .iter()
                                .any(|s| s["name"] == "canterlot" && s["status"] == "connected")
                        });
                        if !connected {
                            self.diagnostic("mcp_unavailable", true);
                        }
                    }
                    if value["type"] == "system" && value["subtype"] == "api_retry" {
                        self.events.push_back(AdapterEvent::Phase {
                            phase: Phase::Generating,
                            tool_name: None,
                            summary: Some(format!(
                                "retry {}/{}",
                                value["attempt"].as_u64().unwrap_or(0),
                                value["max_retries"].as_u64().unwrap_or(0)
                            )),
                        });
                    }
                }
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && value["required"] == true
                    && !matches!(
                        value["type"].as_str(),
                        Some(
                            "system"
                                | "stream_event"
                                | "assistant"
                                | "user"
                                | "result"
                                | "rate_limit_event"
                                | "control_response"
                        )
                    )
                {
                    self.diagnostic("unknown_required_frame", true);
                }
                // Finish is emitted after the error/limit facts from the same result frame.
                let mut finished = None;
                for parsed in self.parser.feed(&line) {
                    if let Parsed::Event(AdapterEvent::RunFinished { .. }) = &parsed {
                        finished = Some(parsed);
                        continue;
                    }
                    self.on_parsed(parsed);
                }
                if let Some(finished) = finished {
                    self.on_parsed(finished);
                }
            }
        }
    }

    fn on_parsed(&mut self, parsed: Parsed) {
        if let Some(queue) = &mut self.queue {
            queue.on_parsed(&parsed);
        }
        match parsed {
            Parsed::Init {
                session_id,
                permission_mode,
                version,
                ..
            } => {
                if !version.is_empty()
                    && version::parse_version(&version)
                        .is_ok_and(|v| v < version::MIN_SUPPORTED_VERSION)
                {
                    self.diagnostic("cli_too_old", true);
                }
                self.initialized = true;
                self.permission_mode = permission_mode;
                if session_id != self.native_id {
                    self.diagnostic("session_id_mismatch", true);
                }
            }
            Parsed::Event(event) => {
                match &event {
                    AdapterEvent::BlockStarted {
                        block_id,
                        kind: BlockKind::ToolCall,
                        ..
                    } => {
                        self.tools.insert(*block_id, event);
                        return;
                    }
                    AdapterEvent::BlockFinal { block_id, .. }
                        if self.tools.contains_key(block_id) =>
                    {
                        self.tool_finals.insert(*block_id, event);
                        return;
                    }
                    AdapterEvent::BlockStarted {
                        kind: BlockKind::ToolResult,
                        parent: Some(parent),
                        ..
                    } => {
                        let mark = self
                            .tool_ids
                            .iter()
                            .find(|(_, block)| **block == *parent)
                            .and_then(|(id, _)| {
                                mcp::cli_policy_mark(&self.permission_mode, &self.approved, id)
                            });
                        self.flush_tool(*parent, mark);
                    }
                    AdapterEvent::RunFinished { .. } => {
                        let blocks: Vec<_> = self.tools.keys().copied().collect();
                        for block in blocks {
                            self.flush_tool(block, None);
                        }
                    }
                    _ => {}
                }
                self.events.push_back(event);
            }
            Parsed::ToolCallComplete {
                block_id,
                tool_use_id,
                tool_name,
                input,
            } => {
                self.images
                    .on_tool_call(block_id, &tool_use_id, &tool_name, &input);
                self.tool_ids.insert(tool_use_id.clone(), block_id);
                if self.approved.contains(&tool_use_id)
                    || self.permission_mode == "bypassPermissions"
                    || matches!(tool_name.as_str(), "Agent" | "Task")
                {
                    self.flush_tool(block_id, None);
                }
            }
            Parsed::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                if let Some(event) = self.images.on_tool_result(&tool_use_id, &content, is_error) {
                    self.events.push_back(event);
                }
                self.tool_ids.remove(&tool_use_id);
                self.approved.remove(&tool_use_id);
            }
            Parsed::RateLimit(info) => self.events.extend(
                self.limits
                    .on_rate_limit(&info)
                    .into_iter()
                    .map(AdapterEvent::Limit),
            ),
            Parsed::TurnError {
                status,
                error,
                terminal_reason,
            } => {
                if terminal_reason.as_deref() == Some("aborted_tools") {
                    return;
                }
                let error = error.unwrap_or_else(|| "API error".into());
                let lower = error.to_lowercase();
                if status == Some(401)
                    || lower.contains("/login")
                    || lower.contains("log in")
                    || lower.contains("not logged in")
                {
                    self.diagnostic("auth_required", true);
                }
                if let Some(fact) = self.limits.on_turn_error(status, Some(&error)) {
                    self.events.push_back(AdapterEvent::Limit(fact));
                }
                let text = if status == Some(404) || lower.contains("model_not_found") {
                    "model not found".into()
                } else if let Some(status) = status {
                    format!("{status}: {error}")
                } else {
                    error
                };
                self.error_block(text);
            }
            _ => {}
        }
    }

    fn flush_tool(&mut self, block_id: Uuid, mark: Option<&str>) {
        if let Some(mut event) = self.tools.remove(&block_id) {
            if let AdapterEvent::BlockStarted { approved_by, .. } = &mut event {
                *approved_by = mark.map(str::to_owned);
            }
            self.events.push_back(event);
        }
        if let Some(event) = self.tool_finals.remove(&block_id) {
            self.events.push_back(event);
        }
    }

    async fn broker_event(&mut self, event: BrokerEvent) {
        match event {
            BrokerEvent::Opened {
                request_id,
                kind,
                payload,
            } => {
                self.approved.insert(request_id.clone());
                if let Some(block) = self.tool_ids.get(&request_id).copied() {
                    self.flush_tool(block, None);
                }
                self.requests
                    .insert(request_id.clone(), (kind.into(), payload.clone()));
                self.events.push_back(AdapterEvent::Phase {
                    phase: if kind == "question" {
                        Phase::AwaitingAnswer
                    } else {
                        Phase::AwaitingPermission
                    },
                    tool_name: None,
                    summary: None,
                });
                self.events.push_back(AdapterEvent::RequestOpened {
                    request_id,
                    kind: kind.into(),
                    payload,
                });
            }
            BrokerEvent::Cancelled { request_id } => {
                self.requests.remove(&request_id);
                self.events.push_back(AdapterEvent::RequestClosed {
                    request_id,
                    reason: "cancelled".into(),
                });
            }
            BrokerEvent::LimitsQuery { rpc_id } => {
                let snap = self.limits.snapshot();
                let current = [
                    (LimitWindow::FiveHour, snap.five_hour),
                    (LimitWindow::Weekly, snap.weekly),
                ]
                .into_iter()
                .filter_map(|(window, observation)| {
                    observation.map(|(used, reset)| LimitObservation {
                        window: match window {
                            LimitWindow::FiveHour => {
                                canterlot_proto::worker::v1::LimitWindow::FiveHour as i32
                            }
                            LimitWindow::Weekly => {
                                canterlot_proto::worker::v1::LimitWindow::Weekly as i32
                            }
                        },
                        used_permille: used.into(),
                        resets_at_ms: reset.unwrap_or_default(),
                        ..Default::default()
                    })
                })
                .collect();
                self.broker
                    .as_mut()
                    .unwrap()
                    .answer_limits(
                        rpc_id,
                        &LimitsSnapshot {
                            provider: "claude".into(),
                            current,
                            ..Default::default()
                        },
                    )
                    .await;
            }
        }
    }

    // Every wait is cancellation-safe: the line reader retains partial bytes.
    async fn pump(&mut self) {
        if self.exited || self.cli.is_none() {
            std::future::pending::<()>().await;
            return;
        }
        let cli = self.cli.as_mut().unwrap();
        let broker = self.broker.as_mut().unwrap();
        if self.eof {
            tokio::select! {
                status = cli.wait() => {
                    self.exited = true;
                    self.events.push_back(status.map(process::exit_event).unwrap_or(AdapterEvent::ProcessExited { code: None, signal: None }));
                }
                event = broker.next() => self.broker_event(event).await,
            }
        } else {
            tokio::select! {
                biased;
                line = cli.next_line() => self.feed(line),
                event = broker.next() => self.broker_event(event).await,
                _ = tokio::time::sleep(Duration::from_millis(25)) => {
                    if let Some(status) = cli.try_wait().ok().flatten() {
                        self.exited = true;
                        self.events.push_back(process::exit_event(status));
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Adapter for ClaudeCodeAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Supported,
            questions: Support::Supported,
            background_work: Support::Supported,
            interrupt_keeps_process: false,
        }
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
        match version::check(&self.cfg.executable).await {
            Ok(version::CliVersion::UntestedNewer(_)) => {
                self.diagnostic("untested_cli_version", false)
            }
            Ok(_) => {}
            Err(e) => {
                return Err(AdapterError::StartFailed(match e {
                    version::VersionError::NotFound => "cli_not_found",
                    version::VersionError::BelowMinimum(_) => "cli_too_old",
                    _ => "cli_version_failed",
                }));
            }
        }
        self.native_id = spec
            .native_conversation_id
            .clone()
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        // Пресеты инструкций — задача 6 плана 2c: пока поддерживается только «Как у владельца».
        let profile_name = match spec.instructions.mode {
            InstructionMode::Owner => "owner",
            _ => return Err(AdapterError::StartFailed("instructions_unsupported")),
        };
        let launch = LaunchSpec {
            executable: &self.cfg.executable,
            exec_dir: &self.exec_dir,
            workdir: &spec.workdir,
            session: spec
                .native_conversation_id
                .clone()
                .map(SessionArg::Resume)
                .unwrap_or_else(|| SessionArg::New(Uuid::parse_str(&self.native_id).unwrap())),
            model: &spec.model,
            effort: &spec.effort,
            permission_mode: &spec.permission_mode,
            instruction_profile: profile_name,
            instructions_file: None,
        };
        // The exec manager stages a named Task-1 profile before starting the supervisor.
        let broker = Broker::bind(&self.exec_dir).await?;
        process::write_exec_files(
            &launch,
            &self.cfg,
            &self
                .relay_executable
                .clone()
                .unwrap_or(std::env::current_exe()?),
        )
        .map_err(|_| AdapterError::StartFailed("cli_launch_failed"))?;
        self.broker = Some(broker);
        self.queue = Some(InputQueue::new(LaunchSettings {
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            permission_mode: spec.permission_mode.clone(),
        }));
        let cli = CliProcess::spawn(&launch, &self.cfg, group)
            .await
            .map_err(|_| AdapterError::StartFailed("cli_launch_failed"))?;
        let started = Started {
            native_conversation_id: self.native_id.clone(),
            cli_pid: cli.pid,
            cli_start_ticks: cli.start_ticks,
        };
        self.cli = Some(cli);
        Ok(started)
    }
    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        self.queue
            .as_ref()
            .ok_or("cli_not_started")?
            .validate(input.settings.as_ref())
    }
    async fn transmit(
        &mut self,
        input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.cli.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        if let Err(reason) = self.validate_transmission(input) {
            return TransmitOutcome::Rejected(reason.into());
        }
        self.queue.as_mut().unwrap().enqueue(input);
        let mut limit = Instant::now() + self.deadline;
        let mut written = false;
        loop {
            let queue = self.queue.as_mut().unwrap();
            if queue.is_proven(input.command_id) {
                if !self.initialized {
                    self.diagnostic("cli_init_missing", true);
                    return TransmitOutcome::Undetermined;
                }
                return TransmitOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind: input::PROOF_KIND,
                });
            }
            if let Some(reason) = queue.take_rejection(input.command_id) {
                return TransmitOutcome::Rejected(reason.into());
            }
            if self.eof || self.exited {
                return TransmitOutcome::Undetermined;
            }
            if let Next::Write(line) = queue.next(self.limits.snapshot().weekly) {
                if let Some(active) = queue.active() {
                    self.parser.set_input_command(active.command_id);
                    if active.command_id == input.command_id && !written {
                        written = true;
                        limit = Instant::now() + self.deadline;
                    }
                }
                if !matches!(
                    timeout_at(limit, self.cli.as_mut().unwrap().write_line(&line)).await,
                    Ok(Ok(()))
                ) {
                    return TransmitOutcome::Undetermined;
                }
                continue;
            }
            let until = queue.control_deadline().unwrap_or(limit).min(limit);
            if timeout_at(until, self.pump()).await.is_err() {
                if self.queue.as_ref().unwrap().control_deadline().is_some() {
                    self.queue.as_mut().unwrap().control_timed_out();
                    continue;
                }
                if !self.initialized && written {
                    self.diagnostic("cli_init_missing", true);
                }
                if written {
                    return TransmitOutcome::Undetermined;
                }
                self.queue.as_mut().unwrap().withdraw(input.command_id);
                return TransmitOutcome::Rejected("turn_active".into());
            }
        }
    }
    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        let Some((kind, payload)) = self.requests.get(request_id) else {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        };
        if !valid_decision(kind, payload, decision) {
            return DecisionOutcome::NotApplied("decision_invalid".into());
        }
        let outcome = self
            .broker
            .as_mut()
            .unwrap()
            .answer(request_id, decision)
            .await;
        if matches!(outcome, DecisionOutcome::Applied(_)) {
            self.requests.remove(request_id);
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id: request_id.into(),
                reason: "answered".into(),
            });
        }
        outcome
    }
    async fn interrupt(&mut self) -> InterruptOutcome {
        if self.exited || self.eof {
            return InterruptOutcome::NotApplicable;
        }
        let Some(queue) = self.queue.as_mut() else {
            return InterruptOutcome::NotApplicable;
        };
        if !queue.interrupt_sent() {
            return InterruptOutcome::NotApplicable;
        }
        if self
            .cli
            .as_ref()
            .unwrap()
            .signal(process::Signal::Int)
            .is_err()
        {
            return InterruptOutcome::Undetermined;
        }
        for request_id in self.requests.keys() {
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id: request_id.clone(),
                reason: "interrupted".into(),
            });
        }
        self.requests.clear();
        let until = Instant::now() + self.deadline;
        loop {
            if self.queue.as_ref().unwrap().interrupt_acknowledged() {
                return InterruptOutcome::Acknowledged;
            }
            if self.eof || timeout_at(until, self.pump()).await.is_err() {
                return InterruptOutcome::Undetermined;
            }
        }
    }
    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            self.pump().await;
        }
    }
    async fn close(
        &mut self,
        mode: CloseMode,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        if let Some(broker) = &mut self.broker {
            broker.shutdown().await;
        }
        let Some(cli) = &mut self.cli else {
            return Ok(());
        };
        cli.close_stdin();
        let grace = Duration::from_secs(2);
        if !self.exited {
            if matches!(mode, CloseMode::Force) || timeout(grace, cli.wait()).await.is_err() {
                group.signal_member(
                    &Member {
                        pid: cli.pid,
                        start_ticks: cli.start_ticks,
                    },
                    rustix::process::Signal::KILL,
                )?;
                timeout(grace, cli.wait())
                    .await
                    .map_err(|_| AdapterError::Deadline)??;
            }
            self.exited = true;
        }
        Ok(())
    }
}
