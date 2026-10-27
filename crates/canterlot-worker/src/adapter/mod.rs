use crate::containment::{Containment, ContainmentError};
use std::{fmt, path::PathBuf, time::Duration};
use uuid::Uuid;

#[cfg(feature = "synthetic-adapter")]
pub mod synthetic;
#[cfg(feature = "synthetic-adapter")]
pub mod synthetic_cli;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    Unsupported,
    Unavailable(&'static str),
}
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub streaming: Support,
    pub resume: Support,
    pub interrupt: Support,
    pub approvals: Support,
    pub questions: Support,
    pub background_work: Support,
    pub interrupt_keeps_process: bool,
}
#[derive(Clone)]
pub struct StartSpec {
    pub execution_id: Uuid,
    pub generation: u64,
    pub workdir: PathBuf,
    pub native_conversation_id: Option<String>,
    pub profile: String,
}
#[derive(Debug)]
pub struct Started {
    pub native_conversation_id: String,
    pub cli_pid: u32,
    pub cli_start_ticks: u64,
}
pub struct Transmission {
    pub command_id: Uuid,
    pub attempt: u32,
    pub text: String,
}
impl fmt::Debug for Transmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transmission")
            .field("command_id", &self.command_id)
            .field("attempt", &self.attempt)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}
#[derive(Debug)]
pub struct Proof {
    pub command_id: Uuid,
    pub kind: &'static str,
}
#[derive(Debug)]
pub enum TransmitOutcome {
    Accepted(Proof),
    Rejected(String),
    Undetermined,
}
#[derive(Debug)]
pub enum InterruptOutcome {
    Acknowledged,
    Undetermined,
    NotApplicable,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    AwaitingInput,
    Generating,
    RunningTool,
    AwaitingPermission,
    AwaitingAnswer,
    ContextMaintenance,
    Unknown,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
    Warning,
    Error,
}
#[derive(Clone)]
pub enum AdapterEvent {
    Phase(Phase),
    BlockStarted {
        block_id: Uuid,
        kind: BlockKind,
        input_command_id: Option<Uuid>,
        parent: Option<Uuid>,
    },
    BlockDelta {
        block_id: Uuid,
        append: String,
    },
    BlockFinal {
        block_id: Uuid,
        text: String,
    },
    RunFinished {
        input_command_id: Uuid,
    },
    Diagnostic {
        kind: &'static str,
        event_type: String,
        required: bool,
    },
    ProcessExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
}
impl fmt::Debug for AdapterEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Phase(phase) => f.debug_tuple("Phase").field(phase).finish(),
            Self::BlockStarted {
                block_id,
                kind,
                input_command_id,
                parent,
            } => f
                .debug_struct("BlockStarted")
                .field("block_id", block_id)
                .field("kind", kind)
                .field("input_command_id", input_command_id)
                .field("parent", parent)
                .finish(),
            Self::BlockDelta { block_id, append } => f
                .debug_struct("BlockDelta")
                .field("block_id", block_id)
                .field("append_bytes", &append.len())
                .finish(),
            Self::BlockFinal { block_id, text } => f
                .debug_struct("BlockFinal")
                .field("block_id", block_id)
                .field("text_bytes", &text.len())
                .finish(),
            Self::RunFinished { input_command_id } => f
                .debug_struct("RunFinished")
                .field("input_command_id", input_command_id)
                .finish(),
            Self::Diagnostic {
                kind,
                event_type,
                required,
            } => f
                .debug_struct("Diagnostic")
                .field("kind", kind)
                .field("event_type", event_type)
                .field("required", required)
                .finish(),
            Self::ProcessExited { code, signal } => f
                .debug_struct("ProcessExited")
                .field("code", code)
                .field("signal", signal)
                .finish(),
        }
    }
}
#[derive(Clone, Copy)]
pub enum CloseMode {
    Graceful,
    Force,
}
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("adapter I/O failed")]
    Io(#[from] std::io::Error),
    #[error("containment failed")]
    Containment(#[from] ContainmentError),
    #[error("invalid adapter protocol")]
    Protocol,
    #[error("adapter deadline expired")]
    Deadline,
    #[error("adapter already started")]
    AlreadyStarted,
}

pub const DEFAULT_TRANSMIT_DEADLINE: Duration = Duration::from_secs(10);

#[async_trait::async_trait]
pub trait Adapter: Send {
    fn capabilities(&self) -> Capabilities;
    async fn start(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError>;
    async fn transmit(&mut self, input: &Transmission) -> TransmitOutcome;
    async fn interrupt(&mut self) -> InterruptOutcome;
    async fn next_event(&mut self) -> AdapterEvent;
    async fn close(&mut self, mode: CloseMode, group: &dyn Containment)
    -> Result<(), AdapterError>;
}

pub fn registry(profile: &str) -> Option<Box<dyn Adapter>> {
    registry_with_deadline(profile, DEFAULT_TRANSMIT_DEADLINE)
}

pub fn registry_with_deadline(
    profile: &str,
    transmit_deadline: Duration,
) -> Option<Box<dyn Adapter>> {
    #[cfg(feature = "synthetic-adapter")]
    if matches!(profile, "synthetic" | "synthetic-exit-on-interrupt") {
        return Some(Box::new(
            synthetic::SyntheticAdapter::new(profile).with_transmit_deadline(transmit_deadline),
        ));
    }
    let _ = (profile, transmit_deadline);
    None
}

#[cfg(all(test, not(feature = "synthetic-adapter")))]
mod tests {
    #[test]
    fn synthetic_profiles_are_disabled_without_feature() {
        assert!(super::registry("synthetic").is_none());
        assert!(super::registry("synthetic-exit-on-interrupt").is_none());
    }
}
