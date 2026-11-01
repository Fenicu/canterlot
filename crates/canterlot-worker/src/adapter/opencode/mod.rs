//! Адаптер OpenCode 2: сервер `opencode serve --stdio` на исполнение, HTTP и SSE по loopback.
//! Харнесс спрятан за таблицей `[opencode]`: без неё не показывается и не запускается.

pub mod catalog;
pub mod events;
pub mod failure;
pub mod http;
pub mod input;
pub mod instructions;
pub mod mcp;
pub mod orphans;
pub mod permissions;
pub mod process;
pub mod reconcile;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod requests;
pub mod shells;
pub mod sse;
pub mod version;

use self::{
    events::{PendingRequest, RunFailure, RunOutcome, Translated, Translator},
    http::{Http, HttpError},
    mcp::McpState,
    permissions::PermissionMode,
    process::{DataLayout, LayoutError, ReadyError, ServerProcess},
    reconcile::Snapshot,
    requests::{Intake, Reply, Requests, Settled},
    sse::StreamItem,
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, Capabilities, CloseMode,
        CompactOutcome, DecisionOutcome, HarnessCatalog, InstructionMode, InstructionSpec,
        InterruptOutcome, ModelCatalog, Phase, Proof, SettingsChange, StartSpec, Started, Support,
        Transmission, TransmitOutcome,
        canterlot_mcp::{Broker, BrokerEvent, ToolSet},
        cli_version::{CliVersion, VersionError},
        held_tool::HeldTools,
    },
    config::OpenCodeConfig,
    containment::Containment,
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
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
/// Весь `discover` при старте воркера: запуск сервера, версия и каталог.
const DISCOVER_WAIT: Duration = Duration::from_secs(20);
const INTERRUPT_WAIT: Duration = Duration::from_secs(5);
const POLL_EVERY: Duration = Duration::from_millis(100);
/// Эвристика: `connected` публикуется раньше инструментов, дебаунс `mcp.tools.changed`
/// около 100 мс. Публичного сигнала нет, запас проверяет L2 на повторных холодных стартах.
const MCP_SETTLE: Duration = Duration::from_millis(250);
/// Эвристика: сервер читает `AGENTS.md` по наблюдателю файла с дебаунсом 100 мс, сигнала нет.
/// Замер: 100 мс после записи мало, 200 мс хватает. Промах виден только на первом шаге.
const INSTRUCTIONS_SETTLE: Duration = Duration::from_millis(400);
const INSTRUCTIONS_FAILED: &str = "instructions_change_failed";
const ABORTED: &str = "aborted";
const TERM_GRACE: Duration = Duration::from_secs(2);
const KILL_WAIT: Duration = Duration::from_secs(2);
const STREAM_BUFFER: usize = 4096;
const PATCH_BATCH: usize = 16;
/// Паузы перед подпиской заново. Сервер живёт отдельно от потока: обычно хватает первой.
const RECONNECT_DELAYS: [Duration; 4] = [
    Duration::from_millis(0),
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(3),
];
/// Хвост событий после выхода сервера.
const EXIT_DRAIN: Duration = Duration::from_millis(500);
const SESSION_TITLE: &str = "canterlot";
const STALE_INPUT_NOTICE: &str = "Ввод остался в очереди после падения и не выполнен: он снят, чтобы не исполниться вместе со следующим.";
const ERROR_TEXT_BYTES: usize = 4096;
/// Схема `GET /openapi.json` проверенной версии (`evidence/opencode/openapi-2.1.4.sha256`).
pub const VERIFIED_OPENAPI_SHA256: &str =
    "23a25e5604b1274987ab7093be1eea517bd93d588e849ea53ad93a977878db3a";

/// Итог проверки при старте воркера: версия CLI и каталог моделей.
#[derive(Clone)]
pub struct OpenCodeCatalog {
    pub version: CliVersion,
    pub models: Vec<catalog::ModelEntry>,
}

impl OpenCodeCatalog {
    pub fn harness(&self) -> HarnessCatalog {
        HarnessCatalog {
            harness: "opencode",
            provider: "opencode",
            title: "OpenCode",
            // Картинку примет любая сессия: модель без такого ввода получит путь к файлу.
            images: true,
            settings_midsession: true,
            settings_midsession_effort: true,
            permission_modes: vec!["owner", "ask", "bypass", "read_only"],
            models: self
                .models
                .iter()
                .map(|entry| ModelCatalog {
                    id: entry.id.clone(),
                    title: entry.title.clone(),
                    efforts: entry.efforts.clone(),
                    images: entry.images,
                    context_window: entry.context_window,
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub enum DiscoverError {
    Version(VersionError),
    DataRoot(LayoutError),
    /// Сервер не поднялся или не отдал список моделей.
    Catalog,
    /// Проверка не уложилась в срок: дальше ждут сопровождения других харнессов.
    Timeout,
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
    discover_within(cfg, data_dir, DISCOVER_WAIT).await
}

/// Отмена по сроку безопасна: брошенный процесс снимают EOF stdin и SIGTERM при `Drop`.
pub async fn discover_within(
    cfg: &OpenCodeConfig,
    data_dir: &Path,
    limit: Duration,
) -> Result<OpenCodeCatalog, DiscoverError> {
    tokio::time::timeout(limit, discover_inner(cfg, data_dir))
        .await
        .map_err(|_| DiscoverError::Timeout)?
}

async fn discover_inner(
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
    let models = probe_models(cfg, env, &layout.tmp())
        .await
        .map_err(|_| DiscoverError::Catalog)?;
    Ok(OpenCodeCatalog { version, models })
}

// Сервер ждёт активации плагинов 5 с и отвечает 503: это «ещё нет», а не отказ. Плагины
// владельца на холодном старте дольше этого срока.
async fn models_reply(http: &Http, path: &str, limit: Duration) -> Result<Value, HttpError> {
    let deadline = Instant::now() + limit;
    loop {
        match http.get(path, REQUEST_WAIT).await {
            Err(HttpError::Status { status: 503, .. }) if Instant::now() < deadline => {
                tokio::time::sleep(POLL_EVERY).await;
            }
            reply => return reply,
        }
    }
}

// Список моделей зависит от учётных данных и конфигурации, поэтому его отдаёт сам сервер.
// Запуск короткий и без сессий, на общей базе canterlot это безопасно.
async fn probe_models(
    cfg: &OpenCodeConfig,
    mut env: Vec<(OsString, OsString)>,
    workdir: &Path,
) -> Result<Vec<catalog::ModelEntry>, ()> {
    let password = random_password();
    env.push(("OPENCODE_PASSWORD".into(), password.clone().into()));
    let mut proc = ServerProcess::spawn_probe(&cfg.executable, workdir, &env)
        .await
        .map_err(drop)?;
    let models = async {
        let base = proc.ready_url(READY_WAIT).await.map_err(drop)?;
        let http = Http::new(base, password).await.map_err(drop)?;
        let models = models_reply(&http, "/api/model", DISCOVER_WAIT)
            .await
            .map_err(drop)?;
        let default = http.get("/api/model/default", REQUEST_WAIT).await.ok();
        let mut entries = catalog::entries(&models, default.as_ref(), &cfg.providers);
        if let Some(model) = &cfg.default_model {
            catalog::prefer(&mut entries, model);
        }
        Ok(entries)
    }
    .await;
    proc.close_stdin();
    if tokio::time::timeout(TERM_GRACE, proc.wait()).await.is_err() {
        proc.signal(rustix::process::Signal::KILL);
        let _ = tokio::time::timeout(KILL_WAIT, proc.wait()).await;
    }
    models
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
    if !effort.is_empty() && effort != catalog::NO_VARIANT {
        reference["variant"] = effort.into();
    }
    Some(reference)
}

fn owner_instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn random_password() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
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
    /// Последний принятый ввод: ему приписываются запуски без ввода.
    last: Option<Uuid>,
    /// Идёт запуск без ввода, приписанный этой команде.
    adopted: Option<Uuid>,
    /// `active` записан в очередь сервера.
    enqueued: bool,
    /// `active` стал сообщением: терминал периода закрывает и его.
    delivered: bool,
    /// Идёт период, начатый не вводом `active`: его конец может оказаться чужим.
    foreign: bool,
    /// Ход закрыт по проекции: терминал этого периода мог ещё идти по новому потоку.
    stale_terminal: bool,
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

/// Список фоновых команд после прерывания; отдельная задача по той же причине.
struct ShellList {
    command: Uuid,
    task: tokio::task::JoinHandle<Option<Value>>,
}

impl Drop for ShellList {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn shells_done(list: &mut Option<ShellList>) -> Option<Value> {
    match list {
        Some(pending) => (&mut pending.task).await.ok().flatten(),
        None => std::future::pending().await,
    }
}

async fn lookup_done(lookup: &mut Option<Lookup>) -> bool {
    match lookup {
        Some(pending) => (&mut pending.task).await.unwrap_or(false),
        None => std::future::pending().await,
    }
}

/// Подписка и чтение проекций в отдельной задаче: `next_event` могут бросить в любой момент.
struct Resync {
    task: tokio::task::JoinHandle<Option<Resynced>>,
}

struct Resynced {
    /// `None`: поток жив, потерян один кадр, подписываться заново незачем.
    stream: Option<EventStream>,
    snapshot: Snapshot,
}

impl Drop for Resync {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn resync_done(resync: &mut Option<Resync>) -> Option<Resynced> {
    match resync {
        Some(pending) => (&mut pending.task).await.ok().flatten(),
        None => std::future::pending().await,
    }
}

async fn subscribe(http: &Http, wait: Duration) -> Option<EventStream> {
    let response = http.events(wait).await.ok()?;
    let (sender, mut items) = mpsc::channel(STREAM_BUFFER);
    let task = tokio::spawn(sse::pump(response, sender));
    let stream = async {
        while let Some(item) = items.recv().await {
            match item {
                StreamItem::Event(event) if event["type"] == "server.connected" => return true,
                StreamItem::Ended => return false,
                _ => {}
            }
        }
        false
    };
    let connected = tokio::time::timeout(wait, stream).await;
    let stream = EventStream { items, task };
    matches!(connected, Ok(true)).then_some(stream)
}

// Правила потомку ставятся отдельно: завершённого уже нет, остальной сохранил бы прежние.
// Пачками, а не по одному: это ждёт цикл супервизора, а с ним решения и остановка.
async fn patch_children(http: &Http, children: &[String], body: &Value) -> Result<(), HttpError> {
    for batch in children.chunks(PATCH_BATCH) {
        let mut patches = JoinSet::new();
        for child in batch {
            let (http, body) = (http.clone(), body.clone());
            let path = format!("/api/session/{child}");
            patches.spawn(async move { http.patch(&path, &body, REQUEST_WAIT).await });
        }
        while let Some(done) = patches.join_next().await {
            match done.map_err(|_| HttpError::Lost)? {
                Ok(_) | Err(HttpError::Status { status: 404, .. }) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

// Подписка раньше чтения: иначе событие между ними потеряно навсегда.
async fn resubscribe(http: Http, session: String, children: Vec<String>) -> Option<Resynced> {
    for delay in RECONNECT_DELAYS {
        tokio::time::sleep(delay).await;
        let Some(stream) = subscribe(&http, REQUEST_WAIT).await else {
            continue;
        };
        if let Ok(snapshot) = reconcile::read(&http, &session, children.clone()).await {
            return Some(Resynced {
                stream: Some(stream),
                snapshot,
            });
        }
    }
    None
}

async fn reread(http: Http, session: String, children: Vec<String>) -> Option<Resynced> {
    let snapshot = reconcile::read(&http, &session, children).await.ok()?;
    Some(Resynced {
        stream: None,
        snapshot,
    })
}

/// Настройки сессии на случай отказа во вводе.
struct Settings {
    mode: PermissionMode,
    model: String,
    effort: String,
    instructions: InstructionSpec,
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
    shells: Option<ShellList>,
    resync: Option<Resync>,
    held: HeldTools,
    /// Вызовы, по которым был запрос: пометки CLI у них нет.
    asked: HashSet<String>,
    requests: Requests,
    refusals: JoinSet<()>,
    /// Снимки фоновых команд для журнала сирот.
    shell_journal: JoinSet<()>,
    /// Журнал сирот живой сессии: снимки идут строго по очереди.
    orphan_journal: Option<orphans::Journal>,
    session: String,
    mode: PermissionMode,
    /// На сервере могли остаться другие правила: перед вводом они ставятся заново.
    rules_dirty: bool,
    /// Режим только что вышел из `read_only`: первому вводу после этого нужно напоминание.
    exit_note: bool,
    model: String,
    effort: String,
    /// То же для модели.
    model_dirty: bool,
    /// Варианты доступных моделей: сервер принимает неизвестный и падает только на ходе.
    variants: HashMap<String, Vec<String>>,
    /// Какие модели принимают картинки.
    image_models: HashMap<String, bool>,
    /// Инструкции, которые считает действующими хаб.
    instructions: InstructionSpec,
    /// Инструкции на сервере: после отказа во вводе расходятся с `instructions`.
    applied: InstructionSpec,
    /// Спека запуска: перезапуск повторяет её с той же сессией.
    spec: Option<StartSpec>,
    /// Сжатие в этом периоде отказало из-за переполнения: сервер больше ничего не сделает.
    overflowed: bool,
    /// Отказы подряд: повтор предлагает сжатие.
    stuck: failure::StuckWatch,
    /// Сообщение ручного сжатия, чей итог ждёт супервизор.
    compaction: Option<String>,
    version: Option<CliVersion>,
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
            shells: None,
            resync: None,
            held: HeldTools::default(),
            asked: HashSet::new(),
            requests: Requests::default(),
            refusals: JoinSet::new(),
            shell_journal: JoinSet::new(),
            orphan_journal: None,
            session: String::new(),
            mode: PermissionMode::Owner,
            rules_dirty: false,
            exit_note: false,
            model: String::new(),
            effort: String::new(),
            model_dirty: false,
            variants: HashMap::new(),
            image_models: HashMap::new(),
            instructions: owner_instructions(),
            applied: owner_instructions(),
            spec: None,
            overflowed: false,
            stuck: failure::StuckWatch::default(),
            compaction: None,
            version: None,
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
            field_names: Vec::new(),
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
        let wait = self.ready_wait.min(REQUEST_WAIT);
        let stream = subscribe(self.http()?, wait)
            .await
            .ok_or(AdapterError::StartFailed("server_unreachable"))?;
        self.stream = Some(stream);
        Ok(())
    }

    async fn check_identity(&mut self) -> Result<(), AdapterError> {
        let version = self
            .version
            .clone()
            .ok_or(AdapterError::StartFailed("cli_version_failed"))?;
        let pid = self.proc.as_ref().map(|proc| proc.pid);
        let info = self
            .http()?
            .get("/api/info", REQUEST_WAIT)
            .await
            .map_err(|_| AdapterError::StartFailed("server_unreachable"))?;
        if info["version"].as_str() != Some(version_label(&version))
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
        let stored = self
            .http()?
            .get(&path, REQUEST_WAIT)
            .await
            .map_err(failed)?;
        // Режим мог смениться, пока исполнение стояло: модель увидит тот же `<system-update>`.
        if self.mode != PermissionMode::ReadOnly
            && stored.pointer("/data/permissions")
                == Some(&PermissionMode::ReadOnly.session_rules())
        {
            self.exit_note = true;
        }
        let body = json!({"permissions": self.mode.session_rules()});
        self.http()?
            .patch(&path, &body, REQUEST_WAIT)
            .await
            .map_err(failed)?;
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
    async fn model_list(&mut self) -> Value {
        match self.http {
            Some(ref http) => models_reply(http, "/api/model", self.ready_wait)
                .await
                .unwrap_or(Value::Null),
            None => Value::Null,
        }
    }

    // Окружение зависит от инструкций: своё у каждого запуска, пароль тоже.
    async fn launch_env(
        &self,
        instructions: &InstructionSpec,
    ) -> Result<(Vec<(OsString, OsString)>, String), AdapterError> {
        let layout = DataLayout::for_execution(&self.cfg, &self.exec_dir);
        let mut env = prepared_env(&self.cfg, &layout, self.exec_dir.join("tmp"))
            .await
            .map_err(|error| {
                AdapterError::StartFailed(match error {
                    LayoutError::Refused => "data_root_refused",
                    LayoutError::Io(_) => "data_root_failed",
                })
            })?;
        let owner = instructions::owner_dir(self.cfg.owner_config.as_deref(), &env);
        env.extend(
            instructions::prepare(&self.exec_dir, owner, instructions)
                .await
                .map_err(|_| AdapterError::StartFailed("instructions_failed"))?,
        );
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
        Ok((env, password))
    }

    // Запуск сервера до готовой сессии: первый старт и перезапуск со сменой инструкций.
    async fn launch(
        &mut self,
        spec: &StartSpec,
        env: &[(OsString, OsString)],
        password: String,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        let base = self.spawn_server(spec, env, group).await?;
        let http = Http::new(base, password)
            .await
            .map_err(|_| AdapterError::StartFailed("server_unreachable"))?;
        self.http = Some(http);
        self.open_stream().await?;
        self.check_identity().await?;
        if matches!(self.version, Some(CliVersion::UntestedNewer(_))) {
            self.check_schema().await?;
        }
        self.session = match &spec.native_conversation_id {
            Some(native) => self.resume_session(spec, native).await?,
            None => self.create_session(spec).await?,
        };
        self.wait_mcp().await?;
        self.wait_agents().await
    }

    async fn start_inner(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        self.mode = PermissionMode::parse(&spec.permission_mode)
            .ok_or(AdapterError::StartFailed("unknown_permission_mode"))?;
        let layout = DataLayout::for_execution(&self.cfg, &self.exec_dir);
        let env = prepared_env(&self.cfg, &layout, self.exec_dir.join("tmp"))
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
        if matches!(version, CliVersion::UntestedNewer(_)) {
            self.diagnostic("untested_cli_version", false);
        }
        self.version = Some(version);
        self.broker =
            Some(Broker::bind_with(&self.exec_dir, ToolSet::LimitsOnly, "opencode").await?);
        if let Some(native) = &spec.native_conversation_id {
            self.sweep_orphans(native).await;
        }
        let (env, password) = self.launch_env(&spec.instructions).await?;
        self.launch(spec, &env, password, group).await?;
        self.model = spec.model.clone();
        self.effort = spec.effort.clone();
        self.instructions = spec.instructions.clone();
        self.applied = spec.instructions.clone();
        self.spec = Some(spec.clone());
        let models = self.model_list().await;
        self.variants = catalog::variants(&models);
        self.image_models = catalog::image_support(&models);
        self.translator = Some(Translator::new(
            &self.session,
            catalog::context_windows(&models),
        ));
        if spec.native_conversation_id.is_some() {
            self.reconcile_resume().await?;
        }
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

    fn report_restart(&mut self) {
        if let Some(proc) = self.proc.as_ref() {
            self.events.push_back(AdapterEvent::CliRestarted {
                cli_pid: proc.pid,
                cli_start_ticks: proc.start_ticks,
            });
        }
    }

    async fn relaunch_with(
        &mut self,
        spec: &StartSpec,
        instructions: &InstructionSpec,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        let (env, password) = self.launch_env(instructions).await?;
        self.launch(spec, &env, password, group).await
    }

    // «Как у владельца» отличается окружением запуска: такая смена требует нового процесса,
    // фоновые команды сессии уходят вместе со старым. Сессия та же.
    async fn relaunch(
        &mut self,
        next: &InstructionSpec,
        group: &dyn Containment,
    ) -> Result<(), &'static str> {
        let Some(mut spec) = self.spec.clone() else {
            return Err(INSTRUCTIONS_FAILED);
        };
        spec.native_conversation_id = Some(self.session.clone());
        spec.model = self.model.clone();
        spec.effort = self.effort.clone();
        // Каталог готовится раньше остановки: отказ здесь не трогает работающий сервер.
        let (env, password) = self
            .launch_env(next)
            .await
            .map_err(|_| INSTRUCTIONS_FAILED)?;
        // Сверка старого сервера новому не поможет: у него свой поток и своё чтение на старте.
        self.resync = None;
        let _ = self.stop_server(true).await;
        if self.launch(&spec, &env, password, group).await.is_ok() {
            self.report_restart();
            return Ok(());
        }
        let _ = self.stop_server(false).await;
        let previous = self.applied.clone();
        if self.relaunch_with(&spec, &previous, group).await.is_ok() {
            self.report_restart();
        } else {
            let _ = self.stop_server(false).await;
            self.finish_exit(None);
        }
        Err(INSTRUCTIONS_FAILED)
    }

    async fn apply_instructions(
        &mut self,
        wanted: &InstructionSpec,
        group: &dyn Containment,
    ) -> Result<(), &'static str> {
        if !wanted.same_effect(&self.applied) {
            let owner = InstructionMode::Owner;
            if (wanted.mode == owner) != (self.applied.mode == owner) {
                self.relaunch(wanted, group).await?;
            } else {
                instructions::rewrite(&self.exec_dir, wanted)
                    .await
                    .map_err(|_| INSTRUCTIONS_FAILED)?;
                tokio::time::sleep(INSTRUCTIONS_SETTLE).await;
            }
            self.applied = wanted.clone();
        }
        self.instructions = wanted.clone();
        Ok(())
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

    fn error_block(&mut self, failure: &RunFailure, command: Option<Uuid>) {
        let mut text = format!("Ошибка OpenCode ({}): {}", failure.kind, failure.message);
        text.truncate(text.floor_char_boundary(ERROR_TEXT_BYTES));
        self.text_block(BlockKind::Error, command, text);
    }

    fn text_block(&mut self, kind: BlockKind, command: Option<Uuid>, text: String) {
        let block_id = Uuid::now_v7();
        self.events.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind,
            input_command_id: command,
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

    fn on_run_started(&mut self) {
        if self.delivery.active.is_some() {
            // Период начался раньше записи нашего ввода: начал его не ввод.
            self.delivery.foreign |= !self.delivery.enqueued;
            return;
        }
        if self.delivery.adopted.is_some() {
            return;
        }
        let Some(command) = self.delivery.last else {
            self.diagnostic("autonomous_run_unattributed", false);
            return;
        };
        self.delivery.adopted = Some(command);
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_run(Some(command));
        }
        self.events.push_back(AdapterEvent::RunAdopted {
            input_command_id: command,
        });
    }

    // Лимит пишется внутри хода: хаб привязывает его к текущему вводу. Остальное идёт за концом
    // хода, чтобы расход и блок ошибки успели попасть в журнал до остановки.
    fn judge(&mut self, outcome: &RunOutcome) -> Vec<AdapterEvent> {
        let overflowed = std::mem::take(&mut self.overflowed);
        let failure = match outcome {
            RunOutcome::Failed(failure) => failure,
            RunOutcome::Succeeded => {
                self.stuck.reset();
                return Vec::new();
            }
            // Кадр отказа идёт раньше терминала, но мог выпасть в разрыв, а карточка ждёт итог.
            RunOutcome::Interrupted => {
                self.on_compaction_ended(Some(RunFailure::from_error(None)), true);
                return Vec::new();
            }
        };
        if let Some(fact) = failure::limit_fact(failure, now_ms()) {
            self.events.push_back(AdapterEvent::Limit(fact));
        }
        let mut follow = vec![AdapterEvent::Diagnostic {
            kind: "provider_failure",
            event_type: failure.kind.clone(),
            required: false,
            field_names: Vec::new(),
        }];
        if failure.kind == failure::AUTH_FAILURE {
            follow.push(AdapterEvent::Diagnostic {
                kind: "auth_required",
                event_type: "auth_required".into(),
                required: true,
                field_names: Vec::new(),
            });
        } else if failure.overflow || overflowed {
            follow.push(AdapterEvent::ContextExhausted);
        } else if self.stuck.on_failure(failure) {
            follow.push(AdapterEvent::Diagnostic {
                kind: "session_stuck",
                event_type: "session_stuck".into(),
                required: false,
                field_names: Vec::new(),
            });
            follow.push(AdapterEvent::SessionStuck);
        }
        follow
    }

    fn on_compaction_ended(&mut self, failed: Option<RunFailure>, manual: bool) {
        self.overflowed |= failed.as_ref().is_some_and(|failure| failure.overflow);
        if failed.is_none() {
            self.stuck.reset();
        }
        if !manual || self.compaction.take().is_none() {
            return;
        }
        // Период сжатия кончается успешным терминалом: блока ошибки хода не будет, а отмена
        // владельцем ошибкой не считается.
        if let Some(failure) = failed
            .as_ref()
            .filter(|failure| failure.kind != ABORTED && !failure.message.is_empty())
        {
            self.error_block(failure, self.delivery.adopted.or(self.delivery.last));
        }
        self.events.push_back(AdapterEvent::Compacted {
            ok: failed.is_none(),
        });
    }

    // Итог ручного сжатия мог выпасть в разрыв: сообщение сжатия лежит в проекции.
    fn settle_compaction(&mut self, snapshot: &Snapshot) {
        let Some(id) = self.compaction.as_deref() else {
            return;
        };
        let Some(message) = snapshot
            .messages
            .iter()
            .find(|message| message["id"].as_str() == Some(id))
        else {
            return;
        };
        match message["status"].as_str() {
            Some("completed") => self.on_compaction_ended(None, true),
            Some("failed") => {
                self.on_compaction_ended(Some(RunFailure::from_error(message.get("error"))), true)
            }
            _ => {}
        }
    }

    fn on_run_ended(&mut self, outcome: RunOutcome) {
        self.held.release_all(&mut self.events);
        self.asked.clear();
        self.requests.drop_session(&self.session);
        let follow = self.judge(&outcome);
        let Some(command) = self.delivery.active.or(self.delivery.adopted) else {
            // Запуск без единой команды: приписать некому, блоков хода нет.
            self.awaiting_input();
            self.events.extend(follow);
            return;
        };
        if let RunOutcome::Failed(failure) = &outcome {
            self.error_block(failure, Some(command));
        }
        let interrupted = matches!(outcome, RunOutcome::Interrupted);
        // Конец чужого периода, а наш ввод ещё в очереди: его ход впереди, расход пойдёт к нему.
        if self.delivery.active.is_some()
            && self.delivery.foreign
            && !self.delivery.delivered
            && !interrupted
        {
            self.delivery.foreign = false;
            self.events.extend(follow);
            return;
        }
        self.delivery.active = None;
        self.delivery.adopted = None;
        self.delivery.foreign = false;
        if self.delivery.unconfirmed == Some(command) {
            self.delivery.unconfirmed = None;
            self.lookup = None;
        }
        if let Some(translator) = self.translator.as_mut() {
            self.events.push_back(translator.finish_run(command));
        }
        self.awaiting_input();
        self.events.extend(follow);
        if interrupted {
            self.list_shells(command);
        }
    }

    // После терминала: команды прерванного хода к этому моменту уже сняты сервером.
    fn list_shells(&mut self, command: Uuid) {
        let Some(http) = self.http.clone() else {
            return;
        };
        let task = tokio::spawn(async move { http.get("/api/shell", REQUEST_WAIT).await.ok() });
        self.shells = Some(ShellList { command, task });
    }

    fn on_shells(&mut self, list: Option<Value>) {
        let Some(pending) = self.shells.take() else {
            return;
        };
        let commands = match (&list, &self.translator) {
            (Some(list), Some(translator)) => {
                shells::running(list, |session| translator.owns(session))
            }
            _ => Vec::new(),
        };
        if !commands.is_empty() {
            self.text_block(
                BlockKind::Warning,
                Some(pending.command),
                shells::notice(&commands),
            );
        }
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
        self.delivery.last = Some(command);
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

    fn turn_running(&self) -> bool {
        self.delivery.active.is_some() || self.delivery.adopted.is_some()
    }

    // Ответ без ожидания итога: 404 значит, что сервер уже снял запрос сам. Цикл супервизора
    // не ждёт сервер.
    fn send_refusal(&mut self, reply: Reply) {
        let Some(http) = self.http.clone() else {
            return;
        };
        while self.refusals.try_join_next().is_some() {}
        self.refusals.spawn(async move {
            let _ = reply.send(&http).await;
        });
    }

    fn on_request(&mut self, request: PendingRequest) {
        if let Some(item) = &request.item {
            self.asked.insert(item.clone());
            self.held.release(item, None, &mut self.events);
        }
        let id = request.id.clone();
        match self.requests.intake(request) {
            // Вне хода супервизор карточку не откроет: отказ сразу, иначе вызов ждал бы вечно.
            Intake::Open(..) if !self.turn_running() => {
                self.diagnostic("request_outside_turn", false);
                if let Some(reply) = self.requests.refuse(&id, requests::REFUSED) {
                    self.send_refusal(reply);
                }
            }
            Intake::Open(event, phase) => {
                self.events.push_back(event);
                self.events.push_back(AdapterEvent::Phase {
                    phase,
                    tool_name: None,
                    summary: None,
                });
            }
            Intake::Refuse(reply, kind) => {
                self.diagnostic(kind, false);
                self.send_refusal(reply);
            }
            Intake::Duplicate => {}
        }
    }

    // Запрос снял сам сервер: отказ по соседу в пачке или прерывание.
    fn close_request(&mut self, request_id: String) {
        if self.turn_running() {
            self.events.push_back(AdapterEvent::RequestClosed {
                request_id,
                reason: "cli_cancelled".into(),
            });
        }
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
            Translated::ToolRunning(item) => self.release_tool(&item),
            Translated::ToolDone(item) => {
                self.release_tool(&item);
                for request_id in self.requests.close_item(&item) {
                    self.close_request(request_id);
                }
            }
            Translated::Request(request) => self.on_request(request),
            Translated::RequestSettled(request_id) => {
                if self.requests.on_settled(&request_id) == Some(Settled::Cleared) {
                    self.close_request(request_id);
                }
            }
            Translated::InboxSeen { id, delivered } => {
                if let Some(command) = self.delivery.active
                    && input::message_id(command) == id
                {
                    self.delivery.enqueued = true;
                    self.delivery.delivered |= delivered;
                }
                if let Some(command) = self.delivery.unconfirmed
                    && input::message_id(command) == id
                {
                    self.prove_late(command);
                }
            }
            Translated::RunStarted => {
                self.delivery.stale_terminal = false;
                self.on_run_started();
            }
            Translated::RunEnded(_) if std::mem::take(&mut self.delivery.stale_terminal) => {}
            Translated::RunEnded(outcome) => self.on_run_ended(outcome),
            Translated::ShellStarted => self.journal_shells(),
            Translated::CompactionEnded { failed, manual } => {
                self.on_compaction_ended(failed, manual);
            }
        }
    }

    fn orphan_journal(&self, session: &str) -> PathBuf {
        let layout = DataLayout::for_execution(&self.cfg, &self.exec_dir);
        orphans::journal_path(layout.root(), session)
    }

    fn journal_shells(&mut self) {
        let (Some(http), Some(translator)) = (self.http.clone(), self.translator.as_ref()) else {
            return;
        };
        let owners = translator
            .children()
            .map(str::to_owned)
            .chain([self.session.clone()])
            .collect();
        while self.shell_journal.try_join_next().is_some() {}
        let path = self.orphan_journal(&self.session);
        let journal = self
            .orphan_journal
            .get_or_insert_with(|| orphans::Journal::new(path));
        self.shell_journal
            .spawn(journal.clone().record(http, owners));
    }

    // Запись осталась только после падения и сервера, и супервизора: дерево процессов уже не
    // поможет, снимать некому, кроме следующего запуска.
    async fn sweep_orphans(&mut self, session: &str) {
        let path = self.orphan_journal(session);
        let swept = tokio::task::spawn_blocking(move || orphans::sweep(&path)).await;
        if matches!(swept, Ok(Ok(count)) if count > 0) {
            self.diagnostic("opencode_orphans_stopped", false);
        }
    }

    fn apply_items(&mut self, items: Vec<Translated>) {
        for item in items {
            self.on_translated(item);
        }
    }

    // Запрос с сервера идёт тем же путём, что событие: повтор отсекает `Requests`.
    fn reopen_requests(&mut self, snapshot: &Snapshot) {
        let Some(translator) = self.translator.as_mut() else {
            return;
        };
        let mut items = Vec::new();
        for request in &snapshot.permissions {
            items.extend(translator.pending_permission(request));
        }
        for form in &snapshot.forms {
            items.extend(translator.pending_form(form));
        }
        self.apply_items(items);
        let alive: HashSet<&str> = snapshot
            .permissions
            .iter()
            .chain(&snapshot.forms)
            .filter_map(|request| request.get("id")?.as_str())
            .collect();
        for request_id in self.requests.gone(&alive) {
            self.close_request(request_id);
        }
    }

    fn close_by_projection(&mut self, snapshot: &Snapshot) {
        let input = self.delivery.active.map(input::message_id);
        if let Some(id) = &input {
            let queued = snapshot.inbox.iter().any(|item| item == id);
            // Ввод ещё в очереди или снимок старше запроса: его ход впереди. В оборванном хвосте
            // ввода может просто не быть, а конец хода уже не придёт.
            let unseen = !reconcile::delivered(&snapshot.messages, id) && !snapshot.truncated;
            if queued || unseen {
                return;
            }
        }
        let outcome = reconcile::last_outcome(&snapshot.messages, input.as_deref());
        let outcome = match outcome.and_then(|idle| idle.get("outcome")?.as_str()) {
            Some("succeeded") => RunOutcome::Succeeded,
            Some("failed") => RunOutcome::Failed(RunFailure::from_error(
                outcome.and_then(|idle| idle.get("error")),
            )),
            // Сервер жив и не работает, а итога нет: ход не закрыть иначе.
            _ => RunOutcome::Interrupted,
        };
        self.delivery.stale_terminal = true;
        self.on_run_ended(outcome);
    }

    // Единственный путь сверки на живом сервере: потерянное приходит теми же обработчиками.
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        if snapshot.truncated {
            self.diagnostic("history_unreadable", false);
        }
        let Some(translator) = self.translator.as_mut() else {
            return;
        };
        for child in &snapshot.children {
            translator.add_child(child.clone());
        }
        let unseen = translator.has_unseen(&snapshot.messages);
        let inputs: Vec<(Uuid, bool)> = [self.delivery.active, self.delivery.unconfirmed]
            .into_iter()
            .flatten()
            .map(|command| {
                let id = input::message_id(command);
                (command, reconcile::delivered(&snapshot.messages, &id))
            })
            .collect();
        for (command, delivered) in inputs {
            let id = input::message_id(command);
            if delivered || snapshot.inbox.contains(&id) {
                self.on_translated(Translated::InboxSeen { id, delivered });
            }
        }
        // Период без ввода начался и, возможно, кончился в разрыве.
        if !self.turn_running() && (snapshot.running || unseen) {
            self.on_run_started();
        }
        if let Some(translator) = self.translator.as_mut() {
            let items = translator.replay(&snapshot.messages, false, true);
            self.apply_items(items);
        }
        self.reopen_requests(&snapshot);
        self.settle_compaction(&snapshot);
        if self.turn_running() && !snapshot.running {
            self.close_by_projection(&snapshot);
        } else if self.turn_running() && !self.requests.has_open() {
            self.events.push_back(AdapterEvent::Phase {
                phase: Phase::Generating,
                tool_name: None,
                summary: None,
            });
        }
    }

    fn on_resynced(&mut self, done: Option<Resynced>) {
        self.resync = None;
        let Some(done) = done else {
            if self.stream.is_none() {
                self.give_up_stream();
            }
            return;
        };
        if let Some(stream) = done.stream {
            self.stream = Some(stream);
        }
        self.apply_snapshot(done.snapshot);
    }

    fn start_resync(&mut self, reconnect: bool) {
        let Some(http) = self.http.clone() else {
            return;
        };
        if self.resync.is_some() {
            return;
        }
        let session = self.session.clone();
        let children: Vec<String> = self
            .translator
            .iter()
            .flat_map(|translator| translator.children().map(str::to_owned))
            .collect();
        let task = if reconnect {
            tokio::spawn(resubscribe(http, session, children))
        } else {
            tokio::spawn(reread(http, session, children))
        };
        self.resync = Some(Resync { task });
    }

    // Сервер остался без потока и без проекций: конца хода не будет, поэтому он снимается,
    // а исполнение получает ProcessExited вместо вечного ожидания.
    fn give_up_stream(&mut self) {
        self.diagnostic("event_stream_lost", false);
        if let Some(proc) = self.proc.as_mut() {
            proc.close_stdin();
            proc.signal(rustix::process::Signal::TERM);
            self.kill_at = Some(Instant::now() + TERM_GRACE);
        }
    }

    // Запуск после падения: исполнения нет, поэтому `running` значит сироту, а ввод, оставшийся
    // в очереди, снимается: хаб считает его неопределённым, а следующий ввод исполнил бы его.
    async fn reconcile_resume(&mut self) -> Result<(), AdapterError> {
        let snapshot = reconcile::read(self.http()?, &self.session, Vec::new())
            .await
            .map_err(|_| AdapterError::StartFailed("resume_failed"))?;
        // Субагент продолжает старого потомка по `sessionID`, а копию правил тот получил при
        // создании: без этого он обошёл бы режим возобновления.
        let rules = json!({"permissions": self.mode.session_rules()});
        patch_children(self.http()?, &snapshot.children, &rules)
            .await
            .map_err(|_| AdapterError::StartFailed("resume_failed"))?;
        if snapshot.truncated {
            self.diagnostic("history_unreadable", false);
        }
        if let Some(translator) = self.translator.as_mut() {
            for child in &snapshot.children {
                translator.add_child(child.clone());
            }
            let mut items = translator.replay(&snapshot.messages, !snapshot.running, false);
            // История, а не работа: сессия ждёт ввода, фаза прошлых вызовов к ней не относится.
            items.retain(|item| !matches!(item, Translated::Event(AdapterEvent::Phase { .. })));
            self.apply_items(items);
        }
        for id in &snapshot.inbox {
            let path = format!("/api/session/{}/inbox/{id}", self.session);
            self.http()?
                .delete(&path, &[], REQUEST_WAIT)
                .await
                .map_err(|_| AdapterError::StartFailed("resume_failed"))?;
        }
        if !snapshot.inbox.is_empty() {
            self.resume_notice(STALE_INPUT_NOTICE.to_owned());
        }
        Ok(())
    }

    // Команды старого исполнения хаб не примет: блок без привязки к вводу.
    fn resume_notice(&mut self, text: String) {
        self.text_block(BlockKind::Warning, None, text);
    }

    fn on_lookup(&mut self, found: bool) {
        let Some(lookup) = self.lookup.take() else {
            return;
        };
        if self.delivery.unconfirmed != Some(lookup.command) {
            return;
        }
        if found {
            self.exit_note = false;
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

    fn known_model(&self, model: &str, effort: &str) -> bool {
        self.variants.get(model).is_some_and(|variants| {
            effort.is_empty()
                || effort == catalog::NO_VARIANT
                || variants.iter().any(|variant| variant == effort)
        })
    }

    fn settings_error(&self, change: &SettingsChange) -> Option<&'static str> {
        let model = change.model.as_deref().unwrap_or(&self.model);
        let effort = change.effort.as_deref().unwrap_or(&self.effort);
        let moved = model != self.model || effort != self.effort;
        let unsupported = (moved && !self.known_model(model, effort))
            || change
                .permission_mode
                .as_deref()
                .is_some_and(|mode| PermissionMode::parse(mode).is_none());
        unsupported.then_some("settings_unavailable")
    }

    // Правила действуют со следующей проверки. Потомок получает копию при создании и смены
    // родителя не видит, поэтому свой запрос каждому; завершённого потомка уже нет.
    async fn apply_mode(
        &mut self,
        http: &Http,
        wanted: PermissionMode,
    ) -> Result<(), &'static str> {
        if wanted == self.mode && !self.rules_dirty {
            return Ok(());
        }
        self.rules_dirty = true;
        let body = json!({"permissions": wanted.session_rules()});
        let children: Vec<String> = self
            .translator
            .iter()
            .flat_map(|translator| translator.children().map(str::to_owned))
            .collect();
        let path = format!("/api/session/{}", self.session);
        http.patch(&path, &body, REQUEST_WAIT)
            .await
            .map_err(|_| "settings_unavailable")?;
        patch_children(http, &children, &body)
            .await
            .map_err(|_| "settings_unavailable")?;
        self.mode = wanted;
        self.rules_dirty = false;
        Ok(())
    }

    // Запроса на ввод с моделью нет: смена отдельным вызовом, со следующего периода работы.
    async fn apply_model(
        &mut self,
        http: &Http,
        model: String,
        effort: String,
    ) -> Result<(), &'static str> {
        if model == self.model && effort == self.effort && !self.model_dirty {
            return Ok(());
        }
        self.model_dirty = true;
        if let Some(reference) = model_ref(&model, &effort) {
            let path = format!("/api/session/{}/model", self.session);
            http.post(&path, &json!({"model": reference}), REQUEST_WAIT)
                .await
                .map_err(|_| "settings_unavailable")?;
        }
        self.model = model;
        self.effort = effort;
        self.model_dirty = false;
        Ok(())
    }

    // Настройки вступают в силу только с принятым вводом: хаб применяет их так же. Модель
    // первой: неудачная смена правил не должна оставить открытый режим.
    async fn apply_settings(
        &mut self,
        http: &Http,
        change: Option<&SettingsChange>,
    ) -> Result<(), &'static str> {
        let model = change.and_then(|change| change.model.clone());
        let effort = change.and_then(|change| change.effort.clone());
        let mode = change
            .and_then(|change| change.permission_mode.as_deref())
            .and_then(PermissionMode::parse)
            .unwrap_or(self.mode);
        let (model, effort) = (
            model.unwrap_or_else(|| self.model.clone()),
            effort.unwrap_or_else(|| self.effort.clone()),
        );
        self.apply_model(http, model, effort).await?;
        self.apply_mode(http, mode).await
    }

    fn restore_settings(&mut self, previous: Settings) {
        self.rules_dirty |= self.mode != previous.mode;
        self.model_dirty |= self.model != previous.model || self.effort != previous.effort;
        self.mode = previous.mode;
        self.model = previous.model;
        self.effort = previous.effort;
        self.instructions = previous.instructions;
    }

    async fn transmit_inner(
        &mut self,
        input: &Transmission,
        group: &dyn Containment,
    ) -> TransmitOutcome {
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
        // Вложение читает модель этого ввода, а не прежняя.
        let model = input
            .settings
            .as_ref()
            .and_then(|change| change.model.as_deref())
            .unwrap_or(&self.model);
        let images = self.image_models.get(model).copied().unwrap_or(false);
        let body = match input::prompt_body(input, images) {
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
                    self.exit_note = false;
                    self.delivery.last = Some(command);
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
        let previous_mode = self.mode;
        let previous = Settings {
            mode: self.mode,
            model: self.model.clone(),
            effort: self.effort.clone(),
            instructions: self.instructions.clone(),
        };
        let wanted = input
            .settings
            .as_ref()
            .and_then(|change| change.instructions.clone())
            .unwrap_or_else(|| self.instructions.clone());
        if let Err(reason) = self.apply_instructions(&wanted, group).await {
            return TransmitOutcome::Rejected(reason.into());
        }
        // Перезапуск сменил адрес и пароль.
        let Some(http) = self.http.clone() else {
            return TransmitOutcome::Undetermined;
        };
        let note_owed = self.exit_note;
        if let Err(reason) = self.apply_settings(&http, input.settings.as_ref()).await {
            self.restore_settings(previous);
            return TransmitOutcome::Rejected(reason.into());
        }
        // Сервер сам вернёт Code Mode как `<system-update>`; напоминание уходит только модели.
        match (previous_mode, self.mode) {
            (_, PermissionMode::ReadOnly) => self.exit_note = false,
            (PermissionMode::ReadOnly, _) => self.exit_note = true,
            _ => {}
        }
        let mut body = body;
        if self.exit_note {
            input::add_note(&mut body, input::READ_ONLY_EXIT_NOTE);
        }
        // Супервизор не узнал о запуске без ввода раньше ввода: сервер сольёт их в один период.
        let adopted = self.delivery.adopted.take();
        self.delivery.active = Some(command);
        self.delivery.enqueued = false;
        self.delivery.delivered = false;
        self.delivery.foreign = adopted.is_some();
        if let Some(translator) = self.translator.as_mut() {
            translator.begin_run(Some(command));
        }
        let path = format!("/api/session/{}/prompt", self.session);
        match http.post(&path, &body, self.deadline).await {
            Ok(reply) if input::accepted_id(&reply) == Some(id.as_str()) => {
                self.exit_note = false;
                self.delivery.last = Some(command);
                TransmitOutcome::Accepted(Proof {
                    command_id: command,
                    kind: input::PROOF_KIND,
                })
            }
            // Хаб новых настроек не применил: сервер возвращается к прежним.
            Err(HttpError::Status { status, tag }) if input::is_rejection(status) => {
                self.delivery.active = None;
                self.delivery.adopted = adopted;
                self.delivery.foreign = false;
                if let Some(translator) = self.translator.as_mut() {
                    translator.begin_run(adopted);
                }
                self.restore_settings(previous);
                self.exit_note = note_owed;
                // Период без ввода не должен застать режим, который хаб не принял. Не вышло:
                // правила остаются помеченными и ставятся перед следующим вводом.
                let _ = self.apply_mode(&http, self.mode).await;
                TransmitOutcome::Rejected(tag)
            }
            _ => {
                self.start_lookup(command);
                TransmitOutcome::Undetermined
            }
        }
    }

    // Кадр потерян, как при разрыве, но поток жив.
    fn recover_lost_frame(&mut self) {
        if let Some(translator) = self.translator.as_mut() {
            translator.on_gap();
        }
        self.start_resync(false);
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
                self.recover_lost_frame();
            }
            Some(StreamItem::Oversized) => {
                self.diagnostic("oversized_frame", false);
                self.recover_lost_frame();
            }
            Some(StreamItem::Ended) | None => self.on_stream_end(),
        }
    }

    #[cfg(feature = "synthetic-adapter")]
    pub fn native_session(&self) -> &str {
        &self.session
    }

    /// Разрыв потока по требованию теста с настоящим сервером: снаружи его не устроить.
    #[cfg(feature = "synthetic-adapter")]
    pub fn cut_stream(&mut self) {
        self.on_stream_end();
    }

    fn on_stream_end(&mut self) {
        self.stream = None;
        if let Some((status, _)) = self.exit.take() {
            self.finish_exit(status);
            return;
        }
        if let Some(translator) = self.translator.as_mut() {
            translator.on_gap();
        }
        self.resync = None;
        self.start_resync(true);
    }

    fn finish_exit(&mut self, status: Option<ExitStatus>) {
        use std::os::unix::process::ExitStatusExt as _;
        self.exited = true;
        self.events.push_back(AdapterEvent::ProcessExited {
            code: status.and_then(|status| status.code()),
            signal: status.and_then(|status| status.signal()),
        });
    }

    // Сервер уходит сам по EOF stdin; мягкая остановка ждёт этого, затем сигналы.
    async fn stop_server(&mut self, graceful: bool) -> Result<(), AdapterError> {
        use rustix::process::Signal;
        self.stream = None;
        let Some(proc) = self.proc.as_mut() else {
            return Ok(());
        };
        if proc.finished() {
            return Ok(());
        }
        proc.close_stdin();
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
        Ok(())
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
        // Пока идёт сверка, поток ждёт: конец хода не должен обогнать потерянное.
        let reading = self.resync.is_none();
        let item = async {
            match stream {
                Some(stream) if reading => stream.items.recv().await,
                _ => std::future::pending().await,
            }
        };
        let lookup = &mut self.lookup;
        let shells = &mut self.shells;
        let resync = &mut self.resync;
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
            list = shells_done(shells) => self.on_shells(list),
            done = resync_done(resync) => self.on_resynced(done),
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

    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome {
        // Без потока, пока идёт подписка заново, ввод допустим: сервер жив, а что не придёт
        // событием, покажет сверка.
        if self.exited || self.exit.is_some() || (self.stream.is_none() && self.resync.is_none()) {
            return TransmitOutcome::Undetermined;
        }
        self.transmit_inner(input, group).await
    }

    // Статус ответа и есть доказательство: запрос живёт в памяти сервера, отвечает он
    // синхронно. Обрыв не доказывает ничего, а повтора нет: `answered` уже стоит.
    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        let reply = match self.requests.answer(request_id, decision) {
            Ok(reply) => reply,
            Err(reason) => return DecisionOutcome::NotApplied(reason.into()),
        };
        let Some(http) = self.http.clone() else {
            return DecisionOutcome::Undetermined;
        };
        match reply.send(&http).await {
            Ok(()) => {
                if !self.requests.has_open() {
                    let running = matches!(
                        decision,
                        DecisionPayload::Permission {
                            behavior: PermissionBehavior::Allow,
                            ..
                        }
                    );
                    self.events.push_back(AdapterEvent::Phase {
                        phase: if running {
                            Phase::RunningTool
                        } else {
                            Phase::Generating
                        },
                        tool_name: None,
                        summary: None,
                    });
                }
                DecisionOutcome::Applied(requests::proof(request_id, decision))
            }
            Err(HttpError::Status {
                status: 404 | 409, ..
            }) => {
                self.requests.forget(request_id);
                DecisionOutcome::NotApplied("request_not_pending".into())
            }
            Err(HttpError::Status { status: 400, .. }) => {
                self.requests.reopen(request_id);
                if let Some(reply) = self.requests.refuse(request_id, requests::REFUSED) {
                    self.send_refusal(reply);
                }
                DecisionOutcome::NotApplied("decision_invalid".into())
            }
            Err(_) => DecisionOutcome::Undetermined,
        }
    }

    async fn refuse_request(&mut self, request_id: &str, message: &str) {
        if let Some(reply) = self.requests.refuse(request_id, message) {
            self.send_refusal(reply);
        }
    }

    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        if let Some(broker) = self.broker.as_mut() {
            broker.answer_agent_tool(call_id, &result).await;
        }
    }

    // Карточки супервизор уже закрыл. Сервер снимает потомков вместе с родителем, а всё,
    // что он не снял, получает отказ: иначе вызовы ждали бы ответа вечно.
    async fn interrupt(&mut self) -> InterruptOutcome {
        let Some(http) = self.http.clone() else {
            return InterruptOutcome::NotApplicable;
        };
        let path = format!("/api/session/{}/interrupt", self.session);
        let outcome = match http.post_empty(&path, INTERRUPT_WAIT).await {
            Ok(reply) => match reply.get("interrupted").and_then(Value::as_bool) {
                Some(true) => InterruptOutcome::Acknowledged,
                Some(false) => InterruptOutcome::NotApplicable,
                None => InterruptOutcome::Undetermined,
            },
            Err(HttpError::Status { status: 404, .. }) => InterruptOutcome::NotApplicable,
            Err(_) => InterruptOutcome::Undetermined,
        };
        if matches!(outcome, InterruptOutcome::Acknowledged) {
            self.requests.drop_all();
        } else {
            for reply in self.requests.refuse_all(requests::REFUSED) {
                self.send_refusal(reply);
            }
        }
        outcome
    }

    // Идентификатор из команды: повтор доставки попадёт в тот же элемент очереди сервера.
    async fn compact(&mut self, command: Uuid) -> CompactOutcome {
        if self.turn_running() || self.compaction.is_some() {
            return CompactOutcome::NotApplicable;
        }
        let Some(http) = self.http.clone() else {
            return CompactOutcome::NotApplicable;
        };
        let id = input::message_id(command);
        let path = format!("/api/session/{}/compact", self.session);
        let body = json!({"id": id, "delivery": "queue"});
        match http.post(&path, &body, REQUEST_WAIT).await {
            Ok(reply) if input::accepted_id(&reply) == Some(id.as_str()) => {
                self.compaction = Some(id);
                CompactOutcome::Accepted
            }
            _ => CompactOutcome::Failed,
        }
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
        if let Some(broker) = self.broker.as_mut() {
            broker.shutdown().await;
        }
        self.stop_server(matches!(mode, CloseMode::Graceful))
            .await?;
        self.exited = self.proc.is_some() || self.exited;
        Ok(())
    }
}
