//! Сокет `jobs.sock`: принимает задачи владельца и отдаёт их итоги.

use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::watch,
    task::JoinHandle,
};

use super::{ClientMessage, Hub, JobSpec, Refusal, ServerMessage, Store, decode, encode};

/// Предел одной строки: команда оболочки до 64 КиБ плюс обвязка.
const MAX_LINE: u64 = 256 * 1024;
const BIND_RETRY: Duration = Duration::from_secs(2);
/// Хаб не ответил на `OwnerJobSubmit`: задача не принята.
const HUB_REPLY: Duration = Duration::from_secs(30);

pub struct OwnerJobServer {
    task: JoinHandle<()>,
}

impl OwnerJobServer {
    pub fn start(
        socket: PathBuf,
        store: Arc<Store>,
        hub: Arc<dyn Hub>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        // Первая попытка сразу: сокет появляется к возврату из `start`, повторы — в задаче.
        let first = bind(&socket).ok();
        Self {
            task: tokio::spawn(serve(socket, first, store, hub, shutdown)),
        }
    }
}

impl Drop for OwnerJobServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Сокет с правами 0600 в каталоге 0700. Живой сокет другого процесса — отказ.
fn bind(socket: &Path) -> io::Result<UnixListener> {
    let directory = socket
        .parent()
        .ok_or_else(|| io::Error::other("socket has no parent"))?;
    if !directory.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }
    if fs::symlink_metadata(socket).is_ok() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "jobs socket is served by another process",
            ));
        }
        fs::remove_file(socket)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    UnixListener::from_std(listener)
}

async fn serve(
    socket: PathBuf,
    first: Option<UnixListener>,
    store: Arc<Store>,
    hub: Arc<dyn Hub>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut first = first;
    let listener = loop {
        if let Some(listener) = first.take() {
            break listener;
        }
        match bind(&socket) {
            Ok(listener) => break listener,
            Err(error) => {
                tracing::warn!(%error, socket = %socket.display(), "jobs socket unavailable, retrying");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(BIND_RETRY) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    };
    tracing::info!(socket = %socket.display(), "jobs socket listening");
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let (store, hub) = (store.clone(), hub.clone());
                connections.spawn(async move {
                    if let Err(error) = connection(stream, store, hub).await {
                        tracing::debug!(%error, "jobs connection ended");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

async fn send(stream: &mut UnixStream, message: ServerMessage) -> io::Result<()> {
    stream.write_all(encode(message).as_bytes()).await
}

fn refused(refusal: Refusal) -> ServerMessage {
    ServerMessage::Refused {
        code: refusal.code,
        message: refusal.message,
    }
}

/// Проверка, которую воркер делает сам: без неё хаб получил бы очевидно негодное.
fn check(spec: &JobSpec) -> Result<(), Refusal> {
    if spec.title.trim().is_empty() || spec.title.chars().count() > 200 {
        return Err(Refusal::new("bad_spec", "title must be 1..200 characters"));
    }
    if spec.shell.trim().is_empty() || spec.shell.len() > 64 * 1024 {
        return Err(Refusal::new(
            "bad_spec",
            "shell must be a non-empty command up to 64 KiB",
        ));
    }
    if let Some(source) = &spec.source
        && (source.rev.len() != 40 || !source.rev.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Refusal::new(
            "bad_spec",
            "source rev must be a full 40-hex sha",
        ));
    }
    Ok(())
}

async fn connection(stream: UnixStream, store: Arc<Store>, hub: Arc<dyn Hub>) -> io::Result<()> {
    let mut stream = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let read = (&mut stream).take(MAX_LINE).read_line(&mut line).await?;
        if read == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        let message = match decode::<ClientMessage>(&line) {
            Ok(message) => message,
            Err(error) => {
                let reply = refused(Refusal::new("protocol", error));
                send(stream.get_mut(), reply).await?;
                return Ok(());
            }
        };
        match message {
            ClientMessage::Submit { spec } => {
                let reply = match check(&spec) {
                    Err(refusal) => refused(refusal),
                    Ok(()) => {
                        let json = serde_json::to_string(&spec).map_err(io::Error::other)?;
                        match tokio::time::timeout(HUB_REPLY, hub.submit(json)).await {
                            Ok(Ok(job_id)) => ServerMessage::Accepted {
                                job_id: job_id.to_string(),
                            },
                            Ok(Err(refusal)) => refused(refusal),
                            Err(_) => refused(Refusal::new(
                                "hub_unavailable",
                                "the hub did not answer in time",
                            )),
                        }
                    }
                };
                send(stream.get_mut(), reply).await?;
            }
            ClientMessage::Wait { job_id } => match store.wait(&job_id).await {
                Some(finished) => {
                    send(stream.get_mut(), ServerMessage::Finished(finished)).await?;
                    return Ok(());
                }
                None => {
                    let reply = refused(Refusal::new("bad_job_id", "job id is not a UUID"));
                    send(stream.get_mut(), reply).await?;
                    return Ok(());
                }
            },
        }
    }
}
