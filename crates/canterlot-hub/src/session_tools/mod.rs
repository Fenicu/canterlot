//! Межсессионные инструменты модели: список, осмотр, сообщения и запуск других сессий.
//!
//! Вызывающий разговор и владелец берутся из живого исполнения на машине вызова,
//! а не из аргументов. Тексты сообщений и итоги сессий — данные уровня сообщения:
//! ни в журнал, ни в `Debug` они не попадают. `grpc/agent.rs` только раздаёт вызовы,
//! поэтому каждый инструмент живёт в своём файле этого модуля.

pub mod accept;
pub mod access;
pub mod alarms;
pub mod catalog;
pub mod completion;
pub mod control;
pub mod disk;
pub mod guard;
pub mod inplace_jobs;
pub mod jobs;
pub mod machine_text;
pub mod memory;
pub mod nudge;
pub mod origin;
pub mod outage;
pub mod paths;
pub mod plan;
pub mod probe;
pub mod read;
pub mod report;
pub mod routing;
pub mod search;
pub mod secret;
pub mod send;
pub mod settings_change;
pub mod settle;
pub mod spawn;
pub mod spawn_queue;
pub mod subs;
pub mod summary;
pub mod wait;
pub mod whitelist;

use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use self::settle::SettleConfig;
use crate::{
    config::{LimitsConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    session::Registry,
};

/// Имена хаба для инструментов; на стороне модели к ним добавляются префиксы harness.
pub const TOOLS: &[&str] = &[
    "sessions.list",
    "sessions.inspect",
    "sessions.send",
    "sessions.spawn",
    "sessions.subscribe",
    "sessions.unsubscribe",
    "sessions.wait",
    "sessions.accept",
    "sessions.interrupt",
    "sessions.stop",
    "limits.report",
    "alarms.set",
    "alarms.cancel",
    "alarms.list",
    "nudge.set",
    "nudge.clear",
    "routing.preview",
    "spawn_queue.cancel",
    "plan.list",
    "plan.add",
    "plan.update",
    "plan.archive",
    "plan.adopt",
    "history.search",
    "memory.search",
    "memory.get",
    "memory.write",
    "memory.update",
    "memory.delete",
    "secrets.request",
    "settings.routing",
    "settings.whitelist",
    "settings.propose_routing",
    "settings.propose_whitelist",
    "settings.propose_preset",
    "job.run",
    "job.status",
    "job.log",
    "job.cancel",
    "jobs.list",
    "runners.list",
];

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Коды ошибок инструментов: модель получает код и короткое английское сообщение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolErrorCode {
    UnknownTool,
    SessionNotFound,
    ReadOnlySession,
    RateLimited,
    ChainDepthExceeded,
    SpawnDepthExceeded,
    HarnessUnknown,
    ModelUnknown,
    EffortNotAllowed,
    WorkdirNotAllowed,
    DomainMismatch,
    /// Другая машина не принимает удалённую работу: владелец не включил выключатель.
    RemoteWorkDisabled,
    /// Машина — раннер: сессий на ней нет, задачи (`job_run`) идут.
    MachineIsRunner,
    /// `machine: "auto"` без `source`: хаб размещает только задачи с исходниками.
    SourceRequired,
    /// Ни один раннер несущей и ни одно разрешённое место не подходит по меткам.
    NoRunnerCanSatisfy,
    PermissionModeTooWide,
    ProfileUnknown,
    MachineUnavailable,
    CapabilityMissing,
    QueueFull,
    ConversationClosed,
    NotParent,
    NotDelegated,
    AttachmentNotAvailable,
    TextTooLarge,
    TooLarge,
    TooManyAlarms,
    AlarmNotFound,
    TaskTypeUnknown,
    NoExecutorAvailable,
    MachineBusy,
    SpawnQueueFull,
    PathNotAllowed,
    PlanTaskNotFound,
    PlanTaskClosed,
    PlanFull,
    AlreadyDecided,
    TooManySecrets,
    InvalidArguments,
    /// Коды памяти: чужой домен и несуществующая запись отвечают одинаково.
    NotFound,
    LooksLikeSecret,
    SubjectExists,
    VersionConflict,
    DomainFull,
    BadLevel,
    Invalid,
    MemoryUnavailable,
    /// `job_run` требует режим `bypass` у вызывающей сессии.
    ModeTooNarrow,
    /// Чужая, несуществующая и скрытая сменой домена задача отвечают одинаково.
    JobNotFound,
    Internal,
}

impl ToolErrorCode {
    pub const ALL: [Self; 50] = [
        Self::UnknownTool,
        Self::SessionNotFound,
        Self::ReadOnlySession,
        Self::RateLimited,
        Self::ChainDepthExceeded,
        Self::SpawnDepthExceeded,
        Self::HarnessUnknown,
        Self::ModelUnknown,
        Self::EffortNotAllowed,
        Self::WorkdirNotAllowed,
        Self::DomainMismatch,
        Self::RemoteWorkDisabled,
        Self::MachineIsRunner,
        Self::SourceRequired,
        Self::NoRunnerCanSatisfy,
        Self::PermissionModeTooWide,
        Self::ProfileUnknown,
        Self::MachineUnavailable,
        Self::CapabilityMissing,
        Self::QueueFull,
        Self::ConversationClosed,
        Self::NotParent,
        Self::NotDelegated,
        Self::AttachmentNotAvailable,
        Self::TextTooLarge,
        Self::TooLarge,
        Self::TooManyAlarms,
        Self::AlarmNotFound,
        Self::TaskTypeUnknown,
        Self::NoExecutorAvailable,
        Self::MachineBusy,
        Self::SpawnQueueFull,
        Self::PathNotAllowed,
        Self::PlanTaskNotFound,
        Self::PlanTaskClosed,
        Self::PlanFull,
        Self::AlreadyDecided,
        Self::TooManySecrets,
        Self::InvalidArguments,
        Self::NotFound,
        Self::LooksLikeSecret,
        Self::SubjectExists,
        Self::VersionConflict,
        Self::DomainFull,
        Self::BadLevel,
        Self::Invalid,
        Self::MemoryUnavailable,
        Self::ModeTooNarrow,
        Self::JobNotFound,
        Self::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownTool => "unknown_tool",
            Self::SessionNotFound => "session_not_found",
            Self::ReadOnlySession => "read_only_session",
            Self::RateLimited => "rate_limited",
            Self::ChainDepthExceeded => "chain_depth_exceeded",
            Self::SpawnDepthExceeded => "spawn_depth_exceeded",
            Self::HarnessUnknown => "harness_unknown",
            Self::ModelUnknown => "model_unknown",
            Self::EffortNotAllowed => "effort_not_allowed",
            Self::WorkdirNotAllowed => "workdir_not_allowed",
            Self::DomainMismatch => "domain_mismatch",
            Self::RemoteWorkDisabled => "remote_work_disabled",
            Self::MachineIsRunner => "machine_is_runner",
            Self::SourceRequired => "source_required",
            Self::NoRunnerCanSatisfy => "no_runner_can_satisfy",
            Self::PermissionModeTooWide => "permission_mode_too_wide",
            Self::ProfileUnknown => "profile_unknown",
            Self::MachineUnavailable => "machine_unavailable",
            Self::CapabilityMissing => "capability_missing",
            Self::QueueFull => "queue_full",
            Self::ConversationClosed => "conversation_closed",
            Self::NotParent => "not_parent",
            Self::NotDelegated => "not_delegated",
            Self::AttachmentNotAvailable => "attachment_not_available",
            Self::TextTooLarge => "text_too_large",
            Self::TooLarge => "too_large",
            Self::TooManyAlarms => "too_many_alarms",
            Self::AlarmNotFound => "alarm_not_found",
            Self::TaskTypeUnknown => "task_type_unknown",
            Self::NoExecutorAvailable => "no_executor_available",
            Self::MachineBusy => "machine_busy",
            Self::SpawnQueueFull => "spawn_queue_full",
            Self::PathNotAllowed => "path_not_allowed",
            Self::PlanTaskNotFound => "plan_task_not_found",
            Self::PlanTaskClosed => "plan_task_closed",
            Self::PlanFull => "plan_full",
            Self::AlreadyDecided => "already_decided",
            Self::TooManySecrets => "too_many_secrets",
            Self::InvalidArguments => "invalid_arguments",
            Self::NotFound => "not_found",
            Self::LooksLikeSecret => "looks_like_secret",
            Self::SubjectExists => "subject_exists",
            Self::VersionConflict => "version_conflict",
            Self::DomainFull => "domain_full",
            Self::BadLevel => "bad_level",
            Self::Invalid => "invalid",
            Self::MemoryUnavailable => "memory_unavailable",
            Self::ModeTooNarrow => "mode_too_narrow",
            Self::JobNotFound => "job_not_found",
            Self::Internal => "internal_error",
        }
    }

    pub fn default_message(self) -> &'static str {
        match self {
            Self::UnknownTool => "Unknown tool",
            Self::SessionNotFound => "No such session",
            Self::ReadOnlySession => "A read-only session cannot do this",
            Self::RateLimited => "Too many requests, try later",
            Self::ChainDepthExceeded => "The message chain between agents is too deep",
            Self::SpawnDepthExceeded => "Too many nested delegated sessions",
            Self::HarnessUnknown => "Unknown harness",
            Self::ModelUnknown => "Unknown model",
            Self::EffortNotAllowed => "This effort is not available for the model",
            Self::WorkdirNotAllowed => "This working directory is not allowed",
            Self::DomainMismatch => "The working directory belongs to another memory domain",
            Self::RemoteWorkDisabled => {
                "The owner has not allowed other machines to start work on this machine"
            }
            Self::MachineIsRunner => "This machine is a runner: sessions do not start on it",
            Self::SourceRequired => "Automatic placement needs a source",
            Self::NoRunnerCanSatisfy => "No runner can ever satisfy the labels and placement",
            Self::PermissionModeTooWide => "The mode is wider than the parent session's",
            Self::ProfileUnknown => "Unknown instruction preset",
            Self::MachineUnavailable => "The machine is not available",
            Self::CapabilityMissing => "The worker does not support this yet",
            Self::QueueFull => "The target queue is full",
            Self::ConversationClosed => "The session is closed",
            Self::NotParent => "Only the session that started this one may do this",
            Self::NotDelegated => "The session was not started by an agent",
            Self::AttachmentNotAvailable => "The attachment is not available in this conversation",
            Self::TextTooLarge => "The text is too large",
            Self::TooLarge => "The payload is too large",
            Self::TooManyAlarms => "Too many armed alarms in this session",
            Self::AlarmNotFound => "No such alarm",
            Self::TaskTypeUnknown => "Unknown routing task type",
            Self::NoExecutorAvailable => "No executor available for this task type",
            Self::MachineBusy => "The machine is too busy for a new session",
            Self::SpawnQueueFull => "Too many spawns are already queued for this session",
            Self::PathNotAllowed => "The path is not allowed",
            Self::PlanTaskNotFound => "No such task in your plan",
            Self::PlanTaskClosed => "The plan task is done or cancelled",
            Self::PlanFull => "The plan holds too many tasks",
            Self::AlreadyDecided => "The owner has already answered this request",
            Self::TooManySecrets => "Too many secret requests are already waiting",
            Self::InvalidArguments => "Invalid arguments",
            Self::NotFound => "No such memory entry",
            Self::LooksLikeSecret => "The text looks like a secret",
            Self::SubjectExists => "An active entry with this subject already exists",
            Self::VersionConflict => "The entry changed since you read it",
            Self::DomainFull => "The memory of this domain is full",
            Self::BadLevel => "This session has no project, so project level is not possible",
            Self::Invalid => "Invalid memory field",
            Self::MemoryUnavailable => "This session has no memory",
            Self::ModeTooNarrow => "This session's permission mode is too narrow to run commands",
            Self::JobNotFound => "No such job in this session",
            Self::Internal => "The hub could not complete the call, try later",
        }
    }

    /// Что делать дальше: каждый отказ несёт подсказку, по которой модель исправляется сама.
    pub fn hint(self) -> &'static str {
        match self {
            Self::UnknownTool => "Call tools/list to see the tools of this session.",
            Self::SessionNotFound => "Call sessions_list to find a session id.",
            Self::ReadOnlySession => {
                "End your turn with a plain-text answer, or ask the owner to change the mode."
            }
            Self::RateLimited => {
                "Wait, or arm alarm_set to be woken later; do not repeat the call."
            }
            Self::ChainDepthExceeded => {
                "Answer the parent session yourself instead of sending another message."
            }
            Self::SpawnDepthExceeded => {
                "Do the work in this session, or ask the owner to raise the nesting limit."
            }
            Self::HarnessUnknown => "Pass harness claude_code, codex or opencode.",
            Self::ModelUnknown => {
                "Pass a catalog model id, or call route_preview to let the policy choose."
            }
            Self::EffortNotAllowed => "Lower effort, or pass no effort for the model default.",
            Self::WorkdirNotAllowed => {
                "Pass a workdir under the allowed roots, or omit it to use this session's."
            }
            Self::DomainMismatch => {
                "Omit workdir, or pass one inside this session's own memory domain."
            }
            Self::RemoteWorkDisabled => {
                "Start the session on your own machine, or ask the owner to switch on remote work for that machine in the app."
            }
            Self::MachineIsRunner => {
                "Start the session on a worker machine, or run the command there with job_run and machine set to the runner."
            }
            Self::SourceRequired => {
                "Pass source {repo, rev} (absolute repo path, full 40-hex commit), or name the machine and workdir."
            }
            Self::NoRunnerCanSatisfy => {
                "Call runners_list to see the labels of the runners, drop a label, or allow more placement kinds."
            }
            Self::PermissionModeTooWide => {
                "Pass a mode no wider than this session's, or ask the owner to approve."
            }
            Self::ProfileUnknown => "Call canterlot_presets_list to see the preset ids.",
            Self::MachineUnavailable => {
                "Pass workdir on this machine, or start the session on another."
            }
            Self::CapabilityMissing => {
                "Continue without this step, or ask the owner to update the worker."
            }
            Self::QueueFull => {
                "Wait for the queue to drain, or cancel a queued spawn with spawn_cancel."
            }
            Self::ConversationClosed => "Start a new session with session_spawn.",
            Self::NotParent => "Act only on sessions you started, or ask their parent.",
            Self::NotDelegated => "Call this from a session started by an agent, or ask the owner.",
            Self::AttachmentNotAvailable => {
                "Pass attachment ids from this conversation, or upload the file again."
            }
            Self::TextTooLarge => "Send a shorter text, or hand the file over with share_file.",
            Self::TooLarge => "Send less data in one call.",
            Self::TooManyAlarms => "Cancel an alarm with alarm_cancel, then arm the new one.",
            Self::AlarmNotFound => "Call alarm_list to see the armed alarms.",
            Self::TaskTypeUnknown => "Call route_preview without task_type to list the task types.",
            Self::NoExecutorAvailable => {
                "Pass harness and model yourself, or ask the owner to adjust the routing policy."
            }
            Self::MachineBusy => {
                "Queue the spawn with queue=true, or wait for the machine to free up."
            }
            Self::SpawnQueueFull => {
                "Wait for a queued spawn to start, or cancel one with spawn_cancel."
            }
            Self::PathNotAllowed => "Pass a path under the session workdir or the temp directory.",
            Self::PlanTaskNotFound => "Call plan_list to see task numbers.",
            Self::PlanTaskClosed => "Pick an open task from plan_list.",
            Self::PlanFull => "Finish or cancel tasks with plan_update before adding more.",
            Self::AlreadyDecided => {
                "The owner's answer stands; wait for its result instead of cancelling."
            }
            Self::TooManySecrets => {
                "Wait for the owner to answer the open ones; do not repeat the call."
            }
            Self::InvalidArguments => {
                "Fix the argument and call again; tools/list shows the schema."
            }
            Self::NotFound => "Call memory_search to find an entry id.",
            Self::LooksLikeSecret => {
                "Remove the secret from the text and ask the owner for it with secret_request."
            }
            Self::SubjectExists => {
                "Update the existing entry with memory_update, or pick another subject."
            }
            Self::VersionConflict => {
                "Read the entry again with memory_get, then repeat the call with its current version."
            }
            Self::DomainFull => "Archive entries you no longer need with memory_delete.",
            Self::BadLevel => "Pass level domain, or write from a session that has a project.",
            Self::Invalid => "Fix the named field and call again.",
            Self::MemoryUnavailable => "Continue without memory; do not retry.",
            Self::ModeTooNarrow => {
                "Ask the owner to switch this session to bypass mode, or run the command another way."
            }
            Self::JobNotFound => "Call jobs_list to see the jobs of this session.",
            Self::Internal => "Try again; if it repeats, tell the owner.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
}

impl ToolError {
    pub fn new(code: ToolErrorCode) -> Self {
        Self {
            code,
            message: code.default_message().to_owned(),
        }
    }

    pub fn with_message(code: ToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Текст для модели: сообщение и следующий шаг. Код остаётся отдельным полем.
    pub fn text(&self) -> String {
        let message = self.message.trim_end();
        let separator = if message.ends_with(['.', '!', '?']) {
            " "
        } else {
            ". "
        };
        format!("{message}{separator}{}", self.code.hint())
    }
}

impl ToolError {
    /// Доменные отказы секретов сверх названных вызывающим: только код, без текста.
    pub(crate) fn from_secret(error: crate::secrets::SecretError) -> Self {
        use crate::secrets::SecretError as E;
        match error {
            E::Database(error) => error.into(),
            E::TooLarge => Self::new(ToolErrorCode::TooLarge),
            E::Limit => Self::new(ToolErrorCode::TooManySecrets),
            E::MachineOffline => Self::new(ToolErrorCode::MachineUnavailable),
            _ => Self::new(ToolErrorCode::Internal),
        }
    }
}

/// Текст ошибки базы может цитировать данные: наружу идёт только код.
impl From<sqlx::Error> for ToolError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(
            kind = "session_tool_db_error",
            error = %crate::dberr::describe(&error),
            "session tool failed"
        );
        Self::new(ToolErrorCode::Internal)
    }
}

/// Что нужно инструментам от хаба.
#[derive(Clone)]
pub struct ToolContext {
    pub pool: PgPool,
    pub config: Arc<SessionToolsConfig>,
    pub memory: Arc<crate::config::MemoryConfig>,
    pub limits: Arc<LimitsConfig>,
    pub settle: SettleConfig,
    pub alarms_enabled: bool,
    pub nudge_enabled: bool,
    pub routing_enabled: bool,
    pub routing_stale: std::time::Duration,
    pub routing_queue_horizon: std::time::Duration,
    /// Очередь запусков: срок элемента, предел ожидающих, пауза повтора.
    pub spawn_queue: Arc<crate::config::SpawnQueueConfig>,
    /// Пороги стража машины перед запуском.
    pub guards: Arc<crate::config::GuardsConfig>,
    pub dispatcher: Option<Dispatcher>,
    pub registry: Registry,
    /// Останов хаба: долгие вызовы (`sessions.wait`) возвращаются, не дожидаясь срока.
    pub shutdown: CancellationToken,
}

/// Вызывающий: всё выведено из живого исполнения, не из аргументов.
#[derive(Debug, Clone, Copy)]
pub struct Caller {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub execution_id: Uuid,
    pub machine_id: Uuid,
    pub call_id: Uuid,
}

/// Раздача вызова; пока инструмент не готов, отвечает `unknown_tool`.
pub async fn call(
    context: &ToolContext,
    caller: &Caller,
    tool: &str,
    arguments: Value,
) -> Result<Value, ToolError> {
    match tool {
        "sessions.list" => read::list(context, caller, arguments).await,
        "sessions.inspect" => read::inspect(context, caller, arguments).await,
        "sessions.send" => send::send(context, caller, arguments).await,
        "sessions.spawn" => spawn::spawn(context, caller, arguments).await,
        "sessions.subscribe" => send::subscribe(context, caller, arguments).await,
        "sessions.unsubscribe" => send::unsubscribe(context, caller, arguments).await,
        "sessions.wait" => wait::wait(context, caller, arguments).await,
        "sessions.accept" => accept::accept(context, caller, arguments).await,
        "sessions.interrupt" => control::interrupt(context, caller, arguments).await,
        "sessions.stop" => control::stop(context, caller, arguments).await,
        "limits.report" => report::report(context, caller, arguments).await,
        "alarms.set" => alarms::set(context, caller, arguments).await,
        "alarms.cancel" => alarms::cancel(context, caller, arguments).await,
        "alarms.list" => alarms::list(context, caller, arguments).await,
        "nudge.set" => nudge::set(context, caller, arguments).await,
        "nudge.clear" => nudge::clear(context, caller, arguments).await,
        "routing.preview" => routing::preview(context, caller, arguments).await,
        "spawn_queue.cancel" => spawn_queue::cancel(context, caller, arguments).await,
        "plan.list" => plan::list(context, caller, arguments).await,
        "plan.add" => plan::add(context, caller, arguments).await,
        "plan.update" => plan::update(context, caller, arguments).await,
        "plan.archive" => plan::archive(context, caller, arguments).await,
        "plan.adopt" => plan::adopt(context, caller, arguments).await,
        "history.search" => search::search(context, caller, arguments).await,
        "memory.search" => memory::search(context, caller, arguments).await,
        "memory.get" => memory::get(context, caller, arguments).await,
        "memory.write" => memory::write(context, caller, arguments).await,
        "memory.update" => memory::update(context, caller, arguments).await,
        "memory.delete" => memory::delete(context, caller, arguments).await,
        "secrets.request" => secret::request(context, caller, arguments).await,
        "settings.routing" => settings_change::routing_policy(context, caller, arguments).await,
        "settings.whitelist" => settings_change::whitelist(context, caller, arguments).await,
        "settings.propose_routing" => {
            settings_change::propose_routing(context, caller, arguments).await
        }
        "settings.propose_whitelist" => {
            settings_change::propose_whitelist(context, caller, arguments).await
        }
        "settings.propose_preset" => {
            settings_change::propose_preset(context, caller, arguments).await
        }
        "job.run" => inplace_jobs::run(context, caller, arguments).await,
        "job.status" => inplace_jobs::status(context, caller, arguments).await,
        // Полный журнал читает путь с хранилищем блобов (`grpc/agent.rs`); здесь только хвост.
        "job.log" => inplace_jobs::log(context, caller, arguments, None).await,
        "job.cancel" => inplace_jobs::cancel(context, caller, arguments).await,
        "jobs.list" => inplace_jobs::list(context, caller, arguments).await,
        "runners.list" => inplace_jobs::runners(context, caller, arguments).await,
        _ => Err(ToolError::new(ToolErrorCode::UnknownTool)),
    }
}

/// Аргументы только известной формы; ошибка разбора называет инструмент, неверный аргумент
/// и допустимые имена, но никогда не повторяет значения: в них бывают тексты и брифы.
pub(crate) fn parse_arguments<T: serde::de::DeserializeOwned>(
    tool: &str,
    arguments: Value,
) -> Result<T, ToolError> {
    match serde_json::from_value(arguments.clone()) {
        Ok(value) => Ok(value),
        Err(error) => Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            argument_error::<T>(tool, &arguments, &error),
        )),
    }
}

fn argument_error<T: serde::de::DeserializeOwned>(
    tool: &str,
    arguments: &Value,
    error: &serde_json::Error,
) -> String {
    let detail = redact_values(&error.to_string());
    if let Some(rest) = detail.strip_prefix("unknown field `")
        && let Some((field, expected)) = rest.split_once('`')
    {
        let names = argument_names(expected);
        return if names.is_empty() {
            format!("{tool}: unknown argument '{field}'")
        } else {
            format!(
                "{tool}: unknown argument '{field}'; its arguments are: {}",
                names.join(", ")
            )
        };
    }
    if let Some(rest) = detail.strip_prefix("missing field `")
        && let Some((field, _)) = rest.split_once('`')
    {
        return format!("{tool}: missing required argument '{field}'");
    }
    match offending_key::<T>(arguments) {
        Some(key) => format!("{tool}: argument '{key}': {detail}"),
        None => format!("{tool}: {detail}"),
    }
}

/// Поле, без которого разбор проходит: у структур с `deny_unknown_fields` это и есть виновник.
fn offending_key<T: serde::de::DeserializeOwned>(arguments: &Value) -> Option<String> {
    let object = arguments.as_object()?;
    object.keys().find_map(|key| {
        let mut probe = object.clone();
        probe.remove(key);
        serde_json::from_value::<T>(Value::Object(probe))
            .ok()
            .map(|_| key.clone())
    })
}

/// `expected one of `a`, `b` or `c`` → `a, b, c`.
fn argument_names(expected: &str) -> Vec<String> {
    let expected = expected
        .strip_prefix(", expected one of ")
        .unwrap_or(expected)
        .trim();
    expected
        .replace(" or ", ", ")
        .split(',')
        .map(|name| name.trim().trim_matches('`').to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Значения из сообщения serde вырезаются: в аргументах бывают брифы, тексты и заметки.
fn redact_values(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut quoted = false;
    let mut escaped = false;
    for ch in message.chars() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        if ch == '"' {
            quoted = true;
            continue;
        }
        out.push(ch);
    }
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out.replace(" ,", ",").trim().to_owned()
}
