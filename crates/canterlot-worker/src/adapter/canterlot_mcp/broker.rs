//! Сторона супervisor: сокет MCP, JSON-RPC и события брокера.

use std::{
    collections::HashMap,
    os::unix::fs::DirBuilderExt as _,
    path::Path,
    sync::{Arc, Mutex as SyncMutex},
    time::Duration,
};

use canterlot_proto::{
    requests::{DecisionPayload, PermissionBehavior},
    worker::v1::LimitsSnapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt as _, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::{Instant, timeout_at},
};

use super::mcp::{self, ToolSet};

/// Предел строки JSON-RPC от relay: как у stdout CLI; длиннее — соединение закрывается.
pub const MAX_MCP_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Предел записи в relay. Ответы пишутся на цикле супервизора, который читает stdout
/// CLI: CLI, застрявший на записи в stdout, relay не читает, и ждать здесь нельзя.
const WRITE_DEADLINE: Duration = Duration::from_secs(5);

/// Вид ожидающего вызова задаётся при открытии, а не ключами входа инструмента.
enum PendingKind {
    Permission { input: Value },
    Question { normalized: Value, original: Value },
}

/// Ожидающий вызов агентского инструмента: ответ уходит в соединение вызова, поздний итог
/// хаба после отмены или разрыва молча отбрасывается.
struct AgentCall {
    call_id: uuid::Uuid,
    rpc_id: Value,
    tool: &'static str,
    writer: ConnWriter,
}

/// Ключ — соединение и rpc id: нумерация вызовов у каждого соединения своя.
type AgentPending = Arc<Mutex<HashMap<(u64, String), AgentCall>>>;

/// Ответ уходит в то соединение, откуда пришёл вызов. `None` — запись оборвалась
/// посреди строки, и следующая склеилась бы с её началом.
type ConnWriter = Arc<Mutex<Option<OwnedWriteHalf>>>;

struct PendingCall {
    rpc_id: Value,
    kind: PendingKind,
    conn: u64,
    writer: ConnWriter,
}

/// Вызов `canterlot_limits`: ответ пишется в соединение вызова.
pub struct LimitsCall {
    pub rpc_id: Value,
    writer: ConnWriter,
}

impl std::fmt::Debug for LimitsCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LimitsCall")
            .field("rpc_id", &self.rpc_id)
            .finish_non_exhaustive()
    }
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
        call_id: uuid::Uuid,
    },
    /// `share_file`: путь читает супервизор, ответ вернётся через agent_call_result.
    ShareFile {
        call_id: uuid::Uuid,
        arguments: Value,
    },
    /// Вызов агентского инструмента (пресеты, сессии): уходит в хаб как `AdapterEvent::AgentCall`.
    AgentTool {
        rpc_id: Value,
        call_id: uuid::Uuid,
        tool: &'static str,
        arguments: Value,
    },
}

/// Допуск соединений. С approve (Claude Code) живо одно соединение relay:
/// новое принимается, только если живого нет или адаптер взвёл допуск перед
/// запуском CLI; взведённое вытесняет прежнее. Без approve (Codex) соединений
/// может быть несколько, у каждого свой ответ и свои вызовы.
#[derive(Default)]
struct Gate {
    single: bool,
    live: Option<u64>,
    armed: bool,
    next: u64,
}

impl Gate {
    fn admit(&mut self) -> Option<u64> {
        if self.single && self.live.is_some() && !self.armed {
            return None;
        }
        self.armed = false;
        self.next += 1;
        self.live = Some(self.next);
        Some(self.next)
    }

    fn current(&self, conn: u64) -> bool {
        !self.single || self.live == Some(conn)
    }
}

type Pending = Arc<Mutex<HashMap<String, PendingCall>>>;

async fn write_line(writer: &ConnWriter, value: &Value) -> std::io::Result<()> {
    let mut line = value.to_string();
    line.push('\n');
    let deadline = Instant::now() + WRITE_DEADLINE;
    let expired = || std::io::Error::from(std::io::ErrorKind::TimedOut);
    let mut slot = timeout_at(deadline, writer.lock())
        .await
        .map_err(|_| expired())?;
    let stream = slot
        .as_mut()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
    let written = timeout_at(deadline, async {
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await
    })
    .await;
    if written.is_err() {
        *slot = None;
    }
    written.map_err(|_| expired())?
}

fn rpc_error(id: &Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": message}})
}

/// Переменная окружения relay с секретом сокета этой сессии.
pub const RELAY_SECRET_ENV: &str = "CANTERLOT_MCP_SECRET";

/// Первая строка relay: секрет сессии. Без неё сокет не принимает ни одного вызова.
pub fn relay_hello(secret: &str) -> String {
    format!("{RELAY_HELLO_PREFIX}{secret}\n")
}

const RELAY_HELLO_PREFIX: &str = "canterlot-relay ";
const HELLO_DEADLINE: Duration = Duration::from_secs(5);
const MAX_HELLO_BYTES: u64 = 256;

fn new_secret() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Сравнение без раннего выхода: время не выдаёт совпавшее начало.
fn same_secret(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// Первая строка соединения должна нести секрет этой сессии; иначе соединение закрывается
/// до допуска, и взведённый допуск Claude Code чужому процессу не достаётся.
async fn authenticated(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    secret: &str,
) -> bool {
    let mut line = Vec::new();
    let read = tokio::time::timeout(
        HELLO_DEADLINE,
        (&mut *reader)
            .take(MAX_HELLO_BYTES)
            .read_until(b'\n', &mut line),
    )
    .await;
    if !matches!(read, Ok(Ok(n)) if n > 0) || line.pop() != Some(b'\n') {
        return false;
    }
    line.strip_prefix(RELAY_HELLO_PREFIX.as_bytes())
        .is_some_and(|given| same_secret(given, secret.as_bytes()))
}

/// Соединение от процесса того же пользователя.
fn same_user(stream: &UnixStream) -> bool {
    stream
        .peer_cred()
        .is_ok_and(|cred| cred.uid() == rustix::process::getuid().as_raw())
}

pub struct Broker {
    events: mpsc::UnboundedReceiver<BrokerEvent>,
    pending: Pending,
    /// Ожидающие вызовы агентских инструментов по соединению и строке rpc id.
    agent_pending: AgentPending,
    gate: Arc<SyncMutex<Gate>>,
    task: JoinHandle<()>,
    limits_provider: String,
    secret: String,
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
        let pending: Pending = Arc::default();
        let agent_pending: AgentPending = Arc::default();
        let gate = Arc::new(SyncMutex::new(Gate {
            single: tool_set == ToolSet::ApproveAndLimits,
            ..Gate::default()
        }));
        let provider = limits_provider.to_owned();
        let secret = new_secret();
        let task = {
            let pending = pending.clone();
            let agent_pending = agent_pending.clone();
            let gate = gate.clone();
            let provider = provider.clone();
            let secret = secret.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    if !same_user(&stream) {
                        continue;
                    }
                    let (pending, agent_pending, gate, tx, provider, secret) = (
                        pending.clone(),
                        agent_pending.clone(),
                        gate.clone(),
                        tx.clone(),
                        provider.clone(),
                        secret.clone(),
                    );
                    // Приветствие ждётся в своей задаче: молчащий процесс не держит приём.
                    tokio::spawn(async move {
                        let (read, write) = stream.into_split();
                        let mut reader = BufReader::new(read);
                        if !authenticated(&mut reader, &secret).await {
                            tracing::warn!(
                                kind = "mcp_relay_rejected",
                                "mcp socket connection without the session secret"
                            );
                            return;
                        }
                        let Some(conn) = gate.lock().expect("mcp gate").admit() else {
                            return;
                        };
                        serve_conn(
                            conn,
                            reader,
                            Arc::new(Mutex::new(Some(write))),
                            pending,
                            agent_pending,
                            gate,
                            tx,
                            tool_set,
                            provider,
                        )
                        .await;
                    });
                }
            })
        };
        Ok(Self {
            events: rx,
            pending,
            agent_pending,
            gate,
            task,
            limits_provider: provider,
            secret,
        })
    }

    /// Секрет сокета этой сессии: relay получает его в `CANTERLOT_MCP_SECRET`.
    pub fn relay_secret(&self) -> &str {
        &self.secret
    }

    /// Разрешить следующее соединение relay: адаптер зовёт перед запуском
    /// (или перезапуском) CLI, новое соединение вытесняет прежнее.
    pub fn arm(&self) {
        self.gate.lock().expect("mcp gate").armed = true;
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
        let mut pending = self.pending.lock().await;
        let Some(call) = pending.get(request_id) else {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        };
        // Несовпавшее решение оставляет вызов ожидающим: CLI ещё ждёт ответа.
        let text = match (&call.kind, decision) {
            (
                PendingKind::Permission { input },
                DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    ..
                },
            ) => mcp::allow_text(input),
            (
                PendingKind::Permission { .. },
                DecisionPayload::Permission {
                    behavior: PermissionBehavior::Deny,
                    message,
                },
            ) => mcp::deny_text(message.as_deref()),
            (
                PendingKind::Question {
                    normalized,
                    original,
                },
                DecisionPayload::Question { answers },
            ) => {
                let id_to_text: Vec<(String, String)> = normalized["questions"]
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
                mcp::question_text(original, &id_to_text, answers)
            }
            _ => return DecisionOutcome::NotApplied("decision_mismatch".into()),
        };
        let Some(call) = pending.remove(request_id) else {
            return DecisionOutcome::NotApplied("request_not_pending".into());
        };
        drop(pending);
        let reply = mcp::tool_call_reply(&call.rpc_id, &text_value(&text));
        if write_line(&call.writer, &reply).await.is_err() {
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

    /// Отказ без решения владельца: годится и для вопроса, где ответа просто нет.
    pub async fn refuse(&mut self, request_id: &str, message: &str) {
        let Some(call) = self.pending.lock().await.remove(request_id) else {
            return;
        };
        let reply = mcp::tool_call_reply(&call.rpc_id, &text_value(&mcp::deny_text(Some(message))));
        let _ = write_line(&call.writer, &reply).await;
    }

    /// Итог вызова агентского инструмента от хаба: текст плюс подсказка или `isError` с кодом.
    /// Неизвестный (отменённый или чужой) вызов молча отбрасывается.
    pub async fn answer_agent_tool(
        &mut self,
        call_id: uuid::Uuid,
        result: &crate::adapter::AgentCallResult,
    ) {
        let found = {
            let mut pending = self.agent_pending.lock().await;
            let key = pending
                .iter()
                .find(|(_, call)| call.call_id == call_id)
                .map(|(key, _)| key.clone());
            key.and_then(|key| pending.remove(&key))
        };
        let Some(call) = found else {
            return;
        };
        let (text, is_error) = mcp::agent_tool_text(call.tool, result);
        let mut tool_result = json!({"content": [{"type": "text", "text": text}]});
        if is_error {
            tool_result["isError"] = json!(true);
        }
        let reply = json!({"jsonrpc": "2.0", "id": call.rpc_id, "result": tool_result});
        let _ = write_line(&call.writer, &reply).await;
    }

    // Запись ответа — отдельной задачей: отмена next_event адаптера её не обрывает.
    pub fn answer_limits(&mut self, call: LimitsCall, snapshot: &LimitsSnapshot) {
        let provider = if snapshot.provider.is_empty() {
            self.limits_provider.as_str()
        } else {
            snapshot.provider.as_str()
        };
        let text = mcp::limits_tool_text(snapshot, provider, chrono::Utc::now());
        let reply = mcp::tool_call_reply(&call.rpc_id, &Value::String(text));
        let writer = call.writer.clone();
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
            let _ = write_line(&call.writer, &reply).await;
        }
        // Ожидающие вызовы агентских инструментов отбрасываются: поздний итог хаба проигнорируют.
        self.agent_pending.lock().await.clear();
        self.task.abort();
    }
}

fn text_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// Следующая строка без `\n`; `None` — конец потока, ошибка или строка
/// длиннее предела (соединение закрывается, CLI видит отказ сервера, а не тишину).
async fn next_line(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    buf: &mut Vec<u8>,
) -> Option<String> {
    buf.clear();
    let limit = MAX_MCP_LINE_BYTES as u64 + 1;
    match (&mut *reader).take(limit).read_until(b'\n', buf).await {
        Ok(0) | Err(_) => return None,
        Ok(_) => {}
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > MAX_MCP_LINE_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(buf).into_owned())
}

#[allow(clippy::too_many_arguments)]
async fn serve_conn(
    conn: u64,
    mut reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: ConnWriter,
    pending: Pending,
    agent_pending: AgentPending,
    gate: Arc<SyncMutex<Gate>>,
    events: mpsc::UnboundedSender<BrokerEvent>,
    tool_set: ToolSet,
    provider: String,
) {
    let mut buf = Vec::new();
    let mut rpc_to_request: HashMap<String, String> = HashMap::new();
    while let Some(line) = next_line(&mut reader, &mut buf).await {
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
                if kind == mcp::ToolKind::ShareFile {
                    if !gate.lock().expect("mcp gate").current(conn) {
                        let _ =
                            write_line(&writer, &rpc_error(&rpc_id, "connection superseded")).await;
                        continue;
                    }
                    let call_id = uuid::Uuid::now_v7();
                    agent_pending.lock().await.insert(
                        (conn, rpc_id.to_string()),
                        AgentCall {
                            call_id,
                            rpc_id: rpc_id.clone(),
                            tool: "share_file",
                            writer: writer.clone(),
                        },
                    );
                    let _ = events.send(BrokerEvent::ShareFile { call_id, arguments });
                    continue;
                }
                if kind == mcp::ToolKind::Limits {
                    let call_id = uuid::Uuid::now_v7();
                    agent_pending.lock().await.insert(
                        (conn, rpc_id.to_string()),
                        AgentCall {
                            call_id,
                            rpc_id: rpc_id.clone(),
                            tool: "limits.report",
                            writer: writer.clone(),
                        },
                    );
                    let _ = events.send(BrokerEvent::LimitsQuery { call_id });
                    continue;
                }
                if let mcp::ToolKind::Agent { tool } = kind {
                    // Вытесненный CLI уже не вправе менять сессию.
                    if !gate.lock().expect("mcp gate").current(conn) {
                        let _ =
                            write_line(&writer, &rpc_error(&rpc_id, "connection superseded")).await;
                        continue;
                    }
                    let call_id = uuid::Uuid::now_v7();
                    agent_pending.lock().await.insert(
                        (conn, rpc_id.to_string()),
                        AgentCall {
                            call_id,
                            rpc_id: rpc_id.clone(),
                            tool,
                            writer: writer.clone(),
                        },
                    );
                    let _ = events.send(BrokerEvent::AgentTool {
                        rpc_id,
                        call_id,
                        tool,
                        arguments,
                    });
                    continue;
                }
                let Some(request_id) = arguments
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    // Без id ответа не будет никогда: CLI ждал бы вечно.
                    let _ =
                        write_line(&writer, &rpc_error(&rpc_id, "tool_use_id is required")).await;
                    continue;
                };
                // Вытесненное соединение (CLI уже заменён) запросов владельцу не открывает.
                if !gate.lock().expect("mcp gate").current(conn) {
                    let _ = write_line(&writer, &rpc_error(&rpc_id, "connection superseded")).await;
                    continue;
                }
                let input = arguments.get("input").cloned().unwrap_or(Value::Null);
                let (kind, call_kind, payload) = match kind {
                    mcp::ToolKind::Question => {
                        let payload = mcp::normalize_questions(&input);
                        let original = input.get("questions").cloned().unwrap_or(Value::Null);
                        (
                            "question",
                            PendingKind::Question {
                                normalized: payload.clone(),
                                original,
                            },
                            payload,
                        )
                    }
                    _ => (
                        "permission",
                        PendingKind::Permission {
                            input: input.clone(),
                        },
                        json!({
                            "tool_name": arguments.get("tool_name").cloned().unwrap_or(Value::Null),
                            "input": input,
                            "tool_use_id": request_id,
                        }),
                    ),
                };
                {
                    let mut pending = pending.lock().await;
                    // Повтор id не перезаписывает ожидающий вызов: тот ждёт своего ответа.
                    if pending.contains_key(&request_id) {
                        drop(pending);
                        let _ =
                            write_line(&writer, &rpc_error(&rpc_id, "duplicate tool_use_id")).await;
                        continue;
                    }
                    rpc_to_request.insert(rpc_id.to_string(), request_id.clone());
                    pending.insert(
                        request_id.clone(),
                        PendingCall {
                            rpc_id,
                            kind: call_kind,
                            conn,
                            writer: writer.clone(),
                        },
                    );
                }
                let _ = events.send(BrokerEvent::Opened {
                    request_id,
                    kind,
                    payload,
                });
            }
            mcp::McpAction::Cancelled { request_id } => {
                // Отмена агентского вызова: тихо отбрасываем, поздний итог проигнорируют.
                if agent_pending
                    .lock()
                    .await
                    .remove(&(conn, request_id.to_string()))
                    .is_some()
                {
                    continue;
                }
                let Some(mapped) = rpc_to_request.remove(&request_id.to_string()) else {
                    continue;
                };
                let mut pending = pending.lock().await;
                if pending.get(&mapped).is_some_and(|call| call.conn == conn) {
                    pending.remove(&mapped);
                    drop(pending);
                    let _ = events.send(BrokerEvent::Cancelled { request_id: mapped });
                }
            }
            mcp::McpAction::Ignore => {}
        }
    }
    {
        let mut gate = gate.lock().expect("mcp gate");
        if gate.live == Some(conn) {
            gate.live = None;
        }
    }
    // Разрыв отменяет только вызовы этого соединения.
    let open: Vec<String> = {
        let mut pending = pending.lock().await;
        let mine: Vec<String> = pending
            .iter()
            .filter(|(_, call)| call.conn == conn)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &mine {
            pending.remove(id);
        }
        mine
    };
    for request_id in open {
        let _ = events.send(BrokerEvent::Cancelled { request_id });
    }
    // Агентские вызовы разорванного соединения тихо отбрасываются.
    agent_pending
        .lock()
        .await
        .retain(|(owner, _), _| *owner != conn);
}
