//! Движок: VAD и распознавание. Внутри процесса движок — два потока (VAD и
//! распознавание) за диспетчером; надзиратель говорит с ним через дочерний процесс
//! (`canterlot-speech engine`) теми же запросами и событиями, закодированными кадрами.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::mpsc;

use crate::frame::{self, Frame, KIND_AUDIO, KIND_JSON, ReadLimits};
use crate::live::{Decoded, Word};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Gpu,
    Cpu,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Gpu => "gpu",
            Mode::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Loaded {
    pub mode: Mode,
    pub load_ms: u32,
    pub hash_ms: u32,
    pub vad_frame: u32,
    /// Причина, по которой GPU не подошёл (код), если режим CPU при `device = "auto"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_error: Option<String>,
}

pub enum Request {
    Load,
    VadReset,
    VadFeed(Vec<i16>),
    VadFlush,
    Decode { id: u64, pcm: Vec<i16> },
    Cancel { id: u64 },
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Request::Load => write!(f, "Load"),
            Request::VadReset => write!(f, "VadReset"),
            Request::VadFeed(p) => write!(f, "VadFeed({} samples)", p.len()),
            Request::VadFlush => write!(f, "VadFlush"),
            Request::Decode { id, pcm } => write!(f, "Decode({id}, {} samples)", pcm.len()),
            Request::Cancel { id } => write!(f, "Cancel({id})"),
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Loaded(Loaded),
    LoadFailed(String),
    Vad(Result<Vec<f32>, String>),
    Decoded {
        id: u64,
        result: Result<Decoded, String>,
    },
    Exited,
}

pub trait Vad: Send {
    fn reset(&mut self);
    fn feed(&mut self, pcm: &[i16]) -> Result<Vec<f32>, &'static str>;
    fn flush(&mut self) -> Result<Vec<f32>, &'static str>;
}

pub trait Asr: Send {
    /// Отмена текущего прогона из другого потока.
    fn canceller(&self) -> Arc<dyn Fn() + Send + Sync>;
    /// Сбросить отмену перед следующим прогоном.
    fn reset_cancel(&mut self);
    fn decode(&mut self, pcm: &[i16]) -> Result<Decoded, &'static str>;
}

pub type LoadResult = Result<(Loaded, Box<dyn Vad>, Box<dyn Asr>), String>;

pub trait Loader: Send + 'static {
    fn load(self: Box<Self>) -> LoadResult;
}

/// Связь с движком: запросы туда, события обратно.
pub struct EngineLink {
    pub tx: mpsc::UnboundedSender<Request>,
    pub rx: mpsc::UnboundedReceiver<Event>,
}

/// Движок в этом процессе: диспетчер и два рабочих потока.
pub fn spawn_inprocess(loader: Box<dyn Loader>) -> EngineLink {
    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
    std::thread::Builder::new()
        .name("engine-dispatch".into())
        .spawn(move || {
            let mut loader = Some(loader);
            let mut vad_tx: Option<std::sync::mpsc::Sender<Request>> = None;
            let mut asr_tx: Option<std::sync::mpsc::Sender<(u64, Vec<i16>)>> = None;
            let mut cancel: Option<Arc<dyn Fn() + Send + Sync>> = None;
            let current = Arc::new(AtomicU64::new(0));
            while let Some(req) = req_rx.blocking_recv() {
                match req {
                    Request::Load => {
                        let Some(l) = loader.take() else { continue };
                        match l.load() {
                            Ok((loaded, vad, asr)) => {
                                cancel = Some(asr.canceller());
                                vad_tx = Some(spawn_vad(vad, ev_tx.clone()));
                                asr_tx = Some(spawn_asr(asr, ev_tx.clone(), current.clone()));
                                let _ = ev_tx.send(Event::Loaded(loaded));
                            }
                            Err(code) => {
                                let _ = ev_tx.send(Event::LoadFailed(code));
                            }
                        }
                    }
                    Request::VadReset | Request::VadFeed(_) | Request::VadFlush => {
                        if let Some(tx) = &vad_tx {
                            let _ = tx.send(req);
                        }
                    }
                    Request::Decode { id, pcm } => {
                        if let Some(tx) = &asr_tx {
                            let _ = tx.send((id, pcm));
                        }
                    }
                    Request::Cancel { id } => {
                        if current.load(Ordering::SeqCst) == id
                            && let Some(c) = &cancel
                        {
                            c();
                        }
                    }
                }
            }
        })
        .expect("поток диспетчера движка");
    EngineLink {
        tx: req_tx,
        rx: ev_rx,
    }
}

fn spawn_vad(
    mut vad: Box<dyn Vad>,
    ev: mpsc::UnboundedSender<Event>,
) -> std::sync::mpsc::Sender<Request> {
    let (tx, rx) = std::sync::mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("engine-vad".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                let r = match req {
                    Request::VadReset => {
                        vad.reset();
                        continue;
                    }
                    Request::VadFeed(pcm) => vad.feed(&pcm),
                    Request::VadFlush => vad.flush(),
                    _ => continue,
                };
                let _ = ev.send(Event::Vad(r.map_err(str::to_owned)));
            }
        })
        .expect("поток VAD");
    tx
}

fn spawn_asr(
    mut asr: Box<dyn Asr>,
    ev: mpsc::UnboundedSender<Event>,
    current: Arc<AtomicU64>,
) -> std::sync::mpsc::Sender<(u64, Vec<i16>)> {
    let (tx, rx) = std::sync::mpsc::channel::<(u64, Vec<i16>)>();
    std::thread::Builder::new()
        .name("engine-asr".into())
        .spawn(move || {
            while let Ok((id, pcm)) = rx.recv() {
                asr.reset_cancel();
                current.store(id, Ordering::SeqCst);
                let result = asr.decode(&pcm).map_err(str::to_owned);
                current.store(0, Ordering::SeqCst);
                let _ = ev.send(Event::Decoded { id, result });
            }
        })
        .expect("поток распознавания");
    tx
}

// --- провод между надзирателем и дочерним процессом ---

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ToChild {
    Load,
    VadReset,
    VadFlush,
    /// Следом идёт звуковой кадр с PCM задания.
    Decode {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum FromChild {
    Loaded(Loaded),
    LoadFailed {
        code: String,
    },
    Vad {
        probs: Vec<f32>,
    },
    VadFailed {
        code: String,
    },
    Decoded {
        id: u64,
        text: String,
        words: Vec<(i64, i64, String)>,
        ms: u32,
    },
    DecodeFailed {
        id: u64,
        code: String,
    },
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("сериализация кадра движка")
}

/// Сторона дочернего процесса: кадры из stdin в движок, события в stdout.
/// Возвращается, когда stdin закрыт.
pub async fn serve_child<R, W>(mut input: R, mut output: W, link: EngineLink)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let EngineLink { tx, mut rx } = link;
    let writer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let msg = match ev {
                Event::Loaded(l) => FromChild::Loaded(l),
                Event::LoadFailed(code) => FromChild::LoadFailed { code },
                Event::Vad(Ok(probs)) => FromChild::Vad { probs },
                Event::Vad(Err(code)) => FromChild::VadFailed { code },
                Event::Decoded { id, result: Ok(d) } => FromChild::Decoded {
                    id,
                    text: d.text,
                    words: d
                        .words
                        .into_iter()
                        .map(|w| (w.t0_ms, w.t1_ms, w.text))
                        .collect(),
                    ms: d.decode_ms,
                },
                Event::Decoded {
                    id,
                    result: Err(code),
                } => FromChild::DecodeFailed { id, code },
                Event::Exited => break,
            };
            if frame::write_frame(&mut output, KIND_JSON, &json(&msg))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let mut pending_decode: Option<u64> = None;
    while let Ok(f) = frame::read_frame(&mut input, ReadLimits::ENGINE).await {
        let req = match f {
            Frame::Json(bytes) => match serde_json::from_slice::<ToChild>(&bytes) {
                Ok(ToChild::Load) => Request::Load,
                Ok(ToChild::VadReset) => Request::VadReset,
                Ok(ToChild::VadFlush) => Request::VadFlush,
                Ok(ToChild::Decode { id }) => {
                    pending_decode = Some(id);
                    continue;
                }
                Ok(ToChild::Cancel { id }) => Request::Cancel { id },
                Err(_) => break,
            },
            Frame::Audio(bytes) => {
                let pcm = frame::pcm_from_bytes(&bytes);
                match pending_decode.take() {
                    Some(id) => Request::Decode { id, pcm },
                    None => Request::VadFeed(pcm),
                }
            }
        };
        if tx.send(req).is_err() {
            break;
        }
    }
    drop(tx);
    writer.abort();
}

/// Движок в дочернем процессе, со стороны надзирателя.
pub struct ChildEngine {
    pub link: EngineLink,
    child: tokio::process::Child,
    pub pid: u32,
}

/// Что запускать дочерним процессом.
#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Переменные окружения, которые получает движок; остальное (в том числе `TRANSCRIBE_*`) отрезано.
const CHILD_ENV: &[&str] = &["CUDA_VISIBLE_DEVICES", "LD_LIBRARY_PATH", "RUST_BACKTRACE"];

impl ChildEngine {
    pub fn spawn(spec: &ChildSpec) -> std::io::Result<ChildEngine> {
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args)
            .env_clear()
            .env("CUDA_CACHE_DISABLE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in CHILD_ENV {
            if let Some(v) = std::env::var_os(name) {
                cmd.env(name, v);
            }
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0);
        let mut stdin = child.stdin.take().expect("stdin движка");
        let mut stdout = child.stdout.take().expect("stdout движка");
        let stderr = child.stderr.take().expect("stderr движка");
        tokio::spawn(filter_stderr(stderr));

        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<Request>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<Event>();
        tokio::spawn(async move {
            while let Some(req) = req_rx.recv().await {
                let r = match req {
                    Request::Load => write_ctl(&mut stdin, &ToChild::Load).await,
                    Request::VadReset => write_ctl(&mut stdin, &ToChild::VadReset).await,
                    Request::VadFlush => write_ctl(&mut stdin, &ToChild::VadFlush).await,
                    Request::Cancel { id } => write_ctl(&mut stdin, &ToChild::Cancel { id }).await,
                    Request::VadFeed(pcm) => {
                        frame::write_frame(&mut stdin, KIND_AUDIO, &frame::pcm_to_bytes(&pcm)).await
                    }
                    Request::Decode { id, pcm } => {
                        match write_ctl(&mut stdin, &ToChild::Decode { id }).await {
                            Ok(()) => {
                                frame::write_frame(
                                    &mut stdin,
                                    KIND_AUDIO,
                                    &frame::pcm_to_bytes(&pcm),
                                )
                                .await
                            }
                            Err(e) => Err(e),
                        }
                    }
                };
                if r.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Ok(f) = frame::read_frame(&mut stdout, ReadLimits::ENGINE).await {
                let Frame::Json(bytes) = f else { break };
                let Ok(msg) = serde_json::from_slice::<FromChild>(&bytes) else {
                    break;
                };
                let ev = match msg {
                    FromChild::Loaded(l) => Event::Loaded(l),
                    FromChild::LoadFailed { code } => Event::LoadFailed(code),
                    FromChild::Vad { probs } => Event::Vad(Ok(probs)),
                    FromChild::VadFailed { code } => Event::Vad(Err(code)),
                    FromChild::Decoded {
                        id,
                        text,
                        words,
                        ms,
                    } => Event::Decoded {
                        id,
                        result: Ok(Decoded {
                            text,
                            words: words
                                .into_iter()
                                .map(|(t0_ms, t1_ms, text)| Word { t0_ms, t1_ms, text })
                                .collect(),
                            decode_ms: ms,
                        }),
                    },
                    FromChild::DecodeFailed { id, code } => Event::Decoded {
                        id,
                        result: Err(code),
                    },
                };
                if ev_tx.send(ev).is_err() {
                    return;
                }
            }
            let _ = ev_tx.send(Event::Exited);
        });
        Ok(ChildEngine {
            link: EngineLink {
                tx: req_tx,
                rx: ev_rx,
            },
            child,
            pid,
        })
    }

    pub async fn kill(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }

    /// Мягкая остановка: закрыть stdin, подождать выхода, иначе SIGKILL.
    pub async fn shutdown(mut self, wait: Duration) -> &'static str {
        let EngineLink { tx, rx } = self.link;
        drop(tx);
        drop(rx);
        match tokio::time::timeout(wait, self.child.wait()).await {
            Ok(_) => "exited",
            Err(_) => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
                "killed"
            }
        }
    }

    pub fn try_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

async fn write_ctl<W: AsyncWrite + Unpin>(w: &mut W, msg: &ToChild) -> std::io::Result<()> {
    frame::write_frame(w, KIND_JSON, &json(msg)).await
}

/// Префиксы строк stderr движка, которые можно пропустить в журнал.
const STDERR_ALLOW: &[&str] = &["transcribe:", "ggml", "GGML", "CUDA", "cuda"];
const STDERR_MAX: usize = 300;

pub fn stderr_line_allowed(line: &str) -> bool {
    STDERR_ALLOW.iter().any(|p| line.starts_with(p))
}

async fn filter_stderr(stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    let mut dropped = 0u64;
    while let Ok(Some(line)) = lines.next_line().await {
        if stderr_line_allowed(&line) {
            let cut: String = line.chars().take(STDERR_MAX).collect();
            tracing::warn!(kind = "engine_stderr", line = %cut, "engine native diagnostics");
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        tracing::info!(
            kind = "engine_stderr_dropped",
            count = dropped,
            "engine stderr lines dropped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_filter_keeps_only_diagnostics() {
        assert!(stderr_line_allowed("ggml_cuda_init: found 1 CUDA devices"));
        assert!(stderr_line_allowed("transcribe: model load failed"));
        assert!(stderr_line_allowed("CUDA error: out of memory"));
        assert!(!stderr_line_allowed("Привет, это текст"));
        assert!(!stderr_line_allowed("  ggml indented"));
    }
}
