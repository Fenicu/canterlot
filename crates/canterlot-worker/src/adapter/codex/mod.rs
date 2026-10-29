pub mod events;
pub mod input;
pub mod limits;
pub mod mcp;
pub mod probe;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod requests;
pub mod rpc;
pub mod thread;
pub mod version;

use self::{
    events::{Translated, Translator, TurnErrorInfo, TurnStatus},
    input::{CurrentSettings, ModelEntry, Outbox, model_entries},
    limits::{CrunchContext, LimitTracker, ReachedDecision},
    process::{AppServerProcess, LaunchSpec, StdoutLine},
    requests::{Intake, Requests, Resolved},
    rpc::{Incoming, RpcClient, RpcError, RpcId},
    thread::{Effective, OwnerDefaults, PermissionMode},
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, BlockKind, Capabilities, CloseMode, DecisionOutcome,
        DecisionProof, InterruptOutcome, ModelCatalog, Phase, Proof, StartSpec, Started, Support,
        Transmission, TransmitOutcome, canterlot_mcp::Broker, canterlot_mcp::BrokerEvent,
    },
    config::CodexConfig,
    containment::Containment,
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::Duration,
};
use tokio::time::{Instant, timeout, timeout_at};
use uuid::Uuid;

// Каталог моделей из зонда worker start: версия CLI и записи model/list.
#[derive(Clone)]
pub struct CodexCatalog {
    pub version: version::CliVersion,
    pub models: Vec<ModelEntry>,
}

impl CodexCatalog {
    // Модели каталога, умолчательная первой.
    pub fn models(&self) -> Vec<ModelCatalog> {
        let mut models = self.models.clone();
        models.sort_by_key(|entry| !entry.default);
        models
            .into_iter()
            .map(|entry| ModelCatalog {
                id: entry.id,
                title: entry.title,
                efforts: entry.efforts,
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum DiscoverError {
    Version(version::VersionError),
    Probe(probe::ProbeError),
}

// `codex --version` и model/list без запроса к модели; три попытки за минуту.
pub async fn discover(cfg: &CodexConfig) -> Result<CodexCatalog, DiscoverError> {
    let version = version::check(&cfg.executable)
        .await
        .map_err(DiscoverError::Version)?;
    let mut failure = None;
    for attempt in 0..3 {
        match probe::probe(
            &cfg.executable,
            "model/list",
            json!({"includeHidden": false, "limit": 100}),
            Duration::from_secs(15),
        )
        .await
        {
            Ok(result) => {
                return Ok(CodexCatalog {
                    version,
                    models: model_entries(&result),
                });
            }
            Err(error) => {
                failure = Some(error);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
    Err(DiscoverError::Probe(
        failure.unwrap_or(probe::ProbeError::Timeout),
    ))
}

const HANDSHAKE_STEP: Duration = Duration::from_secs(20);
const CRUNCH_PROBE: Duration = Duration::from_secs(15);

type RpcResult = Result<Value, RpcError>;

pub struct CodexAdapter {
    cfg: CodexConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    proc: Option<AppServerProcess>,
    rpc: RpcClient,
    translator: Option<Translator>,
    requests: Requests,
    outbox: Outbox,
    limits: LimitTracker,
    broker: Option<Broker>,
    events: VecDeque<AdapterEvent>,
    hold: Vec<AdapterEvent>,
    pending_writes: VecDeque<Value>,
    effective: Option<Effective>,
    owner: OwnerDefaults,
    models: Vec<ModelEntry>,
    mode: PermissionMode,
    effort: Option<String>,
    last_noted: Option<u16>,
    turn_items: HashSet<String>,
    started_items: HashMap<String, Value>,
    completed_status: HashMap<String, Option<String>>,
    responses: HashMap<i64, RpcResult>,
    resolved: VecDeque<Resolved>,
    proof_for: Option<Uuid>,
    proof_seen: bool,
    eof: bool,
    exited: bool,
    start_attempted: bool,
    relay_executable: Option<PathBuf>,
}

impl CodexAdapter {
    pub fn new(cfg: CodexConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            proc: None,
            rpc: RpcClient::new(),
            translator: None,
            requests: Requests::new(),
            outbox: Outbox::new(),
            limits: LimitTracker::default(),
            broker: None,
            events: VecDeque::new(),
            hold: Vec::new(),
            pending_writes: VecDeque::new(),
            effective: None,
            owner: OwnerDefaults {
                approval_policy: None,
                sandbox_mode: None,
                sandbox_workspace_write: None,
                developer_instructions: None,
            },
            models: Vec::new(),
            mode: PermissionMode::Owner,
            effort: None,
            last_noted: None,
            turn_items: HashSet::new(),
            started_items: HashMap::new(),
            completed_status: HashMap::new(),
            responses: HashMap::new(),
            resolved: VecDeque::new(),
            proof_for: None,
            proof_seen: false,
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

    fn error_block(&mut self, text: String, command_id: Option<Uuid>) {
        let block_id = Uuid::now_v7();
        let model = self
            .effective
            .as_ref()
            .map(|effective| effective.model.clone())
            .unwrap_or_default();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::Error,
            input_command_id: command_id,
            parent: None,
            agent: "orchestrator".into(),
            model,
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

    // --- чтение потока ------------------------------------------------------

    // Одно чтение stdout, событие брокера или проверка выхода; отмена безопасна.
    async fn pump_once(&mut self) {
        if self.exited || self.proc.is_none() {
            std::future::pending::<()>().await;
            return;
        }
        let proc = self.proc.as_mut().unwrap();
        let broker = self.broker.as_mut().unwrap();
        if self.eof {
            tokio::select! {
                status = proc.wait() => {
                    self.exited = true;
                    self.events.push_back(status.map(exit_event).unwrap_or(AdapterEvent::ProcessExited { code: None, signal: None }));
                }
                event = broker.next() => self.on_broker(event).await,
            }
        } else {
            tokio::select! {
                biased;
                line = proc.next_line() => self.dispatch_line(line).await,
                event = broker.next() => self.on_broker(event).await,
                _ = tokio::time::sleep(Duration::from_millis(25)) => {
                    if let Some(status) = proc.try_wait().ok().flatten() {
                        self.exited = true;
                        self.events.push_back(exit_event(status));
                    }
                }
            }
        }
    }

    async fn dispatch_line(&mut self, line: StdoutLine) {
        match line {
            StdoutLine::Eof => self.eof = true,
            StdoutLine::Oversized(_) => self.diagnostic("oversized_frame", false),
            StdoutLine::Line(line) => match rpc::classify(&line) {
                Incoming::Response { id, result } => {
                    if let RpcId::Int(number) = id {
                        self.responses.insert(number, result);
                    }
                }
                Incoming::Notification { method, params } => {
                    let translated = self.feed_notification(&method, &params);
                    self.dispatch_translated(translated).await;
                }
                Incoming::ServerRequest { id, method, params } => {
                    self.on_server_request(id, &method, &params)
                }
                Incoming::Invalid => self.diagnostic("unparsed_line", false),
            },
        }
    }

    // Уведомления без ожидания: перевод и очередь событий.
    fn feed_notification(&mut self, method: &str, params: &Value) -> Vec<Translated> {
        if let Some(event) = mcp::startup_status_event(method, params) {
            self.events.push_back(event);
        }
        if method == "item/started"
            && let Some(active) = self.outbox.active()
            && params.get("turnId").and_then(Value::as_str) == Some(active.turn_id.as_str())
            && let Some(item) = params.get("item")
            && let Some(id) = item.get("id").and_then(Value::as_str)
        {
            self.turn_items.insert(id.to_owned());
            if item.get("type").and_then(Value::as_str) == Some("fileChange") {
                self.started_items.insert(id.to_owned(), item.clone());
            }
        }
        if method == "item/completed"
            && let Some(id) = params
                .get("item")
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
        {
            self.started_items.remove(id);
        }
        match self.translator.as_mut() {
            Some(translator) => translator.feed(method, params),
            None => Vec::new(),
        }
    }

    // Полная обработка с возможными зондами (ошибки хода, лимиты).
    async fn dispatch_translated(&mut self, translated: Vec<Translated>) {
        for item in translated {
            self.on_translated(item).await;
        }
    }

    fn on_server_request(&mut self, id: RpcId, method: &str, params: &Value) {
        let main = params.get("threadId").and_then(Value::as_str)
            == self
                .effective
                .as_ref()
                .map(|effective| effective.thread_id.as_str());
        let ctx = requests::RouteCtx {
            agent: if main { "orchestrator" } else { "subagent" },
            item: params
                .get("itemId")
                .and_then(Value::as_str)
                .and_then(|item| self.started_items.get(item))
                .map(|item| item as &Value),
        };
        match self.requests.on_server_request(id, method, params, &ctx) {
            Intake::Open(event) => {
                let phase = match &event {
                    AdapterEvent::RequestOpened { kind, .. } if kind == "question" => {
                        Some(Phase::AwaitingAnswer)
                    }
                    AdapterEvent::RequestOpened { .. } => Some(Phase::AwaitingPermission),
                    _ => None,
                };
                self.events.push_back(event);
                if let Some(phase) = phase {
                    self.events.push_back(AdapterEvent::Phase {
                        phase,
                        tool_name: None,
                        summary: None,
                    });
                }
            }
            Intake::Reply(line) => self.pending_writes.push_back(line),
        }
        for diagnostic in self.requests.drain_diagnostics() {
            self.events.push_back(diagnostic);
        }
    }

    async fn on_translated(&mut self, translated: Translated) {
        match translated {
            Translated::TurnCompleted {
                turn_id,
                status,
                error,
            } => self.on_turn_completed(turn_id, status, error).await,
            Translated::RateLimits(snapshot) => {
                for fact in self.limits.on_snapshot(&snapshot) {
                    self.events.push_back(AdapterEvent::Limit(fact));
                }
            }
            Translated::ServerRequestResolved { request_id } => {
                match self.requests.on_resolved(&request_id) {
                    Some(Resolved::Ours { request_id }) => {
                        self.resolved.push_back(Resolved::Ours { request_id })
                    }
                    Some(Resolved::Cleared { request_id })
                        if self.outbox.active().is_some() =>
                    {
                        self.events.push_back(AdapterEvent::RequestClosed {
                            request_id,
                            reason: "cli_cancelled".into(),
                        });
                    }
                    Some(Resolved::Cleared { .. }) | None => {}
                }
            }
            Translated::ItemCompleted {
                item_id,
                item_type,
                status,
                approved_by_cli,
            } => {
                self.completed_status.insert(item_id, status);
                let mark = (matches!(item_type.as_str(), "commandExecution" | "fileChange")
                    && approved_by_cli)
                    .then(|| {
                        self.effective.as_ref().and_then(|effective| {
                            requests::cli_policy_mark(
                                &effective.approval_policy,
                                &effective.sandbox,
                            )
                        })
                    })
                    .flatten();
                for held in &mut self.hold {
                    if let AdapterEvent::BlockStarted {
                        kind: BlockKind::ToolCall,
                        approved_by,
                        ..
                    } = held
                        && approved_by.is_none()
                    {
                        *approved_by = mark.map(str::to_owned);
                    }
                }
                self.events.extend(self.hold.drain(..));
            }
            Translated::SettingsUpdated(settings) => {
                if let Some(effective) = self.effective.as_mut() {
                    if let Some(policy) = settings.get("approvalPolicy") {
                        effective.approval_policy = policy.clone();
                    }
                    if let Some(sandbox) = settings.get("sandbox") {
                        effective.sandbox = sandbox.clone();
                    }
                    if let Some(model) = settings.get("model").and_then(Value::as_str) {
                        effective.model = model.to_owned();
                    }
                }
            }
            Translated::UserMessage { client_id, .. } => {
                if self.proof_for.is_some_and(|command| {
                    client_id.as_deref() == Some(command.to_string().as_str())
                }) {
                    self.proof_for = None;
                    self.proof_seen = true;
                }
            }
            Translated::TurnStarted { turn_id } => self.outbox.on_turn_started(&turn_id),
            Translated::Event(event) => match &event {
                AdapterEvent::BlockStarted {
                    kind: BlockKind::ToolCall | BlockKind::ToolResult,
                    ..
                } => self.hold.push(event),
                AdapterEvent::BlockFinal { block_id, .. }
                    if self.hold.iter().any(|held| {
                        matches!(held, AdapterEvent::BlockStarted { block_id: id, .. } if id == block_id)
                    }) =>
                {
                    self.hold.push(event);
                }
                _ => self.events.push_back(event),
            },
            Translated::McpStatus { .. } | Translated::Ignored => {}
        }
    }

    async fn on_turn_completed(
        &mut self,
        turn_id: String,
        status: TurnStatus,
        error: Option<TurnErrorInfo>,
    ) {
        let command = self
            .outbox
            .active()
            .filter(|active| active.turn_id == turn_id)
            .map(|active| active.command_id);
        let was_active = self.outbox.on_turn_completed(&turn_id);
        // Отложенные вызовы уходят как есть: ход кончился без их завершения.
        self.events.extend(self.hold.drain(..));
        self.turn_items.clear();
        self.completed_status.clear();
        if !was_active {
            return;
        }
        match (status, error) {
            (TurnStatus::Interrupted, _) => {}
            (_, None) if status != TurnStatus::Failed => {}
            (_, Some(error)) => self.on_turn_error(error, command).await,
            (_, None) => self.error_block("Ошибка Codex".into(), command),
        }
        if let Some(translator) = self.translator.as_mut() {
            let finished = translator.finish_turn();
            self.events.push_back(finished);
        }
        self.events.push_back(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
    }

    async fn on_turn_error(&mut self, error: TurnErrorInfo, command: Option<Uuid>) {
        match error.kind.as_str() {
            "unauthorized" => {
                self.error_block(
                    "Codex требует входа: выполните codex login на машине".into(),
                    command,
                );
                self.diagnostic("auth_required", false);
            }
            "contextWindowExceeded" => {}
            "usageLimitExceeded" => {
                let decision = self.limits.on_turn_failed(&limits::TurnErrorInfo {
                    codex_error_info: Some(error.kind.clone()),
                });
                self.crunch_reached(decision).await;
                self.error_block("Исчерпан лимит Codex".into(), command);
            }
            "rateLimitExceeded" => {
                let decision = self.limits.on_turn_failed(&limits::TurnErrorInfo {
                    codex_error_info: Some(error.kind.clone()),
                });
                self.crunch_reached(decision).await;
            }
            "other" if error.message.contains("model_not_found") => {
                self.error_block("Модель не найдена".into(), command);
            }
            kind => {
                self.error_block(turn_error_text(kind, error.http_status), command);
            }
        }
    }

    async fn crunch_reached(&mut self, decision: ReachedDecision) {
        match decision {
            ReachedDecision::Reached(fact) => {
                self.events.push_back(AdapterEvent::Limit(fact));
            }
            ReachedDecision::NeedFreshRead(ctx) => {
                for fact in self.fresh_read(ctx).await {
                    self.events.push_back(AdapterEvent::Limit(fact));
                }
            }
            ReachedDecision::NotALimit => {}
        }
    }

    // Свежие окна без запроса к модели: короткий probe account/rateLimits/read.
    async fn fresh_read(&mut self, ctx: CrunchContext) -> Vec<crate::adapter::LimitFact> {
        let response = probe::probe(
            &self.cfg.executable,
            "account/rateLimits/read",
            json!({}),
            CRUNCH_PROBE,
        )
        .await;
        match response {
            Ok(response) => self.limits.on_fresh_read(&response, ctx),
            Err(_) => Vec::new(),
        }
    }

    async fn on_broker(&mut self, event: BrokerEvent) {
        let BrokerEvent::LimitsQuery { rpc_id } = event else {
            return;
        };
        let snapshot = self.limits.snapshot();
        let current = [
            (crate::adapter::LimitWindow::FiveHour, snapshot.five_hour),
            (crate::adapter::LimitWindow::Weekly, snapshot.weekly),
        ]
        .into_iter()
        .filter_map(|(window, observation)| {
            observation.map(
                |(used, reset)| canterlot_proto::worker::v1::LimitObservation {
                    window: match window {
                        crate::adapter::LimitWindow::FiveHour => {
                            canterlot_proto::worker::v1::LimitWindow::FiveHour as i32
                        }
                        crate::adapter::LimitWindow::Weekly => {
                            canterlot_proto::worker::v1::LimitWindow::Weekly as i32
                        }
                    },
                    used_permille: used.into(),
                    resets_at_ms: reset.unwrap_or_default(),
                    ..Default::default()
                },
            )
        })
        .collect();
        self.broker
            .as_mut()
            .unwrap()
            .answer_limits(
                rpc_id,
                &canterlot_proto::worker::v1::LimitsSnapshot {
                    provider: "codex".into(),
                    current,
                    ..Default::default()
                },
            )
            .await;
    }

    // --- RPC поверх насоса ----------------------------------------------------

    async fn write_request(&mut self, message: Value) -> bool {
        let Some(proc) = self.proc.as_mut() else {
            return false;
        };
        proc.write_message(&message).await.is_ok()
    }

    async fn wait_response(&mut self, id: i64, until: Instant) -> Option<RpcResult> {
        loop {
            if let Some(result) = self.responses.remove(&id) {
                return Some(result);
            }
            if self.eof || self.exited {
                return None;
            }
            self.flush_writes().await;
            if timeout_at(until, self.pump_once()).await.is_err() {
                return self.responses.remove(&id);
            }
        }
    }

    async fn flush_writes(&mut self) {
        while let Some(line) = self.pending_writes.pop_front() {
            let Some(proc) = self.proc.as_mut() else {
                self.pending_writes.push_front(line);
                break;
            };
            if proc.write_message(&line).await.is_err() {
                self.pending_writes.push_front(line);
                break;
            }
        }
    }

    async fn rpc_round(
        &mut self,
        method: &'static str,
        params: Value,
        per_step: Duration,
    ) -> Option<RpcResult> {
        let (id, message) = self.rpc.request(method, params);
        if !self.write_request(message).await {
            return None;
        }
        self.wait_response(id, Instant::now() + per_step).await
    }

    // --- handshake ------------------------------------------------------------

    async fn handshake(&mut self, spec: &StartSpec) -> Result<Started, AdapterError> {
        let worker_version = env!("CARGO_PKG_VERSION");
        let (id, message) = self
            .rpc
            .request("initialize", thread::initialize_params(worker_version));
        if !self.write_request(message).await {
            return Err(AdapterError::StartFailed("app_server_failed"));
        }
        if self
            .wait_response(id, Instant::now() + HANDSHAKE_STEP)
            .await
            .is_none()
        {
            return Err(AdapterError::StartFailed("app_server_failed"));
        }
        if !self
            .write_request(RpcClient::notification("initialized", json!({})))
            .await
        {
            return Err(AdapterError::StartFailed("app_server_failed"));
        }
        let account = self
            .rpc_round(
                "account/read",
                thread::account_read_params(),
                HANDSHAKE_STEP,
            )
            .await
            .ok_or(AdapterError::StartFailed("app_server_failed"))?
            .map_err(|_| AdapterError::StartFailed("app_server_failed"))?;
        thread::check_account(&account).map_err(|_| AdapterError::StartFailed("auth_required"))?;
        let config = self
            .rpc_round(
                "config/read",
                thread::config_read_params(&spec.workdir),
                HANDSHAKE_STEP,
            )
            .await
            .ok_or(AdapterError::StartFailed("app_server_failed"))?
            .map_err(|_| AdapterError::StartFailed("app_server_failed"))?;
        self.owner = thread::owner_defaults_from(&config);
        let models = self
            .rpc_round(
                "model/list",
                json!({"includeHidden": false, "limit": 100}),
                HANDSHAKE_STEP,
            )
            .await
            .ok_or(AdapterError::StartFailed("model_list_failed"))?
            .map_err(|_| AdapterError::StartFailed("model_list_failed"))?;
        self.models = model_entries(&models);
        // Пресеты инструкций — задача 7 плана 2c: пока поддерживается только «Как у владельца».
        let profile_name = match spec.instructions.mode {
            crate::adapter::InstructionMode::Owner => "owner",
            _ => return Err(AdapterError::StartFailed("instructions_unsupported")),
        };
        let instructions = thread::compose_developer_instructions(
            profile_name,
            &self.owner,
            mcp::SERVER_INSTRUCTIONS,
        )
        .map_err(|_| AdapterError::StartFailed("unsupported_instructions"))?;
        let model = (!spec.model.is_empty()).then(|| spec.model.clone());
        let effective = if let Some(native) = spec.native_conversation_id.as_deref() {
            let result = self
                .rpc_round(
                    "thread/resume",
                    thread::thread_resume_params(
                        native,
                        self.mode,
                        model.as_deref(),
                        &spec.workdir,
                        &instructions,
                    ),
                    HANDSHAKE_STEP,
                )
                .await
                .ok_or(AdapterError::StartFailed("app_server_failed"))?
                .map_err(|_| AdapterError::StartFailed("resume_failed"))?;
            thread::parse_thread_result(&result)
                .map_err(|_| AdapterError::StartFailed("resume_failed"))?
        } else {
            let result = self
                .rpc_round(
                    "thread/start",
                    thread::thread_start_params(
                        self.mode,
                        model.as_deref(),
                        &spec.workdir,
                        &instructions,
                    ),
                    HANDSHAKE_STEP,
                )
                .await
                .ok_or(AdapterError::StartFailed("app_server_failed"))?
                .map_err(|_| AdapterError::StartFailed("thread_start_failed"))?;
            thread::parse_thread_result(&result)
                .map_err(|_| AdapterError::StartFailed("thread_start_failed"))?
        };
        let mut translator = Translator::new(&effective.thread_id, &effective.model);
        translator.set_workdir(&spec.workdir);
        self.translator = Some(translator);
        let started = Started {
            native_conversation_id: effective.thread_id.clone(),
            cli_pid: self.proc.as_ref().map(|proc| proc.pid).unwrap_or_default(),
            cli_start_ticks: self
                .proc
                .as_ref()
                .map(|proc| proc.start_ticks)
                .unwrap_or_default(),
        };
        self.effective = Some(effective);
        Ok(started)
    }

    // --- передача ---------------------------------------------------------------

    fn current_settings(&self) -> CurrentSettings {
        CurrentSettings {
            model: self
                .effective
                .as_ref()
                .map(|effective| effective.model.clone()),
            effort: self.effort.clone(),
            mode: self.mode,
        }
    }

    // Смена настроек едет оверрайдами; действующие правим сразу.
    fn apply_settings_override(&mut self, input: &Transmission) {
        let Some(change) = input.settings.as_ref() else {
            return;
        };
        if let Some(model) = change.model.clone() {
            if let Some(effective) = self.effective.as_mut() {
                effective.model = model.clone();
            }
            if let Some(translator) = self.translator.as_mut() {
                translator.set_model(&model);
            }
        }
        if let Some(effort) = change.effort.clone() {
            self.effort = Some(effort);
        }
        if let Some(mode) = change.permission_mode.as_deref()
            && let Ok(mode) = PermissionMode::parse(mode)
        {
            self.mode = mode;
            let (approval, sandbox) = thread::turn_policy(mode, &self.owner);
            if let Some(effective) = self.effective.as_mut() {
                if let Some(approval) = approval {
                    effective.approval_policy = approval;
                }
                if let Some(sandbox) = sandbox {
                    effective.sandbox = sandbox;
                }
            }
        }
    }

    async fn history_lookup(&mut self, command_id: &str) -> Option<String> {
        let thread_id = self.thread_id();
        let (id, message) = self.rpc.request(
            "thread/turns/list",
            input::history_lookup_params(&thread_id),
        );
        if !self.write_request(message).await {
            return None;
        }
        let result = self
            .wait_response(id, Instant::now() + self.deadline)
            .await?;
        let result = result.ok()?;
        input::history_contains(&result, command_id)
    }

    async fn transmit_inner(&mut self, input: &Transmission) -> TransmitOutcome {
        if self.translator.is_none() {
            return TransmitOutcome::Undetermined;
        }
        if let Err(reason) = input::validate_settings(
            input
                .settings
                .as_ref()
                .unwrap_or(&crate::adapter::SettingsChange::default()),
            &self.models,
        ) {
            return TransmitOutcome::Rejected(reason.into());
        }
        if self.outbox.active().is_some() {
            return TransmitOutcome::Rejected("turn_active".into());
        }
        if input.attempt > 1
            && let Some(turn) = self.history_lookup(&input.command_id.to_string()).await
        {
            // Ввод уже принят: не шлём второй раз, цепляемся к живому ходу.
            self.outbox.attach(&turn, input.command_id);
            if let Some(translator) = self.translator.as_mut() {
                translator.begin_turn(&turn, input.command_id);
            }
            return TransmitOutcome::Accepted(Proof {
                command_id: input.command_id,
                kind: input::PROOF_KIND_HISTORY,
            });
        }
        let note = input::threshold_note(self.limits.snapshot().weekly, &mut self.last_noted);
        let request = match input::build_turn_request(
            &self.thread_id(),
            input,
            &self.current_settings(),
            &self.owner,
            note.as_deref(),
        ) {
            Ok(request) => request,
            Err(error) => {
                return TransmitOutcome::Rejected(
                    if error.to_string().contains("input_too_long") {
                        "input_too_long"
                    } else {
                        "input_build_failed"
                    }
                    .into(),
                );
            }
        };
        let (id, message) = self.rpc.request("turn/start", request.params);
        if !self.write_request(message).await {
            return TransmitOutcome::Undetermined;
        }
        self.apply_settings_override(input);
        let limit = Instant::now() + self.deadline;
        let response = self.wait_response(id, limit).await;
        let turn_id = match response {
            Some(Ok(value)) => match value
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(Value::as_str)
            {
                Some(id) => id.to_owned(),
                None => return TransmitOutcome::Undetermined,
            },
            Some(Err(error)) => return TransmitOutcome::Rejected(input::error_rejection(&error)),
            None => return TransmitOutcome::Undetermined,
        };
        if let Some(diagnostic) = self.outbox.on_turn_response(input.command_id, &turn_id) {
            self.events.push_back(diagnostic);
            return TransmitOutcome::Undetermined;
        }
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(&turn_id, input.command_id);
        }
        self.turn_items.clear();
        self.proof_for = Some(input.command_id);
        self.proof_seen = false;
        loop {
            if self.proof_seen {
                self.proof_for = None;
                return TransmitOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind: input::PROOF_KIND_MESSAGE,
                });
            }
            if self.eof || self.exited {
                self.proof_for = None;
                return TransmitOutcome::Undetermined;
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                self.proof_for = None;
                return TransmitOutcome::Undetermined;
            }
        }
    }

    fn thread_id(&self) -> String {
        self.effective
            .as_ref()
            .map(|effective| effective.thread_id.clone())
            .unwrap_or_default()
    }
}

fn turn_error_text(kind: &str, http_status: Option<u16>) -> String {
    let name = match kind {
        "serverOverloaded" => "Сервер перегружен",
        "internalServerError" => "Внутренняя ошибка сервера",
        "responseTooManyFailedAttempts" => "Слишком много неудачных попыток",
        "httpConnectionFailed" => "Ошибка соединения",
        "badRequest" => "Некорректный запрос",
        _ => "Ошибка Codex",
    };
    match http_status {
        Some(status) => format!("{name} (HTTP {status})"),
        None => name.to_owned(),
    }
}

fn exit_event(status: std::process::ExitStatus) -> AdapterEvent {
    use std::os::unix::process::ExitStatusExt;
    AdapterEvent::ProcessExited {
        code: status.code(),
        signal: status.signal(),
    }
}

#[async_trait::async_trait]
impl Adapter for CodexAdapter {
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
            Err(version::VersionError::NotFound) => {
                return Err(AdapterError::StartFailed("cli_not_found"));
            }
            Err(version::VersionError::BelowMinimum(_)) => {
                return Err(AdapterError::StartFailed("cli_too_old"));
            }
            Err(_) => return Err(AdapterError::StartFailed("cli_version_failed")),
        }
        self.mode = PermissionMode::parse(&spec.permission_mode)
            .map_err(|_| AdapterError::StartFailed("unknown_permission_mode"))?;
        self.effort = (!spec.effort.is_empty()).then(|| spec.effort.clone());
        let broker = Broker::bind_with(&self.exec_dir, mcp::ToolSet::LimitsOnly, "codex").await?;
        self.broker = Some(broker);
        let relay = self
            .relay_executable
            .clone()
            .unwrap_or(std::env::current_exe()?);
        let launch = LaunchSpec {
            executable: &self.cfg.executable,
            exec_dir: &self.exec_dir,
            workdir: &spec.workdir,
            relay_exe: &relay,
        };
        self.proc = Some(AppServerProcess::spawn(&launch, group).map_err(|error| {
            // Мгновенно упавший сервер: запуск удался, умер сам процесс.
            if matches!(error, process::LaunchError::CliNotFoundInContainment) {
                AdapterError::StartFailed("app_server_failed")
            } else {
                AdapterError::StartFailed("cli_launch_failed")
            }
        })?);
        self.handshake(spec).await
    }

    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        input::validate_settings(
            input
                .settings
                .as_ref()
                .unwrap_or(&crate::adapter::SettingsChange::default()),
            &self.models,
        )
    }

    async fn transmit(
        &mut self,
        input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.proc.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        self.transmit_inner(input).await
    }

    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        let Some((kind, payload, item_id)) = self.requests.pending_info(request_id) else {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        };
        if !requests::check_decision(&kind, &payload, decision) {
            return DecisionOutcome::NotApplied("decision_invalid".into());
        }
        let line = match self.requests.answer(request_id, decision) {
            Ok(line) => line,
            Err(reason) => return DecisionOutcome::NotApplied(reason.into()),
        };
        if !self.write_request(line).await {
            return DecisionOutcome::Undetermined;
        }
        let decision_kind = match decision {
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                ..
            } => "allow",
            DecisionPayload::Permission { .. } => "deny",
            DecisionPayload::Question { .. } => "answer",
        };
        let limit = Instant::now() + Duration::from_secs(5);
        loop {
            if self.outbox.active().is_none() {
                return DecisionOutcome::Undetermined;
            }
            if let Some(position) = self
                .resolved
                .iter()
                .position(|resolved| matches!(resolved, Resolved::Ours { request_id: id } if id.as_str() == request_id))
            {
                self.resolved.remove(position);
                let mut payload = serde_json::Map::new();
                payload.insert("request_id".to_owned(), request_id.into());
                payload.insert("decision_kind".to_owned(), decision_kind.into());
                if let Some(status) = self.completed_status.get(item_id.as_str())
                    && let Some(status) = status
                {
                    payload.insert("item_status".to_owned(), status.clone().into());
                }
                return DecisionOutcome::Applied(DecisionProof {
                    proof_kind: "codex_server_request_resolved".into(),
                    payload: Value::Object(payload),
                });
            }
            if self.eof || self.exited {
                return DecisionOutcome::Undetermined;
            }
            self.flush_writes().await;
            if timeout_at(limit, self.pump_once()).await.is_err() {
                return DecisionOutcome::Undetermined;
            }
        }
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        let Some(active) = self.outbox.active().cloned() else {
            return InterruptOutcome::NotApplicable;
        };
        let (id, message) = self.rpc.request(
            "turn/interrupt",
            input::interrupt_params(&self.thread_id(), &active.turn_id),
        );
        if !self.write_request(message).await {
            return InterruptOutcome::Undetermined;
        }
        let acknowledged = matches!(
            self.wait_response(id, Instant::now() + input::INTERRUPT_TIMEOUT)
                .await,
            Some(Ok(_))
        );
        if !acknowledged {
            return InterruptOutcome::Undetermined;
        }
        let thread_id = self.thread_id();
        let (list_id, list_message) = self.rpc.request(
            "thread/backgroundTerminals/list",
            input::background_terminals_list_params(&thread_id),
        );
        if self.write_request(list_message).await
            && let Some(Ok(list)) = self
                .wait_response(list_id, Instant::now() + input::INTERRUPT_TIMEOUT)
                .await
        {
            for process_id in input::terminals_to_terminate(&list, &self.turn_items) {
                let (terminate_id, terminate) = self.rpc.request(
                    "thread/backgroundTerminals/terminate",
                    input::background_terminal_terminate_params(&thread_id, &process_id),
                );
                if !self.write_request(terminate).await {
                    break;
                }
                if timeout(
                    input::INTERRUPT_TIMEOUT,
                    self.wait_response(terminate_id, Instant::now() + input::INTERRUPT_TIMEOUT),
                )
                .await
                .is_err()
                {
                    self.diagnostic("interrupt_cleanup_failed", false);
                }
            }
        } else {
            self.diagnostic("interrupt_cleanup_failed", false);
        }
        self.requests.drop_all();
        InterruptOutcome::Acknowledged
    }

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            self.flush_writes().await;
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            self.pump_once().await;
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
        let Some(proc) = &mut self.proc else {
            return Ok(());
        };
        proc.close_stdin();
        let grace = Duration::from_secs(2);
        if !self.exited {
            if matches!(mode, CloseMode::Force) || timeout(grace, proc.wait()).await.is_err() {
                group.signal_member(
                    &crate::containment::Member {
                        pid: proc.pid,
                        start_ticks: proc.start_ticks,
                    },
                    rustix::process::Signal::KILL,
                )?;
                timeout(grace, proc.wait())
                    .await
                    .map_err(|_| AdapterError::Deadline)??;
            }
            self.exited = true;
        }
        Ok(())
    }
}
