//! Межсессионные инструменты модели: список, осмотр, сообщения и запуск других сессий.
//!
//! Вызывающий разговор и владелец берутся из живого исполнения на машине вызова,
//! а не из аргументов. Тексты сообщений и итоги сессий — данные уровня сообщения:
//! ни в журнал, ни в `Debug` они не попадают. `grpc/agent.rs` только раздаёт вызовы,
//! поэтому каждый инструмент живёт в своём файле этого модуля.

pub mod accept;
pub mod access;
pub mod alarms;
pub mod completion;
pub mod control;
pub mod disk;
pub mod jobs;
pub mod nudge;
pub mod origin;
pub mod outage;
pub mod read;
pub mod report;
pub mod send;
pub mod settle;
pub mod spawn;
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
    InvalidArguments,
    Internal,
}

impl ToolErrorCode {
    pub const ALL: [Self; 25] = [
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
        _ => Err(ToolError::new(ToolErrorCode::UnknownTool)),
    }
}

/// Аргументы только известной формы; текст ошибки разбора цитирует их и наружу не идёт.
pub(crate) fn parse_arguments<T: serde::de::DeserializeOwned>(
    arguments: Value,
) -> Result<T, ToolError> {
    serde_json::from_value(arguments).map_err(|_| ToolError::new(ToolErrorCode::InvalidArguments))
}
