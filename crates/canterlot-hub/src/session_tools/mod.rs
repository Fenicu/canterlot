//! Межсессионные инструменты модели: список, осмотр, сообщения и запуск других сессий.
//!
//! Вызывающий разговор и владелец берутся из живого исполнения на машине вызова,
//! а не из аргументов. Тексты сообщений и итоги сессий — данные уровня сообщения:
//! ни в журнал, ни в `Debug` они не попадают. `grpc/agent.rs` только раздаёт вызовы,
//! поэтому каждый инструмент живёт в своём файле этого модуля.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{config::SessionToolsConfig, crunch::JobOutcome, scheduler::ClaimedJob};

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
    TextTooLarge,
    InvalidArguments,
}

impl ToolErrorCode {
    pub const ALL: [Self; 20] = [
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
        Self::TextTooLarge,
        Self::InvalidArguments,
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
            Self::TextTooLarge => "text_too_large",
            Self::InvalidArguments => "invalid_arguments",
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
            Self::TextTooLarge => "The text is too large",
            Self::InvalidArguments => "Invalid arguments",
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
    _pool: &PgPool,
    _config: &SessionToolsConfig,
    _caller: &Caller,
    _tool: &str,
    _arguments: Value,
) -> Result<Value, ToolError> {
    Err(ToolError::new(ToolErrorCode::UnknownTool))
}

/// Задание `session_task`: обработчик придёт вместе с подписками.
pub async fn run_job_in_tx(
    _conn: &mut PgConnection,
    _job: &ClaimedJob,
) -> Result<JobOutcome, sqlx::Error> {
    Ok(JobOutcome::Cancelled("unsupported"))
}
