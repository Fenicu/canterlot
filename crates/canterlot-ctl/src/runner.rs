use std::{
    process::Stdio,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::{io::AsyncWriteExt, process::Command};

use crate::error::CtlError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    pub as_user: Option<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn failed(status: i32, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }

    pub fn ok(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, cmd: Cmd) -> Result<Output, CtlError>;
}

pub struct SystemRunner;

/// Каталог пользовательского менеджера нужен только для `systemctl --user`.
async fn runtime_dir(user: &str) -> Option<String> {
    let output = Command::new("id").args(["-u", user]).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let uid = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if uid.is_empty() {
        return None;
    }
    Some(format!("/run/user/{uid}"))
}

#[async_trait]
impl Runner for SystemRunner {
    async fn run(&self, cmd: Cmd) -> Result<Output, CtlError> {
        let mut command = match &cmd.as_user {
            Some(user) => {
                let mut command = Command::new("runuser");
                command.args(["-u", user, "--", &cmd.program]);
                command.args(&cmd.args);
                command
            }
            None => {
                let mut command = Command::new(&cmd.program);
                command.args(&cmd.args);
                command
            }
        };
        command.envs(
            cmd.env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        if cmd.program == "systemctl"
            && cmd.args.first().map(String::as_str) == Some("--user")
            && let Some(user) = &cmd.as_user
            && let Some(dir) = runtime_dir(user).await
        {
            command.env("XDG_RUNTIME_DIR", dir);
        }
        let mut child = command
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(bytes) = cmd.stdin
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin.write_all(&bytes).await?;
        }
        let output = child.wait_with_output().await?;
        Ok(Output {
            status: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Script {
    program: String,
    args: Vec<String>,
    output: Output,
}

type Responder = Box<dyn Fn(&Cmd) -> Option<Output> + Send + Sync>;

#[derive(Default)]
struct FakeState {
    responders: Vec<Responder>,
    scripts: Vec<Script>,
    default: Output,
    calls: Vec<Cmd>,
}

/// Заготовки ответов по префиксу `[program, args…]` и журнал вызовов.
#[derive(Clone, Default)]
pub struct FakeRunner {
    state: Arc<Mutex<FakeState>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn script(&self, program: &str, args: &[&str], output: Output) {
        let mut state = self.state.lock().unwrap();
        state.scripts.push(Script {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
            output,
        });
    }

    /// Ответ по произвольному условию (stdin, пользователь); проверяется раньше префиксов.
    pub fn respond(&self, f: impl Fn(&Cmd) -> Option<Output> + Send + Sync + 'static) {
        self.state.lock().unwrap().responders.push(Box::new(f));
    }

    pub fn set_default(&self, output: Output) {
        self.state.lock().unwrap().default = output;
    }

    pub fn calls(&self) -> Vec<Cmd> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn called(&self, program: &str) -> bool {
        self.calls().iter().any(|cmd| cmd.program == program)
    }
}

#[async_trait]
impl Runner for FakeRunner {
    async fn run(&self, cmd: Cmd) -> Result<Output, CtlError> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(cmd.clone());
        for responder in &state.responders {
            if let Some(output) = responder(&cmd) {
                return Ok(output);
            }
        }
        for script in &state.scripts {
            if script.program == cmd.program
                && cmd.args.len() >= script.args.len()
                && cmd.args[..script.args.len()] == script.args[..]
            {
                return Ok(script.output.clone());
            }
        }
        Ok(state.default.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
            stdin: None,
            as_user: None,
            env: Vec::new(),
        }
    }

    #[tokio::test]
    async fn fake_runner_matches_prefix_and_records_calls() {
        let runner = FakeRunner::new();
        runner.script("docker", &["compose"], Output::ok("up"));
        let out = runner
            .run(cmd("docker", &["compose", "up", "-d"]))
            .await
            .unwrap();
        assert_eq!(out.stdout, b"up");
        assert_eq!(runner.calls().len(), 1);
        assert!(runner.called("docker"));
    }
}
