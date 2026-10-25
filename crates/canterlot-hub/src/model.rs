use std::str::FromStr;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum PauseReason {
    UserStop,
    FatalError,
    ProcessExited,
    LimitReached,
    UnconfirmedDelivery { input_id: Uuid },
    OwnerLost,
    RestorePending,
}

impl PauseReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserStop => "user_stop",
            Self::FatalError => "fatal_error",
            Self::ProcessExited => "process_exited",
            Self::LimitReached => "limit_reached",
            Self::UnconfirmedDelivery { .. } => "unconfirmed_delivery",
            Self::OwnerLost => "owner_lost",
            Self::RestorePending => "restore_pending",
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
            "limit_reached" => Ok(Self::LimitReached),
            "unconfirmed_delivery" => Err(ModelError::MissingReasonData("unconfirmed_delivery")),
            "owner_lost" => Ok(Self::OwnerLost),
            "restore_pending" => Ok(Self::RestorePending),
            _ => Err(ModelError::UnknownValue {
                kind: "pause reason",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HoldReason {
    RunInProgress,
    AwaitingUserRequest,
    ContextMaintenance,
    NoLiveExecution,
    ExecutionStarting,
    StorageLow,
    WorkdirBusy { conflict_conversation_id: Uuid },
    AttachmentsPending { input_id: Uuid },
}

impl HoldReason {
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
            _ => Err(ModelError::UnknownValue {
                kind: "hold reason",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    DeliverInput,
    CancelInput,
    WithdrawInput,
    Interrupt,
    StartExecution,
    StopExecution,
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
