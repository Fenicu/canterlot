//! Сторона супервизора: Unix-сокет, к которому подключается MCP-сервер разрешений.
//!
//! Решение принимает политика сценария; вызов можно задержать (Hold) и ответить
//! позже через `release`, чтобы проверить поздние и отменённые решения.

use std::collections::HashMap;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use crate::evidence::Recorder;
use crate::framing::{Frame, LineFramer, decode_line};

pub enum PolicyAction {
    Respond(Value),
    Hold,
}

pub type Policy = Arc<dyn Fn(&Value) -> PolicyAction + Send + Sync>;

#[derive(Debug, Clone)]
pub struct PermissionSeen {
    pub call_id: u64,
    pub arguments: Value,
    pub t_ms: u64,
    pub decision: Option<Value>,
}

type Writer = Arc<Mutex<OwnedWriteHalf>>;

pub struct Broker {
    held: Arc<Mutex<HashMap<u64, Writer>>>,
    policy: Arc<std::sync::Mutex<Policy>>,
    rec: Arc<Recorder>,
    task: JoinHandle<()>,
}

pub fn allow(input: &Value) -> Value {
    json!({"behavior": "allow", "updatedInput": input})
}

pub fn deny(message: &str) -> Value {
    json!({"behavior": "deny", "message": message})
}

/// Имя инструмента из аргументов вызова prompt-tool (форма не описана в документации).
pub fn requested_tool(args: &Value) -> &str {
    ["tool_name", "toolName", "name"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .unwrap_or_default()
}

pub fn requested_input(args: &Value) -> Value {
    ["input", "tool_input", "arguments"]
        .iter()
        .find_map(|k| args.get(*k).cloned())
        .unwrap_or(Value::Null)
}

pub fn requested_tool_use_id(args: &Value) -> Option<&str> {
    ["tool_use_id", "toolUseID", "toolUseId"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
}

pub fn deny_all(reason: &'static str) -> Policy {
    Arc::new(move |_| PolicyAction::Respond(deny(reason)))
}

async fn write_line(w: &Writer, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    let mut g = w.lock().await;
    g.write_all(line.as_bytes()).await?;
    g.flush().await
}

impl Broker {
    pub async fn start(
        socket: &Path,
        rec: Arc<Recorder>,
        policy: Policy,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<PermissionSeen>)> {
        if let Some(dir) = socket.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        let (tx, rx) = mpsc::unbounded_channel();
        let held: Arc<Mutex<HashMap<u64, Writer>>> = Arc::default();
        let policy = Arc::new(std::sync::Mutex::new(policy));
        let task = {
            let held = held.clone();
            let policy = policy.clone();
            let rec = rec.clone();
            tokio::spawn(async move {
                let mut conn_no = 0u32;
                while let Ok((stream, _)) = listener.accept().await {
                    conn_no += 1;
                    rec.note(json!({"mcp_connection": {"conn": conn_no, "event": "connected"}}));
                    let (read, write) = stream.into_split();
                    let writer: Writer = Arc::new(Mutex::new(write));
                    tokio::spawn(serve_conn(
                        conn_no,
                        read,
                        writer,
                        held.clone(),
                        policy.clone(),
                        rec.clone(),
                        tx.clone(),
                    ));
                }
            })
        };
        Ok((
            Self {
                held,
                policy,
                rec,
                task,
            },
            rx,
        ))
    }

    pub fn set_policy(&self, policy: Policy) {
        if let Ok(mut p) = self.policy.lock() {
            *p = policy;
        }
    }

    /// Ответить на задержанный вызов. false, если вызова нет или связь потеряна.
    pub async fn release(&self, call_id: u64, decision: Value) -> bool {
        let writer = self.held.lock().await.remove(&call_id);
        let Some(writer) = writer else {
            self.rec.note(
                json!({"permission_release": {"call_id": call_id, "result": "no such held call"}}),
            );
            return false;
        };
        let ok = write_line(&writer, &json!({"call_id": call_id, "decision": decision}))
            .await
            .is_ok();
        self.rec.note(json!({"permission_release": {"call_id": call_id, "decision": decision, "written": ok}}));
        ok
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_conn(
    conn_no: u32,
    mut read: tokio::net::unix::OwnedReadHalf,
    writer: Writer,
    held: Arc<Mutex<HashMap<u64, Writer>>>,
    policy: Arc<std::sync::Mutex<Policy>>,
    rec: Arc<Recorder>,
    tx: mpsc::UnboundedSender<PermissionSeen>,
) {
    let mut framer = LineFramer::new(16 << 20);
    let mut buf = vec![0u8; 8192];
    // JSON-RPC id запроса CLI -> наш call_id (для notifications/cancelled).
    let mut by_rpc_id: HashMap<String, u64> = HashMap::new();
    loop {
        let n = match read.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in framer.push(&buf[..n]) {
            let Frame::Line(bytes) = frame else { continue };
            let Ok(text) = decode_line(&bytes) else {
                continue;
            };
            let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            match msg.get("kind").and_then(Value::as_str) {
                Some("trace") => {
                    rec.note(json!({"mcp_trace": {"conn": conn_no, "dir": msg["dir"], "frame": msg["frame"]}}));
                }
                Some("cancelled") => {
                    let call_id = by_rpc_id.get(&msg["jsonrpc_id"].to_string()).copied();
                    let still_held = match call_id {
                        Some(id) => held.lock().await.contains_key(&id),
                        None => false,
                    };
                    rec.note(json!({"permission_cancelled_by_cli": {"call_id": call_id, "jsonrpc_id": msg["jsonrpc_id"],
                                    "reason": msg["reason"], "was_unanswered": still_held}}));
                }
                Some("permission") => {
                    let call_id = msg.get("call_id").and_then(Value::as_u64).unwrap_or(0);
                    by_rpc_id.insert(msg["jsonrpc_id"].to_string(), call_id);
                    let arguments = msg.get("arguments").cloned().unwrap_or(Value::Null);
                    let t_ms = rec.note(
                        json!({"permission_request": {"call_id": call_id, "arguments": arguments}}),
                    );
                    let action = match policy.lock() {
                        Ok(p) => (p)(&arguments),
                        Err(_) => PolicyAction::Respond(deny("policy poisoned")),
                    };
                    let decision = match action {
                        PolicyAction::Respond(decision) => {
                            let ok = write_line(
                                &writer,
                                &json!({"call_id": call_id, "decision": decision}),
                            )
                            .await
                            .is_ok();
                            rec.note(json!({"permission_decision": {"call_id": call_id, "decision": decision, "written": ok}}));
                            Some(decision)
                        }
                        PolicyAction::Hold => {
                            held.lock().await.insert(call_id, writer.clone());
                            rec.note(json!({"permission_held": {"call_id": call_id}}));
                            None
                        }
                    };
                    let _ = tx.send(PermissionSeen {
                        call_id,
                        arguments,
                        t_ms,
                        decision,
                    });
                }
                _ => {
                    rec.note(json!({"mcp_unknown_ipc": msg}));
                }
            }
        }
    }
    rec.note(json!({"mcp_connection": {"conn": conn_no, "event": "closed"}}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    async fn read_json<R: AsyncBufReadExt + Unpin>(r: &mut R) -> Value {
        let mut line = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), r.read_line(&mut line))
            .await
            .expect("timeout")
            .expect("read");
        serde_json::from_str(&line).expect("json")
    }

    #[test]
    fn extracts_request_fields_tolerantly() {
        let args =
            json!({"tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "toolu_1"});
        assert_eq!(requested_tool(&args), "Bash");
        assert_eq!(requested_input(&args), json!({"command": "ls"}));
        assert_eq!(requested_tool_use_id(&args), Some("toolu_1"));
        assert_eq!(requested_tool(&json!({})), "");
        assert_eq!(requested_input(&json!({})), Value::Null);
    }

    #[tokio::test]
    async fn permission_round_trip_through_mcp_server_with_hold_and_release() {
        let dir = std::env::temp_dir().join(format!("claude-probe-test-{}", uuid::Uuid::new_v4()));
        let socket = dir.join("perm.sock");
        let rec = Arc::new(Recorder::with_sink(Box::new(std::io::sink())));
        let policy: Policy = Arc::new(|args: &Value| {
            if requested_input(args)["command"] == "touch ok" {
                PolicyAction::Respond(allow(&requested_input(args)))
            } else {
                PolicyAction::Hold
            }
        });
        let (broker, mut seen) = Broker::start(&socket, rec, policy).await.expect("broker");

        let (client, server_side) = tokio::io::duplex(1 << 16);
        let (server_read, server_write) = tokio::io::split(server_side);
        let socket_str = socket.to_string_lossy().into_owned();
        let server = tokio::spawn(async move {
            crate::mcp_server::run_with(&socket_str, server_read, server_write).await
        });
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        let frames = [
            json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"approve",
                "arguments":{"tool_name":"Bash","input":{"command":"touch ok"},"tool_use_id":"t1"}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"approve",
                "arguments":{"tool_name":"Bash","input":{"command":"rm -rf x"},"tool_use_id":"t2"}}}),
        ];
        for f in frames {
            client_write
                .write_all(format!("{f}\n").as_bytes())
                .await
                .expect("write");
        }
        let init = read_json(&mut client_read).await;
        assert_eq!(init["id"], 0);
        let first = read_json(&mut client_read).await;
        assert_eq!(first["id"], 1);
        let decision: Value = serde_json::from_str(
            first["result"]["content"][0]["text"]
                .as_str()
                .expect("text"),
        )
        .expect("json");
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(decision["updatedInput"]["command"], "touch ok");

        let s1 = seen.recv().await.expect("seen 1");
        assert!(s1.decision.is_some());
        let s2 = seen.recv().await.expect("seen 2");
        assert!(s2.decision.is_none());
        assert_eq!(requested_tool_use_id(&s2.arguments), Some("t2"));

        assert!(broker.release(s2.call_id, deny("no")).await);
        assert!(!broker.release(s2.call_id, deny("twice")).await);
        let second = read_json(&mut client_read).await;
        assert_eq!(second["id"], 2);
        assert!(
            second["result"]["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("\"deny\"")
        );

        drop(client_write);
        drop(client_read);
        let finished = tokio::time::timeout(std::time::Duration::from_secs(5), server).await;
        assert!(finished.is_ok(), "mcp server must stop on stdin EOF");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
