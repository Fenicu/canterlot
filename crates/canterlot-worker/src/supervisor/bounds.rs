//! Потолки того, что попадает в SJ. Запись больше пакета не доехала бы до хаба
//! и остановила поток данных всей машины, больше мегабайта — сам журнал.

use canterlot_proto::{
    limits::MAX_EVENT_BATCH_BYTES,
    worker::v1::{AdapterDiagnostic, TodoItem, record_body::Body},
};
use prost::Message;
use serde_json::Value;
use uuid::Uuid;

/// Потолок JSON карточки: запись вместе с обвязкой должна влезать в пакет SJ.
pub(super) const MAX_REQUEST_PAYLOAD_BYTES: usize = 262_144;
/// Потолок идентификатора: он приходит от CLI и хранится в индексируемой колонке.
pub(super) const MAX_REQUEST_ID_BYTES: usize = 1024;

/// Потолки списка задач: весь факт не больше карточки запроса.
const MAX_TODO_BYTES: usize = MAX_REQUEST_PAYLOAD_BYTES;
const MAX_TODO_ITEMS: usize = 256;
const MAX_TODO_TEXT_BYTES: usize = 4_096;

/// Потолок записи: остаток пакета уходит на его обвязку.
const MAX_RECORD_BYTES: usize = MAX_EVENT_BATCH_BYTES - 1024;
/// Имена и сводки от адаптера. Хаб хранит 128 и 1024 символа, столько и остаётся.
const NAME_BYTES: usize = 512;
const SUMMARY_BYTES: usize = 4096;

/// Шаги ужатия: длина строки в байтах и число элементов контейнера.
const STEPS: [(usize, usize); 4] = [(32_768, usize::MAX), (4_096, 1_024), (512, 128), (128, 16)];

fn clip(text: &mut String, limit: usize) {
    text.truncate(text.floor_char_boundary(limit));
}

/// Запись, которую можно журналировать: длинные метаданные подрезаны, а всё,
/// что не влезает и так, заменяется диагностикой.
pub(super) fn bounded_record(execution_id: Uuid, mut body: Body) -> Body {
    match &mut body {
        Body::BlockStarted(f) => {
            clip(&mut f.agent, NAME_BYTES);
            clip(&mut f.model, NAME_BYTES);
            clip(&mut f.tool_name, NAME_BYTES);
            clip(&mut f.summary, SUMMARY_BYTES);
        }
        Body::BlockFinal(f) => clip(&mut f.summary, SUMMARY_BYTES),
        Body::PhaseChanged(f) => {
            clip(&mut f.tool_name, NAME_BYTES);
            clip(&mut f.summary, SUMMARY_BYTES);
        }
        Body::RunFinished(f) => {
            for usage in &mut f.usage {
                clip(&mut usage.agent, NAME_BYTES);
                clip(&mut usage.model, NAME_BYTES);
            }
        }
        _ => {}
    }
    if body.encoded_len() <= MAX_RECORD_BYTES {
        return body;
    }
    tracing::warn!(kind = "record_too_large", "oversized record replaced");
    Body::AdapterDiagnostic(AdapterDiagnostic {
        execution_id: execution_id.to_string(),
        kind: "unknown_optional_event".into(),
        event_type: "record_too_large".into(),
    })
}

/// Список в пределах потолка. Длинный пункт подрезается с многоточием, лишние
/// отбрасываются с хвоста; второе значение — сколько пунктов отброшено.
pub(super) fn bounded_todo(mut items: Vec<TodoItem>) -> (Vec<TodoItem>, u32) {
    let total = items.len();
    items.truncate(MAX_TODO_ITEMS);
    let mut bytes = 0;
    let mut kept = 0;
    for item in &mut items {
        if item.text.len() > MAX_TODO_TEXT_BYTES {
            clip(&mut item.text, MAX_TODO_TEXT_BYTES);
            item.text.push('…');
        }
        bytes += item.encoded_len() + prost::length_delimiter_len(item.encoded_len()) + 1;
        if bytes > MAX_TODO_BYTES {
            break;
        }
        kept += 1;
    }
    items.truncate(kept);
    (items, u32::try_from(total - kept).unwrap_or(u32::MAX))
}

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
    use canterlot_proto::{
        requests::{AnswerValue, DecisionPayload},
        worker::v1::{BlockStarted, RequestClosed},
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    fn bounded(kind: &str, payload: Value) -> Value {
        let json = bounded_json(kind, payload).expect("preview fits");
        assert!(json.len() <= MAX_REQUEST_PAYLOAD_BYTES, "{}", json.len());
        serde_json::from_slice(&json).unwrap()
    }

    fn todo(text: &str) -> TodoItem {
        TodoItem {
            text: text.to_owned(),
            status: canterlot_proto::worker::v1::TodoStatus::Pending as i32,
        }
    }

    #[test]
    fn short_list_passes_untouched() {
        let items = vec![todo("один"), todo("два")];
        assert_eq!(bounded_todo(items.clone()), (items, 0));
    }

    #[test]
    fn long_list_is_cut_with_a_count_of_what_was_dropped() {
        let (items, omitted) = bounded_todo(vec![todo("пункт"); MAX_TODO_ITEMS + 44]);
        assert_eq!(items.len(), MAX_TODO_ITEMS);
        assert_eq!(omitted, 44);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary_with_an_ellipsis() {
        let (items, omitted) = bounded_todo(vec![todo(&"щ".repeat(MAX_TODO_TEXT_BYTES))]);
        assert_eq!(omitted, 0);
        let text = &items[0].text;
        assert!(text.ends_with('…'), "{text}");
        assert!(text.len() <= MAX_TODO_TEXT_BYTES + '…'.len_utf8());
    }

    #[test]
    fn huge_list_fits_the_ceiling_and_survives_the_record_bound() {
        let id = Uuid::now_v7();
        let (items, omitted) = bounded_todo(vec![todo(&"щ".repeat(100_000)); 1_000]);
        assert!(!items.is_empty());
        assert_eq!(items.len() + omitted as usize, 1_000);
        let fact = canterlot_proto::worker::v1::TodoUpdated {
            items,
            omitted,
            ..Default::default()
        };
        assert!(
            fact.encoded_len() <= MAX_TODO_BYTES + 64,
            "{}",
            fact.encoded_len()
        );
        assert!(matches!(
            bounded_record(id, Body::TodoUpdated(fact)),
            Body::TodoUpdated(_)
        ));
    }

    #[test]
    fn long_metadata_is_clipped_and_the_record_survives() {
        let id = Uuid::now_v7();
        let body = bounded_record(
            id,
            Body::BlockStarted(BlockStarted {
                block_id: "b".into(),
                agent: "щ".repeat(600_000),
                model: "м".repeat(600_000),
                tool_name: "😀".repeat(300_000),
                summary: "aщ".repeat(600_000),
                ..Default::default()
            }),
        );
        let Body::BlockStarted(started) = body else {
            panic!("metadata must not cost the block: {body:?}")
        };
        assert_eq!(started.agent, "щ".repeat(256));
        assert_eq!(started.tool_name, "😀".repeat(128));
        assert!(started.summary.len() <= SUMMARY_BYTES);
    }

    #[test]
    fn record_that_cannot_be_clipped_becomes_a_diagnostic() {
        let id = Uuid::now_v7();
        let body = bounded_record(
            id,
            Body::RequestClosed(RequestClosed {
                request_id: "r".into(),
                reason: "щ".repeat(MAX_EVENT_BATCH_BYTES),
            }),
        );
        let Body::AdapterDiagnostic(stub) = body else {
            panic!("oversized record must be replaced: {body:?}")
        };
        assert_eq!(stub.event_type, "record_too_large");
        assert_eq!(stub.execution_id, id.to_string());
    }

    #[test]
    fn small_record_is_left_alone() {
        let closed = RequestClosed {
            request_id: "r".into(),
            reason: "answered".into(),
        };
        let body = bounded_record(Uuid::now_v7(), Body::RequestClosed(closed.clone()));
        assert!(matches!(body, Body::RequestClosed(f) if f == closed));
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
