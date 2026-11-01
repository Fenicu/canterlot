use canterlot_proto::worker::v1::{LimitAbsence, LimitObservation, LimitWindow};
use serde_json::Value;
use std::{
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::Instant};

use super::limits::{LimitsFetch, LimitsSource};

const RUN_TIMEOUT: Duration = Duration::from_secs(20);
/// Повторный запуск раньше этого срока бессмыслен: CodexBar сам кеширует ответы провайдера.
const MIN_GAP: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const WEEK_MINUTES: u64 = 10080;

/// Причины пустого опроса. Ни один вариант не несёт вывод программы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexBarError {
    Missing,
    Spawn,
    Timeout,
    Status(i32),
    Output,
}

impl fmt::Display for CodexBarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("codexbar executable not found"),
            Self::Spawn => f.write_str("codexbar failed to start"),
            Self::Timeout => f.write_str("codexbar timed out"),
            Self::Status(code) => write!(f, "codexbar exited with status {code}"),
            Self::Output => f.write_str("codexbar output invalid"),
        }
    }
}

impl std::error::Error for CodexBarError {}

/// Окна по `windowMinutes`: `primary` у CodexBar не обязательно пятичасовое
/// (у Claude без пятичасового окна он повторяет недельное), поэтому имена слотов не используются.
pub fn observations_from_output(
    provider: &str,
    stdout: &[u8],
    now_ms: u64,
) -> Result<Vec<LimitObservation>, CodexBarError> {
    Ok(reading_from_output(provider, stdout, now_ms)?.observations)
}

/// Показания и отсутствующие окна одного чтения. Пятичасовое окно считается отсутствующим только
/// по полному чтению: другие окна провайдера прочитаны, а строки на 300 минут нет вовсе.
/// Чтение с пятичасовой строкой без значения неполное и ничего не скрывает.
pub fn reading_from_output(
    provider: &str,
    stdout: &[u8],
    now_ms: u64,
) -> Result<LimitsFetch, CodexBarError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|_| CodexBarError::Output)?;
    let entry = value
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("provider").and_then(Value::as_str) == Some(provider))
        })
        .ok_or(CodexBarError::Output)?;
    let usage = entry.get("usage").ok_or(CodexBarError::Output)?;
    let seen_ms = usage
        .get("updatedAt")
        .and_then(Value::as_str)
        .and_then(super::limits::rfc3339_ms)
        .map_or(now_ms, |at| at.min(now_ms));
    let mut observations: Vec<LimitObservation> = Vec::new();
    let mut five_hour_row = false;
    for slot in ["primary", "secondary", "tertiary"] {
        let Some(row) = usage.get(slot).filter(|row| row.is_object()) else {
            continue;
        };
        let window = match row.get("windowMinutes").and_then(Value::as_u64) {
            Some(FIVE_HOUR_MINUTES) => {
                five_hour_row = true;
                LimitWindow::FiveHour
            }
            Some(WEEK_MINUTES) => LimitWindow::Weekly,
            _ => continue,
        };
        if observations.iter().any(|o| o.window == window as i32) {
            continue;
        }
        let Some(used) = row
            .get("usedPercent")
            .and_then(Value::as_f64)
            .filter(|used| used.is_finite() && *used >= 0.0)
        else {
            continue;
        };
        observations.push(LimitObservation {
            provider: provider.to_owned(),
            window: window as i32,
            used_permille: (used * 10.0).round().min(1000.0) as u32,
            resets_at_ms: row
                .get("resetsAt")
                .and_then(Value::as_str)
                .and_then(super::limits::rfc3339_ms)
                .unwrap_or(0),
            observed_machine_ms: seen_ms,
        });
    }
    let absent = if !five_hour_row && !observations.is_empty() {
        vec![LimitAbsence {
            provider: provider.to_owned(),
            window: LimitWindow::FiveHour as i32,
            observed_machine_ms: seen_ms,
        }]
    } else {
        Vec::new()
    };
    Ok(LimitsFetch {
        observations,
        absent,
    })
}

/// Исполняемый файл CodexBar: заданный путь, иначе `PATH`, иначе `~/.local/bin`.
pub fn locate(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path.is_file().then(|| path.to_path_buf());
    }
    let from_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("codexbar"))
            .find(|candidate| candidate.is_file())
    });
    from_path.or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let candidate = home.join(".local/bin/codexbar");
        candidate.is_file().then_some(candidate)
    })
}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct State {
    last_request: Option<Instant>,
    reported: Option<CodexBarError>,
}

/// Окна провайдера через `codexbar usage`: без запроса к модели, учётные данные
/// читает сам CodexBar, воркер их не открывает.
pub struct CodexBarLimitsSource {
    configured: Option<PathBuf>,
    interval: Duration,
    fallback: Option<Arc<dyn LimitsSource>>,
    clock: Clock,
    state: Mutex<State>,
}

impl fmt::Debug for CodexBarLimitsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexBarLimitsSource")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl CodexBarLimitsSource {
    pub fn new(
        executable: Option<PathBuf>,
        interval: Duration,
        fallback: Option<Arc<dyn LimitsSource>>,
    ) -> Self {
        Self {
            configured: executable,
            interval,
            fallback,
            clock: Arc::new(Instant::now),
            state: Mutex::default(),
        }
    }

    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Один запуск без учёта интервала.
    pub async fn fetch(&self, provider: &str) -> Result<LimitsFetch, CodexBarError> {
        let executable = locate(self.configured.as_deref()).ok_or(CodexBarError::Missing)?;
        let mut child = crate::adapter::spawn_with_retry_io(|| {
            let mut command = Command::new(&executable);
            command
                .args([
                    "usage",
                    "--provider",
                    provider,
                    "--format",
                    "json",
                    "--no-color",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            std::future::ready(command.spawn())
        })
        .await
        .map_err(|_| CodexBarError::Spawn)?;
        let mut stdout = child.stdout.take().ok_or(CodexBarError::Spawn)?;
        let run = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(MAX_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| CodexBarError::Output)?;
            if bytes.len() > MAX_OUTPUT_BYTES {
                return Err(CodexBarError::Output);
            }
            let status = child.wait().await.map_err(|_| CodexBarError::Spawn)?;
            if !status.success() {
                return Err(CodexBarError::Status(status.code().unwrap_or(-1)));
            }
            Ok(bytes)
        };
        let bytes = tokio::time::timeout(RUN_TIMEOUT, run)
            .await
            .map_err(|_| CodexBarError::Timeout)??;
        reading_from_output(provider, &bytes, super::now_ms())
    }

    async fn poll_with(&self, provider: &str, idle: bool) -> LimitsFetch {
        if !matches!(provider, "claude" | "codex") {
            return LimitsFetch::default();
        }
        let now = (self.clock)();
        {
            let mut state = self.state.lock().expect("codexbar poll state");
            let since = |gap: Duration| {
                state
                    .last_request
                    .is_some_and(|at| now.saturating_duration_since(at) < gap)
            };
            if since(MIN_GAP) || (idle && since(self.interval)) {
                return LimitsFetch::default();
            }
            state.last_request = Some(now);
        }
        let result = self.fetch(provider).await;
        {
            let mut state = self.state.lock().expect("codexbar poll state");
            match &result {
                Ok(_) => {
                    if state.reported.take().is_some() {
                        tracing::info!(
                            kind = "codexbar_poll_recovered",
                            "codexbar limits poll recovered"
                        );
                    }
                }
                Err(error) => {
                    if state.reported.replace(*error) == Some(*error) {
                        tracing::debug!(kind = "codexbar_poll_repeated", reason = %error, "codexbar limits poll failed again");
                    } else {
                        tracing::warn!(kind = "codexbar_poll_failed", reason = %error, "codexbar limits poll failed");
                    }
                }
            }
        }
        match result {
            Ok(fetched) if !fetched.observations.is_empty() => fetched,
            _ => LimitsFetch {
                observations: match &self.fallback {
                    Some(fallback) if idle => fallback.poll_idle(provider).await,
                    Some(fallback) => fallback.poll(provider).await,
                    None => Vec::new(),
                },
                absent: Vec::new(),
            },
        }
    }
}

#[async_trait::async_trait]
impl LimitsSource for CodexBarLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, false).await.observations
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, true).await.observations
    }

    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        self.poll_with(provider, idle).await
    }
}
