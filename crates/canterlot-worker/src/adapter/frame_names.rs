//! Имена из неизвестных кадров CLI для диагностики: только имена ключей верхнего
//! уровня, значения не читаются и дальше не идут.

use serde_json::Value;

/// Сколько имён уходит в одну диагностику.
pub const MAX_FIELD_NAMES: usize = 32;
/// Длина одного имени в символах.
pub const MAX_NAME_CHARS: usize = 64;

/// Имя из чужого кадра в безопасном виде: буквы, цифры и `_`, остальное `_`.
pub fn safe_name(raw: &str) -> String {
    raw.chars()
        .take(MAX_NAME_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Отсортированные имена без повторов, не больше `MAX_FIELD_NAMES`.
pub fn field_names<'a>(keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut names: Vec<String> = keys
        .into_iter()
        .map(safe_name)
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.truncate(MAX_FIELD_NAMES);
    names
}

/// Имена ключей объекта; не объект даёт пустой список.
pub fn object_field_names(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| field_names(object.keys().map(String::as_str)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_sorted_sanitized_and_bounded() {
        let long = "k".repeat(200);
        let names = field_names(["zeta", "alpha", "a-b", "a b", "", &long, "alpha"]);
        assert_eq!(names, ["a_b", "alpha", "k".repeat(64).as_str(), "zeta"]);
    }

    #[test]
    fn at_most_thirty_two_names_survive() {
        let keys: Vec<String> = (0..100).map(|n| format!("key{n:03}")).collect();
        let names = field_names(keys.iter().map(String::as_str));
        assert_eq!(names.len(), MAX_FIELD_NAMES);
        assert_eq!(names[0], "key000");
        assert_eq!(names[31], "key031");
    }

    #[test]
    fn values_never_reach_the_names() {
        let frame = json!({
            "token": "sk-live-SECRET-value",
            "nested": {"password": "hunter2"},
            "list": ["SECRET-in-list"],
        });
        let names = object_field_names(&frame);
        assert_eq!(names, ["list", "nested", "token"]);
        let joined = names.join(" ");
        for secret in ["SECRET", "hunter2", "sk-live", "password"] {
            assert!(!joined.contains(secret), "{secret}");
        }
        assert!(object_field_names(&json!("text")).is_empty());
        assert!(object_field_names(&Value::Null).is_empty());
    }
}
