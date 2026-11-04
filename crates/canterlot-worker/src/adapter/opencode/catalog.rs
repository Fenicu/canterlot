//! Чтение `GET /api/model`. Ответ несёт `settings.apiKey` открытым текстом, поэтому
//! отсюда выходит только перечисленное, а сам ответ не хранится и не журналируется.

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// Провайдер, который в каталоге всегда: подписка владельца (решение 2 спеки).
pub const GO_PROVIDER: &str = "opencode-go";
/// Усилие модели без вариантов: форма и хаб требуют хотя бы одно значение.
pub const NO_VARIANT: &str = "default";

/// Запись каталога машины, идентификатор `provider/model`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub title: String,
    pub efforts: Vec<String>,
    pub images: Option<bool>,
    pub context_window: Option<u64>,
}

/// Окно контекста по идентификатору `provider/model`; в событиях его нет.
pub fn context_windows(models: &Value) -> HashMap<String, u64> {
    models
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let provider = model.get("providerID")?.as_str()?;
            let id = model.get("id")?.as_str()?;
            let window = model.pointer("/limit/context")?.as_u64()?;
            Some((format!("{provider}/{id}"), window))
        })
        .collect()
}

fn variants_of(model: &Value) -> Vec<String> {
    model
        .get("variants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|variant| variant.get("id")?.as_str().map(str::to_owned))
        .collect()
}

fn qualified(model: &Value) -> Option<String> {
    let provider = model.get("providerID")?.as_str()?;
    let id = model.get("id")?.as_str()?;
    Some(format!("{provider}/{id}"))
}

// `None`: ответ не говорит о вводе модели, это не то же, что запрет.
fn accepts_images(model: &Value) -> Option<bool> {
    model
        .pointer("/capabilities/input")?
        .as_array()
        .map(|input| input.iter().any(|kind| kind == "image"))
}

/// Какие модели принимают картинки: остальным файл уходит путём в тексте.
pub fn image_support(models: &Value) -> HashMap<String, bool> {
    models
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| Some((qualified(model)?, accepts_images(model)?)))
        .collect()
}

/// Варианты всех доступных моделей: сервер принимает неизвестный вариант и падает на ходе.
pub fn variants(models: &Value) -> HashMap<String, Vec<String>> {
    models
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| Some((qualified(model)?, variants_of(model))))
        .collect()
}

/// Разбор шаблона фильтра: максимум одна `*` в начале и одна в конце.
/// `None` — звёздочка осталась в середине (`gpt*mini`, `**x`, `x**`).
pub fn pattern_parts(pattern: &str) -> Option<(bool, bool, &str)> {
    let (leading, rest) = match pattern.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, pattern),
    };
    let (trailing, core) = match rest.strip_suffix('*') {
        Some(core) => (true, core),
        None => (false, rest),
    };
    (!core.contains('*')).then_some((leading, trailing, core))
}

fn match_pattern(pattern: &str, id: &str) -> bool {
    match pattern_parts(pattern) {
        None => false,
        Some((true, true, core)) => id.contains(core),
        Some((true, false, core)) => id.ends_with(core),
        Some((false, true, core)) => id.starts_with(core),
        Some((false, false, core)) => id == core,
    }
}

fn usable(model: &Value) -> bool {
    model.get("enabled").and_then(Value::as_bool) != Some(false)
        && model.get("status").and_then(Value::as_str) != Some("deprecated")
}

/// Записи каталога машины: Go и явно включённые провайдеры, выбор по умолчанию первым.
pub fn entries(
    models: &Value,
    default: Option<&Value>,
    providers: &[String],
    provider_models: &BTreeMap<String, Vec<String>>,
) -> Vec<ModelEntry> {
    let default = default.and_then(|reply| qualified(reply.get("data")?));
    let mut entries: Vec<ModelEntry> = models
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| usable(model))
        .filter(|model| {
            model
                .get("providerID")
                .and_then(Value::as_str)
                .is_some_and(|provider| {
                    provider == GO_PROVIDER || providers.iter().any(|name| name == provider)
                })
        })
        .filter(|model| {
            let provider = model
                .get("providerID")
                .and_then(Value::as_str)
                .unwrap_or("");
            let id = model.get("id").and_then(Value::as_str).unwrap_or("");
            match provider_models.get(provider) {
                None => true,
                Some(patterns) => patterns.iter().any(|pattern| match_pattern(pattern, id)),
            }
        })
        .filter_map(|model| {
            let id = qualified(model)?;
            let title = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_owned();
            let mut efforts = variants_of(model);
            if efforts.is_empty() {
                efforts.push(NO_VARIANT.to_owned());
            }
            Some(ModelEntry {
                id,
                title,
                efforts,
                images: accepts_images(model),
                context_window: model.pointer("/limit/context").and_then(Value::as_u64),
            })
        })
        .collect();
    entries.sort_by_key(|entry| Some(&entry.id) != default.as_ref());
    entries
}

/// Модель из настройки воркера идёт первой: форма сессии выбирает первую.
pub fn prefer(entries: &mut [ModelEntry], model: &str) {
    if entries.iter().any(|entry| entry.id == model) {
        entries.sort_by_key(|entry| entry.id != model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(provider: &str, id: &str) -> Value {
        json!({
            "id": id, "providerID": provider, "name": id,
            "settings": {"apiKey": "секрет", "baseURL": "https://secret.example"},
            "variants": [{"id": "low", "settings": {"reasoningEffort": "low"}}],
            "limit": {"context": 1000}, "enabled": true, "status": "active",
        })
    }

    #[test]
    fn key_never_leaves_the_reply() {
        let list = json!({"data": [model("opencode-go", "a")]});
        let shown = format!("{:?}", entries(&list, None, &[], &BTreeMap::new()));
        assert!(shown.contains("opencode-go/a"));
        assert!(!shown.contains("секрет"));
        assert!(!shown.contains("secret.example"));
    }

    #[test]
    fn disabled_and_deprecated_models_are_left_out() {
        let mut disabled = model("opencode-go", "off");
        disabled["enabled"] = false.into();
        let mut old = model("opencode-go", "old");
        old["status"] = "deprecated".into();
        let list = json!({"data": [disabled, old, model("opencode-go", "ok")]});
        let found = entries(&list, None, &[], &BTreeMap::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "opencode-go/ok");
    }

    #[test]
    fn default_model_goes_first_and_the_rest_keep_their_order() {
        let list = json!({"data": [
            model("opencode-go", "a"), model("opencode-go", "b"), model("opencode-go", "c"),
        ]});
        let default = json!({"data": {"id": "b", "providerID": "opencode-go"}});
        let ids: Vec<String> = entries(&list, Some(&default), &[], &BTreeMap::new())
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, ["opencode-go/b", "opencode-go/a", "opencode-go/c"]);
    }

    #[test]
    fn preferred_model_goes_first_and_an_unknown_one_changes_nothing() {
        let list = json!({"data": [model("opencode-go", "a"), model("opencode-go", "b")]});
        let mut found = entries(&list, None, &[], &BTreeMap::new());
        prefer(&mut found, "opencode-go/b");
        assert_eq!(found[0].id, "opencode-go/b");
        prefer(&mut found, "opencode-go/absent");
        assert_eq!(found[0].id, "opencode-go/b");
    }

    #[test]
    fn model_without_variants_has_one_effort() {
        let mut plain = model("opencode-go", "plain");
        plain["variants"] = json!([]);
        let found = entries(&json!({"data": [plain]}), None, &[], &BTreeMap::new());
        assert_eq!(found[0].efforts, [NO_VARIANT]);
    }

    #[test]
    fn pictures_and_the_context_window_are_per_model() {
        let mut vision = model("opencode-go", "vision");
        vision["capabilities"] = json!({"input": ["text", "image"]});
        let mut plain = model("opencode-go", "plain");
        plain["capabilities"] = json!({"input": ["text"]});
        let list = json!({"data": [vision, plain]});
        let found = entries(&list, None, &[], &BTreeMap::new());
        assert_eq!(found[0].images, Some(true));
        assert_eq!(found[0].context_window, Some(1000));
        assert_eq!(found[1].images, Some(false));
        let silent = entries(
            &json!({"data": [model("opencode-go", "old")]}),
            None,
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(silent[0].images, None, "the reply says nothing");
        let support = image_support(&list);
        assert_eq!(support.get("opencode-go/vision"), Some(&true));
        assert_eq!(support.get("opencode-go/plain"), Some(&false));
        assert_eq!(support.get("opencode-go/unknown"), None);
    }

    #[test]
    fn id_with_a_slash_stays_whole() {
        let list = json!({"data": [model("openrouter", "anthropic/fable")]});
        let found = entries(&list, None, &["openrouter".to_owned()], &BTreeMap::new());
        assert_eq!(found[0].id, "openrouter/anthropic/fable");
    }

    fn filters(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(provider, patterns)| {
                (
                    (*provider).to_owned(),
                    patterns
                        .iter()
                        .map(|pattern| (*pattern).to_owned())
                        .collect(),
                )
            })
            .collect()
    }

    fn ids(found: Vec<ModelEntry>) -> Vec<String> {
        found.into_iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn provider_models_keep_only_matching_models() {
        let list = json!({"data": [
            model("opencode", "crouton-alpha-free"),
            model("opencode", "big-pickle"),
            model("opencode", "gpt-6.1-sol"),
        ]});
        let providers = vec!["opencode".to_owned()];
        let found = entries(
            &list,
            None,
            &providers,
            &filters(&[("opencode", &["*-free", "big-pickle"])]),
        );
        assert_eq!(
            ids(found),
            ["opencode/crouton-alpha-free", "opencode/big-pickle"]
        );
    }

    #[test]
    fn provider_without_an_entry_keeps_every_model() {
        let list = json!({"data": [
            model("opencode-go", "go-a"),
            model("opencode-go", "go-b"),
            model("opencode", "keep"),
        ]});
        let providers = vec!["opencode".to_owned()];
        let found = entries(
            &list,
            None,
            &providers,
            &filters(&[("opencode", &["keep"])]),
        );
        assert_eq!(
            ids(found),
            ["opencode-go/go-a", "opencode-go/go-b", "opencode/keep"]
        );
    }

    #[test]
    fn empty_provider_list_keeps_no_models_of_that_provider() {
        let list = json!({"data": [
            model("opencode-go", "go-a"),
            model("opencode", "a"),
        ]});
        let providers = vec!["opencode".to_owned()];
        let found = entries(&list, None, &providers, &filters(&[("opencode", &[])]));
        assert_eq!(ids(found), ["opencode-go/go-a"]);
    }

    #[test]
    fn go_provider_models_are_filtered_too() {
        let list = json!({"data": [
            model("opencode-go", "crouton-alpha-free"),
            model("opencode-go", "gpt-6.1-sol"),
        ]});
        let found = entries(&list, None, &[], &filters(&[("opencode-go", &["*-free"])]));
        assert_eq!(ids(found), ["opencode-go/crouton-alpha-free"]);
    }

    #[test]
    fn patterns_anchor_stars_at_the_ends() {
        assert!(match_pattern("gpt-*", "gpt-6.1-sol"));
        assert!(!match_pattern("gpt-*", "crouton-gpt-6.1"));
        assert!(match_pattern("*-free", "crouton-alpha-free"));
        assert!(!match_pattern("*-free", "free-alpha"));
        assert!(match_pattern("*flash*", "muse-flash-1"));
        assert!(!match_pattern("*flash*", "muse"));
        assert!(match_pattern("*", "anything/at/all"));
        assert!(match_pattern("big-pickle", "big-pickle"));
        assert!(!match_pattern("big-pickle", "big-pickle-2"));
    }

    #[test]
    fn only_leading_and_trailing_stars_are_patterns() {
        for pattern in ["gpt*mini", "**x", "x**", "a*b*c"] {
            assert!(pattern_parts(pattern).is_none(), "{pattern}");
        }
        for pattern in ["*", "*-free", "gpt-*", "*flash*", "exact"] {
            assert!(pattern_parts(pattern).is_some(), "{pattern}");
        }
    }

    #[test]
    fn slash_model_id_matches_the_whole_part_after_the_provider() {
        let list = json!({"data": [model("openrouter", "anthropic/fable")]});
        let providers = vec!["openrouter".to_owned()];
        let found = entries(
            &list,
            None,
            &providers,
            &filters(&[("openrouter", &["anthropic/*"])]),
        );
        assert_eq!(ids(found), ["openrouter/anthropic/fable"]);
    }

    #[test]
    fn default_model_filtered_out_stays_out() {
        let list = json!({"data": [
            model("opencode-go", "keep"),
            model("opencode-go", "default"),
        ]});
        let default = json!({"data": {"id": "default", "providerID": "opencode-go"}});
        let found = entries(
            &list,
            Some(&default),
            &[],
            &filters(&[("opencode-go", &["keep"])]),
        );
        assert_eq!(ids(found), ["opencode-go/keep"]);
    }
}
