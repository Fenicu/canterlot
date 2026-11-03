//! Двойник `agy acp` для тестов: ACP по сценарию из кадров этапа 0.
//!
//! Сценарий — упорядоченные ожидания входящих запросов и исходящие кадры:
//! `expect` ждёт метод (и, если заданы, поля `params`), затем отвечает `result`
//! или `error`; `send` пишет произвольный кадр; `delay` и `die` задают паузы и
//! точки смерти процесса. Журнал запуска и кадров пишется в `log`.
//!
//! Запуск идёт через обёртку теста, которая передаёт `--script`: адаптер очищает
//! окружение, а путь сценария должен пережить `env_clear`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Script {
    /// Строка `agy --version`.
    pub version: String,
    pub steps: Vec<Step>,
    pub log: Option<PathBuf>,
    /// EOF stdin не завершает: проверка `Force`.
    pub ignore_eof: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            version: "agy 1.4.12".into(),
            steps: Vec::new(),
            log: None,
            ignore_eof: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Expect {
        method: String,
        #[serde(default)]
        params: Option<Value>,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
        #[serde(default)]
        delay_ms: u64,
    },
    Send {
        frame: Value,
        #[serde(default)]
        delay_ms: u64,
    },
    Delay {
        ms: u64,
    },
    Die {
        code: i32,
        #[serde(default)]
        kill: bool,
    },
}

impl Script {
    pub fn expect(method: &str, result: Value) -> Step {
        Step::Expect {
            method: method.into(),
            params: None,
            result: Some(result),
            error: None,
            delay_ms: 0,
        }
    }

    pub fn expect_error(method: &str, error: Value) -> Step {
        Step::Expect {
            method: method.into(),
            params: None,
            result: None,
            error: Some(error),
            delay_ms: 0,
        }
    }

    /// Сценарий из записи: входящий запрос → ответ, уведомления после него — отдельными кадрами.
    pub fn from_evidence(name: &str) -> Self {
        let mut script = Self::default();
        let mut pending: Option<(Value, usize)> = None;
        for line in evidence_lines(name) {
            let frame = match line.get("line").and_then(Value::as_str) {
                Some(text) => match serde_json::from_str::<Value>(text) {
                    Ok(frame) => frame,
                    Err(_) => continue,
                },
                None => continue,
            };
            match line.get("dir").and_then(Value::as_str) {
                Some("in") => {
                    if let Some(method) = frame["method"].as_str() {
                        script.steps.push(Script::expect(method, json!({})));
                        pending = Some((frame["id"].clone(), script.steps.len() - 1));
                    }
                }
                Some("out") => {
                    let response = frame.get("method").is_none();
                    let matches_pending =
                        pending.as_ref().is_some_and(|(id, _)| *id == frame["id"]);
                    if response && matches_pending {
                        if let Some((_, index)) = pending.take()
                            && let Step::Expect { result, error, .. } = &mut script.steps[index]
                        {
                            if let Some(value) = frame.get("error") {
                                *error = Some(value.clone());
                            } else {
                                *result = Some(frame.get("result").cloned().unwrap_or(Value::Null));
                            }
                        }
                    } else if frame.get("method").is_some() {
                        script.steps.push(Step::Send { frame, delay_ms: 0 });
                    }
                }
                _ => {}
            }
        }
        if let Some((_, index)) = pending
            && let Step::Expect { result, .. } = &mut script.steps[index]
        {
            *result = None;
        }
        script
    }

    /// Оставить только первые `count` шагов: запись содержит лишние пробы.
    pub fn take(mut self, count: usize) -> Self {
        self.steps.truncate(count);
        self
    }
}

pub fn evidence_path(name: &str) -> PathBuf {
    let file = if name.ends_with(".jsonl") {
        name.to_owned()
    } else {
        format!("{name}.jsonl")
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/antigravity")
        .join(file)
}

pub fn evidence_lines(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(evidence_path(name)).expect("evidence file");
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

/// Кадр записи по id: `in`-запрос или `out`-ответ.
pub fn evidence_frame(name: &str, id: i64) -> Value {
    evidence_lines(name)
        .into_iter()
        .filter_map(|line| line.get("line").and_then(Value::as_str).map(str::to_owned))
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .find(|frame| {
            frame["id"] == id && (frame.get("result").is_some() || frame.get("error").is_some())
        })
        .expect("recorded frame")
}

/// `result` записанного ответа.
pub fn evidence_result(name: &str, id: i64) -> Value {
    evidence_frame(name, id)
        .get("result")
        .cloned()
        .expect("recorded result")
}

/// `session/list` в форме кадра: ключи записи `p1-session-list-summary.json`.
/// Заголовки и cwd владельца были вырезаны из evidence, поэтому значения тестовые.
pub fn session_list(ids: &[&str]) -> Value {
    json!({
        "sessions": ids.iter().map(|id| json!({
            "sessionId": id,
            "cwd": "/tmp/agy-stage0-kFuf",
            "title": "canterlot",
            "updatedAt": "2026-11-03T12:00:00Z",
        })).collect::<Vec<_>>(),
    })
}

pub fn main(args: Vec<String>) -> ExitCode {
    let script_path = args
        .iter()
        .position(|arg| arg == "--script")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);
    let script = script_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Script>(&text).ok())
        .unwrap_or_default();
    let mut log = Log(script.log.clone());
    if args.contains(&"--version".to_string()) {
        println!("{}", script.version);
        return ExitCode::SUCCESS;
    }
    log.boot(&args);
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut output = std::io::stdout();
    for step in &script.steps {
        match step {
            Step::Expect {
                method,
                params,
                result,
                error,
                delay_ms,
            } => {
                let mut line = String::new();
                match input.read_line(&mut line) {
                    Ok(0) => {
                        log.entry(json!({"dir": "eof"}));
                        return ExitCode::SUCCESS;
                    }
                    Ok(_) => {}
                    Err(_) => return ExitCode::from(70),
                }
                let frame = match serde_json::from_str::<Value>(line.trim_end()) {
                    Ok(frame) => frame,
                    Err(_) => {
                        log.entry(json!({"dir": "in", "invalid": true}));
                        return ExitCode::from(66);
                    }
                };
                log.entry(json!({"dir": "in", "frame": frame}));
                if frame["method"].as_str() != Some(method.as_str())
                    || params
                        .as_ref()
                        .is_some_and(|expected| !subset(expected, &frame["params"]))
                {
                    log.entry(json!({"dir": "unexpected", "frame": frame}));
                    return ExitCode::from(66);
                }
                if *delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(*delay_ms));
                }
                let mut reply = json!({"jsonrpc": "2.0", "id": frame["id"]});
                if let Some(error) = error {
                    reply["error"] = error.clone();
                } else {
                    reply["result"] = result.clone().unwrap_or_else(|| json!({}));
                }
                log.entry(json!({"dir": "out", "frame": reply}));
                if writeln!(output, "{reply}").is_err() {
                    return ExitCode::from(70);
                }
                let _ = output.flush();
            }
            Step::Send { frame, delay_ms } => {
                if *delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(*delay_ms));
                }
                log.entry(json!({"dir": "out", "frame": frame}));
                if writeln!(output, "{frame}").is_err() {
                    return ExitCode::from(70);
                }
                let _ = output.flush();
            }
            Step::Delay { ms } => std::thread::sleep(Duration::from_millis(*ms)),
            Step::Die { code, kill } => {
                let _ = output.flush();
                if *kill {
                    let _ = rustix::process::kill_process(
                        rustix::process::getpid(),
                        rustix::process::Signal::KILL,
                    );
                }
                return ExitCode::from(u8::try_from(*code).unwrap_or(1));
            }
        }
    }
    // Сценарий кончился: лишний запрос — ошибка сценария, EOF — чистый выход.
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) => {}
        Ok(_) => {
            let frame = serde_json::from_str::<Value>(line.trim_end()).unwrap_or(Value::Null);
            log.entry(json!({"dir": "unexpected", "frame": frame}));
            return ExitCode::from(66);
        }
        Err(_) => return ExitCode::from(70),
    }
    if script.ignore_eof {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    ExitCode::SUCCESS
}

fn subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected
            .iter()
            .all(|(key, value)| actual.get(key).is_some_and(|found| subset(value, found))),
        _ => expected == actual,
    }
}

struct Log(Option<PathBuf>);

impl Log {
    fn boot(&mut self, args: &[String]) {
        let env: BTreeMap<String, String> = std::env::vars()
            .filter(|(key, _)| {
                key == "GEMINI_FORCE_FILE_STORAGE"
                    || super::process::ENV_ALLOWLIST.contains(&key.as_str())
            })
            .collect();
        let mut keys: Vec<String> = std::env::vars().map(|(key, _)| key).collect();
        keys.sort();
        self.entry(json!({
            "boot": true,
            "program": std::env::current_exe().ok().map(|path| path.to_string_lossy().into_owned()),
            "args": args,
            "cwd": std::env::current_dir().ok().map(|path| path.to_string_lossy().into_owned()),
            "env_keys": keys,
            "env": env,
        }));
    }

    fn entry(&mut self, value: Value) {
        let Some(path) = &self.0 else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{value}");
        }
    }
}
