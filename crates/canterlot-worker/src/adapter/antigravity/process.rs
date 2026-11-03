//! Процесс `agy acp`: закреплённая копия, окружение из allowlist, запуск и закрытие.

use super::version::{self, CliVersion, VersionError};
use crate::adapter::spawn_with_retry_sync;
use crate::config::AntigravityConfig;
use crate::containment::{Containment, Member};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

/// Всё, что переживает `env_clear`; остальное — от владельца и не передаётся.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "TERM",
    "TMPDIR",
    "SSH_AUTH_SOCK",
    "CARGO_BUILD_JOBS",
];

/// `base_env` оставляет только разрешённые имена, значения берутся у родителя.
pub fn base_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    parent
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            ENV_ALLOWLIST.contains(&key.as_ref())
        })
        .collect()
}

/// Окружение ACP-процесса: allowlist и headless-хранилище без keyring.
pub fn process_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut env = base_env(parent);
    env.push((
        OsString::from("GEMINI_FORCE_FILE_STORAGE"),
        OsString::from("true"),
    ));
    env
}

/// Аргументы проверенной версии, включая `--dangerously-skip-permissions` только в bypass.
pub fn launch_args(exec_dir: &Path, permission_mode: &str) -> Result<Vec<OsString>, LaunchError> {
    let mut args: Vec<OsString> = vec![
        "acp".into(),
        "--log-file".into(),
        exec_dir.join("agy.log").into_os_string(),
        "--disable-telemetry".into(),
    ];
    match permission_mode {
        "bypass" => args.push("--dangerously-skip-permissions".into()),
        "owner" | "ask" => {}
        _ => return Err(LaunchError::UnknownPermissionMode),
    }
    // `--gemini-dir` не передаётся: сессии и вход владельца в его `~/.gemini`.
    Ok(args)
}

#[derive(Debug)]
pub enum LaunchError {
    UnknownPermissionMode,
    WorkdirNotDir,
    DataDirMissing,
    SourceNotFound,
    SpawnFailed(std::io::Error),
    CliNotFoundInContainment,
    Io(std::io::Error),
}

#[derive(Debug)]
pub enum PrepareError {
    MissingDataDir,
    MissingBinary,
    Version(VersionError),
    Io(std::io::Error),
}

impl From<std::io::Error> for PrepareError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct Pinned {
    pub executable: PathBuf,
    pub version: CliVersion,
}

fn resolve_source(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file().then(|| path.to_owned());
    }
    let path_env = std::env::var_os("PATH")?;
    std::env::split_paths(&path_env)
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
}

fn source_binary(cfg: &AntigravityConfig) -> Result<PathBuf, PrepareError> {
    if let Some(path) = &cfg.binary {
        return resolve_source(path).ok_or(PrepareError::MissingBinary);
    }
    let home = std::env::var_os("HOME").ok_or(PrepareError::MissingBinary)?;
    resolve_source(&Path::new(&home).join(".local/bin/agy")).ok_or(PrepareError::MissingBinary)
}

/// Закреплённая копия `<data_dir>/antigravity/bin/agy-<version>`.
/// Копия создаётся один раз; исходник проверяется `--version` при каждом запуске.
pub async fn prepare_pinned(cfg: &AntigravityConfig) -> Result<Pinned, PrepareError> {
    let source = source_binary(cfg)?;
    let version = version::check(&source)
        .await
        .map_err(PrepareError::Version)?;
    let data_dir = cfg.data_dir.clone().ok_or(PrepareError::MissingDataDir)?;
    let bin_dir = data_dir.join("antigravity").join("bin");
    create_private_dir(&bin_dir)?;
    let target = bin_dir.join(format!("agy-{}", version.label()));
    if !target.is_file() && target != source {
        // Копия через временное имя: оборванный запуск не оставит половину файла.
        let temporary = bin_dir.join(format!(".agy-{}-copy", std::process::id()));
        std::fs::copy(&source, &temporary)?;
        set_executable(&temporary)?;
        std::fs::rename(&temporary, &target)?;
    } else {
        set_executable(&target)?;
    }
    Ok(Pinned {
        executable: target,
        version,
    })
}

fn create_private_dir(path: &Path) -> Result<(), PrepareError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

fn set_executable(path: &Path) -> Result<(), PrepareError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[derive(Debug)]
pub enum StdoutLine {
    Line(String),
    Oversized(usize),
    Eof,
}

pub struct Process {
    pub pid: u32,
    pub start_ticks: u64,
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    partial: Vec<u8>,
    discarded: Option<usize>,
}

impl Process {
    pub fn spawn(
        executable: &Path,
        exec_dir: &Path,
        workdir: &Path,
        permission_mode: &str,
        group: &dyn Containment,
    ) -> Result<Self, LaunchError> {
        if !workdir.is_dir() {
            return Err(LaunchError::WorkdirNotDir);
        }
        let args = launch_args(exec_dir, permission_mode)?;
        let env = process_env(std::env::vars_os());
        // Повтор при ETXTBSY: тесты пишут двойника перед запуском.
        let child = spawn_with_retry_sync(|| {
            let mut cmd = TokioCommand::new(executable);
            cmd.env_clear()
                .args(&args)
                .current_dir(workdir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            for (key, value) in &env {
                cmd.env(key, value);
            }
            group.prepare_command(&mut cmd);
            // Процесс переживает адаптер: остановка идёт через EOF stdin и containment.
            cmd.kill_on_drop(false);
            cmd.spawn()
        })
        .map_err(LaunchError::SpawnFailed)?;
        let child_pid = child
            .id()
            .ok_or_else(|| LaunchError::SpawnFailed(std::io::Error::other("no pid")))?;
        let member = group
            .members()
            .map_err(|error| LaunchError::Io(std::io::Error::other(error.to_string())))?
            .into_iter()
            .find(|member| member.pid == child_pid)
            .ok_or(LaunchError::CliNotFoundInContainment)?;
        group.register_cli(&member);
        Ok(Self::from_child(child, member.pid, member.start_ticks))
    }

    /// Короткий процесс пробы: своя группа, без containment и MCP.
    pub fn spawn_probe(
        executable: &Path,
        workdir: &Path,
        exec_dir: &Path,
    ) -> Result<Self, LaunchError> {
        if !workdir.is_dir() {
            return Err(LaunchError::WorkdirNotDir);
        }
        let args = launch_args(exec_dir, "ask")?;
        let env = process_env(std::env::vars_os());
        let child = spawn_with_retry_sync(|| {
            let mut cmd = TokioCommand::new(executable);
            cmd.env_clear()
                .args(&args)
                .current_dir(workdir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .kill_on_drop(true);
            for (key, value) in &env {
                cmd.env(key, value);
            }
            cmd.spawn()
        })
        .map_err(LaunchError::SpawnFailed)?;
        let pid = child
            .id()
            .ok_or_else(|| LaunchError::SpawnFailed(std::io::Error::other("no pid")))?;
        Ok(Self::from_child(child, pid, 0))
    }

    fn from_child(mut child: tokio::process::Child, pid: u32, start_ticks: u64) -> Self {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(tokio::io::BufReader::new);
        Self {
            pid,
            start_ticks,
            child,
            stdin,
            stdout,
            partial: Vec::new(),
            discarded: None,
        }
    }

    /// Запись в stdin в срок операции: CLI, переставший читать, не держит вызывающего
    /// и не оставляет половину строки в канале — таймаут виден как `TimedOut`.
    pub async fn write_message_until(
        &mut self,
        message: &serde_json::Value,
        deadline: tokio::time::Instant,
    ) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let Some(stdin) = &mut self.stdin else {
            return Err(std::io::Error::other("no stdin"));
        };
        let line = format!("{message}\n");
        match tokio::time::timeout_at(deadline, async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
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

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }

    pub fn kill_now(&mut self) {
        let _ = self.child.start_kill();
    }

    pub fn member(&self) -> Member {
        Member {
            pid: self.pid,
            start_ticks: self.start_ticks,
        }
    }
}
