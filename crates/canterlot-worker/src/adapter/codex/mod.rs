pub mod events;
pub mod home;
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
    home::{CodexHome, HomeError},
    input::{CurrentSettings, ModelEntry, Outbox, model_entries},
    limits::{CrunchContext, LimitTracker, ReachedDecision},
    process::{AppServerProcess, LaunchSpec, StdoutLine},
    requests::{Intake, Requests, Resolved},
    rpc::{Incoming, RpcClient, RpcError, RpcId},
    thread::{Effective, OwnerDefaults, PermissionMode},
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, Capabilities, CloseMode,
        CompactOutcome, DecisionOutcome, DecisionProof, InstructionMode, InstructionSpec,
        InterruptOutcome, ModelCatalog, Phase, Proof, SettingsChange, StartSpec, Started,
        SteerOutcome, Support, Transmission, TransmitOutcome, canterlot_mcp::Broker,
        canterlot_mcp::BrokerEvent, held_tool::HeldTools, reaped,
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
                ..Default::default()
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
        match probe::probe_pages(
            &cfg.executable,
            "model/list",
            input::model_list_params(None),
            Duration::from_secs(15),
            input::MODEL_LIST_MAX_PAGES,
        )
        .await
        {
            Ok(pages) => {
                return Ok(CodexCatalog {
                    version,
                    models: pages.iter().flat_map(model_entries).collect(),
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
/// Codex переносит состояние в sqlite при первом запуске на доме и не отвечает
/// на `initialize`, пока перенос идёт. Столько ждём, увидев об этом строку в stderr.
const BACKFILL_TIMEOUT: Duration = Duration::from_secs(600);
const BACKFILL_REPORT: &str = "state db backfill is running";
const BACKFILL_STUCK: &str = "timed out waiting for state db backfill";
const CRUNCH_PROBE: Duration = Duration::from_secs(15);
/// Сколько кнопочное сжатие ждёт пару `contextCompaction`, прежде чем признать отказ.
const COMPACT_TIMEOUT: Duration = Duration::from_secs(300);

type RpcResult = Result<Value, RpcError>;

pub struct CodexAdapter {
    cfg: CodexConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    /// Срок шага рукопожатия; в тестах короче, чтобы не ждать 20 с.
    handshake_step: Duration,
    /// Сколько ждать `initialize`, пока app-server переносит состояние в sqlite.
    backfill_timeout: Duration,
    proc: Option<AppServerProcess>,
    rpc: RpcClient,
    translator: Option<Translator>,
    requests: Requests,
    limits_calls: HashMap<Uuid, String>,
    outbox: Outbox,
    limits: LimitTracker,
    broker: Option<Broker>,
    events: VecDeque<AdapterEvent>,
    hold: HeldTools,
    pending_writes: VecDeque<Value>,
    effective: Option<Effective>,
    owner: OwnerDefaults,
    models: Vec<ModelEntry>,
    mode: PermissionMode,
    effort: Option<String>,
    workdir: PathBuf,
    instructions: InstructionSpec,
    codex_home: Option<CodexHome>,
    /// Делегированная сессия: дом без конфига MCP владельца, без приложений и плагинов.
    isolate_mcp: bool,
    last_noted: Option<u16>,
    turn_items: HashSet<String>,
    started_items: HashMap<String, Value>,
    completed_status: HashMap<String, Option<String>>,
    responses: HashMap<i64, RpcResult>,
    resolved: VecDeque<Resolved>,
    proof_for: Option<Uuid>,
    proof_seen: bool,
    /// Вводы с `Undetermined` по сроку: позднее доказательство уходит `InputAccepted`.
    unconfirmed: HashSet<Uuid>,
    /// turn/start без разобранного ответа: в пути или опоздал к сроку.
    pending_turn: Option<PendingTurn>,
    /// Кнопка «Сжать контекст» запустила `thread/compact/start`: ближайший эпизод ручной.
    compact_manual: bool,
    /// Команда сжатия ждёт итога парой `contextCompaction`: им она и закрывается.
    compact_pending: Option<(Uuid, Instant)>,
    /// Сжатие идёт своим ходом: команда закрывается его концом, а не элементом.
    compact_turn: bool,
    /// Итог элемента `contextCompaction`, ждущий конца хода сжатия.
    compact_done: Option<bool>,
    eof: bool,
    exited: bool,
    start_attempted: bool,
    relay_executable: Option<PathBuf>,
    crunch: Option<PendingCrunch>,
}

/// Ход, кончившийся лимитом без исчерпанного окна в снимке: свежее чтение
/// лимитов идёт отдельной задачей, конец хода ждёт его в `self`, а не на стеке
/// `next_event`, который супервизор бросает.
struct PendingCrunch {
    ctx: CrunchContext,
    command: Option<Uuid>,
    task: tokio::task::JoinHandle<Result<Value, probe::ProbeError>>,
    finish: Vec<AdapterEvent>,
}

type CrunchResult = Result<Result<Value, probe::ProbeError>, tokio::task::JoinError>;

async fn crunch_done(crunch: &mut Option<PendingCrunch>) -> CrunchResult {
    match crunch {
        Some(pending) => (&mut pending.task).await,
        None => std::future::pending().await,
    }
}

struct PendingTurn {
    rpc_id: i64,
    command_id: Uuid,
    settings: Option<SettingsChange>,
}

/// Параметры перезапуска app-server с возобновлением треда.
struct ResumeLaunch<'a> {
    mode: PermissionMode,
    launch_mode: InstructionMode,
    model: Option<&'a str>,
    dev: &'a str,
    home: Option<&'a CodexHome>,
}

impl CodexAdapter {
    pub fn new(cfg: CodexConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            handshake_step: HANDSHAKE_STEP,
            backfill_timeout: BACKFILL_TIMEOUT,
            proc: None,
            rpc: RpcClient::new(),
            translator: None,
            requests: Requests::new(),
            limits_calls: HashMap::new(),
            outbox: Outbox::new(),
            limits: LimitTracker::default(),
            broker: None,
            events: VecDeque::new(),
            hold: HeldTools::default(),
            pending_writes: VecDeque::new(),
            effective: None,
            owner: thread::owner_defaults_from(&Value::Null),
            models: Vec::new(),
            mode: PermissionMode::Owner,
            effort: None,
            workdir: PathBuf::new(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            codex_home: None,
            isolate_mcp: false,
            last_noted: None,
            turn_items: HashSet::new(),
            started_items: HashMap::new(),
            completed_status: HashMap::new(),
            responses: HashMap::new(),
            resolved: VecDeque::new(),
            proof_for: None,
            proof_seen: false,
            unconfirmed: HashSet::new(),
            pending_turn: None,
            compact_manual: false,
            compact_pending: None,
            compact_turn: false,
            compact_done: None,
            eof: false,
            exited: false,
            start_attempted: false,
            relay_executable: None,
            crunch: None,
        }
    }

    /// Сроки рукопожатия для тестов: обычный шаг и ожидание переноса sqlite.
    pub fn with_handshake_timing(
        mut self,
        handshake_step: Duration,
        backfill_timeout: Duration,
    ) -> Self {
        self.handshake_step = handshake_step;
        self.backfill_timeout = backfill_timeout;
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

    // Одно чтение stdout, событие брокера, конец свежего чтения лимитов или
    // проверка выхода. Отмена безопасна: обработка синхронна, состояние в `self`.
    async fn pump_once(&mut self) {
        if self.exited || self.proc.is_none() {
            let result = crunch_done(&mut self.crunch).await;
            self.finish_crunch(result);
            return;
        }
        let proc = self.proc.as_mut().unwrap();
        let broker = self.broker.as_mut().unwrap();
        let crunch = &mut self.crunch;
        if self.eof {
            tokio::select! {
                result = crunch_done(crunch) => self.finish_crunch(result),
                status = proc.wait() => {
                    self.exited = true;
                    self.events.push_back(status.map(exit_event).unwrap_or(AdapterEvent::ProcessExited { code: None, signal: None }));
                }
                event = broker.next() => self.on_broker(event),
            }
        } else {
            tokio::select! {
                biased;
                result = crunch_done(crunch) => self.finish_crunch(result),
                line = proc.next_line() => self.dispatch_line(line),
                event = broker.next() => self.on_broker(event),
                _ = tokio::time::sleep(Duration::from_millis(25)) => {
                    if let Some(status) = proc.try_wait().ok().flatten() {
                        self.exited = true;
                        self.events.push_back(exit_event(status));
                    }
                }
            }
        }
    }

    fn dispatch_line(&mut self, line: StdoutLine) {
        match line {
            StdoutLine::Eof => self.eof = true,
            StdoutLine::Oversized(_) => self.diagnostic("oversized_frame", false),
            StdoutLine::Line(line) => match rpc::classify(&line) {
                Incoming::Response { id, result } => {
                    if self.rpc.resolve(&id).is_some()
                        && let RpcId::Int(number) = id
                    {
                        self.responses.insert(number, result);
                    }
                }
                Incoming::Notification { method, params } => {
                    let translated = self.feed_notification(&method, &params);
                    let markable = (method == "item/started")
                        .then(|| params.get("item"))
                        .flatten()
                        .filter(|item| {
                            matches!(
                                item.get("type").and_then(Value::as_str),
                                Some("commandExecution" | "fileChange")
                            )
                        })
                        .and_then(|item| item.get("id").and_then(Value::as_str))
                        .map(str::to_owned);
                    for item in translated {
                        match (&markable, item) {
                            (
                                Some(item_id),
                                Translated::Event(
                                    event @ AdapterEvent::BlockStarted {
                                        kind: BlockKind::ToolCall,
                                        ..
                                    },
                                ),
                            ) => self.hold_tool(item_id.clone(), event),
                            (_, item) => self.on_translated(item),
                        }
                    }
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
            && let Some(item) = params.get("item")
            && let Some(id) = item.get("id").and_then(Value::as_str)
        {
            if self.outbox.active().is_some_and(|active| {
                params.get("turnId").and_then(Value::as_str) == Some(active.turn_id.as_str())
            }) {
                self.turn_items.insert(id.to_owned());
            }
            // Правка любого треда (и субагента): карточке разрешения нужен её diff.
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

    fn hold_tool(&mut self, item_id: String, event: AdapterEvent) {
        let AdapterEvent::BlockStarted { block_id, .. } = &event else {
            return;
        };
        let block_id = *block_id;
        // Без возможной пометки ждать нечего: тихая команда видна сразу.
        if self.cli_mark_for(&item_id).is_none() {
            self.events.push_back(event);
            return;
        }
        self.hold.hold(item_id, block_id, event);
    }

    /// Пометка по действующей политике, если по элементу не было запроса владельцу.
    fn cli_mark_for(&self, item_id: &str) -> Option<&'static str> {
        if self
            .translator
            .as_ref()
            .is_some_and(|translator| translator.approval_noted(item_id))
        {
            return None;
        }
        self.effective.as_ref().and_then(|effective| {
            requests::cli_policy_mark(&effective.approval_policy, &effective.sandbox)
        })
    }

    fn release_item(&mut self, item_id: &str, mark: Option<&'static str>) {
        self.hold.release(item_id, mark, &mut self.events);
    }

    fn on_server_request(&mut self, id: RpcId, method: &str, params: &Value) {
        // Запрос разрешения по элементу: решает владелец, пометки CLI не будет.
        if method.ends_with("/requestApproval")
            && let Some(item_id) = params.get("itemId").and_then(Value::as_str)
        {
            if let Some(translator) = self.translator.as_mut() {
                translator.note_approval(item_id);
            }
            self.release_item(item_id, None);
        }
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
            // Вне хода супервизор карточку не откроет, а фаза ожидания держала бы
            // очередь хаба: сервер сразу получает отказ.
            Intake::Open(AdapterEvent::RequestOpened { request_id, .. })
                if !self.turn_running() =>
            {
                self.pending_writes
                    .extend(self.requests.refuse(&request_id));
                self.diagnostic("request_outside_turn", false);
            }
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

    fn on_translated(&mut self, translated: Translated) {
        match translated {
            Translated::TurnCompleted {
                turn_id,
                status,
                error,
            } => self.on_turn_completed(turn_id, status, error),
            Translated::UntrackedTurnEnded => {
                if !self.turn_running() {
                    self.events.push_back(AdapterEvent::Phase {
                        phase: Phase::AwaitingInput,
                        tool_name: None,
                        summary: None,
                    });
                }
                // Ход сжатия кончился: без завершения элемента сжатие не удалось.
                if self.compact_pending.take().is_some() {
                    let ok = self.compact_done.take().unwrap_or(false);
                    self.compact_turn = false;
                    self.compact_manual = false;
                    self.events.push_back(AdapterEvent::Compacted { ok });
                }
            }
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
                    Some(Resolved::Cleared { request_id }) if self.outbox.active().is_some() => {
                        self.events.push_back(AdapterEvent::RequestClosed {
                            request_id,
                            reason: "cli_cancelled".into(),
                        });
                    }
                    Some(Resolved::Cleared { .. }) | None => {}
                }
            }
            Translated::ItemCompleted {
                item_id, status, ..
            } => {
                let mark = self.cli_mark_for(&item_id);
                self.completed_status.insert(item_id.clone(), status);
                self.release_item(&item_id, mark);
            }
            Translated::SettingsUpdated(settings) => {
                if let Some(effective) = self.effective.as_mut() {
                    if let Some(policy) = settings.get("approvalPolicy") {
                        effective.approval_policy = policy.clone();
                    }
                    // ThreadSettings называет песочницу sandboxPolicy (ev/question.jsonl).
                    if let Some(sandbox) = settings
                        .get("sandboxPolicy")
                        .or_else(|| settings.get("sandbox"))
                    {
                        effective.sandbox = sandbox.clone();
                    }
                    if let Some(model) = settings.get("model").and_then(Value::as_str) {
                        effective.model = model.to_owned();
                    }
                }
            }
            Translated::UserMessage { client_id, .. } => {
                let command = client_id.and_then(|id| Uuid::parse_str(&id).ok());
                if command.is_some() && self.proof_for == command {
                    self.proof_for = None;
                    self.proof_seen = true;
                } else if let Some(command_id) = command
                    && self.unconfirmed.remove(&command_id)
                {
                    self.events
                        .push_back(AdapterEvent::InputAccepted { command_id });
                }
            }
            Translated::TurnStarted { turn_id } => {
                if self.compact_pending.is_some() && !self.turn_running() {
                    self.compact_turn = true;
                }
                self.outbox.on_turn_started(&turn_id);
                // Ответ turn/start ещё не разобран, а элементы хода уже идут.
                if self.outbox.active().is_none()
                    && let Some(command_id) = self.pending_turn.as_ref().map(|t| t.command_id)
                {
                    self.outbox.adopt(&turn_id, command_id);
                    self.begin_turn(&turn_id, command_id);
                }
            }
            Translated::Event(event) => self.on_block_event(event),
            Translated::McpStatus { .. } | Translated::Ignored => {}
        }
    }

    // Начало результата у отложенного вызова — команда уже выполняется без
    // запроса: вызов отпускается с пометкой, и вывод идёт вживую. Прочие события
    // отложенного блока ждут вместе с ним.
    fn on_block_event(&mut self, event: AdapterEvent) {
        // Кнопочное сжатие Codex идёт методом `thread/compact/start`: кадры триггера не
        // несут, ручным эпизод помечает флаг запроса. Итог закрывает и команду хаба.
        let mut complete_compact: Option<bool> = None;
        let event = match event {
            AdapterEvent::CompactionStarted { manual: false } if self.compact_manual => {
                AdapterEvent::CompactionStarted { manual: true }
            }
            AdapterEvent::CompactionFinished {
                manual: false,
                ok,
                pre_tokens,
                post_tokens,
            } if self.compact_manual => {
                self.compact_manual = false;
                complete_compact = self.compact_item_done(ok);
                AdapterEvent::CompactionFinished {
                    manual: true,
                    ok,
                    pre_tokens,
                    post_tokens,
                }
            }
            AdapterEvent::CompactionFinished { ok, .. } => {
                self.compact_manual = false;
                complete_compact = self.compact_item_done(ok);
                event
            }
            _ => event,
        };
        if let AdapterEvent::BlockStarted {
            kind: BlockKind::ToolResult,
            parent: Some(parent),
            ..
        } = &event
            && let Some(item_id) = self.hold.item_of_block(*parent).map(str::to_owned)
        {
            let mark = self.cli_mark_for(&item_id);
            self.release_item(&item_id, mark);
        }
        if let Some(event) = self.hold.capture(event) {
            self.events.push_back(event);
        }
        if let Some(ok) = complete_compact {
            self.events.push_back(AdapterEvent::Compacted { ok });
        }
    }

    fn on_turn_completed(
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
        if let Some(command) = command {
            self.unconfirmed.remove(&command);
        }
        // Отложенные вызовы уходят без пометки: ход кончился без их завершения.
        self.hold.release_all(&mut self.events);
        // turn_items живут до следующего хода: при прерывании конец хода приходит
        // раньше списка терминалов (ev/interrupt_cleanup.jsonl:34-37).
        self.completed_status.clear();
        if !was_active {
            return;
        }
        self.pending_writes.extend(self.requests.refuse_subagents());
        let mut finish = Vec::new();
        if let Some(translator) = self.translator.as_mut() {
            finish.push(translator.finish_turn());
        }
        finish.push(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
        match (status, error) {
            (TurnStatus::Interrupted, _) => {}
            (_, None) if status != TurnStatus::Failed => {}
            (_, Some(error)) => {
                if let Some(ctx) = self.on_turn_error(error, command) {
                    self.start_fresh_read(ctx, command, finish);
                    return;
                }
            }
            (_, None) => self.error_block("Ошибка Codex".into(), command),
        }
        self.events.extend(finish);
    }

    // `Some` — нужен свежий снимок лимитов до конца хода.
    fn on_turn_error(
        &mut self,
        error: TurnErrorInfo,
        command: Option<Uuid>,
    ) -> Option<CrunchContext> {
        match error.kind.as_str() {
            "unauthorized" => {
                self.error_block(
                    "Codex требует входа: выполните codex login на машине".into(),
                    command,
                );
                self.diagnostic("auth_required", false);
            }
            "contextWindowExceeded" => {}
            "usageLimitExceeded" | "rateLimitExceeded" => {
                let decision = self.limits.on_turn_failed(
                    &limits::TurnErrorInfo {
                        codex_error_info: Some(error.kind.clone()),
                    },
                    limits::now_ms(),
                );
                let usage = error.kind == "usageLimitExceeded";
                let fresh = match decision {
                    ReachedDecision::Reached(fact) => {
                        self.events.push_back(AdapterEvent::Limit(fact));
                        None
                    }
                    ReachedDecision::NeedFreshRead(ctx) => Some(ctx),
                    ReachedDecision::NotALimit => None,
                };
                if usage {
                    self.error_block("Исчерпан лимит Codex".into(), command);
                }
                return fresh;
            }
            "other" if error.message.contains("model_not_found") => {
                self.error_block("Модель не найдена".into(), command);
            }
            kind => {
                self.error_block(turn_error_text(kind, error.http_status), command);
            }
        }
        None
    }

    // Свежие окна без запроса к модели: короткий probe account/rateLimits/read
    // отдельной задачей; конец хода (`finish`) ждёт его завершения.
    fn start_fresh_read(
        &mut self,
        ctx: CrunchContext,
        command: Option<Uuid>,
        finish: Vec<AdapterEvent>,
    ) {
        let executable = self.cfg.executable.clone();
        let task = tokio::spawn(async move {
            probe::probe(
                &executable,
                "account/rateLimits/read",
                json!({}),
                CRUNCH_PROBE,
            )
            .await
        });
        self.crunch = Some(PendingCrunch {
            ctx,
            command,
            task,
            finish,
        });
    }

    fn finish_crunch(&mut self, result: CrunchResult) {
        let Some(pending) = self.crunch.take() else {
            return;
        };
        let facts = match result {
            Ok(Ok(response)) => self
                .limits
                .on_fresh_read(&response, pending.ctx, limits::now_ms()),
            _ => self.limits.on_read_failed(pending.ctx, limits::now_ms()),
        };
        let reached = facts
            .iter()
            .any(|fact| matches!(fact, crate::adapter::LimitFact::Reached { .. }));
        self.events
            .extend(facts.into_iter().map(AdapterEvent::Limit));
        if pending.ctx == CrunchContext::RateLimitExceeded && !reached {
            self.error_block("Codex ограничил частоту запросов".into(), pending.command);
        }
        self.events.extend(pending.finish);
    }

    fn on_broker(&mut self, event: BrokerEvent) {
        if let BrokerEvent::AgentTool {
            call_id,
            tool,
            arguments,
            ..
        } = event
        {
            // Вызов пресетов уходит в хаб; итог вернётся через agent_call_result.
            self.events.push_back(AdapterEvent::AgentCall {
                call_id,
                tool: tool.to_owned(),
                arguments,
            });
            return;
        }
        if let BrokerEvent::ShareFile { call_id, arguments } = event {
            self.events
                .push_back(AdapterEvent::ShareFile { call_id, arguments });
            return;
        }
        let BrokerEvent::LimitsQuery { call_id } = event else {
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
                    window: window.into(),
                    used_permille: used.into(),
                    resets_at_ms: reset.unwrap_or_default(),
                    ..Default::default()
                },
            )
        })
        .collect();
        let text = crate::adapter::canterlot_mcp::mcp::limits_tool_text(
            &canterlot_proto::worker::v1::LimitsSnapshot {
                provider: "codex".into(),
                current,
                ..Default::default()
            },
            "codex",
        );
        self.limits_calls.insert(call_id, text);
        self.events.push_back(AdapterEvent::Limits { call_id });
    }

    // --- RPC поверх насоса ----------------------------------------------------

    async fn write_request(&mut self, message: Value) -> bool {
        let Some(proc) = self.proc.as_mut() else {
            return false;
        };
        match timeout(self.deadline, proc.write_message(&message)).await {
            Ok(written) => written.is_ok(),
            // Сервер не читает stdin, а цикл супервизора ждёт эту запись: сервер
            // снимается, и исполнение получает ProcessExited.
            Err(_) => {
                signal_group(proc.pid, rustix::process::Signal::KILL);
                false
            }
        }
    }

    async fn wait_response(&mut self, id: i64, until: Instant) -> Option<RpcResult> {
        loop {
            if let Some(result) = self.responses.remove(&id) {
                return Some(result);
            }
            if self.eof || self.exited {
                return None;
            }
            if timeout_at(until, self.flush_writes()).await.is_err()
                || timeout_at(until, self.pump_once()).await.is_err()
            {
                // Опоздавший ответ turn/start ещё нужен settle_late_turn.
                if self
                    .pending_turn
                    .as_ref()
                    .is_none_or(|turn| turn.rpc_id != id)
                {
                    self.rpc.forget(id);
                }
                return self.responses.remove(&id);
            }
        }
    }

    /// `initialize` ждёт дольше обычного шага, пока app-server переносит состояние
    /// в sqlite: в это время он не читает запросы, а убитый перенос оставляет
    /// в базе статус `running`, после которого Codex больше не запускается.
    async fn wait_initialize(&mut self, id: i64) -> Option<RpcResult> {
        let mut until = Instant::now() + self.handshake_step;
        let mut waiting_backfill = false;
        loop {
            if let Some(result) = self.responses.remove(&id) {
                return Some(result);
            }
            if self.eof || self.exited {
                return None;
            }
            if !waiting_backfill && self.stderr_text().contains(BACKFILL_REPORT) {
                waiting_backfill = true;
                until = Instant::now() + self.backfill_timeout;
                self.diagnostic("codex_state_backfill", false);
            }
            if timeout_at(until, self.flush_writes()).await.is_err()
                || timeout_at(until, self.pump_once()).await.is_err()
            {
                self.rpc.forget(id);
                return self.responses.remove(&id);
            }
        }
    }

    fn stderr_text(&self) -> String {
        self.proc
            .as_ref()
            .map(|proc| String::from_utf8_lossy(&proc.stderr_tail()).into_owned())
            .unwrap_or_default()
    }

    /// Отказ рукопожатия: отдельно узнаём застрявший перенос sqlite, чтобы владелец
    /// получил причину и действие, а не общее `app_server_failed`.
    async fn start_failure(&mut self, fallback: &'static str) -> AdapterError {
        let mut tail = self.stderr_text();
        // Читатель stderr — отдельная задача: после выхода процесса даём ей дочитать.
        for _ in 0..50 {
            if tail.contains(BACKFILL_STUCK) || !(self.eof || self.exited) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            tail = self.stderr_text();
        }
        if tail.contains(BACKFILL_STUCK) {
            AdapterError::StartFailed("codex_state_backfill_stuck")
        } else {
            AdapterError::StartFailed(fallback)
        }
    }

    // Строка уходит в буфер процесса синхронно, до первой точки ожидания:
    // отмена посреди записи её не теряет и не рвёт (дописывается следующим вызовом).
    async fn flush_writes(&mut self) {
        let Some(proc) = self.proc.as_mut() else {
            return;
        };
        if proc.flush_out().await.is_err() {
            return;
        }
        while let Some(line) = self.pending_writes.pop_front() {
            if proc.write_message(&line).await.is_err() {
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

    /// Базовое рукопожатие: initialize, account/read, config/read. Дом и модели — отдельно.
    async fn handshake_base(&mut self) -> Result<(), AdapterError> {
        let worker_version = env!("CARGO_PKG_VERSION");
        let (id, message) = self
            .rpc
            .request("initialize", thread::initialize_params(worker_version));
        if !self.write_request(message).await {
            return Err(self.start_failure("app_server_failed").await);
        }
        if self.wait_initialize(id).await.is_none() {
            return Err(self.start_failure("app_server_failed").await);
        }
        if !self
            .write_request(RpcClient::notification("initialized", json!({})))
            .await
        {
            return Err(self.start_failure("app_server_failed").await);
        }
        let account = self
            .rpc_round(
                "account/read",
                thread::account_read_params(),
                self.handshake_step,
            )
            .await
            .ok_or(AdapterError::StartFailed("app_server_failed"))?
            .map_err(|_| AdapterError::StartFailed("app_server_failed"))?;
        thread::check_account(&account).map_err(|_| AdapterError::StartFailed("auth_required"))?;
        let config = self
            .rpc_round(
                "config/read",
                thread::config_read_params(&self.workdir),
                self.handshake_step,
            )
            .await
            .ok_or(AdapterError::StartFailed("app_server_failed"))?
            .map_err(|_| AdapterError::StartFailed("app_server_failed"))?;
        if self.isolate_mcp && !mcp::isolated_config(&config) {
            return Err(AdapterError::StartFailed("mcp_isolation_failed"));
        }
        self.owner.reread(&config);
        Ok(())
    }

    /// Все страницы model/list на соединении сессии.
    async fn list_models(&mut self) -> Result<Vec<ModelEntry>, AdapterError> {
        let mut models = Vec::new();
        let mut cursor = None;
        for _ in 0..input::MODEL_LIST_MAX_PAGES {
            let page = self
                .rpc_round(
                    "model/list",
                    input::model_list_params(cursor.as_deref()),
                    self.handshake_step,
                )
                .await
                .ok_or(AdapterError::StartFailed("model_list_failed"))?
                .map_err(|_| AdapterError::StartFailed("model_list_failed"))?;
            models.extend(model_entries(&page));
            cursor = input::next_cursor(&page);
            if cursor.is_none() {
                break;
            }
        }
        Ok(models)
    }

    async fn handshake(&mut self, spec: &StartSpec) -> Result<Started, AdapterError> {
        self.workdir = spec.workdir.clone();
        self.handshake_base().await?;
        self.models = self.list_models().await?;
        self.workdir = spec.workdir.clone();
        self.instructions = spec.instructions.clone();
        let instructions = thread::compose_developer_instructions(
            &spec.instructions,
            &self.owner,
            mcp::SERVER_INSTRUCTIONS,
            thread::ChangeNote::None,
        );
        let model = (!spec.model.is_empty()).then(|| spec.model.clone());
        let effective = if let Some(native) = spec.native_conversation_id.as_deref() {
            let launch = ResumeLaunch {
                mode: self.mode,
                launch_mode: spec.instructions.mode,
                model: model.as_deref(),
                dev: &instructions,
                home: None,
            };
            self.resume_thread(native, launch).await?
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
                    self.handshake_step,
                )
                .await
                .ok_or(AdapterError::StartFailed("app_server_failed"))?
                .map_err(|_| AdapterError::StartFailed("thread_start_failed"))?;
            let effective = thread::parse_thread_result(&result)
                .map_err(|_| AdapterError::StartFailed("thread_start_failed"))?;
            // Старт без полей политики показывает действующую политику владельца.
            if self.mode == PermissionMode::Owner {
                self.owner
                    .observe_thread(&effective.approval_policy, &effective.sandbox);
            }
            effective
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

    // --- передача ---------------------------------------------------------------

    // --- перезапуск между ходами --------------------------------------------

    /// Остановить старый app-server: stdin EOF, до 5 с ожидания с чтением stdout,
    /// затем SIGTERM группе и до 2 с, затем SIGKILL. Никогда не трогает сдерживание.
    async fn stop_old_server(&mut self) {
        let Some(mut proc) = self.proc.take() else {
            return;
        };
        proc.close_stdin();
        if !drain_until_exit(&mut proc, self, Duration::from_secs(5)).await {
            signal_group(proc.pid, rustix::process::Signal::TERM);
            if !drain_until_exit(&mut proc, self, Duration::from_secs(2)).await {
                signal_group(proc.pid, rustix::process::Signal::KILL);
                // Stdout может держать процесс вне группы: EOF тогда не придёт.
                let _ = timeout(Duration::from_secs(2), drain_to_eof(&mut proc, self)).await;
            }
        }
        self.eof = false;
        self.exited = false;
    }

    /// Запустить app-server для перезапуска: мгновенно упавший процесс —
    /// неудача запуска (init приходит только после первого ввода).
    async fn spawn_for_restart(
        &mut self,
        mode: InstructionMode,
        home: Option<&CodexHome>,
        group: &dyn Containment,
    ) -> Option<AppServerProcess> {
        if version::check(&self.cfg.executable).await.is_err() {
            return None;
        }
        let relay = self
            .relay_executable
            .clone()
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_default());
        let workdir = self.workdir.clone();
        let exec_dir = self.exec_dir.clone();
        let launch = LaunchSpec {
            executable: &self.cfg.executable,
            exec_dir: &exec_dir,
            workdir: &workdir,
            relay_exe: &relay,
            relay_secret: self.broker.as_ref().map(Broker::relay_secret),
        };
        let mut proc = AppServerProcess::spawn(&launch, group, mode, home).ok()?;
        if exited_early(&mut proc).await {
            return None;
        }
        Some(proc)
    }

    /// thread/resume с новыми инструкциями. Рулоны всех домов лежат в одном
    /// `sessions` (см. `CodexHome::prepare`): параметр `path` в 0.164 не помогает.
    async fn resume_thread(
        &mut self,
        thread_id: &str,
        launch: ResumeLaunch<'_>,
    ) -> Result<thread::Effective, AdapterError> {
        let workdir = self.workdir.clone();
        let params = thread::thread_resume_params(
            thread_id,
            launch.mode,
            launch.model,
            &workdir,
            launch.dev,
        );
        let result = self
            .rpc_round("thread/resume", params, self.handshake_step)
            .await
            .ok_or(AdapterError::StartFailed("app_server_failed"))?
            .map_err(|_| AdapterError::StartFailed("resume_failed"))?;
        thread::parse_thread_result(&result).map_err(|_| AdapterError::StartFailed("resume_failed"))
    }

    /// Перезапустить app-server между ходами с новыми инструкциями.
    async fn restart_for_instructions(
        &mut self,
        change: &SettingsChange,
        group: &dyn Containment,
    ) -> Result<(), &'static str> {
        let next = change
            .instructions
            .clone()
            .ok_or("instructions_change_failed")?;
        let model = change.model.clone().or_else(|| {
            self.effective
                .as_ref()
                .map(|effective| effective.model.clone())
        });
        let mode = change
            .permission_mode
            .as_deref()
            .and_then(|mode| PermissionMode::parse(mode).ok())
            .unwrap_or(self.mode);
        let effort = change.effort.clone().or_else(|| self.effort.clone());
        let new_home = if self.wants_home(next.mode) {
            Some(self.prepare_home(next.mode)?)
        } else {
            None
        };
        let note = match next.mode {
            InstructionMode::Preset => thread::ChangeNote::To {
                title: next.title.as_str(),
                revision: next.revision,
            },
            InstructionMode::Clean => thread::ChangeNote::ToClean,
            InstructionMode::Owner => thread::ChangeNote::ToOwner,
        };
        let dev = thread::compose_developer_instructions(
            &next,
            &self.owner,
            mcp::SERVER_INSTRUCTIONS,
            note,
        );
        self.discard_server().await;
        let launch = ResumeLaunch {
            mode,
            launch_mode: next.mode,
            model: model.as_deref(),
            dev: &dev,
            home: new_home.as_ref(),
        };
        match self.spawn_and_resume(launch, group).await {
            Ok(effective) => {
                let started = (
                    self.proc.as_ref().map(|proc| proc.pid).unwrap_or_default(),
                    self.proc
                        .as_ref()
                        .map(|proc| proc.start_ticks)
                        .unwrap_or_default(),
                );
                self.effective = Some(effective);
                self.mode = mode;
                self.effort = effort;
                self.instructions = next;
                self.codex_home = new_home;
                self.events.push_back(AdapterEvent::CliRestarted {
                    cli_pid: started.0,
                    cli_start_ticks: started.1,
                });
                Ok(())
            }
            Err(_) => {
                // Упавший или зависший новый сервер уходит до запасного запуска:
                // иначе он работает рядом, а его eof/exited ломают рукопожатие.
                self.discard_server().await;
                self.respawn_previous(group).await
            }
        }
    }

    /// Остановить текущий app-server и забыть состояние его соединения.
    async fn discard_server(&mut self) {
        self.stop_old_server().await;
        if let Some(translator) = self.translator.as_mut() {
            self.events.extend(translator.abandon_children());
        }
        self.requests.next_generation();
        self.rpc = RpcClient::new();
        self.responses.clear();
        self.pending_writes.clear();
        self.unconfirmed.clear();
        self.pending_turn = None;
    }

    async fn spawn_and_resume(
        &mut self,
        launch: ResumeLaunch<'_>,
        group: &dyn Containment,
    ) -> Result<thread::Effective, AdapterError> {
        let proc = match self
            .spawn_for_restart(launch.launch_mode, launch.home, group)
            .await
        {
            Some(proc) => proc,
            None => return Err(AdapterError::StartFailed("app_server_failed")),
        };
        self.proc = Some(proc);
        self.handshake_base().await?;
        let thread_id = self.thread_id();
        self.resume_thread(&thread_id, launch).await
    }

    /// Запасной запуск со старыми инструкциями и домом после неудачного
    /// перезапуска. Успех — отказ входа, провал — остановка исполнения.
    async fn respawn_previous(&mut self, group: &dyn Containment) -> Result<(), &'static str> {
        let previous = self.instructions.clone();
        let previous_home = if !self.wants_home(previous.mode) {
            None
        } else {
            match self.prepare_home(previous.mode) {
                Ok(home) => Some(home),
                // Сервера уже нет: без этого события сессия выглядела бы живой.
                Err(_) => {
                    self.events.push_back(AdapterEvent::ProcessExited {
                        code: None,
                        signal: None,
                    });
                    return Err("instructions_change_failed");
                }
            }
        };
        let mode = self.mode;
        let model = self
            .effective
            .as_ref()
            .map(|effective| effective.model.clone());
        let dev = thread::compose_developer_instructions(
            &previous,
            &self.owner,
            mcp::SERVER_INSTRUCTIONS,
            thread::ChangeNote::None,
        );
        let launch = ResumeLaunch {
            mode,
            launch_mode: previous.mode,
            model: model.as_deref(),
            dev: &dev,
            home: previous_home.as_ref(),
        };
        match self.spawn_and_resume(launch, group).await {
            Ok(effective) => {
                let started = (
                    self.proc.as_ref().map(|proc| proc.pid).unwrap_or_default(),
                    self.proc
                        .as_ref()
                        .map(|proc| proc.start_ticks)
                        .unwrap_or_default(),
                );
                self.effective = Some(effective);
                self.codex_home = previous_home;
                self.events.push_back(AdapterEvent::CliRestarted {
                    cli_pid: started.0,
                    cli_start_ticks: started.1,
                });
                Err("instructions_change_failed")
            }
            Err(_) => {
                self.discard_server().await;
                self.events.push_back(AdapterEvent::ProcessExited {
                    code: None,
                    signal: None,
                });
                Err("instructions_change_failed")
            }
        }
    }

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

    /// Свой дом нужен всем режимам, кроме «Как у владельца»; делегированной сессии нужен и ему.
    fn wants_home(&self, mode: InstructionMode) -> bool {
        mode != InstructionMode::Owner || self.isolate_mcp
    }

    /// Дом canterlot для запуска (релинк при каждом запуске и перезапуске).
    fn prepare_home(&mut self, mode: InstructionMode) -> Result<CodexHome, &'static str> {
        let home_error = |error| match error {
            HomeError::Unusable => "codex_home_unusable",
            HomeError::Io(_) => "codex_home_failed",
        };
        let mut home = CodexHome::resolve(&self.cfg, &self.exec_dir, &owner_env(&self.cfg))
            .map_err(home_error)?;
        if self.isolate_mcp {
            home = home.isolated(mode == InstructionMode::Owner);
        }
        let report = home
            .prepare(mcp::SERVER_INSTRUCTIONS, self.cfg.home_policy)
            .map_err(home_error)?;
        // Настоящий файл вместо ссылки перестал следовать за настройками владельца.
        if !report.conflicts.is_empty() {
            self.events.push_back(AdapterEvent::Diagnostic {
                kind: "codex_home_conflict",
                event_type: "codex_home_conflict".into(),
                required: false,
                field_names: report.conflicts,
            });
        }
        Ok(home)
    }

    fn turn_running(&self) -> bool {
        self.outbox.active().is_some() || self.pending_turn.is_some()
    }

    /// Элемент сжатия завершён. Если сжатие идёт своим ходом, команда ждёт его конца:
    /// иначе ввод владельца ушёл бы в ещё идущий ход сжатия.
    fn compact_item_done(&mut self, ok: bool) -> Option<bool> {
        self.compact_pending?;
        if self.compact_turn {
            self.compact_done = Some(ok);
            return None;
        }
        self.compact_pending = None;
        Some(ok)
    }

    /// Просроченное сжатие — отказ: пара `contextCompaction` так и не пришла,
    /// висеть вечно нельзя, иначе команда хаба останется открытой навсегда.
    /// Эпизод, уже закрытый элементом, второй раз не закрывается.
    fn compact_timed_out(&mut self) {
        if self
            .compact_pending
            .is_some_and(|(_, until)| Instant::now() >= until)
        {
            self.compact_pending = None;
            self.compact_manual = false;
            self.compact_turn = false;
            let done = self.compact_done.take();
            self.events.push_back(AdapterEvent::Compacted {
                ok: done.unwrap_or(false),
            });
            if done.is_none() {
                self.events.push_back(AdapterEvent::CompactionFinished {
                    manual: true,
                    ok: false,
                    pre_tokens: 0,
                    post_tokens: 0,
                });
            }
        }
    }

    fn begin_turn(&mut self, turn_id: &str, command_id: Uuid) {
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_turn(turn_id, command_id);
        }
        self.turn_items.clear();
    }

    // Ответ turn/start после срока: ввод уже назван неподтверждённым. Ход принимается,
    // а отказ освобождает исполнение: иначе конца хода никто не дождётся.
    fn settle_late_turn(&mut self) {
        let Some(rpc_id) = self.pending_turn.as_ref().map(|turn| turn.rpc_id) else {
            return;
        };
        let Some(result) = self.responses.remove(&rpc_id) else {
            if self.outbox.adopted_turn_ended() {
                self.rpc.forget(rpc_id);
                self.pending_turn = None;
            }
            return;
        };
        let Some(pending) = self.pending_turn.take() else {
            return;
        };
        let accepted = result.as_ref().ok().map(input::started_turn_id);
        match accepted {
            Some(Some(turn_id)) => {
                self.apply_settings_override(pending.settings.as_ref());
                if self.outbox.take_adopted(&turn_id) {
                    return;
                }
                match self.outbox.on_turn_response(pending.command_id, &turn_id) {
                    Some(diagnostic) => self.events.push_back(diagnostic),
                    None => self.begin_turn(&turn_id, pending.command_id),
                }
            }
            _ => {
                self.unconfirmed.remove(&pending.command_id);
                self.finish_without_turn(pending.command_id);
            }
        }
    }

    // Ввод закрыт без хода: конца хода от сервера не будет.
    fn finish_without_turn(&mut self, command_id: Uuid) {
        self.events.push_back(AdapterEvent::RunFinished {
            input_command_id: command_id,
            usage: Vec::new(),
            context_tokens: 0,
            context_window_tokens: 0,
        });
        self.events.push_back(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
    }

    // Смена настроек едет оверрайдами; действующие правим сразу.
    fn apply_settings_override(&mut self, change: Option<&SettingsChange>) {
        let Some(change) = change else {
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

    // `Err` — истории нет: это не значит, что ввода в ней нет.
    async fn history_lookup(
        &mut self,
        command_id: &str,
    ) -> Result<Option<(String, Option<String>)>, ()> {
        let thread_id = self.thread_id();
        let (id, message) = self.rpc.request(
            "thread/turns/list",
            input::history_lookup_params(&thread_id),
        );
        if !self.write_request(message).await {
            return Err(());
        }
        let result = self
            .wait_response(id, Instant::now() + self.deadline)
            .await
            .ok_or(())?
            .map_err(|_| ())?;
        Ok(input::history_turn(&result, command_id))
    }

    async fn transmit_inner(
        &mut self,
        input: &Transmission,
        group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.translator.is_none() {
            return TransmitOutcome::Undetermined;
        }
        if let Err(reason) = input::validate_settings(
            input
                .settings
                .as_ref()
                .unwrap_or(&crate::adapter::SettingsChange::default()),
            &self.models,
            self.effective
                .as_ref()
                .map(|effective| effective.model.as_str()),
        ) {
            return TransmitOutcome::Rejected(reason.into());
        }
        // Смена инструкций — перезапуск app-server между ходами, до записи ввода.
        if let Some(change) = input.settings.clone()
            && change
                .instructions
                .as_ref()
                .is_some_and(|next| !next.same_effect(&self.instructions))
        {
            if self.turn_running() {
                return TransmitOutcome::Rejected("turn_active".into());
            }
            if let Err(reason) = self.restart_for_instructions(&change, group).await {
                return TransmitOutcome::Rejected(reason.into());
            }
        }
        if self.turn_running() {
            return TransmitOutcome::Rejected("turn_active".into());
        }
        if input.attempt > 1 {
            let Ok(found) = self.history_lookup(&input.command_id.to_string()).await else {
                // Codex не дедуплицирует clientUserMessageId: повтор вслепую дал бы
                // второй ход. Исход решает владелец.
                self.finish_without_turn(input.command_id);
                return TransmitOutcome::Undetermined;
            };
            if let Some((turn, status)) = found {
                // Ввод уже принят: второй раз не шлём. Цепляемся, только если ход
                // идёт в этом процессе; иначе (ход завершён или остался от убитого
                // процесса) turn/completed не придёт, и ход закрывается сразу.
                if status.as_deref() == Some("inProgress") && self.outbox.knows(&turn) {
                    self.outbox.attach(&turn, input.command_id);
                    self.begin_turn(&turn, input.command_id);
                } else {
                    self.finish_without_turn(input.command_id);
                }
                return TransmitOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind: input::PROOF_KIND_HISTORY,
                });
            }
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
        // До записи: turn/started и userMessage могут обогнать ответ.
        self.proof_for = Some(input.command_id);
        self.proof_seen = false;
        self.pending_turn = Some(PendingTurn {
            rpc_id: id,
            command_id: input.command_id,
            settings: input.settings.clone(),
        });
        if !self.write_request(message).await {
            self.proof_for = None;
            self.pending_turn = None;
            return TransmitOutcome::Undetermined;
        }
        let limit = Instant::now() + self.deadline;
        let Some(response) = self.wait_response(id, limit).await else {
            // pending_turn остаётся: поздний ответ разберёт settle_late_turn.
            self.proof_for = None;
            if self.proof_seen {
                return TransmitOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind: input::PROOF_KIND_MESSAGE,
                });
            }
            self.unconfirmed.insert(input.command_id);
            return TransmitOutcome::Undetermined;
        };
        self.pending_turn = None;
        // Отказ сервера оставляет прежние модель, усилие и режим.
        if response.is_ok() {
            self.apply_settings_override(input.settings.as_ref());
        }
        let turn_id = match response {
            Ok(value) => match input::started_turn_id(&value) {
                Some(id) => id,
                None => {
                    self.proof_for = None;
                    return TransmitOutcome::Undetermined;
                }
            },
            Err(error) => {
                self.proof_for = None;
                return TransmitOutcome::Rejected(input::error_rejection(&error));
            }
        };
        if !self.outbox.take_adopted(&turn_id) {
            if let Some(diagnostic) = self.outbox.on_turn_response(input.command_id, &turn_id) {
                self.proof_for = None;
                self.events.push_back(diagnostic);
                return TransmitOutcome::Undetermined;
            }
            self.begin_turn(&turn_id, input.command_id);
        }
        loop {
            if self.proof_seen {
                self.proof_for = None;
                return TransmitOutcome::Accepted(Proof {
                    command_id: input.command_id,
                    kind: input::PROOF_KIND_MESSAGE,
                });
            }
            // Ход кончился без userMessage (например, блокирующий хук): ждать нечего.
            if self.eof || self.exited || self.outbox.active().is_none() {
                self.proof_for = None;
                return TransmitOutcome::Undetermined;
            }
            if timeout_at(limit, self.pump_once()).await.is_err() {
                self.proof_for = None;
                self.unconfirmed.insert(input.command_id);
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

/// Окружение для дома владельца: явный корень из конфигурации подменяет $CODEX_HOME.
fn owner_env(cfg: &CodexConfig) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = std::env::vars_os().collect();
    if let Some(owner) = &cfg.owner_home {
        env.retain(|(key, _)| key.to_string_lossy() != "CODEX_HOME");
        env.push((
            std::ffi::OsString::from("CODEX_HOME"),
            owner.as_os_str().to_os_string(),
        ));
    }
    env
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

/// Сигнал группе процессов app-server (сервер — лидер своей группы).
fn signal_group(pid: u32, signal: rustix::process::Signal) {
    use rustix::process::{Pid, kill_process_group};
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    let _ = kill_process_group(pid, signal);
}

/// Ранний выход app-server сразу после запуска: процесс не прожил и секунды.
async fn exited_early(proc: &mut AppServerProcess) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        match proc.try_wait() {
            Ok(Some(_)) => {
                let _ = proc.wait().await;
                return true;
            }
            Err(error) if reaped(&error) => return true,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn drain_until_exit(
    proc: &mut AppServerProcess,
    adapter: &mut CodexAdapter,
    limit: Duration,
) -> bool {
    let start = Instant::now();
    loop {
        match proc.try_wait() {
            Ok(Some(_)) => {
                drain_available(proc, adapter).await;
                return true;
            }
            Err(error) if reaped(&error) => return true,
            _ => {}
        }
        if start.elapsed() >= limit {
            return false;
        }
        match timeout(Duration::from_millis(50), proc.next_line()).await {
            // Конец stdout отвечает мгновенно, а процесс ещё жив: без паузы цикл крутился бы.
            Ok(StdoutLine::Eof) => {
                adapter.dispatch_line(StdoutLine::Eof);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok(line) => adapter.dispatch_line(line),
            Err(_) => {}
        }
    }
}

async fn drain_available(proc: &mut AppServerProcess, adapter: &mut CodexAdapter) {
    loop {
        match timeout(Duration::from_millis(50), proc.next_line()).await {
            Ok(line @ StdoutLine::Line(_)) => adapter.dispatch_line(line),
            _ => return,
        }
    }
}

async fn drain_to_eof(proc: &mut AppServerProcess, adapter: &mut CodexAdapter) {
    loop {
        match proc.next_line().await {
            line @ StdoutLine::Line(_) => adapter.dispatch_line(line),
            _ => return,
        }
    }
}

#[async_trait::async_trait]
impl Adapter for CodexAdapter {
    fn isolates_mcp(&self) -> bool {
        true
    }

    /// Пара `contextCompaction` приходит тем же каналом между ходами: итог чужого
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
        // Дом canterlot для всех пресетов, кроме «Как у владельца» (план 2c,
        // задача 7): готов до запуска, иначе первый сервер видит ~/.codex.
        self.isolate_mcp = spec.isolate_mcp;
        self.codex_home = if self.wants_home(spec.instructions.mode) {
            Some(
                self.prepare_home(spec.instructions.mode)
                    .map_err(AdapterError::StartFailed)?,
            )
        } else {
            None
        };
        let launch = LaunchSpec {
            executable: &self.cfg.executable,
            exec_dir: &self.exec_dir,
            workdir: &spec.workdir,
            relay_exe: &relay,
            relay_secret: self.broker.as_ref().map(Broker::relay_secret),
        };
        self.proc = Some(
            AppServerProcess::spawn(
                &launch,
                group,
                spec.instructions.mode,
                self.codex_home.as_ref(),
            )
            .map_err(|error| {
                // Мгновенно упавший сервер: запуск удался, умер сам процесс.
                if matches!(error, process::LaunchError::CliNotFoundInContainment) {
                    AdapterError::StartFailed("app_server_failed")
                } else {
                    AdapterError::StartFailed("cli_launch_failed")
                }
            })?,
        );
        self.handshake(spec).await
    }

    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        input::validate_settings(
            input
                .settings
                .as_ref()
                .unwrap_or(&crate::adapter::SettingsChange::default()),
            &self.models,
            self.effective
                .as_ref()
                .map(|effective| effective.model.as_str()),
        )
    }

    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome {
        if self.proc.is_none() || self.exited || self.eof {
            return TransmitOutcome::Undetermined;
        }
        self.transmit_inner(input, group).await
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
            if timeout_at(limit, self.flush_writes()).await.is_err()
                || timeout_at(limit, self.pump_once()).await.is_err()
            {
                return DecisionOutcome::Undetermined;
            }
        }
    }

    async fn refuse_request(&mut self, request_id: &str, _message: &str) {
        let Some((kind, ..)) = self.requests.pending_info(request_id) else {
            return;
        };
        let decision = if kind == "question" {
            DecisionPayload::Question {
                answers: Default::default(),
            }
        } else {
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message: None,
            }
        };
        if let Ok(line) = self.requests.answer(request_id, &decision) {
            self.pending_writes.push_back(line);
        }
    }

    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        // Итог вызова пресетов уходит запросившему CLI; чужой итог брокер отбросит.
        let result = crate::adapter::limits_fallback(&mut self.limits_calls, call_id, result);
        if let Some(broker) = self.broker.as_mut() {
            broker.answer_agent_tool(call_id, &result).await;
        }
    }

    /// Сжатие по кнопке методом app-server `thread/compact/start` между ходами.
    /// Итог приходит парой `contextCompaction`, ручным эпизод помечает флаг.
    async fn compact(&mut self, command: Uuid) -> CompactOutcome {
        if self.turn_running() || self.compact_manual || self.compact_pending.is_some() {
            return CompactOutcome::NotApplicable;
        }
        if self.eof || self.exited {
            return CompactOutcome::Failed;
        }
        let thread_id = self.thread_id();
        if thread_id.is_empty() {
            return CompactOutcome::Failed;
        }
        let (id, message) = self.rpc.request(
            "thread/compact/start",
            serde_json::json!({ "threadId": thread_id }),
        );
        // Кадры хода сжатия могут прийти раньше ответа: команда открыта уже к ним.
        self.compact_manual = true;
        self.compact_pending = Some((command, Instant::now() + COMPACT_TIMEOUT));
        self.compact_turn = false;
        self.compact_done = None;
        let accepted = self.write_request(message).await
            && matches!(
                self.wait_response(id, Instant::now() + self.deadline).await,
                Some(Ok(_))
            );
        if !accepted {
            self.compact_manual = false;
            self.compact_pending = None;
            self.compact_turn = false;
            self.compact_done = None;
            return CompactOutcome::Failed;
        }
        CompactOutcome::Accepted
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
                // Нет ответа к сроку или отказ — терминал мог остаться жив.
                if !matches!(
                    self.wait_response(terminate_id, Instant::now() + input::INTERRUPT_TIMEOUT)
                        .await,
                    Some(Ok(_))
                ) {
                    self.diagnostic("interrupt_cleanup_failed", false);
                }
            }
        } else {
            self.diagnostic("interrupt_cleanup_failed", false);
        }
        self.pending_writes.extend(self.requests.refuse_subagents());
        self.requests.drop_all();
        InterruptOutcome::Acknowledged
    }

    /// Влить ожидающий ввод в идущий ход (`turn/steer`): тот же ход,
    /// доказательство — элемент `userMessage` с нашим `clientId`.
    /// Протухший `expectedTurnId` (`-32600`) — не отказ, а конец хода:
    /// ввод остаётся в очереди и уйдёт следующим ходом.
    async fn steer(&mut self, input: &Transmission) -> SteerOutcome {
        let Some(active) = self.outbox.active().cloned() else {
            return SteerOutcome::NotApplicable("no_turn".into());
        };
        let expected = active.turn_id.clone();
        let request = match input::build_turn_steer(&self.thread_id(), &expected, input) {
            Ok(request) => request,
            Err(error) => {
                return SteerOutcome::Rejected(
                    if error.to_string().contains("input_too_long") {
                        "input_too_long"
                    } else {
                        "input_build_failed"
                    }
                    .into(),
                );
            }
        };
        // До записи: `item/started` вливки может обогнать ответ.
        let (id, message) = self.rpc.request("turn/steer", request);
        self.proof_for = Some(input.command_id);
        self.proof_seen = false;
        if !self.write_request(message).await {
            self.proof_for = None;
            return SteerOutcome::Undetermined;
        }
        let limit = Instant::now() + self.deadline;
        let response = self.wait_response(id, limit).await;
        match response {
            None => {
                self.proof_for = None;
                if self.proof_seen {
                    return SteerOutcome::Accepted(Proof {
                        command_id: input.command_id,
                        kind: input::PROOF_KIND_MESSAGE,
                    });
                }
                self.unconfirmed.insert(input.command_id);
                SteerOutcome::Undetermined
            }
            Some(Err(error)) => {
                self.proof_for = None;
                if error.code == -32600 {
                    // Хода уже нет или `expectedTurnId` протух: ввод остаётся
                    // в очереди, следующим ходом уйдёт сам.
                    let stale = error.message.contains("expected active turn id");
                    SteerOutcome::NotApplicable(if stale { "stale_turn" } else { "no_turn" }.into())
                } else {
                    SteerOutcome::Rejected(input::error_rejection(&error))
                }
            }
            Some(Ok(value)) => {
                let same = value
                    .get("turnId")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|turn| turn == expected);
                if !same {
                    // Ответ признал чужой ход: доказательство может прийти поздно.
                    self.proof_for = None;
                    self.unconfirmed.insert(input.command_id);
                    return SteerOutcome::Undetermined;
                }
                loop {
                    if self.proof_seen {
                        self.proof_for = None;
                        return SteerOutcome::Accepted(Proof {
                            command_id: input.command_id,
                            kind: input::PROOF_KIND_MESSAGE,
                        });
                    }
                    // Ход кончился без элемента вливки: ждать нечего.
                    if self.eof || self.exited || self.outbox.active().is_none() {
                        self.proof_for = None;
                        return SteerOutcome::Undetermined;
                    }
                    if timeout_at(limit, self.pump_once()).await.is_err() {
                        self.proof_for = None;
                        self.unconfirmed.insert(input.command_id);
                        return SteerOutcome::Undetermined;
                    }
                }
            }
        }
    }

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            self.compact_timed_out();
            self.flush_writes().await;
            self.settle_late_turn();
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
        if let Some(pending) = self.crunch.take() {
            pending.task.abort();
        }
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
