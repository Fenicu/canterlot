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
        .join("tests/fixtures/claude_code")
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
