//! Адаптер OpenCode 2: сервер `opencode serve --stdio` на исполнение, HTTP и SSE по loopback.
//! Харнесс спрятан за таблицей `[opencode]`: без неё не показывается и не запускается.

pub mod catalog;
pub mod events;
pub mod http;
pub mod input;
pub mod instructions;
pub mod mcp;
pub mod permissions;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod sse;
pub mod version;

use self::{
    events::{PendingRequest, RequestKind, RunFailure, RunOutcome, Translated, Translator},
    http::{Http, HttpError},
    mcp::McpState,
    permissions::PermissionMode,
    process::{DataLayout, LayoutError, ReadyError, ServerProcess},
    sse::StreamItem,
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, Capabilities, CloseMode,
        DecisionOutcome, HarnessCatalog, InstructionMode, InstructionSpec, InterruptOutcome, Phase,
        Proof, SettingsChange, StartSpec, Started, Support, Transmission, TransmitOutcome,
        canterlot_mcp::{Broker, BrokerEvent, ToolSet},
        cli_version::{CliVersion, VersionError},
        held_tool::HeldTools,
    },
    config::OpenCodeConfig,
    containment::Containment,
};
use canterlot_proto::requests::DecisionPayload;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet, time::Instant};
use uuid::Uuid;

/// Строка готовности и ожидание MCP: на проверенной версии доли секунды.
const READY_WAIT: Duration = Duration::from_secs(20);
const REQUEST_WAIT: Duration = Duration::from_secs(10);
const POLL_EVERY: Duration = Duration::from_millis(100);
/// Эвристика: `connected` публикуется раньше инструментов, дебаунс `mcp.tools.changed`
/// около 100 мс. Публичного сигнала нет, запас проверяет L2 на повторных холодных стартах.
const MCP_SETTLE: Duration = Duration::from_millis(250);
const TERM_GRACE: Duration = Duration::from_secs(2);
const KILL_WAIT: Duration = Duration::from_secs(2);
const STREAM_BUFFER: usize = 4096;
/// Хвост событий после выхода сервера.
const EXIT_DRAIN: Duration = Duration::from_millis(500);
const SESSION_TITLE: &str = "canterlot";
const ERROR_TEXT_BYTES: usize = 4096;
/// Текст отказа уходит модели: без него сервер оборвал бы весь ход.
const REFUSAL_MESSAGE: &str =
    "This request cannot be answered in this session. Continue without it.";
/// Схема `GET /openapi.json` проверенной версии (`evidence/opencode/openapi-2.1.4.sha256`).
const VERIFIED_OPENAPI_SHA256: &str =
    "23a25e5604b1274987ab7093be1eea517bd93d588e849ea53ad93a977878db3a";

/// Итог проверки при старте воркера: версия CLI. Каталог моделей появится с его срезом.
#[derive(Clone)]
pub struct OpenCodeCatalog {
    pub version: CliVersion,
}

impl OpenCodeCatalog {
    pub fn harness(&self) -> HarnessCatalog {
        HarnessCatalog {
            harness: "opencode",
            provider: "opencode",
            title: "OpenCode",
            images: false,
            settings_midsession: false,
            settings_midsession_effort: false,
            permission_modes: vec!["owner", "bypass"],
            models: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum DiscoverError {
    Version(VersionError),
    DataRoot(LayoutError),
}

// Каталоги и окружение своих данных: без них проверка версии пишет к владельцу.
async fn prepared_env(
    cfg: &OpenCodeConfig,
    layout: &DataLayout,
    tmp: PathBuf,
) -> Result<Vec<(OsString, OsString)>, LayoutError> {
    let cfg = cfg.clone();
    let layout = layout.clone();
    tokio::task::spawn_blocking(move || {
        let parent: Vec<_> = std::env::vars_os().collect();
        process::check_root(&layout, &cfg, &parent)?;
        layout.create().map_err(LayoutError::Io)?;
        std::fs::create_dir_all(&tmp).map_err(LayoutError::Io)?;
        Ok(process::base_env(parent.into_iter(), &layout, &tmp))
    })
    .await
    .map_err(|error| LayoutError::Io(error.into()))?
}

/// Проверка при старте воркера; без неё харнесс не попадает в каталог.
pub async fn discover(
    cfg: &OpenCodeConfig,
    data_dir: &Path,
) -> Result<OpenCodeCatalog, DiscoverError> {
    let layout = DataLayout::for_worker(cfg, data_dir);
    let env = prepared_env(cfg, &layout, layout.tmp())
        .await
        .map_err(DiscoverError::DataRoot)?;
    let version = version::check(&cfg.executable, &env)
        .await
        .map_err(DiscoverError::Version)?;
    Ok(OpenCodeCatalog { version })
}

fn version_failure(error: VersionError) -> AdapterError {
    AdapterError::StartFailed(match error {
        VersionError::NotFound => "cli_not_found",
        VersionError::BelowMinimum(_) => "cli_too_old",
        _ => "cli_version_failed",
    })
}

fn version_label(version: &CliVersion) -> &str {
    match version {
        CliVersion::Verified(label) | CliVersion::UntestedNewer(label) => label,
    }
}

/// `provider/model` каталога в `Model.Ref`; усилие OpenCode называет вариантом.
fn model_ref(model: &str, effort: &str) -> Option<Value> {
    let (provider, id) = model.split_once('/')?;
    let mut reference = json!({"id": id, "providerID": provider});
    if !effort.is_empty() {
        reference["variant"] = effort.into();
    }
    Some(reference)
}

fn random_password() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn session_id() -> String {
    format!("ses_{}", Uuid::now_v7().simple())
}

// Ход сервера закрывает одно терминальное событие: поддерживается строго один ввод.
#[derive(Default)]
struct Delivery {
    /// Ввод, чей конец хода ждём; ставится до запроса, события могут обогнать ответ.
    active: Option<Uuid>,
    /// Доказательства нет, супервизору сказано `Undetermined`.
    unconfirmed: Option<Uuid>,
}

/// Чтение проекций после `Undetermined`. Отдельная задача: итог ждёт в `self`, а не на
/// стеке `next_event`, который супервизор бросает.
struct Lookup {
    command: Uuid,
    task: tokio::task::JoinHandle<bool>,
}

impl Drop for Lookup {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn lookup_done(lookup: &mut Option<Lookup>) -> bool {
    match lookup {
        Some(pending) => (&mut pending.task).await.unwrap_or(false),
        None => std::future::pending().await,
    }
}

struct EventStream {
    items: mpsc::Receiver<StreamItem>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct OpenCodeAdapter {
    cfg: OpenCodeConfig,
    exec_dir: PathBuf,
    deadline: Duration,
    stop_grace: Duration,
    ready_wait: Duration,
    relay_executable: Option<PathBuf>,
    start_attempted: bool,
    proc: Option<ServerProcess>,
    http: Option<Http>,
    stream: Option<EventStream>,
    broker: Option<Broker>,
    events: VecDeque<AdapterEvent>,
    translator: Option<Translator>,
    delivery: Delivery,
    lookup: Option<Lookup>,
    held: HeldTools,
    /// Вызовы, по которым был запрос: пометки CLI у них нет.
    asked: HashSet<String>,
    refusals: JoinSet<()>,
    session: String,
    mode: PermissionMode,
    model: String,
    effort: String,
    instructions: InstructionSpec,
    /// Момент, после которого хвост потока больше не ждём.
    exit: Option<(Option<ExitStatus>, Instant)>,
    /// Поток потерян, сервер получил SIGTERM: срок до SIGKILL.
    kill_at: Option<Instant>,
    exited: bool,
}

impl OpenCodeAdapter {
    pub fn new(cfg: OpenCodeConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            stop_grace: Duration::from_secs(5),
            ready_wait: READY_WAIT,
            relay_executable: None,
            start_attempted: false,
            proc: None,
            http: None,
            stream: None,
            broker: None,
            events: VecDeque::new(),
            translator: None,
            delivery: Delivery::default(),
            lookup: None,
            held: HeldTools::default(),
            asked: HashSet::new(),
            refusals: JoinSet::new(),
            session: String::new(),
            mode: PermissionMode::Owner,
            model: String::new(),
            effort: String::new(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            exit: None,
            kill_at: None,
            exited: false,
        }
    }

    pub fn with_relay_executable(mut self, executable: PathBuf) -> Self {
        self.relay_executable = Some(executable);
        self
    }

    /// Срок строки готовности и каждого ожидания на старте.
    pub fn set_ready_wait(&mut self, wait: Duration) {
        self.ready_wait = wait;
    }

    fn diagnostic(&mut self, kind: &'static str, required: bool) {
        self.events.push_back(AdapterEvent::Diagnostic {
            kind,
            event_type: kind.into(),
            required,
        });
    }

    fn http(&self) -> Result<&Http, AdapterError> {
        self.http
            .as_ref()
            .ok_or(AdapterError::StartFailed("server_unreachable"))
    }

    async fn spawn_server(
        &mut self,
        spec: &StartSpec,
        env: &[(OsString, OsString)],
        group: &dyn Containment,
    ) -> Result<String, AdapterError> {
        let mut proc = ServerProcess::spawn(&self.cfg.executable, &spec.workdir, env, group)
            .await
            .map_err(|error| {
                // Мгновенно упавший сервер: запуск удался, умер сам процесс.
                AdapterError::StartFailed(match error {
                    process::LaunchError::Spawn(_) => "cli_launch_failed",
                    _ => "server_not_ready",
                })
            })?;
        let ready = proc.ready_url(self.ready_wait).await;
        self.proc = Some(proc);
        ready.map_err(|error| {
            AdapterError::StartFailed(match error {
                ReadyError::NotLoopback => "server_not_loopback",
                ReadyError::NoLine | ReadyError::Malformed => "server_not_ready",
            })
        })
    }

    // Поток открывается раньше всех запросов: событие до подписки потеряно навсегда.
    async fn open_stream(&mut self) -> Result<(), AdapterError> {
        let response = self
            .http()?
            .events(self.ready_wait.min(REQUEST_WAIT))
            .await
            .map_err(|_| AdapterError::StartFailed("server_unreachable"))?;
        let (sender, mut items) = mpsc::channel(STREAM_BUFFER);
        let task = tokio::spawn(sse::pump(response, sender));
        let connected = tokio::time::timeout(self.ready_wait, async {
            while let Some(item) = items.recv().await {
                match item {
                    StreamItem::Event(event) if event["type"] == "server.connected" => return true,
                    StreamItem::Ended => return false,
                    _ => {}
                }
            }
            false
        })
        .await;
        self.stream = Some(EventStream { items, task });
        if !matches!(connected, Ok(true)) {
            return Err(AdapterError::StartFailed("server_unreachable"));
        }
        Ok(())
    }

    async fn check_identity(&mut self, version: &CliVersion) -> Result<(), AdapterError> {
        let pid = self.proc.as_ref().map(|proc| proc.pid);
        let info = self
            .http()?
            .get("/api/info", REQUEST_WAIT)
            .await
            .map_err(|_| AdapterError::StartFailed("server_unreachable"))?;
        if info["version"].as_str() != Some(version_label(version))
            || info["pid"].as_u64() != pid.map(u64::from)
        {
            return Err(AdapterError::StartFailed("server_identity_mismatch"));
        }
        Ok(())
    }

    // Весь API подписан как экспериментальный: у непроверенной версии сверяется схема.
    async fn check_schema(&mut self) -> Result<(), AdapterError> {
        let same = match self.http()?.bytes("/openapi.json", REQUEST_WAIT).await {
            Ok(bytes) => {
                let digest: String = Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                digest == VERIFIED_OPENAPI_SHA256
            }
            Err(_) => false,
        };
        if !same {
            self.diagnostic("openapi_changed", false);
        }
        Ok(())
    }

    async fn create_session(&mut self, spec: &StartSpec) -> Result<String, AdapterError> {
        let id = session_id();
        let mut body = json!({
            "id": id,
            "title": SESSION_TITLE,
            "location": {"directory": spec.workdir.to_string_lossy()},
        });
        if let Some(model) = model_ref(&spec.model, &spec.effort) {
            body["model"] = model;
        }
        if let Some(rules) = self.mode.ruleset() {
            body["permissions"] = rules;
        }
        let created = self
            .http()?
            .post("/api/session", &body, REQUEST_WAIT)
            .await
            .map_err(|_| AdapterError::StartFailed("session_create_failed"))?;
        if created.pointer("/data/id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(AdapterError::StartFailed("session_create_failed"));
        }
        Ok(id)
    }

    // Повторное создание не меняет сессию, поэтому правила и модель ставятся отдельно.
    async fn resume_session(
        &mut self,
        spec: &StartSpec,
        native: &str,
    ) -> Result<String, AdapterError> {
        let failed = |_| AdapterError::StartFailed("resume_failed");
        let path = format!("/api/session/{native}");
        self.http()?
            .get(&path, REQUEST_WAIT)
            .await
            .map_err(failed)?;
        if let Some(rules) = self.mode.ruleset() {
            let body = json!({"permissions": rules});
            self.http()?
                .patch(&path, &body, REQUEST_WAIT)
                .await
                .map_err(failed)?;
        }
        if let Some(model) = model_ref(&spec.model, &spec.effort) {
            let body = json!({"model": model});
            self.http()?
                .post(&format!("{path}/model"), &body, REQUEST_WAIT)
                .await
                .map_err(failed)?;
        }
        Ok(native.to_owned())
    }

    async fn wait_mcp(&mut self) -> Result<(), AdapterError> {
        let limit = Instant::now() + self.ready_wait;
        let state = loop {
            let state = match self.http()?.get("/api/mcp", REQUEST_WAIT).await {
                Ok(list) => mcp::state_of(&list),
                Err(HttpError::Lost) => {
                    return Err(AdapterError::StartFailed("server_unreachable"));
                }
                Err(_) => McpState::Pending,
            };
            if state != McpState::Pending || Instant::now() >= limit {
                break state;
            }
            tokio::time::sleep(POLL_EVERY).await;
        };
        if state == McpState::Connected {
            tokio::time::sleep(MCP_SETTLE).await;
        } else {
            self.diagnostic("mcp_unavailable", false);
        }
        Ok(())
    }

    // Локация поднимается лениво: сразу после старта список пуст.
    async fn wait_agents(&mut self) -> Result<(), AdapterError> {
        let limit = Instant::now() + self.ready_wait;
        loop {
            let agents = self
                .http()?
                .get("/api/agent", REQUEST_WAIT)
                .await
                .map_err(|_| AdapterError::StartFailed("agents_unavailable"))?;
            if agents["data"]
                .as_array()
                .is_some_and(|list| !list.is_empty())
            {
                return Ok(());
            }
            if Instant::now() >= limit {
                return Err(AdapterError::StartFailed("agents_unavailable"));
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
    }

    // Окно контекста не приходит в событиях. Без каталога ход всё равно работает.
    async fn context_windows(&mut self) -> HashMap<String, u64> {
        match self.http {
            Some(ref http) => http
                .get("/api/model", REQUEST_WAIT)
                .await
                .map(|models| catalog::context_windows(&models))
                .unwrap_or_default(),
            None => HashMap::new(),
        }
    }

    async fn start_inner(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        self.mode = PermissionMode::parse(&spec.permission_mode)
            .ok_or(AdapterError::StartFailed("unknown_permission_mode"))?;
        let layout = DataLayout::for_execution(&self.cfg, &self.exec_dir);
        let mut env = prepared_env(&self.cfg, &layout, self.exec_dir.join("tmp"))
            .await
            .map_err(|error| {
                AdapterError::StartFailed(match error {
                    LayoutError::Refused => "data_root_refused",
                    LayoutError::Io(_) => "data_root_failed",
                })
            })?;
        let version = version::check(&self.cfg.executable, &env)
            .await
            .map_err(version_failure)?;
        let untested = matches!(version, CliVersion::UntestedNewer(_));
        if untested {
            self.diagnostic("untested_cli_version", false);
        }
        env.extend(
            instructions::prepare(&self.exec_dir, &spec.instructions)
                .await
                .map_err(|_| AdapterError::StartFailed("instructions_failed"))?,
        );
        self.broker =
            Some(Broker::bind_with(&self.exec_dir, ToolSet::LimitsOnly, "opencode").await?);
        let relay = match &self.relay_executable {
            Some(relay) => relay.clone(),
            None => std::env::current_exe()?,
        };
        let password = random_password();
        env.push(("OPENCODE_PASSWORD".into(), password.clone().into()));
        env.push((
            "OPENCODE_CONFIG_CONTENT".into(),
            mcp::config_content(&relay, &self.exec_dir).into(),
        ));
        let base = self.spawn_server(spec, &env, group).await?;
        let http = Http::new(base, password)
            .await
            .map_err(|_| AdapterError::StartFailed("server_unreachable"))?;
        self.http = Some(http);
        self.open_stream().await?;
        self.check_identity(&version).await?;
        if untested {
            self.check_schema().await?;
        }
        self.session = match &spec.native_conversation_id {
            Some(native) => self.resume_session(spec, native).await?,
            None => self.create_session(spec).await?,
        };
        self.wait_mcp().await?;
        self.wait_agents().await?;
        self.model = spec.model.clone();
        self.effort = spec.effort.clone();
        self.instructions = spec.instructions.clone();
        let windows = self.context_windows().await;
        self.translator = Some(Translator::new(&self.session, windows));
        let proc = self
            .proc
            .as_ref()
            .ok_or(AdapterError::StartFailed("server_not_ready"))?;
        Ok(Started {
            native_conversation_id: self.session.clone(),
            cli_pid: proc.pid,
            cli_start_ticks: proc.start_ticks,
        })
    }

    // Упавший запуск не оставляет сервера: супервизор адаптера ещё не получил.
    async fn abandon_server(&mut self) {
        self.stream = None;
        if let Some(broker) = self.broker.as_mut() {
            broker.shutdown().await;
        }
        self.broker = None;
        if let Some(proc) = self.proc.as_mut() {
            proc.close_stdin();
            proc.signal(rustix::process::Signal::KILL);
            let _ = tokio::time::timeout(KILL_WAIT, proc.wait()).await;
        }
        self.proc = None;
        self.exited = true;
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
            BrokerEvent::LimitsQuery { call } => {
                if let Some(broker) = self.broker.as_mut() {
                    broker.answer_limits(
                        call,
                        &canterlot_proto::worker::v1::LimitsSnapshot {
                            provider: "opencode".into(),
                            ..Default::default()
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn error_block(&mut self, failure: &RunFailure, command: Uuid) {
        let mut text = format!("Ошибка OpenCode ({}): {}", failure.kind, failure.message);
        text.truncate(text.floor_char_boundary(ERROR_TEXT_BYTES));
        let block_id = Uuid::now_v7();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::Error,
            input_command_id: Some(command),
            parent: None,
            agent: "orchestrator".into(),
            model: self.model.clone(),
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

    fn awaiting_input(&mut self) {
        self.events.push_back(AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            tool_name: None,
            summary: None,
        });
    }

    fn on_run_ended(&mut self, outcome: RunOutcome) {
        self.held.release_all(&mut self.events);
        self.asked.clear();
        let Some(command) = self.delivery.active.take() else {
            // Запуски без ввода разбирает свой срез: здесь только готовность к вводу.
            self.awaiting_input();
            return;
        };
        if self.delivery.unconfirmed == Some(command) {
            self.delivery.unconfirmed = None;
            self.lookup = None;
        }
        if let RunOutcome::Failed(failure) = &outcome {
            self.error_block(failure, command);
        }
        if let Some(translator) = self.translator.as_mut() {
            self.events.push_back(translator.finish_run(command));
        }
        self.awaiting_input();
    }

    // Ввод закрыт без хода: конца хода от сервера не будет.
    fn finish_without_turn(&mut self, command: Uuid) {
        self.delivery.active = None;
        self.events.push_back(AdapterEvent::RunFinished {
            input_command_id: command,
            usage: Vec::new(),
            context_tokens: 0,
            context_window_tokens: 0,
        });
        self.awaiting_input();
    }

    fn prove_late(&mut self, command: Uuid) {
        self.delivery.unconfirmed = None;
        self.lookup = None;
        self.events.push_back(AdapterEvent::InputAccepted {
            command_id: command,
        });
    }

    /// Пометка «Разрешено правилами CLI»: вызов прошёл без запроса при режиме не `bypass`.
    fn cli_mark(&self, item: &str) -> Option<&'static str> {
        (self.mode != PermissionMode::Bypass && !self.asked.contains(item)).then_some("cli_policy")
    }

    fn release_tool(&mut self, item: &str) {
        let mark = self.cli_mark(item);
        self.held.release(item, mark, &mut self.events);
    }

    // Карточек пока нет: без ответа запрос держал бы ход вечно, поэтому отказ сразу и
    // всегда с сообщением. `always` не отправляется: он пишет правило на весь проект.
    fn refuse(&mut self, request: PendingRequest) {
        if let Some(item) = &request.item {
            self.asked.insert(item.clone());
            self.held.release(item, None, &mut self.events);
        }
        self.diagnostic("opencode_request_refused", false);
        let Some(http) = self.http.clone() else {
            return;
        };
        while self.refusals.try_join_next().is_some() {}
        self.refusals.spawn(async move {
            let (session, id) = (&request.session, &request.id);
            let _ = match request.kind {
                RequestKind::Permission => {
                    let path = format!("/api/session/{session}/permission/{id}/reply");
                    let body = json!({"decision": "reject", "message": REFUSAL_MESSAGE});
                    http.post(&path, &body, REQUEST_WAIT).await
                }
                RequestKind::Form => {
                    let path = format!("/api/session/{session}/form/{id}");
                    http.delete(&path, &[("message", REFUSAL_MESSAGE)], REQUEST_WAIT)
                        .await
                }
            };
        });
    }

    fn on_translated(&mut self, item: Translated) {
        match item {
            Translated::Event(event) => {
                if let Some(event) = self.held.capture(event) {
                    self.events.push_back(event);
                }
            }
            Translated::ToolCall { item, block, event } => {
                if self.mode == PermissionMode::Bypass {
                    self.events.push_back(event);
                } else {
                    self.held.hold(item, block, event);
                }
            }
            Translated::ToolRunning(item) | Translated::ToolDone(item) => self.release_tool(&item),
            Translated::Request(request) => self.refuse(request),
            Translated::InboxSeen(id) => {
                if let Some(command) = self.delivery.unconfirmed
                    && input::message_id(command) == id
                {
                    self.prove_late(command);
                }
            }
            Translated::RunEnded(outcome) => self.on_run_ended(outcome),
        }
    }

    fn on_lookup(&mut self, found: bool) {
        let Some(lookup) = self.lookup.take() else {
            return;
        };
        if self.delivery.unconfirmed != Some(lookup.command) {
            return;
        }
        if found {
            self.prove_late(lookup.command);
        } else {
            // Записи нет, но и доказательства отказа нет: ввод остаётся неподтверждённым.
            self.delivery.unconfirmed = None;
            self.finish_without_turn(lookup.command);
        }
    }

    fn start_lookup(&mut self, command: Uuid) {
        self.delivery.unconfirmed = Some(command);
        let Some(http) = self.http.clone() else {
            return;
        };
        let task = tokio::spawn(input::reconcile(
            http,
            self.session.clone(),
            input::message_id(command),
        ));
        self.lookup = Some(Lookup { command, task });
    }

    // Смена настроек на ходу приходит со своими срезами: молча её терять нельзя.
    fn settings_error(&self, change: &SettingsChange) -> Option<&'static str> {
        let differs = |wanted: &Option<String>, current: &str| {
            wanted.as_deref().is_some_and(|wanted| wanted != current)
        };
        let unsupported = differs(&change.model, &self.model)
            || differs(&change.effort, &self.effort)
            || change
                .permission_mode
                .as_deref()
                .is_some_and(|mode| PermissionMode::parse(mode) != Some(self.mode))
            || change
                .instructions
                .as_ref()
                .is_some_and(|next| !next.same_effect(&self.instructions));
        unsupported.then_some("settings_unavailable")
    }

    async fn transmit_inner(&mut self, input: &Transmission) -> TransmitOutcome {
        let command = input.command_id;
        if self.delivery.active.is_some() {
            return TransmitOutcome::Rejected("turn_active".into());
        }
        if let Some(reason) = input
            .settings
            .as_ref()
            .and_then(|change| self.settings_error(change))
        {
            return TransmitOutcome::Rejected(reason.into());
        }
        let body = match input::prompt_body(input) {
            Ok(body) => body,
            Err(reason) => return TransmitOutcome::Rejected(reason.into()),
        };
        let Some(http) = self.http.clone() else {
            return TransmitOutcome::Undetermined;
        };
        let id = input::message_id(command);
        if input.attempt > 1 {
            match input::find_input(&http, &self.session, &id).await {
                Ok(true) => {
                    self.finish_without_turn(command);
                    return TransmitOutcome::Accepted(Proof {
                        command_id: command,
                        kind: input::PROOF_KIND_HISTORY,
                    });
                }
                Ok(false) => {}
                // Повтор вслепую мог бы разбудить пустой период: исход решает владелец.
                Err(_) => {
                    self.finish_without_turn(command);
                    return TransmitOutcome::Undetermined;
                }
            }
        }
        self.delivery.active = Some(command);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_run(command);
        }
        let path = format!("/api/session/{}/prompt", self.session);
        match http.post(&path, &body, self.deadline).await {
            Ok(reply) if input::accepted_id(&reply) == Some(id.as_str()) => {
                TransmitOutcome::Accepted(Proof {
                    command_id: command,
                    kind: input::PROOF_KIND,
                })
            }
            Err(HttpError::Status { status, tag }) if input::is_rejection(status) => {
                self.delivery.active = None;
                TransmitOutcome::Rejected(tag)
            }
            _ => {
                self.start_lookup(command);
                TransmitOutcome::Undetermined
            }
        }
    }

    fn on_stream(&mut self, item: Option<StreamItem>) {
        match item {
            Some(StreamItem::Event(event)) => {
                let items = self
                    .translator
                    .as_mut()
                    .map(|translator| translator.feed(&event))
                    .unwrap_or_default();
                for item in items {
                    self.on_translated(item);
                }
            }
            Some(StreamItem::Undecodable { required }) => {
                self.diagnostic("unparsed_event", required);
            }
            Some(StreamItem::Oversized) => self.diagnostic("oversized_frame", false),
            Some(StreamItem::Ended) | None => self.on_stream_end(),
        }
    }

    // Переподключения пока нет: без потока конца хода не будет, поэтому сервер снимается,
    // а исполнение получает ProcessExited вместо вечного ожидания.
    fn on_stream_end(&mut self) {
        self.stream = None;
        if let Some((status, _)) = self.exit.take() {
            self.finish_exit(status);
            return;
        }
        self.diagnostic("event_stream_lost", false);
        if let Some(proc) = self.proc.as_mut() {
            proc.close_stdin();
            proc.signal(rustix::process::Signal::TERM);
            self.kill_at = Some(Instant::now() + TERM_GRACE);
        }
    }

    fn finish_exit(&mut self, status: Option<ExitStatus>) {
        use std::os::unix::process::ExitStatusExt as _;
        self.exited = true;
        self.events.push_back(AdapterEvent::ProcessExited {
            code: status.and_then(|status| status.code()),
            signal: status.and_then(|status| status.signal()),
        });
    }

    // Отмена безопасна: обработка синхронна, состояние в `self`.
    async fn pump_once(&mut self) {
        if self.exited {
            return std::future::pending().await;
        }
        let stream = self.stream.as_mut();
        if let Some((status, until)) = self.exit {
            let item = async {
                match stream {
                    Some(stream) => stream.items.recv().await,
                    None => None,
                }
            };
            tokio::select! {
                item = item => self.on_stream(item),
                _ = tokio::time::sleep_until(until) => {
                    self.exit = None;
                    self.finish_exit(status);
                }
            }
            return;
        }
        let (Some(proc), Some(broker)) = (self.proc.as_mut(), self.broker.as_mut()) else {
            return std::future::pending().await;
        };
        let item = async {
            match stream {
                Some(stream) => stream.items.recv().await,
                None => std::future::pending().await,
            }
        };
        let lookup = &mut self.lookup;
        let kill_at = self.kill_at;
        let escalate = async {
            match kill_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            item = item => self.on_stream(item),
            event = broker.next() => self.on_broker(event),
            found = lookup_done(lookup) => self.on_lookup(found),
            () = escalate => {
                self.kill_at = None;
                proc.signal(rustix::process::Signal::KILL);
            }
            status = proc.wait() => {
                if self.stream.is_none() {
                    self.finish_exit(status);
                } else {
                    self.exit = Some((status, Instant::now() + EXIT_DRAIN));
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Adapter for OpenCodeAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Unavailable("opencode_interrupt"),
            approvals: Support::Unavailable("opencode_permissions"),
            questions: Support::Unavailable("opencode_questions"),
            background_work: Support::Unavailable("opencode_interrupt"),
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
        let result = self.start_inner(spec, group).await;
        if result.is_err() {
            self.abandon_server().await;
        }
        result
    }

    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        match input
            .settings
            .as_ref()
            .and_then(|change| self.settings_error(change))
        {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    async fn transmit(
        &mut self,
        input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        if self.exited || self.exit.is_some() || self.stream.is_none() {
            return TransmitOutcome::Undetermined;
        }
        self.transmit_inner(input).await
    }

    async fn deliver_decision(
        &mut self,
        _request_id: &str,
        _decision: &DecisionPayload,
    ) -> DecisionOutcome {
        DecisionOutcome::NotApplied("request_not_pending".into())
    }

    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        if let Some(broker) = self.broker.as_mut() {
            broker.answer_agent_tool(call_id, &result).await;
        }
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        InterruptOutcome::NotApplicable
    }

    fn set_stop_grace(&mut self, grace: Duration) {
        self.stop_grace = grace;
    }

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            self.pump_once().await;
        }
    }

    async fn close(
        &mut self,
        mode: CloseMode,
        _group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        use rustix::process::Signal;
        self.stream = None;
        if let Some(broker) = self.broker.as_mut() {
            broker.shutdown().await;
        }
        let Some(proc) = self.proc.as_mut() else {
            return Ok(());
        };
        if proc.finished() {
            return Ok(());
        }
        proc.close_stdin();
        let graceful = matches!(mode, CloseMode::Graceful);
        let gone = graceful
            && (tokio::time::timeout(self.stop_grace, proc.wait())
                .await
                .is_ok()
                || {
                    proc.signal(Signal::TERM);
                    tokio::time::timeout(TERM_GRACE, proc.wait()).await.is_ok()
                });
        if !gone {
            proc.signal(Signal::KILL);
            tokio::time::timeout(KILL_WAIT, proc.wait())
                .await
                .map_err(|_| AdapterError::Deadline)?;
        }
        self.exited = true;
        Ok(())
    }
}
