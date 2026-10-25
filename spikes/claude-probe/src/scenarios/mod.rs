//! Сценарии этапа 0. Каждый пишет свой файл доказательств и итоговый verdict.

pub mod background;
pub mod delivery_ack;
pub mod diagnostics;
pub mod image_input;
pub mod interrupt;
pub mod multi_input;
pub mod permissions;
pub mod question;
pub mod resume;
pub mod service_failure;
pub mod start_stream;
pub mod tools;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::broker::{Broker, PermissionSeen, Policy};
use crate::config::{self, Config};
use crate::correlate::{InputLedger, ToolLedger};
use crate::evidence::Recorder;
use crate::process::{ClaudeProcess, SpawnSpec};
use crate::protocol::{self, Kind};

pub const TURN_TIMEOUT: Duration = Duration::from_secs(150);

pub const ALL: &[&str] = &[
    "diagnostics",
    "start_stream",
    "multi_input",
    "delivery_ack",
    "tools",
    "permissions",
    "question",
    "interrupt",
    "resume",
    "service_failure",
    "background",
    "image_input",
];

pub async fn run(name: &str, cfg: &Config) -> anyhow::Result<()> {
    match name {
        "diagnostics" => diagnostics::run(cfg).await,
        "start_stream" => start_stream::run(cfg).await,
        "multi_input" => multi_input::run(cfg).await,
        "delivery_ack" => delivery_ack::run(cfg).await,
        "tools" => tools::run(cfg).await,
        "permissions" => permissions::run(cfg).await,
        "question" => question::run(cfg).await,
        "interrupt" => interrupt::run(cfg).await,
        "resume" => resume::run(cfg).await,
        "service_failure" => service_failure::run(cfg).await,
        "background" => background::run(cfg).await,
        "image_input" => image_input::run(cfg).await,
        other => anyhow::bail!("unknown scenario: {other}"),
    }
}

pub struct Ctx {
    pub cfg: Config,
    pub name: String,
    pub cwd: PathBuf,
    pub mcp_config_path: PathBuf,
    pub rec: Arc<Recorder>,
    pub broker: Broker,
    pub perms: mpsc::UnboundedReceiver<PermissionSeen>,
    pub turns: u32,
}

impl Ctx {
    pub async fn new(cfg: &Config, name: &str, policy: Policy) -> anyhow::Result<Self> {
        let cwd = cfg.scratch_root.join(name);
        fresh_dir(&cwd)?;
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&cwd)
            .status()?;
        anyhow::ensure!(git.success(), "git init failed");
        let ipc_dir = cfg.scratch_root.join(".ipc").join(name);
        fresh_dir(&ipc_dir)?;
        let socket = ipc_dir.join("perm.sock");
        let mcp_config_path = ipc_dir.join("mcp.json");
        std::fs::write(&mcp_config_path, cfg.mcp_config(&socket).to_string())?;

        let rec = Recorder::create(&cfg.evidence_dir.join(format!("{name}.jsonl")))?;
        rec.note(json!({"scenario": name, "header": {
            "cli": cfg.claude,
            "model": cfg.model,
            "effort": cfg.effort,
            "cwd": cwd,
            "unix_time": unix_time(),
            "env_allowlist": cfg.child_env(&[]).iter().map(|(k, v)| json!({k: v})).collect::<Vec<_>>(),
        }}));
        let (broker, perms) = Broker::start(&socket, rec.clone(), policy).await?;
        Ok(Self {
            cfg: cfg.clone(),
            name: name.to_owned(),
            cwd,
            mcp_config_path,
            rec,
            broker,
            perms,
            turns: 0,
        })
    }

    pub fn base_args(&self) -> Vec<String> {
        self.cfg.base_args(&self.mcp_config_path)
    }

    pub fn spawn_args(
        &self,
        args: Vec<String>,
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<Session> {
        let spec = SpawnSpec {
            program: self.cfg.claude.clone(),
            args,
            cwd: self.cwd.clone(),
            env: self.cfg.child_env(extra_env),
        };
        let p = ClaudeProcess::spawn(&spec, self.rec.clone())?;
        Ok(Session::new(p, self.rec.clone()))
    }

    pub fn spawn(
        &self,
        extra_args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<Session> {
        let mut args = self.base_args();
        args.extend(extra_args.iter().map(|s| (*s).to_owned()));
        self.spawn_args(args, extra_env)
    }

    pub fn step(&self, what: &str) -> u64 {
        self.rec.note(json!({"step": what}))
    }

    pub fn check(&self, name: &str, ok: bool, detail: Value) -> bool {
        self.rec
            .note(json!({"check": name, "ok": ok, "detail": detail}));
        ok
    }

    /// Итог сценария; ведёт общий учёт потраченных ходов.
    pub fn finish(&self, verdict: &str, reason: &str) {
        self.rec.note(json!({"turns_used": self.turns}));
        self.rec.note(json!({"verdict": verdict, "reason": reason}));
        let ledger = self.cfg.scratch_root.join("turns.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ledger)
        {
            let _ = writeln!(
                f,
                "{}",
                json!({"scenario": self.name, "turns": self.turns, "verdict": verdict, "unix_time": unix_time()})
            );
        }
        println!("{}: {verdict} ({} turns) - {reason}", self.name, self.turns);
    }

    /// Выполнить подкоманду CLI без stream-json (диагностика).
    pub async fn exec(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> anyhow::Result<ExecResult> {
        let start = std::time::Instant::now();
        self.rec.note(json!({"exec": args}));
        let out = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new(&self.cfg.claude)
                .args(args)
                .current_dir(&self.cwd)
                .env_clear()
                .envs(self.cfg.child_env(extra_env))
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await??;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // Целый JSON пишем одним объектом, чтобы маскирование работало по ключам.
        match serde_json::from_str::<Value>(&stdout) {
            Ok(whole) => {
                self.rec.record(crate::evidence::Dir::Out, whole);
            }
            Err(_) => {
                for line in stdout.lines() {
                    self.rec
                        .record(crate::evidence::Dir::Out, Value::String(line.to_owned()));
                }
            }
        }
        for line in stderr.lines() {
            self.rec
                .record(crate::evidence::Dir::Err, Value::String(line.to_owned()));
        }
        let res = ExecResult {
            code: out.status.code(),
            stdout,
            ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        self.rec
            .note(json!({"exec_done": {"args": args, "code": res.code, "ms": res.ms}}));
        Ok(res)
    }
}

pub struct ExecResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub ms: u64,
}

fn fresh_dir(dir: &Path) -> anyhow::Result<()> {
    // Удаляем только собственный scratch-каталог пробы.
    anyhow::ensure!(
        dir.starts_with(config::SCRATCH_ROOT),
        "refusing to touch {dir:?}"
    );
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::DirBuilder::new().recursive(true).create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Состояние одного процесса CLI, собранное из его потока.
pub struct Session {
    pub p: ClaudeProcess,
    rec: Arc<Recorder>,
    pub inputs: InputLedger,
    pub tools: ToolLedger,
    pub init: Option<Value>,
    pub results: Vec<(u64, Value)>,
    pub deltas: Vec<(u64, String)>,
    pub system: Vec<(u64, Value)>,
    pub control: Vec<(u64, Value)>,
    pub assistants: Vec<(u64, Value)>,
    pub other: Vec<(u64, Value)>,
    /// UNDOCUMENTED `command_lifecycle`: (t_ms, command_uuid, state).
    pub lifecycle: Vec<(u64, String, String)>,
    /// Эхо пользовательских сообщений (`isReplay: true`).
    pub replays: Vec<(u64, Value)>,
    /// Прочие user-кадры (результаты инструментов и т. п.).
    pub users: Vec<(u64, Value)>,
    pub tool_inputs: protocol::ToolInputAssembler,
    isolation_checked: bool,
}

pub struct TurnOutcome {
    pub uuid: String,
    pub written_ms: u64,
    pub result: Option<Value>,
}

impl Session {
    fn new(p: ClaudeProcess, rec: Arc<Recorder>) -> Self {
        Self {
            p,
            rec,
            inputs: InputLedger::default(),
            tools: ToolLedger::default(),
            init: None,
            results: Vec::new(),
            deltas: Vec::new(),
            system: Vec::new(),
            control: Vec::new(),
            assistants: Vec::new(),
            other: Vec::new(),
            lifecycle: Vec::new(),
            replays: Vec::new(),
            users: Vec::new(),
            tool_inputs: protocol::ToolInputAssembler::default(),
            isolation_checked: false,
        }
    }

    pub fn session_id(&self) -> Option<String> {
        self.init
            .as_ref()
            .and_then(protocol::session_id)
            .or_else(|| {
                self.results
                    .last()
                    .and_then(|(_, r)| protocol::session_id(r))
            })
            .map(str::to_owned)
    }

    /// Обработать все кадры, пока `stop` не вернёт true или не выйдет время.
    pub async fn pump_until<F>(&mut self, timeout: Duration, mut stop: F) -> Option<(u64, Value)>
    where
        F: FnMut(u64, &Value) -> bool,
    {
        let Self {
            p,
            inputs,
            tools,
            init,
            results,
            deltas,
            system,
            control,
            assistants,
            other,
            lifecycle,
            replays,
            users,
            tool_inputs,
            ..
        } = self;
        let res = p
            .wait_for(timeout, |t, v| {
                inputs.observe(t, v);
                tools.observe(v);
                tool_inputs.observe(v);
                match protocol::classify(v) {
                    Kind::SystemInit => {
                        init.get_or_insert_with(|| v.clone());
                    }
                    Kind::System(_) => system.push((t, v.clone())),
                    Kind::StreamEvent => {
                        if let Some(d) = protocol::text_delta(v) {
                            deltas.push((t, d.to_owned()));
                        }
                    }
                    Kind::Assistant => assistants.push((t, v.clone())),
                    Kind::Result => results.push((t, v.clone())),
                    Kind::ControlRequest | Kind::ControlResponse => control.push((t, v.clone())),
                    Kind::Other(kind) => {
                        // UNDOCUMENTED: command_lifecycle (queued/started/completed по command_uuid).
                        if kind == "command_lifecycle" {
                            lifecycle.push((
                                t,
                                v["command_uuid"].as_str().unwrap_or_default().to_owned(),
                                v["state"].as_str().unwrap_or_default().to_owned(),
                            ));
                        }
                        other.push((t, v.clone()));
                    }
                    Kind::User => {
                        if protocol::is_replay(v) {
                            replays.push((t, v.clone()));
                        } else {
                            users.push((t, v.clone()));
                        }
                    }
                }
                stop(t, v)
            })
            .await;
        self.maybe_check_isolation();
        res
    }

    pub async fn pump_for(&mut self, window: Duration) {
        let _ = self.pump_until(window, |_, _| false).await;
    }

    fn maybe_check_isolation(&mut self) {
        if self.isolation_checked {
            return;
        }
        let Some(init) = &self.init else { return };
        self.isolation_checked = true;
        let (ok, details) = config::check_isolation(init, &config::user_skill_names());
        self.rec
            .note(json!({"isolation": {"ok": ok, "details": details}}));
    }

    pub fn isolation_ok(&self) -> Option<bool> {
        let init = self.init.as_ref()?;
        Some(config::check_isolation(init, &config::user_skill_names()).0)
    }

    pub async fn send_user(&mut self, text: &str) -> anyhow::Result<(String, u64)> {
        let id = new_uuid();
        let t = self.p.send(&protocol::user_message(&id, text)).await?;
        self.inputs.written(&id, t);
        Ok((id, t))
    }

    pub async fn send_user_value(&mut self, id: &str, v: &Value) -> anyhow::Result<u64> {
        let t = self.p.send(v).await?;
        self.inputs.written(id, t);
        Ok(t)
    }

    /// Ждать следующий result (после уже полученных).
    pub async fn wait_next_result(&mut self, timeout: Duration) -> Option<(u64, Value)> {
        self.pump_until(timeout, |_, v| protocol::classify(v) == Kind::Result)
            .await
    }

    /// Ввод и ожидание итога именно этого хода.
    pub async fn turn(&mut self, text: &str, timeout: Duration) -> anyhow::Result<TurnOutcome> {
        let (uuid, written_ms) = self.send_user(text).await?;
        let got = self.wait_next_result(timeout).await;
        Ok(TurnOutcome {
            uuid,
            written_ms,
            result: got.map(|(_, v)| v),
        })
    }

    /// Гигиена потока: stderr, не-JSON строки stdout, эхо чужих UUID.
    pub fn hygiene_note(&self) -> Value {
        json!({
            "stderr_lines": self.p.stderr_lines.iter().map(|(t, l)| json!({"t_ms": t, "line": l})).collect::<Vec<_>>(),
            "non_json_stdout_lines": self.p.non_json_lines.len(),
            "stdout_closed_ms": self.p.stdout_closed_ms,
            "echoes_of_uuids_we_did_not_send": self.inputs.foreign_echoes(),
        })
    }

    pub fn delivery_note(&self, uuid: &str) -> Value {
        let lifecycle: Vec<Value> = self
            .lifecycle
            .iter()
            .filter(|(_, id, _)| id == uuid)
            .map(|(t, _, state)| json!({"state": state, "t_ms": t}))
            .collect();
        match self.inputs.get(uuid) {
            Some(e) => json!({
                "uuid": uuid,
                "written_ms": e.written_ms,
                "echo_ms": e.echo_ms,
                "first_reply_ms": e.first_reply_ms,
                "result_ms": e.result_ms,
                "delivery": format!("{:?}", e.delivery()),
                "undocumented_command_lifecycle": lifecycle,
            }),
            None => {
                json!({"uuid": uuid, "delivery": "unknown", "undocumented_command_lifecycle": lifecycle})
            }
        }
    }

    pub fn replay_count(&self, uuid: &str) -> usize {
        self.replays
            .iter()
            .filter(|(_, v)| protocol::uuid(v) == Some(uuid))
            .count()
    }

    /// Эхо нашего ввода: UUID сохранён, текст совпадает с отправленным.
    pub fn replay_matches(&self, uuid: &str, sent: &Value) -> bool {
        self.replays.iter().any(|(_, v)| {
            protocol::uuid(v) == Some(uuid) && v["message"]["content"] == sent["message"]["content"]
        })
    }

    pub fn text_since(&self, t_ms: u64) -> String {
        self.deltas
            .iter()
            .filter(|(t, _)| *t >= t_ms)
            .map(|(_, d)| d.as_str())
            .collect()
    }

    pub fn final_text_since(&self, t_ms: u64) -> String {
        self.assistants
            .iter()
            .filter(|(t, _)| *t >= t_ms)
            .map(|(_, v)| protocol::assistant_text(v))
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Краткая сводка result-кадра для заметок.
pub fn result_summary(r: &Value) -> Value {
    json!({
        "subtype": r["subtype"],
        "is_error": r["is_error"],
        "stop_reason": r["stop_reason"],
        "terminal_reason": r["terminal_reason"],
        "result": r["result"],
        "errors": r["errors"],
        "api_error_status": r["api_error_status"],
        "num_turns": r["num_turns"],
        "duration_ms": r["duration_ms"],
        "duration_api_ms": r["duration_api_ms"],
        "total_cost_usd": r["total_cost_usd"],
        "usage": r["usage"],
        "user_message_uuid": r["user_message_uuid"],
        "user_message_uuids": r["user_message_uuids"],
        "permission_denials": r["permission_denials"],
        "queued_turn_count": r["queued_turn_count"],
        "result_index": r["result_index"],
        "session_id": r["session_id"],
    })
}
