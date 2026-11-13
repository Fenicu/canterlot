//! Настоящий движок на transcribe.cpp: Parakeet на CUDA или CPU, Silero VAD на CPU.

use std::sync::Arc;
use std::time::Instant;

use transcribe_cpp::{
    Backend, CancelToken, ErrorKind, Model, ModelOptions, RunOptions, Session, SessionOptions,
    TimestampKind, VadSession, VadSessionOptions,
};

use crate::config::{Config, Device, verify_models};
use crate::engine::{Asr, LoadResult, Loaded, Loader, Mode, Vad};
use crate::live::{Decoded, Word};

/// Сообщения transcribe.cpp: только warn и error, обрезанные, с префиксом `transcribe:`.
struct NativeLog;

const NATIVE_LOG_MAX: usize = 300;

impl log::Log for NativeLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let text = record.args().to_string();
        let cut: String = text
            .chars()
            .filter(|c| *c != '\n')
            .take(NATIVE_LOG_MAX)
            .collect();
        eprintln!(
            "transcribe: {} {cut}",
            record.level().as_str().to_lowercase()
        );
    }
    fn flush(&self) {}
}

static NATIVE_LOG: NativeLog = NativeLog;

/// Направляет журнал transcribe.cpp и ggml через обработчик выше. Один раз до загрузки.
pub fn init_native_logging() {
    if log::set_logger(&NATIVE_LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    transcribe_cpp::init_logging();
}

pub struct NativeLoader {
    pub config: Config,
}

impl Loader for NativeLoader {
    fn load(self: Box<Self>) -> LoadResult {
        let cfg = self.config;
        let started = Instant::now();
        verify_models(&cfg).map_err(|_| "model_mismatch".to_owned())?;
        let hash_ms = started.elapsed().as_millis() as u32;

        let vad_model = Model::load_with(
            &cfg.vad.path,
            &ModelOptions {
                backend: Backend::Cpu,
                ..Default::default()
            },
        )
        .map_err(|e| format!("vad_{}", code(e.kind())))?;
        let vad_frame = vad_model
            .vad_info()
            .map_err(|e| format!("vad_{}", code(e.kind())))?
            .frame_samples as u32;
        let vad = vad_model
            .vad_session_with(&VadSessionOptions { n_threads: 1 })
            .map_err(|e| format!("vad_{}", code(e.kind())))?;

        let threads = cfg.threads as i32;
        let (session, mode, gpu_error) = match cfg.device {
            Device::Cpu => (load_asr(&cfg, Backend::Cpu, threads)?, Mode::Cpu, None),
            Device::Gpu => (load_asr(&cfg, Backend::Cuda, threads)?, Mode::Gpu, None),
            Device::Auto => match load_asr(&cfg, Backend::Cuda, threads) {
                Ok(s) => (s, Mode::Gpu, None),
                Err(gpu) => (load_asr(&cfg, Backend::Cpu, threads)?, Mode::Cpu, Some(gpu)),
            },
        };
        let token = CancelToken::new();
        let mut session = session;
        session.set_cancel_token(&token);
        let loaded = Loaded {
            mode,
            load_ms: started.elapsed().as_millis() as u32,
            hash_ms,
            vad_frame,
            gpu_error,
        };
        Ok((
            loaded,
            Box::new(NativeVad { session: vad }),
            Box::new(NativeAsr { session, token }),
        ))
    }
}

/// Загрузка Parakeet на заданный бэкенд и прогрев секундой тишины: так нехватка
/// памяти GPU видна при загрузке, а не на первой фразе.
fn load_asr(cfg: &Config, backend: Backend, threads: i32) -> Result<Session, String> {
    let prefix = if backend == Backend::Cpu {
        "cpu"
    } else {
        "gpu"
    };
    if backend != Backend::Cpu && !transcribe_cpp::backend_available(backend) {
        return Err(format!("{prefix}_unavailable"));
    }
    let model = Model::load_with(
        &cfg.model.path,
        &ModelOptions {
            backend,
            ..Default::default()
        },
    )
    .map_err(|e| format!("{prefix}_{}", code(e.kind())))?;
    let mut session = model
        .session_with(&SessionOptions {
            n_threads: threads,
            ..Default::default()
        })
        .map_err(|e| format!("{prefix}_{}", code(e.kind())))?;
    session
        .run(&vec![0.0f32; 16_000], &run_options())
        .map_err(|e| format!("{prefix}_warmup_{}", code(e.kind())))?;
    Ok(session)
}

fn run_options() -> RunOptions {
    RunOptions {
        timestamps: TimestampKind::Word,
        ..Default::default()
    }
}

/// Код ошибки без её текста: `Error::Aborted` несёт частичную расшифровку.
fn code(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::OutOfMemory => "oom",
        ErrorKind::Aborted => "aborted",
        ErrorKind::Busy => "busy",
        _ => "failed",
    }
}

fn to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|s| *s as f32 / 32768.0).collect()
}

struct NativeVad {
    session: VadSession,
}

impl Vad for NativeVad {
    fn reset(&mut self) {
        self.session.stream_reset();
    }
    fn feed(&mut self, pcm: &[i16]) -> Result<Vec<f32>, &'static str> {
        self.session
            .stream_feed(&to_f32(pcm))
            .map(|r| r.probs)
            .map_err(|e| code(e.kind()))
    }
    fn flush(&mut self) -> Result<Vec<f32>, &'static str> {
        self.session
            .stream_flush()
            .map(|r| r.probs)
            .map_err(|e| code(e.kind()))
    }
}

struct NativeAsr {
    session: Session,
    token: CancelToken,
}

impl Asr for NativeAsr {
    fn canceller(&self) -> Arc<dyn Fn() + Send + Sync> {
        let token = self.token.clone();
        Arc::new(move || token.cancel())
    }

    fn reset_cancel(&mut self) {
        self.token.reset();
    }

    fn decode(&mut self, pcm: &[i16]) -> Result<Decoded, &'static str> {
        let started = Instant::now();
        let t = self
            .session
            .run(&to_f32(pcm), &run_options())
            .map_err(|e| code(e.kind()))?;
        Ok(Decoded {
            words: t
                .words
                .into_iter()
                .map(|w| Word {
                    t0_ms: w.t0_ms,
                    t1_ms: w.t1_ms,
                    text: w.text,
                })
                .collect(),
            text: t.text,
            decode_ms: started.elapsed().as_millis() as u32,
        })
    }
}
