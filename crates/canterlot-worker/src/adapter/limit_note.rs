//! Общая пометка о недельном лимите для адаптеров: одна функция на оба harness,
//! текст отличается только именем провайдера.
pub const WEEKLY_THRESHOLDS: [u16; 3] = [800, 900, 950];

pub fn local_time(ms: u64) -> Option<String> {
    let at = chrono::DateTime::from_timestamp_millis(i64::try_from(ms).ok()?)?;
    Some(
        at.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M %:z")
            .to_string(),
    )
}

// Старший непомеченный порог, crossed weekly; `noted` — прошлая пометка (0 — нет).
pub fn next_threshold(
    weekly: Option<(u16, Option<u64>)>,
    noted: u16,
    provider: &str,
) -> Option<(u16, String)> {
    let (permille, resets_at_ms) = weekly?;
    let threshold = WEEKLY_THRESHOLDS
        .into_iter()
        .filter(|threshold| permille >= *threshold && *threshold > noted)
        .max()?;
    let percent = permille.min(1000) / 10;
    let text = match resets_at_ms.and_then(local_time) {
        Some(when) => {
            format!("[canterlot] Weekly {provider} limit: {percent} % used, resets {when}.")
        }
        None => format!("[canterlot] Weekly {provider} limit: {percent} % used."),
    };
    Some((threshold, text))
}
