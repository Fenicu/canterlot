//! Обвязка тестов OpenCode: обёртка над `fake-opencode`, журнал запросов и сторож процессов.
//!
//! Адаптер очищает окружение, поэтому сценарий приходит аргументом из обёртки; `exec`
//! сохраняет PID, как у настоящего сервера. Сторож ищет процессы по исполняемому файлу
//! и пути сценария и снимает их по PID, чужие серверы OpenCode не затрагиваются.

#![allow(dead_code)]

use canterlot_worker::adapter::opencode::replay::Script;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use tempfile::TempDir;

#[path = "fake_exec.rs"]
mod fake_exec;

pub struct Fake {
    pub dir: TempDir,
    pub exe: PathBuf,
    script: PathBuf,
    log: PathBuf,
}

fn state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..].trim_start().chars().next()
}

impl Fake {
    pub fn new(mut script: Script) -> Self {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("requests.jsonl");
        script.log = Some(log.clone());
        let path = dir.path().join("script.json");
        fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
        let exe = fake_exec::write_executable(
            &dir.path().join("opencode"),
            format!(
                "#!/bin/sh\nexec '{}' --script '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_fake-opencode"),
                path.display()
            ),
            0o700,
        );
        Self {
            dir,
            exe,
            script: path,
            log,
        }
    }

    /// Ответ, известный только во время прогона: идентификаторы создаёт адаптер.
    pub fn write_override(&self, method: &str, path: &str, body: &Value) {
        let entry = serde_json::json!({"method": method, "path": path, "body": body});
        let mut text =
            fs::read_to_string(self.script.with_extension("overrides")).unwrap_or_default();
        text.push_str(&format!("{entry}\n"));
        let temporary = self.script.with_extension("overrides.tmp");
        fs::write(&temporary, text).unwrap();
        fs::rename(temporary, self.script.with_extension("overrides")).unwrap();
    }

    /// Следующий запуск пойдёт по другому сценарию: так проверяется возобновление.
    pub fn rewrite(&self, mut script: Script) {
        script.log = Some(self.log.clone());
        fs::write(&self.script, serde_json::to_vec(&script).unwrap()).unwrap();
    }

    /// Запрос и строка запуска из журнала двойника, в порядке прихода.
    pub fn entries(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    pub fn boot(&self) -> Value {
        self.entries()
            .into_iter()
            .find(|entry| entry["boot"] == true)
            .expect("fake-opencode did not boot")
    }

    pub fn requests(&self) -> Vec<Value> {
        self.entries()
            .into_iter()
            .filter(|entry| entry["boot"] != true)
            .collect()
    }

    pub fn pids(&self) -> Vec<u32> {
        let needle = self.script.as_os_str().as_encoded_bytes();
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                let cmdline = fs::read(entry.path().join("cmdline")).ok()?;
                let found = cmdline
                    .split(|byte| *byte == 0)
                    .any(|argument| argument == needle);
                (found && !matches!(state(pid), Some('Z' | 'X'))).then_some(pid)
            })
            .collect()
    }

    pub async fn wait_gone(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.pids().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("fake-opencode survived: {:?}", self.pids());
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        for pid in self.pids() {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}

pub fn path_of(request: &Value) -> &str {
    request["path"].as_str().unwrap_or_default()
}

pub fn find<'a>(requests: &'a [Value], method: &str, tail: &str) -> Option<&'a Value> {
    requests
        .iter()
        .find(|request| request["method"] == method && path_of(request).ends_with(tail))
}

pub fn workdir(fake: &Fake) -> PathBuf {
    let dir = fake.dir.path().join("work");
    fs::create_dir_all(&dir).unwrap();
    dir
}
