//! Надзиратель `serve`: unix-сокет, диктовки по одной, движок в дочернем процессе.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;

use crate::config::Config;
use crate::dictation::{ClientMsg, Dictation, EngineFate, FinalReason, Sink, percentile};
use crate::engine::{ChildEngine, ChildSpec, Loaded, Mode, Request};
use crate::frame::{self, Frame, FrameError, KIND_JSON, ReadLimits};
use crate::live::{Params, Update};

const HANDSHAKE: Duration = Duration::from_secs(5);
const UNLOAD_WAIT: Duration = Duration::from_secs(5);
const CLIENT_QUEUE: usize = 64;

#[derive(Debug, Default, Clone)]
struct Status {
    warm: bool,
    mode: Option<Mode>,
    busy: bool,
    load_error: Option<String>,
}

#[derive(Default)]
struct Slot {
    engine: Option<ChildEngine>,
    loaded: Option<Loaded>,
}

pub struct Server {
    config: Config,
    spec: ChildSpec,
    slot: tokio::sync::Mutex<Slot>,
    waiting: AtomicUsize,
    status: Mutex<Status>,
    epoch: AtomicU64,
    next_conn: AtomicU64,
    stop: watch::Sender<bool>,
}

impl Server {
    pub fn new(config: Config, spec: ChildSpec) -> Arc<Server> {
        Arc::new(Server {
            config,
            spec,
            slot: tokio::sync::Mutex::new(Slot::default()),
            waiting: AtomicUsize::new(0),
            status: Mutex::new(Status::default()),
            epoch: AtomicU64::new(0),
            next_conn: AtomicU64::new(1),
            stop: watch::channel(false).0,
        })
    }

    /// Привязка сокета. Оставшийся от прошлого запуска файл удаляется, если его никто не слушает.
    pub async fn bind(path: &Path) -> std::io::Result<UnixListener> {
        if path.exists() {
            if UnixStream::connect(path).await.is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("{} уже слушает другая служба", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        UnixListener::bind(path)
    }

    /// Принимает соединения, пока не придёт `shutdown`.
    pub async fn run(self: Arc<Self>, listener: UnixListener) {
        let mut stop = self.stop.subscribe();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let me = self.clone();
                    tokio::spawn(async move { me.handle(stream).await });
                }
                _ = stop.wait_for(|s| *s) => break,
            }
        }
        // Активная диктовка видит stop сама; здесь только ждём слот и выключаем движок.
        let mut slot = self.slot.lock().await;
        if let Some(engine) = slot.engine.take() {
            let how = engine.shutdown(UNLOAD_WAIT).await;
            tracing::info!(
                kind = "engine_unloaded",
                reason = "shutdown",
                how,
                "engine stopped"
            );
        }
        slot.loaded = None;
    }

    pub fn shutdown(&self) {
        let _ = self.stop.send(true);
    }

    pub fn health(&self) -> serde_json::Value {
        let st = self.status.lock().unwrap().clone();
        json!({
            "type": "health",
            "ok": st.load_error.is_none(),
            "engine": if st.warm { "warm" } else { "cold" },
            "mode": st.mode.map(Mode::as_str),
            "model": self.config.model_name(),
            "busy": st.busy,
        })
    }

    async fn handle(self: Arc<Self>, stream: UnixStream) {
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let uid = stream.peer_cred().map(|c| c.uid()).unwrap_or(u32::MAX);
        let (mut rd, mut wr) = stream.into_split();
        let first = tokio::time::timeout(HANDSHAKE, frame::read_frame(&mut rd, ReadLimits::CLIENT));
        let start = match first.await {
            Ok(Ok(Frame::Json(bytes))) => serde_json::from_slice::<First>(&bytes).ok(),
            _ => None,
        };
        let start = match start {
            Some(First::Health) => {
                let _ = frame::write_json(&mut wr, &self.health()).await;
                return;
            }
            Some(First::Start(s)) => s,
            None => {
                let _ = frame::write_json(&mut wr, &error_json("bad_request")).await;
                return;
            }
        };
        let started = Instant::now();
        if start.protocol != 1 {
            let _ = frame::write_json(&mut wr, &error_json("bad_request")).await;
            return;
        }
        if start.sample_rate != 16_000 || start.format != "s16le" {
            let _ = frame::write_json(&mut wr, &error_json("bad_audio")).await;
            return;
        }
        let client: String = start.client.chars().take(64).collect();
        self.dictation(conn, uid, client, started, rd, wr).await;
    }

    async fn dictation(
        self: Arc<Self>,
        conn: u64,
        uid: u32,
        client: String,
        started: Instant,
        rd: OwnedReadHalf,
        wr: OwnedWriteHalf,
    ) {
        let (failed_tx, failed_rx) = watch::channel(false);
        let outbox = Arc::new(OutboxShared::default());
        let writer = tokio::spawn(writer_task(
            outbox.clone(),
            wr,
            self.config.limits.write_timeout,
            failed_tx,
        ));
        let mut sink = SocketSink {
            outbox: outbox.clone(),
        };
        let (client_tx, client_rx) = mpsc::channel(CLIENT_QUEUE);
        let reader = tokio::spawn(reader_task(rd, client_tx));

        let guard = match self.slot.try_lock() {
            Ok(g) => Some(g),
            // Ждать может только одна диктовка, следующая сразу получает busy.
            Err(_) => match self
                .waiting
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Err(_) => None,
                Ok(_) => {
                    sink.loading();
                    let wait = self.config.limits.queue_wait;
                    let g = tokio::time::timeout(wait, self.slot.lock()).await.ok();
                    self.waiting.store(0, Ordering::SeqCst);
                    g
                }
            },
        };
        let Some(mut slot) = guard else {
            tracing::info!(kind = "speech_busy", conn, "dictation refused: engine busy");
            sink.error("busy");
            outbox.close();
            reader.abort();
            let _ = writer.await;
            return;
        };
        self.epoch.fetch_add(1, Ordering::SeqCst);

        if slot.engine.as_mut().is_some_and(|e| e.try_exited()) {
            slot.engine = None;
            slot.loaded = None;
            self.set_status(|s| {
                s.warm = false;
                s.mode = None;
            });
        }
        if slot.engine.is_none() {
            match ChildEngine::spawn(&self.spec) {
                Ok(engine) => {
                    tracing::info!(
                        kind = "engine_spawned",
                        pid = engine.pid,
                        "engine process started"
                    );
                    let _ = engine.link.tx.send(Request::Load);
                    slot.engine = Some(engine);
                    slot.loaded = None;
                }
                Err(e) => {
                    tracing::warn!(kind = "engine_spawn_failed", error = %e, "engine process failed to start");
                    sink.error("engine_unavailable");
                    outbox.close();
                    reader.abort();
                    let _ = writer.await;
                    return;
                }
            }
        }
        self.set_status(|s| s.busy = true);
        let was_loaded = slot.loaded.is_some();
        tracing::info!(kind = "dictation_started", conn, uid, client = %client, warm = was_loaded, "dictation started");

        let slot_ref = &mut *slot;
        let engine = slot_ref.engine.as_mut().expect("движок запущен");
        let d = Dictation {
            link: &mut engine.link,
            loaded: slot_ref.loaded.clone(),
            params: Params::from_config(&self.config.live),
            limits: self.config.limits.clone(),
            started,
            writer_failed: Some(failed_rx),
            stop: Some(self.stop.subscribe()),
            sink,
        };
        let (outcome, _sink) = d.run(client_rx).await;
        reader.abort();
        outbox.close();

        if !was_loaded && let Some(l) = &outcome.loaded {
            tracing::info!(
                kind = "engine_loaded",
                mode = l.mode.as_str(),
                load_ms = l.load_ms,
                hash_ms = l.hash_ms,
                gpu_error = l.gpu_error.as_deref().unwrap_or(""),
                "engine loaded"
            );
        }
        slot_ref.loaded = outcome.loaded.clone();
        let load_error = (outcome.result == "engine_unavailable").then(|| "load_failed".to_owned());
        if let EngineFate::Kill(reason) = outcome.fate {
            if let Some(engine) = slot_ref.engine.take() {
                engine.kill().await;
            }
            slot_ref.loaded = None;
            tracing::info!(kind = "engine_killed", reason, "engine process killed");
        }
        let warm = slot_ref.loaded.is_some();
        let mode = slot_ref.loaded.as_ref().map(|l| l.mode);
        self.set_status(|s| {
            s.busy = false;
            s.warm = warm;
            s.mode = mode;
            if warm {
                s.load_error = None;
            } else if load_error.is_some() {
                s.load_error = load_error.clone();
            }
        });
        let st = &outcome.stats;
        tracing::info!(
            kind = "dictation_finished",
            conn,
            outcome = outcome.result,
            mode = mode.map(Mode::as_str).unwrap_or(""),
            audio_ms = outcome.audio_samples / 16,
            frames = outcome.frames,
            segments = st.segments,
            forced_cuts = st.forced_cuts,
            commits = st.commits,
            tentatives = st.tentatives,
            stale = st.stale,
            tentative_p50_ms = percentile(&st.tentative_ms, 50),
            tentative_p90_ms = percentile(&st.tentative_ms, 90),
            commit_p50_ms = percentile(&st.commit_ms, 50),
            commit_p90_ms = percentile(&st.commit_ms, 90),
            listening_ms = outcome
                .listening_after
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            final_ms = outcome
                .final_after_finish
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            "dictation finished"
        );
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        drop(slot);
        if warm {
            let me = self.clone();
            tokio::spawn(async move { me.unload_after_idle(epoch).await });
        }
        let _ = writer.await;
    }

    async fn unload_after_idle(self: Arc<Self>, epoch: u64) {
        tokio::time::sleep(self.config.idle_unload).await;
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        let Ok(mut slot) = self.slot.try_lock() else {
            return;
        };
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return;
        }
        if let Some(engine) = slot.engine.take() {
            let how = engine.shutdown(UNLOAD_WAIT).await;
            tracing::info!(
                kind = "engine_unloaded",
                reason = "idle",
                how,
                "engine unloaded"
            );
        }
        slot.loaded = None;
        self.set_status(|s| {
            s.warm = false;
            s.mode = None;
        });
    }

    fn set_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap());
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum First {
    Start(Start),
    Health,
}

#[derive(Deserialize)]
struct Start {
    #[serde(default)]
    protocol: u32,
    sample_rate: u32,
    format: String,
    #[serde(default)]
    client: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Control {
    Finish,
    Cancel,
}

fn error_json(code: &str) -> serde_json::Value {
    json!({"type": "error", "code": code})
}

/// Читатель сокета: проверка кадров и порядка сообщений.
async fn reader_task(mut rd: OwnedReadHalf, tx: mpsc::Sender<ClientMsg>) {
    let mut finished = false;
    loop {
        let msg = match frame::read_frame(&mut rd, ReadLimits::CLIENT).await {
            Err(FrameError::Closed) | Err(FrameError::Io(_)) => return,
            Err(FrameError::TooLarge { .. }) => ClientMsg::Error("bad_audio"),
            Err(FrameError::UnknownKind(_)) => ClientMsg::Error("bad_request"),
            Ok(Frame::Audio(bytes)) => {
                if finished {
                    ClientMsg::Error("bad_request")
                } else if bytes.is_empty() || bytes.len() % 2 != 0 {
                    ClientMsg::Error("bad_audio")
                } else {
                    ClientMsg::Audio(frame::pcm_from_bytes(&bytes))
                }
            }
            Ok(Frame::Json(bytes)) => match serde_json::from_slice::<Control>(&bytes) {
                Ok(Control::Finish) if !finished => {
                    finished = true;
                    ClientMsg::Finish
                }
                Ok(Control::Cancel) => ClientMsg::Cancel,
                _ => ClientMsg::Error("bad_request"),
            },
        };
        let stop = matches!(msg, ClientMsg::Error(_) | ClientMsg::Cancel);
        if tx.send(msg).await.is_err() || stop {
            return;
        }
    }
}

#[derive(Default)]
struct Outbox {
    queue: VecDeque<serde_json::Value>,
    text: Option<Update>,
    terminal: Option<serde_json::Value>,
    closed: bool,
}

#[derive(Default)]
struct OutboxShared {
    inner: Mutex<Outbox>,
    notify: Notify,
}

impl OutboxShared {
    fn push(&self, f: impl FnOnce(&mut Outbox)) {
        f(&mut self.inner.lock().unwrap());
        self.notify.notify_one();
    }
    fn close(&self) {
        self.push(|o| o.closed = true);
    }
}

struct SocketSink {
    outbox: Arc<OutboxShared>,
}

impl Sink for SocketSink {
    fn listening(&mut self, mode: Mode) {
        self.outbox.push(|o| {
            o.queue
                .push_back(json!({"type":"state","state":"listening","mode":mode.as_str()}))
        });
    }
    fn loading(&mut self) {
        self.outbox.push(|o| {
            o.queue
                .push_back(json!({"type":"state","state":"loading","mode":null}))
        });
    }
    fn text(&mut self, update: Update) {
        self.outbox.push(|o| o.text = Some(update));
    }
    fn final_text(&mut self, text: String, reason: FinalReason) {
        self.outbox.push(|o| {
            o.terminal = Some(json!({"type":"final","text":text,"reason":reason.as_str()}))
        });
    }
    fn error(&mut self, code: &'static str) {
        self.outbox.push(|o| {
            if o.terminal.is_none() {
                o.terminal = Some(error_json(code));
            }
        });
    }
}

/// Писатель: состояния по порядку, текст — только последний, затем итог или ошибка.
async fn writer_task(
    outbox: Arc<OutboxShared>,
    mut wr: OwnedWriteHalf,
    timeout: Duration,
    failed: watch::Sender<bool>,
) {
    let mut seq = 0u64;
    loop {
        let (msg, last) = {
            let mut o = outbox.inner.lock().unwrap();
            if let Some(m) = o.queue.pop_front() {
                (Some(m), false)
            } else if let Some(u) = o.text.take() {
                seq += 1;
                (
                    Some(
                        json!({"type":"text","committed":u.committed,"tentative":u.tentative,"seq":seq}),
                    ),
                    false,
                )
            } else if let Some(t) = o.terminal.take() {
                (Some(t), true)
            } else if o.closed {
                (None, true)
            } else {
                (None, false)
            }
        };
        match msg {
            Some(m) => {
                let ok = matches!(
                    tokio::time::timeout(timeout, frame::write_json(&mut wr, &m)).await,
                    Ok(Ok(()))
                );
                if !ok {
                    let _ = failed.send(true);
                    return;
                }
                if last {
                    let _ = wr.shutdown().await;
                    return;
                }
            }
            None if last => {
                let _ = wr.shutdown().await;
                return;
            }
            None => outbox.notify.notified().await,
        }
    }
}

/// Клиент `health`: ответ службы или ошибка соединения.
pub async fn query_health(socket: &Path) -> std::io::Result<serde_json::Value> {
    let mut stream = UnixStream::connect(socket).await?;
    frame::write_frame(&mut stream, KIND_JSON, br#"{"type":"health"}"#).await?;
    let f = tokio::time::timeout(
        Duration::from_secs(5),
        frame::read_frame(&mut stream, ReadLimits::SERVICE),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "нет ответа"))?
    .map_err(std::io::Error::other)?;
    match f {
        Frame::Json(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
        Frame::Audio(_) => Err(std::io::Error::other("неожиданный кадр")),
    }
}
