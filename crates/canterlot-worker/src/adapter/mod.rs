use crate::containment::{Containment, ContainmentError};
use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_proto::worker::v1::LimitWindow as ProtoLimitWindow;
use std::{fmt, path::PathBuf, time::Duration};
use uuid::Uuid;

pub mod antigravity;
pub mod canterlot_mcp;
pub mod claude_code;
pub mod cli_version;
pub mod codex;
pub mod frame_names;
pub mod held_tool;
pub mod jsonrpc;
pub mod labels;
pub mod limit_note;
pub mod opencode;
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
    pub instructions: InstructionSpec,
    /// Делегированная сессия: ни один MCP-сервер владельца не грузится, остаётся только canterlot.
    pub isolate_mcp: bool,
    /// Подсказки следующего сообщения: только Claude Code, только если хаб их включил.
    pub prompt_suggestions: bool,
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
/// Исход вливания ожидающего ввода в идущий ход («Отправить сейчас»).
/// В отличие от `transmit`, ход не открывается: доказательство приписывает
/// ввод чужому ходу, а конец хода его не закрывает.
#[derive(Debug)]
pub enum SteerOutcome {
    Accepted(Proof),
    Rejected(String),
    /// Вливать некуда (хода уже нет, запрос открыт, сжатие) или нечего:
    /// ввод остаётся в очереди и уйдёт следующим ходом.
    NotApplicable(String),
    Undetermined,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    /// Сервер принял запрос, итог придёт событием `Compacted`.
    Accepted,
    NotApplicable,
    /// Отказ или неизвестный исход: можно попробовать снова, запрос идемпотентен.
    Failed,
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    // Значения берутся из proto: приведение `as i32` не может дать чужое окно.
    FiveHour = ProtoLimitWindow::FiveHour as isize,
    Weekly = ProtoLimitWindow::Weekly as isize,
    WeeklyFable = ProtoLimitWindow::WeeklyFable as isize,
}

impl From<LimitWindow> for ProtoLimitWindow {
    fn from(window: LimitWindow) -> Self {
        match window {
            LimitWindow::FiveHour => Self::FiveHour,
            LimitWindow::Weekly => Self::Weekly,
            LimitWindow::WeeklyFable => Self::WeeklyFable,
        }
    }
}

impl From<LimitWindow> for i32 {
    fn from(window: LimitWindow) -> Self {
        ProtoLimitWindow::from(window).into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub instructions: Option<InstructionSpec>,
}

/// Инструкции агента на исполнение: режим, текст и версия пресета.
/// Текст — данные уровня сообщения: в Debug только длина.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionMode {
    Clean,
    Owner,
    Preset,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstructionSpec {
    pub mode: InstructionMode,
    pub text: String,
    pub preset_id: Option<Uuid>,
    pub revision: u64,
    pub title: String,
}

impl fmt::Debug for InstructionSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstructionSpec")
            .field("mode", &self.mode)
            .field("preset_id", &self.preset_id)
            .field("revision", &self.revision)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstructionError {
    UnknownMode,
    EmptyText,
    TextTooLarge,
    UnexpectedText,
    BadPresetId,
}

impl InstructionSpec {
    /// Разбирает proto-инструкции хаба; `None` — старший хаб, значит «Как у владельца».
    pub fn from_proto(
        proto: Option<&canterlot_proto::worker::v1::Instructions>,
    ) -> Result<Self, InstructionError> {
        use canterlot_proto::limits::MAX_PRESET_TEXT_BYTES;
        let Some(proto) = proto else {
            return Ok(Self {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            });
        };
        match proto.mode.as_str() {
            "clean" | "owner" => {
                if !proto.text.is_empty() || !proto.preset_id.is_empty() || proto.revision != 0 {
                    return Err(InstructionError::UnexpectedText);
                }
                Ok(Self {
                    mode: if proto.mode == "clean" {
                        InstructionMode::Clean
                    } else {
                        InstructionMode::Owner
                    },
                    text: String::new(),
                    preset_id: None,
                    revision: 0,
                    title: String::new(),
                })
            }
            "preset" => {
                if proto.text.trim().is_empty() {
                    return Err(InstructionError::EmptyText);
                }
                if proto.text.len() > MAX_PRESET_TEXT_BYTES {
                    return Err(InstructionError::TextTooLarge);
                }
                Ok(Self {
                    mode: InstructionMode::Preset,
                    text: proto.text.clone(),
                    preset_id: Some(
                        proto
                            .preset_id
                            .parse()
                            .map_err(|_| InstructionError::BadPresetId)?,
                    ),
                    revision: proto.revision,
                    title: proto.title.clone(),
                })
            }
            _ => Err(InstructionError::UnknownMode),
        }
    }

    pub fn to_proto(&self) -> canterlot_proto::worker::v1::Instructions {
        canterlot_proto::worker::v1::Instructions {
            mode: match self.mode {
                InstructionMode::Clean => "clean".into(),
                InstructionMode::Owner => "owner".into(),
                InstructionMode::Preset => "preset".into(),
            },
            text: self.text.clone(),
            preset_id: self.preset_id.map(|id| id.to_string()).unwrap_or_default(),
            revision: self.revision,
            title: self.title.clone(),
        }
    }

    /// Одинаковый эффект: режим, пресет и версия совпали; текст из хаба всегда свежий.
    pub fn same_effect(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.preset_id == other.preset_id
            && self.revision == other.revision
    }
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
    /// Картинка в base64: декодирует подготовка вне цикла сопровождения.
    Base64(String),
}
impl fmt::Debug for MediaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => f
                .debug_struct("Bytes")
                .field("bytes", &bytes.len())
                .finish(),
            Self::Path(_) => f.write_str("Path"),
            Self::Base64(text) => f.debug_struct("Base64").field("len", &text.len()).finish(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
/// Пункт собственного списка задач харнесса; текст — данные сообщения.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}
impl fmt::Debug for TodoItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodoItem")
            .field("text_bytes", &self.text.len())
            .field("status", &self.status)
            .finish()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    Agent,
    Bash,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundState {
    Running,
    Completed,
    Failed,
    Stopped,
}
/// Фоновая задача агента; описание — данные сообщения.
#[derive(Clone, PartialEq, Eq)]
pub struct BackgroundTask {
    pub task_id: String,
    pub kind: BackgroundKind,
    /// Блок вызова Agent или Bash.
    pub block_id: Option<Uuid>,
    pub parent_task_id: Option<String>,
    pub description: String,
    pub state: BackgroundState,
}
impl fmt::Debug for BackgroundTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackgroundTask")
            .field("task_id", &self.task_id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .finish_non_exhaustive()
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
    /// Агент работает без ввода (завершилась фоновая задача): для сопровождения это ход
    /// последней команды, его можно прервать, а новый ввод ждёт его конца.
    RunAdopted {
        input_command_id: Uuid,
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
        /// Имена ключей неизвестного кадра, без значений; см. `frame_names`.
        field_names: Vec<String>,
    },
    ProcessExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// Доказательство доставки пришло после того, как `transmit` вернул
    /// `Undetermined` (срок вышел раньше первого кадра ответа).
    InputAccepted {
        command_id: Uuid,
    },
    /// Агент перезапущен между ходами со сменой инструкций; нативный ID тот же.
    CliRestarted {
        cli_pid: u32,
        cli_start_ticks: u64,
    },
    /// Агент вызывает инструмент canterlot через хаб; аргументы — данные сообщения.
    AgentCall {
        call_id: Uuid,
        tool: String,
        arguments: serde_json::Value,
    },
    /// Файл для владельца: читает и загружает супервизор.
    ShareFile {
        call_id: Uuid,
        arguments: serde_json::Value,
    },
    /// Лимиты всех провайдеров: ответ хаба, при отказе — местный снимок.
    Limits {
        call_id: Uuid,
    },
    /// Полный список задач агента. `lane` — блок вызова субагента, у основного агента `None`.
    TodoUpdated {
        lane: Option<Uuid>,
        items: Vec<TodoItem>,
    },
    /// Предсказанное следующее сообщение владельца, уже обрезанное до `MAX_PROMPT_SUGGESTION_BYTES`.
    PromptSuggested {
        text: String,
    },
    /// Все работающие фоновые задачи и закончившиеся с прошлого события; `wake_turn` — идёт
    /// ход, который агент начал сам.
    BackgroundWork {
        tasks: Vec<BackgroundTask>,
        wake_turn: bool,
    },
    /// Отложенная смена настроек дождалась: фоновой работы нет, ход не идёт. Сопровождение
    /// решает, можно ли останавливать CLI, и зовёт `apply_deferred`.
    DeferredRestartDue,
    /// Один и тот же отказ подряд: владельцу предлагается сжать контекст, очередь встаёт.
    SessionStuck,
    /// Сжатие, запрошенное через `compact`, закончилось.
    Compacted {
        ok: bool,
    },
    /// Контекст сжимается (автоматически или по кнопке): начало эпизода для
    /// durable ленты. Транзиентная фаза `ContextMaintenance` шлётся отдельно.
    CompactionStarted {
        manual: bool,
    },
    /// Эпизод сжатия закончен. `pre_tokens` — токенов до сжатия, если CLI дал
    /// (0 — неизвестно). Приходит и без `CompactionStarted` (переподключение):
    /// сопровождение тогда открывает и закрывает эпизод сразу.
    CompactionFinished {
        manual: bool,
        ok: bool,
        pre_tokens: u64,
        /// Токенов после сжатия, если CLI дал (0 — неизвестно): индикатор ждёт новый размер.
        post_tokens: u64,
    },
    /// Живой расход контекста в ходу; адаптер ограничивает частоту, хаб держит
    /// последнее, итог хода в `RunFinished` главнее.
    ContextProgress {
        context_tokens: u64,
        context_window_tokens: u64,
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
            Self::RunAdopted { input_command_id } => f
                .debug_struct("RunAdopted")
                .field("input_command_id", input_command_id)
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
            Self::InputAccepted { command_id } => f
                .debug_struct("InputAccepted")
                .field("command_id", command_id)
                .finish(),
            Self::CliRestarted {
                cli_pid,
                cli_start_ticks,
            } => f
                .debug_struct("CliRestarted")
                .field("cli_pid", cli_pid)
                .field("cli_start_ticks", cli_start_ticks)
                .finish(),
            Self::AgentCall { call_id, tool, .. } => f
                .debug_struct("AgentCall")
                .field("call_id", call_id)
                .field("tool", tool)
                .finish_non_exhaustive(),
            Self::ShareFile { call_id, .. } => f
                .debug_struct("ShareFile")
                .field("call_id", call_id)
                .finish_non_exhaustive(),
            Self::Limits { call_id } => f
                .debug_struct("Limits")
                .field("call_id", call_id)
                .finish_non_exhaustive(),
            Self::BackgroundWork { tasks, wake_turn } => f
                .debug_struct("BackgroundWork")
                .field("tasks", &tasks.len())
                .field("wake_turn", wake_turn)
                .finish(),
            Self::DeferredRestartDue => f.write_str("DeferredRestartDue"),
            Self::SessionStuck => f.write_str("SessionStuck"),
            Self::Compacted { ok } => f.debug_struct("Compacted").field("ok", ok).finish(),
            Self::CompactionStarted { manual } => f
                .debug_struct("CompactionStarted")
                .field("manual", manual)
                .finish(),
            Self::CompactionFinished { manual, ok, .. } => f
                .debug_struct("CompactionFinished")
                .field("manual", manual)
                .field("ok", ok)
                .finish_non_exhaustive(),
            Self::ContextProgress { .. } => f.write_str("ContextProgress"),
            Self::TodoUpdated { lane, items } => f
                .debug_struct("TodoUpdated")
                .field("lane", lane)
                .field("items", &items.len())
                .finish(),
            Self::PromptSuggested { text } => f
                .debug_struct("PromptSuggested")
                .field("text_bytes", &text.len())
                .finish(),
        }
    }
}

/// Ответ хаба на вызов инструмента агента; результат — данные сообщения.
pub enum AgentCallResult {
    Ok(serde_json::Value),
    Err { code: String, message: String },
}

/// Отказ хаба по `limits.report` заменяется местным снимком: старый хаб не знает
/// многопровайдерный отчёт, а `canterlot_limits` должен отвечать и там.
pub fn limits_fallback(
    pending: &mut std::collections::HashMap<uuid::Uuid, String>,
    call_id: uuid::Uuid,
    result: AgentCallResult,
) -> AgentCallResult {
    let Some(text) = pending.remove(&call_id) else {
        return result;
    };
    match result {
        AgentCallResult::Ok(value) if value.get("text").is_some() => AgentCallResult::Ok(value),
        _ => AgentCallResult::Ok(serde_json::json!({"text": text})),
    }
}

impl fmt::Debug for AgentCallResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok(_) => f.debug_tuple("Ok").field(&"[REDACTED]").finish(),
            Self::Err { code, .. } => f
                .debug_struct("Err")
                .field("code", code)
                .finish_non_exhaustive(),
        }
    }
}
#[derive(Default)]
pub struct ModelCatalog {
    pub id: String,
    pub title: String,
    pub efforts: Vec<String>,
    /// Модель принимает картинки; `None`: у харнесса это не свойство модели.
    pub images: Option<bool>,
    /// Окно контекста модели в токенах, если харнесс его знает.
    pub context_window: Option<u64>,
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
// Первая модель харнесса — выбор формы по умолчанию. `haiku[1m]` CLI отказывает в
// `set_model`, поэтому у Haiku варианта 1M нет. Обычные варианты убраны из каталога,
// но сессии на них продолжают работать: каталог ограничивает только новый выбор.
/// Окно контекста моделей Claude Code: варианты 1M отдают 1M, а не 200K.
const CLAUDE_MODELS: &[(&str, &str, u64)] = &[
    ("opus[1m]", "Opus 1M", 1_000_000),
    ("sonnet[1m]", "Sonnet 1M", 1_000_000),
    ("fable[1m]", "Fable 1M", 1_000_000),
    ("haiku", "Haiku", 200_000),
];

/// Харнессы боевой сборки в порядке каталога: те же строки хаб хранит в
/// `machine_harnesses.harness`. Синтетические сюда не входят.
pub const HARNESSES: &[&str] = &["claude_code", "codex", "opencode", "antigravity"];

pub fn catalog() -> Vec<HarnessCatalog> {
    catalog_with(None, None, None, None, None)
}

pub fn catalog_with(
    cfg: Option<&crate::config::ClaudeCodeConfig>,
    cli: Option<&claude_code::version::CliVersion>,
    codex: Option<&codex::CodexCatalog>,
    opencode: Option<&opencode::OpenCodeCatalog>,
    antigravity: Option<&antigravity::AntigravityCatalog>,
) -> Vec<HarnessCatalog> {
    let mut entries = Vec::new();
    if cfg.is_some() && cli.is_some() {
        entries.push(HarnessCatalog {
            harness: "claude_code",
            provider: "claude",
            title: "Claude Code",
            images: true,
            settings_midsession: true,
            settings_midsession_effort: true,
            permission_modes: vec!["owner", "ask", "bypass", "read_only"],
            models: CLAUDE_MODELS
                .iter()
                .map(|&(id, title, context_window)| ModelCatalog {
                    id: id.into(),
                    title: title.into(),
                    efforts: ["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    context_window: Some(context_window),
                    ..Default::default()
                })
                .collect(),
        });
    }
    if let Some(catalog) = codex {
        entries.push(HarnessCatalog {
            harness: "codex",
            provider: "codex",
            title: "Codex",
            images: true,
            settings_midsession: true,
            settings_midsession_effort: true,
            permission_modes: vec!["owner", "ask", "bypass", "read_only"],
            models: catalog.models(),
        });
    }
    if let Some(catalog) = opencode {
        entries.push(catalog.harness());
    }
    if let Some(catalog) = antigravity {
        entries.push(catalog.harness());
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
                    id: "synthetic-large".into(),
                    title: "Большая синтетическая".into(),
                    efforts: ["low", "medium", "high"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    ..Default::default()
                },
                ModelCatalog {
                    id: "synthetic-small".into(),
                    title: "Малая синтетическая".into(),
                    efforts: ["low", "medium"].into_iter().map(str::to_owned).collect(),
                    ..Default::default()
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
    /// Умеет ли адаптер запуститься без MCP-серверов владельца. Без этого делегированная
    /// сессия не стартует: молчаливый запуск с серверами владельца запрещён.
    fn isolates_mcp(&self) -> bool {
        false
    }
    async fn start(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError>;
    fn validate_transmission(&self, _input: &Transmission) -> Result<(), &'static str> {
        Ok(())
    }
    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome;
    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome;
    /// Отказать в запросе без участия владельца: он его не видел или уже не может
    /// ответить, а CLI ждёт. Журнал этого запроса ведёт сопровождение: `RequestClosed`
    /// адаптер не шлёт.
    async fn refuse_request(&mut self, request_id: &str, message: &str) {
        let deny = DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message: Some(message.to_owned()),
        };
        let _ = self.deliver_decision(request_id, &deny).await;
    }
    async fn interrupt(&mut self) -> InterruptOutcome;
    /// Влить ожидающий ввод в идущий ход («Отправить сейчас»). Вызывается,
    /// только когда ход идёт; новый ход не открывается, слот хода не трогается.
    /// Отказ — через `NotApplicable` с причиной для подписи в очереди.
    async fn steer(&mut self, input: &Transmission) -> SteerOutcome;
    /// Сжать контекст по действию владельца. Сжимает только адаптер, который это умеет.
    async fn compact(&mut self, _command: Uuid) -> CompactOutcome {
        CompactOutcome::NotApplicable
    }
    /// Ввод ждёт итога сжатия: служебный ввод этого CLI идёт тем же каналом между
    /// ходами, и чужой итог приписался бы ходу владельца. По умолчанию сжатие
    /// серверное (`delivery: "queue"`), гонки нет, hold не нужен.
    fn holds_input_during_compaction(&self) -> bool {
        false
    }
    async fn next_event(&mut self) -> AdapterEvent;
    /// Применить смену настроек, отложенную до конца фоновой работы; см. `DeferredRestartDue`.
    async fn apply_deferred(&mut self, _group: &dyn Containment) {}
    async fn agent_call_result(&mut self, _call_id: Uuid, _result: AgentCallResult) {}
    /// Срок мягкой остановки из таймеров сопровождения: сколько `Graceful`
    /// close ждёт выхода после SIGTERM, прежде чем убить CLI.
    fn set_stop_grace(&mut self, _grace: Duration) {}
    /// Первый срок пробы `<cli> --version`; повтор идёт с удвоенным сроком.
    fn set_cli_version_probe(&mut self, _deadline: Duration) {}
    /// Общий на процессы файл успешных проб версии; `None` — только память.
    fn set_cli_version_cache(&mut self, _cache: Option<PathBuf>) {}
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
            ready_deadline: transmit_deadline,
            exec_dir: PathBuf::new(),
            claude_code: None,
            codex: None,
            opencode: None,
            antigravity: None,
            delegate_mcp_servers: Vec::new(),
        },
    )
}

pub struct AdapterContext {
    pub transmit_deadline: Duration,
    /// Срок запуска CLI: пока агент не показал себя, передача ждёт его, а не `transmit_deadline`.
    pub ready_deadline: Duration,
    pub exec_dir: PathBuf,
    pub claude_code: Option<crate::config::ClaudeCodeConfig>,
    pub codex: Option<crate::config::CodexConfig>,
    pub opencode: Option<crate::config::OpenCodeConfig>,
    pub antigravity: Option<crate::config::AntigravityConfig>,
    /// Серверы MCP владельца, разрешённые делегированной сессии рядом с canterlot (D24).
    /// Antigravity их не получает.
    pub delegate_mcp_servers: Vec<String>,
}

/// Разрешённые имена без повторов и без негодных: canterlot владелец не подменит.
pub(crate) fn delegate_mcp_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if crate::config::valid_delegate_mcp_name(name) && !out.contains(name) {
            out.push(name.clone());
        }
    }
    out
}

pub fn known_harness(profile: &str) -> bool {
    if HARNESSES.contains(&profile) {
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
            Box::new(
                claude_code::ClaudeCodeAdapter::new(
                    cfg,
                    ctx.exec_dir.clone(),
                    ctx.transmit_deadline,
                )
                .with_ready_deadline(ctx.ready_deadline)
                .with_delegate_mcp_servers(&ctx.delegate_mcp_servers),
            ) as Box<dyn Adapter>
        });
    }
    if profile == "codex" {
        return ctx.codex.clone().map(|cfg| {
            Box::new(
                codex::CodexAdapter::new(cfg, ctx.exec_dir.clone(), ctx.transmit_deadline)
                    .with_delegate_mcp_servers(&ctx.delegate_mcp_servers),
            ) as Box<dyn Adapter>
        });
    }
    if profile == "opencode" {
        return ctx.opencode.clone().map(|cfg| {
            Box::new(
                opencode::OpenCodeAdapter::new(cfg, ctx.exec_dir.clone(), ctx.transmit_deadline)
                    .with_delegate_mcp_servers(&ctx.delegate_mcp_servers),
            ) as Box<dyn Adapter>
        });
    }
    if profile == "antigravity" {
        return ctx.antigravity.clone().map(|cfg| {
            Box::new(
                antigravity::AntigravityAdapter::new(
                    cfg,
                    ctx.exec_dir.clone(),
                    ctx.transmit_deadline,
                )
                .with_ready_deadline(ctx.ready_deadline),
            ) as Box<dyn Adapter>
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

#[cfg(test)]
mod harness_list_tests {
    use super::*;

    fn real(harness: &&str) -> bool {
        !harness.starts_with("synthetic")
    }

    #[test]
    fn harness_list_matches_full_catalog_and_proto() {
        let claude = crate::config::ClaudeCodeConfig::default();
        let claude_cli = claude_code::version::CliVersion::Verified("2.1.311".into());
        let version = || cli_version::CliVersion::Verified("1.0.0".into());
        let codex = codex::CodexCatalog {
            version: version(),
            models: vec![],
        };
        let opencode = opencode::OpenCodeCatalog {
            version: version(),
            models: vec![],
        };
        let antigravity = antigravity::AntigravityCatalog {
            version: version(),
            models: vec![],
            current_model: None,
            efforts: vec![],
            current_effort: None,
        };
        let listed: Vec<&str> = catalog_with(
            Some(&claude),
            Some(&claude_cli),
            Some(&codex),
            Some(&opencode),
            Some(&antigravity),
        )
        .iter()
        .map(|entry| entry.harness)
        .filter(real)
        .collect();
        assert_eq!(listed, HARNESSES);
        let proto: Vec<&str> = canterlot_proto::harness::HARNESS_PROVIDERS
            .iter()
            .map(|&(harness, _)| harness)
            .filter(real)
            .collect();
        assert_eq!(proto, HARNESSES);
        assert!(HARNESSES.iter().all(|harness| known_harness(harness)));
    }
}

// Общий вспомогательный код адаптеров.

use std::io;

/// Ребёнок уже убран другой проверкой members (общий супервизор тестового
/// процесса): `ECHILD` означает, что процесса точно нет.
pub(crate) fn reaped(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error())
}

/// Выполнить `spawn` с повторами при ETXTBSY/`ExecutableFileBusy`.
pub async fn spawn_with_retry_io<F, Fut, T>(mut spawn: F) -> io::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut attempts = 0;
    loop {
        match spawn().await {
            Ok(v) => return Ok(v),
            Err(err) => {
                if err.kind() == io::ErrorKind::ExecutableFileBusy {
                    attempts += 1;
                    if attempts < 5 {
                        tokio::time::sleep(Duration::from_millis(25 * attempts as u64)).await;
                        continue;
                    }
                }
                return Err(err);
            }
        }
    }
}

/// Синхронный вариант для мест запуска без асинхронного контекста
/// (`AppServerProcess::spawn`, запуск супервизора): спящий поток блокируется
/// только в редкой ветви ETXTBSY при старте процесса, обычный путь без задержек.
pub fn spawn_with_retry_sync<T>(mut spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempts = 0u64;
    loop {
        match spawn() {
            Ok(v) => return Ok(v),
            Err(err) => {
                if err.kind() == io::ErrorKind::ExecutableFileBusy && attempts < 4 {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(25 * attempts));
                    continue;
                }
                return Err(err);
            }
        }
    }
}

#[cfg(test)]
mod spawn_retry_tests {
    use super::{spawn_with_retry_io, spawn_with_retry_sync};
    use std::cell::Cell;
    use std::io;

    fn busy() -> io::Error {
        io::Error::from(io::ErrorKind::ExecutableFileBusy)
    }

    #[test]
    fn sync_retry_recovers_after_transient_busy() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_sync(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                Err(busy())
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn sync_retry_gives_up_and_keeps_the_error() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_sync(|| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(busy())
        });
        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::ExecutableFileBusy
        );
        assert_eq!(attempts.get(), 5);
    }

    #[test]
    fn sync_retry_passes_other_errors_through_at_once() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_sync(|| {
            attempts.set(attempts.get() + 1);
            Err::<(), _>(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test]
    async fn async_retry_recovers_after_transient_busy() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_io(|| {
            attempts.set(attempts.get() + 1);
            let attempt = attempts.get();
            async move { if attempt < 3 { Err(busy()) } else { Ok(7) } }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(attempts.get(), 3);
    }

    #[tokio::test]
    async fn async_retry_gives_up_and_keeps_the_error() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_io(|| {
            attempts.set(attempts.get() + 1);
            async { Err::<(), _>(busy()) }
        })
        .await;
        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::ExecutableFileBusy
        );
        assert_eq!(attempts.get(), 5);
    }

    #[tokio::test]
    async fn async_retry_passes_other_errors_through_at_once() {
        let attempts = Cell::new(0);
        let result = spawn_with_retry_io(|| {
            attempts.set(attempts.get() + 1);
            async { Err::<(), _>(io::Error::from(io::ErrorKind::NotFound)) }
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1);
    }
}
