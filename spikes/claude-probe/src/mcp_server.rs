//! Минимальный MCP-сервер (stdio, JSON-RPC 2.0) для `--permission-prompt-tool`.
//!
//! Запускается самим CLI как `claude-probe mcp-permission --socket <path>`.
//! Каждый вызов инструмента пересылается супервизору по Unix-сокету;
//! решение принимает супервизор, сервер только транслирует его.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, oneshot};

use crate::framing::{Frame, LineFramer, decode_line};

pub const SERVER_NAME: &str = "canterlot";
pub const TOOL_NAME: &str = "approve";
const FALLBACK_PROTOCOL: &str = "2025-06-18";

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Ответить сразу.
    Reply(Value),
    /// Уведомление без ответа.
    Ignore,
    /// Вызов инструмента: ответ придёт после решения супервизора.
    ToolCall { id: Value, arguments: Value },
    /// CLI отменил свой запрос (MCP `notifications/cancelled`).
    Cancelled { request_id: Value, reason: Value },
}

pub fn handle_rpc(msg: &Value) -> Action {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(id) = msg.get("id").cloned() else {
        if method == "notifications/cancelled" {
            return Action::Cancelled {
                request_id: msg
                    .pointer("/params/requestId")
                    .cloned()
                    .unwrap_or(Value::Null),
                reason: msg
                    .pointer("/params/reason")
                    .cloned()
                    .unwrap_or(Value::Null),
            };
        }
        return Action::Ignore;
    };
    match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            reply(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                }),
            )
        }
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(
            &id,
            json!({"tools": [{
                "name": TOOL_NAME,
                "description": "Permission prompt handler of the canterlot probe supervisor. Not for direct use.",
                "inputSchema": {"type": "object", "additionalProperties": true},
            }]}),
        ),
        "tools/call" => {
            if msg.pointer("/params/name").and_then(Value::as_str) != Some(TOOL_NAME) {
                return error(&id, -32602, "unknown tool");
            }
            let arguments = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Action::ToolCall { id, arguments }
        }
        _ => error(&id, -32601, "method not found"),
    }
}

fn reply(id: &Value, result: Value) -> Action {
    Action::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error(id: &Value, code: i64, message: &str) -> Action {
    Action::Reply(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

/// Ответ на tools/call: решение сериализуется в текстовый блок.
pub fn tool_call_reply(id: &Value, decision: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"content": [{"type": "text", "text": decision.to_string()}]},
    })
}

pub fn fail_closed_decision(reason: &str) -> Value {
    json!({"behavior": "deny", "message": format!("canterlot supervisor unavailable: {reason}")})
}

/// Ожидающие решения вызовы, сопоставляемые по call_id.
#[derive(Default)]
pub struct PendingCalls {
    next: AtomicU64,
    waiters: std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

impl PendingCalls {
    pub fn register(&self) -> (u64, oneshot::Receiver<Value>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        if let Ok(mut w) = self.waiters.lock() {
            w.insert(id, tx);
        }
        (id, rx)
    }

    /// Доставить решение; false, если такого вызова нет (дубликат или чужой id).
    pub fn resolve(&self, call_id: u64, decision: Value) -> bool {
        let tx = match self.waiters.lock() {
            Ok(mut w) => w.remove(&call_id),
            Err(_) => None,
        };
        match tx {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Закрыть все ожидания (связь с супервизором потеряна).
    pub fn fail_all(&self, reason: &str) {
        let drained: Vec<_> = match self.waiters.lock() {
            Ok(mut w) => w.drain().collect(),
            Err(_) => return,
        };
        for (_, tx) in drained {
            let _ = tx.send(fail_closed_decision(reason));
        }
    }
}

type SharedWriter<W> = Arc<Mutex<W>>;

async fn write_line<W: AsyncWriteExt + Unpin>(
    w: &SharedWriter<W>,
    v: &Value,
) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    let mut guard = w.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.flush().await
}

pub async fn run(socket_path: &str) -> anyhow::Result<()> {
    run_with(socket_path, tokio::io::stdin(), tokio::io::stdout()).await
}

pub async fn run_with<R, W>(socket_path: &str, input: R, output: W) -> anyhow::Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let stream = UnixStream::connect(socket_path).await?;
    let (sock_read, sock_write) = stream.into_split();
    let sock_w: SharedWriter<_> = Arc::new(Mutex::new(sock_write));
    let stdout_w: SharedWriter<W> = Arc::new(Mutex::new(output));
    let pending = Arc::new(PendingCalls::default());

    // Ответы супервизора.
    {
        let pending = pending.clone();
        tokio::spawn(async move {
            let mut reader = sock_read;
            let mut framer = LineFramer::new(1 << 20);
            let mut buf = vec![0u8; 8192];
            loop {
                let n = match reader.read(&mut buf).await {
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
                    let call_id = msg.get("call_id").and_then(Value::as_u64);
                    let decision = msg.get("decision").cloned();
                    if let (Some(call_id), Some(decision)) = (call_id, decision) {
                        pending.resolve(call_id, decision);
                    }
                }
            }
            pending.fail_all("socket closed");
        });
    }

    let mut stdin = input;
    let mut framer = LineFramer::new(16 << 20);
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stdin.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        for frame in framer.push(&buf[..n]) {
            let Frame::Line(bytes) = frame else { continue };
            let Ok(text) = decode_line(&bytes) else {
                continue;
            };
            let Ok(msg) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let _ = write_line(
                &sock_w,
                &json!({"kind": "trace", "dir": "mcp_in", "frame": msg}),
            )
            .await;
            match handle_rpc(&msg) {
                Action::Ignore => {}
                Action::Cancelled { request_id, reason } => {
                    // Супервизор помечает запрос устаревшим; позднее решение CLI уже не применит.
                    let _ = write_line(
                        &sock_w,
                        &json!({"kind": "cancelled", "jsonrpc_id": request_id, "reason": reason}),
                    )
                    .await;
                }
                Action::Reply(reply) => {
                    let _ = write_line(
                        &sock_w,
                        &json!({"kind": "trace", "dir": "mcp_out", "frame": reply}),
                    )
                    .await;
                    write_line(&stdout_w, &reply).await?;
                }
                Action::ToolCall { id, arguments } => {
                    let (call_id, rx) = pending.register();
                    let request = json!({"kind": "permission", "call_id": call_id, "jsonrpc_id": id, "arguments": arguments});
                    if write_line(&sock_w, &request).await.is_err() {
                        pending.resolve(call_id, fail_closed_decision("write failed"));
                    }
                    let sock_w = sock_w.clone();
                    let stdout_w = stdout_w.clone();
                    tokio::spawn(async move {
                        let decision = rx
                            .await
                            .unwrap_or_else(|_| fail_closed_decision("decision dropped"));
                        let reply = tool_call_reply(&id, &decision);
                        let _ = write_line(
                            &sock_w,
                            &json!({"kind": "trace", "dir": "mcp_out", "frame": reply}),
                        )
                        .await;
                        let _ = write_line(&stdout_w, &reply).await;
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_client_protocol_and_declares_tools() {
        let msg = json!({"jsonrpc":"2.0","id":0,"method":"initialize",
            "params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"claude-code"}}});
        let Action::Reply(r) = handle_rpc(&msg) else {
            panic!("expected reply")
        };
        assert_eq!(r["id"], 0);
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn tools_list_exposes_single_permissive_tool() {
        let Action::Reply(r) = handle_rpc(&json!({"jsonrpc":"2.0","id":"x","method":"tools/list"}))
        else {
            panic!("expected reply")
        };
        let tools = r["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], TOOL_NAME);
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
    }

    #[test]
    fn tools_call_is_forwarded_and_others_handled() {
        let call = json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"approve","arguments":{"tool_name":"Bash","input":{"command":"ls"}}}});
        assert_eq!(
            handle_rpc(&call),
            Action::ToolCall {
                id: json!(7),
                arguments: json!({"tool_name":"Bash","input":{"command":"ls"}})
            }
        );
        assert_eq!(
            handle_rpc(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Action::Ignore
        );
        assert_eq!(
            handle_rpc(&json!({"jsonrpc":"2.0","method":"notifications/cancelled",
                "params":{"requestId":2,"reason":"AbortError: remote-cancel"}})),
            Action::Cancelled {
                request_id: json!(2),
                reason: json!("AbortError: remote-cancel")
            }
        );
        let Action::Reply(ping) = handle_rpc(&json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
        else {
            panic!("expected reply")
        };
        assert_eq!(ping["result"], json!({}));
        let Action::Reply(err) =
            handle_rpc(&json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}))
        else {
            panic!("expected reply")
        };
        assert_eq!(err["error"]["code"], -32601);
        let Action::Reply(bad) = handle_rpc(
            &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"other"}}),
        ) else {
            panic!("expected reply")
        };
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[test]
    fn decision_is_returned_as_json_text_block() {
        let decision = json!({"behavior":"deny","message":"нет"});
        let r = tool_call_reply(&json!(7), &decision);
        assert_eq!(r["id"], 7);
        let text = r["result"]["content"][0]["text"].as_str().expect("text");
        assert_eq!(serde_json::from_str::<Value>(text).expect("json"), decision);
    }

    #[tokio::test]
    async fn pending_calls_route_out_of_order_and_reject_duplicates() {
        let p = PendingCalls::default();
        let (a, rx_a) = p.register();
        let (b, rx_b) = p.register();
        assert_ne!(a, b);
        assert!(p.resolve(b, json!("B")));
        assert!(p.resolve(a, json!("A")));
        assert!(!p.resolve(a, json!("again")));
        assert!(!p.resolve(999, json!("unknown")));
        assert_eq!(rx_a.await.expect("a"), json!("A"));
        assert_eq!(rx_b.await.expect("b"), json!("B"));
        let (_, rx_c) = p.register();
        p.fail_all("gone");
        assert_eq!(rx_c.await.expect("c")["behavior"], "deny");
    }
}
