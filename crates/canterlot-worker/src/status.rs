//! `canterlot-worker status --json`: состояние работающего воркера для утилиты `canterlot`
//! (join, обновление, удаление). Только чтение: запрос `worker_status` по сокету аренды,
//! тому же, через который идёт `lease status`.

use std::{
    fmt,
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use crate::lease::{ClientMessage, ServerMessage, decode, encode};

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct WorkerStatusView {
    pub version: String,
    /// Последнее рукопожатие принято хабом и сеанс жив.
    pub connected: bool,
    /// Время последнего принятого рукопожатия (RFC 3339).
    pub connected_since: Option<String>,
    /// Версия хаба из последнего рукопожатия; у хабов старше поля — пусто.
    pub hub_version: Option<String>,
    pub executions: Vec<ExecutionStatusView>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ExecutionStatusView {
    pub id: String,
    /// Воркер держит связь с сопровождением этого исполнения.
    pub attached: bool,
    /// `/proc/<pid>/exe` сопровождения; ` (deleted)` в конце значит, что файла уже нет.
    pub supervisor_exe: Option<String>,
}

/// Источник снимка для сокета: собирается из состояния сеанса и менеджера исполнений.
#[derive(Clone)]
pub struct StatusSource(Arc<dyn Fn() -> WorkerStatusView + Send + Sync>);

impl StatusSource {
    pub fn new(source: impl Fn() -> WorkerStatusView + Send + Sync + 'static) -> Self {
        Self(Arc::new(source))
    }

    pub fn snapshot(&self) -> WorkerStatusView {
        (self.0)()
    }
}

impl fmt::Debug for StatusSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StatusSource")
    }
}

/// Путь бинаря процесса, если его время старта совпадает: pid мог достаться другому.
pub fn process_exe(pid: u32, start_ticks: u64) -> Option<String> {
    let stat = crate::containment::read_stat(format!("/proc/{pid}/stat")).ok()?;
    let end = stat.rfind(')')?;
    let ticks: u64 = stat[end + 1..].split_whitespace().nth(19)?.parse().ok()?;
    if ticks != start_ticks {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn socket_for(config: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(config)
        .map_err(|error| format!("read {}: {error}", config.display()))?;
    let data_dir = crate::fatal::data_dir_hint(&text)
        .ok_or_else(|| format!("{}: no absolute data_dir", config.display()))?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    Ok(crate::lease::socket_path(runtime.as_deref(), &data_dir))
}

async fn query(socket: &Path) -> Result<WorkerStatusView, String> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|error| format!("worker unreachable ({}: {error})", socket.display()))?;
    let (read, mut write) = stream.into_split();
    write
        .write_all(encode(ClientMessage::WorkerStatus).as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    let mut lines = BufReader::new(read).lines();
    let line = tokio::time::timeout(REPLY_TIMEOUT, lines.next_line())
        .await
        .map_err(|_| format!("worker did not answer within {}s", REPLY_TIMEOUT.as_secs()))?
        .map_err(|error| error.to_string())?
        .ok_or("worker closed the connection")?;
    match decode::<ServerMessage>(&line)? {
        ServerMessage::WorkerStatus(view) => Ok(view),
        ServerMessage::Error { reason } => Err(reason),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

pub async fn run(config: &Path, json: bool) -> ExitCode {
    let result = match socket_for(config) {
        Ok(socket) => query(&socket).await,
        Err(error) => Err(error),
    };
    let view = match result {
        Ok(view) => view,
        Err(error) => {
            eprintln!("canterlot-worker status: {error}");
            return ExitCode::FAILURE;
        }
    };
    let text = if json {
        let mut line = serde_json::to_string(&view).unwrap_or_default();
        line.push('\n');
        line
    } else {
        let mut text = format!(
            "version {}\nconnected {}\nhub {}\n",
            view.version,
            view.connected,
            view.hub_version.as_deref().unwrap_or("-")
        );
        for exec in &view.executions {
            text.push_str(&format!(
                "execution {} attached={}\n",
                exec.id, exec.attached
            ));
        }
        text
    };
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
