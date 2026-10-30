// Перевод уведомлений app-server в события адаптера: блоки, фазы, субагенты,
// расход, картинки и ошибки. Переводчик не паникует и сессию не завершает:
// неизвестное игнорируется с одной диагностикой на метод за исполнение.
use super::super::{AdapterEvent, BlockKind, MediaSource, ModelUsage, Phase};
use super::rpc::RpcId;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    Unknown,
}

impl fmt::Debug for TurnStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            TurnStatus::Completed => "Completed",
            TurnStatus::Interrupted => "Interrupted",
            TurnStatus::Failed => "Failed",
            TurnStatus::InProgress => "InProgress",
            TurnStatus::Unknown => "Unknown",
        };
        f.write_str(name)
    }
}

fn parse_turn_status(status: &str) -> TurnStatus {
    match status {
        "completed" => TurnStatus::Completed,
        "interrupted" => TurnStatus::Interrupted,
        "failed" => TurnStatus::Failed,
        "inProgress" => TurnStatus::InProgress,
        _ => TurnStatus::Unknown,
    }
}

pub struct TurnErrorInfo {
    pub kind: String,
    pub http_status: Option<u16>,
    pub message: String,
}

impl fmt::Debug for TurnErrorInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnErrorInfo")
            .field("kind", &self.kind)
            .field("http_status", &self.http_status)
            .finish_non_exhaustive()
    }
}

fn parse_turn_error(error: &Value) -> TurnErrorInfo {
    let info = error.get("codexErrorInfo").unwrap_or(&Value::Null);
    let (kind, http_status) = match info {
        Value::String(tag) => (tag.clone(), None),
        Value::Object(map) => {
            let kind = map.keys().next().cloned().unwrap_or_default();
            let http_status = map
                .values()
                .next()
                .and_then(|v| v.get("httpStatusCode"))
                .and_then(Value::as_u64)
                .and_then(|n| u16::try_from(n).ok());
            (kind, http_status)
        }
        _ => (String::new(), None),
    };
    TurnErrorInfo {
        kind,
        http_status,
        message: error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

pub enum Translated {
    Event(AdapterEvent),
    UserMessage {
        thread_id: String,
        turn_id: String,
        client_id: Option<String>,
    },
    TurnStarted {
        turn_id: String,
    },
    TurnCompleted {
        turn_id: String,
        status: TurnStatus,
        error: Option<TurnErrorInfo>,
    },
    RateLimits(Value),
    ServerRequestResolved {
        request_id: RpcId,
    },
    ItemCompleted {
        item_id: String,
        item_type: String,
        status: Option<String>,
        approved_by_cli: bool,
    },
    SettingsUpdated(Value),
    McpStatus {
        name: String,
        status: String,
    },
    Ignored,
}

impl fmt::Debug for Translated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Translated::Event(event) => f.debug_tuple("Event").field(event).finish(),
            Translated::UserMessage {
                thread_id,
                turn_id,
                client_id,
            } => f
                .debug_struct("UserMessage")
                .field("thread_id", thread_id)
                .field("turn_id", turn_id)
                .field("has_client_id", &client_id.is_some())
                .finish(),
            Translated::TurnStarted { turn_id } => f
                .debug_struct("TurnStarted")
                .field("turn_id", turn_id)
                .finish(),
            Translated::TurnCompleted {
                turn_id,
                status,
                error,
            } => f
                .debug_struct("TurnCompleted")
                .field("turn_id", turn_id)
                .field("status", status)
                .field("has_error", &error.is_some())
                .finish(),
            Translated::RateLimits(_) => f.write_str("RateLimits"),
            Translated::ServerRequestResolved { request_id } => f
                .debug_struct("ServerRequestResolved")
                .field("request_id", request_id)
                .finish(),
            Translated::ItemCompleted {
                item_id,
                item_type,
                status,
                approved_by_cli,
            } => f
                .debug_struct("ItemCompleted")
                .field("item_id", item_id)
                .field("item_type", item_type)
                .field("status", status)
                .field("approved_by_cli", approved_by_cli)
                .finish(),
            Translated::SettingsUpdated(_) => f.write_str("SettingsUpdated"),
            Translated::McpStatus { name, status } => f
                .debug_struct("McpStatus")
                .field("name", name)
                .field("status", status)
                .finish(),
            Translated::Ignored => f.write_str("Ignored"),
        }
    }
}

struct ChildInfo {
    agent: String,
    model: String,
    parent: Option<Uuid>,
}

struct OpenBlock {
    block_id: Uuid,
    agent: String,
    model: String,
    parent: Option<Uuid>,
    command_id: Option<Uuid>,
    item_type: String,
    text: String,
    result_block: Option<Uuid>,
    result_text: String,
    reasoning: Option<ReasoningState>,
    pending_diff: String,
}

struct ReasoningState {
    parts: Vec<String>,
    has_summary: bool,
    opened: bool,
}

struct UsageAcc {
    agent: String,
    model: String,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

pub struct Translator {
    main_thread_id: String,
    model: String,
    workdir: Option<PathBuf>,
    children: HashMap<String, ChildInfo>,
    spawn_parents: HashMap<String, Uuid>,
    open: HashMap<String, OpenBlock>,
    active_turn: Option<(String, Uuid)>,
    usage: HashMap<(String, String), UsageAcc>,
    last_context_tokens: u64,
    last_context_window: u64,
    phase: Phase,
    previous_phase: Phase,
    retry_count: u32,
    approval_items: HashSet<String>,
    unknown_methods: HashSet<String>,
}

impl fmt::Debug for Translator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Translator")
            .field("main_thread_id", &self.main_thread_id)
            .field("open_blocks", &self.open.len())
            .field("children", &self.children.len())
            .field("has_active_turn", &self.active_turn.is_some())
            .field("usage_entries", &self.usage.len())
            .finish_non_exhaustive()
    }
}

fn cut120(text: &str) -> String {
    text.chars().take(120).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl Translator {
    pub fn new(main_thread_id: &str, model: &str) -> Self {
        Self {
            main_thread_id: main_thread_id.to_owned(),
            model: model.to_owned(),
            workdir: None,
            children: HashMap::new(),
            spawn_parents: HashMap::new(),
            open: HashMap::new(),
            active_turn: None,
            usage: HashMap::new(),
            last_context_tokens: 0,
            last_context_window: 0,
            phase: Phase::Generating,
            previous_phase: Phase::Generating,
            retry_count: 0,
            approval_items: HashSet::new(),
            unknown_methods: HashSet::new(),
        }
    }

    // Рабочий каталог для относительных путей в сводке apply_patch.
    pub fn set_workdir(&mut self, workdir: &Path) {
        self.workdir = Some(workdir.to_owned());
    }

    // Задача 3 зовёт после ответа turn/start.
    pub fn begin_turn(&mut self, turn_id: &str, command_id: Uuid) {
        self.active_turn = Some((turn_id.to_owned(), command_id));
        self.usage.clear();
        self.last_context_tokens = 0;
        self.last_context_window = 0;
        self.retry_count = 0;
    }

    // Запрос разрешения по элементу пришёл серверным запросом: решал владелец, не CLI.
    pub fn note_approval(&mut self, item_id: &str) {
        self.approval_items.insert(item_id.to_owned());
    }

    pub fn approval_noted(&self, item_id: &str) -> bool {
        self.approval_items.contains(item_id)
    }

    // Смена модели ходом без уведомления от сервера (задача 7 правит оптимистично).
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_owned();
    }

    // Не-JSON строка stdout: диагностика, поток жив.
    pub fn feed_unparsed(&mut self) -> Translated {
        Translated::Event(AdapterEvent::Diagnostic {
            kind: "unparsed_line",
            event_type: "unparsed_line".into(),
            required: false,
        })
    }

    // RunFinished с расходом и контекстом; зовут на TurnCompleted главного хода.
    pub fn finish_turn(&mut self) -> AdapterEvent {
        let command_id = self
            .active_turn
            .take()
            .map(|(_, id)| id)
            .unwrap_or_default();
        let mut usage: Vec<ModelUsage> = self
            .usage
            .values()
            .map(|acc| ModelUsage {
                model: acc.model.clone(),
                agent: acc.agent.clone(),
                input_tokens: acc.input,
                output_tokens: acc.output,
                cache_read_tokens: acc.cache_read,
                cache_write_tokens: acc.cache_write,
            })
            .collect();
        usage.sort_by(|a, b| (&a.agent, &a.model).cmp(&(&b.agent, &b.model)));
        self.usage.clear();
        AdapterEvent::RunFinished {
            input_command_id: command_id,
            usage,
            context_tokens: self.last_context_tokens,
            context_window_tokens: self.last_context_window,
        }
    }

    pub fn feed(&mut self, method: &str, params: &Value) -> Vec<Translated> {
        match method {
            "turn/started" => self.on_turn_started(params),
            "turn/completed" => self.on_turn_completed(params),
            "item/started" => self.on_item_started(params),
            "item/completed" => self.on_item_completed(params),
            "item/agentMessage/delta" => self.on_text_delta(params),
            "item/reasoning/summaryTextDelta" => self.on_reasoning_delta(params, true),
            "item/reasoning/textDelta" => self.on_reasoning_delta(params, false),
            "item/reasoning/summaryPartAdded" => self.on_reasoning_part(params),
            "item/commandExecution/outputDelta" => self.on_output_delta(params),
            "item/fileChange/patchUpdated" => self.on_patch_updated(params),
            "item/mcpToolCall/progress" => self.on_mcp_progress(params),
            "thread/status/changed" => self.on_status_changed(params),
            "thread/tokenUsage/updated" => self.on_token_usage(params),
            "thread/started" => self.on_thread_started(params),
            "thread/closed" => self.on_thread_closed(params),
            "thread/settings/updated" => self.on_settings_updated(params),
            "thread/compacted" => vec![self.context_maintenance()],
            "model/rerouted" => self.on_model_rerouted(params),
            "subAgentActivity" => self.on_sub_agent_activity(params),
            "serverRequest/resolved" => self.on_request_resolved(params),
            "account/rateLimits/updated" => vec![Translated::RateLimits(
                params.get("rateLimits").cloned().unwrap_or(Value::Null),
            )],
            "mcpServer/startupStatus/updated" => self.on_mcp_status(params),
            "error" => self.on_error(params),
            "warning" => self.on_warning(params),
            "configWarning" => vec![self.optional("codex_config_warning")],
            "deprecationNotice" => vec![self.optional("codex_deprecation")],
            "account/updated"
            | "turn/moderationMetadata"
            | "remoteControl/status/changed"
            | "thread/readState/changed"
            | "turn/diff/updated"
            | "turn/plan/updated"
            | "skills/changed"
            | "fs/changed"
            | "app/list/updated" => vec![Translated::Ignored],
            _ if method.starts_with("hook/") => vec![Translated::Ignored],
            _ if method.ends_with("/requestApproval") => self.on_approval_request(params),
            _ if method == "item/tool/requestUserInput" => vec![Translated::Ignored],
            _ => self.on_unknown(method),
        }
    }

    fn optional(&self, kind: &'static str) -> Translated {
        Translated::Event(AdapterEvent::Diagnostic {
            kind,
            event_type: kind.into(),
            required: false,
        })
    }

    fn on_unknown(&mut self, method: &str) -> Vec<Translated> {
        if self.unknown_methods.insert(method.to_owned()) {
            vec![
                Translated::Event(AdapterEvent::Diagnostic {
                    kind: "unknown_notification",
                    event_type: method.to_owned(),
                    required: false,
                }),
                Translated::Ignored,
            ]
        } else {
            vec![Translated::Ignored]
        }
    }

    fn thread_of(params: &Value) -> Option<&str> {
        params.get("threadId").and_then(Value::as_str)
    }

    fn is_main(&self, params: &Value) -> bool {
        Self::thread_of(params) == Some(self.main_thread_id.as_str())
    }

    fn on_turn_started(&mut self, params: &Value) -> Vec<Translated> {
        let turn_id = params
            .get("turn")
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !self.is_main(params) {
            return vec![Translated::Ignored];
        }
        self.phase = Phase::Generating;
        vec![
            Translated::Event(AdapterEvent::Phase {
                phase: Phase::Generating,
                tool_name: None,
                summary: None,
            }),
            Translated::TurnStarted {
                turn_id: turn_id.to_owned(),
            },
        ]
    }

    fn on_turn_completed(&mut self, params: &Value) -> Vec<Translated> {
        if !self.is_main(params) {
            return vec![Translated::Ignored];
        }
        let turn = params.get("turn").unwrap_or(&Value::Null);
        let turn_id = turn
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let active = self
            .active_turn
            .as_ref()
            .is_some_and(|(id, _)| *id == turn_id);
        if !active {
            return vec![Translated::Ignored];
        }
        // Поздние завершения старого хода не должны переоткрывать блоки.
        let active_command = self.active_turn.as_ref().map(|(_, id)| *id);
        self.open
            .retain(|_, block| block.command_id != active_command);
        let status = parse_turn_status(turn.get("status").and_then(Value::as_str).unwrap_or(""));
        let error = turn
            .get("error")
            .filter(|e| !e.is_null())
            .map(parse_turn_error);
        let context_exhausted = error
            .as_ref()
            .is_some_and(|e| e.kind == "contextWindowExceeded");
        let mut out = vec![Translated::TurnCompleted {
            turn_id,
            status,
            error,
        }];
        if context_exhausted {
            out.push(Translated::Event(AdapterEvent::ContextExhausted));
        }
        out
    }

    fn on_item_started(&mut self, params: &Value) -> Vec<Translated> {
        let item = params.get("item").unwrap_or(&Value::Null);
        let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let thread_id = Self::thread_of(params).unwrap_or_default();
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if thread_id == self.main_thread_id {
            let known_turn = self
                .active_turn
                .as_ref()
                .is_some_and(|(id, _)| id == turn_id);
            let known_item = self.open.contains_key(item_id);
            if !known_turn && !known_item {
                return vec![Translated::Ignored];
            }
            if item_type == "userMessage" {
                if known_item || !known_turn {
                    return vec![Translated::Ignored];
                }
                return vec![Translated::UserMessage {
                    thread_id: thread_id.to_owned(),
                    turn_id: turn_id.to_owned(),
                    client_id: item
                        .get("clientId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }];
            }
            return self.start_main_item(params, item, item_id, item_type);
        }
        if item_type == "userMessage" {
            return vec![Translated::Ignored];
        }
        if let Some((agent, model, parent)) = self
            .children
            .get(thread_id)
            .map(|child| (child.agent.clone(), child.model.clone(), child.parent))
        {
            // Детские ходы главный не завершают; их блоки без привязки к команде.
            return self.start_item(item, item_id, item_type, &agent, model, parent, None);
        }
        vec![Translated::Ignored]
    }

    fn start_main_item(
        &mut self,
        params: &Value,
        item: &Value,
        item_id: &str,
        item_type: &str,
    ) -> Vec<Translated> {
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let command_id = self
            .active_turn
            .as_ref()
            .filter(|(id, _)| *id == turn_id)
            .map(|(_, id)| *id);
        self.start_item(
            item,
            item_id,
            item_type,
            "orchestrator",
            self.model.clone(),
            None,
            command_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_item(
        &mut self,
        item: &Value,
        item_id: &str,
        item_type: &str,
        agent: &str,
        model: String,
        parent: Option<Uuid>,
        command_id: Option<Uuid>,
    ) -> Vec<Translated> {
        // Пустой reasoning ничего не даёт: блок откроется на первой дельте.
        if item_type == "reasoning" {
            self.open.insert(
                item_id.to_owned(),
                OpenBlock {
                    block_id: Uuid::now_v7(),
                    agent: agent.to_owned(),
                    model,
                    parent,
                    command_id,
                    item_type: item_type.to_owned(),
                    text: String::new(),
                    result_block: None,
                    result_text: String::new(),
                    reasoning: Some(ReasoningState {
                        parts: Vec::new(),
                        has_summary: false,
                        opened: false,
                    }),
                    pending_diff: String::new(),
                },
            );
            return vec![Translated::Ignored];
        }
        if item_type == "contextCompaction" {
            return vec![self.context_maintenance()];
        }
        let block_id = Uuid::now_v7();
        let mut out = Vec::new();
        match item_type {
            "agentMessage" => {
                out.push(self.started(
                    block_id,
                    BlockKind::Text,
                    agent,
                    &model,
                    parent,
                    command_id,
                    None,
                    None,
                ));
            }
            "commandExecution" => {
                let summary = command_summary(item);
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some("shell"),
                    Some(&summary),
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some("shell".into()),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
            }
            "fileChange" => {
                let summary = file_summary(item, self.workdir.as_deref());
                let diff = file_diff(item);
                self.open.insert(
                    item_id.to_owned(),
                    OpenBlock {
                        block_id,
                        agent: agent.to_owned(),
                        model,
                        parent,
                        command_id,
                        item_type: item_type.to_owned(),
                        text: String::new(),
                        result_block: None,
                        result_text: String::new(),
                        reasoning: None,
                        pending_diff: diff,
                    },
                );
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &self.open[item_id].model,
                    parent,
                    command_id,
                    Some("apply_patch"),
                    Some(&summary),
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some("apply_patch".into()),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
                return out;
            }
            "mcpToolCall" => {
                let (name, summary) = mcp_summary(item);
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some(&name),
                    Some(&summary),
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some(name),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
            }
            "collabAgentToolCall" => {
                let (name, summary, model_override) = collab_summary(item);
                let model = model_override.unwrap_or(model);
                if item.get("tool").and_then(Value::as_str) == Some("spawnAgent") {
                    for child_id in item
                        .get("receiverThreadIds")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        self.spawn_parents.insert(child_id.to_owned(), block_id);
                    }
                }
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some(&name),
                    if summary.is_empty() {
                        None
                    } else {
                        Some(&summary)
                    },
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some(name),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
                self.open.insert(
                    item_id.to_owned(),
                    OpenBlock {
                        block_id,
                        agent: agent.to_owned(),
                        model,
                        parent,
                        command_id,
                        item_type: item_type.to_owned(),
                        text: String::new(),
                        result_block: None,
                        result_text: String::new(),
                        reasoning: None,
                        pending_diff: String::new(),
                    },
                );
                return out;
            }
            "webSearch" => {
                let summary = item
                    .get("query")
                    .and_then(Value::as_str)
                    .map(cut120)
                    .unwrap_or_default();
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some("web_search"),
                    if summary.is_empty() {
                        None
                    } else {
                        Some(&summary)
                    },
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some("web_search".into()),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
            }
            "imageView" => {
                let path = item.get("path").and_then(Value::as_str).unwrap_or_default();
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some("view_image"),
                    Some(&cut120(file_name(path))),
                ));
                // Сопровождение читает файл сразу; относительный путь — от workdir.
                out.push(Translated::Event(AdapterEvent::Media {
                    block_id,
                    source: MediaSource::Path(PathBuf::from(path)),
                }));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some("view_image".into()),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
                self.open.insert(
                    item_id.to_owned(),
                    OpenBlock {
                        block_id,
                        agent: agent.to_owned(),
                        model,
                        parent,
                        command_id,
                        item_type: item_type.to_owned(),
                        text: String::new(),
                        result_block: None,
                        result_text: String::new(),
                        reasoning: None,
                        pending_diff: String::new(),
                    },
                );
                return out;
            }
            _ => {
                out.push(self.started(
                    block_id,
                    BlockKind::ToolCall,
                    agent,
                    &model,
                    parent,
                    command_id,
                    Some(item_type),
                    None,
                ));
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::RunningTool,
                    tool_name: Some(item_type.to_owned()),
                    summary: None,
                }));
                self.phase = Phase::RunningTool;
            }
        }
        self.open.insert(
            item_id.to_owned(),
            OpenBlock {
                block_id,
                agent: agent.to_owned(),
                model,
                parent,
                command_id,
                item_type: item_type.to_owned(),
                text: String::new(),
                result_block: None,
                result_text: String::new(),
                reasoning: None,
                pending_diff: String::new(),
            },
        );
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn started(
        &self,
        block_id: Uuid,
        kind: BlockKind,
        agent: &str,
        model: &str,
        parent: Option<Uuid>,
        command_id: Option<Uuid>,
        tool_name: Option<&str>,
        summary: Option<&str>,
    ) -> Translated {
        Translated::Event(AdapterEvent::BlockStarted {
            block_id,
            kind,
            input_command_id: command_id,
            parent,
            agent: agent.to_owned(),
            model: model.to_owned(),
            tool_name: tool_name.map(str::to_owned),
            summary: summary.map(str::to_owned),
            approved_by: None,
        })
    }

    fn context_maintenance(&mut self) -> Translated {
        self.phase = Phase::ContextMaintenance;
        Translated::Event(AdapterEvent::Phase {
            phase: Phase::ContextMaintenance,
            tool_name: None,
            summary: None,
        })
    }

    fn on_item_completed(&mut self, params: &Value) -> Vec<Translated> {
        let item = params.get("item").unwrap_or(&Value::Null);
        let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let Some(mut block) = self.open.remove(item_id) else {
            return vec![Translated::Ignored];
        };
        let mut out = Vec::new();
        // Отложенный reasoning: непустой финал открывает и закрывает блок парой.
        if let Some(reasoning) = block.reasoning.take() {
            let final_text = reasoning_final(item, &reasoning);
            if final_text.is_empty() {
                return vec![Translated::Ignored];
            }
            let mut out = Vec::new();
            if !reasoning.opened {
                out.push(self.started(
                    block.block_id,
                    BlockKind::Reasoning,
                    &block.agent,
                    &block.model,
                    block.parent,
                    block.command_id,
                    None,
                    None,
                ));
            }
            out.push(Translated::Event(AdapterEvent::BlockFinal {
                block_id: block.block_id,
                text: final_text,
                summary: None,
            }));
            return out;
        }
        match item_type {
            "agentMessage" => {
                let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: text.to_owned(),
                    summary: None,
                }));
            }
            "commandExecution" => {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let exit = item.get("exitCode").and_then(Value::as_u64);
                let summary = format!("{}, {}", command_summary(item), call_status(status, exit));
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let output = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let result_block = block.result_block.unwrap_or_else(Uuid::now_v7);
                if block.result_block.is_none() {
                    out.push(self.started(
                        result_block,
                        BlockKind::ToolResult,
                        &block.agent,
                        &block.model,
                        Some(block.block_id),
                        block.command_id,
                        None,
                        None,
                    ));
                }
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: result_block,
                    text: output.to_owned(),
                    summary: None,
                }));
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: command.to_owned(),
                    summary: Some(summary),
                }));
                out.push(self.item_completed(item));
                self.phase = Phase::Generating;
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
            "fileChange" => {
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let summary = file_summary(item, self.workdir.as_deref());
                let diff = file_diff(item);
                if !diff.is_empty() {
                    block.pending_diff = diff;
                }
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: block.pending_diff,
                    summary: Some(format!("{}, {}", summary, status)),
                }));
                out.push(self.item_completed(item));
                self.phase = Phase::Generating;
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
            "mcpToolCall" => {
                let text = mcp_result_text(item);
                let result_block = block.result_block.unwrap_or_else(Uuid::now_v7);
                if block.result_block.is_none() {
                    out.push(self.started(
                        result_block,
                        BlockKind::ToolResult,
                        &block.agent,
                        &block.model,
                        Some(block.block_id),
                        block.command_id,
                        None,
                        None,
                    ));
                }
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: result_block,
                    text,
                    summary: None,
                }));
                let arguments = item.get("arguments").cloned().unwrap_or(Value::Null);
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: serde_json::to_string(&arguments).unwrap_or_default(),
                    summary: None,
                }));
                out.push(self.item_completed(item));
                self.phase = Phase::Generating;
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
            "collabAgentToolCall" | "webSearch" | "imageView" => {
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: String::new(),
                    summary: None,
                }));
                if matches!(item_type, "collabAgentToolCall") {
                    out.push(self.item_completed(item));
                }
                self.phase = Phase::Generating;
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
            _ => {
                out.push(Translated::Event(AdapterEvent::BlockFinal {
                    block_id: block.block_id,
                    text: String::new(),
                    summary: None,
                }));
                self.phase = Phase::Generating;
                out.push(Translated::Event(AdapterEvent::Phase {
                    phase: Phase::Generating,
                    tool_name: None,
                    summary: None,
                }));
            }
        }
        out
    }

    fn item_completed(&self, item: &Value) -> Translated {
        Translated::ItemCompleted {
            item_id: item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            item_type: item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: item
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            // Элемент, по которому был запрос разрешения, CLI сам не одобрял.
            approved_by_cli: !self
                .approval_items
                .contains(item.get("id").and_then(Value::as_str).unwrap_or_default()),
        }
    }

    fn on_text_delta(&mut self, params: &Value) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(block) = self.open.get_mut(item_id) else {
            return vec![Translated::Ignored];
        };
        if block.reasoning.is_some() || block.item_type != "agentMessage" {
            return vec![Translated::Ignored];
        }
        block.text.push_str(delta);
        vec![Translated::Event(AdapterEvent::BlockDelta {
            block_id: block.block_id,
            append: delta.to_owned(),
        })]
    }

    fn on_reasoning_delta(&mut self, params: &Value, summary: bool) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if delta.is_empty() {
            return vec![Translated::Ignored];
        }
        let Some(block) = self.open.get_mut(item_id) else {
            return vec![Translated::Ignored];
        };
        if block.reasoning.is_none() {
            return vec![Translated::Ignored];
        }
        // Сырой текст идёт только пока сводки не было.
        if !summary && block.reasoning.as_ref().is_some_and(|r| r.has_summary) {
            return vec![Translated::Ignored];
        }
        let already_open = block.reasoning.as_ref().is_some_and(|r| r.opened);
        {
            let reasoning = block.reasoning.as_mut().unwrap();
            if summary {
                reasoning.has_summary = true;
            }
            if reasoning.parts.is_empty() {
                reasoning.parts.push(String::new());
            }
            reasoning.parts.last_mut().unwrap().push_str(delta);
            reasoning.opened = true;
        }
        let block_id = block.block_id;
        let mut out = Vec::new();
        if !already_open {
            let (agent, model, parent, command_id) = (
                block.agent.clone(),
                block.model.clone(),
                block.parent,
                block.command_id,
            );
            out.push(Translated::Event(AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Reasoning,
                input_command_id: command_id,
                parent,
                agent,
                model,
                tool_name: None,
                summary: None,
                approved_by: None,
            }));
        }
        out.push(Translated::Event(AdapterEvent::BlockDelta {
            block_id,
            append: delta.to_owned(),
        }));
        out
    }

    fn on_reasoning_part(&mut self, params: &Value) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(block) = self.open.get_mut(item_id) else {
            return vec![Translated::Ignored];
        };
        let Some(reasoning) = block.reasoning.as_mut() else {
            return vec![Translated::Ignored];
        };
        let has_content = reasoning.parts.iter().any(|p| !p.is_empty());
        if !has_content {
            if reasoning.parts.is_empty() {
                reasoning.parts.push(String::new());
            }
            return vec![Translated::Ignored];
        }
        reasoning.parts.push(String::new());
        vec![Translated::Event(AdapterEvent::BlockDelta {
            block_id: block.block_id,
            append: "\n\n".into(),
        })]
    }

    fn on_output_delta(&mut self, params: &Value) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(block) = self.open.get(item_id) else {
            return vec![Translated::Ignored];
        };
        if block.item_type != "commandExecution" {
            return vec![Translated::Ignored];
        }
        let mut out = Vec::new();
        if block.result_block.is_none() {
            let result_block = Uuid::now_v7();
            out.push(self.started(
                result_block,
                BlockKind::ToolResult,
                &block.agent,
                &block.model,
                Some(block.block_id),
                block.command_id,
                None,
                None,
            ));
            let block = self.open.get_mut(item_id).unwrap();
            block.result_block = Some(result_block);
        }
        let block = self.open.get_mut(item_id).unwrap();
        block.result_text.push_str(delta);
        out.push(Translated::Event(AdapterEvent::BlockDelta {
            block_id: block.result_block.unwrap(),
            append: delta.to_owned(),
        }));
        out
    }

    fn on_patch_updated(&mut self, params: &Value) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(block) = self.open.get_mut(item_id) else {
            return vec![Translated::Ignored];
        };
        if block.item_type != "fileChange" {
            return vec![Translated::Ignored];
        }
        if let Some(diff) = params.get("diff").and_then(Value::as_str) {
            block.pending_diff = diff.to_owned();
        } else if let Some(changes) = params.get("changes") {
            let diff = extract_diffs(changes);
            if !diff.is_empty() {
                block.pending_diff = diff;
            }
        }
        vec![Translated::Ignored]
    }

    fn on_mcp_progress(&mut self, params: &Value) -> Vec<Translated> {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !self.open.contains_key(item_id) {
            return vec![Translated::Ignored];
        }
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        vec![Translated::Event(AdapterEvent::Phase {
            phase: Phase::RunningTool,
            tool_name: None,
            summary: if message.is_empty() {
                None
            } else {
                Some(message.to_owned())
            },
        })]
    }

    fn on_status_changed(&mut self, params: &Value) -> Vec<Translated> {
        if !self.is_main(params) {
            return vec![Translated::Ignored];
        }
        let status = params.get("status").unwrap_or(&Value::Null);
        if status.get("type").and_then(Value::as_str) == Some("systemError") {
            return vec![Translated::Event(AdapterEvent::Diagnostic {
                kind: "codex_system_error",
                event_type: "codex_system_error".into(),
                required: false,
            })];
        }
        let flags: Vec<&str> = status
            .get("activeFlags")
            .and_then(Value::as_array)
            .map(|flags| flags.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if flags.contains(&"waitingOnApproval") {
            self.previous_phase = self.phase.clone();
            self.phase = Phase::AwaitingPermission;
            return vec![Translated::Event(AdapterEvent::Phase {
                phase: Phase::AwaitingPermission,
                tool_name: None,
                summary: None,
            })];
        }
        if flags.contains(&"waitingOnUserInput") {
            self.previous_phase = self.phase.clone();
            self.phase = Phase::AwaitingAnswer;
            return vec![Translated::Event(AdapterEvent::Phase {
                phase: Phase::AwaitingAnswer,
                tool_name: None,
                summary: None,
            })];
        }
        if matches!(
            self.phase,
            Phase::AwaitingPermission | Phase::AwaitingAnswer
        ) {
            self.phase = self.previous_phase.clone();
            return vec![Translated::Event(AdapterEvent::Phase {
                phase: self.phase.clone(),
                tool_name: None,
                summary: None,
            })];
        }
        vec![Translated::Ignored]
    }

    fn on_token_usage(&mut self, params: &Value) -> Vec<Translated> {
        let thread_id = Self::thread_of(params).unwrap_or_default();
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
        let last = usage.get("last").unwrap_or(&Value::Null);
        let get = |key: &str| last.get(key).and_then(Value::as_u64).unwrap_or(0);
        let input = get("inputTokens").saturating_sub(get("cachedInputTokens"));
        if thread_id == self.main_thread_id {
            if self
                .active_turn
                .as_ref()
                .is_some_and(|(id, _)| id == turn_id)
            {
                let key = ("orchestrator".to_owned(), self.model.clone());
                let acc = self.usage.entry(key.clone()).or_insert(UsageAcc {
                    agent: key.0,
                    model: key.1,
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                });
                acc.input += input;
                acc.output += get("outputTokens");
                acc.cache_read += get("cachedInputTokens");
                acc.cache_write += get("cacheWriteInputTokens");
                self.last_context_tokens =
                    last.get("totalTokens").and_then(Value::as_u64).unwrap_or(0);
                self.last_context_window = usage
                    .get("modelContextWindow")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            return vec![Translated::Ignored];
        }
        // Расход ребёнка засчитывается, пока жив главный ход.
        if self.active_turn.is_some()
            && let Some(child) = self.children.get(thread_id)
        {
            let key = (child.agent.clone(), child.model.clone());
            let acc = self.usage.entry(key.clone()).or_insert(UsageAcc {
                agent: key.0,
                model: key.1,
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
            });
            acc.input += input;
            acc.output += get("outputTokens");
            acc.cache_read += get("cachedInputTokens");
            acc.cache_write += get("cacheWriteInputTokens");
        }
        vec![Translated::Ignored]
    }

    fn on_thread_started(&mut self, params: &Value) -> Vec<Translated> {
        let thread = params.get("thread").unwrap_or(&Value::Null);
        let id = thread.get("id").and_then(Value::as_str).unwrap_or_default();
        let parent = thread.get("parentThreadId").and_then(Value::as_str);
        match parent {
            // Главный поток уже известен из конструктора.
            None if id == self.main_thread_id => vec![Translated::Ignored],
            None => vec![Translated::Ignored],
            Some(_) => {
                let agent = thread
                    .get("agentRole")
                    .and_then(Value::as_str)
                    .or_else(|| thread.get("agentNickname").and_then(Value::as_str))
                    .unwrap_or("subagent")
                    .to_owned();
                let model = thread
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.model)
                    .to_owned();
                let parent_block = self.spawn_parents.get(id).copied();
                self.children.insert(
                    id.to_owned(),
                    ChildInfo {
                        agent,
                        model,
                        parent: parent_block,
                    },
                );
                vec![Translated::Ignored]
            }
        }
    }

    fn on_thread_closed(&mut self, params: &Value) -> Vec<Translated> {
        if !self.is_main(params) {
            return vec![Translated::Ignored];
        }
        vec![Translated::Event(AdapterEvent::Diagnostic {
            kind: "thread_closed",
            event_type: "thread_closed".into(),
            required: true,
        })]
    }

    fn on_settings_updated(&mut self, params: &Value) -> Vec<Translated> {
        let settings = params.get("threadSettings").cloned().unwrap_or(Value::Null);
        if let Some(model) = settings.get("model").and_then(Value::as_str) {
            self.model = model.to_owned();
        }
        vec![Translated::SettingsUpdated(settings)]
    }

    fn on_model_rerouted(&mut self, params: &Value) -> Vec<Translated> {
        if let Some(model) = params
            .get("toModel")
            .and_then(Value::as_str)
            .or_else(|| params.get("model").and_then(Value::as_str))
        {
            self.model = model.to_owned();
        }
        vec![Translated::Ignored]
    }

    fn on_sub_agent_activity(&mut self, params: &Value) -> Vec<Translated> {
        let name = params
            .get("agentName")
            .and_then(Value::as_str)
            .unwrap_or("subagent");
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        vec![Translated::Event(AdapterEvent::Phase {
            phase: Phase::Generating,
            tool_name: None,
            summary: Some(format!("Субагент {name}: {kind}")),
        })]
    }

    fn on_request_resolved(&mut self, params: &Value) -> Vec<Translated> {
        let request_id = params.get("requestId").cloned().unwrap_or(Value::Null);
        let id = match serde_json::from_value::<RpcId>(request_id) {
            Ok(id) => id,
            Err(_) => return vec![Translated::Ignored],
        };
        vec![Translated::ServerRequestResolved { request_id: id }]
    }

    fn on_mcp_status(&mut self, params: &Value) -> Vec<Translated> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| params.get("serverName").and_then(Value::as_str))
            .unwrap_or_default();
        let status = params
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        vec![Translated::McpStatus {
            name: name.to_owned(),
            status: status.to_owned(),
        }]
    }

    fn on_error(&mut self, params: &Value) -> Vec<Translated> {
        let will_retry = params
            .get("willRetry")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !will_retry {
            // Запомнили; supersede придёт с turn/completed.turn.error.
            return vec![Translated::Ignored];
        }
        self.retry_count += 1;
        let n = self.retry_count;
        self.phase = Phase::Generating;
        vec![Translated::Event(AdapterEvent::Phase {
            phase: Phase::Generating,
            tool_name: None,
            summary: Some(format!("повтор запроса к модели ({n})")),
        })]
    }

    fn on_warning(&mut self, params: &Value) -> Vec<Translated> {
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message.starts_with("Under-development features enabled:") {
            return vec![Translated::Ignored];
        }
        if self.active_turn.is_none() {
            return vec![Translated::Event(AdapterEvent::Diagnostic {
                kind: "codex_warning",
                event_type: "codex_warning".into(),
                required: false,
            })];
        }
        let command_id = self.active_turn.as_ref().map(|(_, id)| *id);
        let block_id = Uuid::now_v7();
        vec![
            Translated::Event(AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::Warning,
                input_command_id: command_id,
                parent: None,
                agent: "orchestrator".into(),
                model: self.model.clone(),
                tool_name: None,
                summary: None,
                approved_by: None,
            }),
            Translated::Event(AdapterEvent::BlockFinal {
                block_id,
                text: message.to_owned(),
                summary: None,
            }),
        ]
    }

    fn on_approval_request(&mut self, params: &Value) -> Vec<Translated> {
        // Задача 4 разбирает запрос сама; здесь только метка для ItemCompleted.
        if let Some(item_id) = params.get("itemId").and_then(Value::as_str) {
            self.approval_items.insert(item_id.to_owned());
        }
        vec![Translated::Ignored]
    }
}

fn command_summary(item: &Value) -> String {
    let actions = item
        .get("commandActions")
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(|a| a.get("command").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    if actions.is_empty() {
        cut120(
            item.get("command")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    } else {
        cut120(&actions)
    }
}

fn call_status(status: &str, exit: Option<u64>) -> String {
    match (status, exit) {
        ("completed", Some(code)) => format!("exit {code}"),
        ("completed", None) => "completed".into(),
        ("failed", Some(code)) => format!("exit {code}"),
        ("failed", None) => "failed".into(),
        ("declined", _) => "declined".into(),
        (other, _) => other.to_owned(),
    }
}

fn relative_path<'a>(path: &'a str, workdir: Option<&Path>) -> &'a str {
    match workdir {
        Some(root) => path
            .strip_prefix(root.to_string_lossy().as_ref())
            .map(|rest| rest.strip_prefix('/').unwrap_or(rest))
            .unwrap_or(path),
        None => path,
    }
}

fn file_summary(item: &Value, workdir: Option<&Path>) -> String {
    let paths = item
        .get("changes")
        .and_then(Value::as_array)
        .map(|changes| {
            changes
                .iter()
                .filter_map(|c| c.get("path").and_then(Value::as_str))
                .map(|p| relative_path(p, workdir))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    cut120(&paths)
}

fn extract_diffs(changes: &Value) -> String {
    changes
        .as_array()
        .map(|changes| {
            changes
                .iter()
                .filter_map(|c| c.get("diff").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

fn file_diff(item: &Value) -> String {
    item.get("changes").map(extract_diffs).unwrap_or_default()
}

fn mcp_summary(item: &Value) -> (String, String) {
    let name = format!(
        "{}.{}",
        item.get("server")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        item.get("tool").and_then(Value::as_str).unwrap_or_default()
    );
    let summary = item
        .get("arguments")
        .and_then(Value::as_object)
        .and_then(|args| args.iter().filter_map(|(_, v)| v.as_str()).next())
        .map(cut120)
        .unwrap_or_default();
    (name, summary)
}

fn collab_summary(item: &Value) -> (String, String, Option<String>) {
    match item.get("tool").and_then(Value::as_str) {
        Some("spawnAgent") => (
            "spawn_agent".into(),
            item.get("prompt")
                .and_then(Value::as_str)
                .map(cut120)
                .unwrap_or_default(),
            item.get("model").and_then(Value::as_str).map(str::to_owned),
        ),
        Some(tool) => (tool.to_owned(), String::new(), None),
        None => ("collabAgentToolCall".into(), String::new(), None),
    }
}

fn mcp_result_text(item: &Value) -> String {
    if let Some(content) = item
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(Value::as_array)
    {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    item.get("error")
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn reasoning_final(item: &Value, reasoning: &ReasoningState) -> String {
    let stream = reasoning.parts.join("\n\n");
    if reasoning.has_summary || !stream.trim().is_empty() {
        if stream.is_empty() {
            summary_of(item)
        } else {
            stream
        }
    } else {
        content_of(item)
    }
}

fn summary_of(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

fn content_of(item: &Value) -> String {
    item.get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}
