use crate::containment::{Containment, ContainmentError};
use canterlot_proto::requests::{AnswerValue, DecisionPayload};
use std::{fmt, path::PathBuf, time::Duration};
use uuid::Uuid;

pub mod canterlot_mcp;
pub mod claude_code;
pub mod codex;
pub mod limit_note;
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
pub enum DecisionOutcome {
    Applied(DecisionProof),
    NotApplied(String),
    Undetermined,
}
impl fmt::Debug for DecisionOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied(proof) => f.debug_tuple("Applied").field(proof).finish(),
            Self::NotApplied(_) => f.debug_tuple("NotApplied").field(&"[REDACTED]").finish(),
            Self::Undetermined => f.write_str("Undetermined"),
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DecisionProof {
    pub proof_kind: String,
    pub payload: serde_json::Value,
}
impl fmt::Debug for DecisionProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecisionProof")
            .field("proof_kind", &self.proof_kind)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn valid_decision(
    kind: &str,
    payload: &serde_json::Value,
    decision: &DecisionPayload,
) -> bool {
    match (kind, decision) {
        ("permission", DecisionPayload::Permission { .. }) => true,
        ("question", DecisionPayload::Question { answers }) => {
            let Some(questions) = payload["questions"].as_array() else {
                return false;
            };
            if questions.is_empty() || questions.len() > 4 || answers.len() != questions.len() {
                return false;
            }
            let mut ids = std::collections::HashSet::new();
            questions.iter().all(|q| {
                let Some(id) = q["id"].as_str().filter(|id| !id.is_empty()) else {
                    return false;
                };
                if !ids.insert(id) {
                    return false;
                }
                let valid = |answer: &str| {
                    q["allow_custom"].as_bool() == Some(true)
                        || q["is_other"].as_bool() == Some(true)
                        || q["options"].as_array().is_some_and(|options| {
                            options.iter().any(|option| {
                                option.as_str().or_else(|| option["label"].as_str()) == Some(answer)
                            })
                        })
                };
                match answers.get(id) {
                    Some(AnswerValue::Single(answer)) => {
                        q["multi_select"].as_bool() != Some(true) && valid(answer)
                    }
                    Some(AnswerValue::Multiple(values)) => {
                        q["multi_select"].as_bool() == Some(true)
                            && !values.is_empty()
                            && values
                                .iter()
                                .collect::<std::collections::HashSet<_>>()
                                .len()
                                == values.len()
                            && values.iter().all(|value| valid(value))
                    }
                    None => false,
                }
            })
        }
        _ => false,
    }
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
    RequestOpened {
        request_id: String,
        kind: String,
        payload: serde_json::Value,
    },
    RequestClosed {
        request_id: String,
        reason: String,
    },
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
        /// Set to `"cli_policy"` when the tool ran without an `approve` prompt
        /// while the effective mode was not `bypassPermissions`.
        approved_by: Option<String>,
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
            Self::RequestOpened {
                request_id, kind, ..
            } => f
                .debug_struct("RequestOpened")
                .field("request_id", request_id)
                .field("kind", kind)
                .field("payload", &"[REDACTED]")
                .finish(),
            Self::RequestClosed { request_id, .. } => f
                .debug_struct("RequestClosed")
                .field("request_id", request_id)
                .finish_non_exhaustive(),
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
    pub settings_midsession_effort: bool,
    pub permission_modes: Vec<&'static str>,
    pub models: Vec<ModelCatalog>,
}
pub fn catalog() -> Vec<HarnessCatalog> {
    catalog_with(None, None)
}

pub fn catalog_with(
    cfg: Option<&crate::config::ClaudeCodeConfig>,
    cli: Option<&claude_code::version::CliVersion>,
) -> Vec<HarnessCatalog> {
    let mut entries = Vec::new();
    if cfg.is_some() && cli.is_some() {
        entries.push(HarnessCatalog {
            harness: "claude_code",
            provider: "claude",
            title: "Claude Code",
            images: true,
            settings_midsession: true,
            settings_midsession_effort: false,
            permission_modes: vec!["owner", "ask", "bypass", "read_only"],
            models: [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
                .into_iter()
                .map(|(id, title)| ModelCatalog {
                    id,
                    title,
                    efforts: vec!["low", "medium", "high", "xhigh", "max"],
                })
                .collect(),
        });
    }
    #[cfg(feature = "synthetic-adapter")]
    entries.extend(
        [
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
            settings_midsession_effort: true,
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
        }),
    );
    entries
}
#[derive(Clone, Copy)]
pub enum CloseMode {
    Graceful,
    Force,
}
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("{0}")]
    StartFailed(&'static str),
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
    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome;
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
    registry_with(
        profile,
        &AdapterContext {
            transmit_deadline,
            exec_dir: PathBuf::new(),
            claude_code: None,
        },
    )
}

pub struct AdapterContext {
    pub transmit_deadline: Duration,
    pub exec_dir: PathBuf,
    pub claude_code: Option<crate::config::ClaudeCodeConfig>,
}

pub fn known_harness(profile: &str) -> bool {
    if profile == "claude_code" {
        return true;
    }
    #[cfg(feature = "synthetic-adapter")]
    if matches!(
        profile,
        "synthetic" | "synthetic-exit-on-interrupt" | "synthetic-codex"
    ) {
        return true;
    }
    false
}

pub fn registry_with(profile: &str, ctx: &AdapterContext) -> Option<Box<dyn Adapter>> {
    if profile == "claude_code" {
        return ctx.claude_code.clone().map(|cfg| {
            Box::new(claude_code::ClaudeCodeAdapter::new(
                cfg,
                ctx.exec_dir.clone(),
                ctx.transmit_deadline,
            )) as Box<dyn Adapter>
        });
    }
    #[cfg(feature = "synthetic-adapter")]
    if matches!(
        profile,
        "synthetic" | "synthetic-exit-on-interrupt" | "synthetic-codex"
    ) {
        return Some(Box::new(
            synthetic::SyntheticAdapter::new(profile).with_transmit_deadline(ctx.transmit_deadline),
        ));
    }
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
