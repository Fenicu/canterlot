//! Дочерний процесс CLI: запуск с чистым окружением, чтение stdout/stderr,
//! запись в stdin, сигналы и снимок дерева процессов.

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::evidence::{Dir, Recorder};
use crate::framing::{Frame, LineFramer, decode_line};

#[derive(Debug, Clone)]
pub enum Event {
    Out(u64, Value),
    OutText(u64, String),
    Err(u64, String),
    StdoutClosed(u64),
}

pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

pub struct ClaudeProcess {
    child: Child,
    pub pid: i32,
    stdin: Option<ChildStdin>,
    rx: mpsc::UnboundedReceiver<Event>,
    rec: Arc<Recorder>,
    /// Момент закрытия stdout ребёнка (t_ms), если уже закрыт.
    pub stdout_closed_ms: Option<u64>,
    /// Строки stderr: диагностика CLI, не протокол.
    pub stderr_lines: Vec<(u64, String)>,
    /// Строки stdout, не являющиеся JSON (нарушение формата stream-json).
    pub non_json_lines: Vec<(u64, String)>,
    exit: Option<ExitStatus>,
}

impl ClaudeProcess {
    pub fn spawn(spec: &SpawnSpec, rec: Arc<Recorder>) -> anyhow::Result<Self> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Своя группа: сигналы терминала не доходят, дерево можно адресовать целиком.
            .process_group(0)
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let pid = child
            .id()
            .and_then(|p| i32::try_from(p).ok())
            .ok_or_else(|| anyhow::anyhow!("child has no pid"))?;
        rec.note(json!({"spawn": {
            "pid": pid,
            "program": spec.program,
            "args": spec.args,
            "cwd": spec.cwd,
            "env_keys": spec.env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        }}));
        let (tx, rx) = mpsc::unbounded_channel();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stderr"))?;
        tokio::spawn(read_stream(stdout, rec.clone(), tx.clone(), true));
        tokio::spawn(read_stream(stderr, rec.clone(), tx, false));
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            pid,
            rx,
            rec,
            stdout_closed_ms: None,
            stderr_lines: Vec::new(),
            non_json_lines: Vec::new(),
            exit: None,
        })
    }

    pub async fn send(&mut self, v: &Value) -> anyhow::Result<u64> {
        let mut line = v.to_string();
        line.push('\n');
        self.write_raw(line, v.clone()).await
    }

    /// Запись произвольной строки (например, заведомо битого JSON).
    pub async fn send_text(&mut self, text: &str) -> anyhow::Result<u64> {
        self.write_raw(format!("{text}\n"), Value::String(text.to_owned()))
            .await
    }

    async fn write_raw(&mut self, line: String, logged: Value) -> anyhow::Result<u64> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("stdin already closed"))?;
        let t = self.rec.record(Dir::In, logged);
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(t)
    }

    pub fn close_stdin(&mut self) {
        if self.stdin.take().is_some() {
            self.rec.note(json!({"step": "stdin closed (EOF)"}));
        }
    }

    pub async fn next(&mut self, timeout: Duration) -> Option<Event> {
        let ev = tokio::time::timeout(timeout, self.rx.recv()).await.ok()??;
        match &ev {
            Event::StdoutClosed(t) => self.stdout_closed_ms = Some(*t),
            Event::Err(t, line) => self.stderr_lines.push((*t, line.clone())),
            Event::OutText(t, line) => self.non_json_lines.push((*t, line.clone())),
            Event::Out(..) => {}
        }
        Some(ev)
    }

    /// Ждать первый JSON-кадр stdout, для которого `pred` вернул true.
    /// `pred` видит каждый кадр, поэтому годится и для ведения журналов.
    pub async fn wait_for<F>(&mut self, timeout: Duration, mut pred: F) -> Option<(u64, Value)>
    where
        F: FnMut(u64, &Value) -> bool,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.next(left).await? {
                Event::Out(t, v) => {
                    if pred(t, &v) {
                        return Some((t, v));
                    }
                }
                Event::StdoutClosed(_) => return None,
                Event::OutText(..) | Event::Err(..) => {}
            }
        }
    }

    pub fn signal(&self, sig: Signal) -> u64 {
        let res = signal::kill(Pid::from_raw(self.pid), sig);
        self.rec.note(json!({"signal": {"target": "pid", "pid": self.pid, "signal": sig.as_str(), "ok": res.is_ok()}}))
    }

    pub fn signal_group(&self, sig: Signal) -> u64 {
        let res = signal::killpg(Pid::from_raw(self.pid), sig);
        self.rec.note(json!({"signal": {"target": "process_group", "pgid": self.pid, "signal": sig.as_str(), "ok": res.is_ok()}}))
    }

    pub fn try_exit(&mut self) -> Option<ExitStatus> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().ok().flatten();
        }
        self.exit
    }

    pub async fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        if let Some(st) = self.exit {
            return Some(st);
        }
        let status = tokio::time::timeout(timeout, self.child.wait())
            .await
            .ok()?
            .ok()?;
        self.exit = Some(status);
        self.rec.note(json!({"exit": exit_json(&status)}));
        Some(status)
    }

    /// Снимок процессов группы CLI (включая потомков, сменивших группу).
    pub fn tree(&self) -> Vec<ProcInfo> {
        process_tree(self.pid)
    }

    pub fn record_tree(&self, label: &str) -> Vec<ProcInfo> {
        let tree = self.tree();
        self.rec.note(json!({"process_tree": {"label": label, "root_pid": self.pid, "processes": tree.iter().map(ProcInfo::to_json).collect::<Vec<_>>()}}));
        tree
    }
}

pub fn exit_json(status: &ExitStatus) -> Value {
    use std::os::unix::process::ExitStatusExt;
    json!({"code": status.code(), "signal": status.signal().map(|s| Signal::try_from(s).map(|x| x.as_str().to_owned()).unwrap_or(s.to_string()))})
}

async fn read_stream<R: AsyncRead + Unpin>(
    mut r: R,
    rec: Arc<Recorder>,
    tx: mpsc::UnboundedSender<Event>,
    is_stdout: bool,
) {
    let mut framer = LineFramer::new(32 << 20);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in framer.push(&buf[..n]) {
            emit(frame, &rec, &tx, is_stdout);
        }
    }
    if let Some(frame) = framer.finish() {
        emit(frame, &rec, &tx, is_stdout);
    }
    if is_stdout {
        let t = rec.note(json!({"step": "stdout closed"}));
        let _ = tx.send(Event::StdoutClosed(t));
    }
}

fn emit(frame: Frame, rec: &Recorder, tx: &mpsc::UnboundedSender<Event>, is_stdout: bool) {
    let line = match frame {
        Frame::Line(bytes) => bytes,
        Frame::Oversized { dropped } => {
            rec.note(json!({"oversized_line": {"stdout": is_stdout, "bytes": dropped}}));
            return;
        }
    };
    let text = match decode_line(&line) {
        Ok(t) => t,
        Err(lossy) => {
            rec.note(json!({"invalid_utf8_line": {"stdout": is_stdout}}));
            lossy
        }
    };
    if !is_stdout {
        let t = rec.record(Dir::Err, Value::String(text.clone()));
        let _ = tx.send(Event::Err(t, text));
        return;
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => {
            let t = rec.record(Dir::Out, v.clone());
            let _ = tx.send(Event::Out(t, v));
        }
        Err(_) => {
            let t = rec.record(Dir::Out, Value::String(text.clone()));
            let _ = tx.send(Event::OutText(t, text));
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub state: char,
    pub comm: String,
    /// Время старта в тиках с загрузки: вместе с pid идентифицирует процесс.
    pub start_ticks: u64,
    pub cmdline: String,
}

impl ProcInfo {
    pub fn to_json(&self) -> Value {
        json!({"pid": self.pid, "ppid": self.ppid, "pgid": self.pgid, "state": self.state.to_string(),
               "comm": self.comm, "start_ticks": self.start_ticks, "cmdline": self.cmdline})
    }

    /// Тот же ли это процесс сейчас (pid не переиспользован).
    pub fn still_alive(&self) -> bool {
        std::fs::read_to_string(format!("/proc/{}/stat", self.pid))
            .ok()
            .and_then(|s| parse_stat(&s))
            .is_some_and(|now| now.start_ticks == self.start_ticks && now.state != 'Z')
    }
}

/// Разбор /proc/<pid>/stat: имя в скобках может содержать пробелы и скобки.
pub fn parse_stat(stat: &str) -> Option<ProcInfo> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let pid = stat[..open].trim().parse().ok()?;
    let comm = stat[open + 1..close].to_owned();
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let state = rest.first()?.chars().next()?;
    let ppid = rest.get(1)?.parse().ok()?;
    let pgid = rest.get(2)?.parse().ok()?;
    // Поле 22 (starttime) идёт 20-м после имени.
    let start_ticks = rest.get(19).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(ProcInfo {
        pid,
        ppid,
        pgid,
        state,
        comm,
        start_ticks,
        cmdline: String::new(),
    })
}

pub fn proc_snapshot() -> Vec<ProcInfo> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(mut info) = parse_stat(&stat) else {
            continue;
        };
        info.cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                let s: String = String::from_utf8_lossy(&b).replace('\0', " ");
                s.trim().chars().take(160).collect()
            })
            .unwrap_or_default();
        out.push(info);
    }
    out
}

/// Процессы группы `root` и все потомки `root` по цепочке ppid.
pub fn select_tree(all: &[ProcInfo], root: i32) -> Vec<ProcInfo> {
    let mut keep: Vec<i32> = vec![root];
    let mut changed = true;
    while changed {
        changed = false;
        for p in all {
            if !keep.contains(&p.pid) && (keep.contains(&p.ppid) || p.pgid == root) {
                keep.push(p.pid);
                changed = true;
            }
        }
    }
    let mut out: Vec<ProcInfo> = all
        .iter()
        .filter(|p| keep.contains(&p.pid))
        .cloned()
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

pub fn process_tree(root: i32) -> Vec<ProcInfo> {
    select_tree(&proc_snapshot(), root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_spaces_and_parens_in_comm() {
        let s = "1234 (my (odd) proc) S 1000 1234 1234 0 -1 4194560 151 0 0 0 1 2 0 0 20 0 1 0 987654 1000 10";
        let got = parse_stat(s).expect("parsed");
        assert_eq!(
            (
                got.pid,
                got.comm.as_str(),
                got.state,
                got.ppid,
                got.pgid,
                got.start_ticks
            ),
            (1234, "my (odd) proc", 'S', 1000, 1234, 987654)
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn own_process_is_alive_and_fake_start_time_is_not() {
        let me = std::process::id() as i32;
        let info = parse_stat(&std::fs::read_to_string(format!("/proc/{me}/stat")).expect("stat"))
            .expect("parsed");
        assert!(info.still_alive());
        let mut other = info.clone();
        other.start_ticks += 1;
        assert!(!other.still_alive());
    }

    fn p(pid: i32, ppid: i32, pgid: i32) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            pgid,
            state: 'S',
            comm: format!("p{pid}"),
            start_ticks: 0,
            cmdline: String::new(),
        }
    }

    #[test]
    fn selects_group_members_and_descendants_only() {
        let all = vec![
            p(1, 0, 1),
            p(100, 1, 100),
            p(101, 100, 100),
            p(102, 101, 102), // потомок, сменивший группу
            p(103, 1, 100),   // член группы, переподвешенный к init
            p(200, 1, 200),   // посторонний
        ];
        let got: Vec<i32> = select_tree(&all, 100).iter().map(|x| x.pid).collect();
        assert_eq!(got, vec![100, 101, 102, 103]);
    }
}
