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

/// Сколько соединений одновременно проходят допуск (родство и приветствие). Следующее
/// закрывается сразу, до порождения работы: до проверки секрета соединения того же
/// пользователя ничего не стоят тому, кто их открывает.
pub const MAX_ADMISSIONS: usize = 16;
/// Сколько раз проверка родства начинается заново, если цепочка поменялась под рукой.
const CHAIN_ATTEMPTS: usize = 3;
/// Предел длины цепочки родителей: глубже дерево исполнения не бывает.
const CHAIN_DEPTH: usize = 128;

/// Pid соединившегося процесса, если он того же пользователя. `SO_PEERCRED` даёт только
/// номер; время старта, прочитанное сразу при приёме, привязывает проверку к поколению
/// процесса, который соединился. `SO_PEERPIDFD` привязал бы надёжнее, но rustix его не
/// читает, а небезопасный код в крейте запрещён.
fn same_user_peer(stream: &UnixStream) -> Option<u32> {
    let cred = stream.peer_cred().ok()?;
    if cred.uid() != rustix::process::getuid().as_raw() {
        return None;
    }
    u32::try_from(cred.pid()?).ok()
}

/// Звено цепочки родителей: процесс и номер его родителя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Link {
    pid: u32,
    start_ticks: u64,
    parent: u32,
}

fn read_link(pid: u32) -> Option<Link> {
    let stat = crate::containment::stat(pid).ok()?;
    Some(Link {
        pid,
        start_ticks: stat.member.start_ticks,
        parent: stat.parent,
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Walk {
    Inside,
    Outside,
    /// Цепочка поменялась между чтениями: читать заново.
    Changed,
}

/// Процесс `peer` (pid, время старта) — корень дерева или его потомок. Одно
/// сопровождение ведёт одно исполнение: CLI и его relay — потомки сопровождения,
/// осиротевшие процессы исполнения достаются ему же (`PR_SET_CHILD_SUBREAPER`). Агент
/// соседней сессии в это дерево не попадает, даже зная секрет: тот читается с диска и из
/// `/proc` под тем же uid.
///
/// Читается только цепочка родителей самого peer, не весь `/proc`: закрытый от чтения
/// чужой процесс (`hidepid`) допуску не мешает, а работа ограничена длиной цепочки.
/// Каждое звено привязано к поколению: peer тот же, что при приёме, родитель старше
/// ребёнка, а ребёнок после чтения родителя всё ещё у него. Иначе номер умершего
/// родителя мог занять другой процесс, и звенья принадлежали бы разным поколениям.
/// Поменявшаяся цепочка перечитывается до `CHAIN_ATTEMPTS` раз; не подтвердилось или не
/// прочлось — отказ.
fn in_tree(peer: (u32, u64), root: (u32, u64)) -> bool {
    in_tree_with(peer, root, read_link)
}

fn in_tree_with(
    peer: (u32, u64),
    root: (u32, u64),
    mut read: impl FnMut(u32) -> Option<Link>,
) -> bool {
    for _ in 0..CHAIN_ATTEMPTS {
        match walk(peer, root, &mut read) {
            Walk::Inside => return true,
            Walk::Outside => return false,
            Walk::Changed => {}
        }
    }
    false
}

fn walk(peer: (u32, u64), root: (u32, u64), read: &mut impl FnMut(u32) -> Option<Link>) -> Walk {
    let mut child = peer;
    for _ in 0..CHAIN_DEPTH {
        if child == root {
            return Walk::Inside;
        }
        if child.0 == root.0 {
            return Walk::Outside;
        }
        let link = read(child.0).filter(|link| (link.pid, link.start_ticks) == child);
        let Some(link) = link else {
            // Соединившийся процесс исчез или его номер уже занят: не от кого допускать.
            return if child == peer {
                Walk::Outside
            } else {
                Walk::Changed
            };
        };
        if link.parent <= 1 {
            return Walk::Outside;
        }
        let Some(parent) = read(link.parent) else {
            return Walk::Changed;
        };
        if parent.start_ticks > link.start_ticks || read(child.0) != Some(link) {
            return Walk::Changed;
        }
        child = (parent.pid, parent.start_ticks);
    }
    Walk::Outside
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
        Self::bind_in_tree(exec_dir, tool_set, limits_provider, std::process::id()).await
    }

    /// То же с явным корнем дерева допуска. В работе корень — само сопровождение; тесты
    /// подставляют другой процесс, чтобы соединиться извне дерева.
    pub async fn bind_in_tree(
        exec_dir: &Path,
        tool_set: ToolSet,
        limits_provider: &str,
        tree_root: u32,
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
            single: tool_set.has_approve(),
            ..Gate::default()
        }));
        let provider = limits_provider.to_owned();
        let secret = new_secret();
        let root = (
            tree_root,
            crate::containment::start_ticks(tree_root).map_err(std::io::Error::other)?,
        );
        let admissions = Arc::new(tokio::sync::Semaphore::new(MAX_ADMISSIONS));
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
                    let Some(peer) = same_user_peer(&stream) else {
                        continue;
                    };
                    // Предел проверяемых соединений берётся до любой работы.
                    let Ok(admission) = admissions.clone().try_acquire_owned() else {
                        tracing::warn!(
                            kind = "mcp_relay_rejected",
                            "too many mcp socket connections awaiting admission"
                        );
                        continue;
                    };
                    // Одно чтение `stat` сразу после приёма: поколение соединившегося.
                    let Ok(started) = crate::containment::start_ticks(peer) else {
                        tracing::warn!(
                            kind = "mcp_relay_rejected",
                            "mcp socket peer is gone before admission"
                        );
                        continue;
                    };
                    let peer = (peer, started);
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
                        // Цепочка родителей — блокирующие чтения: не на потоке цикла приёма.
                        let inside = tokio::task::spawn_blocking(move || in_tree(peer, root)).await;
                        if !matches!(inside, Ok(true)) {
                            tracing::warn!(
                                kind = "mcp_relay_rejected",
                                "mcp socket connection from outside the session process tree"
                            );
                            return;
                        }
                        let (read, write) = stream.into_split();
                        let mut reader = BufReader::new(read);
                        if !authenticated(&mut reader, &secret).await {
                            tracing::warn!(
                                kind = "mcp_relay_rejected",
                                "mcp socket connection without the session secret"
                            );
                            return;
                        }
                        drop(admission);
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
    let mut initialized = false;
    while let Some(line) = next_line(&mut reader, &mut buf).await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !initialized && msg.get("method").and_then(Value::as_str) == Some("initialize") {
            initialized = true;
            mcp::log_initialize(&msg);
        }
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const ROOT: (u32, u64) = (100, 10);

    fn link(pid: u32, start_ticks: u64, parent: u32) -> Link {
        Link {
            pid,
            start_ticks,
            parent,
        }
    }

    /// `/proc` из таблицы; прочитанные номера записываются.
    fn table(links: &[Link]) -> (HashMap<u32, Link>, Vec<u32>) {
        (
            links.iter().map(|link| (link.pid, *link)).collect(),
            Vec::new(),
        )
    }

    /// Обход читает только цепочку peer: чужой процесс, закрытый от чтения, ему не мешает,
    /// и работа не зависит от числа процессов машины.
    #[test]
    fn the_walk_reads_only_the_peer_chain() {
        let (procs, mut reads) = table(&[link(100, 10, 1), link(200, 20, 100), link(300, 30, 200)]);
        let inside = in_tree_with((300, 30), ROOT, |pid| {
            reads.push(pid);
            // Всё вне цепочки закрыто (`hidepid`): прочесть нельзя.
            procs.get(&pid).copied()
        });
        assert!(inside);
        assert!(
            reads.iter().all(|pid| [100, 200, 300].contains(pid)),
            "{reads:?}"
        );
        assert!(reads.len() <= 3 * 3, "{reads:?}");
        // Корень сам себе допуск.
        assert!(in_tree_with(ROOT, ROOT, |_| None));
    }

    /// Процесс вне дерева, родитель, ушедший к init, и peer с другим временем старта
    /// (номер уже занят другим процессом) — отказ без повторов.
    #[test]
    fn foreign_orphaned_and_replaced_peers_are_outside() {
        let (procs, _) = table(&[
            link(100, 10, 1),
            link(400, 40, 1),
            link(500, 50, 400),
            link(300, 30, 1),
        ]);
        let mut reads = 0;
        let mut read = |pid: u32| {
            reads += 1;
            procs.get(&pid).copied()
        };
        assert!(!in_tree_with((500, 50), ROOT, &mut read));
        assert!(!in_tree_with((300, 30), ROOT, &mut read));
        assert!(
            !in_tree_with((300, 29), ROOT, &mut read),
            "pid reused after accept"
        );
        // Номер корня с другим временем старта — не корень.
        assert!(!in_tree_with((100, 11), ROOT, &mut read));
        assert!(reads <= 6, "no retries for a settled answer: {reads}");
    }

    /// Номер умершего родителя занял процесс моложе ребёнка: звенья разных поколений.
    /// Такая цепочка не подтверждается, а повторов не больше `CHAIN_ATTEMPTS`.
    #[test]
    fn a_parent_younger_than_its_child_is_another_generation() {
        let (procs, _) = table(&[link(100, 10, 1), link(200, 90, 100), link(300, 30, 200)]);
        let mut walks = 0;
        let inside = in_tree_with((300, 30), ROOT, |pid| {
            if pid == 300 {
                walks += 1;
            }
            procs.get(&pid).copied()
        });
        assert!(!inside);
        assert!(walks <= CHAIN_ATTEMPTS * 2, "bounded rereads: {walks}");
    }

    /// Ребёнка усыновили между чтением его и родителя: родитель умер, сирота перешёл к
    /// сопровождению (subreaper). Обход перечитывает цепочку и видит новое родство.
    #[test]
    fn a_reparented_link_is_reread() {
        let mut parent_of_300 = 200;
        let mut reads_of_300 = 0;
        let inside = in_tree_with((300, 30), ROOT, |pid| match pid {
            100 => Some(link(100, 10, 1)),
            200 => {
                // Родитель уходит сразу после того, как его прочли.
                parent_of_300 = 100;
                Some(link(200, 20, 100))
            }
            300 => {
                reads_of_300 += 1;
                Some(link(300, 30, parent_of_300))
            }
            _ => None,
        });
        assert!(inside);
        assert!(reads_of_300 >= 2, "the chain was reread");
    }
}
