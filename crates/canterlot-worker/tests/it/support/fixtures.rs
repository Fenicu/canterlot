//! Replay of stage-0 evidence wrappers and hand-built fixtures.
//!
//! Evidence files are `{"t_ms","dir","data"}` wrappers: `out` lines are CLI
//! frames, `in` lines are what the prototype wrote to stdin, `note` lines are
//! prototype annotations. Constructed fixtures are plain frames, one per line.

use serde_json::Value;
use std::path::PathBuf;

fn read_lines(path: PathBuf) -> Vec<String> {
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    text.lines().map(str::to_owned).collect()
}

fn evidence_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/claude-code")
        .join(format!("{name}.jsonl"))
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/it/fixtures/claude_code")
        .join(format!("{name}.jsonl"))
}

pub struct Replay {
    /// Serialized `data` of `out` lines, in file order.
    pub out: Vec<String>,
    /// Serialized `data` of `in` lines, in file order.
    pub incoming: Vec<String>,
}

fn unwrap(dir: &str, line: &str) -> Option<(String, Value)> {
    let wrapper: Value = serde_json::from_str(line).ok()?;
    if wrapper.get("dir").and_then(Value::as_str) != Some(dir) {
        return None;
    }
    let data = wrapper.get("data")?.clone();
    let text = serde_json::to_string(&data).expect("serialize data");
    Some((text, data))
}

/// Evidence frames: `out` lines unwrapped, `note` lines skipped.
pub fn replay_evidence(name: &str) -> Replay {
    let mut replay = Replay {
        out: Vec::new(),
        incoming: Vec::new(),
    };
    for line in read_lines(evidence_path(name)) {
        if let Some((text, _)) = unwrap("out", &line) {
            replay.out.push(text);
        } else if let Some((text, _)) = unwrap("in", &line) {
            replay.incoming.push(text);
        }
    }
    replay
}

/// Hand-built fixture frames, unwrapped from their `{"data", ...}` envelope.
pub fn replay_fixture(name: &str) -> Vec<String> {
    read_lines(fixture_path(name))
        .into_iter()
        .filter_map(|line| {
            let wrapper: Value = serde_json::from_str(&line).ok()?;
            if wrapper.get("dir").and_then(Value::as_str) != Some("out") {
                return None;
            }
            let data = wrapper.get("data")?;
            serde_json::to_string(data).ok()
        })
        .collect()
}

/// Кадры фикстуры по порядку файла, с направлением: `in` — записанное в stdin, `out` — кадр CLI.
#[allow(dead_code)]
pub fn replay_fixture_rows(name: &str) -> Vec<(String, String)> {
    read_lines(fixture_path(name))
        .into_iter()
        .filter_map(|line| {
            let wrapper: Value = serde_json::from_str(&line).ok()?;
            let dir = wrapper.get("dir")?.as_str()?.to_owned();
            if dir == "note" {
                return None;
            }
            Some((dir, serde_json::to_string(wrapper.get("data")?).ok()?))
        })
        .collect()
}

pub mod codex {
    //! Повтор кадров Codex: свидетельства и сконструированные фикстуры в обёртке
    //! `{"t_ms","dir","data"}`. `out` — сообщение app-server, `in` — то, что писал
    //! зонд, `err` — строка stderr, `note` пропускается. Не записанные дословно
    //! ответы (`<omitted...>`) видны как `Omitted`.
    //!
    //! Использует одна цель; остальным разрешаем мёртвый код здесь, а не у них.
    #![allow(dead_code)]
    use serde_json::Value;
    use std::path::PathBuf;

    pub enum Frame {
        Out(Value),
        In(Value),
        Err(String),
        Omitted { dir: String },
    }

    fn read_lines(path: PathBuf) -> Vec<String> {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        text.lines().map(str::to_owned).collect()
    }

    fn is_omitted(data: &Value) -> bool {
        data.as_str().is_some_and(|s| s.starts_with("<omitted"))
    }

    // Сначала фикстура, затем свидетельство с тем же именем.
    pub fn replay(name: &str) -> Vec<Frame> {
        replay_lines(name)
            .into_iter()
            .map(|(_, frame)| frame)
            .collect()
    }

    // Кадры с номерами строк (с единицы, как в плане) для точечных тестов.
    pub fn replay_lines(name: &str) -> Vec<(usize, Frame)> {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/it/fixtures/codex")
            .join(format!("{name}.jsonl"));
        let path = if fixture.exists() {
            fixture
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/stage0/evidence/codex")
                .join(format!("{name}.jsonl"))
        };
        let mut frames = Vec::new();
        for (index, line) in read_lines(path).iter().enumerate() {
            let wrapper: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(dir) = wrapper.get("dir").and_then(Value::as_str) else {
                // Сырая строка сконструированной фикстуры без обёртки.
                if wrapper.get("data").is_none() {
                    frames.push((index + 1, Frame::Out(wrapper)));
                }
                continue;
            };
            if dir == "note" {
                continue;
            }
            let data = wrapper.get("data").cloned().unwrap_or(Value::Null);
            if is_omitted(&data) {
                frames.push((
                    index + 1,
                    Frame::Omitted {
                        dir: dir.to_owned(),
                    },
                ));
                continue;
            }
            match dir {
                "out" => frames.push((index + 1, Frame::Out(data))),
                "in" => frames.push((index + 1, Frame::In(data))),
                "err" => frames.push((
                    index + 1,
                    Frame::Err(match data {
                        Value::String(s) => s,
                        other => serde_json::to_string(&other).unwrap_or_default(),
                    }),
                )),
                _ => {}
            }
        }
        frames
    }

    // Только данные `out`-кадров: уведомления для переводчика.
    pub fn out_values(frames: &[Frame]) -> Vec<Value> {
        frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Out(data) => Some(data.clone()),
                _ => None,
            })
            .collect()
    }
}
