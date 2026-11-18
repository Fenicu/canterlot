//! Одный генератор моментов для планировщика и предпросмотра редактора.
//!
//! Выражение — пять полей (минута, час, день месяца, месяц, день недели) без
//! секунд. Моменты считаются в часовом поясе владельца: несуществующий локальный
//! день (весенний сдвиг) пропускается, повторная локальная минута (осенний сдвиг)
//! fires once at the earlier UTC instant.

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::str::FromStr;

/// Минимальный интервал между соседними срабатываниями.
pub const MIN_INTERVAL: TimeDelta = TimeDelta::minutes(5);
/// Горизонт проверки интервала: неделя.
pub const INTERVAL_HORIZON: TimeDelta = TimeDelta::days(7);
/// Часовой пояс по умолчанию, если у владельца нет настройки.
pub const FALLBACK_TZ: Tz = chrono_tz::Europe::Moscow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronError {
    InvalidExpression,
    InvalidTimezone,
    TooFrequent,
}

impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidExpression => "invalid_cron",
            Self::InvalidTimezone => "invalid_timezone",
            Self::TooFrequent => "too_frequent",
        })
    }
}
impl std::error::Error for CronError {}

/// Разобранное выражение. Хранит cron-расписание с нулевой секундой.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    schedule: Schedule,
}

fn parse_tz(name: &str) -> Result<Tz, CronError> {
    if name.trim().is_empty() {
        return Ok(FALLBACK_TZ);
    }
    name.parse::<Tz>().map_err(|_| CronError::InvalidTimezone)
}

impl CronSchedule {
    pub fn parse(expression: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expression.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronError::InvalidExpression);
        }
        let expanded = format!("0 {}", expression.trim());
        let schedule = Schedule::from_str(&expanded).map_err(|_| CronError::InvalidExpression)?;
        Ok(Self { schedule })
    }

    /// Ближайшее срабатывание строго после `after`, в UTC.
    pub fn next_after(&self, after: DateTime<Utc>, timezone: Tz) -> Option<DateTime<Utc>> {
        let local = after.with_timezone(&timezone).naive_local();
        // Считаем моменты в «фальшивом UTC», поля которого равны локальным:
        // cron тогда перебирает именно локальные минуты, а перевод в зонуbelow
        // явно обрабатывает дыры и повторы DST.
        let wall: DateTime<Utc> = DateTime::from_naive_utc_and_offset(local, Utc);
        // Повторный час: первое и второе вхождение делят одну «фальшивую»
        // минуту. Если стена уже прошла её, `after(&wall)` пропустит оба.
        // Начинаем на 2 часа раньше и отбрасываем прошлое по реальному `after`.
        let start = wall - TimeDelta::hours(2);
        let mut iter = self.schedule.after(&start);
        for _ in 0..366 * 24 * 60 {
            let candidate = iter.next()?;
            let naive = candidate.naive_utc();
            // Повторный час даёт два кандидата: ранний и поздний UTC.
            // Прошедшие моменты отбрасываем, иначе осенний повтор вернёт
            // уже случившийся первый час вместо будущего второго.
            let candidates: Vec<DateTime<Utc>> = match timezone.from_local_datetime(&naive) {
                chrono::LocalResult::Single(at) => vec![at.with_timezone(&Utc)],
                chrono::LocalResult::Ambiguous(earlier, later) => {
                    vec![earlier.with_timezone(&Utc), later.with_timezone(&Utc)]
                }
                chrono::LocalResult::None => continue,
            };
            for at in candidates {
                if at > after {
                    return Some(at);
                }
            }
        }
        None
    }

    /// Следующие `count` срабатываний строго после `after`.
    pub fn next_n_after(
        &self,
        after: DateTime<Utc>,
        timezone: Tz,
        count: usize,
    ) -> Vec<DateTime<Utc>> {
        let mut result = Vec::with_capacity(count);
        let mut cursor = after;
        for _ in 0..count {
            match self.next_after(cursor, timezone) {
                Some(at) => {
                    result.push(at);
                    cursor = at;
                }
                None => break,
            }
        }
        result
    }

    /// Проверка минимального интервала на реальных соседних моментах за неделю.
    pub fn validate_min_interval(
        &self,
        from: DateTime<Utc>,
        timezone: Tz,
    ) -> Result<(), CronError> {
        let limit = from + INTERVAL_HORIZON;
        let mut previous: Option<DateTime<Utc>> = None;
        let mut cursor = from;
        for _ in 0..7 * 24 * 60 {
            let Some(at) = self.next_after(cursor, timezone) else {
                break;
            };
            if at > limit {
                break;
            }
            if previous.is_some_and(|prev| at - prev < MIN_INTERVAL) {
                return Err(CronError::TooFrequent);
            }
            previous = Some(at);
            cursor = at;
        }
        Ok(())
    }
}

pub fn timezone(name: &str) -> Result<Tz, CronError> {
    parse_tz(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn parses_five_fields_and_rejects_others() {
        assert!(CronSchedule::parse("0 9 * * 1-5").is_ok());
        assert!(CronSchedule::parse("*/15 * * * *").is_ok());
        assert_eq!(
            CronSchedule::parse("0 0 9 * * *").unwrap_err(),
            CronError::InvalidExpression
        );
        assert_eq!(
            CronSchedule::parse("61 9 * * *").unwrap_err(),
            CronError::InvalidExpression
        );
    }

    #[test]
    fn next_fire_uses_the_owner_timezone() {
        let schedule = CronSchedule::parse("0 9 * * *").unwrap();
        let berlin = timezone("Europe/Berlin").unwrap();
        // 09:00 Berlin in winter is 08:00 UTC, in summer 07:00 UTC.
        assert_eq!(
            schedule.next_after(utc(2026, 1, 5, 0, 0), berlin),
            Some(utc(2026, 1, 5, 8, 0))
        );
        assert_eq!(
            schedule.next_after(utc(2026, 7, 5, 0, 0), berlin),
            Some(utc(2026, 7, 5, 7, 0))
        );
    }

    #[test]
    fn dst_gap_local_minute_is_skipped() {
        let schedule = CronSchedule::parse("30 2 * * *").unwrap();
        let berlin = timezone("Europe/Berlin").unwrap();
        // 02:30 on 2026-03-29 does not exist in Berlin; the next one is 2026-03-30.
        let next = schedule
            .next_after(utc(2026, 3, 28, 1, 30), berlin)
            .unwrap();
        assert_eq!(next, utc(2026, 3, 30, 0, 30));
    }

    #[test]
    fn repeated_local_minute_uses_the_earlier_utc_instant() {
        let schedule = CronSchedule::parse("30 2 * * *").unwrap();
        let berlin = timezone("Europe/Berlin").unwrap();
        // 02:30 on 2026-10-25 happens twice: 00:30Z and 01:30Z. Always the first.
        let next = schedule
            .next_after(utc(2026, 10, 24, 0, 30), berlin)
            .unwrap();
        assert_eq!(next, utc(2026, 10, 25, 0, 30));
        assert_eq!(next.hour(), 0);
    }

    #[test]
    fn repeated_hour_skips_past_candidate_after_first_occurrence() {
        let schedule = CronSchedule::parse("30 2 * * *").unwrap();
        let berlin = timezone("Europe/Berlin").unwrap();
        // После первого вхождения (00:30Z) следующее — второе (01:30Z), а не прошлое.
        assert_eq!(
            schedule.next_after(utc(2026, 10, 25, 0, 40), berlin),
            Some(utc(2026, 10, 25, 1, 30))
        );
        // После 01:10Z (местные 02:10 второго часа) кандидат 00:30Z в прошлом:
        // выбираем будущее 01:30Z.
        assert_eq!(
            schedule.next_after(utc(2026, 10, 25, 1, 10), berlin),
            Some(utc(2026, 10, 25, 1, 30))
        );
        // После второго вхождения — следующий день.
        assert_eq!(
            schedule.next_after(utc(2026, 10, 25, 1, 40), berlin),
            Some(utc(2026, 10, 26, 1, 30))
        );
    }

    #[test]
    fn minimum_interval_refuses_a_four_minute_gap_at_the_hour() {
        assert_eq!(
            CronSchedule::parse("*/7 * * * *")
                .unwrap()
                .validate_min_interval(utc(2026, 11, 18, 0, 0), chrono_tz::UTC)
                .unwrap_err(),
            CronError::TooFrequent
        );
        assert!(
            CronSchedule::parse("*/5 * * * *")
                .unwrap()
                .validate_min_interval(utc(2026, 11, 18, 0, 0), chrono_tz::UTC)
                .is_ok()
        );
    }

    #[test]
    fn preview_lists_distinct_future_fires() {
        let schedule = CronSchedule::parse("0 9 * * 1-5").unwrap();
        let fires = schedule.next_n_after(utc(2026, 11, 18, 0, 0), chrono_tz::UTC, 5);
        assert_eq!(fires.len(), 5);
        assert_eq!(fires[0], utc(2026, 11, 18, 9, 0));
        assert!(fires.windows(2).all(|w| w[1] > w[0]));
    }
}
