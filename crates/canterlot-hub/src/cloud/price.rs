//! Деньги облачного окна: резерв и расход, только целые копейки, без дробей и без `f64`.
//!
//! Ставки часа приходят от провайдера двумя числами (`yandex::Price`): машина с публичным
//! адресом и диск. Резерв берётся целыми часами с запасом в один час по сумме ставок. Расход
//! считается по секундам и по каждой ставке отдельно: машина платится, пока она есть в
//! каталоге, диск — пока есть диск. Оценка осторожная и расход не занижает: остановленная
//! машина считается по полной ставке до подтверждённого исчезновения.
//!
//! Все функции чистые; переполнение даёт `PriceError::Overflow`, а не обрезанное число.

use std::fmt;

use chrono::{DateTime, Utc};

use super::yandex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceError {
    /// Результат не помещается в `i64` копеек.
    Overflow,
    /// Цена отрицательна: считать нечего.
    Invalid,
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => f.write_str("price overflows i64 kopecks"),
            Self::Invalid => f.write_str("price is negative"),
        }
    }
}

impl std::error::Error for PriceError {}

fn to_i64(value: i128) -> Result<i64, PriceError> {
    i64::try_from(value).map_err(|_| PriceError::Overflow)
}

/// Целые оплачиваемые часы для заданных минут: `ceil(minutes / 60)`.
pub fn ceil_hours(minutes: u64) -> u64 {
    minutes / 60 + u64::from(!minutes.is_multiple_of(60))
}

/// Сумма минут резерва: жёсткий предел, срок на доработку, запас на снос.
pub fn total_minutes(
    hard_cap_minutes: u64,
    drain_grace_minutes: u64,
    teardown_margin_minutes: u64,
) -> Result<u64, PriceError> {
    hard_cap_minutes
        .checked_add(drain_grace_minutes)
        .and_then(|sum| sum.checked_add(teardown_margin_minutes))
        .ok_or(PriceError::Overflow)
}

/// Резерв под окно: `час × (ceil(минуты / 60) + 1)`, где час — сумма обеих ставок.
pub fn reservation_kopecks(hourly_kopecks: i64, total_minutes: u64) -> Result<i64, PriceError> {
    if hourly_kopecks < 0 {
        return Err(PriceError::Invalid);
    }
    // `ceil_hours` не больше u64::MAX / 60 + 1, прибавка единицы не переполняет.
    let hours = ceil_hours(total_minutes) + 1;
    to_i64(i128::from(hourly_kopecks) * i128::from(hours))
}

fn part(hourly_kopecks: i64, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<i64, PriceError> {
    yandex::cost_kopecks(hourly_kopecks, from, to).map_err(|error| match error {
        yandex::PriceError::Invalid => PriceError::Invalid,
        _ => PriceError::Overflow,
    })
}

/// Расход окна из двух частей, каждая по секундам и без минимума:
/// - машина с адресом: `ceil(compute × (compute_end − called) / час)`;
/// - диск: `ceil(disk × (disk_end − called) / час)`.
///
/// `called` — метка вызова создания. Концы — подтверждённое исчезновение машины и диска, а
/// у ещё живых — «сейчас».
pub fn ledger_kopecks(
    compute_kopecks_per_hour: i64,
    disk_kopecks_per_hour: i64,
    called: DateTime<Utc>,
    compute_end: DateTime<Utc>,
    disk_end: DateTime<Utc>,
) -> Result<i64, PriceError> {
    part(compute_kopecks_per_hour, called, compute_end)?
        .checked_add(part(disk_kopecks_per_hour, called, disk_end)?)
        .ok_or(PriceError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone as _};

    #[test]
    fn ceil_hours_boundaries() {
        assert_eq!(ceil_hours(0), 0);
        assert_eq!(ceil_hours(1), 1);
        assert_eq!(ceil_hours(59), 1);
        assert_eq!(ceil_hours(60), 1);
        assert_eq!(ceil_hours(61), 2);
        assert_eq!(ceil_hours(120), 2);
        assert_eq!(ceil_hours(u64::MAX), u64::MAX / 60 + 1);
    }

    #[test]
    fn total_minutes_sums_and_guards_overflow() {
        assert_eq!(total_minutes(120, 30, 30), Ok(180));
        assert_eq!(total_minutes(0, 0, 0), Ok(0));
        assert_eq!(total_minutes(u64::MAX, 1, 0), Err(PriceError::Overflow));
    }

    #[test]
    fn reservation_adds_one_hour_to_the_rounded_up_window() {
        // 2 ч + 30 + 30 мин = 3 ч, плюс час запаса = 4 часа по 1806.
        let total = total_minutes(120, 30, 30).unwrap();
        assert_eq!(reservation_kopecks(1806, total), Ok(1806 * 4));
        // Ровно час: ceil = 1, плюс 1 = 2 часа.
        assert_eq!(reservation_kopecks(100, 60), Ok(200));
        // Час и минута: ceil = 2, плюс 1 = 3 часа.
        assert_eq!(reservation_kopecks(100, 61), Ok(300));
        // Ноль минут: всё равно час запаса.
        assert_eq!(reservation_kopecks(100, 0), Ok(100));
        assert_eq!(reservation_kopecks(0, 600), Ok(0));
    }

    #[test]
    fn reservation_guards_overflow_and_negative_price() {
        assert_eq!(reservation_kopecks(i64::MAX, 60), Err(PriceError::Overflow));
        assert_eq!(reservation_kopecks(-1, 60), Err(PriceError::Invalid));
        // Часов больше, чем помещается в i64.
        assert_eq!(
            reservation_kopecks(100, u64::MAX),
            Err(PriceError::Overflow)
        );
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, 10, 0, 0).unwrap() + TimeDelta::seconds(seconds)
    }

    #[test]
    fn the_ledger_charges_each_part_until_its_own_end() {
        // Машина 1646 коп/ч жила 30 минут, диск 160 коп/ч — 45 минут: 823 + 120.
        assert_eq!(
            ledger_kopecks(1646, 160, at(0), at(1800), at(2700)),
            Ok(943)
        );
        // Диск пропал вместе с машиной: обе части за одно время.
        assert_eq!(
            ledger_kopecks(1646, 160, at(0), at(3600), at(3600)),
            Ok(1806)
        );
    }

    #[test]
    fn the_ledger_is_per_second_without_a_minimum_and_rounds_each_part_up() {
        assert_eq!(ledger_kopecks(1646, 160, at(0), at(0), at(0)), Ok(0));
        // Одна секунда: 1646/3600 = 0.457 → 1, 160/3600 = 0.044 → 1.
        assert_eq!(ledger_kopecks(1646, 160, at(0), at(1), at(1)), Ok(2));
        // 90 секунд: 41.15 → 42 и 4 ровно.
        assert_eq!(ledger_kopecks(1646, 160, at(0), at(90), at(90)), Ok(46));
        // Конец раньше начала: времени не прошло.
        assert_eq!(ledger_kopecks(1646, 160, at(10), at(0), at(0)), Ok(0));
    }

    #[test]
    fn the_ledger_never_exceeds_the_reservation_inside_the_reserved_time() {
        for (cap, drain, teardown) in [(1u64, 0u64, 0u64), (60, 30, 30), (61, 0, 0), (240, 30, 30)]
        {
            let total = total_minutes(cap, drain, teardown).unwrap();
            let reserved = reservation_kopecks(1806, total).unwrap();
            let end = at(i64::try_from(total).unwrap() * 60);
            assert!(ledger_kopecks(1646, 160, at(0), end, end).unwrap() <= reserved);
        }
    }

    #[test]
    fn the_ledger_guards_negative_rates_and_overflow() {
        assert_eq!(
            ledger_kopecks(-1, 0, at(0), at(1), at(1)),
            Err(PriceError::Invalid)
        );
        assert_eq!(
            ledger_kopecks(0, -1, at(0), at(1), at(1)),
            Err(PriceError::Invalid)
        );
        let year = at(365 * 24 * 3600);
        assert_eq!(
            ledger_kopecks(i64::MAX, 0, at(0), year, year),
            Err(PriceError::Overflow)
        );
        assert_eq!(
            ledger_kopecks(i64::MAX / 2, i64::MAX / 2, at(0), at(7200), at(7200)),
            Err(PriceError::Overflow)
        );
    }
}
