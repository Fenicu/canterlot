//! Ставки часа машины из каталога SKU и стоимость окна по секундам.
//!
//! Цены приходят десятичными строками в рублях. Счёт идёт в целых числах: рубли переводятся
//! в единицы по 10⁻¹² рубля, `f64` не используется нигде. Каждая ставка округляется вверх до
//! целой копейки: оценка не занижает расход.

use std::fmt;

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::ListError;

/// Единиц счёта в рубле: двенадцать знаков после запятой.
const SCALE: i128 = 1_000_000_000_000;
const SCALE_DIGITS: usize = 12;
/// Единиц счёта в копейке.
const PER_KOPECK: i128 = SCALE / 100;
/// Микросекунд в часе.
const HOUR_MICROS: i128 = 3_600_000_000;

/// Номера SKU в каталоге Яндекса: общие для всех аккаунтов, сверены живьём 24.11.2026.
/// Сопоставление идёт по номеру, а не по имени: в именах стоят неразрывные пробелы.
pub const SKU_REGULAR_VCPU: &str = "dn2k3vqlk9snp1jv351u";
pub const SKU_REGULAR_RAM: &str = "dn2ilq72mjc3bej6j74p";
pub const SKU_PREEMPTIBLE_VCPU: &str = "dn2e2fphfupugm21k4hv";
pub const SKU_PREEMPTIBLE_RAM: &str = "dn26ur5frjbgdek2a0g5";
pub const SKU_SSD: &str = "dn27ajm6m8mnfcshbi61";
pub const SKU_PUBLIC_IP: &str = "dn229q5mnmp58t58tfel";

/// Единицы тарификации, как их называет каталог.
pub(super) const UNIT_CORE_HOUR: &str = "core*hour";
pub(super) const UNIT_GB_HOUR: &str = "gbyte*hour";
pub(super) const UNIT_IP_HOUR: &str = "fip*hour";

/// Все номера, которые клиент ищет в каталоге.
pub const SKU_IDS: [&str; 6] = [
    SKU_REGULAR_VCPU,
    SKU_REGULAR_RAM,
    SKU_PREEMPTIBLE_VCPU,
    SKU_PREEMPTIBLE_RAM,
    SKU_SSD,
    SKU_PUBLIC_IP,
];

/// Цена машины в целых копейках за час, двумя ставками: машина с адресом платится, пока
/// жива машина, диск — пока жив диск. Каждая ставка округлена вверх.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    /// `cores×vCPU + memory_gb×RAM + публичный адрес`.
    pub compute_kopecks_per_hour: i64,
    /// `disk_gb×SSD`.
    pub disk_kopecks_per_hour: i64,
}

impl Price {
    /// Обе ставки вместе. `None`, если сумма не помещается в `i64`.
    pub fn total_kopecks_per_hour(&self) -> Option<i64> {
        self.compute_kopecks_per_hour
            .checked_add(self.disk_kopecks_per_hour)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceError {
    /// Каталог SKU не прочитан целиком.
    Catalog(ListError),
    /// В каталоге нет SKU с таким номером.
    MissingSku(String),
    /// SKU есть, но цена из него не читается однозначно.
    MalformedSku { id: String, why: &'static str },
    /// Результат не помещается в `i64` копеек.
    Overflow,
    /// Отрицательная цена часа.
    Invalid,
}

impl PriceError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Catalog(error) if error.retryable())
    }
}

impl fmt::Display for PriceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => write!(f, "cannot read the SKU catalog: {error}"),
            Self::MissingSku(id) => write!(f, "the SKU catalog has no `{id}`"),
            Self::MalformedSku { id, why } => write!(f, "SKU `{id}` has no usable price: {why}"),
            Self::Overflow => f.write_str("price overflows i64 kopecks"),
            Self::Invalid => f.write_str("hourly price is negative"),
        }
    }
}

impl std::error::Error for PriceError {}

/// Десятичная строка рублей в единицы счёта. Знаки дальше двенадцатого округляются вверх.
/// Знак, экспонента, пробелы и пустая строка не принимаются.
pub(super) fn parse_rubles(text: &str) -> Option<i128> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut value: i128 = 0;
    for digit in whole.bytes() {
        value = value
            .checked_mul(10)?
            .checked_add(i128::from(digit - b'0'))?;
    }
    value = value.checked_mul(SCALE)?;
    let mut unit = SCALE;
    for digit in fraction.bytes().take(SCALE_DIGITS) {
        unit /= 10;
        value = value.checked_add(i128::from(digit - b'0') * unit)?;
    }
    if fraction
        .bytes()
        .skip(SCALE_DIGITS)
        .any(|digit| digit != b'0')
    {
        value = value.checked_add(1)?;
    }
    Some(value)
}

/// Цена единицы из одного SKU каталога.
///
/// Берётся версия `STREET_PRICE`, действующая на `now`: с самым поздним `effectiveTime` не
/// позже `now`. В ней должна быть ровно одна ставка, в рублях и с нулевого количества.
/// Единица тарификации должна быть ожидаемой. Всё остальное — отказ: цена не угадывается.
pub(super) fn sku_unit_price(
    sku: &Value,
    unit: &str,
    now: DateTime<Utc>,
) -> Result<i128, &'static str> {
    if sku["pricingUnit"].as_str() != Some(unit) {
        return Err("unexpected pricingUnit");
    }
    let versions = sku["pricingVersions"]
        .as_array()
        .ok_or("no pricingVersions")?;
    let mut current: Option<(DateTime<Utc>, &Value)> = None;
    for version in versions {
        if version["type"].as_str() != Some("STREET_PRICE") {
            continue;
        }
        let effective = version["effectiveTime"]
            .as_str()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .ok_or("pricing version has no readable effectiveTime")?
            .with_timezone(&Utc);
        if effective > now {
            continue;
        }
        match current {
            Some((best, _)) if effective == best => {
                return Err("two street prices take effect at the same time");
            }
            Some((best, _)) if effective < best => {}
            _ => current = Some((effective, version)),
        }
    }
    let (_, version) = current.ok_or("no street price in effect")?;
    let mut rates = version["pricingExpressions"]
        .as_array()
        .ok_or("no pricingExpressions")?
        .iter()
        .flat_map(|expression| expression["rates"].as_array().into_iter().flatten());
    let (Some(rate), None) = (rates.next(), rates.next()) else {
        return Err("the price in effect must have exactly one rate");
    };
    if rate["startPricingQuantity"].as_str() != Some("0") {
        return Err("the rate does not start at quantity 0");
    }
    if rate["currency"].as_str() != Some("RUB") {
        return Err("the rate is not in RUB");
    }
    rate["unitPrice"]
        .as_str()
        .and_then(parse_rubles)
        .ok_or("unitPrice is not a decimal string")
}

/// Цены единиц в единицах счёта за час.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnitPrices {
    pub vcpu: i128,
    pub ram_gb: i128,
    pub ssd_gb: i128,
    pub public_ip: i128,
}

/// Единицы счёта в целые копейки, вверх.
fn ceil_kopecks(total: i128) -> Result<i64, PriceError> {
    let kopecks = total
        .checked_add(PER_KOPECK - 1)
        .ok_or(PriceError::Overflow)?
        / PER_KOPECK;
    i64::try_from(kopecks).map_err(|_| PriceError::Overflow)
}

/// Две ставки: `cores×vcpu + memory_gb×ram + ip` и `disk_gb×ssd`, каждая вверх до целой
/// копейки.
pub(super) fn hourly(
    prices: &UnitPrices,
    cores: u32,
    memory_gb: u64,
    disk_gb: u64,
) -> Result<Price, PriceError> {
    let part = |count: i128, unit: i128| count.checked_mul(unit).ok_or(PriceError::Overflow);
    let compute = part(i128::from(cores), prices.vcpu)?
        .checked_add(part(i128::from(memory_gb), prices.ram_gb)?)
        .and_then(|sum| sum.checked_add(prices.public_ip))
        .ok_or(PriceError::Overflow)?;
    let price = Price {
        compute_kopecks_per_hour: ceil_kopecks(compute)?,
        disk_kopecks_per_hour: ceil_kopecks(part(i128::from(disk_gb), prices.ssd_gb)?)?,
    };
    price.total_kopecks_per_hour().ok_or(PriceError::Overflow)?;
    Ok(price)
}

/// Стоимость работы машины с `from` до `to` по секундам:
/// `ceil(hourly × elapsed_µs / 3_600_000_000)`. Минимума нет: ноль времени стоит ноль.
/// Если `to` раньше `from`, время считается нулевым.
pub fn cost_kopecks(
    hourly_kopecks: i64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<i64, PriceError> {
    if hourly_kopecks < 0 {
        return Err(PriceError::Invalid);
    }
    let elapsed = i128::from(to.timestamp_micros()) - i128::from(from.timestamp_micros());
    let elapsed = elapsed.max(0);
    let product = i128::from(hourly_kopecks)
        .checked_mul(elapsed)
        .ok_or(PriceError::Overflow)?;
    let kopecks = product
        .checked_add(HOUR_MICROS - 1)
        .ok_or(PriceError::Overflow)?
        / HOUR_MICROS;
    i64::try_from(kopecks).map_err(|_| PriceError::Overflow)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _};
    use serde_json::json;

    use super::*;

    fn at(micros: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 24, 10, 0, 0).unwrap() + Duration::microseconds(micros)
    }

    #[test]
    fn rubles_parse_exactly() {
        assert_eq!(parse_rubles("0"), Some(0));
        assert_eq!(parse_rubles("1"), Some(SCALE));
        assert_eq!(parse_rubles("0.34"), Some(340_000_000_000));
        assert_eq!(parse_rubles("0.26352"), Some(263_520_000_000));
        assert_eq!(parse_rubles("0.0199"), Some(19_900_000_000));
        assert_eq!(parse_rubles("12.5"), Some(12_500_000_000_000));
        assert_eq!(parse_rubles(".5"), Some(500_000_000_000));
        assert_eq!(parse_rubles("5."), Some(5 * SCALE));
        assert_eq!(parse_rubles("0.000000000001"), Some(1));
    }

    #[test]
    fn digits_past_the_scale_round_up() {
        assert_eq!(parse_rubles("0.0000000000001"), Some(1));
        assert_eq!(parse_rubles("0.0000000000010"), Some(1));
        assert_eq!(parse_rubles("0.0000000000011"), Some(2));
    }

    #[test]
    fn rubles_refuse_everything_that_is_not_a_plain_decimal() {
        for bad in [
            "", ".", "-1", "+1", "1e3", " 1", "1 ", "1,5", "0.3.4", "NaN", "0x10",
        ] {
            assert_eq!(parse_rubles(bad), None, "{bad:?}");
        }
        let huge = "9".repeat(60);
        assert_eq!(parse_rubles(&huge), None);
    }

    fn prices() -> UnitPrices {
        UnitPrices {
            vcpu: parse_rubles("0.34").unwrap(),
            ram_gb: parse_rubles("0.083").unwrap(),
            ssd_gb: parse_rubles("0.0199").unwrap(),
            public_ip: parse_rubles("0.26352").unwrap(),
        }
    }

    fn rates(cores: u32, memory_gb: u64, disk_gb: u64) -> (i64, i64) {
        let price = hourly(&prices(), cores, memory_gb, disk_gb).unwrap();
        (price.compute_kopecks_per_hour, price.disk_kopecks_per_hour)
    }

    #[test]
    fn hourly_rates_are_exact_and_each_rounds_up_to_a_kopeck() {
        // 32×0.34 + 64×0.083 + 0.26352 = 16.45552 ₽ → 1646; 80×0.0199 = 1.592 ₽ → 160.
        assert_eq!(rates(32, 64, 80), (1646, 160));
        // 2×0.34 + 2×0.083 + 0.26352 = 1.10952 ₽ → 111; 10×0.0199 = 0.199 ₽ → 20.
        assert_eq!(rates(2, 2, 10), (111, 20));
        assert_eq!(
            hourly(&prices(), 32, 64, 80)
                .unwrap()
                .total_kopecks_per_hour(),
            Some(1806)
        );
    }

    #[test]
    fn a_whole_kopeck_rate_is_not_rounded_further() {
        let round = UnitPrices {
            vcpu: parse_rubles("0.25").unwrap(),
            ram_gb: parse_rubles("0.05").unwrap(),
            ssd_gb: parse_rubles("0.02").unwrap(),
            public_ip: parse_rubles("0.10").unwrap(),
        };
        let price = hourly(&round, 2, 2, 100).unwrap();
        assert_eq!(price.compute_kopecks_per_hour, 70);
        assert_eq!(price.disk_kopecks_per_hour, 200);
        let hair_over = UnitPrices {
            public_ip: parse_rubles("0.100000000001").unwrap(),
            ssd_gb: parse_rubles("0.020000000001").unwrap(),
            ..round
        };
        let price = hourly(&hair_over, 2, 2, 100).unwrap();
        assert_eq!(price.compute_kopecks_per_hour, 71);
        assert_eq!(price.disk_kopecks_per_hour, 201);
        // Без диска ставка диска нулевая.
        assert_eq!(hourly(&round, 2, 2, 0).unwrap().disk_kopecks_per_hour, 0);
    }

    #[test]
    fn hourly_price_overflow_is_an_error() {
        let absurd = UnitPrices {
            vcpu: i128::MAX / 2,
            ram_gb: 0,
            ssd_gb: 0,
            public_ip: 0,
        };
        assert_eq!(hourly(&absurd, 4, 0, 0), Err(PriceError::Overflow));
        let too_many_kopecks = UnitPrices {
            vcpu: parse_rubles("99999999999999999").unwrap(),
            ram_gb: 0,
            ssd_gb: 0,
            public_ip: 0,
        };
        assert_eq!(
            hourly(&too_many_kopecks, 4, 0, 0),
            Err(PriceError::Overflow)
        );
        // Каждая ставка помещается, сумма — нет.
        let half = UnitPrices {
            vcpu: parse_rubles("50000000000000000").unwrap(),
            ram_gb: 0,
            ssd_gb: parse_rubles("50000000000000000").unwrap(),
            public_ip: 0,
        };
        assert_eq!(hourly(&half, 1, 0, 1), Err(PriceError::Overflow));
    }

    #[test]
    fn cost_boundaries() {
        let hour = 3_600_000_000;
        assert_eq!(cost_kopecks(1805, at(0), at(0)), Ok(0));
        assert_eq!(cost_kopecks(1805, at(0), at(1)), Ok(1));
        assert_eq!(cost_kopecks(1805, at(0), at(hour)), Ok(1805));
        assert_eq!(cost_kopecks(1805, at(0), at(hour + 1)), Ok(1806));
    }

    #[test]
    fn cost_is_per_second_without_a_minimum() {
        // 90 секунд из часа за 1805 копеек: 45.125 → 46.
        assert_eq!(cost_kopecks(1805, at(0), at(90_000_000)), Ok(46));
        assert_eq!(cost_kopecks(0, at(0), at(90_000_000)), Ok(0));
        // Конец раньше начала: времени не прошло.
        assert_eq!(cost_kopecks(1805, at(5), at(0)), Ok(0));
        assert_eq!(cost_kopecks(-1, at(0), at(1)), Err(PriceError::Invalid));
    }

    fn sku(versions: Value) -> Value {
        json!({
            "id": SKU_PREEMPTIBLE_VCPU,
            "name": "x",
            "pricingUnit": "core*hour",
            "pricingVersions": versions,
        })
    }

    fn rate(start: &str, price: &str, currency: &str) -> Value {
        json!({"startPricingQuantity": start, "unitPrice": price, "currency": currency})
    }

    fn version_with(kind: &str, effective: &str, rates: Vec<Value>) -> Value {
        json!({"type": kind, "effectiveTime": effective, "pricingExpressions": [{"rates": rates}]})
    }

    fn version(kind: &str, effective: &str, price: &str) -> Value {
        version_with(kind, effective, vec![rate("0", price, "RUB")])
    }

    fn unit_price(sku: &Value) -> Result<i128, &'static str> {
        sku_unit_price(sku, "core*hour", at(0))
    }

    #[test]
    fn the_street_price_in_effect_is_used_whatever_the_order() {
        let versions = vec![
            version("STREET_PRICE", "2025-12-31T21:00:00Z", "1.1529"),
            version("STREET_PRICE", "2026-05-01T00:00:00+03:00", "0.34"),
            version("STREET_PRICE", "2027-01-01T00:00:00Z", "9.99"),
            version("CONTRACT_PRICE", "2026-06-01T00:00:00Z", "0.01"),
        ];
        let expected = Ok(parse_rubles("0.34").unwrap());
        assert_eq!(unit_price(&sku(json!(versions))), expected);
        let reversed: Vec<Value> = versions.into_iter().rev().collect();
        assert_eq!(unit_price(&sku(json!(reversed))), expected);
    }

    #[test]
    fn a_version_taking_effect_exactly_now_is_in_effect() {
        let sku = sku(json!([
            version("STREET_PRICE", "2020-01-01T00:00:00Z", "1"),
            version("STREET_PRICE", "2026-11-24T10:00:00Z", "2"),
        ]));
        assert_eq!(unit_price(&sku), Ok(parse_rubles("2").unwrap()));
    }

    #[test]
    fn an_ambiguous_or_unreadable_sku_is_refused() {
        let old = "2020-01-01T00:00:00Z";
        let street = |rates: Vec<Value>| sku(json!([version_with("STREET_PRICE", old, rates)]));
        let refused = [
            json!({"id": "x", "pricingUnit": "core*hour"}),
            sku(json!([])),
            // Только будущая цена и только договорная цена.
            sku(json!([version(
                "STREET_PRICE",
                "2027-01-01T00:00:00Z",
                "1"
            )])),
            sku(json!([version("CONTRACT_PRICE", old, "1")])),
            sku(json!([{"type": "STREET_PRICE", "pricingExpressions": []}])),
            sku(
                json!([{"type": "STREET_PRICE", "effectiveTime": 1_700_000_000, "pricingExpressions": []}]),
            ),
            // Две версии с одного момента.
            sku(json!([
                version("STREET_PRICE", old, "1"),
                version("STREET_PRICE", old, "2")
            ])),
            // Ступени цены, не та валюта, не с нуля, не строка, нет ставок.
            street(vec![rate("0", "0", "RUB"), rate("10", "0.5", "RUB")]),
            street(vec![rate("0", "0.5", "USD")]),
            street(vec![rate("10", "0.5", "RUB")]),
            street(vec![
                json!({"startPricingQuantity": "0", "unitPrice": 0.34, "currency": "RUB"}),
            ]),
            street(vec![
                json!({"startPricingQuantity": "0", "unitPrice": "-1", "currency": "RUB"}),
            ]),
            street(vec![
                json!({"startPricingQuantity": "0", "unitPrice": "0.34"}),
            ]),
            street(vec![]),
        ];
        for sku in refused {
            assert!(unit_price(&sku).is_err(), "{sku}");
        }
        // Две ставки в двух выражениях — тоже две ставки.
        let split = sku(json!([{
            "type": "STREET_PRICE",
            "effectiveTime": old,
            "pricingExpressions": [
                {"rates": [rate("0", "1", "RUB")]},
                {"rates": [rate("0", "2", "RUB")]},
            ],
        }]));
        assert!(unit_price(&split).is_err());
    }

    #[test]
    fn a_sku_in_another_unit_is_refused() {
        let sku = sku(json!([version(
            "STREET_PRICE",
            "2020-01-01T00:00:00Z",
            "1"
        )]));
        assert!(unit_price(&sku).is_ok());
        assert!(sku_unit_price(&sku, "gbyte*hour", at(0)).is_err());
    }
}
