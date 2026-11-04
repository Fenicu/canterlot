//! Решение «устоявшееся состояние делегата — это завершение?».
//!
//! Пока завершением считается любое устоявшееся состояние: шлюз автопинка этапа B
//! добавит сюда удержание `idle` без маркера. Доставка подписки, пачки, автопинок и
//! сторож спрашивают это решение только здесь.

use super::settle::Settled;

/// Устоявшееся состояние считается завершением делегата. Разбор исчерпывающий:
/// новое состояние `Settled` обязано получить решение здесь.
pub fn is_completion(state: Settled) -> bool {
    match state {
        Settled::Idle
        | Settled::WaitingForUser
        | Settled::Stalled
        | Settled::QuotaPaused
        | Settled::Failed
        | Settled::OwnerLost
        | Settled::Stopped => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;

    use super::super::settle::{self, SettleConfig, SettleFacts, Settled};
    use crate::{
        activity::{Activity, ActivityState, Evidence, QueueFacts},
        model::RunSlot,
    };

    fn facts(now: chrono::DateTime<chrono::Utc>) -> SettleFacts {
        SettleFacts {
            now,
            since: now - chrono::TimeDelta::hours(1),
            last_turn_at: None,
            last_event_at: Some(now - chrono::TimeDelta::hours(1)),
            phase_since: None,
        }
    }

    fn config() -> SettleConfig {
        SettleConfig {
            idle_quiet: Duration::from_secs(5),
        }
    }

    fn queue() -> QueueFacts {
        QueueFacts {
            slot: RunSlot::Free,
            queued: 0,
            pause_reasons: vec![],
            hold_reasons: vec![],
        }
    }

    #[test]
    fn every_settled_state_is_a_completion_today() {
        for state in [
            Settled::Idle,
            Settled::WaitingForUser,
            Settled::Stalled,
            Settled::QuotaPaused,
            Settled::Failed,
            Settled::OwnerLost,
            Settled::Stopped,
        ] {
            assert!(super::is_completion(state), "{state:?}");
        }
    }

    #[test]
    fn stalled_while_the_turn_is_working_is_not_settled_and_so_not_a_completion() {
        let now = Utc::now();
        let activity = Activity {
            state: ActivityState::Working,
            evidence: Evidence::Working {
                phase: "running_tool".into(),
                tool_name: None,
                summary: None,
                phase_since: Some(now - chrono::TimeDelta::hours(1)),
            },
        };
        assert_eq!(
            settle::settled(&activity, &queue(), &facts(now), &config()),
            None,
            "работающий ход не осел, завершением он быть не может"
        );
    }
}
