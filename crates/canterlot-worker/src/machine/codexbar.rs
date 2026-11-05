use canterlot_proto::worker::v1::{
    LimitAbsence, LimitDataConfidence, LimitObservation, LimitWindow,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::Instant};

use super::limits::{LimitsFetch, LimitsSource};

const RUN_TIMEOUT: Duration = Duration::from_secs(45);
/// Повторный запуск раньше этого срока бессмыслен: CodexBar сам кеширует ответы провайдера.
const MIN_GAP: Duration = Duration::from_secs(10);
/// Потолок паузы после постоянных сбоев: окно лимита важнее, но точка usage держит блокировку после 429.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);
/// Временный сбой повторяем скоро: первый раз через минуту, второй через две, дальше обычный интервал.
const TRANSIENT_RETRY_FIRST: Duration = Duration::from_secs(60);
const TRANSIENT_RETRY_SECOND: Duration = Duration::from_secs(120);
/// CodexBar не должен конкурировать со сборкой за процессор.
const NICE: &str = "nice";
const NICE_INCREMENT: &str = "10";
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const FIVE_HOUR_MINUTES: u64 = 300;
const WEEK_MINUTES: u64 = 10080;
const MONTH_MINUTES: u64 = 43200;
/// Провайдер хаба и имя в выводе `codexbar usage --provider`.
const OPENCODE_HUB: &str = "opencode";
const OPENCODE_CLI: &str = "opencodego";

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

impl CodexBarError {
    /// Временный сбой: машина занята или процесс не успел запуститься. Повторяем скоро.
    fn transient(self) -> bool {
        matches!(self, Self::Spawn | Self::Timeout | Self::Output)
    }
}

#[cfg(test)]
mod tests {
    use super::{RUN_TIMEOUT, command_for};
    use std::{path::Path, time::Duration};

    #[test]
    fn a_slow_run_gets_forty_five_seconds() {
        assert_eq!(RUN_TIMEOUT, Duration::from_secs(45));
    }

    #[test]
    fn codexbar_starts_through_nice_with_the_provider_and_json_format() {
        let command = command_for(Path::new("/opt/codexbar"), "opencode");
        let command = command.as_std();
        assert_eq!(command.get_program(), "nice");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-n",
                "10",
                "/opt/codexbar",
                "usage",
                "--provider",
                "opencodego",
                "--format",
                "json",
                "--no-color",
            ]
        );
    }
}

/// Имя для запуска `codexbar usage --provider`: у OpenCode оно отличается от провайдера хаба.
fn cli_provider(provider: &str) -> &str {
    if provider == OPENCODE_HUB {
        OPENCODE_CLI
    } else {
        provider
    }
}

/// Запуск CodexBar с пониженным приоритетом процессора, чтобы сборка не морила его голодом.
/// `#![forbid(unsafe_code)]` не пускает `pre_exec`, поэтому приоритет задаёт `nice`.
fn command_for(executable: &Path, provider: &str) -> Command {
    let mut command = Command::new(NICE);
    command
        .arg("-n")
        .arg(NICE_INCREMENT)
        .arg(executable)
        .args([
            "usage",
            "--provider",
            cli_provider(provider),
            "--format",
            "json",
            "--no-color",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

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
            items.iter().find(|item| {
                item.get("provider")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == provider || name == cli_provider(provider))
            })
        })
        .ok_or(CodexBarError::Output)?;
    let usage = entry.get("usage").ok_or(CodexBarError::Output)?;
    let data_confidence = match usage.get("dataConfidence").and_then(Value::as_str) {
        Some("estimated") => LimitDataConfidence::Estimated as i32,
        Some("exact") => LimitDataConfidence::Exact as i32,
        _ => LimitDataConfidence::Unspecified as i32,
    };
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
            Some(MONTH_MINUTES) => LimitWindow::Monthly,
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
            data_confidence,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    Idle,
    Hub,
    Refresh,
}

#[derive(Default)]
struct ProviderState {
    last_request: Option<Instant>,
    reported: Option<CodexBarError>,
    /// Постоянные сбои (нет программы, ненулевой код выхода): пауза растёт.
    failures: u32,
    /// Временные сбои подряд: минута, две, дальше обычный интервал.
    transient: u32,
    backoff_until: Option<Instant>,
}

/// У каждого провайдера своё состояние: пауза OpenCode не задерживает Claude и Codex.
#[derive(Default)]
struct State {
    providers: HashMap<String, ProviderState>,
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
            std::future::ready(command_for(&executable, provider).spawn())
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

    async fn fallback(&self, provider: &str, request: Request) -> LimitsFetch {
        LimitsFetch {
            observations: match &self.fallback {
                Some(fallback) if request == Request::Idle => fallback.poll_idle(provider).await,
                Some(fallback) => fallback.poll(provider).await,
                None => Vec::new(),
            },
            absent: Vec::new(),
        }
    }

    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32.checked_shl(failures).unwrap_or(u32::MAX);
        self.interval
            .saturating_mul(factor)
            .min(MAX_BACKOFF.max(self.interval))
    }

    /// Пауза перед повтором временного сбоя: минута, две, дальше обычный интервал и не больше.
    fn transient_backoff(&self, failures: u32) -> Duration {
        match failures {
            0 | 1 => TRANSIENT_RETRY_FIRST,
            2 => TRANSIENT_RETRY_SECOND,
            _ => self.interval,
        }
    }

    /// Сколько осталось до назначенного повтора: этим воркер сам будит тикер простоя.
    pub fn retry_in(&self, provider: &str) -> Option<Duration> {
        let now = (self.clock)();
        let state = self.state.lock().expect("codexbar poll state");
        state
            .providers
            .get(provider)?
            .backoff_until
            .map(|until| until.saturating_duration_since(now))
    }

    async fn poll_with(&self, provider: &str, request: Request) -> LimitsFetch {
        if !matches!(provider, "claude" | "codex" | OPENCODE_HUB) {
            return LimitsFetch::default();
        }
        let now = (self.clock)();
        let backed_off = {
            let mut state = self.state.lock().expect("codexbar poll state");
            let entry = state.providers.entry(provider.to_owned()).or_default();
            let since = |at: Option<Instant>, gap: Duration| {
                at.is_some_and(|at| now.saturating_duration_since(at) < gap)
            };
            if since(entry.last_request, MIN_GAP) {
                return LimitsFetch::default();
            }
            // Истёкшая пауза разрешает повтор раньше обычного интервала.
            let retry_due = entry.backoff_until.is_some_and(|until| now >= until);
            let backed_off = request != Request::Refresh
                && !retry_due
                && entry.backoff_until.is_some_and(|until| now < until);
            if !backed_off
                && request != Request::Refresh
                && !retry_due
                && since(entry.last_request, self.interval)
            {
                return LimitsFetch::default();
            }
            if retry_due {
                entry.backoff_until = None;
            }
            if !backed_off {
                entry.last_request = Some(now);
            }
            backed_off
        };
        if backed_off {
            return self.fallback(provider, request).await;
        }
        let result = self.fetch(provider).await;
        {
            let mut state = self.state.lock().expect("codexbar poll state");
            let entry = state.providers.entry(provider.to_owned()).or_default();
            match &result {
                Ok(fetched) if !fetched.observations.is_empty() => {
                    if entry.reported.take().is_some() {
                        tracing::info!(
                            kind = "codexbar_poll_recovered",
                            provider = provider,
                            "codexbar limits poll recovered"
                        );
                    }
                    entry.failures = 0;
                    entry.transient = 0;
                    entry.backoff_until = None;
                }
                Err(error) => {
                    if entry.reported.is_none() {
                        tracing::warn!(
                            kind = "codexbar_poll_failed",
                            provider = provider,
                            reason = %error,
                            "codexbar limits poll failed"
                        );
                    } else {
                        tracing::debug!(
                            kind = "codexbar_poll_repeated",
                            provider = provider,
                            reason = %error,
                            "codexbar limits poll failed again"
                        );
                    }
                    entry.reported = Some(*error);
                    if error.transient() {
                        entry.transient = entry.transient.saturating_add(1);
                        entry.failures = 0;
                        entry.backoff_until = Some(now + self.transient_backoff(entry.transient));
                    } else {
                        entry.failures = entry.failures.saturating_add(1);
                        entry.transient = 0;
                        entry.backoff_until = Some(now + self.backoff(entry.failures));
                    }
                }
                // Пустое, но разобранное чтение: как и раньше, пауза растёт.
                Ok(_) => {
                    entry.failures = entry.failures.saturating_add(1);
                    entry.transient = 0;
                    entry.backoff_until = Some(now + self.backoff(entry.failures));
                }
            }
        }
        match result {
            Ok(fetched) if !fetched.observations.is_empty() => fetched,
            _ => self.fallback(provider, request).await,
        }
    }
}

#[async_trait::async_trait]
impl LimitsSource for CodexBarLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, Request::Hub).await.observations
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, Request::Idle).await.observations
    }

    async fn poll_report(&self, provider: &str, idle: bool) -> LimitsFetch {
        let request = if idle { Request::Idle } else { Request::Hub };
        self.poll_with(provider, request).await
    }

    async fn poll_refresh(&self, provider: &str) -> LimitsFetch {
        self.poll_with(provider, Request::Refresh).await
    }
}
