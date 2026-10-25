//! Запись доказательств сценария в JSONL: `{"t_ms", "dir", "data"}`.
//!
//! Перед записью значения проходят редактирование (идентификаторы аккаунта,
//! токены) и усечение длинных строк, чтобы файл оставался маленьким и без секретов.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

/// Максимальная длина одной строки-значения внутри записи.
pub const MAX_STRING: usize = 2000;
/// Максимальный размер сериализованной записи.
pub const MAX_RECORD: usize = 24 * 1024;

#[derive(Clone)]
pub struct Recorder {
    inner: Arc<Inner>,
}

struct Inner {
    start: Instant,
    path: PathBuf,
    file: Mutex<BufWriter<File>>,
    /// id ответов, тело которых заменяется заглушкой (сводка пишется в note).
    quiet: Mutex<HashSet<i64>>,
    turn_starts: AtomicU32,
    turn_steers: AtomicU32,
}

#[derive(Clone, Copy)]
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

impl Recorder {
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        Ok(Self {
            inner: Arc::new(Inner {
                start: Instant::now(),
                path: path.to_path_buf(),
                file: Mutex::new(BufWriter::new(file)),
                quiet: Mutex::new(HashSet::new()),
                turn_starts: AtomicU32::new(0),
                turn_steers: AtomicU32::new(0),
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.inner.start.elapsed().as_millis()
    }

    pub fn mark_quiet(&self, id: i64) {
        if let Ok(mut q) = self.inner.quiet.lock() {
            q.insert(id);
        }
    }

    fn is_quiet(&self, v: &Value) -> bool {
        let Some(id) = v.get("id").and_then(Value::as_i64) else {
            return false;
        };
        if v.get("method").is_some() {
            return false;
        }
        self.inner
            .quiet
            .lock()
            .map(|q| q.contains(&id))
            .unwrap_or(false)
    }

    /// Сколько раз отправлены `turn/start` и `turn/steer` (учёт расхода квоты).
    pub fn turn_counts(&self) -> (u32, u32) {
        (
            self.inner.turn_starts.load(Ordering::SeqCst),
            self.inner.turn_steers.load(Ordering::SeqCst),
        )
    }

    /// Строка протокола в указанном направлении: JSON, если разбирается, иначе строка.
    pub fn line(&self, dir: Dir, raw: &str) {
        if let Dir::In = dir {
            if raw.contains("\"method\":\"turn/start\"") {
                self.inner.turn_starts.fetch_add(1, Ordering::SeqCst);
            } else if raw.contains("\"method\":\"turn/steer\"") {
                self.inner.turn_steers.fetch_add(1, Ordering::SeqCst);
            }
        }
        let data = match serde_json::from_str::<Value>(raw) {
            Ok(v) if self.is_quiet(&v) => json!({
                "id": v.get("id").cloned().unwrap_or(Value::Null),
                "result": "<omitted: summarized in a following note>"
            }),
            Ok(v) => v,
            Err(_) => Value::String(raw.to_string()),
        };
        self.write(dir, data);
    }

    pub fn note(&self, data: Value) {
        self.write(Dir::Note, data);
    }

    pub fn step(&self, text: &str) {
        self.write(Dir::Note, json!({ "step": text }));
    }

    pub fn verdict(&self, verdict: &str, reason: &str) {
        self.write(Dir::Note, json!({ "verdict": verdict, "reason": reason }));
    }

    pub fn write(&self, dir: Dir, data: Value) {
        let data = truncate(redact(data), MAX_STRING);
        let mut rec =
            json!({ "t_ms": self.elapsed_ms() as u64, "dir": dir.as_str(), "data": data });
        let mut line = rec.to_string();
        if line.len() > MAX_RECORD {
            rec["data"] = Value::String(format!("<truncated {} bytes>", line.len()));
            line = rec.to_string();
        }
        if let Ok(mut f) = self.inner.file.lock() {
            // ошибка записи evidence не должна ронять сценарий: она будет видна по пустому файлу
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

const SENSITIVE_KEYS: &[&str] = &[
    "email",
    "chatgptaccountid",
    "accountid",
    "installationid",
    "accesstoken",
    "idtoken",
    "apikey",
    "api_key",
    "authorization",
    "password",
    "secret",
    "bearer",
];

fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|s| k == *s)
        || (k.ends_with("token") && k != "tokenusage" && !k.ends_with("_tokens"))
}

/// Заменяет значения чувствительных ключей и подозрительные строки.
/// Логические значения (например, `refreshToken: false`) сохраняются.
pub fn redact(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, val)| {
                    let val = if is_sensitive_key(&k) && matches!(val, Value::String(_)) {
                        Value::String("<redacted>".into())
                    } else {
                        redact(val)
                    };
                    (k, val)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        Value::String(s) => Value::String(redact_str(&s)),
        other => other,
    }
}

fn redact_str(s: &str) -> String {
    let mut out = s.to_string();
    for marker in ["Bearer ", "sk-", "eyJ"] {
        let mut search_from = 0;
        while let Some(rel) = out[search_from..].find(marker) {
            let start = search_from + rel;
            let tail_start = start + marker.len();
            let end = out[tail_start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-_.".contains(c)))
                .map(|e| tail_start + e)
                .unwrap_or(out.len());
            // короткие совпадения ("sk-" в обычном тексте) не трогаем
            if end - tail_start >= 16 {
                out.replace_range(start..end, "<redacted>");
                search_from = start + "<redacted>".len();
            } else {
                search_from = tail_start;
            }
        }
    }
    out
}

/// Усекает длинные строки с явной пометкой `<truncated N bytes>`.
pub fn truncate(v: Value, max: usize) -> Value {
    match v {
        Value::String(s) if s.len() > max => {
            let mut cut = max;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            Value::String(format!("{}<truncated {} bytes>", &s[..cut], s.len() - cut))
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, val)| (k, truncate(val, max)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(|i| truncate(i, max)).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_account_identifiers_but_keeps_booleans() {
        let v = json!({
            "account": {"type": "chatgpt", "email": "a@b.c", "planType": "plus"},
            "workspaceRouting": {"chatgptAccountId": "8b4b-..."},
            "refreshToken": false,
            "installationId": "8e33",
            "tokenUsage": {"total": {"totalTokens": 5}}
        });
        let r = redact(v);
        assert_eq!(r["account"]["email"], "<redacted>");
        assert_eq!(r["account"]["planType"], "plus");
        assert_eq!(r["workspaceRouting"]["chatgptAccountId"], "<redacted>");
        assert_eq!(r["refreshToken"], false);
        assert_eq!(r["installationId"], "<redacted>");
        assert_eq!(r["tokenUsage"]["total"]["totalTokens"], 5);
    }

    #[test]
    fn redacts_bearer_and_key_patterns_in_text() {
        let s = "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz' sk-or-v1-0123456789abcdef0123";
        let r = redact_str(s);
        assert!(!r.contains("abcdefghijklmnop"));
        assert!(!r.contains("0123456789abcdef0123"));
        assert!(redact_str("task-list sk-1").contains("sk-1"));
    }

    #[test]
    fn truncates_on_char_boundary_with_marker() {
        let s = "ж".repeat(10); // 20 байт
        let t = truncate(json!({"x": s}), 5);
        let out = t["x"].as_str().expect("string");
        assert!(out.starts_with("жж"));
        assert!(out.ends_with("<truncated 16 bytes>"));
    }

    #[test]
    fn quiet_responses_are_replaced() {
        let dir = std::env::temp_dir().join(format!("codex-probe-test-{}", std::process::id()));
        let path = dir.join("quiet.jsonl");
        let rec = Recorder::create(&path).expect("create");
        rec.mark_quiet(5);
        rec.line(Dir::Out, r#"{"id":5,"result":{"config":{"secret":"x"}}}"#);
        rec.line(Dir::Out, r#"{"id":6,"result":{"ok":true}}"#);
        rec.line(Dir::Out, "not json");
        let text = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).expect("jsonl"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0]["data"]["result"]
                .as_str()
                .is_some_and(|s| s.starts_with("<omitted"))
        );
        assert_eq!(lines[1]["data"]["result"]["ok"], true);
        assert_eq!(lines[2]["data"], "not json");
        assert_eq!(lines[2]["dir"], "out");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
