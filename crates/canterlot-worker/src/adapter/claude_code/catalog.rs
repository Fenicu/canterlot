//! Проба каталога команд Claude Code без хода модели: `control_request initialize`
//! в stream-json. Запуск с теми же флагами изоляции, что и сессия воркера.

use super::process::build_env;
use crate::machine::slash_catalog::{CatalogEntry, parse_claude};
use rustix::process::{Pid, Signal, kill_process_group};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

const PROBE_WAIT: Duration = Duration::from_secs(15);
const LINE_CAP: u64 = 2 * 1024 * 1024;

/// Группа короткого процесса: kill_on_drop убивает только обёртку, не её ребёнка.
struct GroupKill(Option<Pid>);

impl GroupKill {
    fn of(child: &tokio::process::Child) -> Self {
        Self(
            child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .and_then(Pid::from_raw),
        )
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
}

/// Ответ `initialize` содержит `commands` (команды и навыки) и не запускает ход.
pub async fn catalog_probe(executable: &Path) -> Result<Vec<CatalogEntry>, String> {
    let work = async {
        let mut cmd = Command::new(executable);
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--strict-mcp-config",
            "--settings",
            r#"{"permissions":{"allow":[]}}"#,
            "--system-prompt-snapshot",
            "off",
        ])
        .envs(build_env(std::env::vars_os()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let mut group = GroupKill::of(&child);
        let mut stdin = child.stdin.take().ok_or("no_stdin")?;
        let stdout = child.stdout.take().ok_or("no_stdout")?;
        let mut reader = BufReader::new(stdout).lines();
        let request = r#"{"type":"control_request","request_id":"canterlot-catalog","request":{"subtype":"initialize","params":{}}}
"#;
        stdin
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        drop(stdin);
        let mut output = String::new();
        loop {
            let line = reader
                .next_line()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("early_exit")?;
            output.push_str(&line);
            output.push('\n');
            if line.contains("\"type\":\"control_response\"") {
                break;
            }
            if output.len() as u64 > LINE_CAP {
                return Err("oversized".into());
            }
        }
        group.disarm();
        let _ = child.kill().await;
        Ok(parse_claude(&output))
    };
    timeout(PROBE_WAIT, work)
        .await
        .map_err(|_| "timeout".to_owned())?
}
