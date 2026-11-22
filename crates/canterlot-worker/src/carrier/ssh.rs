//! Вызовы `ssh` для носителя. Каждый — отдельный процесс с `BatchMode=yes`: без вопросов,
//! паролей и подтверждения ключей хоста; всё, что нужно, лежит в `~/.ssh/config` владельца.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{io::AsyncWriteExt, process::Command};

/// Что подать на stdin удалённой команды.
pub enum Input<'a> {
    None,
    /// Скрипт или небольшое содержимое файла.
    Bytes(&'a [u8]),
    /// Файл целиком (бинарник воркера).
    File(&'a Path),
}

pub struct Output {
    /// Код выхода удалённой команды.
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Связи нет: `ssh` не запустился, не уложился в срок или сам вышел с 255.
#[derive(Debug)]
pub struct Unreachable;

#[derive(Clone, Debug)]
pub struct Ssh {
    alias: String,
    config: Option<PathBuf>,
}

impl Ssh {
    pub fn new(alias: impl Into<String>, config: Option<PathBuf>) -> Self {
        Self {
            alias: alias.into(),
            config,
        }
    }

    fn base(&self) -> Command {
        let mut command = Command::new("ssh");
        if let Some(config) = &self.config {
            command.arg("-F").arg(config);
        }
        command
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ConnectTimeout=10"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(["-o", "ServerAliveCountMax=3"])
            .kill_on_drop(true);
        command
    }

    /// Выполняет `remote` на раннере. Нулевой код не требуется: отказ команды — это
    /// ответ раннера, а `Err` — только потеря связи.
    pub async fn run(
        &self,
        remote: &[&str],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Output, Unreachable> {
        let mut command = self.base();
        command.arg("-T").arg(&self.alias).args(remote);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let bytes = match input {
            Input::None => {
                command.stdin(Stdio::null());
                None
            }
            Input::Bytes(bytes) => {
                command.stdin(Stdio::piped());
                Some(bytes)
            }
            Input::File(path) => {
                let file = tokio::fs::File::open(path).await.map_err(|_| Unreachable)?;
                command.stdin(Stdio::from(file.into_std().await));
                None
            }
        };
        let work = async {
            let mut child = command.spawn().map_err(|_| Unreachable)?;
            if let Some(bytes) = bytes {
                let mut stdin = child.stdin.take().ok_or(Unreachable)?;
                // Раннер мог закрыть stdin раньше времени (например, `cat` упал): итог скажет код.
                let _ = stdin.write_all(bytes).await;
                drop(stdin);
            }
            child.wait_with_output().await.map_err(|_| Unreachable)
        };
        let output = tokio::time::timeout(limit, work)
            .await
            .map_err(|_| Unreachable)??;
        let code = output.status.code().unwrap_or(255);
        if code == 255 {
            return Err(Unreachable);
        }
        Ok(Output {
            code,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Скрипт для `sh -s` через stdin.
    pub async fn script(&self, script: &str, limit: Duration) -> Result<Output, Unreachable> {
        self.run(&["sh", "-s"], Input::Bytes(script.as_bytes()), limit)
            .await
    }

    /// Долгоживущий процесс обратного проброса: сокет `socket` на раннере ведёт к `target`
    /// на стороне носителя. Убивается вместе с владельцем.
    pub fn forward(&self, socket: &Path, target: &str) -> std::io::Result<tokio::process::Child> {
        let mut command = self.base();
        command
            .arg("-N")
            .args(["-o", "ExitOnForwardFailure=yes"])
            .arg("-R")
            .arg(format!("{}:{target}", socket.display()))
            .arg(&self.alias)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command.spawn()
    }
}
