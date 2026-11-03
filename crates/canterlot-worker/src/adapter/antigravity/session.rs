//! Рукопожатие ACP: `initialize`, новая сессия, сверка `session/list` и `session/resume`.

use super::process::Process;
use super::rpc::{Incoming, RpcClient, RpcError, RpcId};
use crate::adapter::InstructionSpec;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::time::{Instant, timeout_at};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session_id: String,
    pub models: Vec<ModelEntry>,
    pub current_model: Option<String>,
    /// Значения `thought_level`; в каталог не идут до контрактного теста по P14.
    pub efforts: Vec<String>,
    pub current_effort: Option<String>,
}

#[derive(Debug)]
pub enum SessionError {
    Rpc(RpcError),
    Eof,
    Timeout,
    Protocol,
    Io(std::io::Error),
}

impl From<std::io::Error> for SessionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "canterlot-worker", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn append(instructions: &InstructionSpec) -> &str {
    &instructions.text
}

pub fn new_params(cwd: &Path, instructions: &InstructionSpec) -> Value {
    json!({
        "cwd": cwd.to_string_lossy(),
        "mcpServers": [],
        "_meta": {"antigravity": {"systemPrompt": {"append": append(instructions)}}},
    })
}

pub fn resume_params(session_id: &str, instructions: &InstructionSpec) -> Value {
    json!({
        "sessionId": session_id,
        "_meta": {"antigravity": {"systemPrompt": {"append": append(instructions)}}},
    })
}

/// Один запрос: ответ по id, уведомления и чужие запросы пропускаются.
pub async fn call(
    proc: &mut Process,
    rpc: &mut RpcClient,
    method: &'static str,
    params: Value,
    deadline: Instant,
) -> Result<Value, SessionError> {
    let (id, message) = rpc.request_v2(method, params);
    proc.write_message(&message).await?;
    loop {
        let line = match timeout_at(deadline, proc.next_line()).await {
            Ok(super::process::StdoutLine::Line(line)) => line,
            Ok(super::process::StdoutLine::Oversized(_)) => {
                rpc.forget(id);
                return Err(SessionError::Protocol);
            }
            Ok(super::process::StdoutLine::Eof) => {
                rpc.forget(id);
                return Err(SessionError::Eof);
            }
            Err(_) => {
                rpc.forget(id);
                return Err(SessionError::Timeout);
            }
        };
        match super::rpc::classify(&line) {
            Incoming::Response {
                id: RpcId::Int(response_id),
                result,
            } if response_id == id => {
                rpc.forget(id);
                return result.map_err(SessionError::Rpc);
            }
            Incoming::Response { .. } => {}
            Incoming::Notification { .. } => {}
            Incoming::ServerRequest { id, .. } => {
                // До задач 3 запросов сервера в рукопожатии нет; отвечаем, чтобы не зависнуть.
                let message = RpcClient::error(&id, -32601, "Method not found");
                proc.write_message(&message).await?;
            }
            Incoming::Invalid => {
                rpc.forget(id);
                return Err(SessionError::Protocol);
            }
        }
    }
}

pub async fn initialize(
    proc: &mut Process,
    rpc: &mut RpcClient,
    timeout: Duration,
) -> Result<Value, SessionError> {
    call(
        proc,
        rpc,
        "initialize",
        initialize_params(),
        Instant::now() + timeout,
    )
    .await
}

pub async fn new_session(
    proc: &mut Process,
    rpc: &mut RpcClient,
    cwd: &Path,
    instructions: &InstructionSpec,
    timeout: Duration,
) -> Result<SessionInfo, SessionError> {
    let result = call(
        proc,
        rpc,
        "session/new",
        new_params(cwd, instructions),
        Instant::now() + timeout,
    )
    .await?;
    parse_session_result(&result).ok_or(SessionError::Protocol)
}

/// `session/list` перед resume: неизвестный id `session/load` принимает молча.
pub async fn contains_session(
    proc: &mut Process,
    rpc: &mut RpcClient,
    session_id: &str,
    timeout: Duration,
) -> Result<bool, SessionError> {
    let result = call(
        proc,
        rpc,
        "session/list",
        json!({}),
        Instant::now() + timeout,
    )
    .await?;
    Ok(result["sessions"].as_array().is_some_and(|sessions| {
        sessions
            .iter()
            .any(|session| session["sessionId"].as_str() == Some(session_id))
    }))
}

pub async fn resume(
    proc: &mut Process,
    rpc: &mut RpcClient,
    session_id: &str,
    instructions: &InstructionSpec,
    timeout: Duration,
) -> Result<SessionInfo, SessionError> {
    let result = call(
        proc,
        rpc,
        "session/resume",
        resume_params(session_id, instructions),
        Instant::now() + timeout,
    )
    .await?;
    parse_session_result(&result).ok_or(SessionError::Protocol)
}

pub async fn set_model(
    proc: &mut Process,
    rpc: &mut RpcClient,
    session_id: &str,
    model: &str,
    timeout: Duration,
) -> Result<(), SessionError> {
    call(
        proc,
        rpc,
        "session/set_model",
        json!({"sessionId": session_id, "modelId": model}),
        Instant::now() + timeout,
    )
    .await
    .map(|_| ())
}

pub async fn set_effort(
    proc: &mut Process,
    rpc: &mut RpcClient,
    session_id: &str,
    effort: &str,
    timeout: Duration,
) -> Result<(), SessionError> {
    call(
        proc,
        rpc,
        "session/set_config_option",
        json!({"sessionId": session_id, "configId": "thought_level", "value": effort}),
        Instant::now() + timeout,
    )
    .await
    .map(|_| ())
}

/// Поля `session/new` и `session/resume`; `configOptions.model` — семейство, не id модели.
pub fn parse_session_result(result: &Value) -> Option<SessionInfo> {
    let session_id = result["sessionId"].as_str()?.to_owned();
    let models = result["models"]["availableModels"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(ModelEntry {
                        id: entry["modelId"].as_str()?.to_owned(),
                        title: entry["name"].as_str().unwrap_or_default().to_owned(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_model = result["models"]["currentModelId"]
        .as_str()
        .map(str::to_owned);
    let thought_level = result["configOptions"].as_array().and_then(|options| {
        options
            .iter()
            .find(|option| option["id"].as_str() == Some("thought_level"))
    });
    let efforts = thought_level
        .and_then(|option| option["options"].as_array())
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option["value"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let current_effort = thought_level
        .and_then(|option| option["currentValue"].as_str())
        .map(str::to_owned);
    Some(SessionInfo {
        session_id,
        models,
        current_model,
        efforts,
        current_effort,
    })
}
