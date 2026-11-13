//! `probe`: живой алгоритм в одном процессе на WAV-файле с замерами.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::Config;
use crate::dictation::{ClientMsg, Dictation, FinalReason, Sink, percentile};
use crate::engine::{Event, Loader, Mode, Request, spawn_inprocess};
use crate::live::{JobKind, Params, RATE, Update};

pub struct ProbeOptions {
    pub realtime: bool,
    pub json: bool,
    pub text: bool,
}

/// WAV в моно 16 кГц s16: смешение каналов и линейное пересэмплирование.
pub fn read_wav(path: &Path) -> Result<Vec<i16>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let out = resample(&mono, spec.sample_rate, RATE as u32);
    Ok(out
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect())
}

pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let n = (input.len() as u64 * to as u64 / from as u64) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = input[j.min(input.len() - 1)];
            let b = input[(j + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

struct Record {
    wall_ms: u64,
    kind: JobKind,
    decode_ms: u32,
    word_end_ms: Option<u64>,
    committed_chars: usize,
    tentative_chars: usize,
}

struct ProbeSink {
    audio_start: Instant,
    records: Vec<Record>,
    final_text: Option<String>,
    final_at: Option<Instant>,
    error: Option<&'static str>,
}

impl Sink for ProbeSink {
    fn listening(&mut self, _mode: Mode) {}
    fn loading(&mut self) {}
    fn text(&mut self, u: Update) {
        self.records.push(Record {
            wall_ms: self.audio_start.elapsed().as_millis() as u64,
            kind: u.info.kind,
            decode_ms: u.info.decode_ms,
            word_end_ms: u.info.newest_word_end.map(|s| s * 1000 / RATE),
            committed_chars: u.committed.chars().count(),
            tentative_chars: u.tentative.chars().count(),
        });
    }
    fn final_text(&mut self, text: String, _reason: FinalReason) {
        self.final_text = Some(text);
        self.final_at = Some(Instant::now());
    }
    fn error(&mut self, code: &'static str) {
        self.error = Some(code);
    }
}

pub async fn run(
    config: Config,
    loader: Box<dyn Loader>,
    wav: &Path,
    opts: ProbeOptions,
) -> Result<bool, String> {
    let pcm = read_wav(wav)?;
    let audio_ms = pcm.len() as u64 * 1000 / RATE;
    let emit = |v: serde_json::Value, human: String| {
        if opts.json {
            println!("{v}");
        } else {
            println!("{human}");
        }
    };

    // Память всего устройства: в LXC nvidia-smi не сопоставляет PID процессов.
    let gpu_base = tokio::task::spawn_blocking(gpu_used_mib)
        .await
        .ok()
        .flatten();
    let gpu = GpuSampler::start(gpu_base.is_some());
    let mut link = spawn_inprocess(loader);
    let load_started = Instant::now();
    let _ = link.tx.send(Request::Load);
    let loaded = loop {
        match link.rx.recv().await {
            Some(Event::Loaded(l)) => break l,
            Some(Event::LoadFailed(code)) => return Err(format!("загрузка не удалась: {code}")),
            None => return Err("движок завершился".into()),
            Some(_) => {}
        }
    };
    let cold_ms = load_started.elapsed().as_millis() as u64;
    let gpu_after_load = tokio::task::spawn_blocking(gpu_used_mib)
        .await
        .ok()
        .flatten();
    emit(
        json!({"event":"load","mode":loaded.mode.as_str(),"cold_load_ms":cold_ms,
               "engine_load_ms":loaded.load_ms,"hash_ms":loaded.hash_ms,
               "gpu_error":loaded.gpu_error,"gpu_used_base_mib":gpu_base,
               "gpu_used_after_load_mib":gpu_after_load,"audio_ms":audio_ms}),
        format!(
            "загрузка: режим {}, холодная {} мс (sha256 {} мс){}, GPU занято {} → {} МиБ, звук {} мс",
            loaded.mode.as_str(),
            cold_ms,
            loaded.hash_ms,
            loaded
                .gpu_error
                .as_ref()
                .map(|e| format!(", GPU не подошёл: {e}"))
                .unwrap_or_default(),
            gpu_base.map_or("?".into(), |m| m.to_string()),
            gpu_after_load.map_or("?".into(), |m| m.to_string()),
            audio_ms
        ),
    );

    let mut limits = config.limits.clone();
    limits.max_audio = Duration::from_millis(audio_ms + 1000).max(limits.max_audio);
    limits.lifetime = limits.max_audio + Duration::from_secs(600);
    if !opts.realtime {
        limits.max_lead = Duration::from_secs(1 << 30);
        limits.idle = Duration::from_secs(600);
    }
    let (tx, rx) = mpsc::channel(4096);
    let audio_start = Instant::now();
    let realtime = opts.realtime;
    let feeder = tokio::spawn(async move {
        for (k, chunk) in pcm.chunks(1600).enumerate() {
            if realtime {
                tokio::time::sleep_until(audio_start + Duration::from_millis(100 * (k as u64 + 1)))
                    .await;
            }
            if tx.send(ClientMsg::Audio(chunk.to_vec())).await.is_err() {
                return Instant::now();
            }
        }
        let end = Instant::now();
        let _ = tx.send(ClientMsg::Finish).await;
        // Канал держится открытым до конца диктовки: закрытие означало бы отмену.
        tx.closed().await;
        end
    });
    let d = Dictation {
        link: &mut link,
        loaded: Some(loaded.clone()),
        params: Params::from_config(&config.live),
        limits,
        started: audio_start,
        writer_failed: None,
        stop: None,
        sink: ProbeSink {
            audio_start,
            records: Vec::new(),
            final_text: None,
            final_at: None,
            error: None,
        },
    };
    let (outcome, sink) = d.run(rx).await;
    let audio_end = feeder.await.map_err(|e| e.to_string())?;

    let mut lags_t = Vec::new();
    let mut lags_c = Vec::new();
    for r in &sink.records {
        let lag = r.word_end_ms.map(|w| r.wall_ms as i64 - w as i64);
        if let Some(l) = lag
            && l >= 0
        {
            match r.kind {
                JobKind::Tentative => lags_t.push(l as u32),
                _ => lags_c.push(l as u32),
            }
        }
        let kind = match r.kind {
            JobKind::Tentative => "tentative",
            JobKind::Commit => "commit",
            JobKind::Final => "final",
        };
        emit(
            json!({"event":"update","kind":kind,"wall_ms":r.wall_ms,"word_end_ms":r.word_end_ms,
                   "lag_ms":lag,"decode_ms":r.decode_ms,"committed_chars":r.committed_chars,
                   "tentative_chars":r.tentative_chars}),
            format!(
                "{:>7} мс  {:<9} слово до {:>7} мс  отставание {:>6} мс  распознавание {:>5} мс  символов {}+{}",
                r.wall_ms,
                kind,
                r.word_end_ms.map_or("-".into(), |w| w.to_string()),
                lag.map_or("-".into(), |l| l.to_string()),
                r.decode_ms,
                r.committed_chars,
                r.tentative_chars
            ),
        );
    }
    let final_ms = sink
        .final_at
        .map(|f| f.saturating_duration_since(audio_end).as_millis() as u64);
    let st = &outcome.stats;
    let rss = peak_rss_kib();
    let gpu_peak = gpu.stop().await;
    let gpu_delta = gpu_base.zip(gpu_peak).map(|(b, p)| p.saturating_sub(b));
    let summary = json!({
        "event": "summary",
        "outcome": outcome.result,
        "error": sink.error,
        "mode": loaded.mode.as_str(),
        "realtime": opts.realtime,
        "audio_ms": audio_ms,
        "segments": st.segments,
        "forced_cuts": st.forced_cuts,
        "tentatives": st.tentatives,
        "commits": st.commits,
        "stale": st.stale,
        "tentative_decode_ms": {"p50": percentile(&st.tentative_ms, 50), "p90": percentile(&st.tentative_ms, 90), "max": st.tentative_ms.iter().max()},
        "commit_decode_ms": {"p50": percentile(&st.commit_ms, 50), "p90": percentile(&st.commit_ms, 90), "max": st.commit_ms.iter().max()},
        "tentative_lag_ms": {"p50": percentile(&lags_t, 50), "p90": percentile(&lags_t, 90), "n": lags_t.len()},
        "commit_lag_ms": {"p50": percentile(&lags_c, 50), "p90": percentile(&lags_c, 90), "n": lags_c.len()},
        "final_after_audio_ms": final_ms,
        "peak_rss_mib": rss.map(|k| k / 1024),
        "gpu_used_base_mib": gpu_base,
        "gpu_used_peak_mib": gpu_peak,
        "gpu_delta_mib": gpu_delta,
        "final_chars": sink.final_text.as_ref().map(|t| t.chars().count()),
    });
    if opts.json {
        println!("{summary}");
    } else {
        println!(
            "итог: {}, сегментов {} (принудительных {}), хвостов {}, окончательных {}, устаревших {}",
            outcome.result, st.segments, st.forced_cuts, st.tentatives, st.commits, st.stale
        );
        println!(
            "распознавание: хвост p50 {} / p90 {} мс, окончательное p50 {} / p90 {} мс",
            percentile(&st.tentative_ms, 50),
            percentile(&st.tentative_ms, 90),
            percentile(&st.commit_ms, 50),
            percentile(&st.commit_ms, 90)
        );
        println!(
            "отставание от слова: хвост p50 {} / p90 {} мс, окончательный p50 {} / p90 {} мс",
            percentile(&lags_t, 50),
            percentile(&lags_t, 90),
            percentile(&lags_c, 50),
            percentile(&lags_c, 90)
        );
        println!(
            "итог после конца звука: {} мс, пик RSS {} МиБ, GPU занято {} → пик {} МиБ (+{})",
            final_ms.map_or("-".into(), |m| m.to_string()),
            rss.map_or("?".into(), |k| (k / 1024).to_string()),
            gpu_base.map_or("?".into(), |m| m.to_string()),
            gpu_peak.map_or("?".into(), |m| m.to_string()),
            gpu_delta.map_or("?".into(), |m| m.to_string())
        );
    }
    if opts.text
        && let Some(t) = &sink.final_text
    {
        println!("{t}");
    }
    Ok(sink.final_text.is_some())
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Занятая память устройства (МиБ) по `nvidia-smi`, если он есть: первое устройство
/// из `CUDA_VISIBLE_DEVICES` или 0.
fn gpu_used_mib() -> Option<u64> {
    let id = std::env::var("CUDA_VISIBLE_DEVICES")
        .ok()
        .and_then(|v| v.split(',').next()?.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let out = std::process::Command::new("nvidia-smi")
        .arg(format!("--id={id}"))
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_used(&String::from_utf8_lossy(&out.stdout))
}

fn parse_used(stdout: &str) -> Option<u64> {
    stdout.lines().next()?.trim().parse().ok()
}

/// Пик занятой памяти устройства: опрос каждые 250 мс от загрузки до конца прогона.
struct GpuSampler {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Option<u64>>>,
}

impl GpuSampler {
    fn start(enabled: bool) -> GpuSampler {
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let task = enabled.then(|| {
            tokio::spawn(async move {
                let mut peak: Option<u64> = None;
                loop {
                    if let Ok(Some(v)) = tokio::task::spawn_blocking(gpu_used_mib).await {
                        peak = Some(peak.map_or(v, |p| p.max(v)));
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                        _ = rx.wait_for(|s| *s) => return peak,
                    }
                }
            })
        });
        GpuSampler { stop, task }
    }

    async fn stop(mut self) -> Option<u64> {
        let _ = self.stop.send(true);
        self.task.take()?.await.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_used_reads_first_line() {
        assert_eq!(parse_used("6699\n"), Some(6699));
        assert_eq!(parse_used(" 7825 \n100\n"), Some(7825));
        assert_eq!(parse_used("[N/A]\n"), None);
        assert_eq!(parse_used(""), None);
    }

    #[test]
    fn resample_keeps_duration() {
        let x: Vec<f32> = (0..48_000).map(|i| (i as f32 / 100.0).sin()).collect();
        assert_eq!(resample(&x, 48_000, 16_000).len(), 16_000);
        assert_eq!(resample(&x, 16_000, 16_000).len(), 48_000);
    }

    #[test]
    fn read_wav_mixes_and_resamples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 32_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..32_000 {
            w.write_sample(8000i16).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let pcm = read_wav(&path).unwrap();
        assert_eq!(pcm.len(), 16_000);
        assert!((pcm[100] - 4000).abs() < 5, "{}", pcm[100]);
    }
}
