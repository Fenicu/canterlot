//! Ложный движок для тестов (скрытый флаг `--fake-engine`): VAD по энергии, текст —
//! длительность звука. Настройка строкой вида `load=200,decode=50,stall,cpu,fail_load`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::engine::{Asr, LoadResult, Loaded, Loader, Mode, Vad};
use crate::live::{Decoded, Word};

pub const FRAME: usize = 512;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FakeSpec {
    pub load_ms: u64,
    pub decode_ms: u64,
    /// Распознавание висит и не слушает отмену (нативный прогон, который не вернулся).
    pub stall: bool,
    pub cpu: bool,
    pub fail_load: bool,
    pub fail_decode: bool,
    /// Обработчик atexit, который роняет процесс, как деструкторы ggml-cuda после
    /// остановки рантайма CUDA: выход движка обязан их не запускать.
    pub atexit_abort: bool,
}

impl FakeSpec {
    pub fn parse(s: &str) -> Result<FakeSpec, String> {
        let mut spec = FakeSpec::default();
        for part in s.split(',').filter(|p| !p.is_empty()) {
            match part.split_once('=') {
                Some(("load", v)) => spec.load_ms = v.parse().map_err(|_| part.to_owned())?,
                Some(("decode", v)) => spec.decode_ms = v.parse().map_err(|_| part.to_owned())?,
                None if part == "stall" => spec.stall = true,
                None if part == "cpu" => spec.cpu = true,
                None if part == "fail_load" => spec.fail_load = true,
                None if part == "fail_decode" => spec.fail_decode = true,
                None if part == "atexit_abort" => spec.atexit_abort = true,
                _ => return Err(format!("неизвестная настройка ложного движка: {part}")),
            }
        }
        Ok(spec)
    }
}

pub struct FakeLoader(pub FakeSpec);

impl Loader for FakeLoader {
    fn load(self: Box<Self>) -> LoadResult {
        let spec = self.0;
        std::thread::sleep(Duration::from_millis(spec.load_ms));
        if spec.fail_load {
            return Err("load_failed".into());
        }
        if spec.atexit_abort {
            // SAFETY: регистрация обработчика без состояния.
            unsafe { libc::atexit(abort_at_exit) };
        }
        let loaded = Loaded {
            mode: if spec.cpu { Mode::Cpu } else { Mode::Gpu },
            load_ms: spec.load_ms as u32,
            hash_ms: 0,
            vad_frame: FRAME as u32,
            gpu_error: None,
        };
        Ok((
            loaded,
            Box::new(EnergyVad::default()),
            Box::new(FakeAsr {
                spec,
                cancel: Arc::new(AtomicBool::new(false)),
            }),
        ))
    }
}

extern "C" fn abort_at_exit() {
    std::process::abort();
}

/// Кадр — речь, если средняя амплитуда больше 1000.
#[derive(Default)]
pub struct EnergyVad {
    rest: Vec<i16>,
}

impl Vad for EnergyVad {
    fn reset(&mut self) {
        self.rest.clear();
    }
    fn feed(&mut self, pcm: &[i16]) -> Result<Vec<f32>, &'static str> {
        self.rest.extend_from_slice(pcm);
        let whole = self.rest.len() / FRAME * FRAME;
        let probs = self.rest[..whole]
            .chunks(FRAME)
            .map(|f| {
                let mean = f.iter().map(|s| (*s as i64).abs()).sum::<i64>() / FRAME as i64;
                if mean > 1000 { 0.9 } else { 0.05 }
            })
            .collect();
        self.rest.drain(..whole);
        Ok(probs)
    }
    fn flush(&mut self) -> Result<Vec<f32>, &'static str> {
        if self.rest.is_empty() {
            return Ok(Vec::new());
        }
        let missing = FRAME - self.rest.len();
        self.feed(&vec![0; missing])
    }
}

struct FakeAsr {
    spec: FakeSpec,
    cancel: Arc<AtomicBool>,
}

impl Asr for FakeAsr {
    fn canceller(&self) -> Arc<dyn Fn() + Send + Sync> {
        let flag = self.cancel.clone();
        Arc::new(move || flag.store(true, Ordering::SeqCst))
    }

    fn reset_cancel(&mut self) {
        self.cancel.store(false, Ordering::SeqCst);
    }

    fn decode(&mut self, pcm: &[i16]) -> Result<Decoded, &'static str> {
        let started = Instant::now();
        if self.spec.stall {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        while started.elapsed() < Duration::from_millis(self.spec.decode_ms) {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("aborted");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.spec.fail_decode {
            return Err("failed");
        }
        let ms = (pcm.len() / 16) as i64;
        let text = format!("s{ms}");
        Ok(Decoded {
            words: vec![Word {
                t0_ms: 0,
                t1_ms: ms,
                text: text.clone(),
            }],
            text,
            decode_ms: started.elapsed().as_millis() as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parses() {
        let s = FakeSpec::parse("load=200,decode=50,stall,cpu").unwrap();
        assert_eq!(s.load_ms, 200);
        assert_eq!(s.decode_ms, 50);
        assert!(s.stall && s.cpu && !s.fail_load);
        assert!(FakeSpec::parse("bogus").is_err());
    }

    #[test]
    fn energy_vad_scores_whole_frames_across_feeds() {
        let mut v = EnergyVad::default();
        assert!(v.feed(&[5000; 300]).unwrap().is_empty());
        assert_eq!(v.feed(&[5000; 300]).unwrap(), vec![0.9]);
        assert_eq!(v.feed(&[0; 512]).unwrap(), vec![0.05]);
        assert_eq!(v.flush().unwrap().len(), 1);
    }
}
