//! Каталоги, окружение и процесс `opencode serve --stdio`.

use crate::{
    adapter::{reaped, spawn_with_retry_io},
    config::OpenCodeConfig,
    containment::Containment,
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// Ровно это наследуется от воркера: ключи провайдеров и служебные переменные отрезаны.
const INHERITED_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "TERM",
    "XDG_CONFIG_HOME",
    "CARGO_BUILD_JOBS",
];

/// Каталоги canterlot для OpenCode. База, журнал и файлы shell лежат здесь:
/// база владельца не годится, сервер на ней подхватит чужие исполнения.
#[derive(Clone, Debug)]
pub struct DataLayout {
    root: PathBuf,
}

#[derive(Debug)]
pub enum LayoutError {
    Refused,
    Io(std::io::Error),
}

impl DataLayout {
    /// Корень для запуска при старте воркера: `<data_dir>/opencode`.
    pub fn for_worker(cfg: &OpenCodeConfig, data_dir: &Path) -> Self {
        Self {
            root: cfg
                .data_root
                .clone()
                .unwrap_or_else(|| data_dir.join("opencode")),
        }
    }

    /// Корень для исполнения: каталог исполнения лежит в `<data_dir>/executions/<id>`.
    pub fn for_execution(cfg: &OpenCodeConfig, exec_dir: &Path) -> Self {
        let mut ancestors = exec_dir.ancestors().skip(1);
        let data_dir = match (ancestors.next(), ancestors.next()) {
            (Some(executions), Some(data))
                if executions
                    .file_name()
                    .is_some_and(|name| name == "executions") =>
            {
                data
            }
            _ => exec_dir,
        };
        Self::for_worker(cfg, data_dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn data_home(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn state_home(&self) -> PathBuf {
        self.root.join("state")
    }

    pub fn cache_home(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn create(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        for dir in [
            self.data_home(),
            self.state_home(),
            self.cache_home(),
            self.tmp(),
        ] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Ok(())
    }
}

fn env_value<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsStr> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_os_str())
        .filter(|value| !value.is_empty())
}

// Симлинк не должен скрывать совпадение: путь сравнивается по самому длинному
// существующему предку.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    let mut tail = Vec::new();
    let mut current = path;
    loop {
        match std::fs::canonicalize(current) {
            Ok(mut found) => {
                found.extend(tail.iter().rev());
                return Ok(found);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                    return Ok(path.to_owned());
                };
                tail.push(name);
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// База canterlot не может лежать в данных владельца, совпадать с ними или содержать их.
pub fn check_root(
    layout: &DataLayout,
    cfg: &OpenCodeConfig,
    parent: &[(OsString, OsString)],
) -> Result<(), LayoutError> {
    let mut refused: Vec<PathBuf> = cfg.refuse_paths.clone();
    if let Some(data) = env_value(parent, "XDG_DATA_HOME") {
        refused.push(Path::new(data).join("opencode"));
    }
    if let Some(home) = env_value(parent, "HOME") {
        refused.push(Path::new(home).join(".local/share/opencode"));
    }
    let root = resolved(&layout.root).map_err(LayoutError::Io)?;
    let database = resolved(&layout.data_home().join("opencode")).map_err(LayoutError::Io)?;
    for path in refused {
        let path = resolved(&path).map_err(LayoutError::Io)?;
        if root.starts_with(&path) || database.starts_with(&path) || path.starts_with(&database) {
            return Err(LayoutError::Refused);
        }
    }
    Ok(())
}

/// Общая часть окружения сервера и проверки версии: `--version` тоже заводит каталоги и журнал.
pub fn base_env(
    parent: impl Iterator<Item = (OsString, OsString)>,
    layout: &DataLayout,
    tmp: &Path,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = parent
        .filter(|(key, _)| INHERITED_ENV.iter().any(|name| key == name))
        .collect();
    env.extend([
        ("XDG_DATA_HOME".into(), layout.data_home().into_os_string()),
        (
            "XDG_STATE_HOME".into(),
            layout.state_home().into_os_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            layout.cache_home().into_os_string(),
        ),
        ("TMPDIR".into(), tmp.as_os_str().to_owned()),
        ("OPENCODE_DISABLE_AUTOUPDATE".into(), "1".into()),
    ]);
    env
}

#[derive(Debug)]
pub enum LaunchError {
    Spawn(std::io::Error),
    NotInContainment,
    Inspect,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadyError {
    /// Строки нет: срок вышел или процесс завершился.
    NoLine,
    Malformed,
    NotLoopback,
}

/// `{"url":"http://127.0.0.1:PORT"}`: только loopback и только HTTP. Пароль уходит на
/// этот адрес, поэтому чужой хост недопустим.
pub fn parse_ready(line: &str) -> Result<String, ReadyError> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|_| ReadyError::Malformed)?;
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or(ReadyError::Malformed)?;
    let url = url::Url::parse(url).map_err(|_| ReadyError::Malformed)?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http" || !loopback || !url.username().is_empty() || url.password().is_some()
    {
        return Err(ReadyError::NotLoopback);
    }
    let port = url.port().ok_or(ReadyError::Malformed)?;
    let host = url.host_str().ok_or(ReadyError::Malformed)?;
    Ok(format!("http://{host}:{port}"))
}

/// Задача, которая не переживает владельца.
struct Drain(tokio::task::JoinHandle<()>);

impl Drop for Drain {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn drain<R>(mut reader: R) -> Drain
where
    R: AsyncRead + Unpin + Send + 'static,
{
    Drain(tokio::spawn(async move {
        let mut sink = [0u8; 4096];
        while matches!(reader.read(&mut sink).await, Ok(count) if count > 0) {}
    }))
}

/// Процесс сервера. stdin держится открытым всё время жизни: его закрытие, в том числе
/// при смерти супервизора, завершает сервер вместе с его shell.
pub struct ServerProcess {
    pub pid: u32,
    pub start_ticks: u64,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    stdout_drain: Option<Drain>,
    _stderr_drain: Option<Drain>,
    finished: bool,
}

const MAX_READY_LINE: u64 = 64 * 1024;

fn command(executable: &Path, workdir: &Path, env: &[(OsString, OsString)]) -> Command {
    let mut cmd = Command::new(executable);
    cmd.env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .args(["serve", "--stdio", "--port", "0"])
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    cmd
}

impl ServerProcess {
    pub async fn spawn(
        executable: &Path,
        workdir: &Path,
        env: &[(OsString, OsString)],
        group: &dyn Containment,
    ) -> Result<Self, LaunchError> {
        let child = spawn_with_retry_io(|| {
            let mut cmd = command(executable, workdir, env);
            group.prepare_command(&mut cmd);
            async move { cmd.spawn() }
        })
        .await
        .map_err(LaunchError::Spawn)?;
        let pid = child
            .id()
            .ok_or(LaunchError::Spawn(std::io::Error::other("no pid")))?;
        let member = group
            .members()
            .map_err(|_| LaunchError::Inspect)?
            .into_iter()
            .find(|member| member.pid == pid)
            .ok_or(LaunchError::NotInContainment)?;
        group.register_cli(&member);
        Ok(Self::owning(child, pid, member.start_ticks))
    }

    /// Короткий запуск в процессе воркера для чтения каталога: сдерживание принадлежит
    /// супервизорам исполнений, здесь хватает своей группы процессов.
    pub async fn spawn_probe(
        executable: &Path,
        workdir: &Path,
        env: &[(OsString, OsString)],
    ) -> Result<Self, LaunchError> {
        let child = spawn_with_retry_io(|| {
            let mut cmd = command(executable, workdir, env);
            cmd.process_group(0);
            async move { cmd.spawn() }
        })
        .await
        .map_err(LaunchError::Spawn)?;
        let pid = child
            .id()
            .ok_or(LaunchError::Spawn(std::io::Error::other("no pid")))?;
        Ok(Self::owning(child, pid, 0))
    }

    fn owning(mut child: Child, pid: u32, start_ticks: u64) -> Self {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        let stderr = child.stderr.take().map(drain);
        Self {
            pid,
            start_ticks,
            child,
            stdin,
            stdout,
            stdout_drain: None,
            _stderr_drain: stderr,
            finished: false,
        }
    }

    /// Первая строка stdout; дальше сервер в stdout не пишет, но вычитывается до конца.
    pub async fn ready_url(&mut self, limit: Duration) -> Result<String, ReadyError> {
        let mut stdout = self.stdout.take().ok_or(ReadyError::NoLine)?;
        let mut line = Vec::new();
        let read = tokio::time::timeout(
            limit,
            (&mut stdout)
                .take(MAX_READY_LINE)
                .read_until(b'\n', &mut line),
        )
        .await;
        self.stdout_drain = Some(drain(stdout));
        match read {
            Ok(Ok(count)) if count > 0 && line.ends_with(b"\n") => {
                parse_ready(String::from_utf8_lossy(&line).trim())
            }
            Ok(Ok(count)) if count > 0 => Err(ReadyError::Malformed),
            _ => Err(ReadyError::NoLine),
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// Сигнал группе сервера. После выхода PID мог достаться другому процессу.
    pub fn signal(&self, signal: rustix::process::Signal) {
        if self.finished {
            return;
        }
        if let Some(pid) = i32::try_from(self.pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, signal);
        }
    }

    /// Выход процесса; `None` — статус уже забрала проверка сдерживания.
    pub async fn wait(&mut self) -> Option<std::process::ExitStatus> {
        let status = match self.child.wait().await {
            Ok(status) => Some(status),
            Err(error) if reaped(&error) => None,
            Err(_) => None,
        };
        self.finished = true;
        status
    }

    pub fn finished(&self) -> bool {
        self.finished
    }
}

impl Drop for ServerProcess {
    // Брошенный адаптер не оставляет сервера: EOF stdin и SIGTERM дают ему снять свои shell.
    fn drop(&mut self) {
        self.close_stdin();
        self.signal(rustix::process::Signal::TERM);
    }
}
