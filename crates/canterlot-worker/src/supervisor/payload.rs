//! Карточка запроса в журнале: у неё есть потолок, а исходный вход остаётся в адаптере.

use serde_json::Value;

/// Потолок JSON карточки: запись вместе с обвязкой должна влезать в пакет SJ.
pub(super) const MAX_REQUEST_PAYLOAD_BYTES: usize = 262_144;
/// Потолок идентификатора: он приходит от CLI и хранится в индексируемой колонке.
pub(super) const MAX_REQUEST_ID_BYTES: usize = 1024;

/// Шаги ужатия: длина строки в байтах и число элементов контейнера.
const STEPS: [(usize, usize); 4] = [(32_768, usize::MAX), (4_096, 1_024), (512, 128), (128, 16)];

/// JSON карточки в пределах потолка. Крупная карточка заменяется превью с меткой
/// `truncated: true`, решение владельца при этом относится к исходному входу.
/// `None` — не влезает даже превью.
pub(super) fn bounded_json(kind: &str, payload: Value) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(&payload).ok()?;
    if json.len() <= MAX_REQUEST_PAYLOAD_BYTES {
        return Some(json);
    }
    let mut preview = payload;
    let question = kind == "question";
    for (text, items) in STEPS {
        shrink(&mut preview, question, text, items);
        preview
            .as_object_mut()?
            .insert("truncated".into(), Value::Bool(true));
        let json = serde_json::to_vec(&preview).ok()?;
        if json.len() <= MAX_REQUEST_PAYLOAD_BYTES {
            return Some(json);
        }
    }
    None
}

/// У вопроса ответ сверяется по `id` и `label`, поэтому они и состав списков неприкосновенны.
fn shrink(value: &mut Value, question: bool, text: usize, items: usize) {
    match value {
        Value::String(string) => string.truncate(string.floor_char_boundary(text)),
        Value::Array(list) => {
            if !question {
                list.truncate(items);
            }
            for item in list {
                shrink(item, question, text, items);
            }
        }
        Value::Object(map) => {
            if !question && map.len() > items {
                let mut kept = 0;
                map.retain(|_, _| {
                    kept += 1;
                    kept <= items
                });
            }
            for (key, item) in map.iter_mut() {
                if !(question && matches!(key.as_str(), "id" | "label")) {
                    shrink(item, question, text, items);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::valid_decision;
    use canterlot_proto::requests::{AnswerValue, DecisionPayload};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn bounded(kind: &str, payload: Value) -> Value {
        let json = bounded_json(kind, payload).expect("preview fits");
        assert!(json.len() <= MAX_REQUEST_PAYLOAD_BYTES, "{}", json.len());
        serde_json::from_slice(&json).unwrap()
    }

    #[test]
    fn small_payload_is_returned_as_is() {
        let payload = json!({"tool_name": "Bash", "input": {"command": "ls"}});
        assert_eq!(bounded("permission", payload.clone()), payload);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        for (filler, repeat) in [("щ", 600_000), ("😀", 300_000), ("aщ", 400_000)] {
            let text = filler.repeat(repeat);
            let payload = json!({"tool_name": "Write", "tool_use_id": "toolu_1",
                "input": {"content": text}});
            let bounded = bounded("permission", payload);
            assert_eq!(bounded["truncated"], true);
            assert_eq!(bounded["tool_name"], "Write");
            assert_eq!(bounded["tool_use_id"], "toolu_1");
            let preview = bounded["input"]["content"].as_str().unwrap();
            assert!(!preview.is_empty());
            assert!(text.starts_with(preview));
        }
    }

    #[test]
    fn many_short_values_are_dropped_from_a_list_and_a_map() {
        let list: Vec<Value> = (0..200_000)
            .map(|i| json!({"path": format!("src/{i}.rs"), "diff": "+1"}))
            .collect();
        let bounded = bounded(
            "permission",
            json!({"tool_name": "apply_patch", "changes": list}),
        );
        assert_eq!(bounded["truncated"], true);
        assert!(!bounded["changes"].as_array().unwrap().is_empty());

        let map: serde_json::Map<String, Value> = (0..100_000)
            .map(|i| (format!("key-{i}"), json!(i)))
            .collect();
        let payload = json!({"tool_name": "mcp", "input": map});
        let bounded = super::tests::bounded("permission", payload);
        assert_eq!(bounded["truncated"], true);
        assert!(!bounded["input"].as_object().unwrap().is_empty());
    }

    #[test]
    fn question_keeps_what_the_answer_is_checked_against() {
        let payload = json!({"questions": [{
            "id": "q0",
            "text": "щ".repeat(400_000),
            "header": "h",
            "options": [{"label": "да", "description": "щ".repeat(100_000)}, {"label": "нет", "description": ""}],
            "multi_select": false,
            "allow_custom": false,
        }]});
        let bounded = bounded("question", payload.clone());
        assert_eq!(bounded["truncated"], true);
        assert_eq!(bounded["questions"][0]["id"], "q0");
        assert_eq!(bounded["questions"][0]["options"][1]["label"], "нет");
        let decision = DecisionPayload::Question {
            answers: BTreeMap::from([("q0".to_owned(), AnswerValue::Single("да".into()))]),
        };
        assert!(valid_decision("question", &bounded, &decision));
    }

    #[test]
    fn question_with_oversized_labels_gets_no_preview() {
        let payload = json!({"questions": [{"id": "q0", "text": "?",
            "options": [{"label": "щ".repeat(300_000)}]}]});
        assert_eq!(bounded_json("question", payload), None);
    }

    #[test]
    fn payload_that_is_not_an_object_gets_no_preview() {
        assert_eq!(bounded_json("permission", json!("щ".repeat(300_000))), None);
    }
}
