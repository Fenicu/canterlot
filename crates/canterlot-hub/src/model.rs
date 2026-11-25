use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelError {
    #[error("unknown {kind}: {value}")]
    UnknownValue { kind: &'static str, value: String },
    #[error("reason {0} requires data; deserialize it from JSON")]
    MissingReasonData(&'static str),
    #[error("invalid run slot columns: slot={slot}, input_id={input_id:?}, attempt={attempt:?}")]
    InvalidRunSlot {
        slot: String,
        input_id: Option<Uuid>,
        attempt: Option<i32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputState {
    Queued,
    Dispatched,
    OnMachine,
    Transmitting,
    Accepted,
    AcceptanceUnconfirmed,
    Cancelled,
    Rejected,
}

impl InputState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::OnMachine => "on_machine",
            Self::Transmitting => "transmitting",
            Self::Accepted => "accepted",
            Self::AcceptanceUnconfirmed => "acceptance_unconfirmed",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }
}

impl FromStr for InputState {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "dispatched" => Ok(Self::Dispatched),
            "on_machine" => Ok(Self::OnMachine),
            "transmitting" => Ok(Self::Transmitting),
            "accepted" => Ok(Self::Accepted),
            "acceptance_unconfirmed" => Ok(Self::AcceptanceUnconfirmed),
            "cancelled" => Ok(Self::Cancelled),
            "rejected" => Ok(Self::Rejected),
            _ => Err(ModelError::UnknownValue {
                kind: "input state",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
    OwnerLost,
}

impl ExecutionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::OwnerLost => "owner_lost",
        }
    }
}

impl FromStr for ExecutionState {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "created" => Ok(Self::Created),
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "stopping" => Ok(Self::Stopping),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            "owner_lost" => Ok(Self::OwnerLost),
            _ => Err(ModelError::UnknownValue {
                kind: "execution state",
                value: value.into(),
            }),
        }
    }
}

/// Учётная запись лимитов по умолчанию. В JSON удержаний и пауз она не пишется:
/// хранимые до появления учётных записей значения читаются как `owner` и пишутся так же.
pub fn owner_account() -> String {
    canterlot_proto::limits::OWNER_ACCOUNT.to_owned()
}

pub fn is_owner_account(account: &String) -> bool {
    account == canterlot_proto::limits::OWNER_ACCOUNT
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PauseReason {
    UserStop,
    FatalError,
    ProcessExited,
    LimitReached {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// Учётная запись провайдера; для `owner` ключа в JSON нет.
        #[serde(default = "owner_account", skip_serializing_if = "is_owner_account")]
        account: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[serde(with = "optional_utc_time")]
        resets_at: Option<DateTime<Utc>>,
    },
    UnconfirmedDelivery {
        input_id: Uuid,
    },
    OwnerLost,
    RestorePending,
    /// Процесс остановлен хабом после доказанного простоя; следующее сообщение будит сессию.
    Hibernated,
    /// Один и тот же отказ провайдера подряд. Причина отказа неизвестна.
    SessionStuck {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction: Option<CompactionProgress>,
    },
}

/// Ход сжатия контекста, запрошенного владельцем.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactionProgress {
    Requested,
    Failed,
}

impl PauseReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserStop => "user_stop",
            Self::FatalError => "fatal_error",
            Self::ProcessExited => "process_exited",
            Self::LimitReached { .. } => "limit_reached",
            Self::UnconfirmedDelivery { .. } => "unconfirmed_delivery",
            Self::OwnerLost => "owner_lost",
            Self::RestorePending => "restore_pending",
            Self::Hibernated => "hibernated",
            Self::SessionStuck { .. } => "session_stuck",
        }
    }
}

impl FromStr for PauseReason {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user_stop" => Ok(Self::UserStop),
            "fatal_error" => Ok(Self::FatalError),
            "process_exited" => Ok(Self::ProcessExited),
            "limit_reached" => Ok(Self::LimitReached {
                provider: None,
                account: owner_account(),
                window: None,
                resets_at: None,
            }),
            "unconfirmed_delivery" => Err(ModelError::MissingReasonData("unconfirmed_delivery")),
            "owner_lost" => Ok(Self::OwnerLost),
            "session_stuck" => Ok(Self::SessionStuck { compaction: None }),
            "restore_pending" => Ok(Self::RestorePending),
            "hibernated" => Ok(Self::Hibernated),
            _ => Err(ModelError::UnknownValue {
                kind: "pause reason",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HoldReason {
    RunInProgress,
    AwaitingUserRequest,
    ContextMaintenance,
    NoLiveExecution,
    ExecutionStarting,
    StorageLow,
    WorkdirBusy {
        conflict_conversation_id: Uuid,
    },
    AttachmentsPending {
        input_id: Uuid,
    },
    QuotaExhausted {
        provider: String,
        /// Учётная запись провайдера; для `owner` ключа в JSON нет.
        #[serde(default = "owner_account", skip_serializing_if = "is_owner_account")]
        account: String,
        window: String,
        #[serde(with = "utc_time")]
        wake_at: DateTime<Utc>,
    },
    WeeklyReserve {
        provider: String,
        /// Учётная запись провайдера; для `owner` ключа в JSON нет.
        #[serde(default = "owner_account", skip_serializing_if = "is_owner_account")]
        account: String,
        #[serde(default, with = "optional_utc_time")]
        resets_at: Option<DateTime<Utc>>,
    },
    /// Боковой чат OpenCode ждёт простоя родителя: форк активного хода не проверен.
    SideParentBusy,
}

impl HoldReason {
    /// Оба удержания — недельный резерв одной и той же учётной записи одного провайдера.
    pub fn same_reserve(&self, other: &HoldReason) -> bool {
        matches!((self, other), (
            Self::WeeklyReserve { provider: a, account: x, .. },
            Self::WeeklyReserve { provider: b, account: y, .. },
        ) if a == b && x == y)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RunInProgress => "run_in_progress",
            Self::AwaitingUserRequest => "awaiting_user_request",
            Self::ContextMaintenance => "context_maintenance",
            Self::NoLiveExecution => "no_live_execution",
            Self::ExecutionStarting => "execution_starting",
            Self::StorageLow => "storage_low",
            Self::WorkdirBusy { .. } => "workdir_busy",
            Self::AttachmentsPending { .. } => "attachments_pending",
            Self::QuotaExhausted { .. } => "quota_exhausted",
            Self::WeeklyReserve { .. } => "weekly_reserve",
            Self::SideParentBusy => "side_parent_busy",
        }
    }
}

impl FromStr for HoldReason {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "run_in_progress" => Ok(Self::RunInProgress),
            "awaiting_user_request" => Ok(Self::AwaitingUserRequest),
            "context_maintenance" => Ok(Self::ContextMaintenance),
            "no_live_execution" => Ok(Self::NoLiveExecution),
            "execution_starting" => Ok(Self::ExecutionStarting),
            "storage_low" => Ok(Self::StorageLow),
            "workdir_busy" => Err(ModelError::MissingReasonData("workdir_busy")),
            "attachments_pending" => Err(ModelError::MissingReasonData("attachments_pending")),
            "quota_exhausted" => Err(ModelError::MissingReasonData("quota_exhausted")),
            "weekly_reserve" => Err(ModelError::MissingReasonData("weekly_reserve")),
            "side_parent_busy" => Ok(Self::SideParentBusy),
            _ => Err(ModelError::UnknownValue {
                kind: "hold reason",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HoldView {
    RunInProgress,
    AwaitingUserRequest,
    ContextMaintenance,
    NoLiveExecution,
    ExecutionStarting,
    StorageLow,
    WorkdirBusy {
        conflict_conversation_id: Uuid,
    },
    AttachmentsPending {
        input_id: Uuid,
        #[serde(
            default,
            rename = "machine_reason",
            skip_serializing_if = "Option::is_none"
        )]
        reason: Option<String>,
    },
    QuotaExhausted {
        provider: String,
        /// Учётная запись провайдера; для `owner` ключа в JSON нет.
        #[serde(default = "owner_account", skip_serializing_if = "is_owner_account")]
        account: String,
        window: String,
        #[serde(with = "utc_time")]
        wake_at: DateTime<Utc>,
    },
    WeeklyReserve {
        provider: String,
        /// Учётная запись провайдера; для `owner` ключа в JSON нет.
        #[serde(default = "owner_account", skip_serializing_if = "is_owner_account")]
        account: String,
        #[serde(default, with = "optional_utc_time")]
        resets_at: Option<DateTime<Utc>>,
    },
    SideParentBusy,
    MachineNotReconciled,
    CapabilityMissing {
        input_id: Uuid,
    },
}

impl From<HoldReason> for HoldView {
    fn from(reason: HoldReason) -> Self {
        match reason {
            HoldReason::RunInProgress => Self::RunInProgress,
            HoldReason::AwaitingUserRequest => Self::AwaitingUserRequest,
            HoldReason::ContextMaintenance => Self::ContextMaintenance,
            HoldReason::NoLiveExecution => Self::NoLiveExecution,
            HoldReason::ExecutionStarting => Self::ExecutionStarting,
            HoldReason::StorageLow => Self::StorageLow,
            HoldReason::WorkdirBusy {
                conflict_conversation_id,
            } => Self::WorkdirBusy {
                conflict_conversation_id,
            },
            HoldReason::AttachmentsPending { input_id } => Self::AttachmentsPending {
                input_id,
                reason: None,
            },
            HoldReason::QuotaExhausted {
                provider,
                account,
                window,
                wake_at,
            } => Self::QuotaExhausted {
                provider,
                account,
                window,
                wake_at,
            },
            HoldReason::WeeklyReserve {
                provider,
                account,
                resets_at,
            } => Self::WeeklyReserve {
                provider,
                account,
                resets_at,
            },
            HoldReason::SideParentBusy => Self::SideParentBusy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunSlot {
    Free,
    Dispatching { input_id: Uuid, attempt: i32 },
    Running { input_id: Uuid },
}

impl RunSlot {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Dispatching { .. } => "dispatching",
            Self::Running { .. } => "running",
        }
    }

    pub fn to_columns(&self) -> (&'static str, Option<Uuid>, Option<i32>) {
        match self {
            Self::Free => (self.as_str(), None, None),
            Self::Dispatching { input_id, attempt } => {
                (self.as_str(), Some(*input_id), Some(*attempt))
            }
            Self::Running { input_id } => (self.as_str(), Some(*input_id), None),
        }
    }

    pub fn from_columns(
        slot: &str,
        input_id: Option<Uuid>,
        attempt: Option<i32>,
    ) -> Result<Self, ModelError> {
        match (slot, input_id, attempt) {
            ("free", None, None) => Ok(Self::Free),
            ("dispatching", Some(input_id), Some(attempt)) => {
                Ok(Self::Dispatching { input_id, attempt })
            }
            ("running", Some(input_id), None) => Ok(Self::Running { input_id }),
            _ => Err(ModelError::InvalidRunSlot {
                slot: slot.into(),
                input_id,
                attempt,
            }),
        }
    }
}

/// Слот очереди для клиента. Слот называет ввод (`inputs.id`), а блоки хода несут
/// его команду (`input_command_id`); `command_id` связывает их. У стёртого ввода его нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RunSlot)]
pub enum RunSlotView {
    Free,
    Dispatching {
        input_id: Uuid,
        attempt: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        command_id: Option<Uuid>,
    },
    Running {
        input_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        command_id: Option<Uuid>,
    },
}

impl RunSlotView {
    pub fn new(slot: &RunSlot, command_id: Option<Uuid>) -> Self {
        match *slot {
            RunSlot::Free => Self::Free,
            RunSlot::Dispatching { input_id, attempt } => Self::Dispatching {
                input_id,
                attempt,
                command_id,
            },
            RunSlot::Running { input_id } => Self::Running {
                input_id,
                command_id,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    DeliverInput,
    CancelInput,
    WithdrawInput,
    Interrupt,
    StartExecution,
    StopExecution,
    DeliverDecision,
    CompactSession,
    RemoveWorktree,
    ResolveOwnerLost,
    RevokeSecret,
    StartInPlaceJob,
    CancelInPlaceJob,
    StageSource,
    CollectOutputs,
    CleanupSource,
    OwnerJobFinished,
    AttachRunner,
}

impl CommandKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeliverInput => "deliver_input",
            Self::CancelInput => "cancel_input",
            Self::WithdrawInput => "withdraw_input",
            Self::Interrupt => "interrupt",
            Self::StartExecution => "start_execution",
            Self::StopExecution => "stop_execution",
            Self::DeliverDecision => "deliver_decision",
            Self::CompactSession => "compact_session",
            Self::RemoveWorktree => "remove_worktree",
            Self::ResolveOwnerLost => "resolve_owner_lost",
            Self::RevokeSecret => "revoke_secret",
            Self::StartInPlaceJob => "start_inplace_job",
            Self::CancelInPlaceJob => "cancel_inplace_job",
            Self::StageSource => "stage_source",
            Self::CollectOutputs => "collect_outputs",
            Self::CleanupSource => "cleanup_source",
            Self::OwnerJobFinished => "owner_job_finished",
            Self::AttachRunner => "attach_runner",
        }
    }
}

impl FromStr for CommandKind {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "deliver_input" => Ok(Self::DeliverInput),
            "cancel_input" => Ok(Self::CancelInput),
            "withdraw_input" => Ok(Self::WithdrawInput),
            "interrupt" => Ok(Self::Interrupt),
            "start_execution" => Ok(Self::StartExecution),
            "stop_execution" => Ok(Self::StopExecution),
            "deliver_decision" => Ok(Self::DeliverDecision),
            "compact_session" => Ok(Self::CompactSession),
            "remove_worktree" => Ok(Self::RemoveWorktree),
            "resolve_owner_lost" => Ok(Self::ResolveOwnerLost),
            "revoke_secret" => Ok(Self::RevokeSecret),
            "start_inplace_job" => Ok(Self::StartInPlaceJob),
            "cancel_inplace_job" => Ok(Self::CancelInPlaceJob),
            "stage_source" => Ok(Self::StageSource),
            "collect_outputs" => Ok(Self::CollectOutputs),
            "cleanup_source" => Ok(Self::CleanupSource),
            "owner_job_finished" => Ok(Self::OwnerJobFinished),
            "attach_runner" => Ok(Self::AttachRunner),
            _ => Err(ModelError::UnknownValue {
                kind: "command kind",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Held,
    Pending,
    StoredOnMachine,
    Completed,
}

impl DeliveryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Pending => "pending",
            Self::StoredOnMachine => "stored_on_machine",
            Self::Completed => "completed",
        }
    }
}

impl FromStr for DeliveryState {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "held" => Ok(Self::Held),
            "pending" => Ok(Self::Pending),
            "stored_on_machine" => Ok(Self::StoredOnMachine),
            "completed" => Ok(Self::Completed),
            _ => Err(ModelError::UnknownValue {
                kind: "delivery state",
                value: value.into(),
            }),
        }
    }
}

pub(crate) mod utc_time {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        value: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error> {
        DateTime::<Utc>::deserialize(deserializer)
    }
}

pub(crate) mod optional_utc_time {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        value: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        Option::<DateTime<Utc>>::deserialize(deserializer)
    }
}
