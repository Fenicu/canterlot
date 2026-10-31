//! Чтение `GET /api/model`. Ответ несёт `settings.apiKey` открытым текстом, поэтому
//! отсюда выходит только перечисленное, а сам ответ не хранится и не журналируется.

use serde_json::Value;
use std::collections::HashMap;

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

fn usable(model: &Value) -> bool {
    model.get("enabled").and_then(Value::as_bool) != Some(false)
        && model.get("status").and_then(Value::as_str) != Some("deprecated")
}

/// Записи каталога машины: Go и явно включённые провайдеры, выбор по умолчанию первым.
pub fn entries(models: &Value, default: Option<&Value>, providers: &[String]) -> Vec<ModelEntry> {
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
            Some(ModelEntry { id, title, efforts })
        })
        .collect();
    entries.sort_by_key(|entry| Some(&entry.id) != default.as_ref());
    entries
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
        let shown = format!("{:?}", entries(&list, None, &[]));
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
        let found = entries(&list, None, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "opencode-go/ok");
    }

    #[test]
    fn default_model_goes_first_and_the_rest_keep_their_order() {
        let list = json!({"data": [
            model("opencode-go", "a"), model("opencode-go", "b"), model("opencode-go", "c"),
        ]});
        let default = json!({"data": {"id": "b", "providerID": "opencode-go"}});
        let ids: Vec<String> = entries(&list, Some(&default), &[])
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, ["opencode-go/b", "opencode-go/a", "opencode-go/c"]);
    }

    #[test]
    fn model_without_variants_has_one_effort() {
        let mut plain = model("opencode-go", "plain");
        plain["variants"] = json!([]);
        let found = entries(&json!({"data": [plain]}), None, &[]);
        assert_eq!(found[0].efforts, [NO_VARIANT]);
    }

    #[test]
    fn id_with_a_slash_stays_whole() {
        let list = json!({"data": [model("openrouter", "anthropic/fable")]});
        let found = entries(&list, None, &["openrouter".to_owned()]);
        assert_eq!(found[0].id, "openrouter/anthropic/fable");
    }
}
