//! JSON lines: вход `input {command_id, attempt, text}`, `interrupt {id}`,
//! `decision {request_id, decision}`; Codex сначала получает `initialize {capabilities}`.
//! Выход: `ready {native_conversation_id}`, `accepted {command_id}`,
//! `interrupted {id}`, `phase {phase}`, `block_started {block_id, kind,
//! input_command_id, parent}`, `block_delta {block_id, append}`,
//! `block_final {block_id, text}`, `completed {input_command_id}`.
//! Неизвестные кадры несут `type` и `required`; содержимое не диагностируется.
//! Скрипт: {"synthetic":[{"delta":"..."},{"delay_ms":1},{"tool":{"name":"..."}},
//! {"error":"..."},{"unknown":{"event_type":"...","required":false}},
//! {"exit":0},{"detach_child":true},{"hang":true}]}.
//! Запросы: {"request_permission":{"tool_name":"Bash","command":"cargo test","args":{}}},
//! {"ask_question":{"questions":[{"id":"q1","text":"Deploy?","options":["yes","no"]}]}}.
//! Эти шаги ждут решения без таймаута; ответы на вопросы привязаны к `id`.
//! {"echo_instructions":true} пишет блок с режимом и версией инструкций ввода;
//! {"agent_call":{"tool":"...","arguments":{...}}} отдаёт вызов инструмента агента
//! (кадр `agent_call`, затем блок с результатом из хаба). Текста пресета в кадрах
//! нет: только режим, версия, длина и SHA-256.
//! Шаги: {"model":"synthetic-small"}, {"subagent":{"name":"Explore",
//! "model":"synthetic-small","task":"…","steps":[…]}},
//! {"usage":{"model":"…","agent":"main","input":120,"output":40,
//! "cache_read":10,"cache_write":0}}, {"limit_observed":{"window":"weekly",
//! "used_permille":960,"resets_in_ms":3600000}}, {"limit_reached":{
//! "window":"five_hour","resets_in_ms":5000}}, {"context_exhausted":true},
//! {"read_attachment":0}, {"view_image":"<путь>"} или {"view_image":{"path":"…",
//! "inline":true}}: блок `Read` с путём в summary, кадр `media {block_id, path | data}`
//! (data - base64 байтов файла) и результат «<размер> bytes». Ввод также несёт files и settings.
//! Начальные delay/hang/exit исполняются до accepted; обычный текст: три delta.

use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::{BlockKind, InstructionSpec, LocalFile, ModelUsage, Phase, SettingsChange};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize)]
pub(super) struct InitializeCapabilities {
    #[serde(rename = "experimentalApi")]
    pub experimental_api: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Request {
    Initialize {
        capabilities: InitializeCapabilities,
    },
    Decision {
        request_id: String,
        decision: DecisionPayload,
    },
    Input {
        command_id: Uuid,
        attempt: u32,
        text: String,
        #[serde(default)]
        files: Vec<LocalFile>,
        #[serde(default)]
        settings: Option<SettingsChange>,
    },
    Interrupt {
        id: Uuid,
    },
}

fn log_input(line: &str) {
    #[cfg(not(feature = "fault-injection"))]
    let _ = line;
    #[cfg(feature = "fault-injection")]
    trace_input(line);
}

#[cfg(feature = "fault-injection")]
fn trace_input(line: &str) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("CANTERLOT_SYNTHETIC_STDIN_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let receipt = match value["type"].as_str() {
            Some("decision") => json!({"type":"decision", "request_id":value["request_id"]}),
            Some("initialize") => {
                json!({"type":"initialize", "capabilities":{"experimentalApi":value["capabilities"]["experimentalApi"]}})
            }
            _ => {
                json!({"type":value["type"], "command_id":value["command_id"], "attempt":value["attempt"]})
            }
        };
        let _ = writeln!(file, "{receipt}");
    }
}

pub(super) async fn emit(value: Value) -> io::Result<()> {
    let mut bytes =
        serde_json::to_vec(&value).map_err(|_| io::Error::other("frame encoding failed"))?;
    bytes.push(b'\n');
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&bytes).await?;
    stdout.flush().await
}

fn split(text: &str, max: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut result = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(max);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        result.push(&rest[..end]);
        rest = &rest[end..];
    }
    result
}

async fn phase(phase: Phase) -> io::Result<()> {
    emit(json!({"type":"phase", "phase":phase})).await
}

struct Lane {
    agent: String,
    model: String,
    parent: Option<Uuid>,
    text_id: Option<Uuid>,
    accumulated: String,
}
struct Turn {
    decisions: mpsc::UnboundedReceiver<(String, DecisionPayload)>,
    command: Uuid,
    files: Vec<LocalFile>,
    instructions: Option<InstructionSpec>,
    accepted: bool,
    usage: std::collections::BTreeMap<(String, String), ModelUsage>,
    output_chars: u64,
    exhausted: bool,
}
impl Turn {
    async fn request(&mut self, kind: &str, payload: &Value, lane: &Lane) -> io::Result<()> {
        let id = Uuid::now_v7().to_string();
        phase(if kind == "permission" {
            Phase::AwaitingPermission
        } else {
            Phase::AwaitingAnswer
        })
        .await?;
        emit(json!({"type":"request_opened", "request_id":id, "kind":kind, "payload":payload}))
            .await?;
        while let Some((request_id, decision)) = self.decisions.recv().await {
            let reason = if request_id != id {
                "request_closed"
            } else if !super::valid_decision(kind, payload, &decision) {
                "invalid_decision"
            } else {
                ""
            };
            if !reason.is_empty() {
                emit(json!({"type":"decision_not_applied", "request_id":request_id, "reason":reason})).await?;
                continue;
            }
            let proof_kind = if kind == "permission" {
                let allowed = matches!(
                    &decision,
                    DecisionPayload::Permission {
                        behavior: PermissionBehavior::Allow,
                        ..
                    }
                );
                self.block(
                    BlockKind::ToolResult,
                    if allowed { "ok" } else { "denied" },
                    lane,
                    payload["tool_name"].as_str(),
                    None,
                )
                .await?;
                "tool_result"
            } else {
                "server_request_resolved"
            };
            emit(json!({"type":"decision_applied", "request_id":id, "proof_kind":proof_kind, "payload":{"decision":decision}})).await?;
            phase(Phase::Generating).await?;
            return Ok(());
        }
        Err(io::Error::other("decision channel closed"))
    }
    async fn begin(
        &self,
        id: Uuid,
        kind: BlockKind,
        lane: &Lane,
        tool_name: Option<&str>,
        summary: Option<&str>,
    ) -> io::Result<()> {
        emit(json!({"type":"block_started","block_id":id,"kind":kind,"input_command_id":self.command,"parent":lane.parent,"agent":lane.agent,"model":lane.model,"tool_name":tool_name,"summary":summary})).await
    }
    async fn block(
        &mut self,
        kind: BlockKind,
        text: &str,
        lane: &Lane,
        tool_name: Option<&str>,
        summary: Option<&str>,
    ) -> io::Result<Uuid> {
        let id = Uuid::now_v7();
        self.begin(id, kind, lane, tool_name, summary).await?;
        delta(id, text).await?;
        finish(id, text, summary).await?;
        Ok(id)
    }
}
async fn delta(id: Uuid, text: &str) -> io::Result<()> {
    for chunk in split(text, canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES) {
        emit(json!({"type":"block_delta","block_id":id,"append":chunk})).await?;
    }
    Ok(())
}
async fn finish(id: Uuid, text: &str, summary: Option<&str>) -> io::Result<()> {
    emit(json!({"type":"block_final","block_id":id,"text":text,"summary":summary})).await
}
async fn close_text(lane: &mut Lane) -> io::Result<()> {
    if let Some(id) = lane.text_id.take() {
        finish(id, &std::mem::take(&mut lane.accumulated), None).await?;
    }
    Ok(())
}
async fn text_delta(turn: &mut Turn, lane: &mut Lane, text: &str) -> io::Result<()> {
    let id = if let Some(id) = lane.text_id {
        id
    } else {
        let id = Uuid::now_v7();
        turn.begin(id, BlockKind::Text, lane, None, None).await?;
        lane.text_id = Some(id);
        id
    };
    delta(id, text).await?;
    lane.accumulated.push_str(text);
    turn.output_chars = turn
        .output_chars
        .saturating_add(text.chars().count() as u64);
    Ok(())
}
async fn completed(command: Uuid, usage: Vec<ModelUsage>, context_tokens: u64) -> io::Result<()> {
    emit(json!({"type":"completed","input_command_id":command,"usage":usage,"context_tokens":context_tokens,"context_window_tokens":200000})).await
}
async fn detach() -> io::Result<()> {
    let mut launcher = tokio::process::Command::new(std::env::current_exe()?);
    launcher
        .args(["synthetic-cli", "--profile", "__detach_session"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = launcher.spawn()?;
    if !child.wait().await?.success() {
        return Err(io::Error::other("detach failed"));
    }
    Ok(())
}

fn run_steps<'a>(
    turn: &'a mut Turn,
    lane: &'a mut Lane,
    steps: Vec<Value>,
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<Option<i32>>> + Send + 'a>> {
    Box::pin(async move {
        if depth > 32 {
            return Err(io::Error::other("script nesting exceeded"));
        }
        for step in steps {
            if let Some(delay) = step["delay_ms"].as_u64() {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                continue;
            }
            if step["hang"].as_bool() == Some(true) {
                std::future::pending::<()>().await;
            }
            if let Some(code) = step["exit"].as_i64() {
                return Ok(Some(i32::try_from(code).unwrap_or(1)));
            }
            if !turn.accepted {
                emit(json!({"type":"accepted","command_id":turn.command})).await?;
                phase(Phase::Generating).await?;
                turn.accepted = true;
            }
            if let Some(payload) = step.get("request_permission") {
                turn.request("permission", payload, lane).await?;
            } else if let Some(payload) = step.get("ask_question") {
                turn.request("question", payload, lane).await?;
            } else if let Some(text) = step["delta"].as_str() {
                text_delta(turn, lane, text).await?;
            } else if step["echo_instructions"].as_bool() == Some(true) {
                let (mode, revision, bytes, digest) = match &turn.instructions {
                    Some(spec) => {
                        let mode = match spec.mode {
                            super::InstructionMode::Clean => "clean",
                            super::InstructionMode::Owner => "owner",
                            super::InstructionMode::Preset => "preset",
                        };
                        let digest: String = Sha256::digest(spec.text.as_bytes())
                            .iter()
                            .take(6)
                            .map(|b| format!("{b:02x}"))
                            .collect();
                        (mode, spec.revision, spec.text.len(), digest)
                    }
                    None => ("owner", 0, 0, hex_empty()),
                };
                text_delta(
                    turn,
                    lane,
                    &format!(
                        "instructions mode={mode} revision={revision} bytes={bytes} sha256={digest}"
                    ),
                )
                .await?;
            } else if let Some(call) = step.get("agent_call") {
                let tool = call["tool"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid agent_call step"))?;
                let arguments = call.get("arguments").cloned().unwrap_or(Value::Null);
                if !arguments.is_object() {
                    return Err(io::Error::other("invalid agent_call step"));
                }
                emit(json!({"type":"agent_call","tool":tool,"arguments":arguments})).await?;
            } else if let Some(model) = step["model"].as_str() {
                close_text(lane).await?;
                lane.model = model.into();
            } else if let Some(tool) = step.get("tool") {
                let name = tool["name"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid tool step"))?;
                let summary = tool["summary"].as_str();
                emit(json!({"type":"phase","phase":"running_tool","tool_name":name,"summary":summary})).await?;
                let call = turn
                    .block(BlockKind::ToolCall, name, lane, Some(name), summary)
                    .await?;
                let result_lane = Lane {
                    parent: Some(call),
                    agent: lane.agent.clone(),
                    model: lane.model.clone(),
                    text_id: None,
                    accumulated: String::new(),
                };
                turn.block(
                    BlockKind::ToolResult,
                    "ok",
                    &result_lane,
                    Some(name),
                    summary,
                )
                .await?;
                phase(Phase::Generating).await?;
            } else if let Some(sub) = step.get("subagent") {
                let name = sub["name"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid subagent"))?;
                let model = sub["model"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid subagent"))?;
                let task = sub["task"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid subagent"))?;
                let call = Uuid::now_v7();
                turn.begin(call, BlockKind::ToolCall, lane, Some("Agent"), Some(task))
                    .await?;
                let mut child = Lane {
                    agent: name.into(),
                    model: model.into(),
                    parent: Some(call),
                    text_id: None,
                    accumulated: String::new(),
                };
                let first = Uuid::now_v7();
                turn.begin(first, BlockKind::Text, &child, None, None)
                    .await?;
                child.text_id = Some(first);
                let steps = sub["steps"]
                    .as_array()
                    .cloned()
                    .ok_or_else(|| io::Error::other("invalid subagent"))?;
                if let Some(code) = run_steps(turn, &mut child, steps, depth + 1).await? {
                    return Ok(Some(code));
                }
                if turn.exhausted {
                    return Ok(None);
                }
                close_text(&mut child).await?;
                finish(call, "", Some(task)).await?;
                let result_lane = Lane {
                    parent: Some(call),
                    agent: lane.agent.clone(),
                    model: lane.model.clone(),
                    text_id: None,
                    accumulated: String::new(),
                };
                turn.block(
                    BlockKind::ToolResult,
                    "ok",
                    &result_lane,
                    Some("Agent"),
                    Some(task),
                )
                .await?;
            } else if let Some(usage) = step.get("usage") {
                let model = usage["model"].as_str().unwrap_or(&lane.model).to_owned();
                let agent = usage["agent"].as_str().unwrap_or(&lane.agent).to_owned();
                let row = turn
                    .usage
                    .entry((model.clone(), agent.clone()))
                    .or_insert(ModelUsage {
                        model,
                        agent,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                    });
                row.input_tokens = row
                    .input_tokens
                    .saturating_add(usage["input"].as_u64().unwrap_or(0));
                row.output_tokens = row
                    .output_tokens
                    .saturating_add(usage["output"].as_u64().unwrap_or(0));
                row.cache_read_tokens = row
                    .cache_read_tokens
                    .saturating_add(usage["cache_read"].as_u64().unwrap_or(0));
                row.cache_write_tokens = row
                    .cache_write_tokens
                    .saturating_add(usage["cache_write"].as_u64().unwrap_or(0));
            } else if let Some((kind, fact)) = step
                .get("limit_observed")
                .map(|f| ("limit_observed", f))
                .or_else(|| step.get("limit_reached").map(|f| ("limit_reached", f)))
            {
                if kind == "limit_reached" {
                    turn.block(BlockKind::Error, "usage limit reached", lane, None, None)
                        .await?;
                }
                let resets = fact["resets_in_ms"]
                    .as_u64()
                    .map(|ms| crate::machine::now_ms().saturating_add(ms));
                emit(json!({"type":kind,"window":fact["window"],"used_permille":fact["used_permille"],"resets_at_ms":resets})).await?;
            } else if step["context_exhausted"].as_bool() == Some(true) {
                emit(json!({"type":"context_exhausted"})).await?;
                turn.exhausted = true;
                return Ok(None);
            } else if let Some(index) = step["read_attachment"].as_u64() {
                let file = turn
                    .files
                    .get(index as usize)
                    .ok_or_else(|| io::Error::other("missing attachment"))?;
                let bytes = tokio::fs::read(&file.path).await?;
                let hash = format!(
                    "sha256:{}",
                    Sha256::digest(bytes)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                text_delta(turn, lane, &hash).await?;
            } else if let Some(view) = step.get("view_image") {
                let (raw, inline) = match view {
                    Value::String(path) => (path.clone(), false),
                    _ => (
                        view["path"]
                            .as_str()
                            .ok_or_else(|| io::Error::other("invalid view_image step"))?
                            .to_owned(),
                        view["inline"].as_bool().unwrap_or(false),
                    ),
                };
                let path = std::path::absolute(&raw)?;
                emit(
                    json!({"type":"phase","phase":"running_tool","tool_name":"Read","summary":raw}),
                )
                .await?;
                let call = turn
                    .block(BlockKind::ToolCall, &raw, lane, Some("Read"), Some(&raw))
                    .await?;
                let size = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
                if inline {
                    use base64::Engine;
                    let bytes = tokio::fs::read(&path).await?;
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    emit(json!({"type":"media","block_id":call,"data":data})).await?;
                } else {
                    emit(json!({"type":"media","block_id":call,"path":path})).await?;
                }
                let result_lane = Lane {
                    parent: Some(call),
                    agent: lane.agent.clone(),
                    model: lane.model.clone(),
                    text_id: None,
                    accumulated: String::new(),
                };
                let result =
                    size.map_or_else(|| "file not found".to_owned(), |n| format!("{n} bytes"));
                turn.block(
                    BlockKind::ToolResult,
                    &result,
                    &result_lane,
                    Some("Read"),
                    Some(&raw),
                )
                .await?;
                phase(Phase::Generating).await?;
            } else if let Some(error) = step["error"].as_str() {
                turn.block(BlockKind::Error, error, lane, None, None)
                    .await?;
            } else if let Some(unknown) = step.get("unknown") {
                let event_type = unknown["event_type"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid unknown step"))?;
                emit(json!({"type":event_type,"required":unknown["required"].as_bool().unwrap_or(false)})).await?;
            } else if step["detach_child"].as_bool() == Some(true) {
                detach().await?;
            } else {
                return Err(io::Error::other("invalid script step"));
            }
        }
        Ok(None)
    })
}
fn hex_empty() -> String {
    Sha256::digest(b"")
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn run_turn(
    command: Uuid,
    text: String,
    files: Vec<LocalFile>,
    instructions: Option<InstructionSpec>,
    model: &str,
    context: &mut u64,
    decisions: mpsc::UnboundedReceiver<(String, DecisionPayload)>,
) -> io::Result<Option<i32>> {
    let steps = if text.starts_with("{\"synthetic\":") {
        let value: Value =
            serde_json::from_str(&text).map_err(|_| io::Error::other("invalid script"))?;
        value["synthetic"]
            .as_array()
            .cloned()
            .ok_or_else(|| io::Error::other("invalid script"))?
    } else {
        let chars: Vec<_> = text.chars().collect();
        (0..3).map(|i|json!({"delta":chars[chars.len()*i/3..chars.len()*(i+1)/3].iter().collect::<String>()})).collect()
    };
    let mut turn = Turn {
        decisions,
        command,
        files,
        instructions,
        accepted: false,
        usage: Default::default(),
        output_chars: 0,
        exhausted: false,
    };
    let mut lane = Lane {
        agent: "main".into(),
        model: model.into(),
        parent: None,
        text_id: None,
        accumulated: String::new(),
    };
    if let Some(code) = run_steps(&mut turn, &mut lane, steps, 0).await? {
        return Ok(Some(code));
    }
    if turn.exhausted {
        return Ok(None);
    }
    if !turn.accepted {
        emit(json!({"type":"accepted","command_id":command})).await?;
    }
    close_text(&mut lane).await?;
    if turn.usage.is_empty() {
        turn.usage.insert(
            (lane.model.clone(), "main".into()),
            ModelUsage {
                model: lane.model,
                agent: "main".into(),
                input_tokens: text.chars().count() as u64 / 4,
                output_tokens: turn.output_chars / 4,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        );
    }
    let usage: Vec<_> = turn.usage.into_values().collect();
    for row in &usage {
        *context = context
            .saturating_add(row.input_tokens)
            .saturating_add(row.output_tokens)
            .saturating_add(row.cache_read_tokens)
            .saturating_add(row.cache_write_tokens);
    }
    completed(command, usage, *context).await?;
    Ok(None)
}

pub async fn run(
    profile: &str,
    native_id: Option<String>,
    model: Option<String>,
    enabled_features: &[String],
) -> io::Result<()> {
    #[cfg(feature = "fault-injection")]
    if profile == "__burn_parent" {
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args(["synthetic-cli", "--profile", "__burn"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        emit(json!({"child_pid":child.id()})).await?;
        child.wait().await?;
        return Ok(());
    }
    #[cfg(feature = "fault-injection")]
    if profile == "__burn" {
        loop {
            std::hint::spin_loop();
        }
    }
    if profile == "__detach_session" {
        rustix::process::setsid().map_err(io::Error::from)?;
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args(["synthetic-cli", "--profile", "__detach_fork"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        child.wait().await?;
        return Ok(());
    }
    if profile == "__detach_sleep" {
        tokio::time::sleep(Duration::from_secs(300)).await;
        return Ok(());
    }
    if profile == "__detach_fork" {
        // Два spawn вместо fork/pre_exec; последний потомок остаётся в новой сессии.
        tokio::process::Command::new(std::env::current_exe()?)
            .args(["synthetic-cli", "--profile", "__detach_sleep"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        return Ok(());
    }
    if !matches!(
        profile,
        "synthetic" | "synthetic-exit-on-interrupt" | "synthetic-codex"
    ) {
        return Err(io::Error::other("unknown synthetic profile"));
    }
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    if profile == "synthetic-codex" {
        if !enabled_features
            .iter()
            .any(|f| f == "default_mode_request_user_input")
        {
            return Err(io::Error::other("default mode questions are disabled"));
        }
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| io::Error::other("missing initialize"))?;
        log_input(&line);
        if !matches!(
            serde_json::from_str::<Request>(&line),
            Ok(Request::Initialize {
                capabilities: InitializeCapabilities {
                    experimental_api: true
                }
            })
        ) {
            return Err(io::Error::other("experimental API is disabled"));
        }
    }
    emit(json!({"type":"ready", "native_conversation_id":native_id.unwrap_or_else(|| format!("synthetic-{}", Uuid::now_v7()))})).await?;
    let mut model = model
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "synthetic-large".into());
    let mut context_tokens = 0;
    while let Some(line) = lines.next_line().await? {
        log_input(&line);
        let request: Request =
            serde_json::from_str(&line).map_err(|_| io::Error::other("invalid request"))?;
        match request {
            Request::Input {
                command_id,
                text,
                files,
                settings,
                ..
            } => {
                let instructions = settings
                    .as_ref()
                    .and_then(|change| change.instructions.clone());
                if let Some(change) = settings
                    && let Some(next) = change.model
                {
                    model = next;
                }
                let (decisions, receiver) = mpsc::unbounded_channel();
                let turn = run_turn(
                    command_id,
                    text,
                    files,
                    instructions,
                    &model,
                    &mut context_tokens,
                    receiver,
                );
                tokio::pin!(turn);
                // Готовность объявляется после выхода из select активного хода.
                loop {
                    tokio::select! {
                            result = &mut turn => {
                                if let Some(code) = result? { std::process::exit(code); }
                                phase(Phase::AwaitingInput).await?;
                                break;
                            }
                            line = lines.next_line() => {
                                let Some(line) = line? else { return Ok(()) };
                                log_input(&line);
                                match serde_json::from_str::<Request>(&line).map_err(|_| io::Error::other("invalid request"))? {
                                    Request::Decision { request_id, decision } => {
                                        if let Err(error) = decisions.send((request_id, decision)) {
                                            emit(json!({"type":"decision_not_applied", "request_id":error.0.0, "reason":"request_closed"})).await?;
                                        }
                                    }
                                    Request::Interrupt { id } => {
                                        emit(json!({"type":"interrupted", "id":id})).await?;
                                        completed(command_id,vec![],0).await?;
                                        phase(Phase::AwaitingInput).await?;
                                        if profile == "synthetic-exit-on-interrupt" { return Ok(()) }
                                        break;
                                    }
                                Request::Input { .. } | Request::Initialize { .. } => return Err(io::Error::other("turn already active")),
                                }
                            }
                    }
                }
            }
            Request::Decision { request_id, .. } => {
                emit(json!({"type":"decision_not_applied", "request_id":request_id, "reason":"request_closed"})).await?;
            }
            Request::Initialize { .. } => return Err(io::Error::other("already initialized")),
            Request::Interrupt { id } => {
                emit(json!({"type":"interrupted", "id":id})).await?;
                if profile == "synthetic-exit-on-interrupt" {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}
