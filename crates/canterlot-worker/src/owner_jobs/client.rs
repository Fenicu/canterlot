//! `canterlot-worker job run`: отправляет задачу владельца воркеру и, с `--wait`, ждёт итог.
//!
//! Без `--wait` печатает идентификатор задачи и выходит с нулём. С `--wait` печатает хвост
//! журнала в stdout, а каталог результатов и пометку о них — строками `canterlot job: …` в
//! stderr, и выходит с кодом задачи: код команды, 124 по таймауту, 130 при отмене, 1 для
//! `failed` и `lost` без кода. Отказ принять задачу и ошибки разбора аргументов — код 2.
//! Обрыв соединения при ожидании (воркер перезапускается) не теряет итог: команда
//! переподключается и спрашивает итог по тому же идентификатору.

use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use super::{
    ClientMessage, Finished, JobSpec, SOCKET_ENV, SOCKET_NAME, ServerMessage, Source, decode,
    encode,
};

/// Сколько команда терпит недоступный сокет, пока ещё не отправила задачу.
const SUBMIT_PATIENCE: Duration = Duration::from_secs(10);
/// Сколько команда с `--wait` терпит недоступный сокет после обрыва.
const RECONNECT_PATIENCE: Duration = Duration::from_secs(300);
const RETRY_STEP: Duration = Duration::from_millis(250);
const MAX_LINE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct RunArgs {
    pub title: String,
    pub machine: Option<String>,
    pub labels: Vec<String>,
    pub expected_minutes: Option<u32>,
    pub lease: Option<String>,
    pub timeout_minutes: Option<u32>,
    /// `<репозиторий>@<sha>`; `@HEAD` — текущий коммит чистого дерева.
    pub source: Option<String>,
    pub outputs: Vec<String>,
    pub wait: bool,
    pub command: Vec<OsString>,
}

/// Путь сокета: переменная, иначе `$XDG_RUNTIME_DIR/canterlot/jobs.sock`. Окружение
/// исполнения (`CANTERLOT_LEASE_SOCKET`) путь к этому сокету не подсказывает: сессия агента
/// получает сокет аренды, а не сокет задач владельца.
pub fn discover_socket() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty())?;
    Some(PathBuf::from(runtime).join("canterlot").join(SOCKET_NAME))
}

pub async fn run(args: RunArgs) -> ExitCode {
    let code = execute(
        args,
        discover_socket(),
        RECONNECT_PATIENCE,
        &mut io::stdout(),
        &mut io::stderr(),
    )
    .await;
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn note(err: &mut (dyn Write + Send), text: impl AsRef<str>) {
    let _ = writeln!(err, "canterlot job: {}", text.as_ref());
}

/// Всё, что делает команда, с выводом в заданные потоки. Возвращает код выхода.
pub async fn execute(
    args: RunArgs,
    socket: Option<PathBuf>,
    reconnect: Duration,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> i32 {
    let wait = args.wait;
    let spec = match build_spec(args) {
        Ok(spec) => spec,
        Err(message) => {
            note(err, message);
            return 2;
        }
    };
    let Some(socket) = socket else {
        note(
            err,
            "no worker socket known (set CANTERLOT_JOBS_SOCKET or XDG_RUNTIME_DIR)",
        );
        return 2;
    };
    let Some(mut stream) = connect(&socket, SUBMIT_PATIENCE).await else {
        note(
            err,
            format!("the worker socket {} is not reachable", socket.display()),
        );
        return 2;
    };
    let reply = match request(
        &mut stream,
        ClientMessage::Submit {
            spec: Box::new(spec),
        },
    )
    .await
    {
        Ok(reply) => reply,
        Err(error) => {
            note(err, format!("the worker did not answer: {error}"));
            return 2;
        }
    };
    let job_id = match reply {
        ServerMessage::Accepted { job_id } => job_id,
        ServerMessage::Refused { code, message } => {
            note(err, format!("refused: {code}: {message}"));
            return 2;
        }
        ServerMessage::Finished(_) => {
            note(err, "unexpected answer from the worker");
            return 2;
        }
    };
    if !wait {
        let _ = writeln!(out, "{job_id}");
        return 0;
    }
    note(err, format!("{job_id} accepted"));
    let mut stream = Some(stream);
    loop {
        let current = match stream.take() {
            Some(stream) => Some(stream),
            None => connect(&socket, reconnect).await,
        };
        let Some(mut current) = current else {
            note(
                err,
                format!(
                    "the worker is gone; the result stays on its disk, ask again with the same job id {job_id}"
                ),
            );
            return 2;
        };
        match request(
            &mut current,
            ClientMessage::Wait {
                job_id: job_id.clone(),
            },
        )
        .await
        {
            Ok(ServerMessage::Finished(finished)) => return report(&finished, out, err),
            Ok(ServerMessage::Refused { code, message }) => {
                note(err, format!("refused: {code}: {message}"));
                return 2;
            }
            // Обрыв: воркер перезапускается, итог не пропадёт.
            Ok(ServerMessage::Accepted { .. }) | Err(_) => {
                tokio::time::sleep(RETRY_STEP).await;
            }
        }
    }
}

fn report(finished: &Finished, out: &mut (dyn Write + Send), err: &mut (dyn Write + Send)) -> i32 {
    let code = finished.exit_code();
    if !finished.tail.is_empty() {
        let _ = out.write_all(finished.tail.as_bytes());
        if !finished.tail.ends_with('\n') {
            let _ = out.write_all(b"\n");
        }
    }
    note(
        err,
        format!("{} {} (exit {code})", finished.job_id, finished.state),
    );
    if !finished.outputs_dir.is_empty() {
        note(err, format!("outputs: {}", finished.outputs_dir));
    }
    if !finished.outputs_note.is_empty() {
        note(err, &finished.outputs_note);
    }
    code
}

async fn connect(socket: &Path, patience: Duration) -> Option<UnixStream> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        match UnixStream::connect(socket).await {
            Ok(stream) => return Some(stream),
            Err(error) => {
                let transient = matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                );
                if !transient || tokio::time::Instant::now() >= deadline {
                    return None;
                }
                tokio::time::sleep(RETRY_STEP).await;
            }
        }
    }
}

async fn request(stream: &mut UnixStream, message: ClientMessage) -> io::Result<ServerMessage> {
    stream.write_all(encode(message).as_bytes()).await?;
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    let read = (&mut reader).take(MAX_LINE).read_line(&mut line).await?;
    if read == 0 || !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    decode(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Одна строка оболочки: единственный аргумент как есть, несколько — каждый в кавычках.
fn shell_text(command: &[OsString]) -> Result<String, String> {
    let words: Vec<&str> = command
        .iter()
        .map(|word| {
            word.to_str()
                .ok_or_else(|| "the command is not valid UTF-8".to_owned())
        })
        .collect::<Result<_, _>>()?;
    match words.as_slice() {
        [] => Err("a command is required after --".to_owned()),
        [one] => Ok((*one).to_owned()),
        many => Ok(many
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ")),
    }
}

fn quote(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'/' | b'.' | b'_' | b'-' | b'+' | b'@' | b'=' | b':' | b','
                )
        })
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

pub fn build_spec(args: RunArgs) -> Result<JobSpec, String> {
    let shell = shell_text(&args.command)?;
    // Хаб ставит задачу владельца только на подготовленные исходники: рабочего каталога у
    // такой задачи нет, его создаёт носитель.
    let source = args
        .source
        .as_deref()
        .ok_or_else(|| "--source <repo>@<sha> (or @HEAD) is required".to_owned())
        .and_then(resolve_source)?;
    Ok(JobSpec {
        title: args.title,
        // Без названной машины хаб выбирает сам (`auto`).
        machine: args.machine,
        labels: args.labels,
        expected_minutes: args.expected_minutes,
        lease: args.lease,
        timeout_minutes: args.timeout_minutes,
        source: Some(source),
        outputs: args.outputs,
        shell,
    })
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "the path is not valid UTF-8".to_owned())
}

/// Логический текущий каталог (`$PWD`, если он указывает на тот же каталог), иначе физический.
fn current_dir() -> Result<PathBuf, String> {
    let physical =
        std::env::current_dir().map_err(|error| format!("no current directory: {error}"))?;
    if let Some(logical) = std::env::var_os("PWD").map(PathBuf::from)
        && logical.is_absolute()
        && std::fs::canonicalize(&logical).ok().as_deref() == Some(physical.as_path())
    {
        return Ok(logical);
    }
    Ok(physical)
}

/// Абсолютный путь без обращения к диску: `.` и `..` убираются по тексту, ссылки остаются
/// как есть (домен хаб определяет по пути в том виде, как его указал владелец).
fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir()?.join(path)
    };
    let mut clean = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            other => clean.push(other.as_os_str()),
        }
    }
    Ok(clean)
}

fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("git could not be started: {error}"))
}

/// `<репозиторий>@<полный sha>`; `@HEAD` или `<репозиторий>@HEAD` — коммит `HEAD` чистого дерева.
pub fn resolve_source(text: &str) -> Result<Source, String> {
    let (repo, rev) = text
        .rsplit_once('@')
        .ok_or_else(|| "--source is <repo>@<sha> or @HEAD".to_owned())?;
    let repo = if repo.is_empty() {
        current_dir()?
    } else {
        lexical_absolute(Path::new(repo))?
    };
    let top = git(&repo, &["rev-parse", "--show-toplevel"])?;
    let top_path = String::from_utf8_lossy(&top.stdout).trim().to_owned();
    if !top.status.success()
        || std::fs::canonicalize(&top_path).ok() != std::fs::canonicalize(&repo).ok()
    {
        return Err(format!(
            "{} is not the root of a git repository",
            repo.display()
        ));
    }
    let rev = if rev == "HEAD" {
        let status = git(&repo, &["status", "--porcelain"])?;
        if !status.status.success() {
            return Err("git status failed".to_owned());
        }
        if !status.stdout.is_empty() {
            return Err(
                "the working tree is dirty: commit or stash, or pass a full sha instead of HEAD"
                    .to_owned(),
            );
        }
        let head = git(&repo, &["rev-parse", "HEAD"])?;
        if !head.status.success() {
            return Err("the repository has no HEAD commit".to_owned());
        }
        String::from_utf8_lossy(&head.stdout).trim().to_owned()
    } else {
        if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the revision must be a full 40-hex sha (or literally HEAD)".to_owned());
        }
        let known = git(&repo, &["cat-file", "-e", &format!("{rev}^{{commit}}")])?;
        if !known.status.success() {
            return Err(format!("the commit {rev} is not in {}", repo.display()));
        }
        rev.to_ascii_lowercase()
    };
    Ok(Source {
        repo: path_text(&repo)?,
        rev,
    })
}
