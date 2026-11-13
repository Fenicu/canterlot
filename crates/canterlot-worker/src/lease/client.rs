//! `canterlot-worker lease run` и `lease status`.
//!
//! Аренда никогда не делает сборку невозможной: любая ошибка связи или протокола —
//! одна строка в stderr и запуск команды без аренды.
//!
//! Команда остаётся в сессии обёртки и, когда терминал принадлежит группе обёртки, в её
//! группе процессов: Ctrl-C, Ctrl-Z и убийство группы достаются обоим, а INT и QUIT
//! обёртка тогда не пересылает — группа их уже получила. В остальных случаях (исполнения
//! агента, скрипты, фоновые задания) команда получает свою группу процессов: групповой
//! сигнал достаётся одной обёртке и приходит команде ровно один раз — пересланным. TERM и
//! HUP пересылаются всегда, INT и QUIT — только в этой, отдельной группе. Код выхода —
//! код команды; умершая от сигнала команда — тот же сигнал на обёртке.

use std::{
    ffi::OsString,
    io::{self, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

use rustix::process::Signal;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{UnixStream, unix::OwnedReadHalf, unix::OwnedWriteHalf},
    signal::unix::{Signal as SignalStream, SignalKind, signal},
};

use super::{
    BUDGET_ENV, ClientMessage, EntryView, HELD_ENV, Kind, ServerMessage, Snapshot, UNLEASED,
    decode, encode,
};
use crate::containment::LEASE_SOCKET_ENV;

/// Сокет есть, но не отвечает (воркер перезапускается): столько ждём перед запуском без аренды.
const REFUSED_RETRY: Duration = Duration::from_secs(10);
/// Переподключение после обрыва: держатель и ожидающий пробуют не дольше.
const RECONNECT_TOTAL: Duration = Duration::from_secs(30);
const RETRY_STEP: Duration = Duration::from_millis(250);
/// Сокет принял соединение, но не ответил на запрос: как недоступный воркер, без аренды.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Первый ответ на `acquire`: сервер сохраняет состояние с `sync_all` под мьютексом,
/// на заторможенном диске десяти секунд мало, и ожидающие обёртки разом ушли бы без
/// аренды. Тест обёртки сокращает срок переменной окружения.
const FIRST_REPLY_TIMEOUT: Duration = Duration::from_secs(60);
const FIRST_REPLY_TIMEOUT_ENV: &str = "CANTERLOT_LEASE_REPLY_TIMEOUT_MS";

#[derive(Debug, Clone)]
pub struct RunArgs {
    pub kind: Kind,
    pub label: Option<String>,
    pub command: Vec<OsString>,
}

fn note(text: impl AsRef<str>) {
    let _ = writeln!(io::stderr(), "canterlot lease: {}", text.as_ref());
}

fn first_reply_timeout() -> Duration {
    std::env::var(FIRST_REPLY_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(FIRST_REPLY_TIMEOUT)
}

/// Маска игнорируемых сигналов, унаследованная от родителя: `nohup` и фоновые задания
/// ставят SIG_IGN. Перехват tokio перебил бы её, и `exec` вернул бы команде SIG_DFL.
fn inherited_ignored() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("SigIgn:"))
                .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        })
        .unwrap_or(0)
}

fn initially_ignored(mask: u64, sig: Signal) -> bool {
    let raw = u32::try_from(sig.as_raw()).unwrap_or(0);
    (1..=64).contains(&raw) && mask & (1 << (raw - 1)) != 0
}

/// Обработчик ставится только для сигналов, которые родитель не игнорировал:
/// унаследованный SIG_IGN должен дойти и до команды.
fn watch(kind: SignalKind, sig: Signal, ignored: u64) -> io::Result<Option<SignalStream>> {
    if initially_ignored(ignored, sig) {
        Ok(None)
    } else {
        signal(kind).map(Some)
    }
}

/// Ждёт сигнал; у унаследованного SIG_IGN обработчика нет — не ждём никогда.
async fn caught(signal: &mut Option<SignalStream>) {
    match signal {
        Some(signal) => {
            let _ = signal.recv().await;
        }
        None => std::future::pending::<()>().await,
    }
}

fn in_foreground(pgrp: rustix::process::Pid, fd: impl std::os::fd::AsFd) -> bool {
    rustix::termios::isatty(&fd) && rustix::termios::tcgetpgrp(&fd).is_ok_and(|owner| owner == pgrp)
}

/// Терминал принадлежит группе обёртки: она передняя, и Ctrl-C, Ctrl-Z и `tcsetpgrp`
/// команды достаются ей же, как обычному процессу в терминале.
fn owns_terminal() -> bool {
    let pgrp = rustix::process::getpgrp();
    in_foreground(pgrp, io::stdin())
        || in_foreground(pgrp, io::stdout())
        || in_foreground(pgrp, io::stderr())
}

/// Забирает уже доставленный сигнал, не дожидаясь нового.
async fn pending(signal: &mut Option<SignalStream>) -> bool {
    match signal {
        None => false,
        Some(signal) => tokio::select! {
            biased;
            _ = signal.recv() => true,
            _ = std::future::ready(()) => false,
        },
    }
}

/// Путь сокета: переменная исполнения, иначе `$XDG_RUNTIME_DIR/canterlot/lease.sock`.
pub fn discover_socket() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(LEASE_SOCKET_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|p| !p.is_empty())
        .map(|runtime| {
            PathBuf::from(runtime)
                .join("canterlot")
                .join(super::SOCKET_NAME)
        })
}

enum Connect {
    Stream(UnixStream),
    Unreachable(String),
}

/// Любая ошибка подключения (в том числе EPERM/EACCES из песочницы) — недоступность.
/// Отказ существующего сокета (воркер перезапускается) повторяется до `patience`;
/// отсутствующий сокет — только при переподключении (`retry_missing`), когда новый
/// воркер может как раз заменять файл.
async fn connect(path: &std::path::Path, patience: Duration, retry_missing: bool) -> Connect {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        match UnixStream::connect(path).await {
            Ok(stream) => return Connect::Stream(stream),
            Err(error) => {
                let transient = error.kind() == io::ErrorKind::ConnectionRefused
                    || (retry_missing && error.kind() == io::ErrorKind::NotFound);
                if !transient || tokio::time::Instant::now() + RETRY_STEP >= deadline {
                    return Connect::Unreachable(format!("{}: {error}", path.display()));
                }
                tokio::time::sleep(RETRY_STEP).await;
            }
        }
    }
}

/// Запуск команды вместо этого процесса (тот же pid): без аренды или внутри чужой.
fn exec_direct(command: &[OsString], held: Option<&str>) -> ExitCode {
    let mut process = std::process::Command::new(&command[0]);
    process.args(&command[1..]);
    if let Some(value) = held {
        process.env(HELD_ENV, value);
    }
    let error = process.exec();
    spawn_failure(&command[0], &error)
}

fn spawn_failure(program: &OsString, error: &io::Error) -> ExitCode {
    note(format!("{}: {error}", program.to_string_lossy()));
    if error.kind() == io::ErrorKind::NotFound {
        ExitCode::from(127)
    } else {
        ExitCode::from(126)
    }
}

fn unwrapped(command: &[OsString], reason: &str) -> ExitCode {
    note(format!("{reason}, running without lease"));
    exec_direct(command, Some(UNLEASED))
}

type Reader = Lines<BufReader<OwnedReadHalf>>;

async fn read_message(lines: &mut Reader) -> Result<ServerMessage, String> {
    match lines.next_line().await {
        Ok(Some(line)) => decode::<ServerMessage>(&line),
        Ok(None) => Err("connection closed".into()),
        Err(error) => Err(error.to_string()),
    }
}

async fn write_message(write: &mut OwnedWriteHalf, message: ClientMessage) -> io::Result<()> {
    write.write_all(encode(message).as_bytes()).await
}

struct Granted {
    lease_id: String,
    kind: Kind,
    budget: u32,
    playwright_workers: u32,
    lines: Reader,
    write: OwnedWriteHalf,
}

enum Wait {
    Granted(Granted),
    /// Обрыв посреди ожидания: переподключиться с тем же временем запроса. `true` —
    /// воркер успел ответить по этому соединению, отсчёт переподключения начинается заново.
    Lost(String, bool),
    /// Ошибка протокола или отказ: без аренды.
    Failed(String),
}

fn waiting_line(kind: Kind, position: u32, holders: &[EntryView]) -> String {
    let holders = holders
        .iter()
        .map(EntryView::describe)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "waiting for {}, position {position} (holders: {holders})",
        kind.as_str()
    )
}

async fn request(
    stream: UnixStream,
    kind: Kind,
    label: &str,
    dir: &str,
    requested_at_ms: u64,
    last_line: &mut Option<String>,
) -> Wait {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let acquire = ClientMessage::Acquire {
        kind,
        label: label.to_owned(),
        dir: dir.to_owned(),
        requested_at_ms,
    };
    if let Err(error) = write_message(&mut write, acquire).await {
        return Wait::Lost(error.to_string(), false);
    }
    let mut answered = false;
    let reply_timeout = first_reply_timeout();
    loop {
        // Ждём только первый ответ: очередь может стоять сколько угодно.
        let reply = if answered {
            read_message(&mut lines).await
        } else {
            match tokio::time::timeout(reply_timeout, read_message(&mut lines)).await {
                Ok(reply) => reply,
                Err(_) => {
                    return Wait::Failed(format!(
                        "worker did not answer within {}s",
                        reply_timeout.as_secs_f64()
                    ));
                }
            }
        };
        match reply {
            Ok(ServerMessage::Queued {
                position, holders, ..
            }) => {
                answered = true;
                let line = waiting_line(kind, position, &holders);
                if last_line.as_deref() != Some(line.as_str()) {
                    note(&line);
                    *last_line = Some(line);
                }
            }
            Ok(ServerMessage::Granted {
                lease_id,
                kind,
                budget,
                playwright_workers,
                ..
            }) => {
                return Wait::Granted(Granted {
                    lease_id,
                    kind,
                    budget,
                    playwright_workers,
                    lines,
                    write,
                });
            }
            Ok(ServerMessage::Error { reason }) => return Wait::Failed(reason),
            Ok(other) => return Wait::Failed(format!("unexpected reply {other:?}")),
            Err(reason) if reason == "connection closed" => return Wait::Lost(reason, answered),
            Err(reason) => return Wait::Failed(reason),
        }
    }
}

/// Не больше уже заданного меньшего значения в окружении.
fn capped(name: &str, value: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|&v| v > 0)
        .map_or(value, |inherited| inherited.min(value))
}

/// Ограничения ресурсов для команды под арендой.
pub fn cap_env(
    lease_id: &str,
    budget: u32,
    playwright_workers: u32,
) -> Vec<(&'static str, String)> {
    vec![
        (HELD_ENV, lease_id.to_owned()),
        (BUDGET_ENV, budget.to_string()),
        ("CANTERLOT_TEST_THREADS", budget.to_string()),
        (
            "CARGO_BUILD_JOBS",
            capped("CARGO_BUILD_JOBS", budget).to_string(),
        ),
        (
            "PW_WORKERS",
            capped("PW_WORKERS", playwright_workers).to_string(),
        ),
    ]
}

fn current_dir_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

pub async fn run(args: RunArgs) -> ExitCode {
    if args.command.is_empty() {
        note("no command given");
        return ExitCode::from(2);
    }
    if let Ok(held) = std::env::var(HELD_ENV)
        && !held.is_empty()
    {
        note(format!(
            "already inside lease {held}, running {} directly",
            args.kind.as_str()
        ));
        return exec_direct(&args.command, None);
    }
    let Some(socket) = discover_socket() else {
        return unwrapped(&args.command, "no lease socket known");
    };
    // Сигналы ловятся с самого начала: пока ждём, они завершают обёртку тем же сигналом,
    // а после запуска команды INT и QUIT игнорируются. Перехваченный (не игнорируемый)
    // сигнал при exec возвращается к умолчанию, поэтому команда получает обычные.
    // Унаследованный SIG_IGN не перехватываем: так он доживает до команды.
    let ignored = inherited_ignored();
    let (Ok(mut sigint), Ok(mut sigquit), Ok(mut sigterm), Ok(mut sighup)) = (
        watch(SignalKind::interrupt(), Signal::INT, ignored),
        watch(SignalKind::quit(), Signal::QUIT, ignored),
        watch(SignalKind::terminate(), Signal::TERM, ignored),
        watch(SignalKind::hangup(), Signal::HUP, ignored),
    ) else {
        return unwrapped(&args.command, "signal setup failed");
    };
    let label = args.label.clone().unwrap_or_default();
    let dir = current_dir_name();
    let requested_at_ms = super::now_ms();
    let mut last_line = None;
    let mut patience = REFUSED_RETRY;
    // Начало текущего обрыва: 30 с на переподключение считаются от него, а не от запуска.
    let mut lost_since: Option<tokio::time::Instant> = None;
    let mut lost = false;
    let granted = loop {
        let stream = match connect(&socket, patience, lost_since.is_some()).await {
            Connect::Stream(stream) => stream,
            Connect::Unreachable(reason) => {
                return unwrapped(&args.command, &format!("worker unreachable ({reason})"));
            }
        };
        let waiting = request(
            stream,
            args.kind,
            &label,
            &dir,
            requested_at_ms,
            &mut last_line,
        );
        tokio::pin!(waiting);
        let outcome = tokio::select! {
            outcome = &mut waiting => outcome,
            _ = caught(&mut sigint) => reraise(Signal::INT),
            _ = caught(&mut sigquit) => reraise(Signal::QUIT),
            _ = caught(&mut sigterm) => reraise(Signal::TERM),
            _ = caught(&mut sighup) => reraise(Signal::HUP),
        };
        match outcome {
            Wait::Granted(granted) => break granted,
            Wait::Failed(reason) => {
                return unwrapped(&args.command, &format!("lease refused ({reason})"));
            }
            Wait::Lost(reason, answered) => {
                if answered || lost_since.is_none() {
                    lost = true;
                    lost_since = Some(tokio::time::Instant::now());
                    note(format!(
                        "connection to worker lost ({reason}), reconnecting"
                    ));
                }
                let since = lost_since.unwrap_or_else(tokio::time::Instant::now);
                let left = RECONNECT_TOTAL.saturating_sub(since.elapsed());
                if left.is_zero() {
                    return unwrapped(&args.command, "worker unreachable");
                }
                patience = left;
                tokio::time::sleep(RETRY_STEP).await;
            }
        }
    };
    if last_line.is_some() || lost {
        note(format!("granted {}", granted.kind.as_str()));
    }
    let mut command = tokio::process::Command::new(&args.command[0]);
    command.args(&args.command[1..]).envs(cap_env(
        &granted.lease_id,
        granted.budget,
        granted.playwright_workers,
    ));
    // Своя группа процессов у команды только когда терминал не наш: групповой сигнал
    // (`kill -TERM -<pgid>`, Ctrl-C, закрытие терминала) тогда достаётся одной обёртке,
    // и команда получает его ровно один раз — пересланным. Передняя группа терминала
    // остаётся как была: Ctrl-C, Ctrl-Z и `tcsetpgrp` команды работают в ней.
    let isolate = !owns_terminal();
    if isolate {
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return spawn_failure(&args.command[0], &error),
    };
    let child_pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw);
    let forward = |sig: Signal| {
        if let Some(pid) = child_pid {
            let _ = if isolate {
                rustix::process::kill_process_group(pid, sig)
            } else {
                rustix::process::kill_process(pid, sig)
            };
        }
    };
    // INT и QUIT, пришедшие между выдачей и spawn: команда ещё не существовала, и
    // групповой сигнал её не застал — пересылаем ей.
    while pending(&mut sigint).await {
        forward(Signal::INT);
    }
    while pending(&mut sigquit).await {
        forward(Signal::QUIT);
    }
    let Granted {
        lease_id,
        mut lines,
        mut write,
        ..
    } = granted;
    let mut connected = true;
    // Переподключение идёт рядом с ожиданием команды: выход и TERM не ждут его 30 с.
    type Reattach =
        std::pin::Pin<Box<dyn Future<Output = Option<(Reader, OwnedWriteHalf)>> + Send>>;
    let mut reattaching: Option<Reattach> = None;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = caught(&mut sigint) => {
                if isolate { forward(Signal::INT); }
            }
            _ = caught(&mut sigquit) => {
                if isolate { forward(Signal::QUIT); }
            }
            _ = caught(&mut sigterm) => forward(Signal::TERM),
            _ = caught(&mut sighup) => forward(Signal::HUP),
            line = lines.next_line(), if connected => {
                if matches!(line, Ok(Some(_))) { continue; }
                // Воркер ушёл (рестарт): снова прицепиться к своей аренде, команда работает дальше.
                connected = false;
                reattaching = Some(Box::pin(reattach(socket.clone(), lease_id.clone())));
            }
            result = async { reattaching.as_mut().unwrap().await }, if reattaching.is_some() => {
                reattaching = None;
                match result {
                    Some((new_lines, new_write)) => {
                        lines = new_lines;
                        write = new_write;
                        connected = true;
                    }
                    None => note("worker unreachable, continuing without lease"),
                }
            }
        }
    };
    drop(write);
    match status {
        Ok(status) => match (status.code(), status.signal()) {
            (Some(code), _) => ExitCode::from(u8::try_from(code & 0xff).unwrap_or(1)),
            (None, Some(number)) => match signal_from_raw(number) {
                Some(sig) => reraise(sig),
                None => ExitCode::from(u8::try_from((128 + number) & 0xff).unwrap_or(1)),
            },
            (None, None) => ExitCode::FAILURE,
        },
        Err(error) => {
            note(format!("waiting for command failed: {error}"));
            ExitCode::FAILURE
        }
    }
}

/// Держатель после обрыва: переподключение и `attach` в пределах 30 с.
async fn reattach(socket: PathBuf, lease_id: String) -> Option<(Reader, OwnedWriteHalf)> {
    let started = tokio::time::Instant::now();
    loop {
        let left = RECONNECT_TOTAL.saturating_sub(started.elapsed());
        if left.is_zero() {
            return None;
        }
        if let Connect::Stream(stream) = connect(&socket, left, true).await {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let attach = ClientMessage::Attach {
                lease_id: lease_id.clone(),
            };
            if write_message(&mut write, attach).await.is_ok() {
                match read_message(&mut lines).await {
                    Ok(ServerMessage::Attached { .. }) => return Some((lines, write)),
                    Ok(_) => return None,
                    Err(_) => {}
                }
            }
        }
        tokio::time::sleep(RETRY_STEP).await;
    }
}

pub fn signal_from_raw(number: i32) -> Option<Signal> {
    [
        Signal::HUP,
        Signal::INT,
        Signal::QUIT,
        Signal::ILL,
        Signal::TRAP,
        Signal::ABORT,
        Signal::BUS,
        Signal::FPE,
        Signal::KILL,
        Signal::USR1,
        Signal::SEGV,
        Signal::USR2,
        Signal::PIPE,
        Signal::ALARM,
        Signal::TERM,
        Signal::XCPU,
        Signal::XFSZ,
        Signal::VTALARM,
        Signal::PROF,
        Signal::SYS,
    ]
    .into_iter()
    .find(|sig| sig.as_raw() == number)
}

/// Тот же сигнал на себе с обработчиком по умолчанию. Перехват tokio не снять, поэтому
/// процесс заменяет себя (`exec` сбрасывает перехваченные сигналы) скрытой подкомандой,
/// которая посылает сигнал себе.
fn reraise(sig: Signal) -> ! {
    let _ = io::stderr().flush();
    let error = std::process::Command::new("/proc/self/exe")
        .args(["lease", "raise", "--signal"])
        .arg(sig.as_raw().to_string())
        .exec();
    note(format!("re-raise failed: {error}"));
    std::process::exit(128 + sig.as_raw());
}

/// Тело скрытой подкоманды `lease raise`.
pub fn raise(number: i32) -> ExitCode {
    if let Some(sig) = signal_from_raw(number) {
        let _ = rustix::process::kill_process(rustix::process::getpid(), sig);
        // Сигнал доставлен синхронно; сюда доходит только игнорируемый.
    }
    ExitCode::from(u8::try_from((128 + number) & 0xff).unwrap_or(1))
}

pub async fn status(json: bool) -> ExitCode {
    let Some(socket) = discover_socket() else {
        note("no lease socket known (set CANTERLOT_LEASE_SOCKET or XDG_RUNTIME_DIR)");
        return ExitCode::from(1);
    };
    let stream = match connect(&socket, Duration::ZERO, false).await {
        Connect::Stream(stream) => stream,
        Connect::Unreachable(reason) => {
            note(format!("worker unreachable ({reason})"));
            return ExitCode::from(1);
        }
    };
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    if let Err(error) = write_message(&mut write, ClientMessage::Status).await {
        note(error.to_string());
        return ExitCode::from(1);
    }
    let snapshot = match tokio::time::timeout(REPLY_TIMEOUT, read_message(&mut lines)).await {
        Ok(Ok(ServerMessage::Status(snapshot))) => snapshot,
        Ok(Ok(other)) => {
            note(format!("unexpected reply {other:?}"));
            return ExitCode::from(1);
        }
        Ok(Err(reason)) => {
            note(reason);
            return ExitCode::from(1);
        }
        Err(_) => {
            note(format!(
                "worker did not answer within {}s",
                REPLY_TIMEOUT.as_secs()
            ));
            return ExitCode::from(1);
        }
    };
    let text = if json {
        let mut line = serde_json::to_string(&snapshot).unwrap_or_default();
        line.push('\n');
        line
    } else {
        render(&snapshot, super::now_ms())
    };
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn age(now_ms: u64, since_ms: u64) -> String {
    let seconds = now_ms.saturating_sub(since_ms) / 1000;
    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// Таблица для людей: держатели и ожидающие, pid обёртки и его состояние (`T` — остановлен).
pub fn render(snapshot: &Snapshot, now_ms: u64) -> String {
    let used: u32 = snapshot.holders.iter().map(|h| h.weight).sum();
    let mut out = format!("slots {used}/{}\n", snapshot.slots);
    let row = |entry: &EntryView, prefix: String, since: u64| {
        format!(
            "{prefix} {} pid {} [{}] {}\n",
            entry.describe(),
            entry.wrapper_pid,
            if entry.state.is_empty() {
                "?"
            } else {
                &entry.state
            },
            age(now_ms, since)
        )
    };
    if snapshot.holders.is_empty() {
        out.push_str("holders: none\n");
    } else {
        out.push_str("holders:\n");
        for entry in &snapshot.holders {
            out.push_str(&row(
                entry,
                "  held".into(),
                entry.since_ms.unwrap_or(now_ms),
            ));
        }
    }
    if snapshot.waiters.is_empty() {
        out.push_str("waiters: none\n");
    } else {
        out.push_str("waiters:\n");
        for entry in &snapshot.waiters {
            out.push_str(&row(
                entry,
                format!("  #{}", entry.position.unwrap_or_default()),
                entry.requested_at_ms,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_line_names_holders() {
        let holders = vec![
            EntryView {
                kind: "build".into(),
                label: "Баннер связи".into(),
                ..Default::default()
            },
            EntryView {
                kind: "build".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            waiting_line(Kind::Gate, 2, &holders),
            "waiting for gate, position 2 (holders: build «Баннер связи» owner, build owner)"
        );
    }

    #[test]
    fn render_shows_pid_state_and_ages() {
        let snapshot = Snapshot {
            slots: 2,
            holders: vec![EntryView {
                kind: "gate".into(),
                weight: 2,
                label: "gate".into(),
                wrapper_pid: 42,
                state: "T".into(),
                since_ms: Some(1_000),
                ..Default::default()
            }],
            waiters: vec![EntryView {
                kind: "build".into(),
                weight: 1,
                execution_id: Some("abcdef0123".into()),
                wrapper_pid: 43,
                state: "S".into(),
                requested_at_ms: 3_661_000,
                position: Some(1),
                ..Default::default()
            }],
            observed_at_ms: 0,
        };
        assert_eq!(
            render(&snapshot, 3_721_000),
            "slots 2/2\nholders:\n  held gate «gate» owner pid 42 [T] 1h02m\nwaiters:\n  #1 build abcdef01 pid 43 [S] 1m00s\n"
        );
    }

    #[test]
    fn signals_map_back_from_numbers() {
        assert_eq!(signal_from_raw(2), Some(Signal::INT));
        assert_eq!(signal_from_raw(15), Some(Signal::TERM));
        assert_eq!(signal_from_raw(0), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_takes_a_delivered_signal() {
        let mut child_signal =
            Some(tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child()).unwrap());
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        child.wait().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !pending(&mut child_signal).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("SIGCHLD from the child");
        let mut ignored = None;
        assert!(!pending(&mut ignored).await);
    }
}
