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
    /// Ответ `agy -p /usage --output-format json`.
    pub usage: Option<Value>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            version: "agy 1.4.12".into(),
            steps: Vec::new(),
            log: None,
            ignore_eof: false,
            usage: None,
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
        /// id запроса в записи: по нему `Respond` находит настоящий id клиента.
        #[serde(default)]
        recorded_id: Option<Value>,
    },
    Send {
        frame: Value,
        #[serde(default)]
        delay_ms: u64,
    },
    /// Ответ на уже прочитанный запрос; порядок кадров сохраняется как в записи.
    Respond {
        recorded_id: Value,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
    },
    Delay {
        ms: u64,
    },
    /// Ответ нашего клиента на запрос сервера (у него нет `method`).
    ExpectReply {
        id: Value,
        #[serde(default)]
        result: Option<Value>,
    },
    /// Дочерний процесс инструмента: переживает CLI и виден группе исполнения.
    Spawn {
        argv: Vec<String>,
    },
    /// Процесс перестаёт читать stdin и висит до убийства: проверка срока записи.
    Hang,
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
            recorded_id: None,
        }
    }

    pub fn expect_error(method: &str, error: Value) -> Step {
        Step::Expect {
            method: method.into(),
            params: None,
            result: None,
            error: Some(error),
            delay_ms: 0,
            recorded_id: None,
        }
    }

    /// Уведомление клиента: ответа нет, поэтому сценарий только читает кадр.
    pub fn notification(method: &str) -> Step {
        Step::Expect {
            method: method.into(),
            params: None,
            result: None,
            error: None,
            delay_ms: 0,
            recorded_id: None,
        }
    }

    pub fn send(frame: Value) -> Step {
        Step::Send { frame, delay_ms: 0 }
    }

    pub fn send_after(frame: Value, delay_ms: u64) -> Step {
        Step::Send { frame, delay_ms }
    }

    /// Прочитать ответ клиента на запрос сервера с этим id.
    pub fn reply(id: Value) -> Step {
        Step::ExpectReply { id, result: None }
    }

    /// Сценарий из записи: немедленный ответ (рукопожатие) остаётся при запросе,
    /// а ответ на prompt отправляется там же, где был в записи — после потока
    /// уведомлений. Порядок кадров сохраняется.
    pub fn from_evidence(name: &str) -> Self {
        let mut script = Self::default();
        let frames: Vec<(String, Value)> = evidence_lines(name)
            .into_iter()
            .filter_map(|line| {
                let dir = line.get("dir")?.as_str()?.to_owned();
                let frame = serde_json::from_str::<Value>(line.get("line")?.as_str()?).ok()?;
                Some((dir, frame))
            })
            .collect();
        let mut index = 0;
        while index < frames.len() {
            let (dir, frame) = &frames[index];
            match dir.as_str() {
                "in" => {
                    if let Some(method) = frame["method"].as_str() {
                        let recorded_id = frame.get("id").filter(|id| !id.is_null()).cloned();
                        if let Some((next_dir, response)) = frames.get(index + 1)
                            && next_dir == "out"
                            && response.get("method").is_none()
                            && response["id"] == frame["id"]
                        {
                            script.steps.push(Step::Expect {
                                method: method.into(),
                                params: None,
                                result: response.get("result").cloned(),
                                error: response.get("error").cloned(),
                                delay_ms: 0,
                                recorded_id: None,
                            });
                            index += 2;
                            continue;
                        }
                        script.steps.push(Step::Expect {
                            method: method.into(),
                            params: None,
                            result: None,
                            error: None,
                            delay_ms: 0,
                            recorded_id,
                        });
                    } else if frame.get("id").is_some()
                        && (frame.get("result").is_some() || frame.get("error").is_some())
                    {
                        script.steps.push(Script::reply(frame["id"].clone()));
                    }
                }
                "out" => {
                    if frame.get("method").is_some() {
                        script.steps.push(Step::Send {
                            frame: frame.clone(),
                            delay_ms: 0,
                        });
                    } else {
                        script.steps.push(Step::Respond {
                            recorded_id: frame["id"].clone(),
                            result: frame.get("result").cloned(),
                            error: frame.get("error").cloned(),
                        });
                    }
                }
                _ => {}
            }
            index += 1;
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
    // Бесквотный `/usage`: тот же бинарник, но без ACP-сессии и хода.
    if args.iter().any(|arg| arg == "-p") && args.iter().any(|arg| arg == "/usage") {
        let usage = script.usage.clone().unwrap_or_else(|| json!({}));
        match serde_json::to_string(&usage) {
            Ok(text) => println!("{text}"),
            Err(_) => return ExitCode::from(70),
        }
        return ExitCode::SUCCESS;
    }
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut output = std::io::stdout();
    let mut recorded_ids: std::collections::HashMap<String, Value> =
        std::collections::HashMap::new();
    for step in &script.steps {
        match step {
            Step::Expect {
                method,
                params,
                result,
                error,
                delay_ms,
                recorded_id,
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
                if let Some(recorded) = recorded_id {
                    recorded_ids.insert(recorded.to_string(), frame["id"].clone());
                }
                if error.is_none() && result.is_none() {
                    // Уведомление: отвечать нечем.
                    continue;
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
            Step::Respond {
                recorded_id,
                result,
                error,
            } => {
                let Some(id) = recorded_ids.get(&recorded_id.to_string()) else {
                    log.entry(json!({"dir": "unexpected", "missing_response": recorded_id}));
                    return ExitCode::from(66);
                };
                let mut reply = json!({"jsonrpc": "2.0", "id": id});
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
            Step::ExpectReply { id, result } => {
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
                let matches_id = frame.get("method").is_none() && frame["id"] == *id;
                let matches_result = result
                    .as_ref()
                    .is_none_or(|expected| subset(expected, &frame["result"]));
                if !matches_id || !matches_result {
                    log.entry(json!({"dir": "unexpected", "frame": frame}));
                    return ExitCode::from(66);
                }
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
            Step::Spawn { argv } => {
                let Some((program, args)) = argv.split_first() else {
                    log.entry(json!({"dir": "invalid_spawn", "argv": argv}));
                    return ExitCode::from(66);
                };
                let child = std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                match child {
                    Ok(child) => log.entry(json!({
                        "dir": "child",
                        "pid": child.id(),
                        "argv": argv,
                    })),
                    Err(_) => return ExitCode::from(70),
                }
            }
            Step::Delay { ms } => std::thread::sleep(Duration::from_millis(*ms)),
            Step::Hang => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
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
    // Сценарий кончился: ответы клиента на уже отправленные запросы читаются и
    // журналируются, лишний запрос — ошибка сценария, EOF — чистый выход.
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let frame = match serde_json::from_str::<Value>(line.trim_end()) {
                    Ok(frame) => frame,
                    Err(_) => {
                        log.entry(json!({"dir": "in", "invalid": true}));
                        return ExitCode::from(66);
                    }
                };
                log.entry(json!({"dir": "in", "frame": frame}));
                let reply = frame.get("method").is_none()
                    && (frame.get("result").is_some() || frame.get("error").is_some());
                if !reply {
                    log.entry(json!({"dir": "unexpected", "frame": frame}));
                    return ExitCode::from(66);
                }
            }
            Err(_) => return ExitCode::from(70),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_expects(script: &Script) -> Vec<&Step> {
        script
            .steps
            .iter()
            .filter(
                |step| matches!(step, Step::Expect { method, .. } if method == "session/prompt"),
            )
            .collect()
    }

    #[test]
    fn evidence_keeps_the_update_frames_before_the_prompt_response() {
        let script = Script::from_evidence("t2_text");
        let prompts = prompt_expects(&script);
        assert_eq!(prompts.len(), 1);
        assert!(matches!(
            prompts[0],
            Step::Expect {
                result: None,
                recorded_id: Some(_),
                ..
            }
        ));
        let prompt = script
            .steps
            .iter()
            .position(
                |step| matches!(step, Step::Expect { method, .. } if method == "session/prompt"),
            )
            .unwrap();
        assert!(matches!(
            &script.steps[prompt + 1],
            Step::Send { frame, .. } if frame["params"]["update"]["sessionUpdate"] == "user_message_chunk"
        ));
        assert!(script.steps.iter().any(|step| matches!(
            step,
            Step::Respond {
                result: Some(_),
                ..
            }
        )));
    }

    #[test]
    fn both_prompts_of_the_steer_record_get_their_own_response() {
        let script = Script::from_evidence("t7-cancel-p3-steer");
        assert_eq!(prompt_expects(&script).len(), 3);
        let responses: Vec<&Step> = script
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    Step::Respond { result: Some(result), .. } if result.get("stopReason").is_some()
                )
            })
            .collect();
        assert_eq!(responses.len(), 3);
    }

    #[test]
    fn a_cancel_notification_is_read_without_a_reply() {
        let script = Script::from_evidence("t7c-cancel-perm");
        assert!(matches!(
            script.steps.iter().find(|step| matches!(
                step,
                Step::Expect { method, .. } if method == "session/cancel"
            )),
            Some(Step::Expect {
                result: None,
                error: None,
                ..
            })
        ));
    }

    #[test]
    fn a_reply_to_the_permission_request_is_verified() {
        let script = Script::from_evidence("t5-deny");
        assert!(script.steps.iter().any(|step| matches!(
            step,
            Step::ExpectReply { id, .. } if id == &json!("1")
        )));
    }
}
