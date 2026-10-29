//! Supervisor side of the canterlot MCP socket: accepts relay connections,
//! answers JSON-RPC, and turns tool calls into broker events for the adapter.
//!
//! One pending call has no timeout; the supervisor answers it when the owner
//! decides. If the relay goes away (the CLI process ended), every open
//! request is closed as cancelled. Supervisor shutdown denies what is left.

use std::{collections::HashMap, os::unix::fs::DirBuilderExt as _, path::Path, sync::Arc};

use canterlot_proto::{
    requests::{DecisionPayload, PermissionBehavior},
    worker::v1::LimitsSnapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, unix::OwnedWriteHalf},
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

use super::mcp;
use crate::adapter::{DecisionOutcome, DecisionProof};

/// An open tool call waiting for the owner's decision.
struct PendingCall {
    rpc_id: Value,
    /// Permission: the original tool input. Question: normalized payload
    /// plus the original questions under "original".
    stored: Value,
}

fn is_question(stored: &Value) -> bool {
    stored.get("normalized").is_some()
}

/// Event for the adapter: a request opened, was cancelled, or asks for limits.
#[derive(Debug)]
pub enum BrokerEvent {
    Opened {
        request_id: String,
        kind: &'static str,
        payload: Value,
    },
    Cancelled {
        request_id: String,
    },
    LimitsQuery {
        rpc_id: Value,
    },
}

type SharedWriter = Arc<Mutex<Option<OwnedWriteHalf>>>;

async fn write_line(writer: &SharedWriter, value: &Value) -> std::io::Result<()> {
    let mut line = value.to_string();
    line.push('\n');
    let mut guard = writer.lock().await;
    let Some(stream) = guard.as_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "relay is gone",
        ));
    };
    stream.write_all(line.as_bytes()).await?;
    stream.flush().await
}

pub struct Broker {
    events: mpsc::UnboundedReceiver<BrokerEvent>,
    pending: Arc<Mutex<HashMap<String, PendingCall>>>,
    writer: SharedWriter,
    task: JoinHandle<()>,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Broker {
    /// Bind `<exec_dir>/mcp.sock` (socket 0600, dir 0700) and start accepting.
    pub async fn bind(exec_dir: &Path) -> std::io::Result<Self> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(exec_dir)?;
        let path = exec_dir.join("mcp.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<String, PendingCall>>> = Arc::default();
        let writer: SharedWriter = Arc::default();
        let task = {
            let pending = pending.clone();
            let writer = writer.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let (read, write) = stream.into_split();
                    *writer.lock().await = Some(write);
                    tokio::spawn(serve_conn(
                        read,
                        writer.clone(),
                        pending.clone(),
                        tx.clone(),
                    ));
                }
            })
        };
        Ok(Self {
            events: rx,
            pending,
            writer,
            task,
        })
    }

    pub async fn next(&mut self) -> BrokerEvent {
        loop {
            match self.events.recv().await {
                Some(event) => return event,
                None => std::future::pending::<()>().await,
            }
        }
    }

    /// Answer an open request with the owner's decision.
    pub async fn answer(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        let call = self.pending.lock().await.remove(request_id);
        let Some(call) = call else {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        };
        let text = match decision {
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                ..
            } if !is_question(&call.stored) => mcp::allow_text(&call.stored),
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message,
            } if !is_question(&call.stored) => mcp::deny_text(message.as_deref()),
            DecisionPayload::Question { answers } if is_question(&call.stored) => {
                let id_to_text: Vec<(String, String)> = call.stored["normalized"]["questions"]
                    .as_array()
                    .map(|questions| {
                        questions
                            .iter()
                            .filter_map(|question| {
                                Some((
                                    question.get("id")?.as_str()?.to_owned(),
                                    question.get("text")?.as_str()?.to_owned(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let original = call.stored.get("original").cloned().unwrap_or(Value::Null);
                mcp::question_text(&original, &id_to_text, answers)
            }
            _ => return DecisionOutcome::NotApplied("decision_mismatch".into()),
        };
        let reply = mcp::tool_call_reply(&call.rpc_id, &text_value(&text));
        if write_line(&self.writer, &reply).await.is_err() {
            self.pending
                .lock()
                .await
                .insert(request_id.to_owned(), call);
            return DecisionOutcome::Undetermined;
        }
        let behavior = match decision {
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                ..
            } => "allow",
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                ..
            } => "deny",
            DecisionPayload::Question { .. } => "allow",
        };
        DecisionOutcome::Applied(DecisionProof {
            proof_kind: "mcp_tool_result".into(),
            payload: json!({"request_id": request_id, "behavior": behavior}),
        })
    }

    /// Answer a `canterlot_limits` call from the latest observation.
    pub async fn answer_limits(&mut self, rpc_id: Value, snapshot: &LimitsSnapshot) {
        let text = limits_text(snapshot);
        let reply = mcp::tool_call_reply(&rpc_id, &Value::String(text));
        let _ = write_line(&self.writer, &reply).await;
    }

    /// Supervisor shutdown: deny everything still open, then close the socket.
    pub async fn shutdown(&mut self) {
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for (_request_id, call) in pending {
            let reply = mcp::tool_call_reply(
                &call.rpc_id,
                &json!({"behavior": "deny", "message": "canterlot supervisor stopped"}),
            );
            let _ = write_line(&self.writer, &reply).await;
        }
        self.task.abort();
    }
}

fn text_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// Render the limits answer: both windows in percent with local reset times,
/// or that no limit data is available yet.
fn limits_text(snapshot: &LimitsSnapshot) -> String {
    use canterlot_proto::worker::v1::LimitWindow;
    let mut windows: Vec<(&str, u32, u64)> = Vec::new();
    for observation in &snapshot.current {
        let name = match observation.window() {
            LimitWindow::FiveHour => "five-hour",
            LimitWindow::Weekly => "weekly",
            LimitWindow::Unspecified => continue,
        };
        windows.push((name, observation.used_permille, observation.resets_at_ms));
    }
    if windows.is_empty() {
        return "no limit data yet in this session".to_owned();
    }
    windows.sort();
    let parts: Vec<String> = windows
        .into_iter()
        .map(|(name, permille, resets_at_ms)| {
            format!(
                "{name} window {}% used, resets {}",
                permille as f64 / 10.0,
                format_reset(resets_at_ms)
            )
        })
        .collect();
    format!("Claude API budget: {}.", parts.join("; "))
}

/// Render epoch milliseconds as UTC `YYYY-MM-DD HH:MM`. The standard library
/// has no timezone database, so the model converts to the owner's locale.
fn format_reset(resets_at_ms: u64) -> String {
    let seconds = resets_at_ms / 1000;
    let days = (seconds / 86400) as i64;
    let time = (seconds % 86400) as i64;
    let mut year = 1970;
    let mut day = days;
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if day < year_days {
            break;
        }
        day -= year_days;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let lengths = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in lengths {
        if day < length {
            break;
        }
        day -= length;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02} {:02}:{:02} UTC",
        day + 1,
        time / 3600,
        (time % 3600) / 60
    )
}

async fn serve_conn(
    read: tokio::net::unix::OwnedReadHalf,
    writer: SharedWriter,
    pending: Arc<Mutex<HashMap<String, PendingCall>>>,
    events: mpsc::UnboundedSender<BrokerEvent>,
) {
    let mut lines = BufReader::new(read).lines();
    let mut rpc_to_request: HashMap<String, String> = HashMap::new();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            _ => break,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match mcp::handle_rpc(&msg) {
            mcp::McpAction::Reply(reply) => {
                let _ = write_line(&writer, &reply).await;
            }
            mcp::McpAction::ToolCall {
                kind,
                rpc_id,
                arguments,
            } => {
                if kind == mcp::ToolKind::Limits {
                    let _ = events.send(BrokerEvent::LimitsQuery { rpc_id });
                    continue;
                }
                let Some(request_id) = arguments
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    continue;
                };
                rpc_to_request.insert(rpc_id.to_string(), request_id.clone());
                match kind {
                    mcp::ToolKind::Permission => {
                        let payload = json!({
                            "tool_name": arguments.get("tool_name").cloned().unwrap_or(Value::Null),
                            "input": arguments.get("input").cloned().unwrap_or(Value::Null),
                            "tool_use_id": request_id,
                        });
                        pending.lock().await.insert(
                            request_id.clone(),
                            PendingCall {
                                rpc_id,
                                stored: arguments.get("input").cloned().unwrap_or(Value::Null),
                            },
                        );
                        let _ = events.send(BrokerEvent::Opened {
                            request_id,
                            kind: "permission",
                            payload,
                        });
                    }
                    mcp::ToolKind::Question => {
                        let payload = mcp::normalize_questions(
                            &arguments.get("input").cloned().unwrap_or(Value::Null),
                        );
                        pending.lock().await.insert(
                            request_id.clone(),
                            PendingCall {
                                rpc_id,
                                stored: json!({
                                    "normalized": payload,
                                    "original": arguments
                                        .get("input")
                                        .and_then(|input| input.get("questions"))
                                        .cloned()
                                        .unwrap_or(Value::Null),
                                }),
                            },
                        );
                        let _ = events.send(BrokerEvent::Opened {
                            request_id,
                            kind: "question",
                            payload,
                        });
                    }
                    // Handled above: limits calls carry no tool use id.
                    mcp::ToolKind::Limits => {}
                }
            }
            mcp::McpAction::Cancelled { request_id } => {
                if let Some(mapped) = rpc_to_request.remove(&request_id.to_string())
                    && pending.lock().await.remove(&mapped).is_some()
                {
                    let _ = events.send(BrokerEvent::Cancelled { request_id: mapped });
                }
            }
            mcp::McpAction::Ignore => {}
        }
    }
    // The relay is gone: the CLI process ended. Close every open request.
    let open: Vec<String> = pending.lock().await.keys().cloned().collect();
    for request_id in open {
        if pending.lock().await.remove(&request_id).is_some() {
            let _ = events.send(BrokerEvent::Cancelled { request_id });
        }
    }
}
