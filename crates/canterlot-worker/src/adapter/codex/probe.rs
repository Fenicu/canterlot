use super::rpc::{Incoming, RpcClient, RpcId};
use super::thread::probe_initialize_params;
use crate::adapter::spawn_with_retry_io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

const LINE_CAP: usize = 64 * 1024 * 1024;
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub enum ProbeError {
    SpawnFailed,
    WriteFailed,
    ReadFailed,
    Timeout,
    UnexpectedExit,
    InvalidResponse,
    RpcError(String),
}

pub fn probe_launch_args() -> Vec<String> {
    vec!["app-server".into(), "--listen".into(), "stdio://".into()]
}

// Строка stdout не длиннее LINE_CAP: длинная пропускается до перевода строки,
// не накапливаясь в памяти.
async fn read_line(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
) -> Result<Option<String>, ProbeError> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = timeout(Duration::from_secs(15), reader.fill_buf())
            .await
            .map_err(|_| ProbeError::Timeout)?
            .map_err(|_| ProbeError::ReadFailed)?;
        if available.is_empty() {
            return Ok(None);
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let chunk = newline.unwrap_or(available.len());
        if !oversized {
            if line.len() + chunk > LINE_CAP {
                oversized = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(&available[..chunk]);
            }
        }
        reader.consume(newline.map_or(chunk, |pos| pos + 1));
        if newline.is_some() {
            if std::mem::take(&mut oversized) {
                continue;
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

/// SIGKILL всей группе зонда при любом выходе, кроме чистого завершения:
/// kill_on_drop убивает только обёртку (node), родной сервер — её ребёнок.
struct GroupKill(Option<rustix::process::Pid>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

async fn wait_response(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
    id: i64,
) -> Result<serde_json::Value, ProbeError> {
    loop {
        let Some(line) = read_line(reader).await? else {
            return Err(ProbeError::UnexpectedExit);
        };
        match super::rpc::classify(&line) {
            Incoming::Response {
                id: RpcId::Int(response_id),
                result: Ok(value),
            } if response_id == id => return Ok(value),
            Incoming::Response {
                id: RpcId::Int(response_id),
                result: Err(error),
            } if response_id == id => {
                return Err(ProbeError::RpcError(error.message));
            }
            _ => {}
        }
    }
}

/// Короткоживущий `app-server`: initialize, один запрос, EOF stdin, затем завершение.
pub async fn probe(
    executable: &Path,
    method: &'static str,
    params: serde_json::Value,
    probe_timeout: Duration,
) -> Result<serde_json::Value, ProbeError> {
    let mut pages = run(executable, method, params, probe_timeout, 1).await?;
    pages.pop().ok_or(ProbeError::InvalidResponse)
}

/// То же для постраничного метода: запросы с `cursor` из `nextCursor`
/// до последней страницы (не больше `max_pages`) в одном процессе.
pub async fn probe_pages(
    executable: &Path,
    method: &'static str,
    params: serde_json::Value,
    probe_timeout: Duration,
    max_pages: usize,
) -> Result<Vec<serde_json::Value>, ProbeError> {
    run(executable, method, params, probe_timeout, max_pages).await
}

async fn run(
    executable: &Path,
    method: &'static str,
    mut params: serde_json::Value,
    probe_timeout: Duration,
    max_pages: usize,
) -> Result<Vec<serde_json::Value>, ProbeError> {
    let work = async {
        // Повтор при ETXTBSY: зонд запускает свежезаписанного двойника тестов.
        let mut child = spawn_with_retry_io(|| {
            let mut cmd = Command::new(executable);
            // Как у основного процесса: только отфильтрованное окружение, без ключей биллинга.
            cmd.env_clear()
                .args(probe_launch_args())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .process_group(0);
            for (key, value) in super::process::build_env(
                std::env::vars_os(),
                crate::adapter::InstructionMode::Owner,
                None,
            ) {
                cmd.env(key, value);
            }
            async move { cmd.spawn() }
        })
        .await
        .map_err(|_| ProbeError::SpawnFailed)?;
        let mut group = GroupKill(
            child
                .id()
                .and_then(|id| rustix::process::Pid::from_raw(id as i32)),
        );
        let mut stdin = child.stdin.take().ok_or(ProbeError::SpawnFailed)?;
        let stdout = child.stdout.take().ok_or(ProbeError::SpawnFailed)?;
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut rpc = RpcClient::default();
        let worker_version = env!("CARGO_PKG_VERSION");
        let (init_id, init_msg) =
            rpc.request("initialize", probe_initialize_params(worker_version));
        stdin
            .write_all(format!("{init_msg}\n").as_bytes())
            .await
            .map_err(|_| ProbeError::WriteFailed)?;
        let _init_result = wait_response(&mut reader, init_id).await?;
        let initialized = RpcClient::notification("initialized", serde_json::json!({}));
        stdin
            .write_all(format!("{initialized}\n").as_bytes())
            .await
            .map_err(|_| ProbeError::WriteFailed)?;
        let mut pages = Vec::new();
        loop {
            let (req_id, req_msg) = rpc.request(method, params.clone());
            stdin
                .write_all(format!("{req_msg}\n").as_bytes())
                .await
                .map_err(|_| ProbeError::WriteFailed)?;
            let page = wait_response(&mut reader, req_id).await?;
            let cursor = super::input::next_cursor(&page);
            pages.push(page);
            match cursor {
                Some(cursor) if pages.len() < max_pages => params["cursor"] = cursor.into(),
                _ => break,
            }
        }
        drop(stdin);
        // Чистый выход за SHUTDOWN_WAIT — группу не трогаем; иначе её убивает GroupKill.
        if matches!(timeout(SHUTDOWN_WAIT, child.wait()).await, Ok(Ok(_))) {
            group.0 = None;
        }
        Ok(pages)
    };
    timeout(probe_timeout, work)
        .await
        .map_err(|_| ProbeError::Timeout)?
}
