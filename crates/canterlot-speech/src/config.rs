//! Конфигурация `speech.toml`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const DEFAULT_SOCKET: &str = "/run/canterlot-speech/speech.sock";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
    #[serde(default)]
    pub device: Device,
    #[serde(default = "default_threads")]
    pub threads: u32,
    #[serde(default = "default_idle_unload", with = "humantime_serde")]
    pub idle_unload: Duration,
    pub model: ModelFile,
    pub vad: ModelFile,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub live: LiveConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    #[serde(with = "humantime_serde")]
    pub max_audio: Duration,
    #[serde(with = "humantime_serde")]
    pub lifetime: Duration,
    #[serde(with = "humantime_serde")]
    pub start_deadline: Duration,
    #[serde(with = "humantime_serde")]
    pub idle: Duration,
    #[serde(with = "humantime_serde")]
    pub max_lead: Duration,
    #[serde(with = "humantime_serde")]
    pub queue_wait: Duration,
    #[serde(with = "humantime_serde")]
    pub cancel_grace: Duration,
    #[serde(with = "humantime_serde")]
    pub write_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub finish_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_audio: Duration::from_secs(600),
            lifetime: Duration::from_secs(660),
            start_deadline: Duration::from_secs(30),
            idle: Duration::from_secs(15),
            max_lead: Duration::from_secs(5),
            queue_wait: Duration::from_secs(5),
            cancel_grace: Duration::from_secs(2),
            write_timeout: Duration::from_secs(5),
            finish_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LiveConfig {
    #[serde(with = "humantime_serde")]
    pub tentative_interval: Duration,
    #[serde(with = "humantime_serde")]
    pub pause: Duration,
    #[serde(with = "humantime_serde")]
    pub max_segment: Duration,
    #[serde(with = "humantime_serde")]
    pub forced_window: Duration,
    #[serde(with = "humantime_serde")]
    pub preroll: Duration,
    #[serde(with = "humantime_serde")]
    pub overlap: Duration,
    pub vad_threshold: f32,
    pub vad_neg_threshold: f32,
}

impl Default for LiveConfig {
    fn default() -> Self {
        LiveConfig {
            tentative_interval: Duration::from_millis(500),
            pause: Duration::from_millis(500),
            max_segment: Duration::from_secs(20),
            forced_window: Duration::from_secs(5),
            preroll: Duration::from_millis(200),
            overlap: Duration::from_millis(300),
            vad_threshold: 0.5,
            vad_neg_threshold: 0.35,
        }
    }
}

fn default_socket() -> PathBuf {
    PathBuf::from(DEFAULT_SOCKET)
}
fn default_threads() -> u32 {
    4
}
fn default_idle_unload() -> Duration {
    Duration::from_secs(300)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ошибка в {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{0}")]
    Invalid(String),
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut config = Config::parse(&text).map_err(|e| match e {
            ConfigError::Parse { message, .. } => ConfigError::Parse {
                path: path.to_owned(),
                message,
            },
            other => other,
        })?;
        let base = path.parent().unwrap_or(Path::new("."));
        config.model.path = base.join(&config.model.path);
        config.vad.path = base.join(&config.vad.path);
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: PathBuf::new(),
            message: e.message().to_owned(),
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let bad = |m: &str| Err(ConfigError::Invalid(m.to_owned()));
        for (name, file) in [("model", &self.model), ("vad", &self.vad)] {
            let hex = &file.sha256;
            if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ConfigError::Invalid(format!(
                    "{name}.sha256: нужны 64 шестнадцатеричных символа"
                )));
            }
        }
        if self.threads == 0 || self.threads > 256 {
            return bad("threads: от 1 до 256");
        }
        let live = &self.live;
        if !(0.0..=1.0).contains(&live.vad_threshold)
            || !(0.0..=live.vad_threshold).contains(&live.vad_neg_threshold)
        {
            return bad("live: 0 <= vad_neg_threshold <= vad_threshold <= 1");
        }
        if live.tentative_interval < std::time::Duration::from_millis(100) {
            return bad("live.tentative_interval: не меньше 100ms");
        }
        if live.pause < std::time::Duration::from_millis(100) {
            return bad("live.pause: не меньше 100ms");
        }
        if live.max_segment < std::time::Duration::from_secs(2)
            || live.forced_window.is_zero()
            || live.forced_window >= live.max_segment
        {
            return bad("live: max_segment не меньше 2s, 0 < forced_window < max_segment");
        }
        if live.overlap >= live.forced_window {
            return bad("live.overlap: меньше forced_window");
        }
        let l = &self.limits;
        if l.max_audio.is_zero() || l.lifetime <= l.max_audio {
            return bad("limits: lifetime больше max_audio, max_audio больше нуля");
        }
        for (name, d) in [
            ("start_deadline", l.start_deadline),
            ("idle", l.idle),
            ("max_lead", l.max_lead),
            ("cancel_grace", l.cancel_grace),
            ("write_timeout", l.write_timeout),
            ("finish_timeout", l.finish_timeout),
        ] {
            if d.is_zero() {
                return Err(ConfigError::Invalid(format!("limits.{name}: больше нуля")));
            }
        }
        Ok(())
    }

    /// Имя модели для `health`: имя файла без `.gguf`.
    pub fn model_name(&self) -> String {
        let name = self
            .model
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.strip_suffix(".gguf").unwrap_or(&name).to_owned()
    }
}

/// sha256 файла в нижнем регистре.
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Отпечаток файла: устройство, inode, размер и время изменения. Замена файла
/// (rename) или запись в него меняют отпечаток.
pub fn fingerprint(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)?;
    Ok(format!(
        "{}:{}:{}:{}.{}",
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec()
    ))
}

/// Отпечатки моделей после проверки sha256: `serve` передаёт их движку, и тот не
/// пересчитывает sha256 неизменённых файлов (740 МБ — секунды без SHA-NI).
pub fn verified_fingerprints(config: &Config) -> Result<Vec<String>, String> {
    verify_models(config)?;
    [&config.model, &config.vad]
        .iter()
        .map(|f| fingerprint(&f.path).map_err(|e| format!("{}: {e}", f.path.display())))
        .collect()
}

/// Сверяет sha256 файлов, чей отпечаток не совпадает с уже проверенным.
/// Возвращает число пересчитанных файлов.
pub fn verify_models_known(config: &Config, known: &[String]) -> Result<usize, String> {
    let mut hashed = 0;
    for (i, (name, file)) in [("model", &config.model), ("vad", &config.vad)]
        .into_iter()
        .enumerate()
    {
        if known
            .get(i)
            .is_some_and(|k| fingerprint(&file.path).is_ok_and(|f| &f == k))
        {
            continue;
        }
        hashed += 1;
        verify_one(name, file)?;
    }
    Ok(hashed)
}

fn verify_one(name: &str, file: &ModelFile) -> Result<(), String> {
    let got = file_sha256(&file.path)
        .map_err(|e| format!("{name}: не удалось прочитать {}: {e}", file.path.display()))?;
    if !got.eq_ignore_ascii_case(&file.sha256) {
        return Err(format!(
            "{name}: sha256 {} не совпадает с конфигурацией",
            file.path.display()
        ));
    }
    Ok(())
}

/// Сверяет sha256 обоих файлов моделей; ошибка называет файл, но не содержимое.
pub fn verify_models(config: &Config) -> Result<(), String> {
    for (name, file) in [("model", &config.model), ("vad", &config.vad)] {
        let got = file_sha256(&file.path)
            .map_err(|e| format!("{name}: не удалось прочитать {}: {e}", file.path.display()))?;
        if !got.eq_ignore_ascii_case(&file.sha256) {
            return Err(format!(
                "{name}: sha256 {} не совпадает с конфигурацией",
                file.path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"
[model]
path = "m.gguf"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
[vad]
path = "v.gguf"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
"#;

    #[test]
    fn defaults_match_design() {
        let c = Config::parse(MIN).unwrap();
        assert_eq!(c.socket, PathBuf::from(DEFAULT_SOCKET));
        assert_eq!(c.device, Device::Auto);
        assert_eq!(c.threads, 4);
        assert_eq!(c.idle_unload, Duration::from_secs(300));
        assert_eq!(c.live.tentative_interval, Duration::from_millis(500));
        assert_eq!(c.live.pause, Duration::from_millis(500));
        assert_eq!(c.live.max_segment, Duration::from_secs(20));
        assert_eq!(c.live.preroll, Duration::from_millis(200));
        assert_eq!(c.live.overlap, Duration::from_millis(300));
        assert_eq!(c.limits.max_audio, Duration::from_secs(600));
        assert_eq!(c.limits.lifetime, Duration::from_secs(660));
        assert_eq!(c.model_name(), "m");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let text = format!("{MIN}\n[live]\nbogus = 1\n");
        assert!(Config::parse(&text).is_err());
        let text = format!("extra = 1\n{MIN}");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn bad_sha_and_device_are_rejected() {
        assert!(Config::parse(&MIN.replace("0000000000", "zzzzzzzzzz")).is_err());
        assert!(Config::parse(&format!("device = \"tpu\"\n{MIN}")).is_err());
        let c = Config::parse(&format!("device = \"cpu\"\n{MIN}")).unwrap();
        assert_eq!(c.device, Device::Cpu);
    }

    #[test]
    fn verify_models_refuses_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("m.gguf"), b"model").unwrap();
        std::fs::write(dir.path().join("v.gguf"), b"vad").unwrap();
        let cfg_path = dir.path().join("speech.toml");
        let good_m = file_sha256(&dir.path().join("m.gguf")).unwrap();
        let good_v = file_sha256(&dir.path().join("v.gguf")).unwrap();
        let text = MIN
            .replace(
                "0000000000000000000000000000000000000000000000000000000000000000",
                &good_m,
            )
            .replace(
                "1111111111111111111111111111111111111111111111111111111111111111",
                &good_v,
            );
        std::fs::write(&cfg_path, &text).unwrap();
        let c = Config::load(&cfg_path).unwrap();
        verify_models(&c).unwrap();
        std::fs::write(&cfg_path, text.replace(&good_v, &good_m)).unwrap();
        let c = Config::load(&cfg_path).unwrap();
        let err = verify_models(&c).unwrap_err();
        assert!(err.starts_with("vad:"), "{err}");
    }

    #[test]
    fn known_fingerprint_skips_hash_until_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let (m, v) = (dir.path().join("m.gguf"), dir.path().join("v.gguf"));
        std::fs::write(&m, b"model").unwrap();
        std::fs::write(&v, b"vad").unwrap();
        let text = MIN
            .replace(
                "0000000000000000000000000000000000000000000000000000000000000000",
                &file_sha256(&m).unwrap(),
            )
            .replace(
                "1111111111111111111111111111111111111111111111111111111111111111",
                &file_sha256(&v).unwrap(),
            );
        let cfg_path = dir.path().join("speech.toml");
        std::fs::write(&cfg_path, text).unwrap();
        let c = Config::load(&cfg_path).unwrap();
        let known = verified_fingerprints(&c).unwrap();
        assert_eq!(verify_models_known(&c, &known).unwrap(), 0);
        assert_eq!(verify_models_known(&c, &[]).unwrap(), 2);
        // Подмена файла переименованием: другой inode, sha256 считается заново.
        let tmp = dir.path().join("m.new");
        std::fs::write(&tmp, b"other").unwrap();
        std::fs::rename(&tmp, &m).unwrap();
        assert!(
            verify_models_known(&c, &known)
                .unwrap_err()
                .starts_with("model:")
        );
    }
}
