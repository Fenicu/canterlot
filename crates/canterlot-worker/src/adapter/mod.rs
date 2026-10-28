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
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub instruction_profile: String,
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
    pub files: Vec<LocalFile>,
    pub settings: Option<SettingsChange>,
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub agent: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitWindow {
    FiveHour,
    Weekly,
}
#[derive(Clone, Debug)]
pub enum LimitFact {
    Observed {
        window: LimitWindow,
        used_permille: u16,
        resets_at_ms: Option<u64>,
    },
    Reached {
        window: LimitWindow,
        resets_at_ms: Option<u64>,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SettingsChange {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalFile {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub image: bool,
}
impl fmt::Debug for LocalFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalFile")
            .field("size", &self.size)
            .finish()
    }
}
/// Источник картинки: байты из результата инструмента или путь, который сопровождение читает сразу.
#[derive(Clone)]
pub enum MediaSource {
    Bytes(Vec<u8>),
    Path(PathBuf),
}
impl fmt::Debug for MediaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => f
                .debug_struct("Bytes")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Path(_) => f.write_str("Path"),
        }
    }
}
#[derive(Clone)]
pub enum AdapterEvent {
    Phase {
        phase: Phase,
        tool_name: Option<String>,
        summary: Option<String>,
    },
    BlockStarted {
        block_id: Uuid,
        kind: BlockKind,
        input_command_id: Option<Uuid>,
        parent: Option<Uuid>,
        agent: String,
        model: String,
        tool_name: Option<String>,
        summary: Option<String>,
    },
    BlockDelta {
        block_id: Uuid,
        append: String,
    },
    BlockFinal {
        block_id: Uuid,
        text: String,
        summary: Option<String>,
    },
    Media {
        block_id: Uuid,
        source: MediaSource,
    },
    RunFinished {
        input_command_id: Uuid,
        usage: Vec<ModelUsage>,
        context_tokens: u64,
        context_window_tokens: u64,
    },
    Limit(LimitFact),
    ContextExhausted,
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
            Self::Phase { phase, .. } => f
                .debug_struct("Phase")
                .field("phase", phase)
                .finish_non_exhaustive(),
            Self::BlockStarted {
                block_id,
                kind,
                parent,
                ..
            } => f
                .debug_struct("BlockStarted")
                .field("block_id", block_id)
                .field("kind", kind)
                .field("parent", parent)
                .finish_non_exhaustive(),
            Self::BlockDelta { block_id, append } => f
                .debug_struct("BlockDelta")
                .field("block_id", block_id)
                .field("append_bytes", &append.len())
                .finish(),
            Self::BlockFinal { block_id, text, .. } => f
                .debug_struct("BlockFinal")
                .field("block_id", block_id)
                .field("text_bytes", &text.len())
                .finish_non_exhaustive(),
            Self::Media { block_id, source } => f
                .debug_struct("Media")
                .field("block_id", block_id)
                .field("source", source)
                .finish(),
            Self::RunFinished {
                input_command_id, ..
            } => f
                .debug_struct("RunFinished")
                .field("input_command_id", input_command_id)
                .finish_non_exhaustive(),
            Self::Limit(fact) => f.debug_tuple("Limit").field(fact).finish(),
            Self::ContextExhausted => f.write_str("ContextExhausted"),
            Self::Diagnostic { kind, required, .. } => f
                .debug_struct("Diagnostic")
                .field("kind", kind)
                .field("required", required)
                .finish_non_exhaustive(),
            Self::ProcessExited { code, signal } => f
                .debug_struct("ProcessExited")
                .field("code", code)
                .field("signal", signal)
                .finish(),
        }
    }
}
pub struct ModelCatalog {
    pub id: &'static str,
    pub title: &'static str,
    pub efforts: Vec<&'static str>,
}
pub struct HarnessCatalog {
    pub harness: &'static str,
    pub provider: &'static str,
    pub title: &'static str,
    pub images: bool,
    pub settings_midsession: bool,
    pub permission_modes: Vec<&'static str>,
    pub models: Vec<ModelCatalog>,
}
pub fn catalog() -> Vec<HarnessCatalog> {
    #[cfg(feature = "synthetic-adapter")]
    return [
        "synthetic",
        "synthetic-exit-on-interrupt",
        "synthetic-codex",
    ]
    .into_iter()
    .map(|harness| HarnessCatalog {
        harness,
        provider: canterlot_proto::harness::provider_of(harness).expect("known harness"),
        title: match harness {
            "synthetic-codex" => "Синтетический Codex",
            "synthetic-exit-on-interrupt" => "Синтетический с выходом при прерывании",
            _ => "Синтетический",
        },
        images: true,
        settings_midsession: true,
        permission_modes: vec!["owner", "ask", "bypass", "read_only"],
        models: vec![
            ModelCatalog {
                id: "synthetic-large",
                title: "Большая синтетическая",
                efforts: vec!["low", "medium", "high"],
            },
            ModelCatalog {
                id: "synthetic-small",
                title: "Малая синтетическая",
                efforts: vec!["low", "medium"],
            },
        ],
    })
    .collect();
    #[cfg(not(feature = "synthetic-adapter"))]
    Vec::new()
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
    fn validate_transmission(&self, _input: &Transmission) -> Result<(), &'static str> {
        Ok(())
    }
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
    if matches!(
        profile,
        "synthetic" | "synthetic-exit-on-interrupt" | "synthetic-codex"
    ) {
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
