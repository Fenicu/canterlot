//! Лимиты, quota-ошибка и окно контекста Antigravity (§8).
//!
//! `agy -p /usage --output-format json` не запускает ход: воркер берёт окна
//! только группы «Gemini Models», группа Claude/GPT к сессиям canterlot не
//! относится. Модель одна — провайдер `antigravity`.

use super::process::{PrepareError, prepare_pinned, process_env};
use crate::adapter::{LimitFact, LimitWindow};
use crate::config::AntigravityConfig;
use crate::machine::limits::LiveLimits;
use canterlot_proto::worker::v1::{LimitObservation, LimitWindow as ProtoLimitWindow};
use serde_json::Value;
use std::{
    fmt,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MIN_GAP: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(3600);
const GEMINI_GROUP: &str = "Gemini Models";

/// Окно `gemini-3.8-flash`. Документированного значения в кадрах и строках
/// бинарника этапа 0 нет; 1M — семейство Gemini 3, значение не проверено живым
/// кадром (см. §8: формулу контекста закрывает кадр с cache-hit).
pub const GEMINI_3_8_FLASH_CONTEXT_WINDOW: u64 = 1_048_576;

pub fn context_window(model: &str) -> u64 {
    if model.starts_with("gemini-3.8-flash") {
        GEMINI_3_8_FLASH_CONTEXT_WINDOW
    } else {
        0
    }
}

fn window_of(value: &str) -> Option<LimitWindow> {
    match value {
        "5h" => Some(LimitWindow::FiveHour),
        "weekly" => Some(LimitWindow::Weekly),
        _ => None,
    }
}

/// Окна группы «Gemini Models» из JSON `/usage`; остальные группы игнорируются.
pub fn observations_from_usage(value: &Value, observed_ms: u64) -> Vec<LimitObservation> {
    let groups = value
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
        .or_else(|| value.get("groups").and_then(Value::as_array));
    let Some(group) = groups.and_then(|groups| {
        groups
            .iter()
            .find(|group| group.get("name").and_then(Value::as_str) == Some(GEMINI_GROUP))
    }) else {
        return Vec::new();
    };
    let mut observations: Vec<LimitObservation> = Vec::new();
    for bucket in group
        .get("buckets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(window) = bucket
            .get("window")
            .and_then(Value::as_str)
            .and_then(window_of)
        else {
            continue;
        };
        let Some(remaining) = bucket
            .get("remaining_fraction")
            .and_then(Value::as_f64)
            .filter(|fraction| fraction.is_finite() && *fraction >= 0.0)
        else {
            continue;
        };
        if observations
            .iter()
            .any(|observation| observation.window == ProtoLimitWindow::from(window) as i32)
        {
            continue;
        }
        let used = ((1.0 - remaining.clamp(0.0, 1.0)) * 1000.0).round() as u32;
        observations.push(LimitObservation {
            provider: "antigravity".into(),
            window: ProtoLimitWindow::from(window) as i32,
            used_permille: used.min(1000),
            resets_at_ms: bucket
                .get("reset_time")
                .and_then(Value::as_str)
                .and_then(crate::machine::limits::rfc3339_ms)
                .unwrap_or(0),
            observed_machine_ms: observed_ms,
            ..Default::default()
        });
    }
    observations
}

/// Квота в тексте хода. Формат F4 не подтверждён живым кадром, поэтому разбор
/// терпимый: ищем «quota … reached/exceeded» и «Resets in <d>d <h>h <m>m».
pub fn reached_from_text(text: &str, now_ms: u64) -> Option<LimitFact> {
    let lower = text.to_lowercase();
    let quota = lower.contains("quota")
        && (lower.contains("reached")
            || lower.contains("exceeded")
            || lower.contains("exhausted")
            || lower.contains("resource_exhausted"));
    if !quota {
        return None;
    }
    let resets_at_ms = reset_after(&lower, now_ms);
    // Срок дольше пяти часов — недельное окно; иначе пятичасовое.
    let window = match resets_at_ms {
        Some(reset) if reset.saturating_sub(now_ms) > 5 * 3_600_000 => LimitWindow::Weekly,
        Some(_) => LimitWindow::FiveHour,
        None => LimitWindow::Weekly,
    };
    Some(LimitFact::Reached {
        window,
        resets_at_ms,
    })
}

fn reset_after(lower: &str, now_ms: u64) -> Option<u64> {
    let start = lower.find("resets in")? + "resets in".len();
    let mut total = 0u64;
    let mut found = false;
    for token in lower[start..].split_whitespace().take(8) {
        let digits: String = token.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            if found {
                break;
            }
            continue;
        }
        let Ok(value) = digits.parse::<u64>() else {
            break;
        };
        let unit: String = token[digits.len()..]
            .chars()
            .filter(|character| character.is_alphabetic())
            .collect();
        let seconds = match unit.as_str() {
            "d" | "day" | "days" => 86_400,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            _ => {
                if found {
                    break;
                }
                continue;
            }
        };
        total = total.saturating_add(value.saturating_mul(seconds));
        found = true;
    }
    found.then(|| now_ms.saturating_add(total.saturating_mul(1000)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageError {
    Prepare,
    Spawn,
    Timeout,
    Status(i32),
    Output,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare => f.write_str("antigravity binary unavailable"),
            Self::Spawn => f.write_str("agy usage failed to start"),
            Self::Timeout => f.write_str("agy usage timed out"),
            Self::Status(code) => write!(f, "agy usage exited with status {code}"),
            Self::Output => f.write_str("agy usage output invalid"),
        }
    }
}

impl std::error::Error for UsageError {}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct PollState {
    last_request: Option<Instant>,
    backoff_until: Option<Instant>,
    failures: u32,
}

/// `agy -p /usage --output-format json` закреплённой копией, cwd = data_dir.
pub struct AntigravityLimitsSource {
    cfg: AntigravityConfig,
    interval: Duration,
    live: LiveLimits,
    clock: Clock,
    state: Mutex<PollState>,
}

impl fmt::Debug for AntigravityLimitsSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AntigravityLimitsSource")
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl AntigravityLimitsSource {
    pub fn new(cfg: AntigravityConfig, interval: Duration, live: LiveLimits) -> Self {
        Self {
            cfg,
            interval,
            live,
            clock: Arc::new(Instant::now),
            state: Mutex::default(),
        }
    }

    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// Один запуск без учёта интервала и живых показаний.
    pub async fn fetch(&self) -> Result<Vec<LimitObservation>, UsageError> {
        let pinned = prepare_pinned(&self.cfg)
            .await
            .map_err(|error| match error {
                PrepareError::Version(_) | PrepareError::MissingBinary => UsageError::Prepare,
                _ => UsageError::Prepare,
            })?;
        let data_dir = self.cfg.data_dir.clone().ok_or(UsageError::Prepare)?;
        let env = process_env(std::env::vars_os());
        let mut child = crate::adapter::spawn_with_retry_io(|| {
            let mut command = Command::new(&pinned.executable);
            command
                .args([
                    "-p",
                    "/usage",
                    "--output-format",
                    "json",
                    "--disable-telemetry",
                ])
                .current_dir(&data_dir)
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .kill_on_drop(true);
            for (key, value) in &env {
                command.env(key, value);
            }
            std::future::ready(command.spawn())
        })
        .await
        .map_err(|_| UsageError::Spawn)?;
        let mut stdout = child.stdout.take().ok_or(UsageError::Spawn)?;
        let run = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(MAX_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| UsageError::Output)?;
            if bytes.len() > MAX_OUTPUT_BYTES {
                return Err(UsageError::Output);
            }
            let status = child.wait().await.map_err(|_| UsageError::Spawn)?;
            if !status.success() {
                return Err(UsageError::Status(status.code().unwrap_or(-1)));
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| UsageError::Output)?;
            Ok(observations_from_usage(&value, crate::machine::now_ms()))
        };
        tokio::time::timeout(RUN_TIMEOUT, run)
            .await
            .map_err(|_| UsageError::Timeout)?
    }

    fn due(&self, provider: &str, idle: bool) -> bool {
        let now = (self.clock)();
        let mut state = self.state.lock().expect("antigravity limits poll state");
        let since = |at: Option<Instant>, gap: Duration| {
            at.is_some_and(|at| now.saturating_duration_since(at) < gap)
        };
        if state.backoff_until.is_some_and(|until| now < until)
            || since(state.last_request, MIN_GAP)
            || (idle
                && (since(state.last_request, self.interval)
                    || since(self.live.last(provider), self.interval)))
        {
            return false;
        }
        state.last_request = Some(now);
        true
    }

    async fn poll_with(&self, provider: &str, idle: bool) -> Vec<LimitObservation> {
        if provider != "antigravity" || !self.due(provider, idle) {
            return Vec::new();
        }
        match self.fetch().await {
            Ok(observations) => {
                let mut state = self.state.lock().expect("antigravity limits poll state");
                state.failures = 0;
                state.backoff_until = None;
                observations
            }
            Err(error) => {
                let mut state = self.state.lock().expect("antigravity limits poll state");
                state.failures = state.failures.saturating_add(1);
                let factor = 1u32.checked_shl(state.failures).unwrap_or(u32::MAX);
                state.backoff_until =
                    Some((self.clock)() + self.interval.saturating_mul(factor).min(MAX_BACKOFF));
                if error == UsageError::Timeout {
                    tracing::debug!(
                        kind = "antigravity_limits_probe_timeout",
                        "antigravity limits probe timed out"
                    );
                } else {
                    tracing::warn!(
                        kind = "antigravity_limits_probe_failed",
                        reason = %error,
                        "antigravity limits probe failed"
                    );
                }
                Vec::new()
            }
        }
    }
}

#[async_trait::async_trait]
impl crate::machine::limits::LimitsSource for AntigravityLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, false).await
    }

    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.poll_with(provider, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn evidence(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/stage0/evidence/antigravity")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn real_usage_reads_only_the_gemini_group() {
        let observations =
            observations_from_usage(&evidence("usage-before.json"), 1_793_713_000_000);
        assert_eq!(observations.len(), 2);
        assert!(observations.iter().all(|o| o.provider == "antigravity"));
        let weekly = observations
            .iter()
            .find(|o| o.window == ProtoLimitWindow::Weekly as i32)
            .unwrap();
        assert_eq!(weekly.used_permille, 990);
        assert_eq!(weekly.resets_at_ms, 1_793_807_703_000);
        let five = observations
            .iter()
            .find(|o| o.window == ProtoLimitWindow::FiveHour as i32)
            .unwrap();
        assert_eq!(five.used_permille, 0);
        assert_eq!(five.observed_machine_ms, 1_793_713_000_000);
    }

    #[test]
    fn the_live_usage_frame_has_two_gemini_windows_and_ignores_third_party() {
        let observations =
            observations_from_usage(&evidence("p-live-usage-before.json"), 1_793_808_175_000);
        assert_eq!(observations.len(), 2);
        let weekly = observations
            .iter()
            .find(|o| o.window == ProtoLimitWindow::Weekly as i32)
            .unwrap();
        assert_eq!(weekly.used_permille, 0);
        assert_eq!(weekly.resets_at_ms, 1_794_412_503_000);
        let five = observations
            .iter()
            .find(|o| o.window == ProtoLimitWindow::FiveHour as i32)
            .unwrap();
        assert_eq!(five.used_permille, 0);
        assert_eq!(five.resets_at_ms, 1_793_825_825_000);
        assert!(
            observations
                .iter()
                .all(|o| o.observed_machine_ms == 1_793_808_175_000)
        );
    }

    #[test]
    fn the_claude_group_is_ignored() {
        let mut usage = evidence("usage-before.json");
        usage["command"]["data"]["groups"]
            .as_array_mut()
            .unwrap()
            .retain(|group| group["name"] == "Claude and GPT models");
        assert!(observations_from_usage(&usage, 0).is_empty());
    }

    #[test]
    fn quota_text_is_tolerant_and_keeps_the_reset_horizon() {
        let now = 1_793_713_000_000;
        let weekly = reached_from_text(
            "Individual quota reached for gemini-3.8-flash-high. Resets in 1d 2h 15m",
            now,
        )
        .expect("quota");
        assert_eq!(
            weekly,
            LimitFact::Reached {
                window: LimitWindow::Weekly,
                resets_at_ms: Some(now + (26 * 3600 + 15 * 60) * 1000),
            }
        );
        assert_eq!(
            reached_from_text("quota exceeded; resets in 2h 15m", now),
            Some(LimitFact::Reached {
                window: LimitWindow::FiveHour,
                resets_at_ms: Some(now + (2 * 3600 + 15 * 60) * 1000),
            })
        );
        assert_eq!(
            reached_from_text("RESOURCE_EXHAUSTED: quota exhausted", now),
            Some(LimitFact::Reached {
                window: LimitWindow::Weekly,
                resets_at_ms: None,
            })
        );
        assert_eq!(reached_from_text("model returned bad args", now), None);
    }

    #[test]
    fn the_context_window_is_per_model_family() {
        assert_eq!(
            context_window("gemini-3.8-flash-high"),
            GEMINI_3_8_FLASH_CONTEXT_WINDOW
        );
        assert_eq!(context_window("gemini-3.8-flash-low"), 1_048_576);
        assert_eq!(context_window("claude-opus-5-5"), 0);
        assert_eq!(context_window(""), 0);
    }

    #[test]
    fn a_usage_without_the_gemini_group_is_empty() {
        assert!(observations_from_usage(&json!({"groups": []}), 0).is_empty());
        assert!(observations_from_usage(&json!({}), 0).is_empty());
    }
}
