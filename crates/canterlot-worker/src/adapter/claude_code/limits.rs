use crate::adapter::{LimitFact, LimitWindow};
use serde_json::Value;

/// Промилле и сброс (мс) каждого окна по последнему показанию исполнения.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitSnapshot {
    pub five_hour: Option<(u16, Option<u64>)>,
    pub weekly: Option<(u16, Option<u64>)>,
    pub weekly_fable: Option<(u16, Option<u64>)>,
}

const WINDOWS: [(&str, LimitWindow); 3] = [
    ("five_hour", LimitWindow::FiveHour),
    ("seven_day", LimitWindow::Weekly),
    ("seven_day_overage_included", LimitWindow::WeeklyFable),
];

#[derive(Debug, Default)]
pub struct LimitTracker {
    snapshot: LimitSnapshot,
    resets: [Option<u64>; 3],
    limiting: Option<LimitWindow>,
    // Упор, уже сообщённый в текущем эпизоде: повтор того же кадра или 429 хода его не дублирует.
    reached: [Option<Option<u64>>; 3],
    /// Текущий запрос идёт к модели Fable: только тогда недельное окно Fable к нему применимо.
    fable: bool,
}

/// Модель Fable по полному id (`claude-fable-5-2`) или псевдониму (`fable[1m]`).
pub fn is_fable(model: &str) -> bool {
    model.to_lowercase().contains("fable")
}

fn index(window: LimitWindow) -> usize {
    match window {
        LimitWindow::FiveHour => 0,
        LimitWindow::Weekly => 1,
        LimitWindow::WeeklyFable => 2,
    }
}

fn window_named(name: &str) -> Option<LimitWindow> {
    WINDOWS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, w)| *w)
}

fn permille(utilization: f64) -> Option<u16> {
    utilization
        .is_finite()
        .then(|| (utilization * 1000.0).round().clamp(0.0, 1000.0) as u16)
}

fn epoch_ms(seconds: &Value) -> Option<u64> {
    let seconds = seconds.as_u64().or_else(|| {
        seconds
            .as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| s as u64)
    })?;
    Some(seconds.saturating_mul(1000))
}

fn rejected(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("rejected")
}

impl LimitTracker {
    /// Модель текущего запроса. Окно Fable винят только для Fable, но показания
    /// окна копятся при любой модели: их принесёт первый же кадр Fable.
    pub fn set_model(&mut self, model: &str) {
        let fable = is_fable(model);
        if self.fable && !fable {
            // Названный провайдером упор относился к окну Fable и со сменой модели
            // протух: запасной путь следующего 429 не должен винить чужое окно.
            if self.limiting == Some(LimitWindow::WeeklyFable) {
                self.limiting = None;
            }
            self.reached[index(LimitWindow::WeeklyFable)] = None;
        }
        self.fable = fable;
    }

    /// `rate_limit_info` из `rate_limit_event` или из кадра ошибки `assistant`.
    pub fn on_rate_limit(&mut self, info: &Value) -> Vec<LimitFact> {
        let mut facts = Vec::new();
        let windows = info.get("unifiedWindows");
        let limiting = info
            .get("rateLimitType")
            .and_then(Value::as_str)
            .and_then(window_named);
        if limiting.is_some() {
            self.limiting = limiting;
        }
        let mut exhausted = [false; 3];
        let mut fresh = [false; 3];
        for (key, window) in WINDOWS {
            let i = index(window);
            let Some(row) = windows.and_then(|w| w.get(key)) else {
                continue;
            };
            let resets = row.get("resetsAt").and_then(epoch_ms);
            if resets.is_some() {
                self.resets[i] = resets;
                fresh[i] = true;
            }
            if let Some(used) = row
                .get("utilization")
                .and_then(Value::as_f64)
                .and_then(permille)
            {
                let value = Some((used, resets));
                match window {
                    LimitWindow::FiveHour => self.snapshot.five_hour = value,
                    LimitWindow::Weekly => self.snapshot.weekly = value,
                    LimitWindow::WeeklyFable => self.snapshot.weekly_fable = value,
                }
                facts.push(LimitFact::Observed {
                    window,
                    used_permille: used,
                    resets_at_ms: resets,
                });
            }
            exhausted[i] = rejected(row);
        }
        if rejected(info) {
            let i = index(limiting.unwrap_or_else(|| self.highest()));
            if let Some(resets) = info.get("resetsAt").and_then(epoch_ms)
                && !fresh[i]
            {
                self.resets[i] = Some(resets);
            }
            exhausted[i] = true;
        }
        for (_, window) in WINDOWS {
            let i = index(window);
            if !exhausted[i] {
                self.reached[i] = None;
            } else if self.reached[i] != Some(self.resets[i]) {
                self.reached[i] = Some(self.resets[i]);
                facts.push(LimitFact::Reached {
                    window,
                    resets_at_ms: self.resets[i],
                });
            }
        }
        facts
    }

    /// Ход упал: 429 или `usage_limit_reached` — упор в окно из `rateLimitType`
    /// (иначе в самое загруженное) со сбросом из последнего `rate_limit_info`.
    pub fn on_turn_error(&mut self, status: Option<u16>, error: Option<&str>) -> Option<LimitFact> {
        let usage_limit =
            status == Some(429) || error.is_some_and(|e| e.contains("usage_limit_reached"));
        let reported = self.reached.iter().any(Option::is_some);
        self.reached = [None; 3];
        if !usage_limit || reported {
            return None;
        }
        let window = self
            .limiting
            .filter(|window| *window != LimitWindow::WeeklyFable || self.fable)
            .unwrap_or_else(|| self.highest());
        Some(LimitFact::Reached {
            window,
            resets_at_ms: self.resets[index(window)],
        })
    }

    pub fn snapshot(&self) -> LimitSnapshot {
        self.snapshot.clone()
    }

    fn highest(&self) -> LimitWindow {
        let used = |v: Option<(u16, Option<u64>)>| v.map(|(p, _)| p);
        let mut window = LimitWindow::FiveHour;
        let mut best = used(self.snapshot.five_hour);
        if used(self.snapshot.weekly) > best {
            window = LimitWindow::Weekly;
            best = used(self.snapshot.weekly);
        }
        if self.fable && used(self.snapshot.weekly_fable) > best {
            window = LimitWindow::WeeklyFable;
        }
        window
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RESET: u64 = 1_794_877_200;

    /// Кадр Fable: недельное 30 %, окно Fable 95 %, без явного `rateLimitType`.
    fn fable_frame() -> Value {
        json!({
            "status": "allowed",
            "unifiedWindows": {
                "seven_day": {"utilization": 0.3, "resetsAt": RESET, "status": "allowed"},
                "seven_day_overage_included": {"utilization": 0.95, "resetsAt": RESET, "status": "allowed"}
            }
        })
    }

    fn blamed(facts: &[LimitFact]) -> Option<LimitWindow> {
        facts.iter().find_map(|fact| match fact {
            LimitFact::Reached { window, .. } => Some(*window),
            LimitFact::Observed { .. } => None,
        })
    }

    fn tracker_on_fable() -> LimitTracker {
        let mut tracker = LimitTracker::default();
        tracker.set_model("fable[1m]");
        tracker.on_rate_limit(&fable_frame());
        tracker
    }

    #[test]
    fn a_fable_reading_never_blames_opus_after_a_switch() {
        let mut tracker = tracker_on_fable();
        tracker.set_model("opus");
        let facts = tracker.on_rate_limit(&json!({"status": "rejected", "resetsAt": RESET}));
        assert_eq!(blamed(&facts), Some(LimitWindow::Weekly), "{facts:?}");
    }

    #[test]
    fn a_bare_429_after_a_switch_does_not_blame_the_fable_window() {
        let mut tracker = tracker_on_fable();
        tracker.set_model("opus");
        let fact = tracker.on_turn_error(Some(429), None);
        assert_eq!(
            fact.as_ref().map(|f| match f {
                LimitFact::Reached { window, .. } => *window,
                LimitFact::Observed { .. } => unreachable!(),
            }),
            Some(LimitWindow::Weekly)
        );
    }

    #[test]
    fn a_sticky_fable_limit_is_dropped_on_the_switch() {
        let mut tracker = LimitTracker::default();
        tracker.set_model("fable[1m]");
        tracker.on_rate_limit(&json!({
            "status": "allowed",
            "rateLimitType": "seven_day_overage_included",
            "unifiedWindows": {
                "seven_day": {"utilization": 0.3, "resetsAt": RESET, "status": "allowed"},
                "seven_day_overage_included": {"utilization": 0.95, "resetsAt": RESET, "status": "allowed"}
            }
        }));
        tracker.set_model("opus");
        let fact = tracker.on_turn_error(Some(429), None);
        assert_ne!(
            fact.as_ref().map(|f| match f {
                LimitFact::Reached { window, .. } => *window,
                LimitFact::Observed { .. } => unreachable!(),
            }),
            Some(LimitWindow::WeeklyFable)
        );
    }

    #[test]
    fn the_fallback_still_names_the_fable_window_while_the_model_is_fable() {
        let mut tracker = tracker_on_fable();
        let facts = tracker.on_rate_limit(&json!({"status": "rejected", "resetsAt": RESET}));
        assert_eq!(blamed(&facts), Some(LimitWindow::WeeklyFable), "{facts:?}");
    }

    #[test]
    fn an_explicit_fable_type_wins_over_the_model_flag() {
        let mut tracker = LimitTracker::default();
        tracker.set_model("opus");
        let facts = tracker.on_rate_limit(&json!({
            "status": "rejected",
            "rateLimitType": "seven_day_overage_included",
            "resetsAt": RESET,
            "unifiedWindows": {
                "seven_day": {"utilization": 0.3, "resetsAt": RESET, "status": "allowed"},
                "seven_day_overage_included": {"utilization": 1.0, "resetsAt": RESET, "status": "rejected"}
            }
        }));
        assert_eq!(blamed(&facts), Some(LimitWindow::WeeklyFable), "{facts:?}");
    }

    #[test]
    fn switching_back_to_fable_restores_the_fallback() {
        let mut tracker = tracker_on_fable();
        tracker.set_model("opus");
        tracker.on_rate_limit(&json!({"status": "rejected", "resetsAt": RESET}));
        tracker.set_model("fable[1m]");
        let facts = tracker.on_rate_limit(&json!({"status": "rejected", "resetsAt": RESET}));
        assert_eq!(blamed(&facts), Some(LimitWindow::WeeklyFable), "{facts:?}");
    }

    #[test]
    fn is_fable_matches_ids_and_aliases() {
        assert!(is_fable("fable[1m]"));
        assert!(is_fable("claude-fable-5-2"));
        assert!(is_fable("FABLE"));
        assert!(!is_fable("opus"));
        assert!(!is_fable(""));
    }
}
