//! Настоящие модели: запускается только вручную, когда заданы
//! CANTERLOT_SPEECH_TEST_MODEL и CANTERLOT_SPEECH_TEST_VAD (пути к GGUF);
//! CANTERLOT_SPEECH_TEST_WAV — запись речи, без неё проверяется тишина.

use std::path::PathBuf;

use canterlot_speech::config::{Config, file_sha256};
use canterlot_speech::native::{NativeLoader, init_native_logging};
use canterlot_speech::probe::{self, ProbeOptions};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "нужны GGUF Parakeet и Silero VAD"]
async fn real_model_live_pipeline() {
    let (Some(model), Some(vad)) = (
        std::env::var_os("CANTERLOT_SPEECH_TEST_MODEL").map(PathBuf::from),
        std::env::var_os("CANTERLOT_SPEECH_TEST_VAD").map(PathBuf::from),
    ) else {
        panic!("задайте CANTERLOT_SPEECH_TEST_MODEL и CANTERLOT_SPEECH_TEST_VAD");
    };
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let config = dir.path().join("speech.toml");
    std::fs::write(
        &config,
        format!(
            "device = \"auto\"\n[model]\npath = \"{}\"\nsha256 = \"{}\"\n[vad]\npath = \"{}\"\nsha256 = \"{}\"\n",
            model.display(),
            file_sha256(&model).unwrap(),
            vad.display(),
            file_sha256(&vad).unwrap()
        ),
    )
    .unwrap();
    let cfg = Config::load(&config).unwrap();
    let wav = match std::env::var_os("CANTERLOT_SPEECH_TEST_WAV") {
        Some(w) => PathBuf::from(w),
        None => {
            let path = dir.path().join("silence.wav");
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for _ in 0..32_000 {
                w.write_sample(0i16).unwrap();
            }
            w.finalize().unwrap();
            path
        }
    };
    init_native_logging();
    let opts = ProbeOptions {
        realtime: false,
        json: true,
        text: false,
    };
    let finished = probe::run(
        cfg.clone(),
        Box::new(NativeLoader { config: cfg }),
        &wav,
        opts,
    )
    .await
    .unwrap();
    assert!(finished, "итог получен");
}
