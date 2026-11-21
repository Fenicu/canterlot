//! Арифметика цен облачного сервера: только целые копейки, без дробей и без `f64`.
//!
//! Правило тарификации рабочее, до живой проверки: час стоит `месяц / 720` с округлением
//! вверх, окно оплачивается целыми часами, резерв берётся с запасом в один час. Все функции
//! чистые; переполнение даёт `PriceError::Overflow`, а не обрезанное число.

use std::fmt;

/// Часов в расчётном месяце: цена за час — месячная цена, делённая на это число.
pub const HOURS_PER_MONTH: i64 = 720;

/// Мегабайт в гигабайте для цен за ГБ: размеры в API называются в МБ.
const MB_PER_GB: i128 = 1024;

/// Цена единицы ресурса в копейках в месяц: за vCPU, за ГБ памяти, за ГБ диска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitPrices {
    pub cpu: i64,
    pub ram_gb: i64,
    pub disk_gb: i64,
}

/// Конфигурация сервера: размеры в тех же единицах, что в запросе создания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizing {
    pub cpu: u32,
    pub ram_mb: u64,
    pub disk_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceError {
    /// Результат не помещается в `i64` копеек.
    Overflow,
    /// Цена или размер отрицательны или не конечны: считать нечего.
    Invalid,
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => f.write_str("price overflows i64 kopecks"),
            Self::Invalid => f.write_str("price or size is negative"),
        }
    }
}

impl std::error::Error for PriceError {}

/// Рубли из ответа API в копейки. Дробные рубли округляются до ближайшей копейки.
pub fn rubles_to_kopecks(rubles: f64) -> Result<i64, PriceError> {
    if !rubles.is_finite() || rubles < 0.0 {
        return Err(PriceError::Invalid);
    }
    let kopecks = (rubles * 100.0).round();
    // 2^63 точно представим в f64: всё, что не меньше, в i64 не поместится.
    if kopecks >= 9_223_372_036_854_775_808.0 {
        return Err(PriceError::Overflow);
    }
    Ok(kopecks as i64)
}

fn to_i64(value: i128) -> Result<i64, PriceError> {
    i64::try_from(value).map_err(|_| PriceError::Overflow)
}

/// Стоимость `size_mb` мегабайт при цене `per_gb` за гигабайт, округление вверх.
fn per_gb_ceil(size_mb: u64, per_gb: i64) -> i128 {
    let product = i128::from(size_mb) * i128::from(per_gb);
    (product + MB_PER_GB - 1) / MB_PER_GB
}

/// Цена конфигурации в копейках в месяц. Доли гигабайта округляются вверх: оценка не
/// занижает расход.
pub fn monthly_kopecks(prices: &UnitPrices, sizing: &Sizing) -> Result<i64, PriceError> {
    if prices.cpu < 0 || prices.ram_gb < 0 || prices.disk_gb < 0 {
        return Err(PriceError::Invalid);
    }
    // Каждое слагаемое помещается в i128 (u64 × i64), сумма трёх тоже.
    let total = i128::from(sizing.cpu) * i128::from(prices.cpu)
        + per_gb_ceil(sizing.ram_mb, prices.ram_gb)
        + per_gb_ceil(sizing.disk_mb, prices.disk_gb);
    to_i64(total)
}

/// Цена часа: `ceil(месяц / 720)`.
pub fn hourly_kopecks(monthly_kopecks: i64) -> Result<i64, PriceError> {
    if monthly_kopecks < 0 {
        return Err(PriceError::Invalid);
    }
    let hours = i128::from(HOURS_PER_MONTH);
    to_i64((i128::from(monthly_kopecks) + hours - 1) / hours)
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

fn hours_times(hourly_kopecks: i64, hours: u64) -> Result<i64, PriceError> {
    if hourly_kopecks < 0 {
        return Err(PriceError::Invalid);
    }
    to_i64(i128::from(hourly_kopecks) * i128::from(hours))
}

/// Резерв под окно: `час × (ceil(минуты / 60) + 1)`.
pub fn reservation_kopecks(hourly_kopecks: i64, total_minutes: u64) -> Result<i64, PriceError> {
    // `ceil_hours` не больше u64::MAX / 60 + 1, прибавка единицы не переполняет.
    hours_times(hourly_kopecks, ceil_hours(total_minutes) + 1)
}

/// Итоговая цена закончившегося окна: `час × ceil(минуты / 60)`, не меньше одного часа.
/// Минуты идут от вызова создания до подтверждённого удаления.
pub fn final_cost_kopecks(hourly_kopecks: i64, billed_minutes: u64) -> Result<i64, PriceError> {
    hours_times(hourly_kopecks, ceil_hours(billed_minutes).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kopecks(rubles: i64) -> i64 {
        rubles * 100
    }

    /// Проверка пресета из спеки: конфигуратор 133, 28 vCPU, 112 ГБ, 160 ГБ = 133 600 ₽.
    #[test]
    fn preset_high_160_costs_133600_rubles() {
        let prices = UnitPrices {
            cpu: kopecks(1800),
            ram_gb: kopecks(700),
            disk_gb: kopecks(30),
        };
        let sizing = Sizing {
            cpu: 28,
            ram_mb: 112 * 1024,
            disk_mb: 160 * 1024,
        };
        assert_eq!(monthly_kopecks(&prices, &sizing), Ok(kopecks(133_600)));
    }

    /// Дефолтный профиль 151: 32 vCPU, 64 ГБ, 80 ГБ = 20 480 ₽ в месяц.
    #[test]
    fn default_profile_costs_20480_rubles() {
        let prices = UnitPrices {
            cpu: kopecks(210),
            ram_gb: kopecks(200),
            disk_gb: kopecks(12),
        };
        let sizing = Sizing {
            cpu: 32,
            ram_mb: 64 * 1024,
            disk_mb: 80 * 1024,
        };
        let monthly = monthly_kopecks(&prices, &sizing).unwrap();
        assert_eq!(monthly, kopecks(20_480));
        // 20 480 / 720 = 28,44 ₽ → 2845 копеек вверх.
        assert_eq!(hourly_kopecks(monthly), Ok(2845));
    }

    #[test]
    fn fractional_gigabytes_round_up() {
        let prices = UnitPrices {
            cpu: 0,
            ram_gb: 1000,
            disk_gb: 1000,
        };
        // 1 МБ памяти = 1000/1024 копейки → 1; 1 МБ диска → 1.
        let sizing = Sizing {
            cpu: 0,
            ram_mb: 1,
            disk_mb: 1,
        };
        assert_eq!(monthly_kopecks(&prices, &sizing), Ok(2));
        let zero = Sizing {
            cpu: 0,
            ram_mb: 0,
            disk_mb: 0,
        };
        assert_eq!(monthly_kopecks(&prices, &zero), Ok(0));
    }

    #[test]
    fn monthly_overflow_is_an_error() {
        let prices = UnitPrices {
            cpu: i64::MAX,
            ram_gb: 0,
            disk_gb: 0,
        };
        let sizing = Sizing {
            cpu: 2,
            ram_mb: 0,
            disk_mb: 0,
        };
        assert_eq!(monthly_kopecks(&prices, &sizing), Err(PriceError::Overflow));
        let huge = Sizing {
            cpu: 0,
            ram_mb: u64::MAX,
            disk_mb: 0,
        };
        let prices = UnitPrices {
            cpu: 0,
            ram_gb: i64::MAX,
            disk_gb: 0,
        };
        assert_eq!(monthly_kopecks(&prices, &huge), Err(PriceError::Overflow));
    }

    #[test]
    fn negative_unit_prices_are_invalid() {
        let prices = UnitPrices {
            cpu: -1,
            ram_gb: 0,
            disk_gb: 0,
        };
        let sizing = Sizing {
            cpu: 1,
            ram_mb: 0,
            disk_mb: 0,
        };
        assert_eq!(monthly_kopecks(&prices, &sizing), Err(PriceError::Invalid));
    }

    #[test]
    fn hourly_rounds_up_at_boundaries() {
        assert_eq!(hourly_kopecks(0), Ok(0));
        assert_eq!(hourly_kopecks(1), Ok(1));
        assert_eq!(hourly_kopecks(720), Ok(1));
        assert_eq!(hourly_kopecks(721), Ok(2));
        assert_eq!(hourly_kopecks(1440), Ok(2));
        assert_eq!(hourly_kopecks(1441), Ok(3));
        assert_eq!(hourly_kopecks(i64::MAX), Ok(i64::MAX / 720 + 1));
        assert_eq!(hourly_kopecks(-1), Err(PriceError::Invalid));
    }

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
        // 2 ч + 30 + 30 мин = 3 ч, плюс час запаса = 4 часа по 2845.
        let total = total_minutes(120, 30, 30).unwrap();
        assert_eq!(reservation_kopecks(2845, total), Ok(2845 * 4));
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

    #[test]
    fn final_cost_is_at_least_one_hour() {
        assert_eq!(final_cost_kopecks(2845, 0), Ok(2845));
        assert_eq!(final_cost_kopecks(2845, 1), Ok(2845));
        assert_eq!(final_cost_kopecks(2845, 60), Ok(2845));
        assert_eq!(final_cost_kopecks(2845, 61), Ok(2845 * 2));
        assert_eq!(final_cost_kopecks(2845, 120), Ok(2845 * 2));
        assert_eq!(final_cost_kopecks(2845, 121), Ok(2845 * 3));
        assert_eq!(final_cost_kopecks(0, 500), Ok(0));
    }

    #[test]
    fn final_cost_guards_overflow() {
        assert_eq!(final_cost_kopecks(i64::MAX, 61), Err(PriceError::Overflow));
        assert_eq!(final_cost_kopecks(-5, 61), Err(PriceError::Invalid));
    }

    #[test]
    fn final_cost_never_exceeds_the_reservation_for_the_same_window() {
        let hourly = 2845;
        let total = total_minutes(120, 30, 30).unwrap();
        let reserved = reservation_kopecks(hourly, total).unwrap();
        for minutes in [0, 1, 59, 60, 61, 179, 180] {
            assert!(final_cost_kopecks(hourly, minutes).unwrap() <= reserved);
        }
    }

    #[test]
    fn rubles_convert_to_kopecks() {
        assert_eq!(rubles_to_kopecks(0.0), Ok(0));
        assert_eq!(rubles_to_kopecks(210.0), Ok(21_000));
        assert_eq!(rubles_to_kopecks(12.5), Ok(1250));
        // 0,1 + 0,2 в двоичной дроби не ровно 0,3: округление до копейки.
        assert_eq!(rubles_to_kopecks(0.1 + 0.2), Ok(30));
        assert_eq!(rubles_to_kopecks(-1.0), Err(PriceError::Invalid));
        assert_eq!(rubles_to_kopecks(f64::NAN), Err(PriceError::Invalid));
        assert_eq!(rubles_to_kopecks(f64::INFINITY), Err(PriceError::Invalid));
        assert_eq!(rubles_to_kopecks(1e300), Err(PriceError::Overflow));
    }
}
