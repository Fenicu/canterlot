//! Прогноз расхода лимита: средний темп окна, недавний темп и ожидаемое исчерпание.
//!
//! Чистая функция [`forecast`] получает текущее показание, снимки истории и `now`.
//! Снимки другого окна (с другим `resets_at`) не смешиваются: провайдеры сдвигают
//! время сброса на минуты, поэтому совпадение проверяется с допуском ±2 минуты.
//! Точность показания (`data_confidence`) на прогноз не влияет: оценочные
//! показания считаются так же, а их признак клиент видит в поле провайдера.

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use sqlx::{PgConnection, Row};

use crate::limits::Window;

/// Допуск сравнения времени сброса: провайдеры дрожат в пределах пары минут.
const RESET_TOLERANCE: TimeDelta = TimeDelta::minutes(2);
/// Ниже этого расхода прогноз не строится: шум раннего окна слишком велик.
const MIN_USED_PERMILLE: u16 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub used_permille: u16,
    pub resets_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

/// Прогноз одного окна. Темпы — промилле в час.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Forecast {
    /// Средний темп от начала окна: `used / часы(последнее - начало окна)`.
    pub rate_permille_per_hour: f64,
    /// Темп за последний час (пятичасовое окно) или шесть часов (неделя, месяц).
    pub recent_rate_permille_per_hour: Option<f64>,
    /// Когда окно кончится при среднем темпе; `None`, если темп нулевой.
    pub exhausts_at: Option<DateTime<Utc>>,
    /// Исчерпание наступит раньше сброса окна.
    pub before_reset: bool,
}

impl Forecast {
    /// Темп для решения маршрута: больший из среднего и недавнего.
    pub fn pace_permille_per_hour(&self) -> f64 {
        self.recent_rate_permille_per_hour
            .map_or(self.rate_permille_per_hour, |recent| {
                self.rate_permille_per_hour.max(recent)
            })
    }
}

struct Rules {
    nominal: TimeDelta,
    min_elapsed: TimeDelta,
    recent_lookback: TimeDelta,
    min_recent_span: TimeDelta,
}

fn rules(window: Window) -> Rules {
    match window {
        Window::FiveHour => Rules {
            nominal: TimeDelta::hours(5),
            min_elapsed: TimeDelta::minutes(30),
            recent_lookback: TimeDelta::minutes(60),
            min_recent_span: TimeDelta::minutes(15),
        },
        Window::Weekly => Rules {
            nominal: TimeDelta::days(7),
            min_elapsed: TimeDelta::hours(12),
            recent_lookback: TimeDelta::hours(6),
            min_recent_span: TimeDelta::hours(2),
        },
        Window::Monthly => Rules {
            nominal: TimeDelta::days(30),
            min_elapsed: TimeDelta::days(2),
            recent_lookback: TimeDelta::hours(6),
            min_recent_span: TimeDelta::hours(2),
        },
    }
}

fn same_window(reading: &Reading, resets_at: DateTime<Utc>) -> bool {
    reading
        .resets_at
        .is_some_and(|reset| (reset - resets_at).abs() <= RESET_TOLERANCE)
}

/// Прогноз окна по текущему показанию и снимкам истории.
///
/// `current` — строка `provider_limits`, `snapshots` — история `limit_snapshots`.
/// `None` означает «данных мало»; клиент в этом случае не показывает прогноз.
pub fn forecast(
    window: Window,
    current: Reading,
    snapshots: &[Reading],
    now: DateTime<Utc>,
) -> Option<Forecast> {
    let resets_at = current.resets_at?;
    let rules = rules(window);
    if current.used_permille < MIN_USED_PERMILLE {
        return None;
    }
    // Начало окна — самое раннее показание с тем же временем сброса. Если истории
    // нет, окно считается номинальным: сброс минус длина окна.
    let window_start = snapshots
        .iter()
        .filter(|reading| same_window(reading, resets_at))
        .map(|reading| reading.observed_at)
        .min()
        .unwrap_or(resets_at - rules.nominal);
    let elapsed = current.observed_at - window_start;
    if elapsed < rules.min_elapsed {
        return None;
    }
    let hours = elapsed.num_seconds() as f64 / 3600.0;
    if hours <= 0.0 {
        return None;
    }
    let rate = f64::from(current.used_permille) / hours;
    if rate <= 0.0 {
        return Some(Forecast {
            rate_permille_per_hour: 0.0,
            recent_rate_permille_per_hour: None,
            exhausts_at: None,
            before_reset: false,
        });
    }
    let remaining = f64::from(1000 - current.used_permille);
    let exhausts_at =
        current.observed_at + TimeDelta::seconds((remaining / rate * 3600.0).round() as i64);
    let before_reset = exhausts_at < resets_at;
    Some(Forecast {
        rate_permille_per_hour: rate,
        recent_rate_permille_per_hour: recent_rate(&rules, current, snapshots, resets_at, now),
        exhausts_at: Some(exhausts_at),
        before_reset,
    })
}

fn recent_rate(
    rules: &Rules,
    current: Reading,
    snapshots: &[Reading],
    resets_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<f64> {
    let lookback = now - rules.recent_lookback;
    let mut recent: Vec<&Reading> = snapshots
        .iter()
        .chain(std::iter::once(&current))
        .filter(|reading| same_window(reading, resets_at))
        .filter(|reading| reading.observed_at >= lookback)
        .collect();
    recent.sort_by_key(|reading| reading.observed_at);
    let first = recent.first()?;
    let last = recent.last()?;
    let span = last.observed_at - first.observed_at;
    if span < rules.min_recent_span {
        return None;
    }
    let delta = i32::from(last.used_permille) - i32::from(first.used_permille);
    if delta < 0 {
        return None;
    }
    Some(f64::from(delta) / (span.num_seconds() as f64 / 3600.0))
}

/// Прогноз для строки `provider_limits` по истории `limit_snapshots`.
pub async fn load(
    conn: &mut PgConnection,
    provider: &str,
    window: Window,
    current: Reading,
    now: DateTime<Utc>,
) -> Result<Option<Forecast>, sqlx::Error> {
    let Some(resets_at) = current.resets_at else {
        return Ok(None);
    };
    let rows = sqlx::query(
        "select used_permille,resets_at,observed_at from limit_snapshots
          where provider=$1 and window_kind=$2 and resets_at between $3 and $4
          order by observed_at,id",
    )
    .bind(provider)
    .bind(window.as_str())
    .bind(resets_at - RESET_TOLERANCE)
    .bind(resets_at + RESET_TOLERANCE)
    .fetch_all(&mut *conn)
    .await?;
    let snapshots = rows
        .into_iter()
        .map(|row| {
            Ok(Reading {
                used_permille: u16::try_from(row.try_get::<i32, _>("used_permille")?)
                    .map_err(|_| sqlx::Error::Protocol("invalid stored limit".into()))?,
                resets_at: row.try_get("resets_at")?,
                observed_at: row.try_get("observed_at")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(forecast(window, current, &snapshots, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap() + TimeDelta::minutes(minutes)
    }

    fn reset(hours: i64) -> DateTime<Utc> {
        at(hours * 60)
    }

    fn reading(used: u16, resets_at: Option<DateTime<Utc>>, minutes_ago: i64) -> Reading {
        Reading {
            used_permille: used,
            resets_at,
            observed_at: at(-minutes_ago),
        }
    }

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn fresh_window_has_no_forecast() {
        // Окно только началось: прошло меньше 30 минут.
        let current = reading(100, Some(reset(5)), 0);
        assert_eq!(forecast(Window::FiveHour, current, &[], at(0)), None);
    }

    #[test]
    fn too_little_data_has_no_forecast() {
        // Неделя: прошло меньше 12 часов и расход ниже 20 промилле.
        let current = reading(500, Some(reset(168)), 0);
        assert_eq!(forecast(Window::Weekly, current, &[], at(0)), None);
        let current = reading(10, Some(reset(3)), 120);
        assert_eq!(forecast(Window::FiveHour, current, &[], at(0)), None);
        // Месяц: прошло меньше двух дней.
        let current = reading(500, Some(reset(29 * 24)), 24 * 60);
        assert_eq!(forecast(Window::Monthly, current, &[], at(0)), None);
    }

    #[test]
    fn average_pace_comes_from_the_window_start() {
        let current = reading(200, Some(reset(3)), 0);
        let snapshots = [
            reading(100, Some(reset(3)), 120),
            reading(150, Some(reset(3)), 60),
        ];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        close(got.rate_permille_per_hour, 100.0);
        // Недавний темп: 50 промилле за час.
        close(got.recent_rate_permille_per_hour.unwrap(), 50.0);
        close(got.pace_permille_per_hour(), 100.0);
        // 800 промилле при 100/ч — через 8 часов, сброс через 3 часа.
        assert_eq!(got.exhausts_at, Some(at(8 * 60)));
        assert!(!got.before_reset);
    }

    #[test]
    fn jittered_reset_time_still_finds_the_window_start() {
        // Снимок на старте окна отдал время сброса на 90 секунд позже.
        let current = reading(200, Some(reset(1)), 0);
        let snapshots = [
            reading(100, Some(reset(1) + TimeDelta::seconds(90)), 180),
            reading(150, Some(reset(1)), 60),
        ];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        // Начало окна — 3 часа назад, а не номинальные 4 часа.
        close(got.rate_permille_per_hour, 200.0 / 3.0);
    }

    #[test]
    fn a_reset_inside_the_lookback_does_not_mix_windows() {
        let current = reading(200, Some(reset(4)), 0);
        let old = reading(500, Some(reset(4) - TimeDelta::hours(6)), 300);
        let snapshots = [
            old,
            reading(100, Some(reset(4)), 120),
            reading(150, Some(reset(4)), 60),
        ];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        // Считается только новое окно: 2 часа от 100 до 200.
        close(got.rate_permille_per_hour, 100.0);
    }

    #[test]
    fn flat_usage_keeps_a_zero_recent_pace() {
        let current = reading(200, Some(reset(3)), 0);
        let snapshots = [
            reading(200, Some(reset(3)), 120),
            reading(200, Some(reset(3)), 60),
            reading(200, Some(reset(3)), 30),
        ];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        close(got.rate_permille_per_hour, 100.0);
        assert_eq!(got.recent_rate_permille_per_hour, Some(0.0));
    }

    #[test]
    fn before_reset_is_true_when_the_pace_runs_out_first() {
        let current = reading(900, Some(reset(2)), 0);
        let snapshots = [reading(100, Some(reset(2)), 180)];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        // 100 промилле при 300/ч — через 20 минут, сброс через 2 часа.
        close(got.rate_permille_per_hour, 300.0);
        assert_eq!(got.exhausts_at, Some(at(20)));
        assert!(got.before_reset);
    }

    #[test]
    fn no_reset_time_means_no_forecast() {
        let current = reading(900, None, 0);
        assert_eq!(forecast(Window::Weekly, current, &[], at(0)), None);
    }

    #[test]
    fn recent_pace_needs_a_minimum_span() {
        let current = reading(500, Some(reset(144)), 0);
        let snapshots = [
            reading(300, Some(reset(144)), 24 * 60),
            reading(400, Some(reset(144)), 30),
        ];
        let got = forecast(Window::Weekly, current, &snapshots, at(0)).unwrap();
        assert_eq!(got.recent_rate_permille_per_hour, None);
    }

    #[test]
    fn a_negative_delta_gives_no_recent_pace() {
        // Сброс совпал по времени, но расход упал: темпу верить нельзя.
        let current = reading(300, Some(reset(3)), 0);
        let snapshots = [reading(400, Some(reset(3)), 60)];
        let got = forecast(Window::FiveHour, current, &snapshots, at(0)).unwrap();
        assert_eq!(got.recent_rate_permille_per_hour, None);
    }
}
