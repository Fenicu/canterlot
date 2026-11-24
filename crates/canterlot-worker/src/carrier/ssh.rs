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

/// Порт обратного SOCKS5 на облачном раннере: весь его исходящий трафик идёт с аплинка
/// носителя. Должен совпадать с `EGRESS_SOCKS_PORT` хаба (`cloud_init.rs`).
pub const EGRESS_SOCKS_PORT: u16 = 1080;

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
        command.args(self.args()).kill_on_drop(true);
        command
    }

    /// Аргументы `ssh` без адресата: одни и те же для прямого вызова и для транспорта git и
    /// `rsync`.
    fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        match &self.target {
            Target::Alias(_) => {
                if let Some(config) = &self.config {
                    args.push("-F".into());
                    args.push(config.display().to_string());
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
                    args.push("-i".into());
                    args.push(key.display().to_string());
                }
                args.push("-o".into());
                args.push("IdentitiesOnly=yes".into());
                args.push("-o".into());
                args.push(format!("User={user}"));
                args.push("-o".into());
                args.push(format!("Port={port}"));
                args.push("-o".into());
                args.push("StrictHostKeyChecking=yes".into());
                args.push("-o".into());
                args.push(format!("UserKnownHostsFile={}", known_hosts.display()));
                args.push("-o".into());
                args.push("GlobalKnownHostsFile=/dev/null".into());
            }
        }
        for option in [
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
        ] {
            args.push("-o".into());
            args.push(option.into());
        }
        args
    }

    /// Команда `ssh` целиком для `GIT_SSH_COMMAND` и `rsync -e`: тот же адресат и те же опции,
    /// что у прямого вызова. Облачному раннеру нужны свой ключ, порт и закреплённый
    /// `known_hosts`; без них транспорт git не достучался бы до него.
    pub fn command_line(&self) -> String {
        let mut text = String::from("ssh");
        for arg in self.args() {
            text.push(' ');
            text.push_str(&super::stage::sh_quote(&arg));
        }
        text
    }

    /// Адресат scp-подобной записи для git и `rsync`: псевдоним или `user@host` с путём.
    /// Порт облачного раннера задан опцией `Port` в [`Self::command_line`].
    pub fn destination(&self, path: &str) -> String {
        match &self.target {
            Target::Alias(alias) => format!("{alias}:{path}"),
            Target::Host { host, user, .. } => {
                let host = if host.contains(':') && !host.starts_with('[') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                format!("{user}@{host}:{path}")
            }
        }
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
    pub async fn raw(&self, remote: &[&str], limit: Duration) -> Result<Output, Unreachable> {
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
                || output
                    .stderr
                    .contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
                || output.stderr.contains("NO HOST KEY ALGORITHMS"))
    }

    /// Долгоживущий процесс обратного проброса: сокет `socket` на раннере ведёт к `target`
    /// на стороне носителя. Убивается вместе с владельцем. У облачного раннера тем же
    /// процессом поднимается обратный SOCKS5 на его loopback: без него фильтрация в облаке
    /// не пускает ни apt, ни cargo, ни docker. `ExitOnForwardFailure` покрывает оба проброса.
    pub fn forward(
        &self,
        socket: &Path,
        target: &str,
        egress: bool,
    ) -> std::io::Result<tokio::process::Child> {
        let mut command = self.base();
        command
            .args(self.forward_args(socket, target, egress))
            .arg(self.peer())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command.spawn()
    }

    /// Аргументы проброса без адресата. Статическому раннеру — только сокет хаба; облачному
    /// ещё и SOCKS5 на loopback, потому что из облака заблокированные хосты недостижимы.
    fn forward_args(&self, socket: &Path, target: &str, egress: bool) -> Vec<String> {
        let mut args = vec![
            "-N".to_owned(),
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-R".to_owned(),
            format!("{}:{target}", socket.display()),
        ];
        if egress {
            args.push("-R".to_owned());
            args.push(format!("127.0.0.1:{EGRESS_SOCKS_PORT}"));
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(target: Target) -> Ssh {
        Ssh {
            target,
            config: None,
        }
    }

    fn cloud() -> Ssh {
        ssh(Target::Host {
            host: "203.0.113.10".into(),
            port: 22,
            user: "runner".into(),
            identity: None,
            known_hosts: PathBuf::from("/tmp/known_hosts"),
        })
    }

    fn static_runner() -> Ssh {
        ssh(Target::Alias("build-host".into()))
    }

    #[test]
    fn a_cloud_tunnel_also_forwards_the_egress_socks_port() {
        let args =
            cloud().forward_args(Path::new("/srv/runner/hub.sock"), "hub.example:8443", true);
        assert!(
            args.windows(2).any(|pair| pair == ["-R", "127.0.0.1:1080"]),
            "{args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-R", "/srv/runner/hub.sock:hub.example:8443"]),
            "{args:?}"
        );
        assert!(args.iter().any(|arg| arg == "ExitOnForwardFailure=yes"));
    }

    #[test]
    fn a_static_tunnel_keeps_its_arguments() {
        let args = static_runner().forward_args(
            Path::new("/srv/runner/hub.sock"),
            "hub.example:8443",
            false,
        );
        assert_eq!(
            args,
            [
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-R",
                "/srv/runner/hub.sock:hub.example:8443",
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("1080")));
    }
}
