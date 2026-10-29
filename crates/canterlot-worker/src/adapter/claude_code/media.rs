//! Картинки, которые агент открыл инструментом Read (pilot.md §5.7).
//! Байты и пути не попадают ни в журналы, ни в `Debug`.

use crate::adapter::{AdapterEvent, MediaSource};
use base64::Engine;
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

/// Открытые вызовы `Read` для картинок: `tool_use_id` → (блок вызова, путь).
#[derive(Default)]
pub struct ImageWatch {
    calls: HashMap<String, (Uuid, PathBuf)>,
}

impl fmt::Debug for ImageWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageWatch")
            .field("pending", &self.calls.len())
            .finish()
    }
}

fn is_image_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
}

/// Байты первого блока `image` с base64 в результате инструмента: то, что агент получил.
fn received_bytes(content: &Value) -> Option<Vec<u8>> {
    content.as_array()?.iter().find_map(|block| {
        if block.get("type").and_then(Value::as_str) != Some("image") {
            return None;
        }
        let source = block.get("source")?;
        if source.get("type").and_then(Value::as_str) != Some("base64") {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.get("data")?.as_str()?)
            .ok()?;
        (!bytes.is_empty()).then_some(bytes)
    })
}

impl ImageWatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_tool_call(
        &mut self,
        block_id: Uuid,
        tool_use_id: &str,
        tool_name: &str,
        input: &Value,
    ) {
        if tool_name != "Read" {
            return;
        }
        let Some(path) = input.get("file_path").and_then(Value::as_str) else {
            return;
        };
        let path = PathBuf::from(path);
        if is_image_path(&path) {
            self.calls.insert(tool_use_id.to_owned(), (block_id, path));
        }
    }

    /// Результат закрывает вызов: событие отдаётся один раз, ошибка не публикует ничего.
    pub fn on_tool_result(
        &mut self,
        tool_use_id: &str,
        content: &Value,
        is_error: bool,
    ) -> Option<AdapterEvent> {
        let (block_id, path) = self.calls.remove(tool_use_id)?;
        if is_error {
            return None;
        }
        let source = match received_bytes(content) {
            Some(bytes) => MediaSource::Bytes(bytes),
            None => MediaSource::Path(path),
        };
        Some(AdapterEvent::Media { block_id, source })
    }
}
