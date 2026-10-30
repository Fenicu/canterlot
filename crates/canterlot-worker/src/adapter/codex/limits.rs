use crate::adapter::{LimitFact, LimitWindow};
use canterlot_proto::worker::v1::LimitObservation;
use serde_json::Value;

/// Промилле и сброс (мс) по окнам для MCP `canterlot_limits`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitSnapshot {
    pub five_hour: Option<(u16, Option<u64>)>,
    pub weekly: Option<(u16, Option<u64>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrunchContext {
    UsageLimitExceeded,
    RateLimitExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReachedDecision {
    Reached(LimitFact),
    NeedFreshRead(CrunchContext),
    NotALimit,
}

#[derive(Debug)]
pub struct TurnErrorInfo {
    pub codex_error_info: Option<String>,
}

#[derive(Debug, Default)]
struct WindowState {
    permille: Option<u16>,
    resets_at_ms: Option<u64>,
    last_emitted_permille: Option<u16>,
    last_emitted_reset: Option<u64>,
}

#[derive(Debug, Default)]
pub struct LimitTracker {
    five_hour: WindowState,
    weekly: WindowState,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn slot(tracker: &mut LimitTracker, window: LimitWindow) -> &mut WindowState {
    match window {
        LimitWindow::FiveHour => &mut tracker.five_hour,
        LimitWindow::Weekly => &mut tracker.weekly,
    }
}

fn window_from_duration(mins: u64) -> Option<LimitWindow> {
    match mins {
        300 => Some(LimitWindow::FiveHour),
        10080 => Some(LimitWindow::Weekly),
        _ => None,
    }
}

fn limit_id_ok(value: &Value) -> bool {
    match value.get("limitId") {
        None | Some(Value::Null) => true,
        Some(Value::String(id)) => id == "codex",
        _ => false,
    }
}

fn permille_from_percent(value: &Value) -> Option<u16> {
    let percent = value.as_f64().filter(|p| p.is_finite())?;
    Some((percent * 10.0).round().clamp(0.0, 1000.0) as u16)
}

fn resets_at_ms(value: &Value) -> Option<u64> {
    let seconds = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| s as u64)
    })?;
    Some(seconds.saturating_mul(1000))
}

// По схеме RateLimitWindow обязателен только usedPercent: без resetsAt показание
// остаётся. Окно без длительности отбрасывается: его нечем опознать.
fn parse_window(row: &Value) -> Option<(LimitWindow, u16, Option<u64>)> {
    let mins = row.get("windowDurationMins")?.as_u64()?;
    let window = window_from_duration(mins)?;
    let permille = permille_from_percent(row.get("usedPercent")?)?;
    let reset = row.get("resetsAt").and_then(resets_at_ms);
    Some((window, permille, reset))
}

// Поле ответа account/rateLimits/read (GetAccountRateLimitsResponse), не снимка.
fn ordinary_usage_blocked(response: &Value) -> bool {
    response
        .get("ordinaryUsageAllowed")
        .is_some_and(|v| v.as_bool() == Some(false))
}

/// Окна из одного `RateLimitSnapshot` (primary/secondary).
pub fn windows_from_snapshot(snapshot: &Value) -> Vec<(LimitWindow, u16, Option<u64>)> {
    if !limit_id_ok(snapshot) {
        return Vec::new();
    }
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| snapshot.get(key).and_then(parse_window))
        .collect()
}

fn snapshots_from_read_response(response: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(rate_limits) = response.get("rateLimits") {
        out.push(rate_limits.clone());
    }
    if let Some(by_id) = response
        .get("rateLimitsByLimitId")
        .and_then(|m| m.get("codex"))
    {
        out.push(by_id.clone());
    }
    out
}

pub fn observations_from_read_response(
    response: &Value,
    observed_ms: u64,
) -> Vec<LimitObservation> {
    let mut observations = Vec::new();
    for snapshot in snapshots_from_read_response(response) {
        for (window, permille, resets_at_ms) in windows_from_snapshot(&snapshot) {
            observations.push(LimitObservation {
                provider: "codex".into(),
                window: window.into(),
                used_permille: u32::from(permille),
                resets_at_ms: resets_at_ms.unwrap_or_default(),
                observed_machine_ms: observed_ms,
            });
        }
    }
    observations
}

impl LimitTracker {
    pub fn on_snapshot(&mut self, snapshot: &Value) -> Vec<LimitFact> {
        let mut facts = Vec::new();
        for (window, permille, resets_at_ms) in windows_from_snapshot(snapshot) {
            let state = slot(self, window);
            let resets_at_ms = resets_at_ms.or(state.resets_at_ms);
            state.permille = Some(permille);
            state.resets_at_ms = resets_at_ms;
            let changed = state.last_emitted_permille != Some(permille)
                || state.last_emitted_reset != resets_at_ms;
            if changed {
                state.last_emitted_permille = Some(permille);
                state.last_emitted_reset = resets_at_ms;
                facts.push(LimitFact::Observed {
                    window,
                    used_permille: permille,
                    resets_at_ms,
                });
            }
        }
        facts
    }

    pub fn on_turn_failed(&mut self, error: &TurnErrorInfo, now_ms: u64) -> ReachedDecision {
        match error.codex_error_info.as_deref() {
            Some("usageLimitExceeded") => {
                if let Some(window) = self.exhausted_window_prefer_weekly(now_ms) {
                    return ReachedDecision::Reached(LimitFact::Reached {
                        window,
                        resets_at_ms: slot(self, window).resets_at_ms,
                    });
                }
                ReachedDecision::NeedFreshRead(CrunchContext::UsageLimitExceeded)
            }
            Some("rateLimitExceeded") => {
                ReachedDecision::NeedFreshRead(CrunchContext::RateLimitExceeded)
            }
            _ => ReachedDecision::NotALimit,
        }
    }

    pub fn on_fresh_read(
        &mut self,
        response: &Value,
        ctx: CrunchContext,
        now_ms: u64,
    ) -> Vec<LimitFact> {
        let mut facts = Vec::new();
        for snapshot in snapshots_from_read_response(response) {
            facts.extend(self.on_snapshot(&snapshot));
        }
        let exhausted = self.exhausted_window_prefer_weekly(now_ms);
        let blocked = ordinary_usage_blocked(response);
        let highest = self.highest_window();
        match ctx {
            CrunchContext::UsageLimitExceeded => {
                let window = exhausted.unwrap_or(highest);
                facts.push(LimitFact::Reached {
                    window,
                    resets_at_ms: slot(self, window).resets_at_ms,
                });
            }
            CrunchContext::RateLimitExceeded => {
                if exhausted.is_some() || blocked {
                    let window = exhausted.unwrap_or(highest);
                    facts.push(LimitFact::Reached {
                        window,
                        resets_at_ms: slot(self, window).resets_at_ms,
                    });
                }
            }
        }
        facts
    }

    // Свежего снимка нет, но упор в лимит уже случился: очередь должна встать, иначе
    // следующий ввод упадёт так же и запустит ещё одно чтение.
    pub fn on_read_failed(&mut self, ctx: CrunchContext, now_ms: u64) -> Vec<LimitFact> {
        if ctx != CrunchContext::UsageLimitExceeded {
            return Vec::new();
        }
        let window = self
            .exhausted_window_prefer_weekly(now_ms)
            .unwrap_or_else(|| self.highest_window());
        let resets_at_ms = slot(self, window)
            .resets_at_ms
            .filter(|reset| *reset > now_ms);
        vec![LimitFact::Reached {
            window,
            resets_at_ms,
        }]
    }

    pub fn snapshot(&self) -> LimitSnapshot {
        LimitSnapshot {
            five_hour: self
                .five_hour
                .permille
                .map(|p| (p, self.five_hour.resets_at_ms)),
            weekly: self.weekly.permille.map(|p| (p, self.weekly.resets_at_ms)),
        }
    }

    // Окно со сбросом в прошлом исчерпанным не считается: снимок устарел, и его
    // время только разбудило бы очередь сразу же.
    fn exhausted_window_prefer_weekly(&self, now_ms: u64) -> Option<LimitWindow> {
        let exhausted = |state: &WindowState| {
            state.permille.is_some_and(|p| p >= 1000)
                && state.resets_at_ms.is_none_or(|reset| reset > now_ms)
        };
        let five = exhausted(&self.five_hour).then_some(LimitWindow::FiveHour);
        let weekly = exhausted(&self.weekly).then_some(LimitWindow::Weekly);
        weekly.or(five)
    }

    fn highest_window(&self) -> LimitWindow {
        let five = self.five_hour.permille.unwrap_or(0);
        let weekly = self.weekly.permille.unwrap_or(0);
        if weekly > five {
            LimitWindow::Weekly
        } else {
            LimitWindow::FiveHour
        }
    }
}
