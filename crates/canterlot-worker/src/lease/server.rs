//! Сокет аренды в воркере. Очередь живёт в памяти, выданные аренды — ещё и в
//! `<data_dir>/leases.json`: рестарт воркера (сопровождения его переживают) сохраняет
//! держателей, чья обёртка жива, без окна повторных заявок.
//!
//! Аренда держится, пока жива обёртка (pid и время старта) и открыто её соединение.
//! Восстановленная после рестарта аренда держится, пока жива обёртка; переподключившись,
//! обёртка снова отпускает её закрытием соединения. Принудительно воркер ничего не убивает.

use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream, unix::OwnedWriteHalf},
    sync::watch,
    task::JoinHandle,
};

use super::{
    ClientMessage, EntryView, Kind, ServerMessage, Snapshot, decode, encode,
    scheduler::{Entry, Scheduler},
};

const LIVENESS_POLL: Duration = Duration::from_millis(500);
const BIND_RETRY: Duration = Duration::from_secs(5);
const FIRST_LINE_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub socket: PathBuf,
    pub state_file: PathBuf,
    pub slots: u32,
}

impl ServerConfig {
    pub fn new(runtime_dir: Option<&Path>, data_dir: &Path, slots: u32) -> Self {
        Self {
            socket: super::socket_path(runtime_dir, data_dir),
            state_file: data_dir.join(super::STATE_FILE),
            slots,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Meta {
    wrapper_pid: u32,
    start_ticks: u64,
    execution_id: Option<String>,
    label: String,
    dir: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct Persisted {
    lease_id: String,
    kind: Kind,
    weight: u32,
    wrapper_pid: u32,
    start_ticks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    since: u64,
    #[serde(default)]
    requested_at_ms: u64,
    #[serde(default)]
    label: String,
    #[serde(default)]
    dir: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct StateFile {
    version: u32,
    grants: Vec<Persisted>,
}

struct State {
    scheduler: Scheduler<Meta>,
}

struct Shared {
    state: Mutex<State>,
    changes: watch::Sender<Snapshot>,
    state_file: PathBuf,
    nproc: u32,
    /// Ошибка чтения `/proc` уже залогирована: не повторять её каждые 500 мс.
    liveness_warned: AtomicBool,
}

/// Работающий сервер; снимок очереди идёт в хаб.
pub struct LeaseServer {
    snapshot: watch::Receiver<Snapshot>,
    task: JoinHandle<()>,
}

impl LeaseServer {
    /// Поднимает сервер в фоне. Занятый другим воркером сокет повторяется раз в 5 с:
    /// живой чужой сокет никогда не удаляется.
    pub fn start(config: ServerConfig, shutdown: watch::Receiver<bool>) -> Self {
        let mut scheduler = Scheduler::new(config.slots);
        let restored = restore(&config.state_file, &mut scheduler);
        let shared = Arc::new(Shared {
            state: Mutex::new(State { scheduler }),
            changes: watch::channel(Snapshot::default()).0,
            state_file: config.state_file.clone(),
            nproc: super::nproc(),
            liveness_warned: AtomicBool::new(false),
        });
        {
            let state = shared.state.lock().expect("lease state");
            if restored {
                persist(&shared.state_file, &state.scheduler);
            }
            shared.publish(&state);
        }
        let snapshot = shared.changes.subscribe();
        let task = tokio::spawn(serve(shared, config.socket, shutdown));
        Self { snapshot, task }
    }

    pub fn snapshot(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    pub async fn stop(mut self) {
        let task = std::mem::replace(&mut self.task, tokio::spawn(async {}));
        task.abort();
        let _ = task.await;
    }
}

impl Drop for LeaseServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Загружает выданные аренды и оставляет те, чья обёртка жива. `true`, если файл был.
fn restore(path: &Path, scheduler: &mut Scheduler<Meta>) -> bool {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(error) => {
            tracing::warn!(%error, "lease state unreadable, starting empty");
            return true;
        }
    };
    let file: StateFile = match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "lease state invalid, starting empty");
            return true;
        }
    };
    for grant in file.grants {
        match liveness(grant.wrapper_pid, grant.start_ticks) {
            Liveness::Alive => {}
            Liveness::Dead => {
                tracing::info!(lease_id = %grant.lease_id, "lease holder gone during restart, released");
                continue;
            }
            Liveness::Unknown(error) => {
                // Ошибка чтения /proc — не доказательство смерти: аренду держим.
                tracing::warn!(lease_id = %grant.lease_id, %error, "lease holder unreadable, keeping restored lease");
            }
        }
        scheduler.restore(
            grant.lease_id,
            grant.kind,
            grant.weight,
            grant.since,
            grant.requested_at_ms,
            Meta {
                wrapper_pid: grant.wrapper_pid,
                start_ticks: grant.start_ticks,
                execution_id: grant.execution_id,
                label: grant.label,
                dir: grant.dir,
            },
        );
    }
    true
}

/// Атомарная замена файла: сбой посреди записи оставляет прошлую версию.
fn persist(path: &Path, scheduler: &Scheduler<Meta>) {
    let file = StateFile {
        version: STATE_VERSION,
        grants: scheduler
            .holders()
            .iter()
            .map(|entry| Persisted {
                lease_id: entry.id.clone(),
                kind: entry.kind,
                weight: entry.weight,
                wrapper_pid: entry.meta.wrapper_pid,
                start_ticks: entry.meta.start_ticks,
                execution_id: entry.meta.execution_id.clone(),
                since: entry.since_ms.unwrap_or_default(),
                requested_at_ms: entry.requested_at_ms,
                label: entry.meta.label.clone(),
                dir: entry.meta.dir.clone(),
            })
            .collect(),
    };
    let write = || -> io::Result<()> {
        let temporary = path.with_extension("json.tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        let mut out = options.open(&temporary)?;
        io::Write::write_all(&mut out, &serde_json::to_vec_pretty(&file)?)?;
        out.sync_all()?;
        fs::rename(&temporary, path)
    };
    if let Err(error) = write() {
        tracing::warn!(%error, path = %path.display(), "lease state not saved");
    }
}

fn proc_stat(pid: u32) -> io::Result<Option<(char, u64)>> {
    proc_stat_at(Path::new("/proc"), pid)
}

/// Состояние и время старта из `<root>/<pid>/stat`. `Ok(None)` — процесса нет или stat
/// пустой; ошибка чтения — не «процесс умер», а «неизвестно»: при EMFILE нельзя
/// освобождать все аренды.
fn proc_stat_at(root: &Path, pid: u32) -> io::Result<Option<(char, u64)>> {
    let text = match crate::containment::read_stat(root.join(pid.to_string()).join("stat")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(end) = text.rfind(')') else {
        return Ok(None);
    };
    let fields: Vec<_> = text[end + 1..].split_whitespace().collect();
    let Some(state) = fields.first().and_then(|field| field.chars().next()) else {
        return Ok(None);
    };
    let Some(ticks) = fields.get(19).and_then(|field| field.parse().ok()) else {
        return Ok(None);
    };
    Ok(Some((state, ticks)))
}

/// Что говорит `/proc` об обёртке: жива, умерла или прочитать не удалось.
#[derive(Debug)]
enum Liveness {
    Alive,
    Dead,
    Unknown(io::Error),
}

fn classify(stat: io::Result<Option<(char, u64)>>, start_ticks: u64) -> Liveness {
    match stat {
        Ok(Some((state, ticks))) if ticks == start_ticks && !matches!(state, 'Z' | 'X') => {
            Liveness::Alive
        }
        Ok(_) => Liveness::Dead,
        Err(error) => Liveness::Unknown(error),
    }
}

fn liveness_at(root: &Path, pid: u32, start_ticks: u64) -> Liveness {
    classify(proc_stat_at(root, pid), start_ticks)
}

fn liveness(pid: u32, start_ticks: u64) -> Liveness {
    classify(proc_stat(pid), start_ticks)
}


fn view(entry: &Entry<Meta>, position: Option<usize>) -> EntryView {
    EntryView {
        lease_id: entry.id.clone(),
        kind: entry.kind.as_str().into(),
        weight: entry.weight,
        label: entry.meta.label.clone(),
        dir: entry.meta.dir.clone(),
        holder: if entry.meta.execution_id.is_some() {
            "execution".into()
        } else {
            "owner".into()
        },
        execution_id: entry.meta.execution_id.clone(),
        wrapper_pid: entry.meta.wrapper_pid,
        state: String::new(),
        requested_at_ms: entry.requested_at_ms,
        since_ms: entry.since_ms,
        position: position.and_then(|p| u32::try_from(p).ok()),
    }
}

impl Shared {
    fn snapshot(&self, state: &State) -> Snapshot {
        Snapshot {
            slots: state.scheduler.slots(),
            holders: state
                .scheduler
                .holders()
                .iter()
                .map(|e| view(e, None))
                .collect(),
            waiters: state
                .scheduler
                .waiters()
                .iter()
                .enumerate()
                .map(|(at, e)| view(e, Some(at + 1)))
                .collect(),
            observed_at_ms: super::now_ms(),
        }
    }

    /// Новый снимок только при изменении очереди: время снимка изменением не считается.
    fn publish(&self, state: &State) {
        let next = self.snapshot(state);
        self.changes.send_if_modified(|current| {
            let same = current.slots == next.slots
                && current.holders == next.holders
                && current.waiters == next.waiters
                && current.observed_at_ms != 0;
            if !same {
                *current = next;
            }
            !same
        });
    }

    fn release(&self, id: &str, reason: &'static str) {
        let mut state = self.state.lock().expect("lease state");
        let was_holder = state.scheduler.holder(id).is_some();
        let (removed, granted) = state.scheduler.release(id, super::now_ms());
        if let Some(entry) = removed {
            tracing::info!(lease_id = %id, kind = entry.kind.as_str(), reason, holder = was_holder, "lease released");
            if was_holder || !granted.is_empty() {
                persist(&self.state_file, &state.scheduler);
            }
            self.publish(&state);
        }
    }

    fn sweep(&self) {
        let mut state = self.state.lock().expect("lease state");
        let now = super::now_ms();
        let mut dead = Vec::new();
        let mut unknown = None;
        for entry in state
            .scheduler
            .holders()
            .iter()
            .chain(state.scheduler.waiters())
        {
            match liveness(entry.meta.wrapper_pid, entry.meta.start_ticks) {
                Liveness::Alive => {}
                Liveness::Dead => dead.push(entry.id.clone()),
                Liveness::Unknown(error) => unknown = Some(error),
            }
        }
        self.report_liveness(unknown);
        if dead.is_empty() {
            return;
        }
        for id in &dead {
            state.scheduler.release(id, now);
            tracing::info!(lease_id = %id, "lease wrapper died, released");
        }
        persist(&self.state_file, &state.scheduler);
        self.publish(&state);
    }

    /// Ошибку чтения `/proc` показываем один раз до первого успешного чтения:
    /// аренды при ней не снимаются, иначе EMFILE обнулил бы всю очередь.
    fn report_liveness(&self, unknown: Option<io::Error>) {
        match unknown {
            Some(error) => {
                if !self.liveness_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(%error, "lease wrapper state unreadable, keeping leases");
                }
            }
            None => self.liveness_warned.store(false, Ordering::Relaxed),
        }
    }
}

async fn serve(shared: Arc<Shared>, socket: PathBuf, mut shutdown: watch::Receiver<bool>) {
    let sweeper = {
        let shared = shared.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LIVENESS_POLL).await;
                shared.sweep();
            }
        })
    };
    let _sweeper = AbortOnDrop(sweeper);
    let mut warned = false;
    let (listener, _lock) = loop {
        match tokio::task::spawn_blocking({
            let socket = socket.clone();
            move || bind(&socket)
        })
        .await
        {
            Ok(Ok(bound)) => break bound,
            Ok(Err(error)) => {
                if !warned {
                    tracing::warn!(%error, socket = %socket.display(), "lease socket unavailable, retrying");
                    warned = true;
                }
            }
            Err(_) => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(BIND_RETRY) => {}
            _ = shutdown.changed() => return,
        }
        if *shutdown.borrow() {
            return;
        }
    };
    tracing::info!(socket = %socket.display(), "lease socket listening");
    let uid = rustix::process::getuid().as_raw();
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let shared = shared.clone();
                connections.spawn(async move {
                    if let Err(error) = connection(shared, stream, uid).await {
                        tracing::debug!(%error, "lease connection ended");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Сокет и замок рядом с ним. Чужой замок или живой сокет — отказ: сокет не удаляется.
fn bind(socket: &Path) -> io::Result<(UnixListener, fs::File)> {
    let directory = socket
        .parent()
        .ok_or_else(|| io::Error::other("socket has no parent"))?;
    if !directory.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(super::LOCK_NAME))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |error| match error {
            rustix::io::Errno::WOULDBLOCK => io::Error::new(
                io::ErrorKind::WouldBlock,
                "lease lock held by another worker",
            ),
            other => io::Error::from(other),
        },
    )?;
    if fs::symlink_metadata(socket).is_ok() {
        if is_live_socket(socket) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "lease socket is served by another process",
            ));
        }
        fs::remove_file(socket)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok((UnixListener::from_std(listener)?, lock))
}

fn is_live_socket(path: &Path) -> bool {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => true,
        Err(error) => !matches!(
            error.kind(),
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
        ),
    }
}

async fn send(write: &mut OwnedWriteHalf, message: ServerMessage) -> io::Result<()> {
    write.write_all(encode(message).as_bytes()).await
}

/// Предел строки протокола: сообщения короткие, а клиент без перевода строки не должен
/// копить память воркера.
const MAX_LINE: usize = 64 * 1024;

/// Построчный разбор с пределом длины: `BufReader::lines()` без предела копил бы
/// память по чужому потоку без перевода строки.
struct CappedLines<R> {
    reader: BufReader<R>,
    line: Vec<u8>,
}

impl<R: tokio::io::AsyncRead + Unpin> CappedLines<R> {
    fn new(read: R) -> Self {
        Self {
            reader: BufReader::new(read),
            line: Vec::new(),
        }
    }

    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.line.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(self.finish()?));
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(end) => {
                    if self.line.len() + end > MAX_LINE {
                        return Err(too_long());
                    }
                    self.line.extend_from_slice(&available[..end]);
                    self.reader.consume(end + 1);
                    return Ok(Some(self.finish()?));
                }
                None => {
                    if self.line.len() + available.len() > MAX_LINE {
                        return Err(too_long());
                    }
                    let taken = available.len();
                    self.line.extend_from_slice(available);
                    self.reader.consume(taken);
                }
            }
        }
    }

    fn finish(&mut self) -> io::Result<String> {
        let mut line = std::mem::take(&mut self.line);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        String::from_utf8(line)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "lease line is not utf-8"))
    }
}

fn too_long() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "lease line too long")
}

async fn connection(shared: Arc<Shared>, stream: UnixStream, uid: u32) -> io::Result<()> {
    let peer = stream.peer_cred()?;
    if peer.uid() != uid {
        return Err(io::Error::other("foreign uid"));
    }
    let pid = peer
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| io::Error::other("peer pid unknown"))?;
    let (read, mut write) = stream.into_split();
    let mut lines = CappedLines::new(read);
    let first = tokio::time::timeout(FIRST_LINE_TIMEOUT, lines.next_line())
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    let message = match decode::<ClientMessage>(&first) {
        Ok(message) => message,
        Err(reason) => {
            send(&mut write, ServerMessage::Error { reason }).await?;
            return Ok(());
        }
    };
    match message {
        ClientMessage::Status => {
            let mut snapshot = {
                let state = shared.state.lock().expect("lease state");
                shared.snapshot(&state)
            };
            for entry in snapshot.holders.iter_mut().chain(&mut snapshot.waiters) {
                entry.state = proc_stat(entry.wrapper_pid)
                    .ok()
                    .flatten()
                    .map(|(state, _)| state.to_string())
                    .unwrap_or_default();
            }
            send(&mut write, ServerMessage::Status(snapshot)).await
        }
        ClientMessage::Attach { lease_id } => {
            let attached = {
                let state = shared.state.lock().expect("lease state");
                state.scheduler.holder(&lease_id).is_some_and(|entry| {
                    // Ошибка чтения /proc не отменяет переподключение: pid уже совпал.
                    entry.meta.wrapper_pid == pid
                        && !matches!(
                            liveness(pid, entry.meta.start_ticks),
                            Liveness::Dead
                        )
                })
            };
            if !attached {
                return send(
                    &mut write,
                    ServerMessage::Error {
                        reason: "unknown lease".into(),
                    },
                )
                .await;
            }
            send(
                &mut write,
                ServerMessage::Attached {
                    lease_id: lease_id.clone(),
                },
            )
            .await?;
            hold(&shared, &lease_id, lines).await;
            Ok(())
        }
        ClientMessage::Acquire {
            kind,
            label,
            dir,
            requested_at_ms,
        } => {
            let start_ticks = match crate::containment::start_ticks(pid) {
                Ok(start_ticks) => start_ticks,
                Err(error) => {
                    // Без времени старта нельзя отличить переиспользованный pid:
                    // отвечаем ошибкой, чтобы клиент сразу шёл без аренды, а не ждал обрыва.
                    return send(
                        &mut write,
                        ServerMessage::Error {
                            reason: format!("cannot read wrapper start time: {error}"),
                        },
                    )
                    .await;
                }
            };
            let meta = Meta {
                wrapper_pid: pid,
                start_ticks,
                execution_id: crate::containment::read_marker(pid, start_ticks),
                label: truncate(label, 120),
                dir: truncate(dir, 120),
            };
            let id = uuid::Uuid::now_v7().to_string();
            let mut changes = shared.changes.subscribe();
            {
                let mut state = shared.state.lock().expect("lease state");
                let now = super::now_ms();
                let phantom = release_phantom(&mut state.scheduler, pid, start_ticks, now);
                let granted =
                    state
                        .scheduler
                        .request(id.clone(), kind, requested_at_ms, now, meta);
                if !phantom.is_empty() || !granted.is_empty() {
                    persist(&shared.state_file, &state.scheduler);
                }
                shared.publish(&state);
            }
            tracing::info!(lease_id = %id, kind = kind.as_str(), pid, "lease requested");
            if wait(&shared, &id, &mut write, &mut lines, &mut changes).await {
                hold(&shared, &id, lines).await;
            }
            Ok(())
        }
    }
}

/// Снимает аренду, восстановленную после рестарта, когда та же обёртка просит заново:
/// клиент мог не увидеть `Granted`, переподключился с новым номером, и старая аренда
/// иначе держала бы слот до выхода обёртки. Соединение у обёртки всегда одно, поэтому
/// совпадение pid и времени старта — это та же заявка, а не вторая.
fn release_phantom(
    scheduler: &mut Scheduler<Meta>,
    pid: u32,
    start_ticks: u64,
    now_ms: u64,
) -> Vec<String> {
    let phantom: Vec<String> = scheduler
        .holders()
        .iter()
        .filter(|entry| entry.meta.wrapper_pid == pid && entry.meta.start_ticks == start_ticks)
        .map(|entry| entry.id.clone())
        .collect();
    for id in &phantom {
        scheduler.release(id, now_ms);
        tracing::info!(lease_id = %id, pid, "restored lease without a client, released");
    }
    phantom
}

fn truncate(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        text
    } else {
        text.chars().take(limit).collect()
    }
}

enum Status {
    Granted(ServerMessage),
    Queued(u32, Vec<EntryView>),
    Gone,
}

/// Ждёт выдачи, сообщая место при каждом его изменении. `true` — аренда выдана.
async fn wait(
    shared: &Shared,
    id: &str,
    write: &mut OwnedWriteHalf,
    lines: &mut CappedLines<tokio::net::unix::OwnedReadHalf>,
    changes: &mut watch::Receiver<Snapshot>,
) -> bool {
    let mut last: Option<(u32, Vec<String>)> = None;
    loop {
        changes.borrow_and_update();
        let status = {
            let state = shared.state.lock().expect("lease state");
            if let Some(entry) = state.scheduler.holder(id) {
                let slots = state.scheduler.slots();
                Status::Granted(ServerMessage::Granted {
                    lease_id: id.to_owned(),
                    kind: entry.kind,
                    weight: entry.weight,
                    slots,
                    budget: super::budget(shared.nproc, entry.weight, slots),
                    playwright_workers: super::playwright_workers(
                        shared.nproc,
                        entry.weight,
                        slots,
                    ),
                })
            } else if let Some(position) = state.scheduler.position(id) {
                Status::Queued(
                    u32::try_from(position).unwrap_or(u32::MAX),
                    state
                        .scheduler
                        .holders()
                        .iter()
                        .map(|e| view(e, None))
                        .collect(),
                )
            } else {
                Status::Gone
            }
        };
        match status {
            Status::Granted(message) => {
                if send(write, message).await.is_err() {
                    shared.release(id, "client gone");
                    return false;
                }
                return true;
            }
            Status::Gone => return false,
            Status::Queued(position, holders) => {
                let key = (
                    position,
                    holders.iter().map(|h| h.lease_id.clone()).collect(),
                );
                if last.as_ref() != Some(&key) {
                    last = Some(key);
                    let message = ServerMessage::Queued {
                        lease_id: id.to_owned(),
                        position,
                        holders,
                    };
                    if send(write, message).await.is_err() {
                        shared.release(id, "client gone");
                        return false;
                    }
                }
            }
        }
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() { return false; }
            }
            line = lines.next_line() => {
                if !matches!(line, Ok(Some(_))) {
                    shared.release(id, "client closed while waiting");
                    return false;
                }
            }
        }
    }
}

/// Держит аренду до закрытия соединения клиентом.
async fn hold(
    shared: &Shared,
    id: &str,
    mut lines: CappedLines<tokio::net::unix::OwnedReadHalf>,
) {
    while let Ok(Some(_)) = lines.next_line().await {}
    shared.release(id, "client closed");
}

/// Активные аренды для проверки в тестах без сокета.
#[cfg(test)]
fn holder_ids(shared: &Shared) -> std::collections::HashSet<String> {
    shared
        .state
        .lock()
        .unwrap()
        .scheduler
        .holders()
        .iter()
        .map(|e| e.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pid: u32) -> Meta {
        Meta {
            wrapper_pid: pid,
            start_ticks: crate::containment::start_ticks(pid).unwrap(),
            execution_id: None,
            label: "l".into(),
            dir: "d".into(),
        }
    }

    #[test]
    fn persisted_grants_restore_only_live_wrappers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("leases.json");
        let mut before = Scheduler::new(2);
        let me = std::process::id();
        before.request("live".into(), Kind::Build, 1, 10, meta(me));
        let mut dead = meta(me);
        dead.start_ticks += 1;
        before.request("dead".into(), Kind::Build, 2, 20, dead);
        persist(&path, &before);
        let saved: StateFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.version, 1);
        assert_eq!(saved.grants.len(), 2);
        assert_eq!(saved.grants[0].since, 10);

        let mut after = Scheduler::new(2);
        assert!(restore(&path, &mut after));
        let ids: Vec<_> = after.holders().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["live"]);
        assert_eq!(after.holders()[0].since_ms, Some(10));
        assert_eq!(after.holders()[0].requested_at_ms, 1);
        // Восстановленный держатель занимает слот: следующая сборка встаёт рядом.
        assert_eq!(
            after.request("next".into(), Kind::Build, 3, 30, meta(me)),
            ["next"]
        );
    }

    #[test]
    fn broken_state_file_starts_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("leases.json");
        fs::write(&path, b"{not json").unwrap();
        let mut scheduler = Scheduler::<Meta>::new(2);
        assert!(restore(&path, &mut scheduler));
        assert!(scheduler.holders().is_empty());
        assert!(!restore(&directory.path().join("missing"), &mut scheduler));
    }

    #[test]
    fn sweep_releases_dead_holders_and_grants_next() {
        let directory = tempfile::tempdir().unwrap();
        let mut scheduler = Scheduler::new(1);
        let me = std::process::id();
        let mut dead = meta(me);
        dead.start_ticks += 1;
        scheduler.request("dead".into(), Kind::Build, 1, 1, dead);
        scheduler.request("next".into(), Kind::Build, 2, 2, meta(me));
        let shared = Shared {
            state: Mutex::new(State { scheduler }),
            changes: watch::channel(Snapshot::default()).0,
            state_file: directory.path().join("leases.json"),
            nproc: 4,
            liveness_warned: AtomicBool::new(false),
        };
        shared.sweep();
        assert_eq!(
            holder_ids(&shared),
            std::collections::HashSet::from(["next".to_owned()])
        );
        let saved: StateFile =
            serde_json::from_slice(&fs::read(directory.path().join("leases.json")).unwrap())
                .unwrap();
        assert_eq!(saved.grants[0].lease_id, "next");
        assert_eq!(shared.changes.borrow().holders[0].lease_id, "next");
    }

    #[test]
    fn unreadable_stat_is_unknown_not_dead() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let stat = |pid: u32, state: char, ticks: u64| {
            let mut text = format!("{pid} (sh) {state}");
            for field in 1..=19 {
                if field == 19 {
                    text.push_str(&format!(" {ticks}"));
                } else {
                    text.push_str(" 0");
                }
            }
            text
        };

        fs::create_dir(root.join("42")).unwrap();
        fs::write(root.join("42/stat"), stat(42, 'S', 7)).unwrap();
        assert!(matches!(liveness_at(root, 42, 7), Liveness::Alive));
        assert!(matches!(liveness_at(root, 42, 8), Liveness::Dead));
        assert!(matches!(liveness_at(root, 99, 7), Liveness::Dead));

        // stat не читается (каталог вместо файла): это не смерть обёртки.
        fs::create_dir(root.join("43")).unwrap();
        fs::create_dir(root.join("43/stat")).unwrap();
        assert!(matches!(liveness_at(root, 43, 7), Liveness::Unknown(_)));
    }

    #[tokio::test]
    async fn capped_lines_bounds_lines_and_keeps_last_partial() {
        let (mut client, server) = tokio::io::duplex(MAX_LINE * 2);
        let mut lines = CappedLines::new(server);
        async fn write(client: &mut tokio::io::DuplexStream, bytes: &[u8]) {
            tokio::io::AsyncWriteExt::write_all(client, bytes)
                .await
                .unwrap();
        }

        write(&mut client, b"one\r\ntwo").await;
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("one"));
        drop(client);
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("two"));
        assert_eq!(lines.next_line().await.unwrap(), None);
    }

    #[tokio::test]
    async fn capped_lines_drop_overlong_line() {
        let (mut client, server) = tokio::io::duplex(MAX_LINE * 2);
        let mut lines = CappedLines::new(server);
        let long = vec![b'a'; MAX_LINE + 1];
        tokio::io::AsyncWriteExt::write_all(&mut client, &long)
            .await
            .unwrap();
        let error = lines.next_line().await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("too long"));
    }

    #[test]
    fn acquire_releases_restored_grant_of_same_wrapper() {
        let mut scheduler = Scheduler::new(2);
        let me = std::process::id();
        let mine = meta(me);
        scheduler.restore("phantom".into(), Kind::Build, 1, 10, 1, mine.clone());

        let released = release_phantom(&mut scheduler, me, mine.start_ticks, 20);
        assert_eq!(released, ["phantom"]);
        assert!(scheduler.holders().is_empty());

        // Другой процесс и тот же pid с другим временем старта не трогаются.
        let mut other = mine.clone();
        other.wrapper_pid = me.wrapping_add(1);
        scheduler.restore("other".into(), Kind::Build, 1, 10, 1, other.clone());
        let mut reused = mine.clone();
        reused.start_ticks += 1;
        scheduler.restore("reused".into(), Kind::Build, 1, 10, 1, reused.clone());
        assert!(release_phantom(&mut scheduler, me, mine.start_ticks, 20).is_empty());
        assert!(release_phantom(&mut scheduler, other.wrapper_pid, reused.start_ticks, 20).is_empty());
        let ids: Vec<_> = scheduler.holders().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["other", "reused"]);
    }

    #[test]
    fn bind_refuses_live_socket_and_replaces_stale_one() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("canterlot").join("lease.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (first, lock) = bind(&socket).unwrap();
            let mode = fs::metadata(&socket).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = fs::metadata(socket.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o700);
            // Замок держит первый: второй не трогает сокет.
            let error = bind(&socket).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            drop(lock);
            // Замка нет, но сокет живой: всё равно отказ.
            let error = bind(&socket).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
            drop(first);
            // Мёртвый сокет заменяется.
            assert!(socket.exists());
            let (_second, _lock) = bind(&socket).unwrap();
            assert!(is_live_socket(&socket));
        });
    }
}
