//! Двойник `opencode serve --stdio`: настоящий HTTP и SSE на loopback по сценарию,
//! собранному из записей этапа 0 (`docs/stage0/evidence/opencode`). Только тесты и
//! ручной прогон (feature `synthetic-adapter`).
//!
//! Сценарий читается из `--script`, потому что запуск идёт через тот же путь, что у
//! настоящего CLI, с очищенным окружением. Тесты запускают обёртку, которая подставляет
//! этот аргумент (`exec`, поэтому PID совпадает с PID настоящего сервера).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Notify, mpsc},
};

const HEARTBEAT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Script {
    /// Строка `--version`.
    pub version: String,
    pub ready: Ready,
    pub routes: Vec<Route>,
    pub steps: Vec<Step>,
    /// Журнал запросов и запуска: тесты сверяют, что адаптер прислал на самом деле.
    pub log: Option<PathBuf>,
    /// Зависший сервер: EOF stdin и SIGTERM не завершают.
    pub ignore_eof: bool,
    pub ignore_term: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ready {
    /// Своя строка готовности вместо настоящей.
    pub line: Option<String>,
    pub delay_ms: u64,
    /// Не писать строку готовности вовсе.
    pub silent: bool,
    /// Упасть до строки готовности с этим кодом.
    pub exit: Option<u8>,
}

/// Запрос вне порядка: ответы идут по очереди, последний повторяется.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub replies: Vec<Reply>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reply {
    pub status: u16,
    pub body: Option<Value>,
    /// Сырые байты вместо JSON.
    pub raw: Option<String>,
    /// Ответ задержан, а следующие шаги сценария уже идут: приём без подтверждения.
    pub stall_ms: u64,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            status: 200,
            body: None,
            raw: None,
            stall_ms: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Дождаться запроса и ответить. `bind`: записанное значение -> указатель в теле запроса.
    Expect {
        method: String,
        path: String,
        reply: Reply,
        #[serde(default)]
        bind: BTreeMap<String, String>,
    },
    Emit {
        events: Vec<Value>,
    },
    /// Сырые байты в поток событий, обычно битый или неполный кадр.
    Frame {
        raw: String,
    },
    Stall {
        ms: u64,
    },
    DropSse,
    Die {
        code: u8,
        #[serde(default)]
        kill: bool,
    },
}

impl Step {
    pub fn expected_path(&self) -> Option<&str> {
        match self {
            Step::Expect { path, .. } => Some(path),
            _ => None,
        }
    }
}

pub fn evidence_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/opencode")
        .join(format!("{name}.jsonl"))
}

/// Строки записи; у сценарных файлов `scenario` выбирает один прогон.
pub fn capture_lines(name: &str, scenario: Option<&str>) -> Vec<Value> {
    let text = std::fs::read_to_string(evidence_path(name)).expect("evidence file");
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| {
            scenario
                .is_none_or(|wanted| line.get("scenario").and_then(Value::as_str) == Some(wanted))
        })
        .collect()
}

fn recorded_id(reply: &Reply) -> Option<String> {
    reply
        .body
        .as_ref()?
        .pointer("/data/id")?
        .as_str()
        .map(str::to_owned)
}

impl Script {
    /// Сценарий из записи: изменяющие запросы идут строго по порядку с событиями
    /// между ними, чтения становятся маршрутами с записанными ответами.
    pub fn from_capture(name: &str, scenario: Option<&str>) -> Self {
        let mut script = Self {
            version: "opencode v2.1.4".into(),
            ..Default::default()
        };
        let mut request: Option<(String, String)> = None;
        let mut events = Vec::new();
        for line in capture_lines(name, scenario) {
            let data = line.get("data").cloned().unwrap_or(Value::Null);
            match line.get("dir").and_then(Value::as_str) {
                Some("sse") if data["type"] != "server.connected" => events.push(data),
                Some("http_out") => {
                    if !events.is_empty() {
                        script.steps.push(Step::Emit {
                            events: std::mem::take(&mut events),
                        });
                    }
                    request = Some((
                        data["method"].as_str().unwrap_or_default().to_owned(),
                        data["path"].as_str().unwrap_or_default().to_owned(),
                    ));
                }
                Some("http_in") => {
                    let Some((method, path)) = request.take() else {
                        continue;
                    };
                    let mut reply = Reply {
                        status: u16::try_from(data["status"].as_u64().unwrap_or(200))
                            .unwrap_or(200),
                        body: Some(data["body"].clone()).filter(|body| !body.is_null()),
                        ..Default::default()
                    };
                    if method == "GET" {
                        if path == "/api/info"
                            && let Some(body) = reply.body.as_mut()
                        {
                            body["pid"] = "$PID".into();
                        }
                        script.push_route(&method, &path, reply);
                    } else {
                        let bind = recorded_id(&reply)
                            .into_iter()
                            .map(|id| (id, "/id".to_owned()))
                            .collect();
                        script.steps.push(Step::Expect {
                            method,
                            path,
                            reply,
                            bind,
                        });
                    }
                }
                _ => {}
            }
        }
        if !events.is_empty() {
            script.steps.push(Step::Emit { events });
        }
        script
    }

    /// Статус canterlot в `/api/mcp`: записанный `pending`, затем `status`.
    /// Запись `connected` есть только в выводе прогона, тело то же с другим статусом.
    pub fn with_canterlot_mcp(mut self, status: &str) -> Self {
        let pending = Self::from_capture("mcp", Some("mcp"))
            .route("GET", "/api/mcp")
            .and_then(|route| route.replies.first().cloned())
            .expect("recorded /api/mcp");
        let mut settled = pending.clone();
        if let Some(body) = settled.body.as_mut() {
            body["data"][0]["status"] = match status {
                "failed" => json!({"status": "failed", "error": "spawn failed"}),
                other => json!({"status": other}),
            };
        }
        self.set_route("GET", "/api/mcp", vec![pending, settled]);
        self
    }

    /// `GET /api/model` из записи с каталогом: `start_stream` его не читал.
    pub fn with_recorded_models(mut self) -> Self {
        let models = Self::from_capture("models", None)
            .route("GET", "/api/model")
            .and_then(|route| route.replies.first().cloned())
            .expect("recorded /api/model");
        self.set_route("GET", "/api/model", vec![models]);
        self
    }

    pub fn push_route(&mut self, method: &str, path: &str, reply: Reply) {
        match self
            .routes
            .iter_mut()
            .find(|route| route.method == method && route.path == path)
        {
            Some(route) => route.replies.push(reply),
            None => self.routes.push(Route {
                method: method.to_owned(),
                path: path.to_owned(),
                replies: vec![reply],
            }),
        }
    }

    pub fn set_route(&mut self, method: &str, path: &str, replies: Vec<Reply>) {
        self.routes
            .retain(|route| route.method != method || route.path != path);
        self.routes.push(Route {
            method: method.to_owned(),
            path: path.to_owned(),
            replies,
        });
    }

    pub fn route(&self, method: &str, path: &str) -> Option<&Route> {
        self.routes
            .iter()
            .find(|route| route.method == method && route.path == path)
    }
}

struct RouteState {
    route: Route,
    next: usize,
}

struct State {
    steps: Vec<Step>,
    pos: usize,
    table: Vec<(String, String)>,
    routes: Vec<RouteState>,
    subscribers: Vec<mpsc::UnboundedSender<Vec<u8>>>,
    log: Option<std::fs::File>,
}

struct Shared {
    state: Mutex<State>,
    wake: Notify,
    password: String,
    version: String,
    pid: u32,
    /// Ответы, которые тест дописывает на ходу: идентификаторы создаёт адаптер.
    overrides: Option<PathBuf>,
}

impl State {
    fn write_log(&mut self, entry: &Value) {
        if let Some(log) = self.log.as_mut() {
            let _ = writeln!(log, "{entry}");
        }
    }
}

// Записанные идентификаторы заменяются живыми во всех строках ответа и события.
fn substitute(value: &Value, table: &[(String, String)], pid: u32) -> Value {
    match value {
        Value::String(text) if text == "$PID" => pid.into(),
        Value::String(text) => {
            let mut out = text.clone();
            for (from, to) in table {
                if out.contains(from.as_str()) {
                    out = out.replace(from.as_str(), to);
                }
            }
            out.into()
        }
        Value::Array(items) => items.iter().map(|v| substitute(v, table, pid)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| (key.clone(), substitute(value, table, pid)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        other => other.clone(),
    }
}

fn frame(event: &Value) -> Vec<u8> {
    format!("data: {event}\n\n").into_bytes()
}

fn broadcast(state: &mut State, bytes: &[u8]) {
    state
        .subscribers
        .retain(|subscriber| subscriber.send(bytes.to_vec()).is_ok());
}

async fn run_steps(shared: Arc<Shared>) {
    loop {
        let step = {
            let state = shared.state.lock().unwrap();
            state.steps.get(state.pos).cloned()
        };
        let Some(step) = step else { return };
        match step {
            Step::Expect { .. } => {
                shared.wake.notified().await;
                continue;
            }
            Step::Emit { events } => {
                let mut state = shared.state.lock().unwrap();
                for event in events {
                    let event = substitute(&event, &state.table, shared.pid);
                    broadcast(&mut state, &frame(&event));
                }
            }
            Step::Frame { raw } => {
                broadcast(&mut shared.state.lock().unwrap(), raw.as_bytes());
            }
            Step::Stall { ms } => tokio::time::sleep(Duration::from_millis(ms)).await,
            Step::DropSse => shared.state.lock().unwrap().subscribers.clear(),
            Step::Die { code, kill } => {
                if kill {
                    let _ = rustix::process::kill_process(
                        rustix::process::getpid(),
                        rustix::process::Signal::KILL,
                    );
                }
                std::process::exit(i32::from(code));
            }
        }
        shared.state.lock().unwrap().pos += 1;
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn read_request<R>(reader: &mut BufReader<R>) -> Option<Request>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut length = 0usize;
    let mut authorization = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().ok()?,
            "authorization" => authorization = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.ok()?;
    let (path, query) = target
        .split_once('?')
        .map_or((target.clone(), String::new()), |(path, query)| {
            (path.to_owned(), query.to_owned())
        });
    Some(Request {
        method,
        path,
        query,
        authorization,
        body,
    })
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

async fn write_reply<W>(writer: &mut W, status: u16, body: &[u8]) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
        reason(status),
        body.len()
    );
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await
}

async fn serve_events<W>(shared: &Shared, writer: &mut W) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    writer
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache, no-transform\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await?;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let connected = json!({"id": "evt_fake_connected", "type": "server.connected", "data": {}});
    let _ = sender.send(frame(&connected));
    shared.state.lock().unwrap().subscribers.push(sender);
    let mut beat = tokio::time::interval(HEARTBEAT);
    beat.tick().await;
    loop {
        let bytes = tokio::select! {
            bytes = receiver.recv() => bytes,
            _ = beat.tick() => Some(b": heartbeat\n\n".to_vec()),
        };
        let Some(bytes) = bytes else {
            writer.write_all(b"0\r\n\r\n").await?;
            return writer.flush().await;
        };
        writer
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
            .await?;
        writer.write_all(&bytes).await?;
        writer.write_all(b"\r\n").await?;
        writer.flush().await?;
    }
}

fn authorized(shared: &Shared, request: &Request) -> bool {
    use base64::Engine as _;
    let expected =
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{}", shared.password));
    request.authorization.as_deref() == Some(&format!("Basic {expected}"))
}

fn serialize_reply(reply: &Reply, table: &[(String, String)], pid: u32) -> Vec<u8> {
    match (&reply.raw, &reply.body) {
        (Some(raw), _) => raw.clone().into_bytes(),
        (None, Some(body)) => substitute(body, table, pid).to_string().into_bytes(),
        (None, None) => Vec::new(),
    }
}

fn built_in(shared: &Shared, request: &Request) -> Reply {
    if request.method == "GET" && request.path == "/api/info" {
        let version = shared
            .version
            .strip_prefix("opencode v")
            .unwrap_or(&shared.version);
        return Reply {
            body: Some(json!({"version": version, "pid": shared.pid})),
            ..Default::default()
        };
    }
    // Пустая очередь и неизвестное сообщение, как у живого сервера.
    if request.method == "GET" && request.path.ends_with("/inbox") {
        return Reply {
            body: Some(json!({"data": []})),
            ..Default::default()
        };
    }
    Reply {
        status: 404,
        body: Some(json!({"_tag": "NotFoundError", "message": "no scripted reply"})),
        ..Default::default()
    }
}

fn override_reply(shared: &Shared, request: &Request) -> Option<Reply> {
    let text = std::fs::read_to_string(shared.overrides.as_ref()?).ok()?;
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry["method"] == request.method && entry["path"] == request.path)
        .map(|entry| Reply {
            body: Some(entry["body"].clone()),
            ..Default::default()
        })
}

/// Ответ на обычный запрос: добавка теста, шаг сценария, маршрут, встроенный.
fn plan_reply(shared: &Shared, request: &Request) -> (Reply, Vec<u8>, bool) {
    if let Some(reply) = override_reply(shared, request) {
        let bytes = reply
            .body
            .as_ref()
            .map(|body| body.to_string().into_bytes())
            .unwrap_or_default();
        return (reply, bytes, false);
    }
    let mut state = shared.state.lock().unwrap();
    let table = state.table.clone();
    let matched = match state.steps.get(state.pos) {
        Some(Step::Expect {
            method,
            path,
            reply,
            bind,
        }) if *method == request.method
            && substitute(&Value::String(path.clone()), &table, shared.pid)
                == Value::String(request.path.clone()) =>
        {
            Some((reply.clone(), bind.clone()))
        }
        _ => None,
    };
    if let Some((reply, bind)) = matched {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        for (recorded, pointer) in bind {
            if let Some(live) = body.pointer(&pointer).and_then(Value::as_str) {
                let live = live.to_owned();
                state.table.push((recorded, live));
            }
        }
        state.pos += 1;
        let bytes = serialize_reply(&reply, &state.table, shared.pid);
        return (reply, bytes, true);
    }
    let path = request.path.clone();
    let found = state.routes.iter_mut().find(|route| {
        route.route.method == request.method
            && substitute(&Value::String(route.route.path.clone()), &table, shared.pid)
                == Value::String(path.clone())
    });
    let reply = found.and_then(|route| {
        let reply = route
            .route
            .replies
            .get(route.next.min(route.route.replies.len().saturating_sub(1)))
            .cloned();
        route.next += 1;
        reply
    });
    let reply = reply.unwrap_or_else(|| built_in(shared, request));
    let bytes = serialize_reply(&reply, &table, shared.pid);
    (reply, bytes, false)
}

async fn serve_connection(shared: Arc<Shared>, stream: TcpStream) {
    let (read, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read);
    while let Some(request) = read_request(&mut reader).await {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let api = request.path.starts_with("/api/") || request.path == "/openapi.json";
        let ok = authorized(&shared, &request);
        shared.state.lock().unwrap().write_log(&json!({
            "method": request.method,
            "path": request.path,
            "query": request.query,
            "body": body,
            "authorized": ok,
        }));
        if !api {
            if write_reply(&mut writer, 200, b"{}").await.is_err() {
                return;
            }
            continue;
        }
        if !ok {
            let body = br#"{"_tag":"UnauthorizedError"}"#;
            if write_reply(&mut writer, 401, body).await.is_err() {
                return;
            }
            continue;
        }
        if request.method == "GET" && request.path == "/api/event" {
            let _ = serve_events(&shared, &mut writer).await;
            return;
        }
        let (reply, bytes, stepped) = plan_reply(&shared, &request);
        if reply.stall_ms > 0 {
            shared.wake.notify_one();
            tokio::time::sleep(Duration::from_millis(reply.stall_ms)).await;
        }
        let written = write_reply(&mut writer, reply.status, &bytes).await;
        if stepped {
            shared.wake.notify_one();
        }
        if written.is_err() {
            return;
        }
    }
}

fn boot_entry(args: &[String]) -> Value {
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(key, _)| key != "OPENCODE_PASSWORD")
        .collect();
    json!({
        "boot": true,
        "args": args,
        "env": env,
        "password_set": std::env::var_os("OPENCODE_PASSWORD").is_some(),
        "cwd": std::env::current_dir().ok(),
    })
}

fn load(args: &[String]) -> Option<(Script, Option<PathBuf>, Vec<String>)> {
    let position = args.iter().position(|arg| arg == "--script");
    let Some(position) = position else {
        return Some((Script::default(), None, args.to_vec()));
    };
    let path = args.get(position + 1)?;
    let script = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let overrides = Path::new(path).with_extension("overrides");
    let rest = args
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != position && *index != position + 1)
        .map(|(_, arg)| arg.clone())
        .collect();
    Some((script, Some(overrides), rest))
}

pub async fn main(args: Vec<String>) -> ExitCode {
    let Some((script, overrides, rest)) = load(&args) else {
        eprintln!("fake-opencode: bad --script");
        return ExitCode::from(2);
    };
    if rest.iter().any(|arg| arg == "--version") {
        println!("{}", script.version);
        return ExitCode::SUCCESS;
    }
    if rest != ["serve", "--stdio", "--port", "0"] {
        eprintln!("fake-opencode: unexpected arguments");
        return ExitCode::from(2);
    }
    let Some(password) = std::env::var_os("OPENCODE_PASSWORD") else {
        eprintln!("fake-opencode: OPENCODE_PASSWORD is required");
        return ExitCode::from(1);
    };
    let log = script.log.as_ref().and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    if let Some(code) = script.ready.exit {
        return ExitCode::from(code);
    }
    tokio::time::sleep(Duration::from_millis(script.ready.delay_ms)).await;
    let Ok(listener) = TcpListener::bind("127.0.0.1:0").await else {
        return ExitCode::from(1);
    };
    let Ok(address) = listener.local_addr() else {
        return ExitCode::from(1);
    };
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut state = State {
        steps: script.steps,
        pos: 0,
        table: vec![
            ("<probe>/ws".to_owned(), cwd.clone()),
            ("<probe>".to_owned(), cwd),
        ],
        routes: script
            .routes
            .into_iter()
            .map(|route| RouteState { route, next: 0 })
            .collect(),
        subscribers: Vec::new(),
        log,
    };
    state.write_log(&boot_entry(&rest));
    let shared = Arc::new(Shared {
        state: Mutex::new(state),
        wake: Notify::new(),
        password: password.to_string_lossy().into_owned(),
        version: script.version,
        pid: std::process::id(),
        overrides,
    });
    // Обработчики раньше строки готовности: сигнал сразу после неё иначе убил бы процесс.
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut interrupt)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return ExitCode::from(1);
    };
    let ignore_term = script.ignore_term;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = term.recv() => {}
                _ = interrupt.recv() => {}
            }
            if !ignore_term {
                std::process::exit(130);
            }
        }
    });
    if !script.ready.silent {
        let line = script
            .ready
            .line
            .unwrap_or_else(|| json!({"url": format!("http://{address}")}).to_string());
        println!("{line}");
    }
    tokio::spawn(run_steps(shared.clone()));
    let ignore_eof = script.ignore_eof;
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut sink = [0u8; 256];
        while matches!(stdin.read(&mut sink).await, Ok(count) if count > 0) {}
        if !ignore_eof {
            std::process::exit(0);
        }
    });
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return ExitCode::from(1);
        };
        tokio::spawn(serve_connection(shared.clone(), stream));
    }
}
