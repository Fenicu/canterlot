use crate::containment::Containment;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

#[derive(Debug)]
pub enum LaunchError {
    UnknownPermissionMode,
    UnsupportedInstructions,
    WorkdirNotDir,
    SpawnFailed(std::io::Error),
    CliNotFoundInContainment,
    Io(std::io::Error),
}

#[derive(Debug)]
pub enum StdoutLine {
    Line(String),
    Oversized(usize),
    Eof,
}

pub struct LaunchSpec<'a> {
    pub executable: &'a Path,
    pub exec_dir: &'a Path,
    pub workdir: &'a Path,
    pub relay_exe: &'a Path,
}

// Значение `-c key=value` разбирается CLI как TOML, поэтому строки и массивы
// кодируем через toml: пути с пробелами и кавычками остаются валидными.
fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn toml_array(values: &[String]) -> String {
    toml::Value::Array(values.iter().cloned().map(toml::Value::String).collect()).to_string()
}

pub fn mcp_command_value(relay_exe: &Path) -> String {
    toml_string(&relay_exe.to_string_lossy())
}

pub fn mcp_args_value(exec_dir: &Path) -> String {
    toml_array(&[
        "mcp-relay".to_owned(),
        "--socket".to_owned(),
        exec_dir.join("mcp.sock").to_string_lossy().into_owned(),
    ])
}

// Только добавление нашего MCP и флага вопросов; чужая конфигурация не трогается.
pub fn build_args(spec: &LaunchSpec) -> Result<Vec<OsString>, LaunchError> {
    if !spec.workdir.is_dir() {
        return Err(LaunchError::WorkdirNotDir);
    }
    let mut args = vec![
        "app-server".into(),
        "--listen".into(),
        "stdio://".into(),
        "--enable".into(),
        "default_mode_request_user_input".into(),
    ];
    args.extend(super::mcp::launch_overrides(spec.relay_exe, spec.exec_dir));
    Ok(args)
}

// Чужие ключи биллинга и просочившееся окружение родителя отрезаем, ничего не добавляем.
pub fn build_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    let mut out = Vec::new();
    for (k, v) in parent {
        let ks = k.to_string_lossy();
        let drop = ks == "OPENAI_API_KEY"
            || ks == "CODEX_API_KEY"
            || ks == "OPENAI_BASE_URL"
            || ks == "CLAUDECODE"
            || ks.starts_with("CLAUDE_CODE_")
            || ks.starts_with("CANTERLOT_")
            || ks == "RUST_LOG";
        if !drop {
            out.push((k, v));
        }
    }
    out
}

pub struct AppServerProcess {
    pub pid: u32,
    pub start_ticks: u64,
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    // Недочитанная строка живёт здесь, а не в локальной переменной: отмена next_line ничего не теряет.
    partial: Vec<u8>,
    discarded: Option<usize>,
    stderr_ring: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl AppServerProcess {
    pub fn spawn(spec: &LaunchSpec<'_>, group: &dyn Containment) -> Result<Self, LaunchError> {
        let args = build_args(spec)?;
        let envs = build_env(std::env::vars_os());
        let mut cmd = TokioCommand::new(spec.executable);
        cmd.env_clear()
            .args(&args)
            .current_dir(spec.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &envs {
            cmd.env(k, v);
        }
        group.prepare_command(&mut cmd);
        // Процесс переживает адаптер: остановка идёт через EOF stdin и teardown сдерживания.
        cmd.kill_on_drop(false);
        let mut child = cmd.spawn().map_err(LaunchError::SpawnFailed)?;
        let child_pid = child
            .id()
            .ok_or(LaunchError::SpawnFailed(std::io::Error::other("no pid")))?;
        let member = group
            .members()
            .map_err(|e| LaunchError::Io(std::io::Error::other(e.to_string())))?
            .into_iter()
            .find(|m| m.pid == child_pid)
            .ok_or(LaunchError::CliNotFoundInContainment)?;
        let pid = member.pid;
        let start_ticks = member.start_ticks;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(tokio::io::BufReader::new);
        let stderr = child.stderr.take();
        let ring = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        if let Some(mut se) = stderr {
            let ringc = ring.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 4096];
                loop {
                    match se.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut r = ringc.lock().unwrap();
                            r.extend_from_slice(&buf[..n]);
                            if r.len() > 64 * 1024 {
                                let start = r.len() - 64 * 1024;
                                r.drain(..start);
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
        Ok(Self {
            pid,
            start_ticks,
            child,
            stdin,
            stdout,
            partial: Vec::new(),
            discarded: None,
            stderr_ring: ring,
        })
    }

    pub async fn write_message(&mut self, msg: &serde_json::Value) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(stdin) = &mut self.stdin {
            let line = format!("{msg}\n");
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        } else {
            Err(std::io::Error::other("no stdin"))
        }
    }

    /// Следующая строка stdout без `\n`; длиннее 64 МиБ пропускается до перевода строки.
    pub async fn next_line(&mut self) -> StdoutLine {
        use tokio::io::AsyncBufReadExt;
        const CAP: usize = 64 * 1024 * 1024;
        let Some(stdout) = &mut self.stdout else {
            return StdoutLine::Eof;
        };
        loop {
            let available = stdout.fill_buf().await.unwrap_or_default();
            if available.is_empty() {
                if let Some(len) = self.discarded.take() {
                    return StdoutLine::Oversized(len);
                }
                if self.partial.is_empty() {
                    return StdoutLine::Eof;
                }
                let line = std::mem::take(&mut self.partial);
                return StdoutLine::Line(String::from_utf8_lossy(&line).into_owned());
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let chunk = newline.unwrap_or(available.len());
            let consumed = newline.map_or(chunk, |pos| pos + 1);
            if let Some(len) = self.discarded.as_mut() {
                *len += consumed;
            } else if self.partial.len() + chunk > CAP {
                self.discarded = Some(std::mem::take(&mut self.partial).len() + consumed);
            } else {
                self.partial.extend_from_slice(&available[..chunk]);
            }
            stdout.consume(consumed);
            if newline.is_some() {
                if let Some(len) = self.discarded.take() {
                    return StdoutLine::Oversized(len);
                }
                let line = std::mem::take(&mut self.partial);
                return StdoutLine::Line(String::from_utf8_lossy(&line).into_owned());
            }
        }
    }

    // Хвост stderr только для классификации стартовых ошибок, в логи не идёт.
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_ring.lock().unwrap().clone()
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}
