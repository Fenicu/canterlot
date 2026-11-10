//! `limits.report`: провайдеры, которые PWA показывает владельцу (`limits::shown`: харнесс
//! есть в каталоге хотя бы одной его машины), с окнами, процентом расхода, временем сброса
//! и свежестью наблюдения. Ответ только читает снимки.

use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Caller, ToolContext, ToolError, parse_arguments};

/// Наблюдение старше этого срока считается несвежим: воркер всё равно показывает его,
/// но владелец видит возраст данных.
const FRESH_SECONDS: i64 = 600;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    provider: Option<String>,
}

fn rfc3339(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Устаревшее окно по тому же правилу, что и PWA (`windowText`): наблюдение старше
/// `limits.stale_after` или сброс уже прошёл. У такого окна нет ни прогноза, ни отсчёта
/// до прошедшего сброса.
#[derive(Debug, Clone, Copy)]
struct Staleness {
    stale: bool,
    reset_passed: bool,
}

fn staleness(
    state: &crate::limits::WindowState,
    now: chrono::DateTime<Utc>,
    stale_after: std::time::Duration,
) -> Staleness {
    let reset_passed = state.resets_at.is_some_and(|at| at <= now);
    let too_old = (now - state.observed_at).num_seconds()
        > i64::try_from(stale_after.as_secs()).unwrap_or(i64::MAX);
    Staleness {
        stale: reset_passed || too_old,
        reset_passed,
    }
}

fn window(
    name: &str,
    state: Option<&crate::limits::WindowState>,
    now: chrono::DateTime<Utc>,
    stale_after: std::time::Duration,
) -> Value {
    match state {
        Some(state) => {
            let Staleness {
                stale,
                reset_passed,
            } = staleness(state, now, stale_after);
            json!({
            "window": name,
            "used_percent": f64::from(state.used_permille) / 10.0,
            "resets_at": state.resets_at.map(rfc3339),
            "observed_at": rfc3339(state.observed_at),
            "age_seconds": (now - state.observed_at).num_seconds().max(0),
            "fresh": (now - state.observed_at).num_seconds() <= FRESH_SECONDS,
            "stale": stale,
            "reset_passed": reset_passed,
            "forecast": state.forecast.as_ref().filter(|_| !stale).map(|forecast| {
                serde_json::to_value(crate::http::limits::ForecastView::from(forecast.clone()))
                    .unwrap_or(Value::Null)
            }),
            })
        }
        None => json!({"window": name, "absent": true}),
    }
}

fn label(provider: &str) -> &str {
    match provider {
        "codex" => "Codex API budget",
        "opencode" => "OpenCode API budget",
        "antigravity" => "Antigravity (Gemini)",
        _ => "Claude API budget",
    }
}

/// Срок до события простыми словами: `9h 30m`, `2d 4h`, `25m`.
fn countdown(delta: chrono::TimeDelta) -> String {
    let minutes = delta.num_minutes().max(0);
    if minutes == 0 {
        return "less than a minute".to_owned();
    }
    let days = minutes / (24 * 60);
    let hours = (minutes % (24 * 60)) / 60;
    let minutes = minutes % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{minutes}m")
    }
}

fn rate_percent(permille_per_hour: f64) -> String {
    let value = permille_per_hour / 10.0;
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value:.1}")
    }
}

fn line(
    row: &crate::limits::ProviderLimits,
    now: chrono::DateTime<Utc>,
    stale_after: std::time::Duration,
) -> String {
    let mut parts = Vec::new();
    for (name, state) in [
        ("five-hour", row.five_hour.as_ref()),
        ("weekly", row.weekly.as_ref()),
        (
            "Fable weekly",
            if row.provider == "claude" {
                row.weekly_fable.as_ref()
            } else {
                None
            },
        ),
        ("monthly", row.monthly.as_ref()),
    ] {
        if let Some(state) = state {
            let Staleness {
                stale,
                reset_passed,
            } = staleness(state, now, stale_after);
            if reset_passed {
                // Процент относится к прошлому окну: после сброса он уже неверен.
                parts.push(format!(
                    "{name} window: {}, reset already passed",
                    data_from(state.observed_at, now)
                ));
                continue;
            }
            let reset = state
                .resets_at
                .map(|at| format!(", resets in {}", countdown(at - now)))
                .unwrap_or_default();
            let forecast = state
                .forecast
                .as_ref()
                .filter(|_| !stale)
                .map(|forecast| {
                    let pace = rate_percent(forecast.rate_permille_per_hour);
                    match forecast.exhausts_at {
                        Some(at) if forecast.before_reset && at <= now => format!(
                            "; at the average pace ({pace}%/h) runs out now, before the reset"
                        ),
                        Some(at) if forecast.before_reset => format!(
                            "; at the average pace ({pace}%/h) runs out in {}, before the reset",
                            countdown(at - now)
                        ),
                        _ => format!("; at the average pace ({pace}%/h) lasts until the reset"),
                    }
                })
                .unwrap_or_default();
            let data_age = if stale {
                format!(" (stale, {})", data_from(state.observed_at, now))
            } else {
                String::new()
            };
            parts.push(format!(
                "{name} window {}% used{data_age}{reset}{forecast}",
                f64::from(state.used_permille) / 10.0
            ));
        }
    }
    if parts.is_empty() {
        return format!("{}: no limit data yet.", label(&row.provider));
    }
    format!("{}: {}.", label(&row.provider), parts.join("; "))
}

/// Возраст данных окна: `data from 61 h ago` или `data from just now`.
fn data_from(at: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> String {
    format!("data from {}", age(at, now))
}

fn age(at: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> String {
    let seconds = (now - at).num_seconds().max(0);
    if seconds < 60 {
        "just now".to_owned()
    } else if seconds < 3600 {
        format!("{} min ago", seconds / 60)
    } else {
        format!("{} h ago", seconds / 3600)
    }
}

pub async fn report(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: Args = parse_arguments(arguments)?;
    let mut conn = context.pool.acquire().await?;
    // То же правило видимости, что у полосы и экрана лимитов в PWA.
    let mut rows = crate::limits::shown(&mut conn, caller.owner_id).await?;
    let stale_after = context.limits.stale_after;
    rows.sort_by(|a, b| {
        let first = args.provider.as_deref();
        let rank = |provider: &str| (Some(provider) != first, provider.to_owned());
        rank(&a.provider).cmp(&rank(&b.provider))
    });
    let now = Utc::now();
    let providers: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "provider": row.provider,
                "windows": [
                    window("five_hour", row.five_hour.as_ref(), now, stale_after),
                    window("weekly", row.weekly.as_ref(), now, stale_after),
                    window("weekly_fable", row.weekly_fable.as_ref(), now, stale_after),
                    window("monthly", row.monthly.as_ref(), now, stale_after),
                ],
                "five_hour_absent": row.five_hour_absent,
                "weekly_reserve_active": row.weekly_reserve_active,
            })
        })
        .collect();
    tracing::info!(
        kind = "agent_call",
        owner_id = %caller.owner_id,
        providers = providers.len(),
        "limits report read"
    );
    let mut text = rows
        .first()
        .map(|row| line(row, now, stale_after))
        .unwrap_or_else(|| "no limit data yet in this session".to_owned());
    for row in rows.iter().skip(1) {
        text.push('\n');
        text.push_str(&line(row, now, stale_after));
    }
    if !rows.is_empty() {
        let fresh = rows
            .iter()
            .filter_map(|row| {
                let at = row
                    .five_hour
                    .as_ref()
                    .or(row.weekly.as_ref())
                    .or(row.weekly_fable.as_ref())
                    .or(row.monthly.as_ref())?
                    .observed_at;
                Some(format!("{} {}", row.provider, age(at, now)))
            })
            .collect::<Vec<_>>();
        if !fresh.is_empty() {
            text.push_str(&format!("\nObserved: {}.", fresh.join(", ")));
        }
    }
    Ok(json!({
        "providers": providers,
        "text": text,
        "hint": "Fresh means observed within the last ten minutes."
    }))
}
