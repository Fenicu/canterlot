//! Что значит отказ хода: упор в лимит, переполнение контекста, застрявшая сессия. API квот
//! у OpenCode нет, поэтому источник лимита только отказ провайдера.

use super::events::RunFailure;
use crate::adapter::{LimitFact, LimitWindow};
use sha2::{Digest, Sha256};

const LIMIT_KINDS: [&str; 2] = ["provider.quota", "provider.free-limit"];
const INVALID_REQUEST: &str = "provider.invalid-request";
pub const AUTH_FAILURE: &str = "provider.auth";
const FIVE_HOURS_MS: u64 = 5 * 60 * 60 * 1000;

/// Шаблоны `provider-error.ts` версии 2.1.4 без регулярных выражений. Тип ошибки переполнение
/// не отличает. Пропуск не страшен: остаётся блок ошибки и проверка на застревание.
const OVERFLOW_TEXTS: [&str; 22] = [
    "prompt is too long",
    "prompt too long",
    "prompt exceeds max length",
    "input is too long for",
    "too large for the model",
    "too large for model",
    "exceeded this model context window limit",
    "exceeds the context window",
    "maximum context length",
    "number of input tokens exceeds",
    "reduce the length of the messages",
    "exceeds the available context size",
    "greater than the context length",
    "context window exceeds limit",
    "exceeded model token limit",
    "context_length_exceeded",
    "context length exceeded",
    "context length is only",
    "model_context_window_exceeded",
    "too many tokens",
    "token limit exceeded",
    "request_too_large",
];

pub fn is_overflow(kind: &str, message: &str, body: &str) -> bool {
    if kind != INVALID_REQUEST {
        return false;
    }
    let text = format!("{message}\n{body}").to_lowercase();
    OVERFLOW_TEXTS.iter().any(|pattern| text.contains(pattern))
}

/// Упор в лимит. Окна в отказе нет: дальше пяти часов сброс может быть только у длинного окна.
/// Месячного окна у хаба нет.
pub fn limit_fact(failure: &RunFailure, now_ms: u64) -> Option<LimitFact> {
    if !LIMIT_KINDS.contains(&failure.kind.as_str()) {
        return None;
    }
    let window = match failure.retry_after_ms {
        Some(wait) if wait > FIVE_HOURS_MS => LimitWindow::Weekly,
        _ => LimitWindow::FiveHour,
    };
    Some(LimitFact::Reached {
        window,
        resets_at_ms: failure
            .retry_after_ms
            .and_then(|wait| now_ms.checked_add(wait)),
    })
}

/// Один и тот же отказ подряд: история не меняется, следующий ввод получит то же самое.
/// Причина отказа у провайдера не установлена, поэтому здесь сверяется только текст, и
/// хранится только его сумма.
#[derive(Default)]
pub struct StuckWatch {
    last: Option<[u8; 32]>,
}

impl StuckWatch {
    /// `true`, если этот отказ повторяет предыдущий.
    pub fn on_failure(&mut self, failure: &RunFailure) -> bool {
        if failure.kind != INVALID_REQUEST {
            self.last = None;
            return false;
        }
        let digest: [u8; 32] = Sha256::digest(failure.message.as_bytes()).into();
        self.last.replace(digest) == Some(digest)
    }

    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(kind: &str, message: &str, wait: Option<u64>) -> RunFailure {
        RunFailure {
            kind: kind.into(),
            message: message.into(),
            retry_after_ms: wait,
            overflow: false,
        }
    }

    #[test]
    fn long_wait_goes_to_the_long_window() {
        let fact = limit_fact(
            &failure("provider.free-limit", "x", Some(80_814_000)),
            1_000,
        );
        assert_eq!(
            fact,
            Some(LimitFact::Reached {
                window: LimitWindow::Weekly,
                resets_at_ms: Some(80_815_000),
            })
        );
    }

    #[test]
    fn short_wait_and_no_wait_go_to_the_short_window() {
        let fact = limit_fact(&failure("provider.quota", "x", Some(3_600_000)), 5);
        assert_eq!(
            fact,
            Some(LimitFact::Reached {
                window: LimitWindow::FiveHour,
                resets_at_ms: Some(3_600_005),
            })
        );
        let fact = limit_fact(&failure("provider.quota", "x", None), 5);
        assert_eq!(
            fact,
            Some(LimitFact::Reached {
                window: LimitWindow::FiveHour,
                resets_at_ms: None,
            })
        );
    }

    #[test]
    fn other_failures_and_a_huge_wait_are_harmless() {
        assert_eq!(
            limit_fact(&failure("provider.rate-limit", "x", Some(1)), 5),
            None
        );
        let fact = limit_fact(&failure("provider.quota", "x", Some(u64::MAX)), 5);
        assert_eq!(
            fact,
            Some(LimitFact::Reached {
                window: LimitWindow::Weekly,
                resets_at_ms: None,
            })
        );
    }

    #[test]
    fn same_invalid_request_twice_is_stuck() {
        let mut watch = StuckWatch::default();
        let one = failure(INVALID_REQUEST, "same", None);
        assert!(!watch.on_failure(&one));
        assert!(watch.on_failure(&one));
        assert!(watch.on_failure(&one), "every later repeat too");
    }

    #[test]
    fn other_text_or_other_kind_breaks_the_row() {
        let mut watch = StuckWatch::default();
        assert!(!watch.on_failure(&failure(INVALID_REQUEST, "one", None)));
        assert!(!watch.on_failure(&failure(INVALID_REQUEST, "two", None)));
        assert!(!watch.on_failure(&failure("provider.internal", "two", None)));
        assert!(!watch.on_failure(&failure(INVALID_REQUEST, "two", None)));
        watch.reset();
        assert!(!watch.on_failure(&failure(INVALID_REQUEST, "two", None)));
    }

    #[test]
    fn invalid_parameters_are_not_an_overflow() {
        let observed = "The request contains invalid parameters. Check the request body for any errors or inconsistencies.";
        assert!(!is_overflow(INVALID_REQUEST, observed, ""));
        assert!(is_overflow(INVALID_REQUEST, "Prompt is too long", ""));
        assert!(!is_overflow("provider.internal", "prompt is too long", ""));
    }
}
