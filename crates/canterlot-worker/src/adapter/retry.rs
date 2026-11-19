//! Повтор запроса к модели: типизированная причина из того, что CLI уже сообщает сам.
//! Наружу идут только числа и категория из закрытого списка, текст ошибки не пересылается.

use canterlot_proto::worker::v1::RetryInfo;
use serde_json::Value;

pub(crate) fn category(raw: Option<&str>) -> &'static str {
    canterlot_proto::retry_category(raw.unwrap_or_default())
}

fn count(value: &Value) -> u32 {
    value
        .as_u64()
        .map_or(0, |n| n.min(u64::from(u32::MAX)) as u32)
}

/// Кадр `system/api_retry` Claude Code.
pub(crate) fn from_claude(frame: &Value) -> RetryInfo {
    RetryInfo {
        attempt: count(&frame["attempt"]),
        max_retries: count(&frame["max_retries"]),
        delay_ms: frame["retry_delay_ms"].as_u64().unwrap_or(0),
        http_status: count(&frame["error_status"]),
        category: category(frame["error"].as_str()).into(),
        no_response_waited_ms: frame["no_response"]["waited_ms"].as_u64().unwrap_or(0),
    }
}

/// `session.retry.scheduled` OpenCode: `at` — время следующей попытки в мс эпохи.
pub(crate) fn from_opencode(data: &Value, now_ms: u64) -> RetryInfo {
    let status = count(&data["error"]["status"]);
    let category = match data["error"]["type"].as_str() {
        Some("provider.rate-limit") => "rate_limit",
        Some("provider.auth") => "authentication_failed",
        Some("provider.invalid-request") => "invalid_request",
        Some("provider.internal") => "server_error",
        _ => match status {
            529 => "overloaded",
            429 => "rate_limit",
            401 | 403 => "authentication_failed",
            402 => "billing_error",
            500..=599 => "server_error",
            _ => "unknown",
        },
    };
    RetryInfo {
        attempt: data["attempt"]
            .as_u64()
            .map_or(1, |n| n.min(u64::from(u32::MAX)) as u32),
        max_retries: 0,
        delay_ms: data["at"]
            .as_u64()
            .map_or(0, |at| at.saturating_sub(now_ms)),
        http_status: status,
        category: category.into(),
        no_response_waited_ms: 0,
    }
}

/// Строка для старых PWA: `retry 2/10`, без известного потолка `retry 2`.
pub(crate) fn summary(info: &RetryInfo) -> String {
    if info.max_retries > 0 {
        format!("retry {}/{}", info.attempt, info.max_retries)
    } else {
        format!("retry {}", info.attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_frame_with_status_and_delay() {
        let info = from_claude(&json!({"type":"system","subtype":"api_retry","attempt":2,
            "max_retries":10,"retry_delay_ms":8000,"error_status":529,"error":"overloaded"}));
        assert_eq!(
            info,
            RetryInfo {
                attempt: 2,
                max_retries: 10,
                delay_ms: 8000,
                http_status: 529,
                category: "overloaded".into(),
                no_response_waited_ms: 0,
            }
        );
        assert_eq!(summary(&info), "retry 2/10");
    }

    #[test]
    fn claude_frame_without_http_response_has_status_zero() {
        let info = from_claude(&json!({"attempt":1,"max_retries":5,"retry_delay_ms":500,
            "error_status":null,"error":"server_error"}));
        assert_eq!(info.http_status, 0);
        assert_eq!(info.category, "server_error");
    }

    #[test]
    fn claude_unknown_or_hostile_category_becomes_unknown() {
        for raw in [
            json!("brand_new_kind"),
            json!("Rate limit: key sk-secret"),
            json!(7),
            Value::Null,
        ] {
            let info = from_claude(&json!({"attempt":1,"max_retries":5,"error":raw}));
            assert_eq!(info.category, "unknown", "{raw}");
        }
    }

    #[test]
    fn claude_no_response_carries_the_wait() {
        let info = from_claude(&json!({"attempt":2,"max_retries":10,"retry_delay_ms":4000,
            "error_status":null,"error":"unknown",
            "no_response":{"waited_ms":185000,"retry_wait_ms":4000}}));
        assert_eq!(info.no_response_waited_ms, 185_000);
    }

    #[test]
    fn every_documented_category_passes_through() {
        for name in canterlot_proto::RETRY_CATEGORIES {
            assert_eq!(from_claude(&json!({"error": name})).category, name);
        }
    }

    #[test]
    fn missing_fields_are_zero_and_never_panic() {
        let info = from_claude(&json!({"attempt": -1, "max_retries": "x", "retry_delay_ms": 1.5}));
        assert_eq!((info.attempt, info.max_retries, info.delay_ms), (0, 0, 0));
    }

    #[test]
    fn opencode_event_gives_attempt_and_delay_until_the_next_try() {
        let info = from_opencode(
            &json!({"attempt":3,"at":1_700_000_008_000u64,
                "error":{"type":"provider.rate-limit","message":"slow down","status":429}}),
            1_700_000_000_000,
        );
        assert_eq!(
            (info.attempt, info.delay_ms, info.http_status),
            (3, 8000, 429)
        );
        assert_eq!(info.category, "rate_limit");
        assert_eq!(summary(&info), "retry 3");
    }

    #[test]
    fn opencode_past_time_gives_zero_delay_and_status_picks_the_category() {
        let info = from_opencode(
            &json!({"attempt":1,"at":1,"error":{"type":"x","status":529}}),
            1_000,
        );
        assert_eq!((info.delay_ms, info.category.as_str()), (0, "overloaded"));
        assert_eq!(from_opencode(&json!({}), 0).category, "unknown");
    }
}
