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

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

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
        match self.execute_until(remote, input, limit).await? {
            Some(output) => Ok(output),
            // Срок вышел: локальный `ssh` снят вместе с будущим, `Unreachable` прежних
            // вызовов означал и это, и любую другую осечку.
            None => Err(Unreachable),
        }
    }

    /// Как `execute`, но истёкший срок — это `Ok(None)`, а не потеря связи: подготовке окна
    /// нужно отличить одно от другого, чтобы записать код -1.
    async fn execute_until(
        &self,
        remote: &[&str],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Option<Output>, Unreachable> {
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
        let output = match tokio::time::timeout(limit, work).await {
            Ok(output) => output?,
            Err(_) => return Ok(None),
        };
        Ok(Some(Output {
            code: output.status.code().unwrap_or(255),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }))
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

    /// Команда со stdin, срок которой проверяет вызывающий: `Ok(None)` — срок вышел, вывод
    /// скрипта не собран, а локальный `ssh` снят.
    pub async fn until(
        &self,
        remote: &[&str],
        input: Input<'_>,
        limit: Duration,
    ) -> Result<Option<Output>, Unreachable> {
        self.execute_until(remote, input, limit).await
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
        egress: Option<u16>,
    ) -> std::io::Result<tokio::process::Child> {
        let mut command = self.base();
        command
            .args(self.forward_args(socket, target, egress))
            .arg(self.peer());
        spawn_tunnel(command)
    }

    /// Аргументы проброса без адресата. Статическому раннеру — только сокет хаба; облачному
    /// ещё и SOCKS5 на loopback, потому что из облака заблокированные хосты недостижимы.
    fn forward_args(&self, socket: &Path, target: &str, egress: Option<u16>) -> Vec<String> {
        let mut args = vec![
            "-N".to_owned(),
            "-o".to_owned(),
            "ExitOnForwardFailure=yes".to_owned(),
            "-R".to_owned(),
            format!("{}:{target}", socket.display()),
        ];
        if let Some(port) = egress {
            args.push("-R".to_owned());
            args.push(format!("127.0.0.1:{port}"));
        }
        args
    }
}

/// Сколько последних байт диагностики процесса проброса остаётся для лога.
const STDERR_TAIL_BYTES: usize = 2048;

/// Запускает долгоживущий процесс проброса и вычитывает его stderr отдельной задачей.
/// `ssh` пишет туда строку на каждое неудавшееся проброшенное соединение; непрочитанный
/// канал заполняется за 64 КиБ, клиент встаёт в `write`, и проброс висит при живом процессе.
fn spawn_tunnel(mut command: Command) -> std::io::Result<tokio::process::Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let tail = read_tail(stderr).await;
            if !tail.is_empty() {
                tracing::debug!(
                    kind = "ssh_tunnel_stderr",
                    tail = %String::from_utf8_lossy(&tail).trim_end(),
                    "ssh tunnel diagnostics"
                );
            }
        });
    }
    Ok(child)
}

/// Читает поток до конца и возвращает его хвост: память не растёт с объёмом диагностики.
async fn read_tail(mut reader: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut tail = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => return tail,
            Ok(read) => {
                tail.extend_from_slice(&buffer[..read]);
                if tail.len() > STDERR_TAIL_BYTES {
                    tail.drain(..tail.len() - STDERR_TAIL_BYTES);
                }
            }
        }
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
        let args = cloud().forward_args(
            Path::new("/srv/runner/hub.sock"),
            "hub.example:8443",
            Some(EGRESS_SOCKS_PORT),
        );
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

    /// Долгоживущий `ssh -R` пишет строку в stderr на каждое неудавшееся проброшенное
    /// соединение. Канал вмещает 64 КиБ: если его не читать, `ssh` встаёт в `write`, а
    /// проброс висит при живом процессе.
    #[tokio::test]
    async fn a_tunnel_process_is_never_blocked_by_its_own_diagnostics() {
        let mut command = Command::new("sh");
        command.kill_on_drop(true).args([
            "-c",
            "i=0; while [ $i -lt 3000 ]; do \
             echo 'connect_to 192.0.2.1 port 443: failed.' >&2; i=$((i+1)); done",
        ]);
        let mut child = spawn_tunnel(command).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .expect("the process is stuck writing to an unread stderr pipe")
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn only_the_tail_of_the_diagnostics_is_kept() {
        let mut text = vec![b'a'; 100 * 1024];
        text.extend_from_slice(b"the last line");
        let tail = read_tail(text.as_slice()).await;
        assert_eq!(tail.len(), STDERR_TAIL_BYTES);
        assert!(tail.ends_with(b"the last line"));
    }

    #[test]
    fn a_static_tunnel_keeps_its_arguments() {
        let args = static_runner().forward_args(
            Path::new("/srv/runner/hub.sock"),
            "hub.example:8443",
            None,
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
