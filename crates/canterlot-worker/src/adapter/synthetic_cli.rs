//! JSON lines: вход `input {command_id, attempt, text}`, `interrupt {id}`.
//! Выход: `ready {native_conversation_id}`, `accepted {command_id}`,
//! `interrupted {id}`, `phase {phase}`, `block_started {block_id, kind,
//! input_command_id, parent}`, `block_delta {block_id, append}`,
//! `block_final {block_id, text}`, `completed {input_command_id}`.
//! Неизвестные кадры несут `type` и `required`; содержимое не диагностируется.
//! Скрипт: {"synthetic":[{"delta":"..."},{"delay_ms":1},{"tool":{"name":"..."}},
//! {"error":"..."},{"unknown":{"event_type":"...","required":false}},
//! {"exit":0},{"detach_child":true},{"hang":true}]}.
//! Начальные delay/hang/exit исполняются до accepted; обычный текст: три delta.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

use super::{BlockKind, Phase};

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Request {
    Input {
        command_id: Uuid,
        attempt: u32,
        text: String,
    },
    Interrupt {
        id: Uuid,
    },
}

fn log_input(line: &str) {
    use std::io::Write;
    if let Some(path) = std::env::var_os("CANTERLOT_SYNTHETIC_STDIN_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{line}");
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

async fn begin(id: Uuid, kind: BlockKind, command: Uuid, parent: Option<Uuid>) -> io::Result<()> {
    emit(json!({"type":"block_started", "block_id":id, "kind":kind, "input_command_id":command, "parent":parent})).await
}

async fn delta(id: Uuid, text: &str) -> io::Result<()> {
    for chunk in split(text, canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES) {
        emit(json!({"type":"block_delta", "block_id":id, "append":chunk})).await?;
    }
    Ok(())
}

async fn finish(id: Uuid, text: &str) -> io::Result<()> {
    emit(json!({"type":"block_final", "block_id":id, "text":text})).await
}

async fn block(
    kind: BlockKind,
    command: Uuid,
    text: &str,
    parent: Option<Uuid>,
) -> io::Result<Uuid> {
    let id = Uuid::now_v7();
    begin(id, kind, command, parent).await?;
    delta(id, text).await?;
    finish(id, text).await?;
    Ok(id)
}

async fn completed(command: Uuid) -> io::Result<()> {
    emit(json!({"type":"completed", "input_command_id":command})).await
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

async fn run_turn(command: Uuid, text: String) -> io::Result<Option<i32>> {
    let steps = if text.starts_with("{\"synthetic\":") {
        let value: Value =
            serde_json::from_str(&text).map_err(|_| io::Error::other("invalid script"))?;
        value
            .get("synthetic")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| io::Error::other("invalid script"))?
    } else {
        let chars: Vec<_> = text.chars().collect();
        (0..3).map(|i| json!({"delta":chars[chars.len()*i/3..chars.len()*(i+1)/3].iter().collect::<String>()})).collect()
    };
    let mut accepted = false;
    let mut text_id = None;
    let mut accumulated = String::new();
    for step in steps {
        if let Some(delay) = step.get("delay_ms").and_then(Value::as_u64) {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            continue;
        }
        if step.get("hang").and_then(Value::as_bool) == Some(true) {
            std::future::pending::<()>().await;
        }
        if let Some(code) = step.get("exit").and_then(Value::as_i64) {
            return Ok(Some(i32::try_from(code).unwrap_or(1)));
        }
        if !accepted {
            emit(json!({"type":"accepted", "command_id":command})).await?;
            phase(Phase::Generating).await?;
            accepted = true;
        }
        if let Some(text) = step.get("delta").and_then(Value::as_str) {
            let id = if let Some(id) = text_id {
                id
            } else {
                let id = Uuid::now_v7();
                begin(id, BlockKind::Text, command, None).await?;
                text_id = Some(id);
                id
            };
            delta(id, text).await?;
            accumulated.push_str(text);
        } else if let Some(tool) = step.get("tool") {
            phase(Phase::RunningTool).await?;
            let name = tool
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| io::Error::other("invalid tool step"))?;
            let call = block(BlockKind::ToolCall, command, name, None).await?;
            block(BlockKind::ToolResult, command, "ok", Some(call)).await?;
            phase(Phase::Generating).await?;
        } else if let Some(error) = step.get("error").and_then(Value::as_str) {
            block(BlockKind::Error, command, error, None).await?;
        } else if let Some(unknown) = step.get("unknown") {
            let event_type = unknown
                .get("event_type")
                .and_then(Value::as_str)
                .ok_or_else(|| io::Error::other("invalid unknown step"))?;
            emit(json!({"type":event_type, "required":unknown.get("required").and_then(Value::as_bool).unwrap_or(false)})).await?;
        } else if step.get("detach_child").and_then(Value::as_bool) == Some(true) {
            detach().await?;
        } else {
            return Err(io::Error::other("invalid script step"));
        }
    }
    if !accepted {
        emit(json!({"type":"accepted", "command_id":command})).await?;
    }
    if let Some(id) = text_id {
        finish(id, &accumulated).await?;
    }
    completed(command).await?;
    Ok(None)
}

pub async fn run(profile: &str, native_id: Option<String>) -> io::Result<()> {
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
    if !matches!(profile, "synthetic" | "synthetic-exit-on-interrupt") {
        return Err(io::Error::other("unknown synthetic profile"));
    }
    emit(json!({"type":"ready", "native_conversation_id":native_id.unwrap_or_else(|| format!("synthetic-{}", Uuid::now_v7()))})).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        log_input(&line);
        let request: Request =
            serde_json::from_str(&line).map_err(|_| io::Error::other("invalid request"))?;
        match request {
            Request::Input {
                command_id, text, ..
            } => {
                let turn = run_turn(command_id, text);
                tokio::pin!(turn);
                // Готовность объявляется после выхода из select активного хода.
                tokio::select! {
                        result = &mut turn => {
                            if let Some(code) = result? { std::process::exit(code); }
                            phase(Phase::AwaitingInput).await?;
                        }
                        line = lines.next_line() => {
                            let Some(line) = line? else { return Ok(()) };
                            log_input(&line);
                            match serde_json::from_str::<Request>(&line).map_err(|_| io::Error::other("invalid request"))? {
                                Request::Interrupt { id } => {
                                    emit(json!({"type":"interrupted", "id":id})).await?;
                                    completed(command_id).await?;
                                    phase(Phase::AwaitingInput).await?;
                                    if profile == "synthetic-exit-on-interrupt" { return Ok(()) }
                                }
                                Request::Input { .. } => return Err(io::Error::other("turn already active")),
                            }
                        }
                }
            }
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
