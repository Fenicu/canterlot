use crate::config::ClaudeCodeConfig;
use crate::containment::Containment;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SessionArg {
    New(Uuid),
    Resume(String),
}

#[derive(Debug)]
pub enum LaunchError {
    UnknownProfile,
    UnknownPermissionMode,
    MissingInstructions,
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

#[derive(Debug, Clone, Copy)]
pub enum Signal {
    Int,
    Term,
    Kill,
}

pub struct LaunchSpec<'a> {
    pub executable: &'a Path,
    pub exec_dir: &'a Path,
    pub workdir: &'a Path,
    pub session: SessionArg,
    pub model: &'a str,
    pub effort: &'a str,
    pub permission_mode: &'a str,
    pub instruction_profile: &'a str,
    pub instructions_file: Option<&'a Path>,
}

pub fn map_permission_mode(m: &str) -> Result<Option<&'static str>, LaunchError> {
    match m {
        "owner" => Ok(None),
        "ask" => Ok(Some("manual")),
        "bypass" => Ok(Some("bypassPermissions")),
        "read_only" => Ok(Some("plan")),
        _ => Err(LaunchError::UnknownPermissionMode),
    }
}

pub fn build_args(spec: &LaunchSpec, cfg: &ClaudeCodeConfig) -> Result<Vec<OsString>, LaunchError> {
    if !spec.workdir.is_dir() {
        return Err(LaunchError::WorkdirNotDir);
    }
    let is_owner = spec.instruction_profile.is_empty() || spec.instruction_profile == "owner";
    if !is_owner && spec.instructions_file.is_none() {
        return Err(LaunchError::MissingInstructions);
    }
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--replay-user-messages".into(),
        "--mcp-config".into(),
        spec.exec_dir.join("mcp.json").into_os_string(),
        "--permission-prompt-tool".into(),
        "mcp__canterlot__approve".into(),
        "--model".into(),
        spec.model.into(),
        "--effort".into(),
        spec.effort.into(),
    ];
    let mut settings = serde_json::json!({"crossSessionInbound":"refuse"});
    if !is_owner {
        let excludes: Vec<String> = if cfg.claude_md_excludes.is_empty() {
            let home = std::env::var("HOME").unwrap_or_default();
            vec![
                format!("{}/CLAUDE.md", home),
                format!("{}/.claude/CLAUDE.md", home),
            ]
        } else {
            cfg.claude_md_excludes
                .iter()
                .map(|p| p.display().to_string())
                .collect()
        };
        settings["claudeMdExcludes"] = serde_json::json!(excludes);
    }
    args.push("--settings".into());
    args.push(settings.to_string().into());
    if let Some(mode) = map_permission_mode(spec.permission_mode)? {
        args.push("--permission-mode".into());
        args.push(mode.into());
    }
    match &spec.session {
        SessionArg::New(id) => {
            args.push("--session-id".into());
            args.push(id.to_string().into());
        }
        SessionArg::Resume(id) => {
            args.push("--resume".into());
            args.push(id.clone().into());
        }
    }
    if !is_owner {
        args.push("--append-system-prompt-file".into());
        args.push(spec.exec_dir.join("profile.md").into_os_string());
    }
    Ok(args)
}

pub fn build_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    let mut out = Vec::new();
    for (k, v) in parent {
        let ks = k.to_string_lossy();
        let drop = ks == "ANTHROPIC_API_KEY"
            || ks == "ANTHROPIC_AUTH_TOKEN"
            || ks == "ANTHROPIC_BASE_URL"
            || ks == "CLAUDECODE"
            || ks.starts_with("CLAUDE_CODE_")
            || ks.starts_with("CANTERLOT_");
        if !drop {
            out.push((k, v));
        }
    }
    out.push(("DISABLE_AUTOUPDATER".into(), "1".into()));
    out
}

pub fn write_exec_files(
    spec: &LaunchSpec,
    _cfg: &ClaudeCodeConfig,
    relay_exe: &Path,
) -> Result<(), LaunchError> {
    std::fs::create_dir_all(spec.exec_dir).map_err(LaunchError::Io)?;
    let mcp = serde_json::json!({
        "mcpServers": {
            "canterlot": {
                "type": "stdio",
                "command": relay_exe.display().to_string(),
                "args": ["mcp-relay", "--socket", spec.exec_dir.join("mcp.sock").display().to_string()]
            }
        }
    });
    let mcp_path = spec.exec_dir.join("mcp.json");
    std::fs::write(&mcp_path, mcp.to_string()).map_err(LaunchError::Io)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&mcp_path, std::fs::Permissions::from_mode(0o600))
        .map_err(LaunchError::Io)?;
    if !spec.instruction_profile.is_empty() && spec.instruction_profile != "owner" {
        if let Some(src) = spec.instructions_file {
            let dst = spec.exec_dir.join("profile.md");
            std::fs::copy(src, &dst).map_err(LaunchError::Io)?;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o600))
                .map_err(LaunchError::Io)?;
        } else {
            return Err(LaunchError::MissingInstructions);
        }
    }
    Ok(())
}

pub struct CliProcess {
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

impl CliProcess {
    pub async fn spawn(
        spec: &LaunchSpec<'_>,
        cfg: &ClaudeCodeConfig,
        group: &dyn Containment,
    ) -> Result<Self, LaunchError> {
        let args = build_args(spec, cfg)?;
        let envs = build_env(std::env::vars_os());
        let mut cmd = TokioCommand::new(spec.executable);
        cmd.args(&args)
            .current_dir(spec.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &envs {
            cmd.env(k, v);
        }
        group.prepare_command(&mut cmd);
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
    pub async fn write_line(&mut self, json: &serde_json::Value) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(stdin) = &mut self.stdin {
            let line = format!("{}\n", json);
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
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_ring.lock().unwrap().clone()
    }
    pub fn signal(&self, sig: Signal) -> std::io::Result<()> {
        use rustix::process::{Pid, Signal as RSignal, kill_process};
        let pid = Pid::from_raw(self.pid as i32).ok_or_else(|| std::io::Error::other("bad pid"))?;
        let rsignal = match sig {
            Signal::Int => RSignal::INT,
            Signal::Term => RSignal::TERM,
            Signal::Kill => RSignal::KILL,
        };
        kill_process(pid, rsignal).map_err(std::io::Error::other)
    }
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

pub fn exit_event(status: std::process::ExitStatus) -> crate::adapter::AdapterEvent {
    use std::os::unix::process::ExitStatusExt;
    crate::adapter::AdapterEvent::ProcessExited {
        code: status.code(),
        signal: status.signal(),
    }
}
