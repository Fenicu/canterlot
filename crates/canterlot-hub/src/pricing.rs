//! Цена хода по таблице: для моделей, чей CLI цену не называет (Codex).
//!
//! Таблица снята 2026-11-19 с публичного каталога https://models.dev/api.json: провайдеры
//! `openai` и `opencode-go` без моделей Claude (их цену называет сам CLI), без картинок,
//! звука и realtime. Берётся базовая ставка без надбавки за длинный контекст. Цены в
//! миллионных долях доллара за 1 млн токенов. Обновление цен — правка этого файла;
//! живой загрузки нет, а уже записанные ходы не пересчитываются.
//! Свои цены задаёт `[pricing."<модель>"]` в `hub.toml`, они сильнее таблицы.

use std::collections::BTreeMap;

use crate::config::PriceConfig;

/// Цена за 1 млн токенов в микродолларах. Нет цены кеша — берётся цена входа:
/// так оценка не занижает расход.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

/// Токены хода по одной модели: вход без кеша, выход вместе с рассуждениями.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

fn micro(usd: f64) -> u64 {
    (usd * 1_000_000.0).round() as u64
}

fn price_of_config(config: &PriceConfig) -> Price {
    Price {
        input: micro(config.input),
        output: micro(config.output),
        cache_read: config.cache_read.map(micro),
        cache_write: config.cache_write.map(micro),
    }
}

/// Провайдеры, с которых снята таблица: их префикс — псевдоним голого имени.
const TABLE_PROVIDERS: &[&str] = &["openai", "opencode-go"];

#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    overrides: BTreeMap<String, Price>,
}

impl PriceBook {
    pub fn with_overrides(overrides: &BTreeMap<String, PriceConfig>) -> Self {
        Self {
            overrides: overrides
                .iter()
                .map(|(model, price)| (model.clone(), price_of_config(price)))
                .collect(),
        }
    }

    /// Совпадение только по точному id или по явному псевдониму: голое имя и то же имя под
    /// провайдером таблицы (`openai/`, `opencode-go/`). Чужой провайдер — цена неизвестна.
    pub fn price_of(&self, model: &str) -> Option<Price> {
        let tail = model
            .split_once('/')
            .filter(|(provider, _)| TABLE_PROVIDERS.contains(provider))
            .map_or(model, |(_, bare)| bare);
        [model, tail]
            .into_iter()
            .find_map(|name| self.overrides.get(name).copied())
            .or_else(|| {
                [model, tail].into_iter().find_map(|name| {
                    TABLE.iter().find(|row| row.0 == name).map(
                        |&(_, input, output, cache_read, cache_write)| Price {
                            input,
                            output,
                            cache_read,
                            cache_write,
                        },
                    )
                })
            })
    }

    /// Цена хода в микродолларах; `None` — модели нет ни в таблице, ни в `hub.toml`.
    pub fn cost_micro_usd(&self, model: &str, tokens: Tokens) -> Option<u64> {
        let price = self.price_of(model)?;
        let part = |count: u64, rate: u64| u128::from(count) * u128::from(rate);
        let total = part(tokens.input, price.input)
            + part(tokens.output, price.output)
            + part(tokens.cache_read, price.cache_read.unwrap_or(price.input))
            + part(tokens.cache_write, price.cache_write.unwrap_or(price.input));
        // Округление к ближайшему микродоллару: на ход меньше доллара ошибка не заметна.
        Some(u64::try_from((total + 500_000) / 1_000_000).unwrap_or(u64::MAX))
    }
}

/// (модель, вход, выход, чтение кеша, запись кеша).
type Row = (&'static str, u64, u64, Option<u64>, Option<u64>);
const TABLE: &[Row] = &[
    ("gpt-3.5-turbo", 500000, 1500000, Some(0), None),
    ("gpt-4", 30000000, 60000000, None, None),
    ("gpt-4-turbo", 10000000, 30000000, None, None),
    ("gpt-4.1", 2000000, 8000000, Some(500000), None),
    ("gpt-4.1-mini", 400000, 1600000, Some(100000), None),
    ("gpt-4.1-nano", 100000, 400000, Some(25000), None),
    ("gpt-4o", 2500000, 10000000, Some(1250000), None),
    ("gpt-4o-2024-05-13", 5000000, 15000000, None, None),
    ("gpt-4o-2024-08-06", 2500000, 10000000, Some(1250000), None),
    ("gpt-4o-2024-11-20", 2500000, 10000000, Some(1250000), None),
    ("gpt-4o-mini", 150000, 600000, Some(75000), None),
    ("gpt-5", 1250000, 10000000, Some(125000), None),
    ("gpt-5-mini", 250000, 2000000, Some(25000), None),
    ("gpt-5-nano", 50000, 400000, Some(5000), None),
    ("gpt-5-pro", 15000000, 120000000, None, None),
    ("gpt-5.1", 1250000, 10000000, Some(125000), None),
    ("gpt-5.2", 1750000, 14000000, Some(175000), None),
    ("gpt-5.2-pro", 21000000, 168000000, None, None),
    ("gpt-5.3-codex", 1750000, 14000000, Some(175000), None),
    ("gpt-5.3-codex-spark", 1750000, 14000000, Some(175000), None),
    ("gpt-5.4", 2500000, 15000000, Some(250000), None),
    ("gpt-5.4-mini", 750000, 4500000, Some(75000), None),
    ("gpt-5.4-nano", 200000, 1250000, Some(20000), None),
    ("gpt-5.4-pro", 30000000, 180000000, None, None),
    ("gpt-5.5", 5000000, 30000000, Some(500000), None),
    ("gpt-5.5-pro", 30000000, 180000000, None, None),
    ("gpt-5.6", 4000000, 20000000, Some(400000), Some(5000000)),
    ("gpt-5.6-luna", 200000, 1200000, Some(20000), Some(250000)),
    (
        "gpt-5.6-sol",
        4000000,
        20000000,
        Some(400000),
        Some(5000000),
    ),
    (
        "gpt-5.6-terra",
        2000000,
        12000000,
        Some(200000),
        Some(2500000),
    ),
    (
        "gpt-6-astra",
        10000000,
        50000000,
        Some(1000000),
        Some(12500000),
    ),
    ("gpt-6-luna", 100000, 500000, Some(10000), Some(125000)),
    ("gpt-6-sol", 2000000, 10000000, Some(200000), Some(2500000)),
    (
        "gpt-6.1-sol",
        2000000,
        10000000,
        Some(100000),
        Some(2500000),
    ),
    (
        "gpt-6.1-terra",
        1000000,
        5000000,
        Some(50000),
        Some(1250000),
    ),
    (
        "gpt-6.2-astra",
        8000000,
        40000000,
        Some(400000),
        Some(10000000),
    ),
    (
        "gpt-6.2-sol",
        2000000,
        10000000,
        Some(100000),
        Some(2500000),
    ),
    ("o1", 15000000, 60000000, Some(7500000), None),
    ("o1-pro", 150000000, 600000000, None, None),
    ("o3", 2000000, 8000000, Some(500000), None),
    ("o3-mini", 1100000, 4400000, Some(550000), None),
    ("o3-pro", 20000000, 80000000, None, None),
    ("o4-mini", 1100000, 4400000, Some(275000), None),
    ("opencode-go/beam-free", 0, 0, Some(0), None),
    ("opencode-go/cat-jam-free", 0, 0, Some(0), None),
    ("opencode-go/crouton-alpha-free", 0, 0, Some(0), None),
    (
        "opencode-go/deepseek-v4-pro",
        660000,
        1980000,
        Some(22000),
        None,
    ),
    (
        "opencode-go/deepseek-v4.1-flash",
        150000,
        600000,
        Some(3000),
        None,
    ),
    (
        "opencode-go/gemini-4-argon",
        2000000,
        10000000,
        Some(100000),
        None,
    ),
    (
        "opencode-go/gemini-4-argon-contributor",
        4000000,
        20000000,
        Some(200000),
        None,
    ),
    ("opencode-go/glm-5.2", 1400000, 4400000, Some(260000), None),
    ("opencode-go/glm-5.3", 1400000, 4400000, Some(260000), None),
    (
        "opencode-go/glm-5.3-flash",
        150000,
        500000,
        Some(30000),
        None,
    ),
    ("opencode-go/grok-4.7", 2000000, 6000000, Some(500000), None),
    ("opencode-go/hy3", 140000, 580000, Some(35000), None),
    ("opencode-go/hy4", 834000, 2501000, Some(42000), None),
    ("opencode-go/hy4-flash-free", 0, 0, Some(0), None),
    (
        "opencode-go/hy4-preview",
        834000,
        2501000,
        Some(42000),
        None,
    ),
    ("opencode-go/iceman-contributor-free", 0, 0, Some(0), None),
    (
        "opencode-go/kimi-k2.7-code",
        950000,
        4000000,
        Some(190000),
        None,
    ),
    (
        "opencode-go/kimi-k3",
        3000000,
        15000000,
        Some(300000),
        Some(3000000),
    ),
    (
        "opencode-go/kimi-k3.1-pro",
        2000000,
        10000000,
        Some(200000),
        Some(2000000),
    ),
    ("opencode-go/longcat-2.0", 300000, 1200000, Some(6000), None),
    ("opencode-go/longcat-2.5-free", 0, 0, Some(0), None),
    ("opencode-go/longcat-2.5-preview-free", 0, 0, Some(0), None),
    ("opencode-go/mimo-v2.5", 140000, 280000, Some(2800), None),
    (
        "opencode-go/mimo-v2.5-pro",
        435000,
        870000,
        Some(3625),
        None,
    ),
    (
        "opencode-go/mimo-v2.6-flash",
        140000,
        280000,
        Some(2800),
        None,
    ),
    (
        "opencode-go/mimo-v2.6-pro",
        435000,
        870000,
        Some(3625),
        None,
    ),
    (
        "opencode-go/minimax-m2.7",
        300000,
        1200000,
        Some(60000),
        Some(375000),
    ),
    ("opencode-go/minimax-m3", 300000, 1200000, Some(60000), None),
    (
        "opencode-go/minimax-m3.1-flash",
        150000,
        600000,
        Some(15000),
        None,
    ),
    ("opencode-go/minimax-m3.1-flash-free", 0, 0, Some(0), None),
    (
        "opencode-go/muse-spark-1.2-contributor",
        100000,
        200000,
        Some(2000),
        None,
    ),
    (
        "opencode-go/muse-spark-1.3-contributor",
        100000,
        200000,
        Some(2000),
        None,
    ),
    (
        "opencode-go/qwen3.7-plus",
        400000,
        1600000,
        Some(40000),
        Some(500000),
    ),
    (
        "opencode-go/qwen3.8-flash",
        150000,
        470000,
        Some(16000),
        Some(200000),
    ),
    (
        "opencode-go/qwen3.8-max",
        2000000,
        6000000,
        Some(250000),
        Some(2500000),
    ),
    (
        "opencode-go/qwen4-max",
        1500000,
        4500000,
        Some(75000),
        Some(1875000),
    ),
    (
        "opencode-go/space-bunny",
        150000,
        600000,
        Some(30000),
        Some(0),
    ),
    (
        "opencode-go/step-5-preview",
        1000000,
        2700000,
        Some(100000),
        None,
    ),
    ("opencode-go/step-5-preview-free", 0, 0, Some(0), None),
];

#[cfg(test)]
mod tests {
    use super::*;

    const MILLION: Tokens = Tokens {
        input: 1_000_000,
        output: 1_000_000,
        cache_read: 1_000_000,
        cache_write: 1_000_000,
    };

    #[test]
    fn known_model_is_priced_by_the_table() {
        let book = PriceBook::default();
        let tokens = Tokens {
            input: 600,
            output: 200,
            cache_read: 400,
            cache_write: 0,
        };
        // gpt-6.1-sol: вход 2, выход 10, чтение кеша 0,1 доллара за 1 млн.
        assert_eq!(
            book.cost_micro_usd("gpt-6.1-sol", tokens),
            Some(1_200 + 2_000 + 40)
        );
    }

    #[test]
    fn every_part_is_counted_once() {
        let book = PriceBook::default();
        let price = book.price_of("gpt-6.1-sol").unwrap();
        let sum =
            price.input + price.output + price.cache_read.unwrap() + price.cache_write.unwrap();
        assert_eq!(book.cost_micro_usd("gpt-6.1-sol", MILLION), Some(sum));
    }

    #[test]
    fn unknown_model_has_no_cost() {
        assert_eq!(
            PriceBook::default().cost_micro_usd("no-such-model", MILLION),
            None
        );
    }

    #[test]
    fn provider_prefix_is_ignored_when_looking_up() {
        let book = PriceBook::default();
        assert_eq!(
            book.cost_micro_usd("openai/gpt-6.1-sol", MILLION),
            book.cost_micro_usd("gpt-6.1-sol", MILLION)
        );
        assert!(book.cost_micro_usd("openai/gpt-6.1-sol", MILLION).is_some());
    }

    #[test]
    fn foreign_provider_does_not_inherit_the_bare_rate() {
        let book = PriceBook::default();
        assert!(book.price_of("gpt-5").is_some());
        assert_eq!(book.price_of("other-provider/gpt-5"), None);
        assert_eq!(book.cost_micro_usd("other-provider/gpt-5", MILLION), None);
        assert_eq!(book.price_of("a/b/gpt-5"), None);
    }

    #[test]
    fn table_providers_are_known_aliases_of_the_bare_name() {
        let book = PriceBook::default();
        assert_eq!(book.price_of("openai/gpt-5"), book.price_of("gpt-5"));
        assert_eq!(book.price_of("opencode-go/gpt-5"), book.price_of("gpt-5"));
    }

    #[test]
    fn bare_override_follows_the_same_rule() {
        let overrides: BTreeMap<String, PriceConfig> =
            toml::from_str("[\"my-model\"]\ninput = 3.0\noutput = 4.0\n").unwrap();
        let book = PriceBook::with_overrides(&overrides);
        assert!(book.price_of("my-model").is_some());
        assert!(book.price_of("openai/my-model").is_some());
        assert_eq!(book.price_of("other-provider/my-model"), None);
    }

    #[test]
    fn qualified_override_is_an_exact_id() {
        let overrides: BTreeMap<String, PriceConfig> =
            toml::from_str("[\"other-provider/gpt-5\"]\ninput = 1.0\noutput = 2.0\n").unwrap();
        let book = PriceBook::with_overrides(&overrides);
        assert!(book.cost_micro_usd("other-provider/gpt-5", MILLION).is_some());
        assert_ne!(
            book.price_of("other-provider/gpt-5"),
            PriceBook::default().price_of("gpt-5")
        );
    }

    #[test]
    fn config_override_beats_the_table_and_prices_unknown_models() {
        let overrides: BTreeMap<String, PriceConfig> = toml::from_str(
            "[\"gpt-6.1-sol\"]\ninput = 1.0\noutput = 2.0\n[\"my-model\"]\ninput = 3.0\noutput = 4.0\ncache_read = 0.5\n",
        )
        .unwrap();
        let book = PriceBook::with_overrides(&overrides);
        let tokens = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(book.cost_micro_usd("gpt-6.1-sol", tokens), Some(3_000_000));
        assert_eq!(book.cost_micro_usd("my-model", tokens), Some(7_000_000));
        let cached = Tokens {
            input: 0,
            output: 0,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        };
        // Цены записи кеша нет: берётся цена входа.
        assert_eq!(
            book.cost_micro_usd("my-model", cached),
            Some(500_000 + 3_000_000)
        );
    }

    #[test]
    fn invalid_override_is_rejected() {
        let bad = PriceConfig {
            input: -1.0,
            output: 1.0,
            cache_read: None,
            cache_write: None,
        };
        assert!(bad.validate().is_err());
        let nan = PriceConfig {
            input: f64::NAN,
            output: 1.0,
            cache_read: None,
            cache_write: None,
        };
        assert!(nan.validate().is_err());
    }
}
