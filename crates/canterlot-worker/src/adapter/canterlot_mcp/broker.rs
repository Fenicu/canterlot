//! Сторона супervisor: сокет MCP, JSON-RPC и события брокера.

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

use super::mcp::{self, ToolSet};

struct PendingCall {
    rpc_id: Value,
    stored: Value,
}

fn is_question(stored: &Value) -> bool {
    stored.get("normalized").is_some()
}

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
    limits_provider: String,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Broker {
    pub async fn bind(exec_dir: &Path) -> std::io::Result<Self> {
        Self::bind_with(exec_dir, ToolSet::ApproveAndLimits, "claude").await
    }

    pub async fn bind_with(
        exec_dir: &Path,
        tool_set: ToolSet,
        limits_provider: &str,
    ) -> std::io::Result<Self> {
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
        let provider = limits_provider.to_owned();
        let task = {
            let pending = pending.clone();
            let writer = writer.clone();
            let provider = provider.clone();
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
                        tool_set,
                        provider.clone(),
                    ));
                }
            })
        };
        Ok(Self {
            events: rx,
            pending,
            writer,
            task,
            limits_provider: provider,
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

    pub async fn answer(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> crate::adapter::DecisionOutcome {
        use crate::adapter::{DecisionOutcome, DecisionProof};
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

    // Запись ответа — отдельной задачей: отмена next_event адаптера её не обрывает.
    pub fn answer_limits(&mut self, rpc_id: Value, snapshot: &LimitsSnapshot) {
        let provider = if snapshot.provider.is_empty() {
            self.limits_provider.as_str()
        } else {
            snapshot.provider.as_str()
        };
        let text = mcp::limits_tool_text(snapshot, provider);
        let reply = mcp::tool_call_reply(&rpc_id, &Value::String(text));
        let writer = self.writer.clone();
        tokio::spawn(async move {
            let _ = write_line(&writer, &reply).await;
        });
    }

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

async fn serve_conn(
    read: tokio::net::unix::OwnedReadHalf,
    writer: SharedWriter,
    pending: Arc<Mutex<HashMap<String, PendingCall>>>,
    events: mpsc::UnboundedSender<BrokerEvent>,
    tool_set: ToolSet,
    provider: String,
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
        match mcp::handle_rpc_with_provider(&msg, tool_set, &provider) {
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
    let open: Vec<String> = pending.lock().await.keys().cloned().collect();
    for request_id in open {
        if pending.lock().await.remove(&request_id).is_some() {
            let _ = events.send(BrokerEvent::Cancelled { request_id });
        }
    }
}
