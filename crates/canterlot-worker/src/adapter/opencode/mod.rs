//! Адаптер OpenCode 2: сервер `opencode serve --stdio` на исполнение, HTTP и SSE по loopback.
//! Харнесс спрятан за таблицей `[opencode]`: без неё не показывается и не запускается.

pub mod http;
pub mod instructions;
pub mod mcp;
pub mod permissions;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod sse;
pub mod version;

use self::{
    http::{Http, HttpError},
    mcp::McpState,
    permissions::PermissionMode,
    process::{DataLayout, LayoutError, ReadyError, ServerProcess},
    sse::StreamItem,
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, Capabilities, CloseMode,
        DecisionOutcome, HarnessCatalog, InterruptOutcome, StartSpec, Started, Support,
        Transmission, TransmitOutcome,
        canterlot_mcp::{Broker, BrokerEvent, ToolSet},
        cli_version::{CliVersion, VersionError},
    },
    config::OpenCodeConfig,
    containment::Containment,
};
use canterlot_proto::requests::DecisionPayload;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};
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
    stop_grace: Duration,
    ready_wait: Duration,
    relay_executable: Option<PathBuf>,
    start_attempted: bool,
    proc: Option<ServerProcess>,
    http: Option<Http>,
    stream: Option<EventStream>,
    broker: Option<Broker>,
    events: VecDeque<AdapterEvent>,
    session: String,
    mode: PermissionMode,
    /// Момент, после которого хвост потока больше не ждём.
    exit: Option<(Option<ExitStatus>, Instant)>,
    exited: bool,
}

impl OpenCodeAdapter {
    pub fn new(cfg: OpenCodeConfig, exec_dir: PathBuf, _deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            stop_grace: Duration::from_secs(5),
            ready_wait: READY_WAIT,
            relay_executable: None,
            start_attempted: false,
            proc: None,
            http: None,
            stream: None,
            broker: None,
            events: VecDeque::new(),
            session: String::new(),
            mode: PermissionMode::Owner,
            exit: None,
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

    fn on_stream(&mut self, item: Option<StreamItem>) {
        match item {
            Some(StreamItem::Event(_)) => {}
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
        if let Some(proc) = self.proc.as_ref() {
            proc.signal(rustix::process::Signal::TERM);
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
        tokio::select! {
            biased;
            item = item => self.on_stream(item),
            event = broker.next() => self.on_broker(event),
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
            streaming: Support::Unavailable("opencode_turns"),
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

    async fn transmit(
        &mut self,
        _input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        TransmitOutcome::Undetermined
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
