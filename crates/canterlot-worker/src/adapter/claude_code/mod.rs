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
    requests::{DecisionPayload, PermissionBehavior},
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

/// Сколько кнопочное сжатие ждёт `compact_boundary`, прежде чем признать отказ.
const COMPACT_TIMEOUT: Duration = Duration::from_secs(300);

pub struct ClaudeCodeAdapter {
    cfg: ClaudeCodeConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    /// Срок до первого `system/init`: CLI шлёт его после первого ввода и после подключения серверов MCP.
    ready_deadline: Duration,
    stop_grace: Duration,
    cli: Option<CliProcess>,
    broker: Option<Broker>,
    parser: Parser,
    queue: Option<InputQueue>,
    images: ImageWatch,
    limits: LimitTracker,
    events: VecDeque<AdapterEvent>,
    native_id: String,
    permission_mode: String,
    workdir: PathBuf,
    instructions: InstructionSpec,
    approved: HashSet<String>,
    tools: HashMap<Uuid, AdapterEvent>,
    tool_ids: HashMap<String, Uuid>,
    tool_finals: HashMap<Uuid, AdapterEvent>,
    requests: HashMap<String, (String, Value)>,
    limits_calls: HashMap<Uuid, String>,
    /// Вводы, о которых `transmit` вернул `Undetermined`: их позднее
    /// доказательство уходит событием `InputAccepted`.
    unconfirmed: HashSet<Uuid>,
    initialized: bool,
    /// Текущий процесс CLI уже прислал `system/init`; каждый запуск начинает с `false`.
    cli_ready: bool,
    /// Запуск с `--resume`: беседа записана, хотя этот CLI ещё ни разу не ответил.
    resumed: bool,
    eof: bool,
    exited: bool,
    start_attempted: bool,
    isolate_mcp: bool,
    relay_executable: Option<PathBuf>,
    /// Смена, для которой нужен перезапуск CLI, ждёт конца фоновой работы: перезапуск убил бы её.
    deferred: Option<SettingsChange>,
    /// `DeferredRestartDue` уже отдан; сбрасывается новым кадром CLI.
    deferred_due: bool,
    background_changed: Option<Instant>,
    /// Кнопка «Сжать контекст» отправила `/compact` и ждёт `compact_boundary`.
    compact_pending: Option<(Uuid, Instant)>,
    /// Граница сжатия прошла, служебный итог ещё нет: команда закрывается им, а не границей.
    compact_result_due: bool,
}

impl ClaudeCodeAdapter {
    pub fn new(cfg: ClaudeCodeConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            ready_deadline: deadline,
            stop_grace: Duration::from_secs(5),
            cli: None,
            broker: None,
            parser: Parser::new(),
            queue: None,
            images: ImageWatch::new(),
            limits: LimitTracker::default(),
            events: VecDeque::new(),
            native_id: String::new(),
            permission_mode: String::new(),
            workdir: PathBuf::new(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            approved: HashSet::new(),
            tools: HashMap::new(),
            tool_ids: HashMap::new(),
            tool_finals: HashMap::new(),
            requests: HashMap::new(),
            limits_calls: HashMap::new(),
            unconfirmed: HashSet::new(),
            initialized: false,
            cli_ready: false,
            resumed: false,
            eof: false,
            exited: false,
            start_attempted: false,
            isolate_mcp: false,
            relay_executable: None,
            deferred: None,
            deferred_due: false,
            background_changed: None,
            compact_pending: None,
            compact_result_due: false,
        }
    }

    pub fn with_ready_deadline(mut self, ready_deadline: Duration) -> Self {
        self.ready_deadline = ready_deadline;
        self
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
            field_names: Vec::new(),
        });
    }

    fn error_block(&mut self, text: String) {
        self.notice(BlockKind::Error, text);
    }

    fn notice(&mut self, kind: BlockKind, text: String) {
        let block_id = Uuid::now_v7();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind,
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
                self.deferred_due = false;
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
        if let Some(queue) = &mut self.queue
            && let Some(proof) = queue.on_parsed(&parsed)
            && self.unconfirmed.remove(&proof.command_id)
        {
            self.events.push_back(AdapterEvent::InputAccepted {
                command_id: proof.command_id,
            });
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
                self.cli_ready = true;
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
                    AdapterEvent::BackgroundWork { .. } => {
                        self.background_changed = Some(Instant::now());
                    }
                    AdapterEvent::CompactionFinished { ok, .. }
                        if self.compact_pending.is_some() =>
                    {
                        // Граница прошла, служебный итог впереди: команду закрывает
                        // он, иначе ввод владельца влезет между ними и итог
                        // припишется чужому ходу. Отказ закрывает сразу: он и есть итог.
                        if *ok {
                            self.compact_result_due = true;
                        } else if self.compact_pending.take().is_some() {
                            self.compact_result_due = false;
                            self.events.push_back(AdapterEvent::Compacted { ok: *ok });
                        }
                    }
                    _ => {}
                }
                self.events.push_back(event);
            }
            Parsed::CompactResultConsumed { ok } => {
                if self.compact_pending.take().is_some() {
                    self.compact_result_due = false;
                    self.events.push_back(AdapterEvent::Compacted { ok });
                }
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
                if terminal_reason
                    .as_deref()
                    .is_some_and(input::is_interrupt_reason)
                {
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

    /// Ответ на запрос вернул агента к работе, а кадров до конца инструмента не будет.
    fn leave_request_phase(&mut self, phase: Phase) {
        if self.requests.is_empty() {
            self.events.push_back(AdapterEvent::Phase {
                phase,
                tool_name: None,
                summary: None,
            });
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
                self.events.push_back(AdapterEvent::RequestOpened {
                    request_id,
                    kind: kind.into(),
                    payload,
                });
                self.events.push_back(AdapterEvent::Phase {
                    phase: if kind == "question" {
                        Phase::AwaitingAnswer
                    } else {
                        Phase::AwaitingPermission
                    },
                    tool_name: None,
                    summary: None,
                });
            }
            BrokerEvent::Cancelled { request_id } => {
                // Запрос, уже закрытый прерыванием или перезапуском, второй раз не закрывается.
                if self.requests.remove(&request_id).is_some() {
                    self.events.push_back(AdapterEvent::RequestClosed {
                        request_id,
                        reason: "cancelled".into(),
                    });
                    self.leave_request_phase(Phase::Generating);
                }
            }
            BrokerEvent::AgentTool {
                call_id,
                tool,
                arguments,
                ..
            } => {
                // Вызов пресетов уходит в хаб; итог вернётся через agent_call_result.
                self.events.push_back(AdapterEvent::AgentCall {
                    call_id,
                    tool: tool.to_owned(),
                    arguments,
                });
            }
            BrokerEvent::ShareFile { call_id, arguments } => {
                self.events
                    .push_back(AdapterEvent::ShareFile { call_id, arguments });
            }
            BrokerEvent::LimitsQuery { call_id } => {
                let snap = self.limits.snapshot();
                let current = [
                    (LimitWindow::FiveHour, snap.five_hour),
                    (LimitWindow::Weekly, snap.weekly),
                ]
                .into_iter()
                .filter_map(|(window, observation)| {
                    observation.map(|(used, reset)| LimitObservation {
                        window: window.into(),
                        used_permille: used.into(),
                        resets_at_ms: reset.unwrap_or_default(),
                        ..Default::default()
                    })
                })
                .collect();
                let text = crate::adapter::canterlot_mcp::mcp::limits_tool_text(
                    &LimitsSnapshot {
                        provider: "claude".into(),
                        current,
                        ..Default::default()
                    },
                    "claude",
                );
                self.limits_calls.insert(call_id, text);
                self.events.push_back(AdapterEvent::Limits { call_id });
            }
        }
    }

    /// Аргумент беседы для нового процесса: `--resume` отказывает без записанной
    /// беседы («No conversation found»), повторный `--session-id` — при записанной
    /// («already in use»). Запись появляется только с первым ходом.
    fn relaunch_session(&self) -> SessionArg {
        match Uuid::parse_str(&self.native_id) {
            Ok(id) if !self.initialized && !self.resumed => SessionArg::New(id),
            _ => SessionArg::Resume(self.native_id.clone()),
        }
    }

    /// Остановить старый CLI для перезапуска между ходами: stdin EOF, до 5 с
    /// ожидания с чтением stdout через разбор, затем SIGTERM группе процесса
    /// и до 2 с, затем SIGKILL. После SIGKILL дочитывание stdout и ожидание
    /// выхода ограничены: вывод может держать процесс вне группы CLI (инструмент
    /// ведёт свою группу), а сам CLI может застрять в непрерываемом сне; тогда
    /// остатки дерева исполнения гасит контейнер. Открытые запросы закрываются
    /// как прерванные.
    async fn stop_old_cli(&mut self, group: &dyn Containment) {
        let Some(cli) = self.cli.as_mut() else {
            return;
        };
        cli.close_stdin();
        if !drain_until_exit(
            cli,
            &mut self.parser,
            &mut self.queue,
            Duration::from_secs(5),
        )
        .await
        {
            signal_group(cli.pid, process::Signal::Term);
            if !drain_until_exit(
                cli,
                &mut self.parser,
                &mut self.queue,
                Duration::from_secs(2),
            )
            .await
            {
                signal_group(cli.pid, process::Signal::Kill);
                let drained = timeout(
                    KILL_DRAIN,
                    drain_to_eof(cli, &mut self.parser, &mut self.queue),
                )
                .await
                .is_ok();
                if !drained || timeout(KILL_DRAIN, cli.wait()).await.is_err() {
                    // Новый CLI ещё не запущен: всё в контейнере принадлежит старому.
                    let _ = group.terminate(KILL_DRAIN).await;
                    let _ = timeout(KILL_DRAIN, cli.wait()).await;
                }
            }
        }
        self.cli = None;
        self.eof = false;
        self.exited = false;
        for request_id in self.requests.keys().cloned().collect::<Vec<_>>() {
            self.requests.remove(&request_id);
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id,
                reason: "interrupted".into(),
            });
        }
    }

    /// Запустить CLI для перезапуска: мгновенно вышедший процесс считается
    /// неудачей запуска (init приходит только после первого ввода).
    async fn spawn_for_restart(
        &mut self,
        launch: &process::LaunchSpec<'_>,
        group: &dyn Containment,
    ) -> Option<CliProcess> {
        if version::check(&self.cfg.executable).await.is_err() {
            return None;
        }
        if let Some(broker) = &self.broker {
            broker.arm();
        }
        let mut cli = match CliProcess::spawn(launch, &self.cfg, group).await {
            Ok(cli) => cli,
            Err(error) => {
                tracing::warn!(
                    kind = "cli_restart_failed",
                    reason = error.reason(),
                    "cli restart failed"
                );
                return None;
            }
        };
        if exited_early(&mut cli).await {
            if let Some(kind) = process::exit_cause(&cli.final_stderr().await) {
                self.diagnostic(kind, false);
            }
            return None;
        }
        Some(cli)
    }

    /// Перезапустить CLI между ходами с новыми инструкциями: новый процесс
    /// продолжает беседу и поднимается с флагами из действующих настроек с
    /// применённой сменой; ввод пишется только в новый процесс. Ошибка — запасной
    /// запуск со старыми настройками; его успех — отказ входа, провал — остановка.
    async fn restart_for_instructions(
        &mut self,
        change: &SettingsChange,
        group: &dyn Containment,
    ) -> Result<(), &'static str> {
        let next = change
            .instructions
            .clone()
            .ok_or("instructions_change_failed")?;
        let (model, effort, permission_mode) = match self.queue.as_ref() {
            Some(queue) => {
                let effective = queue.effective();
                (
                    change
                        .model
                        .clone()
                        .unwrap_or_else(|| effective.model.clone()),
                    change
                        .effort
                        .clone()
                        .unwrap_or_else(|| effective.effort.clone()),
                    change
                        .permission_mode
                        .clone()
                        .unwrap_or_else(|| effective.permission_mode.clone()),
                )
            }
            None => return Err("instructions_change_failed"),
        };
        let workdir = self.workdir.clone();
        let exec_dir = self.exec_dir.clone();
        let executable = self.cfg.executable.clone();
        // 1. Файл инструкций нового запуска (или удаление устаревшего).
        if process::write_instructions_file(&exec_dir, &next).is_err() {
            return Err("instructions_change_failed");
        }
        // 2. Остановить старый CLI.
        self.stop_old_cli(group).await;
        // 3. Сбросить состояние разбора, запустить новый CLI с той же беседой.
        self.parser.reset_process();
        let launch = process::LaunchSpec {
            executable: &executable,
            exec_dir: &exec_dir,
            workdir: &workdir,
            session: self.relaunch_session(),
            model: &model,
            effort: &effort,
            permission_mode: &permission_mode,
            instructions: &next,
            isolate_mcp: self.isolate_mcp,
        };
        let spawned = self.spawn_for_restart(&launch, group).await;
        let Some(cli) = spawned else {
            return self.respawn_previous(group).await;
        };
        let started = (cli.pid, cli.start_ticks);
        self.cli = Some(cli);
        self.cli_ready = false;
        if let Some(queue) = self.queue.as_mut() {
            queue.apply_restarted(&model, &effort, &permission_mode);
        }
        self.instructions = next;
        self.events.push_back(AdapterEvent::CliRestarted {
            cli_pid: started.0,
            cli_start_ticks: started.1,
        });
        Ok(())
    }

    /// В stdin осталась оборванная строка: следующий кадр склеился бы с ней, ввод уходит
    /// только в новый процесс с той же беседой и действующими настройками.
    async fn restart_torn(&mut self, group: &dyn Containment) -> bool {
        tracing::warn!(
            kind = "cli_stdin_torn",
            "cli stdin torn, restarting the cli"
        );
        self.stop_old_cli(group).await;
        self.parser.reset_process();
        let _ = self.respawn_previous(group).await;
        self.cli.is_some()
    }

    /// Запасной запуск со старыми инструкциями и настройками после неудачного
    /// перезапуска, аргумент беседы тот же, что у перезапуска. Успех — отказ
    /// входа; провал, в том числе невозвращённый файл прежних инструкций, —
    /// остановка исполнения с причиной `restart_failed`.
    async fn respawn_previous(&mut self, group: &dyn Containment) -> Result<(), &'static str> {
        let previous = self.instructions.clone();
        let (model, effort, permission_mode) = match self.queue.as_ref() {
            Some(queue) => {
                let effective = queue.effective();
                (
                    effective.model.clone(),
                    effective.effort.clone(),
                    effective.permission_mode.clone(),
                )
            }
            None => return Err("instructions_change_failed"),
        };
        // Без прежнего файла запасной CLI получил бы новый текст пресета,
        // а адаптер и хаб считали бы действующим старый.
        if process::write_instructions_file(&self.exec_dir, &previous).is_err() {
            self.diagnostic("restart_failed", true);
            return Err("instructions_change_failed");
        }
        let workdir = self.workdir.clone();
        let exec_dir = self.exec_dir.clone();
        let executable = self.cfg.executable.clone();
        let launch = process::LaunchSpec {
            executable: &executable,
            exec_dir: &exec_dir,
            workdir: &workdir,
            session: self.relaunch_session(),
            model: &model,
            effort: &effort,
            permission_mode: &permission_mode,
            instructions: &previous,
            isolate_mcp: self.isolate_mcp,
        };
        match self.spawn_for_restart(&launch, group).await {
            Some(cli) => {
                let started = (cli.pid, cli.start_ticks);
                self.cli = Some(cli);
                self.cli_ready = false;
                self.events.push_back(AdapterEvent::CliRestarted {
                    cli_pid: started.0,
                    cli_start_ticks: started.1,
                });
                Err("instructions_change_failed")
            }
            None => {
                self.diagnostic("restart_failed", true);
                Err("instructions_change_failed")
            }
        }
    }

    /// Перезапуск оборвал бы фоновую работу или ход, который CLI начал сам. Задача только что
    /// закончилась: её пробуждающий ход начнётся через миллисекунды, кадра ещё может не быть.
    fn background_busy(&self) -> bool {
        self.parser.background_running() > 0
            || self.parser.wake_turn_open()
            || self
                .background_changed
                .is_some_and(|at| at.elapsed() < RESTART_SETTLE)
    }

    /// Запомнить смену до конца фоновой работы. Новая смена поверх отложенной: поля новой главнее,
    /// а возврат к действующим инструкциям отменяет перезапуск.
    fn defer_restart(&mut self, change: SettingsChange) {
        match self.deferred.take() {
            Some(known) => {
                let change = merged(known, change);
                self.deferred =
                    process::needs_restart(&self.instructions, &change).then_some(change);
            }
            None => {
                self.deferred = Some(change);
                tracing::info!(
                    kind = "instructions_restart_deferred",
                    "instructions change waits for background work"
                );
                self.notice(BlockKind::Warning, DEFERRED_NOTICE.to_owned());
            }
        }
    }

    /// Отложенная смена дождалась своего часа и уходит вместе со сменой этого ввода, если она есть.
    fn take_deferred(&mut self, own: Option<&SettingsChange>) -> Option<SettingsChange> {
        let deferred = self.deferred.take()?;
        let change = match own {
            Some(own) => merged(deferred, own.clone()),
            None => deferred,
        };
        process::needs_restart(&self.instructions, &change).then_some(change)
    }

    /// Один проход доставки; `Undetermined` запоминается вызывающим.
    async fn transmit_once(
        &mut self,
        input: &Transmission,
        group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.cli.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        if let Err(reason) = self.validate_transmission(input) {
            return TransmitOutcome::Rejected(reason.into());
        }
        if self.cli.as_ref().is_some_and(CliProcess::stdin_torn) && !self.restart_torn(group).await
        {
            return TransmitOutcome::Undetermined;
        }
        // Перезапуск между ходами со сменой инструкций — до постановки и записи ввода. Пока идёт
        // фоновая работа, ждёт только перезапуск. Модель и режим уходят управляющими запросами
        // перед сообщением: хаб покажет их сразу, и агент не должен работать шире показанного.
        let own_restart = input
            .settings
            .as_ref()
            .is_some_and(|change| process::needs_restart(&self.instructions, change));
        let mut settings = input.settings.clone();
        let mut restart_later = None;
        if self.background_busy() {
            let pending = self.deferred.is_some();
            if let Some(change) = settings
                .as_mut()
                .filter(|change| own_restart || (pending && change.instructions.is_some()))
            {
                restart_later = Some(SettingsChange {
                    effort: change.effort.take(),
                    instructions: change.instructions.take(),
                    ..SettingsChange::default()
                });
            }
        } else if let Some(change) = self
            .take_deferred(input.settings.as_ref())
            .or_else(|| input.settings.clone().filter(|_| own_restart))
        {
            let deferred = !own_restart;
            if let Err(reason) = self.restart_for_instructions(&change, group).await {
                if !deferred {
                    return TransmitOutcome::Rejected(reason.into());
                }
                self.error_block(DEFERRED_FAILED.to_owned());
                if self.cli.is_none() {
                    return TransmitOutcome::Undetermined;
                }
            }
        }
        self.queue.as_mut().unwrap().enqueue(&Transmission {
            command_id: input.command_id,
            attempt: input.attempt,
            text: input.text.clone(),
            files: input.files.clone(),
            settings,
        });
        let mut limit = Instant::now() + self.wait_bound();
        let mut written = false;
        let mut ready_seen = self.cli_ready;
        loop {
            // Срок запуска кончается с init: дальше ответ ждёт обычный срок передачи.
            if self.cli_ready && !ready_seen {
                ready_seen = true;
                if written {
                    limit = Instant::now() + self.deadline;
                }
            }
            let queue = self.queue.as_mut().unwrap();
            if queue.is_proven(input.command_id) {
                let kind = queue
                    .proof_kind(input.command_id)
                    .unwrap_or(input::PROOF_KIND);
                if self.cli_ready {
                    return Self::accepted(input.command_id, kind);
                }
                // Ход уже закончился без init: ждать нечего. Идущий ход ещё может получить init.
                if !queue.is_active(input.command_id) {
                    self.diagnostic("cli_init_missing", true);
                    return Self::accepted(input.command_id, kind);
                }
            }
            if let Some(reason) = queue.take_rejection(input.command_id) {
                return TransmitOutcome::Rejected(reason.into());
            }
            if self.eof || self.exited {
                return TransmitOutcome::Undetermined;
            }
            // Ответ раньше init не отвергается: init может прийти следом, дольше срока его не ждут.
            if !queue.is_proven(input.command_id)
                && let Next::Write(line) = queue.next(self.limits.snapshot().weekly)
            {
                if let Some(active) = queue.active() {
                    self.parser.set_input_command(active.command_id);
                    if active.command_id == input.command_id && !written {
                        written = true;
                        limit = Instant::now() + self.wait_bound();
                    }
                }
                // Отказ управляющего запроса отклоняет ввод целиком, перезапуск ждёт только принятый.
                if written && let Some(change) = restart_later.take() {
                    self.defer_restart(change);
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
                if !self.cli_ready && written {
                    self.diagnostic("cli_init_missing", true);
                }
                if written {
                    if let Some(kind) = self.queue.as_ref().unwrap().proof_kind(input.command_id) {
                        return Self::accepted(input.command_id, kind);
                    }
                    return TransmitOutcome::Undetermined;
                }
                self.queue.as_mut().unwrap().withdraw(input.command_id);
                return TransmitOutcome::Rejected("turn_active".into());
            }
        }
    }

    /// Проверка вливания: смена настроек между кадрами хода не применяется,
    /// хаб настроек и не присылает. Непустая смена — отказ, а не молчаливое
    /// вливание без неё.
    fn validate_steer(&self, input: &Transmission) -> Result<(), &'static str> {
        if input
            .settings
            .as_ref()
            .is_some_and(|change| *change != SettingsChange::default())
        {
            return Err("settings_unavailable");
        }
        Ok(())
    }

    fn accepted(command_id: Uuid, kind: &'static str) -> TransmitOutcome {
        TransmitOutcome::Accepted(Proof { command_id, kind })
    }

    /// Пока процесс не прислал init, ждут срок запуска; готовый процесс отвечает за срок передачи.
    fn wait_bound(&self) -> Duration {
        if self.cli_ready {
            self.deadline
        } else {
            self.ready_deadline.max(self.deadline)
        }
    }

    /// Причина встаёт перед событием выхода: после него сопровождение событий не читает.
    /// Событие уже в очереди, поэтому отмена этого ожидания его не теряет.
    async fn name_exit_cause(&mut self) {
        let Some(cli) = self.cli.as_mut() else {
            return;
        };
        if let Some(kind) = process::exit_cause(&cli.final_stderr().await) {
            let at = self.events.len().saturating_sub(1);
            self.events.insert(
                at,
                AdapterEvent::Diagnostic {
                    kind,
                    event_type: kind.into(),
                    required: false,
                    field_names: Vec::new(),
                },
            );
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
        if self.exited {
            self.name_exit_cause().await;
        }
    }
}

/// Предел дочитывания stdout и ожидания выхода после SIGKILL при перезапуске.
const KILL_DRAIN: Duration = Duration::from_secs(1);

/// Сколько список задач должен не меняться, прежде чем останавливать CLI: между концом задачи
/// и `system/init` её пробуждающего хода в записанных сессиях проходило меньше 50 мс.
const RESTART_SETTLE: Duration = Duration::from_secs(2);

const DEFERRED_NOTICE: &str = "Смена инструкций ждёт конца фоновой работы: перезапуск агента её бы остановил. Новые инструкции подействуют, как только задачи закончатся.";
const DEFERRED_FAILED: &str =
    "Отложенная смена инструкций не применилась: агент работает с прежними.";

fn merged(older: SettingsChange, newer: SettingsChange) -> SettingsChange {
    SettingsChange {
        model: newer.model.or(older.model),
        effort: newer.effort.or(older.effort),
        permission_mode: newer.permission_mode.or(older.permission_mode),
        instructions: newer.instructions.or(older.instructions),
    }
}

/// Ранний выход CLI сразу после запуска: процесс не прожил и секунды.
async fn exited_early(cli: &mut CliProcess) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        match cli.try_wait() {
            Ok(Some(_)) => {
                let _ = cli.wait().await;
                return true;
            }
            Err(error) if reaped(&error) => return true,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Сигнал группе процесса CLI (CLI запускается лидером своей группы).
fn signal_group(pid: u32, signal: process::Signal) {
    use rustix::process::{Pid, Signal as RSignal, kill_process_group};
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    let signal = match signal {
        process::Signal::Int => RSignal::INT,
        process::Signal::Term => RSignal::TERM,
        process::Signal::Kill => RSignal::KILL,
    };
    let _ = kill_process_group(pid, signal);
}

/// Питать разбор строками stdout, пока процесс не выйдет или не кончится срок.
async fn drain_until_exit(
    cli: &mut CliProcess,
    parser: &mut Parser,
    queue: &mut Option<InputQueue>,
    limit: Duration,
) -> bool {
    let start = Instant::now();
    loop {
        match cli.try_wait() {
            Ok(Some(_)) => {
                // Инструмент может держать чужой stdout и писать в него без конца.
                let _ = timeout(KILL_DRAIN, drain_available(cli, parser, queue)).await;
                return true;
            }
            // Убран чужой проверкой: процесса нет, остаток stdout уже не прочитать.
            Err(error) if reaped(&error) => return true,
            _ => {}
        }
        if start.elapsed() >= limit {
            return false;
        }
        match timeout(Duration::from_millis(50), cli.next_line()).await {
            Ok(StdoutLine::Line(line)) => {
                for parsed in parser.feed(&line) {
                    if let Some(queue) = queue.as_mut() {
                        queue.on_parsed(&parsed);
                    }
                }
            }
            // Конец stdout отвечает мгновенно, а процесс ещё жив.
            Ok(StdoutLine::Eof) => tokio::time::sleep(Duration::from_millis(50)).await,
            Ok(_) | Err(_) => {}
        }
    }
}

/// Добрать остаток stdout без разбора событий: процесс умирает, кадры не нужны.
async fn drain_available(
    cli: &mut CliProcess,
    parser: &mut Parser,
    queue: &mut Option<InputQueue>,
) {
    loop {
        match timeout(Duration::from_millis(50), cli.next_line()).await {
            Ok(StdoutLine::Line(line)) => {
                for parsed in parser.feed(&line) {
                    if let Some(queue) = queue.as_mut() {
                        queue.on_parsed(&parsed);
                    }
                }
            }
            _ => return,
        }
    }
}

/// Читать stdout до конца после SIGKILL.
async fn drain_to_eof(cli: &mut CliProcess, parser: &mut Parser, queue: &mut Option<InputQueue>) {
    loop {
        match cli.next_line().await {
            StdoutLine::Line(line) => {
                for parsed in parser.feed(&line) {
                    if let Some(queue) = queue.as_mut() {
                        queue.on_parsed(&parsed);
                    }
                }
            }
            _ => return,
        }
    }
}

impl ClaudeCodeAdapter {
    fn send_sigint(&self) -> std::io::Result<()> {
        match self.cli.as_ref() {
            Some(cli) => cli.signal(process::Signal::Int),
            None => Err(std::io::Error::other("no cli")),
        }
    }

    /// Просроченное сжатие — отказ: граница так и не пришла, висеть вечно нельзя.
    /// Граница без итога тоже отпускает очередь: эпизод уже закрыт, дублировать нечего.
    fn compact_timed_out(&mut self) -> bool {
        if self
            .compact_pending
            .is_some_and(|(_, until)| Instant::now() >= until)
        {
            self.compact_pending = None;
            self.parser.forget_compact();
            let boundary_seen = std::mem::take(&mut self.compact_result_due);
            self.events.push_back(AdapterEvent::Compacted { ok: false });
            if !boundary_seen {
                self.events.push_back(AdapterEvent::CompactionFinished {
                    manual: true,
                    ok: false,
                    pre_tokens: 0,
                    post_tokens: 0,
                });
            }
            return true;
        }
        false
    }
}

#[async_trait::async_trait]
impl Adapter for ClaudeCodeAdapter {
    fn isolates_mcp(&self) -> bool {
        true
    }

    /// Служебный `/compact` идёт вводом stream-json между ходами: итог чужого
    /// хода приписался бы вводу владельца без hold.
    fn holds_input_during_compaction(&self) -> bool {
        true
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Supported,
            questions: Support::Supported,
            background_work: Support::Supported,
            interrupt_keeps_process: true,
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
        self.resumed = spec.native_conversation_id.is_some();
        self.native_id = spec
            .native_conversation_id
            .clone()
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        // Пресеты инструкций принимает все режимы: текст пишет хаб через StartSpec.
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
            instructions: &spec.instructions,
            isolate_mcp: spec.isolate_mcp,
        };
        self.isolate_mcp = spec.isolate_mcp;
        self.workdir = spec.workdir.clone();
        self.instructions = spec.instructions.clone();
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
        .map_err(|error| AdapterError::StartFailed(error.reason()))?;
        broker.arm();
        self.broker = Some(broker);
        self.queue = Some(InputQueue::new(LaunchSettings {
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            permission_mode: spec.permission_mode.clone(),
        }));
        let cli = CliProcess::spawn(&launch, &self.cfg, group)
            .await
            .map_err(|error| AdapterError::StartFailed(error.reason()))?;
        let started = Started {
            native_conversation_id: self.native_id.clone(),
            cli_pid: cli.pid,
            cli_start_ticks: cli.start_ticks,
        };
        self.cli = Some(cli);
        Ok(started)
    }
    fn set_stop_grace(&mut self, grace: Duration) {
        self.stop_grace = grace;
    }
    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        // Смена инструкций обрабатывается перезапуском CLI, а не управляющими запросами.
        if let Some(change) = input.settings.as_ref()
            && process::needs_restart(&self.instructions, change)
        {
            return Ok(());
        }
        self.queue
            .as_ref()
            .ok_or("cli_not_started")?
            .validate(input.settings.as_ref())
    }
    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome {
        let outcome = self.transmit_once(input, group).await;
        if matches!(outcome, TransmitOutcome::Undetermined) {
            self.unconfirmed.insert(input.command_id);
        }
        outcome
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
            let allowed = matches!(
                decision,
                DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    ..
                }
            );
            self.leave_request_phase(if allowed {
                Phase::RunningTool
            } else {
                Phase::Generating
            });
        }
        outcome
    }
    async fn refuse_request(&mut self, request_id: &str, message: &str) {
        if self.requests.remove(request_id).is_some()
            && let Some(broker) = self.broker.as_mut()
        {
            broker.refuse(request_id, message).await;
        }
    }
    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        // Итог вызова пресетов уходит запросившему CLI; чужой итог брокер отбросит.
        let result = crate::adapter::limits_fallback(&mut self.limits_calls, call_id, result);
        if let Some(broker) = self.broker.as_mut() {
            broker.answer_agent_tool(call_id, &result).await;
        }
    }

    /// Сжатие по кнопке: `/compact` уходит вводом через stream-json между ходами,
    /// итог приходит границей `compact_boundary` и доводится `Compacted`.
    async fn compact(&mut self, command: Uuid) -> CompactOutcome {
        if self.compact_pending.is_some() {
            return CompactOutcome::NotApplicable;
        }
        let turn_busy = self
            .queue
            .as_ref()
            .is_some_and(|queue| queue.active().is_some())
            || self.parser.wake_turn_open();
        if turn_busy {
            return CompactOutcome::NotApplicable;
        }
        let Some(cli) = self.cli.as_mut() else {
            return CompactOutcome::Failed;
        };
        if self.exited || self.eof || !self.cli_ready {
            return CompactOutcome::Failed;
        }
        let line = serde_json::json!({
            "type": "user",
            "uuid": command.to_string(),
            "message": {"role": "user", "content": "/compact"},
            "parent_tool_use_id": serde_json::Value::Null,
        });
        if !matches!(
            timeout(self.deadline, cli.write_line(&line)).await,
            Ok(Ok(()))
        ) {
            return CompactOutcome::Failed;
        }
        self.parser.mark_compact_manual(command);
        self.compact_pending = Some((command, Instant::now() + COMPACT_TIMEOUT));
        self.compact_result_due = false;
        CompactOutcome::Accepted
    }

    /// Влить ожидающее сообщение в идущий ход («Отправить сейчас»): `user`-кадр
    /// пишется между кадрами хода, CLI ставит его в очередь за текущим вызовом
    /// инструмента и отвечает тем же ходом. Живой прогон показал, что
    /// `interrupt(send_now:true)` для этого не годится: он останавливает ход и
    /// стартует влитое сообщение новым ходом (`send_now: "stopped"` в квитанции).
    /// Доказательство — кадр `command_lifecycle` CLI с UUID вливания.
    async fn steer(&mut self, input: &Transmission) -> SteerOutcome {
        if self.queue.as_mut().is_none() {
            return SteerOutcome::Undetermined;
        }
        {
            let queue = self.queue.as_ref().unwrap();
            if queue.is_steer_proven(input.command_id) {
                let kind = queue
                    .steer_proof_kind(input.command_id)
                    .unwrap_or(input::PROOF_KIND_STARTED);
                return SteerOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind,
                });
            }
        }
        // Процесса нет или его stdin сломан: вливать некуда, сообщение уйдёт обычным вводом.
        if self.exited || self.eof || self.cli.as_ref().is_none_or(CliProcess::stdin_torn) {
            return SteerOutcome::NotApplicable("cli_unavailable".into());
        }
        if let Err(reason) = self.validate_steer(input) {
            return SteerOutcome::Rejected(reason.into());
        }
        let line = match self.queue.as_mut().unwrap().steer_frame(input) {
            Ok(line) => line,
            Err(_) => return SteerOutcome::Rejected("attachment_unreadable".into()),
        };
        // Разбор кадров никому не переназначается: итог хода принадлежит чужому вводу.
        let written_at = Instant::now();
        if !matches!(
            timeout_at(
                written_at + self.deadline,
                self.cli.as_mut().unwrap().write_line(&line)
            )
            .await,
            Ok(Ok(()))
        ) {
            self.queue.as_mut().unwrap().drop_steer(input.command_id);
            return SteerOutcome::Undetermined;
        }
        let until = written_at + self.deadline;
        loop {
            let queue = self.queue.as_mut().unwrap();
            if queue.is_steer_proven(input.command_id) {
                let kind = queue
                    .steer_proof_kind(input.command_id)
                    .unwrap_or(input::PROOF_KIND_STARTED);
                return SteerOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind,
                });
            }
            if self.eof || self.exited {
                self.queue.as_mut().unwrap().drop_steer(input.command_id);
                return SteerOutcome::Undetermined;
            }
            if timeout_at(until, self.pump()).await.is_err() {
                self.unconfirmed.insert(input.command_id);
                return SteerOutcome::Undetermined;
            }
        }
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        if self.exited || self.eof || self.cli.is_none() {
            return InterruptOutcome::NotApplicable;
        }
        let Some(line) = self
            .queue
            .as_mut()
            .and_then(|queue| queue.interrupt_request())
        else {
            return InterruptOutcome::NotApplicable;
        };
        for request_id in self.requests.keys() {
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id: request_id.clone(),
                reason: "interrupted".into(),
            });
        }
        self.requests.clear();
        // Управляющий запрос останавливает ход, процесс жив: следующий ввод
        // уйдёт в тот же CLI без перезапуска. SIGINT — только запасной.
        let written = matches!(
            timeout(self.deadline, self.cli.as_mut().unwrap().write_line(&line)).await,
            Ok(Ok(()))
        );
        let mut sigint_sent = false;
        let mut until = Instant::now() + self.deadline;
        if !written {
            // stdin не принял запрос целиком: дальше только сигнал.
            if self.send_sigint().is_err() {
                return InterruptOutcome::Undetermined;
            }
            sigint_sent = true;
            until = Instant::now() + self.deadline;
        }
        loop {
            if self.queue.as_ref().unwrap().interrupt_acknowledged() {
                return InterruptOutcome::Acknowledged;
            }
            // Обычный итог после квитанции тоже закрывает ход: ждать нечего.
            if self.queue.as_ref().unwrap().turn_over() {
                return InterruptOutcome::Undetermined;
            }
            if self.eof {
                return InterruptOutcome::Undetermined;
            }
            if !sigint_sent && self.queue.as_ref().unwrap().interrupt_receipt_expired() {
                // Запасной путь: квитанции нет в срок, останавливаем ход сигналом.
                if self.send_sigint().is_err() {
                    return InterruptOutcome::Undetermined;
                }
                sigint_sent = true;
                // У сигнала своё окно: иначе его итог никто бы не дождался.
                until = Instant::now() + self.deadline;
            }
            // Пока квитанции нет, ожидание идёт до её срока, даже если срок
            // передачи короче: иначе запасной SIGINT никогда бы не наступил.
            let receipt = self
                .queue
                .as_ref()
                .unwrap()
                .interrupt_receipt_bound()
                .filter(|_| !sigint_sent);
            match receipt {
                Some(bound) => {
                    let _ = timeout_at(bound, self.pump()).await;
                }
                None => {
                    if timeout_at(until, self.pump()).await.is_err() {
                        return InterruptOutcome::Undetermined;
                    }
                }
            }
        }
    }
    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            self.compact_timed_out();
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            if self.deferred.is_some()
                && !self.deferred_due
                && !self.background_busy()
                && self.queue.as_ref().is_some_and(|q| q.active().is_none())
            {
                self.deferred_due = true;
                return AdapterEvent::DeferredRestartDue;
            }
            self.pump().await;
        }
    }
    async fn apply_deferred(&mut self, group: &dyn Containment) {
        if self.background_busy()
            || self.exited
            || self.eof
            || self.cli.is_none()
            || self.queue.as_ref().is_none_or(|q| q.active().is_some())
        {
            return;
        }
        if let Some(change) = self.take_deferred(None)
            && self.restart_for_instructions(&change, group).await.is_err()
        {
            self.error_block(DEFERRED_FAILED.to_owned());
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
        let member = Member {
            pid: cli.pid,
            start_ticks: cli.start_ticks,
        };
        if !self.exited {
            // Мягко: EOF, затем SIGTERM (CLI сам гасит деревья Bash и выходит
            // с 143), затем срок stop_grace; SIGKILL — только после него. Stdout
            // читается и здесь: CLI на записи в полный pipe сигнал бы не обработал.
            let exited = match mode {
                CloseMode::Force => false,
                CloseMode::Graceful => {
                    drain_until_exit(cli, &mut self.parser, &mut self.queue, grace).await || {
                        group.signal_member(&member, rustix::process::Signal::TERM)?;
                        drain_until_exit(cli, &mut self.parser, &mut self.queue, self.stop_grace)
                            .await
                    }
                }
            };
            if !exited {
                group.signal_member(&member, rustix::process::Signal::KILL)?;
                timeout(grace, cli.wait())
                    .await
                    .map_err(|_| AdapterError::Deadline)??;
            }
            self.exited = true;
        }
        Ok(())
    }
}
