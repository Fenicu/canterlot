//! Дочерний процесс `codex app-server` поверх stdio: запуск, чтение, корреляция.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::time::timeout;

use crate::evidence::{Dir, Recorder};
use crate::framing::LineFramer;
use crate::isolation;
use crate::protocol::{self, Correlator, Incoming, RequestId, Resolution, RpcError};

const MAX_LINE: usize = 16 * 1024 * 1024;

/// Событие от сервера, не являющееся ответом на наш запрос.
#[derive(Debug, Clone)]
pub enum Event {
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: RequestId,
        method: String,
        params: Value,
    },
    StdoutClosed,
}

pub struct SpawnOptions<'a> {
    pub scratch: &'a Path,
    pub extra_args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
}

pub struct AppServer {
    child: Child,
    pid: u32,
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    correlator: Arc<Mutex<Correlator>>,
    events: mpsc::UnboundedReceiver<Event>,
    next_id: AtomicI64,
    rec: Recorder,
}

impl AppServer {
    pub fn spawn(opts: SpawnOptions<'_>, rec: Recorder) -> anyhow::Result<Self> {
        let args = isolation::app_server_args(opts.scratch, &opts.extra_args);
        let mut env = isolation::child_env();
        env.extend(opts.extra_env.iter().cloned());
        rec.note(json!({
            "spawn": {
                "program": isolation::CODEX_BIN,
                "args": args,
                "cwd": opts.scratch.to_string_lossy(),
                "env_keys": env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            }
        }));
        let mut cmd = Command::new(isolation::CODEX_BIN);
        cmd.args(&args)
            .current_dir(opts.scratch)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // своя группа процессов: сигнал терминала пробы не задевает CLI, а дерево CLI адресуемо
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn().context("spawn codex app-server")?;
        let pid = child
            .id()
            .ok_or_else(|| anyhow!("child exited immediately"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let correlator = Arc::new(Mutex::new(Correlator::default()));
        let (tx, events) = mpsc::unbounded_channel();

        tokio::spawn(read_stdout(stdout, rec.clone(), correlator.clone(), tx));
        tokio::spawn(read_stderr(stderr, rec.clone()));
        rec.note(json!({ "spawned": { "pid": pid } }));
        Ok(Self {
            child,
            pid,
            stdin: Arc::new(AsyncMutex::new(Some(stdin))),
            correlator,
            events,
            next_id: AtomicI64::new(0),
            rec,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn recorder(&self) -> &Recorder {
        &self.rec
    }

    async fn write_value(&self, v: &Value) -> anyhow::Result<()> {
        let line = v.to_string();
        self.rec.line(Dir::In, &line);
        let mut guard = self.stdin.lock().await;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| anyhow!("stdin already closed"))?;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Отправляет запрос и возвращает id и приёмник ответа (без ожидания).
    pub async fn send_request(
        &self,
        method: &str,
        params: Value,
        quiet: bool,
    ) -> anyhow::Result<(i64, tokio::sync::oneshot::Receiver<Result<Value, RpcError>>)> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let rx = self
            .correlator
            .lock()
            .map_err(|_| anyhow!("correlator poisoned"))?
            .register(id);
        if quiet {
            self.rec.mark_quiet(id);
        }
        if let Err(e) = self
            .write_value(&protocol::request(id, method, params))
            .await
        {
            if let Ok(mut c) = self.correlator.lock() {
                c.forget(id);
            }
            return Err(e);
        }
        Ok((id, rx))
    }

    /// Запрос с ожиданием ответа. Ошибка JSON-RPC возвращается как `Ok(Err(..))`.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> anyhow::Result<Result<Value, RpcError>> {
        self.call_inner(method, params, wait, false).await
    }

    /// То же, но тело ответа не попадает в evidence (сводку пишет сценарий).
    pub async fn call_quiet(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> anyhow::Result<Result<Value, RpcError>> {
        self.call_inner(method, params, wait, true).await
    }

    async fn call_inner(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
        quiet: bool,
    ) -> anyhow::Result<Result<Value, RpcError>> {
        let (id, rx) = self.send_request(method, params, quiet).await?;
        match timeout(wait, rx).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(_)) => bail!("{method} (id {id}): server closed before responding"),
            Err(_) => {
                if let Ok(mut c) = self.correlator.lock() {
                    c.forget(id);
                }
                bail!("{method} (id {id}): no response within {wait:?}")
            }
        }
    }

    /// Запрос, у которого ожидается успешный результат.
    pub async fn call_ok(
        &self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> anyhow::Result<Value> {
        match self.call(method, params, wait).await? {
            Ok(v) => Ok(v),
            Err(e) => bail!("{method} failed: {} ({})", e.message, e.code),
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        self.write_value(&protocol::notification(method, params))
            .await
    }

    pub async fn respond(&self, id: &RequestId, result: Value) -> anyhow::Result<()> {
        self.write_value(&protocol::response(id, result)).await
    }

    /// Следующее событие или `None` по таймауту.
    pub async fn next_event(&mut self, wait: Duration) -> Option<Event> {
        match timeout(wait, self.events.recv()).await {
            Ok(Some(ev)) => Some(ev),
            Ok(None) => Some(Event::StdoutClosed),
            Err(_) => None,
        }
    }

    /// Рукопожатие `initialize` + `initialized`.
    pub async fn initialize(&self, experimental: bool) -> anyhow::Result<Value> {
        let mut params = json!({
            "clientInfo": {
                "name": "canterlot_probe",
                "title": "canterlot stage0 probe",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        if experimental {
            params["capabilities"] = json!({ "experimentalApi": true });
        }
        let res = self
            .call_ok("initialize", params, Duration::from_secs(30))
            .await?;
        self.notify("initialized", json!({})).await?;
        Ok(res)
    }

    pub async fn close_stdin(&self) {
        self.rec.step("close stdin (EOF)");
        let mut guard = self.stdin.lock().await;
        if let Some(mut s) = guard.take() {
            let _ = s.shutdown().await;
        }
    }

    pub fn signal(&self, sig: i32) -> anyhow::Result<()> {
        self.rec
            .note(json!({ "signal": { "pid": self.pid, "sig": sig } }));
        signal_pid(self.pid as i32, sig)
    }

    pub fn signal_group(&self, sig: i32) -> anyhow::Result<()> {
        self.rec
            .note(json!({ "signal_group": { "pgid": self.pid, "sig": sig } }));
        signal_pid(-(self.pid as i32), sig)
    }

    pub async fn wait_exit(&mut self, wait: Duration) -> Option<ExitStatus> {
        match timeout(wait, self.child.wait()).await {
            Ok(Ok(st)) => {
                self.rec
                    .note(json!({ "exit": format!("{st:?}"), "code": st.code(),
                    "signal": std::os::unix::process::ExitStatusExt::signal(&st) }));
                Some(st)
            }
            _ => None,
        }
    }

    /// Мягкое завершение: EOF, затем SIGTERM, затем SIGKILL группы.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.close_stdin().await;
        if self.wait_exit(Duration::from_secs(10)).await.is_some() {
            return Ok(());
        }
        self.rec.step("no exit after EOF within 10s; SIGTERM");
        self.signal(libc::SIGTERM)?;
        if self.wait_exit(Duration::from_secs(10)).await.is_some() {
            return Ok(());
        }
        self.rec.step("no exit after SIGTERM; SIGKILL group");
        self.signal_group(libc::SIGKILL)?;
        self.wait_exit(Duration::from_secs(10)).await;
        Ok(())
    }
}

pub fn signal_pid(pid: i32, sig: i32) -> anyhow::Result<()> {
    // SAFETY: kill(2) не трогает память процесса; pid принадлежит нашему потомку или его группе
    let rc = unsafe { libc::kill(pid, sig) };
    if rc != 0 {
        bail!(
            "kill({pid}, {sig}) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

async fn read_stdout(
    mut stdout: impl AsyncRead + Unpin,
    rec: Recorder,
    correlator: Arc<Mutex<Correlator>>,
    tx: mpsc::UnboundedSender<Event>,
) {
    let mut framer = LineFramer::new(MAX_LINE);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                rec.note(json!({ "stdout_read_error": e.to_string() }));
                break;
            }
        };
        for line in framer.push(&buf[..n]) {
            handle_line(line, &rec, &correlator, &tx);
        }
    }
    if let Some(line) = framer.finish() {
        handle_line(line, &rec, &correlator, &tx);
    }
    rec.note(json!({ "stdout": "closed" }));
    if let Ok(mut c) = correlator.lock() {
        c.fail_all();
    }
    let _ = tx.send(Event::StdoutClosed);
}

fn handle_line(
    line: Result<String, crate::framing::FrameError>,
    rec: &Recorder,
    correlator: &Arc<Mutex<Correlator>>,
    tx: &mpsc::UnboundedSender<Event>,
) {
    let line = match line {
        Ok(l) => l,
        Err(e) => {
            rec.note(json!({ "framing_error": e.to_string() }));
            return;
        }
    };
    rec.line(Dir::Out, &line);
    let value = match serde_json::from_str::<Value>(&line) {
        Ok(v) => v,
        Err(e) => {
            rec.note(json!({ "non_json_stdout_line": e.to_string() }));
            return;
        }
    };
    match protocol::classify(value) {
        Incoming::Response { id, result } => {
            let res = correlator
                .lock()
                .map(|mut c| c.resolve(&id, result))
                .unwrap_or(Resolution::Unmatched);
            if res == Resolution::Unmatched {
                rec.note(json!({ "unmatched_response": id.to_value() }));
            }
        }
        Incoming::Notification { method, params } => {
            let _ = tx.send(Event::Notification { method, params });
        }
        Incoming::ServerRequest { id, method, params } => {
            let _ = tx.send(Event::ServerRequest { id, method, params });
        }
        Incoming::Invalid { reason } => {
            rec.note(json!({ "invalid_message": reason }));
        }
    }
}

async fn read_stderr(mut stderr: impl AsyncRead + Unpin, rec: Recorder) {
    let mut framer = LineFramer::new(MAX_LINE);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for line in framer.push(&buf[..n]) {
            match line {
                Ok(l) => rec.write(Dir::Err, Value::String(strip_ansi(&l))),
                Err(e) => rec.note(json!({ "stderr_framing_error": e.to_string() })),
            }
        }
    }
    if let Some(Ok(l)) = framer.finish() {
        rec.write(Dir::Err, Value::String(strip_ansi(&l)));
    }
}

/// Убирает ANSI-последовательности цвета (`ESC [ ... m`) из строк журнала CLI.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_color_codes() {
        let s = "\u{1b}[2m2026\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m тест";
        assert_eq!(strip_ansi(s), "2026 ERROR тест");
    }
}
