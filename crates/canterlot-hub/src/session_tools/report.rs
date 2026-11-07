//! `limits.report`: все провайдеры, которые знает хаб, с окнами, процентом расхода,
//! временем сброса и свежестью наблюдения. Ответ только читает снимки.

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

fn window(
    name: &str,
    state: Option<&crate::limits::WindowState>,
    now: chrono::DateTime<Utc>,
) -> Value {
    match state {
        Some(state) => json!({
            "window": name,
            "used_percent": f64::from(state.used_permille) / 10.0,
            "resets_at": state.resets_at.map(rfc3339),
            "observed_at": rfc3339(state.observed_at),
            "age_seconds": (now - state.observed_at).num_seconds().max(0),
            "fresh": (now - state.observed_at).num_seconds() <= FRESH_SECONDS,
            "forecast": state.forecast.as_ref().map(|forecast| {
                serde_json::to_value(crate::http::limits::ForecastView::from(forecast.clone()))
                    .unwrap_or(Value::Null)
            }),
        }),
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

fn line(row: &crate::limits::ProviderLimits, now: chrono::DateTime<Utc>) -> String {
    let mut parts = Vec::new();
    for (name, state) in [
        ("five-hour", row.five_hour.as_ref()),
        ("weekly", row.weekly.as_ref()),
        ("monthly", row.monthly.as_ref()),
    ] {
        if let Some(state) = state {
            let reset = state
                .resets_at
                .map(|at| format!(", resets in {}", countdown(at - now)))
                .unwrap_or_default();
            let forecast = state
                .forecast
                .as_ref()
                .map(|forecast| {
                    let pace = rate_percent(forecast.rate_permille_per_hour);
                    match forecast.exhausts_at {
                        Some(at) if forecast.before_reset => format!(
                            "; at the average pace ({pace}%/h) runs out in {}, before the reset",
                            countdown(at - now)
                        ),
                        _ => format!("; at the average pace ({pace}%/h) lasts until the reset"),
                    }
                })
                .unwrap_or_default();
            parts.push(format!(
                "{name} window {}% used{reset}{forecast}",
                f64::from(state.used_permille) / 10.0
            ));
        }
    }
    if parts.is_empty() {
        return format!("{}: no limit data yet.", label(&row.provider));
    }
    format!("{}: {}.", label(&row.provider), parts.join("; "))
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
    let mut rows = crate::limits::current(&mut conn).await?;
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
                    window("five_hour", row.five_hour.as_ref(), now),
                    window("weekly", row.weekly.as_ref(), now),
                    window("monthly", row.monthly.as_ref(), now),
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
        .map(|row| line(row, now))
        .unwrap_or_else(|| "no limit data yet in this session".to_owned());
    for row in rows.iter().skip(1) {
        text.push('\n');
        text.push_str(&line(row, now));
    }
    if !rows.is_empty() {
        let fresh = rows
            .iter()
            .filter_map(|row| {
                let at = row
                    .five_hour
                    .as_ref()
                    .or(row.weekly.as_ref())
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
