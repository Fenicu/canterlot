//! Запись доказательств: JSONL `{"t_ms","dir","data"}` с усечением и маскированием секретов.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    In,
    Out,
    Err,
    Note,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::In => "in",
            Dir::Out => "out",
            Dir::Err => "err",
            Dir::Note => "note",
        }
    }
}

/// Максимум символов одной строки внутри записи.
pub const MAX_STR_CHARS: usize = 2000;
/// Максимум байт одной записи после усечения строк.
pub const MAX_ENTRY_BYTES: usize = 16 * 1024;
/// Бюджет файла: после него сырой поток заменяется маркерами, заметки пишутся всегда.
pub const FILE_BUDGET_BYTES: usize = 185 * 1024;

const SECRET_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "apikey",
    "api_key",
    "authorization",
    "cookie",
    "credential",
    "email",
    "org",
    "account",
];

const NEVER_REDACT: &[&str] = &["apiKeySource"];

pub fn sanitize(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(sanitize_str(s)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize).collect()),
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (key, value) in map {
                let value = if value.is_string() && is_secret_key(&key) {
                    Value::String("<redacted>".to_owned())
                } else {
                    sanitize(value)
                };
                out.insert(key, value);
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn is_secret_key(key: &str) -> bool {
    if NEVER_REDACT.contains(&key) {
        return false;
    }
    let lower = key.to_ascii_lowercase();
    SECRET_KEY_PARTS.iter().any(|part| lower.contains(part))
}

/// Заменить все адреса вида local@domain.tld внутри строки.
fn scrub_emails(s: &str) -> String {
    let is_local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let is_domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = i;
            while start > 0 && is_local(chars[start - 1]) {
                start -= 1;
            }
            let mut end = i + 1;
            while end < chars.len() && is_domain(chars[end]) {
                end += 1;
            }
            while end > i + 1 && chars[end - 1] == '.' {
                end -= 1;
            }
            let domain: String = chars[i + 1..end].iter().collect();
            let valid = start < i
                && domain.contains('.')
                && domain.rsplit('.').next().is_some_and(|tld| !tld.is_empty());
            if valid {
                out.extend(&chars[copied..start]);
                out.push_str("<redacted-email>");
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.extend(&chars[copied..]);
    out
}

fn sanitize_str(s: String) -> String {
    if s.contains("sk-ant-") || s.contains("Bearer ") {
        return "<redacted-secret>".to_owned();
    }
    let s = if s.contains('@') { scrub_emails(&s) } else { s };
    match s.char_indices().nth(MAX_STR_CHARS) {
        Some((cut, _)) => {
            let rest = s.len() - cut;
            format!("{}<truncated {rest} bytes>", &s[..cut])
        }
        None => s,
    }
}

/// Итоговая строка записи с учётом лимита одной записи.
pub fn render_entry(t_ms: u64, dir: Dir, data: Value) -> String {
    let data = sanitize(data);
    let line = json!({"t_ms": t_ms, "dir": dir.as_str(), "data": data}).to_string();
    if line.len() <= MAX_ENTRY_BYTES {
        return line;
    }
    let mut stub = Map::new();
    for key in ["type", "subtype", "uuid", "session_id"] {
        if let Some(v) = data.get(key) {
            stub.insert(key.to_owned(), v.clone());
        }
    }
    stub.insert("<truncated>".to_owned(), json!(line.len()));
    json!({"t_ms": t_ms, "dir": dir.as_str(), "data": Value::Object(stub)}).to_string()
}

struct Inner {
    sink: Box<dyn Write + Send>,
    bytes: usize,
}

pub struct Recorder {
    inner: Mutex<Inner>,
    start: Instant,
}

impl Recorder {
    pub fn create(path: &Path) -> anyhow::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        Ok(Arc::new(Self::with_sink(Box::new(file))))
    }

    pub fn with_sink(sink: Box<dyn Write + Send>) -> Self {
        Self {
            inner: Mutex::new(Inner { sink, bytes: 0 }),
            start: Instant::now(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub fn record(&self, dir: Dir, data: Value) -> u64 {
        let Ok(mut inner) = self.inner.lock() else {
            return self.elapsed_ms();
        };
        // Время берём под блокировкой, чтобы t_ms в файле не убывал.
        let t_ms = self.elapsed_ms();
        let mut line = render_entry(t_ms, dir, data);
        if dir != Dir::Note && inner.bytes + line.len() > FILE_BUDGET_BYTES {
            let marker = format!("<truncated {} bytes>", line.len());
            line = json!({"t_ms": t_ms, "dir": dir.as_str(), "data": marker}).to_string();
        }
        line.push('\n');
        inner.bytes += line.len();
        // Ошибка записи доказательства не должна ронять сценарий.
        let _ = inner.sink.write_all(line.as_bytes());
        let _ = inner.sink.flush();
        t_ms
    }

    pub fn note(&self, data: Value) -> u64 {
        self.record(Dir::Note, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn lines(buf: &SharedBuf) -> Vec<Value> {
        let bytes = buf.0.lock().expect("lock").clone();
        String::from_utf8(bytes)
            .expect("utf8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("json line"))
            .collect()
    }

    #[test]
    fn long_strings_are_truncated_with_marker() {
        let long = "я".repeat(MAX_STR_CHARS + 10);
        let out = sanitize(json!({"text": long}));
        let text = out["text"].as_str().expect("str");
        assert!(text.starts_with(&"я".repeat(MAX_STR_CHARS)));
        assert!(text.ends_with("<truncated 20 bytes>"));
    }

    #[test]
    fn secret_like_string_fields_are_redacted_but_counters_kept() {
        let out = sanitize(json!({
            "accessToken": "abc",
            "usage": {"input_tokens": 12, "output_tokens": 3},
            "nested": [{"email": "a@b.c"}, "user@example.com", "Bearer xyz", "key sk-ant-123"],
            "orgName": "Acme",
            "origin": {"kind": "human"},
            "text": "plain"
        }));
        assert_eq!(out["accessToken"], "<redacted>");
        assert_eq!(out["usage"]["input_tokens"], 12);
        assert_eq!(out["nested"][0]["email"], "<redacted>");
        assert_eq!(out["nested"][1], "<redacted-email>");
        assert_eq!(out["nested"][2], "<redacted-secret>");
        assert_eq!(out["nested"][3], "<redacted-secret>");
        assert_eq!(out["orgName"], "<redacted>");
        assert_eq!(out["origin"]["kind"], "human");
        assert_eq!(out["text"], "plain");
    }

    #[test]
    fn emails_inside_longer_strings_are_scrubbed() {
        let out = sanitize(json!([
            "  \"email\": \"john.doe+x@mail.example.com\",",
            "a@b no",
            "x@y.z's Org"
        ]));
        assert_eq!(out[0], "  \"email\": \"<redacted-email>\",");
        assert_eq!(out[1], "a@b no");
        assert_eq!(out[2], "<redacted-email>'s Org");
    }

    #[test]
    fn oversized_entry_keeps_type_and_size() {
        let big: Vec<Value> = (0..200)
            .map(|i| json!("x".repeat(500) + &i.to_string()))
            .collect();
        let line = render_entry(
            5,
            Dir::Out,
            json!({"type": "user", "subtype": "s", "blob": big}),
        );
        assert!(line.len() <= MAX_ENTRY_BYTES);
        let v: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(v["t_ms"], 5);
        assert_eq!(v["dir"], "out");
        assert_eq!(v["data"]["type"], "user");
        assert!(v["data"]["<truncated>"].as_u64().expect("size") > MAX_ENTRY_BYTES as u64);
    }

    #[test]
    fn recorder_writes_monotonic_jsonl_and_respects_budget() {
        let buf = SharedBuf::default();
        let rec = Recorder::with_sink(Box::new(buf.clone()));
        rec.record(Dir::In, json!({"type": "user"}));
        rec.record(Dir::Err, json!("warn line"));
        let chunk = "y".repeat(MAX_STR_CHARS);
        for _ in 0..200 {
            rec.record(
                Dir::Out,
                json!({"type": "stream_event", "a": chunk, "b": chunk}),
            );
        }
        rec.note(json!({"verdict": "PASS", "reason": "r"}));
        let got = lines(&buf);
        assert_eq!(got[0]["dir"], "in");
        assert_eq!(got[1]["data"], "warn line");
        let last = got.last().expect("last");
        assert_eq!(last["dir"], "note");
        assert_eq!(last["data"]["verdict"], "PASS");
        let mut prev = 0;
        for entry in &got {
            let t = entry["t_ms"].as_u64().expect("t_ms");
            assert!(t >= prev);
            prev = t;
        }
        let total: usize = buf.0.lock().expect("lock").len();
        assert!(total < 200 * 1024, "file too large: {total}");
        assert!(got.iter().any(|e| {
            e["data"]
                .as_str()
                .is_some_and(|s| s.starts_with("<truncated"))
        }));
    }
}
