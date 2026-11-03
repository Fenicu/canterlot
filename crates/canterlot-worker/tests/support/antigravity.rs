//! Обвязка тестов Antigravity: обёртка над `fake-agy`, журнал кадров и сторож процессов.

#![allow(dead_code)]

use canterlot_worker::adapter::antigravity::replay::Script;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
use tempfile::TempDir;

#[path = "fake_exec.rs"]
pub mod fake_exec;

pub struct Fake {
    pub dir: TempDir,
    pub exe: PathBuf,
    pub script: PathBuf,
    log: PathBuf,
}

fn state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 1..].trim_start().chars().next()
}

impl Fake {
    pub fn new(mut script: Script) -> Self {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("frames.jsonl");
        script.log = Some(log.clone());
        let path = dir.path().join("script.json");
        fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
        let exe = fake_exec::write_executable(
            &dir.path().join("agy"),
            format!(
                "#!/bin/sh\nunset PWD SHLVL OLDPWD _\nexec '{}' --script '{}' \"$@\"\n",
                env!("CARGO_BIN_EXE_fake-agy"),
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

    pub fn rewrite(&self, mut script: Script) {
        script.log = Some(self.log.clone());
        fs::write(&self.script, serde_json::to_vec(&script).unwrap()).unwrap();
    }

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
            .expect("fake-agy did not boot")
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
        panic!("fake-agy survived: {:?}", self.pids());
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn exec_dir(&self) -> PathBuf {
        let dir = self.data_dir().join("executions/one");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn workdir(&self) -> PathBuf {
        let dir = self.dir.path().join("work");
        fs::create_dir_all(&dir).unwrap();
        dir
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

pub fn calls<'a>(requests: &'a [Value], method: &str) -> Vec<&'a Value> {
    requests
        .iter()
        .filter(|entry| entry["dir"] == "in" && entry["frame"]["method"].as_str() == Some(method))
        .collect()
}
