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
pub mod jobs;
pub mod machine_text;
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
pub mod send;
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
    "plan.adopt",
    "history.search",
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
    InvalidArguments,
    Internal,
}

impl ToolErrorCode {
    pub const ALL: [Self; 34] = [
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
        Self::InvalidArguments,
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
            Self::InvalidArguments => "invalid_arguments",
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
            Self::InvalidArguments => "Invalid arguments",
            Self::Internal => "The hub could not complete the call, try later",
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
        "plan.adopt" => plan::adopt(context, caller, arguments).await,
        "history.search" => search::search(context, caller, arguments).await,
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
