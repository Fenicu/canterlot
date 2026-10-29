#![forbid(unsafe_code)]

//! Картинки, которые агент открыл инструментом Read (план 2a, Task 5; pilot.md §5.7).
//! Фикстура `read_image.jsonl` построена вручную: формат результата Read для картинок
//! в записях этапа 0 отсутствует, подтверждается живым прогоном.

use base64::Engine;
use canterlot_proto::limits::MAX_MEDIA_BYTES;
use canterlot_worker::{
    adapter::{
        AdapterEvent, BlockKind, MediaSource,
        claude_code::{
            media::ImageWatch,
            parser::{Parsed, Parser},
        },
    },
    media::{self, Skip},
};
use serde_json::{Value, json};
use std::{fs, path::Path};
use uuid::Uuid;

#[allow(dead_code)]
#[path = "support/fixtures.rs"]
mod fixtures;

const PNG_4X3: &str = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAADCAIAAAA7ljmRAAAAEElEQVR42mO4IyICRww4OQD3LwwxOT2XCwAAAABJRU5ErkJggg==";

fn png_bytes() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(PNG_4X3)
        .unwrap()
}

/// Остаётся PNG по сигнатуре, но не совпадает с `png_bytes()`.
fn other_png_bytes() -> Vec<u8> {
    let mut bytes = png_bytes();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    bytes
}

fn image_result(bytes: &[u8]) -> Value {
    json!([{
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }])
}

fn read_call(watch: &mut ImageWatch, tool_use_id: &str, path: &Path) -> Uuid {
    let block = Uuid::now_v7();
    watch.on_tool_call(
        block,
        tool_use_id,
        "Read",
        &json!({"file_path": path.to_str().unwrap()}),
    );
    block
}

fn media_event(event: Option<AdapterEvent>) -> (Uuid, MediaSource) {
    match event {
        Some(AdapterEvent::Media { block_id, source }) => (block_id, source),
        other => panic!("expected Media, got {other:?}"),
    }
}

fn bytes_of(source: MediaSource) -> Vec<u8> {
    match source {
        MediaSource::Bytes(bytes) => bytes,
        other => panic!("expected Bytes, got {other:?}"),
    }
}

fn path_of(source: MediaSource) -> std::path::PathBuf {
    match source {
        MediaSource::Path(path) => path,
        other => panic!("expected Path, got {other:?}"),
    }
}

/// Прогоняет кадры через парсер и ImageWatch так же, как это делает адаптер.
fn drive(lines: &[String]) -> (Vec<(Uuid, String)>, Vec<AdapterEvent>) {
    let mut parser = Parser::new();
    let mut watch = ImageWatch::new();
    let mut tool_blocks = Vec::new();
    let mut media = Vec::new();
    for line in lines {
        for parsed in parser.feed(line) {
            match parsed {
                Parsed::Event(AdapterEvent::BlockStarted {
                    block_id,
                    kind: BlockKind::ToolCall,
                    tool_name: Some(name),
                    ..
                }) => tool_blocks.push((block_id, name)),
                Parsed::ToolCallComplete {
                    block_id,
                    tool_use_id,
                    tool_name,
                    input,
                } => watch.on_tool_call(block_id, &tool_use_id, &tool_name, &input),
                Parsed::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => media.extend(watch.on_tool_result(&tool_use_id, &content, is_error)),
                _ => {}
            }
        }
    }
    (tool_blocks, media)
}

#[test]
fn read_png_with_image_result_publishes_received_bytes() {
    let lines = fixtures::replay_fixture("read_image");
    let (tool_blocks, mut events) = drive(&lines);
    assert_eq!(events.len(), 1, "{events:?}");
    let (block_id, source) = media_event(events.pop());
    assert_eq!(tool_blocks.len(), 1);
    assert_eq!(tool_blocks[0].1, "Read");
    assert_eq!(block_id, tool_blocks[0].0, "media hangs on the Read block");
    let bytes = bytes_of(source);
    assert_eq!(bytes, png_bytes());
    // Файла по пути из вызова нет: байты не могли прийти с диска.
    assert!(!Path::new("/tmp/canterlot-fixture/shot.png").exists());

    // Байты результата сильнее содержимого файла, которое успело измениться.
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("shot.png");
    fs::write(&image, other_png_bytes()).unwrap();
    let mut watch = ImageWatch::new();
    let block = read_call(&mut watch, "toolu_a", &image);
    let (published_block, source) =
        media_event(watch.on_tool_result("toolu_a", &image_result(&png_bytes()), false));
    assert_eq!(published_block, block);
    assert_eq!(bytes_of(source), png_bytes());

    // Через существующий путь публикации байты становятся копией с типом и размерами.
    let exec = root.path().join("exec");
    fs::create_dir(&exec).unwrap();
    let (media_id, block) = (Uuid::now_v7(), Uuid::now_v7());
    let prepared = media::prepare(&exec, media_id, block, MediaSource::Bytes(bytes)).unwrap();
    assert_eq!(prepared.mime, "image/png");
    assert_eq!((prepared.width, prepared.height), (4, 3));
}

#[test]
fn read_png_without_image_block_falls_back_to_path() {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("screen.png");
    fs::write(&image, png_bytes()).unwrap();
    let results = [
        json!("[image: screen.png]"),
        json!([{"type": "text", "text": "image omitted"}]),
        json!([{"type": "image", "source": {"type": "url", "url": "https://example.invalid/a.png"}}]),
        json!([{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "***"}}]),
        json!([{"type": "image", "source": {"type": "base64", "media_type": "image/png"}}]),
        Value::Null,
    ];
    for (index, content) in results.iter().enumerate() {
        let mut watch = ImageWatch::new();
        let id = format!("toolu_{index}");
        let block = read_call(&mut watch, &id, &image);
        let (published, source) = media_event(watch.on_tool_result(&id, content, false));
        assert_eq!(published, block, "{content}");
        assert_eq!(path_of(source), image, "{content}");
    }

    // Файл читается сразу при событии: поздняя правка не подменяет увиденное.
    let mut watch = ImageWatch::new();
    let block = read_call(&mut watch, "toolu_late", &image);
    let (_, source) = media_event(watch.on_tool_result("toolu_late", &json!("ok"), false));
    let exec = root.path().join("exec");
    fs::create_dir(&exec).unwrap();
    let media_id = Uuid::now_v7();
    let prepared = media::prepare(&exec, media_id, block, source).unwrap();
    fs::write(&image, b"changed").unwrap();
    assert_eq!(
        fs::read(media::media_path(&exec, media_id)).unwrap(),
        png_bytes()
    );
    assert_eq!((prepared.width, prepared.height), (4, 3));
}

#[test]
fn relative_or_non_image_paths_are_ignored() {
    let result = image_result(&png_bytes());
    let mut watch = ImageWatch::new();
    let ignored = [
        ("t_rel", "Read", json!({"file_path": "shot.png"})),
        ("t_dot", "Read", json!({"file_path": "./shot.png"})),
        ("t_txt", "Read", json!({"file_path": "/tmp/notes.txt"})),
        ("t_dbl", "Read", json!({"file_path": "/tmp/shot.png.txt"})),
        ("t_bare", "Read", json!({"file_path": "/tmp/png"})),
        ("t_hidden", "Read", json!({"file_path": "/tmp/.png"})),
        ("t_svg", "Read", json!({"file_path": "/tmp/logo.svg"})),
        ("t_bmp", "Read", json!({"file_path": "/tmp/scan.bmp"})),
        ("t_none", "Read", json!({"limit": 10})),
        ("t_num", "Read", json!({"file_path": 7})),
        ("t_write", "Write", json!({"file_path": "/tmp/out.png"})),
        ("t_bash", "Bash", json!({"command": "cat /tmp/out.png"})),
    ];
    for (id, tool, input) in &ignored {
        watch.on_tool_call(Uuid::now_v7(), id, tool, input);
    }
    for (id, _, input) in &ignored {
        let event = watch.on_tool_result(id, &result, false);
        assert!(event.is_none(), "{id} {input}: {event:?}");
    }
    assert!(
        watch
            .on_tool_result("never_called", &result, false)
            .is_none()
    );

    // Расширения без учёта регистра, все пять типов.
    for (index, name) in ["a.png", "b.JPG", "c.jpeg", "d.WebP", "e.GIF", "f.PnG"]
        .into_iter()
        .enumerate()
    {
        let mut watch = ImageWatch::new();
        let id = format!("toolu_{index}");
        let path = Path::new("/tmp/pics").join(name);
        let block = read_call(&mut watch, &id, &path);
        let (published, _) = media_event(watch.on_tool_result(&id, &result, false));
        assert_eq!(published, block, "{name}");
    }
}

#[test]
fn failed_read_publishes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("gone.png");
    let mut watch = ImageWatch::new();
    read_call(&mut watch, "toolu_err", &image);
    let failed = watch.on_tool_result("toolu_err", &json!("File does not exist."), true);
    assert!(failed.is_none(), "{failed:?}");
    // Даже блок картинки в ошибочном результате ничего не публикует.
    read_call(&mut watch, "toolu_err2", &image);
    let failed = watch.on_tool_result("toolu_err2", &image_result(&png_bytes()), true);
    assert!(failed.is_none(), "{failed:?}");
    // Вызов считается закрытым: повторный успешный результат с тем же id ничего не даёт.
    let again = watch.on_tool_result("toolu_err", &image_result(&png_bytes()), false);
    assert!(again.is_none(), "{again:?}");

    // Успешный результат отдаёт событие один раз.
    read_call(&mut watch, "toolu_ok", &image);
    assert!(
        watch
            .on_tool_result("toolu_ok", &image_result(&png_bytes()), false)
            .is_some()
    );
    assert!(
        watch
            .on_tool_result("toolu_ok", &image_result(&png_bytes()), false)
            .is_none()
    );
}

#[test]
fn oversized_image_keeps_path_without_media() {
    let root = tempfile::tempdir().unwrap();
    let exec = root.path().join("exec");
    fs::create_dir(&exec).unwrap();

    // Файл крупнее лимита: событие несёт путь, копии и миниатюры нет.
    let large = root.path().join("large.png");
    fs::write(&large, png_bytes()).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&large).unwrap();
    file.set_len(MAX_MEDIA_BYTES + 1).unwrap();
    let mut watch = ImageWatch::new();
    let block = read_call(&mut watch, "toolu_path", &large);
    let (published, source) =
        media_event(watch.on_tool_result("toolu_path", &json!("[image: large.png]"), false));
    assert_eq!(published, block, "the tool block keeps its path summary");
    assert_eq!(path_of(source.clone()), large);
    let media_id = Uuid::now_v7();
    assert_eq!(
        media::prepare(&exec, media_id, block, source),
        Err(Skip::TooLarge)
    );
    assert!(!media::media_path(&exec, media_id).exists());

    // То же для байтов результата крупнее лимита.
    let mut oversized = png_bytes();
    oversized.resize(MAX_MEDIA_BYTES as usize + 1, 0);
    let mut watch = ImageWatch::new();
    let block = read_call(&mut watch, "toolu_bytes", &large);
    let (published, source) =
        media_event(watch.on_tool_result("toolu_bytes", &image_result(&oversized), false));
    assert_eq!(published, block);
    let media_id = Uuid::now_v7();
    assert_eq!(
        media::prepare(&exec, media_id, block, source),
        Err(Skip::TooLarge)
    );
    assert!(!media::media_path(&exec, media_id).exists());
    let left = fs::read_dir(exec.join(media::MEDIA_DIR))
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "nothing is left behind");
}
