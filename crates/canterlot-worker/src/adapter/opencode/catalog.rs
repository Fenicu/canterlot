//! Чтение `GET /api/model`. Ответ несёт `settings.apiKey` открытым текстом, поэтому
//! отсюда выходит только перечисленное, а сам ответ не хранится и не журналируется.

use serde_json::Value;
use std::collections::HashMap;

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
