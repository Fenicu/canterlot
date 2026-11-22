//! Вызовы `ssh` для носителя. Каждый — отдельный процесс с `BatchMode=yes`: без вопросов,
//! паролей и подтверждения ключей; всё, что нужно, лежит в `~/.ssh/config` владельца.
//!
//! Облачный раннер не использует псевдоним: к нему идут по адресу и DNAT-порту из команды хаба,
//! ключу `[runners].ssh_key` и персональному `known_hosts` с закреплённым ключом хоста.

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

/// Куда подключаться: псевдоним из `ssh_config` или явный адрес облачного сервера.
#[derive(Clone, Debug)]
pub enum Target {
    Alias(String),
    Host {
        host: String,
        port: u16,
        user: String,
        identity: Option<PathBuf>,
        known_hosts: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub struct Ssh {
    target: Target,
    config: Option<PathBuf>,
}

impl Ssh {
    pub fn new(alias: impl Into<String>, config: Option<PathBuf>) -> Self {
        Self {
            target: Target::Alias(alias.into()),
            config,
        }
    }

    pub fn host(
        host: String,
        port: u16,
        user: String,
        identity: Option<PathBuf>,
        known_hosts: PathBuf,
    ) -> Self {
        Self {
            target: Target::Host {
                host,
                port,
                user,
                identity,
                known_hosts,
            },
            config: None,
        }
    }

    fn base(&self) -> Command {
        let mut command = Command::new("ssh");
        match &self.target {
            Target::Alias(_) => {
                if let Some(config) = &self.config {
                    command.arg("-F").arg(config);
                }
            }
            Target::Host {
                port,
                user,
                identity,
                known_hosts,
                ..
            } => {
                if let Some(key) = identity {
                    command.arg("-i").arg(key);
                }
                command
                    .args(["-o", "IdentitiesOnly=yes"])
                    .args(["-o", &format!("User={user}")])
                    .args(["-o", &format!("Port={port}")])
                    .args(["-o", "StrictHostKeyChecking=yes"])
                    .args(["-o", &format!("UserKnownHostsFile={}", known_hosts.display())])
                    .args(["-o", "GlobalKnownHostsFile=/dev/null"]);
            }
        }
        command
            .args(["-o", "BatchMode=yes"])
            .args(["-o", "ConnectTimeout=10"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(["-o", "ServerAliveCountMax=3"])
            .kill_on_drop(true);
        command
    }

    fn peer(&self) -> &str {
        match &self.target {
            Target::Alias(alias) => alias,
            Target::Host { host, .. } => host,
        }
    }

    async fn execute(
        &self,
        remote: &[&str],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Output, Unreachable> {
        let mut command = self.base();
        command.arg("-T").arg(self.peer()).args(remote);
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
        Ok(Output {
            code: output.status.code().unwrap_or(255),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Выполняет `remote` на раннере. Нулевой код не требуется: отказ команды — это
    /// ответ раннера, а `Err` — только потеря связи.
    pub async fn run(
        &self,
        remote: &[&str],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Output, Unreachable> {
        let output = self.execute(remote, input, limit).await?;
        if output.code == 255 {
            return Err(Unreachable);
        }
        Ok(output)
    }

    /// Как `run`, но код 255 остаётся в ответе: облачный носитель отличает отказ хостового
    /// ключа от других причин недоступности по тексту `stderr`.
    pub async fn raw(
        &self,
        remote: &[&str],
        limit: Duration,
    ) -> Result<Output, Unreachable> {
        self.execute(remote, Input::None, limit).await
    }

    /// Скрипт для `sh -s` через stdin.
    pub async fn script(&self, script: &str, limit: Duration) -> Result<Output, Unreachable> {
        self.run(&["sh", "-s"], Input::Bytes(script.as_bytes()), limit)
            .await
    }

    /// Причина отказа ключа хоста: `StrictHostKeyChecking=yes` не предлагает принять новый.
    pub fn host_key_rejected(output: &Output) -> bool {
        output.code == 255
            && (output.stderr.contains("Host key verification failed")
                || output.stderr.contains("Host key verification failed.")
                || output.stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
                || output.stderr.contains("NO HOST KEY ALGORITHMS"))
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
            .arg(self.peer())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command.spawn()
    }
}
