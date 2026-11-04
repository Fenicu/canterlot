#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Контракт адаптера Antigravity на `fake-agy`: кадры этапа 0, запуск, resume и каталог.

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, CloseMode, CompactOutcome,
        DecisionOutcome, InstructionMode, InstructionSpec, InterruptOutcome, LocalFile, Phase,
        SettingsChange, StartSpec, SteerOutcome, Transmission, TransmitOutcome,
        antigravity::{
            AntigravityAdapter, catalog, input,
            process::ENV_ALLOWLIST,
            replay::{Script, Step, evidence_result, session_list},
        },
        canterlot_mcp::relay_hello,
    },
    config::AntigravityConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[path = "support/antigravity.rs"]
mod antigravity;
use antigravity::{Fake, calls};

const WAIT: Duration = Duration::from_secs(10);

fn instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Clean,
        text: "canterlot test append".into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn spec(model: &str, effort: &str, mode: &str) -> StartSpec {
    StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: PathBuf::new(),
        native_conversation_id: None,
        profile: "antigravity".into(),
        model: model.into(),
        effort: effort.into(),
        permission_mode: mode.into(),
        instructions: instructions(),
        isolate_mcp: false,
    }
}

fn init_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[0].clone()
}

fn new_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[2].clone()
}

fn set_model_step() -> Step {
    Script::from_evidence("p14-model-effort.jsonl").steps[3].clone()
}

fn set_effort_step() -> Step {
    Script::from_evidence("p14-model-effort.jsonl").steps[7].clone()
}

struct Bench {
    fake: Fake,
    adapter: AntigravityAdapter,
    group: TreeContainment,
    spec: StartSpec,
    exec_dir: PathBuf,
}

impl Bench {
    fn new(script: Script, model: &str, effort: &str, mode: &str) -> Self {
        Self::with_timing(
            script,
            model,
            effort,
            mode,
            WAIT,
            Duration::from_secs(5),
            Duration::from_secs(60),
        )
    }

    fn with_timing(
        script: Script,
        model: &str,
        effort: &str,
        mode: &str,
        deadline: Duration,
        interrupt: Duration,
        stall: Duration,
    ) -> Self {
        Self::with_questions(
            script, model, effort, mode, deadline, interrupt, stall, true,
        )
    }

    fn new_without_questions(script: Script) -> Self {
        Self::with_questions(
            script,
            "",
            "",
            "bypass",
            WAIT,
            Duration::from_secs(5),
            Duration::from_secs(60),
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_questions(
        script: Script,
        model: &str,
        effort: &str,
        mode: &str,
        deadline: Duration,
        interrupt: Duration,
        stall: Duration,
        questions: bool,
    ) -> Self {
        let fake = Fake::new(script);
        let exec_dir = fake.exec_dir();
        let cfg = AntigravityConfig {
            enabled: true,
            binary: Some(fake.exe.clone()),
            data_dir: Some(fake.data_dir()),
        };
        let adapter = AntigravityAdapter::new(cfg, exec_dir.clone(), deadline)
            .with_ready_deadline(WAIT)
            .with_interrupt_deadline(interrupt)
            .with_turn_stall_deadline(stall)
            .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker")))
            .with_questions_enabled(questions);
        let mut start = spec(model, effort, mode);
        start.workdir = fake.workdir();
        Self {
            fake,
            adapter,
            group: TreeContainment::new().unwrap(),
            spec: start,
            exec_dir,
        }
    }

    async fn start(&mut self) -> canterlot_worker::adapter::Started {
        self.adapter.start(&self.spec, &self.group).await.unwrap()
    }

    async fn close(&mut self) {
        let _ = self.adapter.close(CloseMode::Graceful, &self.group).await;
        self.fake.wait_gone().await;
    }
}

fn handshake() -> Vec<Step> {
    vec![init_step(), new_step()]
}

fn from_prompt(name: &str) -> Vec<Step> {
    let script = Script::from_evidence(name);
    let index = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::Expect { method, .. } if method == "session/prompt"))
        .expect("prompt step");
    script.steps[index..].to_vec()
}

fn send(text: &str) -> Transmission {
    Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: text.into(),
        files: Vec::new(),
        settings: None,
    }
}

fn deferred(method: &str, recorded: i64) -> Step {
    Step::Expect {
        method: method.into(),
        params: None,
        result: None,
        error: None,
        delay_ms: 0,
        recorded_id: Some(json!(recorded)),
    }
}

fn respond(recorded: i64, result: Value) -> Step {
    Step::Respond {
        recorded_id: json!(recorded),
        result: Some(result),
        error: None,
    }
}

fn respond_error(recorded: i64, error: Value) -> Step {
    Step::Respond {
        recorded_id: json!(recorded),
        result: None,
        error: Some(error),
    }
}

fn end_turn(usage: Option<Value>) -> Value {
    let mut result = json!({"stopReason": "end_turn"});
    if let Some(usage) = usage {
        result["usage"] = usage;
    }
    result
}

fn cancelled(usage: Option<Value>) -> Value {
    let mut result = json!({"stopReason": "cancelled"});
    if let Some(usage) = usage {
        result["usage"] = usage;
    }
    result
}

fn usage(input: u64, output: u64, thought: u64) -> Value {
    json!({
        "inputTokens": input,
        "outputTokens": output,
        "cachedReadTokens": 0,
        "cachedWriteTokens": 0,
        "thoughtTokens": thought,
    })
}

fn update(value: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "s", "update": value},
    })
}

fn user_chunk(text: &str) -> Value {
    update(json!({
        "sessionUpdate": "user_message_chunk",
        "content": {"type": "text", "text": text},
    }))
}

fn agent_chunk(text: &str) -> Value {
    update(json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text},
    }))
}

fn thought_chunk(text: &str) -> Value {
    update(json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": {"type": "text", "text": text},
    }))
}

fn tool_call(id: &str, name: &str, raw: Value) -> Value {
    update(json!({
        "sessionUpdate": "tool_call",
        "toolCallId": id,
        "name": name,
        "status": "in_progress",
        "kind": "execute",
        "title": name,
        "rawInput": raw,
    }))
}

fn tool_update(id: &str, status: &str, raw_output: Option<Value>) -> Value {
    let mut value = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": id,
        "status": status,
    });
    if let Some(output) = raw_output {
        value["rawOutput"] = output;
    }
    update(value)
}

async fn drain(adapter: &mut AntigravityAdapter) -> Vec<AdapterEvent> {
    let mut out = Vec::new();
    while let Ok(event) =
        tokio::time::timeout(Duration::from_millis(400), adapter.next_event()).await
    {
        out.push(event);
    }
    out
}

fn finished(events: &[AdapterEvent]) -> Vec<Uuid> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::RunFinished {
                input_command_id, ..
            } => Some(*input_command_id),
            _ => None,
        })
        .collect()
}

fn usage_of(events: &[AdapterEvent]) -> Vec<canterlot_worker::adapter::ModelUsage> {
    events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::RunFinished { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn kinds(events: &[AdapterEvent]) -> Vec<BlockKind> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted { kind, .. } => Some(kind.clone()),
            _ => None,
        })
        .collect()
}

fn allow() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    }
}

fn deny() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Deny,
        message: Some("no".into()),
    }
}

fn answer(question: &str, value: &str) -> DecisionPayload {
    let mut answers = BTreeMap::new();
    answers.insert(question.to_owned(), AnswerValue::Single(value.into()));
    DecisionPayload::Question { answers }
}

/// Первый открытый запрос нужного вида; ходы до него остаются в списке.
async fn wait_request(
    adapter: &mut AntigravityAdapter,
    wanted: &str,
) -> (String, Value, Vec<AdapterEvent>) {
    let mut seen = Vec::new();
    let timeout = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(timeout, adapter.next_event())
            .await
            .expect("request did not open");
        if let AdapterEvent::RequestOpened {
            request_id,
            kind,
            payload,
        } = &event
            && kind == wanted
        {
            let (request_id, payload) = (request_id.clone(), payload.clone());
            seen.push(event);
            // Фаза открытия идёт сразу за карточкой; берём и её.
            if let Ok(next) =
                tokio::time::timeout(Duration::from_millis(200), adapter.next_event()).await
            {
                seen.push(next);
            }
            return (request_id, payload, seen);
        }
        seen.push(event);
    }
}

fn session_prompts(requests: &[Value]) -> Vec<&Value> {
    calls(requests, "session/prompt")
}

#[tokio::test]
async fn start_new_uses_the_recorded_session_and_pinned_copy() {
    let script = Script {
        steps: vec![init_step(), new_step(), set_model_step(), set_effort_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "gemini-3.8-flash-low", "low", "bypass");
    let started = bench.start().await;
    assert_eq!(
        started.native_conversation_id,
        "d41d6945-1310-44ef-89e6-42705ee12e27"
    );
    assert!(started.cli_pid > 0);

    let pinned = bench.fake.data_dir().join("antigravity/bin/agy-1.4.12");
    assert!(pinned.is_file(), "pinned copy missing");
    assert_eq!(
        std::fs::read_to_string(&pinned).unwrap(),
        std::fs::read_to_string(&bench.fake.exe).unwrap(),
        "pinned copy is the configured source binary"
    );
    let boot = bench.fake.boot();
    let args: Vec<&str> = boot["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // Обёртка теста добавляет `--script <файл>` перед аргументами CLI.
    let start = args.iter().position(|arg| *arg == "acp").expect("acp");
    assert_eq!(
        &args[start..],
        [
            "acp",
            "--log-file",
            bench.exec_dir.join("agy.log").to_string_lossy().as_ref(),
            "--disable-telemetry",
            "--dangerously-skip-permissions",
        ]
    );
    assert!(!args.iter().any(|arg| arg.starts_with("--gemini-dir")));
    assert!(!args.iter().any(|arg| arg.starts_with("--gemini_dir")));

    let requests = bench.fake.requests();
    let new = calls(&requests, "session/new");
    assert_eq!(new.len(), 1);
    assert_eq!(
        new[0]["frame"]["params"]["cwd"],
        bench.spec.workdir.to_string_lossy().as_ref()
    );
    assert_relay(&relay_of(&new[0]["frame"]), &bench.exec_dir);
    assert_eq!(
        new[0]["frame"]["params"]["_meta"]["antigravity"]["systemPrompt"]["append"],
        "canterlot test append"
    );
    let model = calls(&requests, "session/set_model");
    assert_eq!(
        model[0]["frame"]["params"]["modelId"],
        "gemini-3.8-flash-low"
    );
    let effort = calls(&requests, "session/set_config_option");
    assert_eq!(effort[0]["frame"]["params"]["configId"], "thought_level");
    assert_eq!(effort[0]["frame"]["params"]["value"], "low");
    assert!(calls(&requests, "session/load").is_empty());
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn rejected_model_setting_fails_start_with_the_32602_code() {
    let script = Script {
        steps: vec![
            init_step(),
            new_step(),
            Script::expect_error(
                "session/set_model",
                serde_json::json!({"code": -32602, "message": "Unknown model ID: no-such-model-xyz"}),
            ),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "no-such-model-xyz", "", "owner");
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(error, AdapterError::StartFailed("model_rejected")));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn rejected_effort_setting_fails_start_with_the_32602_code() {
    let script = Script {
        steps: vec![
            init_step(),
            new_step(),
            set_model_step(),
            Script::expect_error(
                "session/set_config_option",
                serde_json::json!({"code": -32602, "message": "Unknown config option: "}),
            ),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "gemini-3.8-flash-low", "max", "owner");
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::StartFailed("effort_rejected")
    ));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn env_is_an_allowlist_with_forced_file_storage() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    let boot = bench.fake.boot();
    let keys: Vec<&str> = boot["env_keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let allowed = ENV_ALLOWLIST
        .iter()
        .copied()
        .chain(["GEMINI_FORCE_FILE_STORAGE"]);
    for key in &keys {
        assert!(
            allowed.clone().any(|allowed| allowed == *key),
            "unexpected environment key leaked: {key}"
        );
    }
    assert_eq!(boot["env"]["GEMINI_FORCE_FILE_STORAGE"], "true");
    assert!(!keys.contains(&"OPENAI_API_KEY"));
    let args: Vec<&str> = boot["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!args.contains(&"--dangerously-skip-permissions"));
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

fn relay_of(frame: &Value) -> Value {
    let servers = frame["params"]["mcpServers"]
        .as_array()
        .unwrap_or_else(|| panic!("mcpServers array: {frame}"));
    assert_eq!(servers.len(), 1, "{frame}");
    servers[0].clone()
}

fn assert_relay(server: &Value, exec_dir: &std::path::Path) {
    assert_eq!(server["name"], "canterlot");
    assert_eq!(
        server["command"].as_str(),
        Some(env!("CARGO_BIN_EXE_canterlot-worker"))
    );
    let args: Vec<&str> = server["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(args[0], "mcp-relay");
    assert_eq!(args[1], "--socket");
    assert_eq!(
        args[2],
        exec_dir.join("mcp.sock").to_string_lossy(),
        "the relay talks to this session's socket"
    );
    assert_eq!(server["env"][0]["name"], "CANTERLOT_MCP_SECRET");
    assert_eq!(
        server["env"][0]["value"].as_str().map(str::len),
        Some(64),
        "the relay carries this session's secret"
    );
}

#[tokio::test]
async fn new_and_resume_pass_the_canterlot_mcp_relay() {
    let mut bench = Bench::new(
        Script {
            steps: vec![init_step(), new_step()],
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let requests = bench.fake.requests();
    assert_relay(
        &relay_of(&calls(&requests, "session/new")[0]["frame"]),
        &bench.exec_dir,
    );
    bench.close().await;

    let session_id = "7497af9f-8ce0-4bb6-9b42-f14ac3fd1890";
    let mut bench = Bench::new(
        Script {
            steps: vec![
                init_step(),
                Script::expect("session/list", session_list(&[session_id])),
                Script::expect("session/resume", evidence_result("t13b-resume.jsonl", 2)),
            ],
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    bench.spec.native_conversation_id = Some(session_id.into());
    bench.start().await;
    let requests = bench.fake.requests();
    assert_relay(
        &relay_of(&calls(&requests, "session/resume")[0]["frame"]),
        &bench.exec_dir,
    );
    bench.close().await;
}

#[tokio::test]
async fn an_agent_call_over_the_canterlot_relay_round_trips() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let mut bench = Bench::new(
        Script {
            steps: vec![init_step(), new_step()],
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let server = relay_of(&calls(&bench.fake.requests(), "session/new")[0]["frame"]);
    let secret = server["env"][0]["value"].as_str().unwrap().to_owned();
    let stream = UnixStream::connect(bench.exec_dir.join("mcp.sock"))
        .await
        .expect("broker socket");
    let (read, mut write) = stream.into_split();
    write
        .write_all(relay_hello(&secret).as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n")
        .await
        .unwrap();
    tokio::time::timeout(WAIT, reader.read_line(&mut line))
        .await
        .expect("initialize reply")
        .unwrap();
    assert!(line.contains("canterlot"), "{line}");

    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"canterlot_presets_list\",\"arguments\":{}}}\n")
        .await
        .unwrap();
    let event = tokio::time::timeout(WAIT, bench.adapter.next_event())
        .await
        .expect("agent call event");
    let (call_id, tool) = match event {
        AdapterEvent::AgentCall { call_id, tool, .. } => (call_id, tool),
        other => panic!("not an agent call: {other:?}"),
    };
    assert_eq!(tool, "presets.list");
    bench
        .adapter
        .agent_call_result(call_id, AgentCallResult::Ok(json!({"presets": []})))
        .await;
    line.clear();
    tokio::time::timeout(WAIT, reader.read_line(&mut line))
        .await
        .expect("tool reply")
        .unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], 2, "{reply}");
    assert!(
        reply["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("presets")),
        "the result goes back into the connection: {reply}"
    );
    bench.close().await;
}

#[tokio::test]
async fn resume_checks_session_list_and_never_uses_load() {
    let session_id = "7497af9f-8ce0-4bb6-9b42-f14ac3fd1890";
    let script = Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[session_id])),
            Script::expect("session/resume", evidence_result("t13b-resume.jsonl", 2)),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.spec.native_conversation_id = Some(session_id.into());
    let started = bench.start().await;
    assert_eq!(started.native_conversation_id, session_id);
    let requests = bench.fake.requests();
    assert_eq!(calls(&requests, "session/list").len(), 1);
    assert_eq!(calls(&requests, "session/resume").len(), 1);
    assert!(calls(&requests, "session/load").is_empty());
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn missing_session_in_list_fails_start() {
    let script = Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[])),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.spec.native_conversation_id = Some("7497af9f-8ce0-4bb6-9b42-f14ac3fd1890".into());
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::StartFailed("conversation not found")
    ));
    assert!(calls(&bench.fake.requests(), "session/resume").is_empty());
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn unknown_version_is_allowed_with_a_diagnostic() {
    let mut initialized = evidence_result("p1-diagnostics.jsonl", 1);
    initialized["agentInfo"]["version"] = "1.6.0".into();
    let script = Script {
        version: "agy 1.6.0".into(),
        steps: vec![Script::expect("initialize", initialized), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    assert!(matches!(
        bench.adapter.next_event().await,
        AdapterEvent::Diagnostic {
            kind: "untested_cli_version",
            required: false,
            ..
        }
    ));
    assert!(
        bench
            .fake
            .data_dir()
            .join("antigravity/bin/agy-1.6.0")
            .is_file()
    );
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_changed_system_binary_gets_a_new_pinned_copy_and_a_diagnostic() {
    let script = Script {
        version: "agy 1.5.0".into(),
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    let bin = bench.fake.data_dir().join("antigravity/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("agy-1.4.12"), b"old copy").unwrap();
    bench.start().await;
    assert!(matches!(
        bench.adapter.next_event().await,
        AdapterEvent::Diagnostic {
            kind: "antigravity_cli_version_changed",
            required: false,
            ..
        }
    ));
    assert!(bin.join("agy-1.5.0").is_file());
    assert_eq!(
        std::fs::read(bin.join("agy-1.4.12")).unwrap(),
        b"old copy",
        "an existing pinned copy is never overwritten"
    );
    bench.close().await;
}

#[tokio::test]
async fn catalog_probe_reads_real_models_and_excludes_the_family_option() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let fake = Fake::new(script);
    let cfg = AntigravityConfig {
        enabled: true,
        binary: Some(fake.exe.clone()),
        data_dir: Some(fake.data_dir()),
    };
    let catalog = catalog::discover(&cfg).await.unwrap();
    let ids: Vec<&str> = catalog
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
        ]
    );
    assert_eq!(
        catalog.current_model.as_deref(),
        Some("gemini-3.8-flash-high")
    );
    assert_eq!(catalog.efforts, vec!["low", "medium", "high"]);
    assert_eq!(catalog.current_effort.as_deref(), Some("high"));
    assert!(
        !ids.contains(&"gemini-3.8-flash"),
        "configOptions.model is not a model id"
    );
    let requests = fake.requests();
    assert_eq!(calls(&requests, "initialize").len(), 1);
    assert_eq!(calls(&requests, "session/new").len(), 1);
    assert!(calls(&requests, "session/prompt").is_empty());
    fake.wait_gone().await;
}

#[test]
fn catalog_parses_the_recorded_p1_frame() {
    let info = catalog::parse_frame(&evidence_result("p1-diagnostics.jsonl", 3)).unwrap();
    assert_eq!(info.session_id, "d41d6945-1310-44ef-89e6-42705ee12e27");
    let ids: Vec<&str> = info.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
        ]
    );
    assert_eq!(info.efforts, vec!["low", "medium", "high"]);
    let entry = catalog::AntigravityCatalog {
        version: canterlot_worker::adapter::antigravity::version::CliVersion::Verified(
            "1.4.12".into(),
        ),
        models: info.models,
        current_model: info.current_model,
        efforts: info.efforts,
        current_effort: info.current_effort,
    }
    .harness();
    assert_eq!(entry.permission_modes, vec!["owner", "ask", "bypass"]);
    assert!(!entry.images);
    // Один effort на модель: переключателя нет, но форма может отправить значение.
    for model in &entry.models {
        assert_eq!(model.efforts.len(), 1, "{}", model.id);
    }
    assert_eq!(
        entry.models[0].efforts,
        vec!["high".to_owned()],
        "effort follows the model suffix"
    );
    assert!(
        entry
            .models
            .iter()
            .all(|model| model.context_window == Some(1_048_576)),
        "the family context window is visible to the catalog"
    );
}

#[tokio::test]
async fn graceful_close_exits_on_eof() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    let started = std::time::Instant::now();
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    assert!(started.elapsed() < WAIT);
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn force_close_kills_a_process_that_ignores_eof() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ignore_eof: true,
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    bench
        .adapter
        .close(CloseMode::Force, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

// --- задача 2: доставка, steer, логический ход, события, прерывание и сжатие ---

#[tokio::test]
async fn text_turn_streams_one_block_and_finishes_with_usage() {
    let mut steps = handshake();
    steps.extend(from_prompt("t2_text"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("Reply with exactly: apple");
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    match outcome {
        TransmitOutcome::Accepted(proof) => {
            assert_eq!(proof.kind, input::PROOF_KIND);
            assert_eq!(proof.command_id, input.command_id);
        }
        other => panic!("not accepted: {other:?}"),
    }
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    assert!(kinds(&events).contains(&BlockKind::Text));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { text, .. } if text == "apple"
    )));
    let usage = usage_of(&events);
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].input_tokens, 7183);
    assert_eq!(usage[0].output_tokens, 1);
    assert_eq!(usage[0].model, "gemini-3.8-flash-high");
    let (context, window) = events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::RunFinished {
                context_tokens,
                context_window_tokens,
                ..
            } => Some((*context_tokens, *context_window_tokens)),
            _ => None,
        })
        .expect("run finished");
    assert_eq!(context, 7183);
    assert_eq!(window, 1_048_576);
    bench.close().await;
}

#[tokio::test]
async fn thought_chunks_merge_and_a_kind_change_closes_the_block() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("think")),
        Script::send(thought_chunk("a")),
        Script::send(thought_chunk("b")),
        Script::send(agent_chunk("c")),
        respond(2, end_turn(Some(usage(10, 2, 3)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("think");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    let kinds = kinds(&events);
    assert!(kinds.contains(&BlockKind::Reasoning));
    assert!(kinds.contains(&BlockKind::Text));
    let finals: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockFinal { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(finals, vec!["ab", "c"]);
    let usage = usage_of(&events);
    assert_eq!(usage[0].output_tokens, 5);
    bench.close().await;
}

#[tokio::test]
async fn tool_turn_holds_the_call_until_the_owner_allows_it() {
    let mut steps = handshake();
    steps.extend(from_prompt("t5-allow"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "ask",
    );
    bench.start().await;
    let input = send(
        "Run the shell command `echo perm-allow-probe` then reply with exactly: done. Do not ask questions, just run it.",
    );
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, payload, mut events) = wait_request(&mut bench.adapter, "permission").await;
    assert_eq!(request_id, "1:call_730220");
    assert_eq!(payload["tool_name"], "run_command");
    assert_eq!(payload["tool_use_id"], "call_730220");
    assert_eq!(payload["input"]["CommandLine"], "echo perm-allow-probe");
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::AwaitingPermission,
            ..
        }
    )));
    assert!(matches!(
        bench.adapter.deliver_decision(&request_id, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    events.extend(drain(&mut bench.adapter).await);
    let kinds = kinds(&events);
    assert!(kinds.contains(&BlockKind::ToolCall));
    assert!(kinds.contains(&BlockKind::ToolResult));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::RunningTool,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { text, .. } if text.contains("perm-allow-probe")
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    let reply = bench
        .fake
        .requests()
        .into_iter()
        .find(|entry| entry["dir"] == "in" && entry["frame"]["id"] == json!("1"))
        .expect("permission reply");
    assert_eq!(
        reply["frame"]["result"]["outcome"]["optionId"],
        json!("allow")
    );
    assert!(
        !reply["frame"]["result"]["outcome"]
            .to_string()
            .contains("allow_always"),
        "the adapter never asks for a standing grant"
    );
    bench.close().await;
}

#[tokio::test]
async fn permission_deny_answers_deny_and_the_turn_ends() {
    let mut steps = handshake();
    steps.extend(from_prompt("t5-deny"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "ask",
    );
    bench.start().await;
    let input = send(
        "Run the shell command `echo perm-deny-probe` then reply with exactly: done. Do not ask questions, just run it.",
    );
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, mut events) = wait_request(&mut bench.adapter, "permission").await;
    assert_eq!(request_id, "1:call_168341");
    assert!(matches!(
        bench.adapter.deliver_decision(&request_id, &deny()).await,
        DecisionOutcome::Applied(_)
    ));
    events.extend(drain(&mut bench.adapter).await);
    assert_eq!(finished(&events), vec![input.command_id]);
    let reply = bench
        .fake
        .requests()
        .into_iter()
        .find(|entry| entry["dir"] == "in" && entry["frame"]["id"] == json!("1"))
        .expect("permission reply");
    assert_eq!(
        reply["frame"]["result"]["outcome"]["optionId"],
        json!("deny")
    );
    bench.close().await;
}

#[tokio::test]
async fn permission_decision_after_the_turn_ended_is_not_applied() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk(
            "Run the shell command `echo perm-late-probe` then reply with exactly: done.",
        )),
        Script::send(tool_call(
            "call_late",
            "run_command",
            json!({"CommandLine": "echo perm-late-probe"}),
        )),
        Script::send(tool_update("call_late", "pending", None)),
        Script::send(json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "session/request_permission",
            "params": {
                "sessionId": "s",
                "toolCall": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "call_late",
                    "name": "run_command",
                    "status": "pending",
                    "rawInput": {"CommandLine": "echo perm-late-probe"},
                },
                "options": [
                    {"optionId": "allow", "kind": "allow_once"},
                    {"optionId": "deny", "kind": "reject_once"},
                ],
            },
        })),
        respond(4, end_turn(Some(usage(10, 1, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "ask",
    );
    bench.start().await;
    let input = send("Run the shell command `echo perm-late-probe` then reply with exactly: done.");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, mut events) = wait_request(&mut bench.adapter, "permission").await;
    events.extend(drain(&mut bench.adapter).await);
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestClosed { request_id: closed, .. } if closed == &request_id
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    match bench.adapter.deliver_decision(&request_id, &allow()).await {
        DecisionOutcome::NotApplied(reason) => assert_eq!(reason, "request_not_pending"),
        other => panic!("expected NotApplied, got {other:?}"),
    }
    bench.close().await;
}

#[tokio::test]
async fn permission_decision_racing_cancel_is_not_applied_and_the_request_closes() {
    let mut steps = handshake();
    steps.extend(from_prompt("t7c-cancel-perm"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "ask",
    );
    bench.start().await;
    let input = send(
        "Run the shell command `echo cancel-perm-probe` then reply with exactly: done. Do not ask questions, just run it.",
    );
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, mut events) = wait_request(&mut bench.adapter, "permission").await;
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    events.extend(drain(&mut bench.adapter).await);
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestClosed { request_id: closed, reason } if closed == &request_id && reason == "interrupted"
    )));
    assert!(matches!(
        bench.adapter.deliver_decision(&request_id, &allow()).await,
        DecisionOutcome::NotApplied(_)
    ));
    bench.close().await;
}

#[tokio::test]
async fn permission_refusal_answers_deny_without_waiting_for_a_frame() {
    let mut steps = handshake();
    steps.extend(from_prompt("t5-allow"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "ask",
    );
    bench.start().await;
    let input = send(
        "Run the shell command `echo perm-allow-probe` then reply with exactly: done. Do not ask questions, just run it.",
    );
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "permission").await;
    bench.adapter.refuse_request(&request_id, "no").await;
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    let reply = bench
        .fake
        .requests()
        .into_iter()
        .find(|entry| entry["dir"] == "in" && entry["frame"]["id"] == json!("1"))
        .expect("permission reply");
    assert_eq!(
        reply["frame"]["result"]["outcome"]["optionId"],
        json!("deny")
    );
    bench.close().await;
}

#[tokio::test]
async fn a_non_zero_exit_code_is_reported_as_an_error_result() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("run")),
        Script::send(tool_call(
            "call_1",
            "run_command",
            json!({"CommandLine": "exit 3"}),
        )),
        Script::send(tool_update("call_1", "pending", None)),
        Script::send(tool_update("call_1", "in_progress", None)),
        Script::send(tool_update(
            "call_1",
            "completed",
            Some(json!({"output": "boom", "exit_code": 3})),
        )),
        respond(2, end_turn(None)),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("run");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(kinds(&events).contains(&BlockKind::Error));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { summary: Some(summary), .. } if summary == "код выхода 3"
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn steer_with_reverse_responses_finishes_the_original_turn_once() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk("first")),
        deferred("session/prompt", 5),
        Script::send(user_chunk("second")),
        Script::send(agent_chunk("Received ")),
        Script::send(agent_chunk("second")),
        respond(5, end_turn(Some(usage(7538, 25, 0)))),
        respond(4, end_turn(None)),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let first = send("first");
    let second = send("second");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    match bench.adapter.steer(&second).await {
        SteerOutcome::Accepted(proof) => assert_eq!(proof.command_id, second.command_id),
        other => panic!("steer not accepted: {other:?}"),
    }
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![first.command_id]);
    let usage = usage_of(&events);
    assert_eq!(usage[0].input_tokens, 7538);
    assert_eq!(usage[0].output_tokens, 25);
    bench.close().await;
}

#[tokio::test]
async fn steer_with_forward_responses_finishes_the_original_turn_once() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk("first")),
        deferred("session/prompt", 5),
        Script::send(user_chunk("second")),
        respond(4, end_turn(None)),
        respond(5, end_turn(Some(usage(7538, 25, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let first = send("first");
    let second = send("second");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.steer(&second).await,
        SteerOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![first.command_id]);
    assert_eq!(usage_of(&events)[0].output_tokens, 25);
    bench.close().await;
}

#[tokio::test]
async fn two_steers_share_one_turn_and_sum_their_usage() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk("one")),
        deferred("session/prompt", 5),
        Script::send(user_chunk("two")),
        deferred("session/prompt", 6),
        Script::send(user_chunk("three")),
        respond(6, end_turn(Some(usage(3, 3, 0)))),
        respond(4, end_turn(Some(usage(1, 1, 0)))),
        respond(5, end_turn(Some(usage(2, 2, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let first = send("one");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    for text in ["two", "three"] {
        assert!(matches!(
            bench.adapter.steer(&send(text)).await,
            SteerOutcome::Accepted(_)
        ));
    }
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![first.command_id]);
    let usage = usage_of(&events);
    assert_eq!(usage[0].input_tokens, 6);
    assert_eq!(usage[0].output_tokens, 6);
    bench.close().await;
}

#[tokio::test]
async fn a_late_echo_after_the_deadline_is_input_accepted() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send_after(user_chunk("late"), 300),
        Script::send(agent_chunk("ok")),
        respond(2, end_turn(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(100),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );
    bench.start().await;
    let input = send("late");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(matches!(
        events[0],
        AdapterEvent::InputAccepted { command_id } if command_id == input.command_id
    ));
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn a_compact_without_an_echo_cancels_and_waits_for_the_late_response() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::notification("session/cancel"),
        Step::Delay { ms: 100 },
        respond(2, cancelled(None)),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(200),
        Duration::from_millis(300),
        Duration::from_secs(60),
    );
    bench.start().await;
    let started = std::time::Instant::now();
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Failed
    );
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "compact must wait for the outstanding RPC: {:?}",
        started.elapsed()
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: false })),
        "the late compact response is not ignored: {events:?}"
    );
    bench.close().await;
}

#[tokio::test]
async fn a_compact_that_ignores_cancel_is_closed_before_input_is_released() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::notification("session/cancel"),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(200),
        Duration::from_millis(150),
        Duration::from_secs(60),
    );
    bench.start().await;
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Failed
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. })),
        "an unsettled compact retires the process: {events:?}"
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_late_response_for_one_rpc_keeps_the_forced_close_timer_for_the_other() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("one")),
        deferred("session/prompt", 3),
        Script::notification("session/cancel"),
        Step::Delay { ms: 250 },
        // Отвечает только steer после срока `interrupt`; исходный prompt молчит.
        respond(3, cancelled(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(300),
        Duration::from_millis(150),
        Duration::from_millis(200),
    );
    bench.start().await;
    let first = send("one");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.steer(&send("two")).await,
        SteerOutcome::Undetermined
    ));
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. })),
        "the unanswered prompt keeps the watchdog armed: {events:?}"
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_cli_that_stops_reading_stdin_is_retired_inside_the_write_deadline() {
    let steps = vec![init_step(), new_step(), Step::Hang];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(200),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );
    bench.start().await;
    let input = send(&"x".repeat(262_144));
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        bench.adapter.transmit(&input, &bench.group),
    )
    .await
    .expect("a stalled stdin write must not block transmit");
    assert!(matches!(outcome, TransmitOutcome::Undetermined));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. })),
        "a timed-out partial write retires the connection: {events:?}"
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_steer_answered_without_an_echo_does_not_keep_its_echo_for_the_next_input() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("one")),
        deferred("session/prompt", 3),
        // Ответ steer без эха: ожидание эха не должно пережить свой prompt.
        respond(3, end_turn(Some(usage(1, 1, 0)))),
        respond(2, end_turn(Some(usage(1, 1, 0)))),
        deferred("session/prompt", 4),
        Script::send(user_chunk("two")),
        respond(4, end_turn(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(300),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );
    bench.start().await;
    let first = send("one");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let steer = send("two");
    assert!(matches!(
        bench.adapter.steer(&steer).await,
        SteerOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![first.command_id]);

    let next = send("two");
    assert!(
        matches!(
            bench.adapter.transmit(&next, &bench.group).await,
            TransmitOutcome::Accepted(ref proof) if proof.command_id == next.command_id
        ),
        "the stale steer echo must not eat the next input's proof"
    );
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![next.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn a_rejection_after_the_deadline_fails_the_turn_instead_of_dropping_it() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Step::Delay { ms: 300 },
        respond_error(
            2,
            json!({"code": -32603, "message": "trajectory not found"}),
        ),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        Duration::from_millis(100),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );
    bench.start().await;
    let input = send("late refusal");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(
        finished(&events),
        vec![input.command_id],
        "a late rejection must close the turn: {events:?}"
    );
    assert!(kinds(&events).contains(&BlockKind::Error));
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { text, .. } if text.contains("trajectory")
        )),
        "the rejection reason reaches the feed: {events:?}"
    );
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::NotApplicable
    ));
    bench.close().await;
}

#[tokio::test]
async fn an_error_before_the_echo_is_rejected_without_a_run_finished() {
    let steps = vec![
        init_step(),
        new_step(),
        Script::expect_error(
            "session/prompt",
            json!({"code": -32603, "message": "trajectory not found"}),
        ),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let outcome = bench.adapter.transmit(&send("hello"), &bench.group).await;
    assert!(matches!(outcome, TransmitOutcome::Rejected(ref text) if text.contains("trajectory")));
    let events = drain(&mut bench.adapter).await;
    assert!(finished(&events).is_empty());
    bench.close().await;
}

#[tokio::test]
async fn a_prompt_error_after_the_echo_fails_the_turn() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("go")),
        respond_error(2, json!({"code": -32603, "message": "quota exceeded"})),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("go");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(kinds(&events).contains(&BlockKind::Error));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { text, .. } if text.contains("quota exceeded")
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn steer_without_a_turn_is_not_applicable() {
    let mut bench = Bench::new(
        Script {
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert!(matches!(
        bench.adapter.steer(&send("now")).await,
        SteerOutcome::NotApplicable(_)
    ));
    bench.close().await;
}

#[tokio::test]
async fn steer_during_compaction_is_not_applicable() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("/compact")),
        Script::send(agent_chunk("summary")),
        respond(2, end_turn(Some(usage(5, 2, 1)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let command = Uuid::now_v7();
    assert_eq!(
        bench.adapter.compact(command).await,
        CompactOutcome::Accepted
    );
    assert!(bench.adapter.holds_input_during_compaction());
    assert!(matches!(
        bench.adapter.steer(&send("now")).await,
        SteerOutcome::NotApplicable(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: true }))
    );
    bench.close().await;
}

#[tokio::test]
async fn interrupt_acknowledges_cancel_and_keeps_the_process() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("long")),
        Script::notification("session/cancel"),
        Script::send(agent_chunk("cancelled")),
        respond(2, cancelled(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("long");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    assert!(!bench.fake.pids().is_empty(), "process must survive cancel");
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn cancel_losing_to_end_turn_is_not_applicable() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("long")),
        respond(2, end_turn(None)),
        Script::notification("session/cancel"),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("long");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::NotApplicable
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn cancel_without_a_response_is_undetermined_then_forced_closed() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("long")),
        Script::notification("session/cancel"),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        WAIT,
        Duration::from_millis(100),
        Duration::from_millis(200),
    );
    bench.start().await;
    let input = send("long");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn compact_ok_emits_compacted_without_a_run_finished() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("/compact")),
        Script::send(agent_chunk("summary")),
        respond(2, end_turn(Some(usage(5, 2, 1)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: true }))
    );
    assert!(finished(&events).is_empty());
    bench.close().await;
}

#[tokio::test]
async fn compact_error_after_the_echo_emits_a_failed_compacted() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("/compact")),
        respond_error(2, json!({"code": -32603, "message": "summary failed"})),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: false }))
    );
    bench.close().await;
}

#[tokio::test]
async fn process_death_after_the_echo_is_process_exited() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("go")),
        Step::Die {
            code: 7,
            kill: false,
        },
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("go");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { code: Some(7), .. }))
    );
    assert!(finished(&events).is_empty());
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn process_death_during_a_tool_is_process_exited() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("run")),
        Script::send(tool_call(
            "call_1",
            "run_command",
            json!({"CommandLine": "sleep 60"}),
        )),
        Script::send(tool_update("call_1", "in_progress", None)),
        Step::Die {
            code: 9,
            kill: true,
        },
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("run");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(kinds(&events).contains(&BlockKind::ToolCall));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    assert!(finished(&events).is_empty());
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn process_death_during_compaction_fails_the_compacted() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("/compact")),
        Step::Die {
            code: 8,
            kill: false,
        },
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: false }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn pending_settings_go_before_the_prompt() {
    let steps = vec![
        init_step(),
        new_step(),
        Script::expect("session/set_model", json!({})),
        Script::expect("session/set_config_option", json!({})),
        deferred("session/prompt", 9),
        Script::send(user_chunk("hi")),
        respond(9, end_turn(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let mut input = send("hi");
    input.settings = Some(SettingsChange {
        model: Some("gemini-3.8-flash-low".into()),
        effort: Some("low".into()),
        ..Default::default()
    });
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let requests = bench.fake.requests();
    let position = |method: &str| {
        requests
            .iter()
            .position(|entry| entry["dir"] == "in" && entry["frame"]["method"] == method)
            .unwrap_or(usize::MAX)
    };
    assert!(position("session/set_model") < position("session/prompt"));
    assert!(position("session/set_config_option") < position("session/prompt"));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(
        usage_of(&events)[0].model,
        "gemini-3.8-flash-low".to_string()
    );
    bench.close().await;
}

#[tokio::test]
async fn files_are_paths_in_the_text_and_images_are_not_blocks() {
    let path = std::env::temp_dir().join(format!("agy-file-{}", Uuid::now_v7()));
    std::fs::write(&path, b"png").unwrap();
    let file = LocalFile {
        path: path.clone(),
        name: "картинка.png".into(),
        mime: "image/png".into(),
        size: 3,
        image: true,
    };
    let mut input = send("смотри");
    input.files = vec![file];
    let expected = input::prompt_text(&input).unwrap();
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk(&expected)),
        respond(2, end_turn(None)),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let requests = bench.fake.requests();
    let prompt = calls(&requests, "session/prompt");
    let blocks = prompt[0]["frame"]["params"]["prompt"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert!(
        blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains(path.to_str().unwrap())
    );
    assert!(blocks.iter().all(|block| block["type"] != "image"));
    bench.close().await;
    std::fs::remove_file(path).unwrap();

    let mut missing = send("x");
    missing.files = vec![LocalFile {
        path: PathBuf::from("/definitely/not/here.png"),
        name: "x.png".into(),
        mime: "image/png".into(),
        size: 1,
        image: true,
    }];
    assert_eq!(
        bench.adapter.validate_transmission(&missing),
        Err("attachment_missing")
    );
}

#[tokio::test]
async fn a_subagent_without_payload_is_a_collapsed_tool() {
    let mut steps = handshake();
    steps.extend(from_prompt("t9-subagent"));
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send(
        "Delegate to a subagent: have it run the shell command `echo subagent-hi` and report the output back to you. Then reply with exactly: done. Do not ask questions.",
    );
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { tool_name: Some(name), .. } if name == "invoke_subagent"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { summary: Some(summary), .. }
            if summary == "результат субагента придёт текстом агента"
    )));
    bench.close().await;
}

const QUESTION_PROMPT: &str = "Before doing anything, ask me which of two options I prefer using your question tool: the banner color, red or blue. Offer both options, then wait for my answer and stop.";

fn question_live_steps() -> Vec<Step> {
    let mut steps = handshake();
    steps.extend(from_prompt("p-live-question-prompt"));
    steps
}

fn question_script(steps: Vec<Step>) -> Script {
    Script {
        version: "agy 1.5.0".into(),
        steps,
        ..Default::default()
    }
}

fn second_prompt_index(steps: &[Step]) -> usize {
    let mut seen = 0;
    for (index, step) in steps.iter().enumerate() {
        if matches!(step, Step::Expect { method, .. } if method == "session/prompt") {
            seen += 1;
            if seen == 2 {
                return index;
            }
        }
    }
    panic!("second session/prompt step");
}

fn question_without_answer_steps() -> Vec<Step> {
    let mut steps = question_live_steps();
    let index = second_prompt_index(&steps);
    steps.truncate(index);
    steps
}

#[tokio::test]
async fn ask_question_holds_the_turn_and_the_answer_prompt_completes_it() {
    let mut bench = Bench::new(question_script(question_live_steps()), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, payload, mut events) = wait_request(&mut bench.adapter, "question").await;
    events.extend(drain(&mut bench.adapter).await);
    assert_eq!(request_id, "1:call_402994");
    assert_eq!(payload["questions"][0]["id"], "q0");
    assert_eq!(
        payload["questions"][0]["text"],
        "Which banner color do you prefer?"
    );
    assert_eq!(
        payload["questions"][0]["options"],
        json!([
            {"label": "(Recommended) Red", "description": null},
            {"label": "Blue", "description": null},
        ])
    );
    assert_eq!(payload["questions"][0]["multi_select"], false);
    assert_eq!(payload["questions"][0]["allow_custom"], true);
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::AwaitingAnswer,
            ..
        }
    )));
    assert!(
        finished(&events).is_empty(),
        "the logical turn stays open while the question card is open"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestClosed { .. }))
    );
    assert!(matches!(
        bench
            .adapter
            .deliver_decision(&request_id, &answer("q0", "Blue"))
            .await,
        DecisionOutcome::Applied(_)
    ));
    events.extend(drain(&mut bench.adapter).await);
    assert_eq!(finished(&events), vec![input.command_id]);
    let requests = bench.fake.requests();
    assert!(
        calls(&requests, "session/cancel").is_empty(),
        "1.5.0 answers a question without cancel"
    );
    let prompts = session_prompts(&requests);
    assert_eq!(prompts.len(), 2);
    assert_eq!(
        prompts[1]["frame"]["params"]["prompt"][0]["text"],
        "Ответ владельца на вопрос \"Which banner color do you prefer?\": Blue"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { tool_name: Some(name), .. } if name == "ask_question"
    )));
    bench.close().await;
}

#[tokio::test]
async fn a_question_after_end_turn_stays_open_without_an_answer() {
    let mut bench = Bench::new(question_script(question_live_steps()), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, mut events) = wait_request(&mut bench.adapter, "question").await;
    events.extend(drain(&mut bench.adapter).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), bench.adapter.next_event())
            .await
            .is_err(),
        "nothing closes the question while the owner is silent"
    );
    assert!(finished(&events).is_empty());
    assert!(matches!(
        bench
            .adapter
            .deliver_decision(&request_id, &answer("q0", "Red"))
            .await,
        DecisionOutcome::Applied(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn a_second_answer_to_the_same_question_is_not_applied() {
    let mut bench = Bench::new(question_script(question_live_steps()), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "question").await;
    assert!(matches!(
        bench
            .adapter
            .deliver_decision(&request_id, &answer("q0", "Red"))
            .await,
        DecisionOutcome::Applied(_)
    ));
    assert!(matches!(
        bench
            .adapter
            .deliver_decision(&request_id, &answer("q0", "Green"))
            .await,
        DecisionOutcome::NotApplied(_)
    ));
    let requests = bench.fake.requests();
    assert_eq!(
        session_prompts(&requests).len(),
        2,
        "the second answer sends nothing"
    );
    drain(&mut bench.adapter).await;
    bench.close().await;
}

#[tokio::test]
async fn refusing_a_question_cancels_only_and_closes_the_held_turn() {
    let mut steps = question_without_answer_steps();
    steps.push(Script::notification("session/cancel"));
    let mut bench = Bench::new(question_script(steps), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, mut events) = wait_request(&mut bench.adapter, "question").await;
    events.extend(drain(&mut bench.adapter).await);
    assert!(finished(&events).is_empty());
    bench.adapter.refuse_request(&request_id, "no").await;
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    let requests = bench.fake.requests();
    assert_eq!(calls(&requests, "session/cancel").len(), 1);
    assert_eq!(
        session_prompts(&requests).len(),
        1,
        "refusal never starts a prompt"
    );
    bench.close().await;
}

#[tokio::test]
async fn a_late_answer_after_interrupt_is_expired() {
    let mut steps = question_without_answer_steps();
    steps.push(Script::notification("session/cancel"));
    let mut bench = Bench::new(question_script(steps), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "question").await;
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    match bench
        .adapter
        .deliver_decision(&request_id, &answer("q0", "Red"))
        .await
    {
        DecisionOutcome::NotApplied(reason) => assert_eq!(reason, "question expired"),
        other => panic!("expected NotApplied, got {other:?}"),
    }
    bench.close().await;
}

#[tokio::test]
async fn process_death_after_the_answer_prompt_is_undetermined() {
    let mut steps = question_without_answer_steps();
    steps.push(deferred("session/prompt", 4));
    steps.push(Step::Die {
        code: 1,
        kill: false,
    });
    let mut bench = Bench::new(question_script(steps), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "question").await;
    assert!(matches!(
        bench
            .adapter
            .deliver_decision(&request_id, &answer("q0", "Red"))
            .await,
        DecisionOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn process_death_with_a_question_open_expires_a_late_answer() {
    let mut steps = question_without_answer_steps();
    steps.push(Step::Die {
        code: 1,
        kill: false,
    });
    let mut bench = Bench::new(question_script(steps), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "question").await;
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    match bench
        .adapter
        .deliver_decision(&request_id, &answer("q0", "Red"))
        .await
    {
        DecisionOutcome::NotApplied(reason) => assert_eq!(reason, "question expired"),
        other => panic!("expected NotApplied, got {other:?}"),
    }
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_malformed_question_is_a_diagnostic_and_a_cancel() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk(QUESTION_PROMPT)),
        Script::send(tool_call("call_bad", "ask_question", json!({}))),
        Script::send(tool_update("call_bad", "pending", None)),
        Script::notification("session/cancel"),
        respond(4, end_turn(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::new(question_script(steps), "", "", "bypass");
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_question_invalid",
            required: false,
            ..
        }
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestOpened { kind, .. } if kind == "question"
    )));
    assert!(
        bench
            .fake
            .requests()
            .iter()
            .any(|entry| entry["dir"] == "in" && entry["frame"]["method"] == "session/cancel")
    );
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

fn question_call() -> Step {
    Script::send(tool_call(
        "call_q",
        "ask_question",
        json!({"questions": [{"question": "Which?", "options": ["A", "B"]}]}),
    ))
}

fn watch_timings() -> (Duration, Duration, Duration) {
    (WAIT, Duration::from_millis(100), Duration::from_millis(150))
}

async fn wait_process_exit(adapter: &mut AntigravityAdapter) -> Vec<AdapterEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, adapter.next_event())
            .await
            .expect("the cancel watchdog never closed the process");
        let exited = matches!(event, AdapterEvent::ProcessExited { .. });
        events.push(event);
        if exited {
            return events;
        }
    }
}

#[tokio::test]
async fn an_unavailable_question_cancel_that_is_ignored_is_forced_closed() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk(QUESTION_PROMPT)),
        question_call(),
        Script::notification("session/cancel"),
        Step::Hang,
    ];
    let (deadline, interrupt, stall) = watch_timings();
    let mut bench = Bench::with_questions(
        question_script(steps),
        "",
        "",
        "bypass",
        deadline,
        interrupt,
        stall,
        false,
    );
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = wait_process_exit(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_questions_unavailable",
            ..
        }
    )));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_malformed_question_cancel_that_is_ignored_is_forced_closed() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk(QUESTION_PROMPT)),
        Script::send(tool_call("call_bad", "ask_question", json!({}))),
        Script::notification("session/cancel"),
        Step::Hang,
    ];
    let (deadline, interrupt, stall) = watch_timings();
    let mut bench = Bench::with_questions(
        question_script(steps),
        "",
        "",
        "bypass",
        deadline,
        interrupt,
        stall,
        true,
    );
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = wait_process_exit(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_question_invalid",
            ..
        }
    )));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn a_refused_question_cancel_that_is_ignored_is_forced_closed() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk(QUESTION_PROMPT)),
        question_call(),
        Script::notification("session/cancel"),
        Step::Hang,
    ];
    let (deadline, interrupt, stall) = watch_timings();
    let mut bench = Bench::with_questions(
        question_script(steps),
        "",
        "",
        "bypass",
        deadline,
        interrupt,
        stall,
        true,
    );
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let (request_id, _, _) = wait_request(&mut bench.adapter, "question").await;
    bench.adapter.refuse_request(&request_id, "no").await;
    wait_process_exit(&mut bench.adapter).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn ask_question_is_refused_while_the_capability_is_unavailable() {
    let mut steps = question_without_answer_steps();
    steps.push(Script::notification("session/cancel"));
    let mut bench = Bench::new_without_questions(question_script(steps));
    bench.start().await;
    let input = send(QUESTION_PROMPT);
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_questions_unavailable",
            required: false,
            ..
        }
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestOpened { kind, .. } if kind == "question"
    )));
    assert!(
        bench
            .fake
            .requests()
            .iter()
            .any(|entry| entry["dir"] == "in" && entry["frame"]["method"] == "session/cancel")
    );
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn manage_task_is_a_collapsed_service_tool() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("status")),
        Script::send(tool_call(
            "call_1",
            "manage_task",
            json!({"Action": "status", "TaskId": "s/task-2"}),
        )),
        Script::send(tool_update("call_1", "completed", None)),
        respond(2, end_turn(None)),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("status");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { tool_name: Some(name), summary: Some(summary), .. }
            if name == "manage_task" && summary == "Action: status"
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn an_mcp_tool_call_keeps_its_full_name_and_raw_input() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/stage0/evidence/antigravity/p-live-mcp1.jsonl"),
    )
    .unwrap();
    let mut frames = Vec::new();
    for line in text.lines() {
        let entry: Value = serde_json::from_str(line).unwrap();
        let frame: Value = serde_json::from_str(entry["line"].as_str().unwrap()).unwrap();
        if frame["params"]["update"]["toolCallId"] == "call_404323" {
            frames.push(frame);
        }
    }
    assert_eq!(frames.len(), 2, "tool_call and completed");
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 3),
        Script::send(user_chunk("Call the echo tool with pong-7391")),
        Script::send(frames[0].clone()),
        Script::send(frames[1].clone()),
        respond(3, end_turn(Some(usage(16_377, 873, 822)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("Call the echo tool with pong-7391");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { tool_name: Some(name), summary: Some(summary), .. }
            if name == "mcp_echo-probe_echo" && summary == "Echo probe call"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { text, .. } if text.contains("pong-7391")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Diagnostic { .. }))
    );
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn interrupt_without_a_turn_is_not_applicable() {
    let mut bench = Bench::new(
        Script {
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::NotApplicable
    ));
    bench.close().await;
}

#[tokio::test]
async fn cancel_during_compaction_fails_the_compacted() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("/compact")),
        Script::notification("session/cancel"),
        respond(2, cancelled(Some(usage(1, 1, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    assert_eq!(
        bench.adapter.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Compacted { ok: false }))
    );
    bench.close().await;
}

#[tokio::test]
async fn unknown_and_malformed_updates_become_diagnostics() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("x")),
        Script::send(update(json!({"sessionUpdate": "mystery", "x": 1}))),
        Script::send(update(json!({"sessionUpdate": "agent_message_chunk"}))),
        respond(2, end_turn(None)),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("x");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "unknown_session_update",
            required: false,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "malformed_session_update",
            required: true,
            ..
        }
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    bench.close().await;
}

#[tokio::test]
async fn a_late_cancelled_response_clears_the_forced_close_timer() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("long")),
        Script::notification("session/cancel"),
        Script::send_after(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": cancelled(Some(usage(1, 1, 0))),
            }),
            300,
        ),
    ];
    let mut bench = Bench::with_timing(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
        WAIT,
        Duration::from_millis(100),
        Duration::from_millis(500),
    );
    bench.start().await;
    let input = send("long");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    assert!(matches!(
        bench.adapter.interrupt().await,
        InterruptOutcome::Undetermined
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
    );
    assert!(!bench.fake.pids().is_empty(), "live turn keeps the process");
    bench.close().await;
}

// --- задача 3: перезапуск между ходами для режима и инструкций ---

fn boots(fake: &antigravity::Fake) -> Vec<Value> {
    fake.entries()
        .into_iter()
        .filter(|entry| entry["boot"] == true)
        .collect()
}

fn restart_script(session_id: &str) -> Script {
    let mut resumed = evidence_result("p1-diagnostics.jsonl", 3);
    resumed["sessionId"] = json!(session_id);
    Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[session_id])),
            Script::expect("session/resume", resumed),
            set_model_step(),
            set_effort_step(),
            deferred("session/prompt", 4),
            Script::send(user_chunk("after restart")),
            respond(4, end_turn(Some(usage(1, 1, 0)))),
        ],
        ..Default::default()
    }
}

#[tokio::test]
async fn mode_and_instructions_restart_the_process_between_turns() {
    let mut bench = Bench::new(
        Script {
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    bench.spec.native_conversation_id = None;
    let started = bench.start().await;
    let first_pid = started.cli_pid;
    let session_id = started.native_conversation_id.clone();
    bench.fake.rewrite(restart_script(&session_id));

    let mut input = send("after restart");
    let mut next_instructions = instructions();
    next_instructions.text = "new canterlot append".into();
    input.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        instructions: Some(next_instructions),
        ..Default::default()
    });
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    assert!(
        matches!(outcome, TransmitOutcome::Accepted(_)),
        "transmit failed; boots={:?} requests={:?}",
        boots(&bench.fake),
        bench.fake.requests()
    );
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::CliRestarted { cli_pid, .. } if *cli_pid != first_pid
    )));
    assert_eq!(finished(&events), vec![input.command_id]);
    assert_eq!(bench.adapter.session().unwrap().session_id, session_id);

    let booted = boots(&bench.fake);
    assert_eq!(booted.len(), 2, "the process was replaced once");
    let args: Vec<&str> = booted[1]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(args.contains(&"--dangerously-skip-permissions"));
    let requests = bench.fake.requests();
    let resume = calls(&requests, "session/resume");
    assert_eq!(resume.len(), 1);
    assert_eq!(resume[0]["frame"]["params"]["sessionId"], session_id);
    assert_eq!(
        resume[0]["frame"]["params"]["_meta"]["antigravity"]["systemPrompt"]["append"],
        "new canterlot append"
    );
    assert!(
        calls(&requests, "session/new").len() == 1,
        "resume, not a new session"
    );
    bench.close().await;
}

#[tokio::test]
async fn failed_restart_stops_the_execution_without_the_old_settings() {
    let mut bench = Bench::new(
        Script {
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    let started = bench.start().await;
    assert!(!started.native_conversation_id.is_empty());
    // Новая настройка не запускается: сессии нет в `session/list`.
    bench.fake.rewrite(Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[])),
        ],
        ..Default::default()
    });

    let mut input = send("after restart");
    input.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    assert!(
        matches!(&outcome, TransmitOutcome::Rejected(reason) if reason == "settings_restart_failed"),
        "{outcome:?}"
    );
    let events = drain(&mut bench.adapter).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "restart_failed",
                required: true,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::CliRestarted { .. })),
        "a failed restart is not a restart: {events:?}"
    );
    let booted = boots(&bench.fake);
    assert_eq!(
        booted.len(),
        2,
        "no fallback boot with the previous settings: {booted:?}"
    );
    let args: Vec<&str> = booted[1]["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        args.contains(&"--dangerously-skip-permissions"),
        "the new mode was attempted: {args:?}"
    );
    assert!(
        !calls(&bench.fake.requests(), "session/prompt")
            .iter()
            .any(|call| call["frame"]["params"]["prompt"][0]["text"] == "after restart"),
        "input is not sent after a failed restart"
    );
    assert!(bench.fake.pids().is_empty(), "the process is stopped");
    bench.close().await;
}

/// Системный `agy` заменён другим файлом: старый закреплённый ярлык остаётся
/// единственным источником версии и набора возможностей исполнения.
fn replace_system_binary(fake: &antigravity::Fake, version: &str) -> PathBuf {
    let script = fake.dir.path().join("replacement-script.json");
    let log = fake.dir.path().join("replacement.log");
    std::fs::write(
        &script,
        serde_json::to_vec(&Script {
            version: format!("agy {version}"),
            log: Some(log.clone()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    antigravity::fake_exec::write_executable(
        &fake.exe,
        format!(
            "#!/bin/sh\nunset PWD SHLVL OLDPWD _\nexec '{}' --script '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_fake-agy"),
            script.display()
        ),
        0o700,
    );
    log
}

fn process_alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(end) = stat.rfind(')') else {
        return false;
    };
    !matches!(stat[end + 1..].trim_start().chars().next(), Some('Z' | 'X'))
}

struct KillChild(u32);

impl Drop for KillChild {
    fn drop(&mut self) {
        if let Some(pid) = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

#[tokio::test]
async fn restart_terminates_tool_children_before_the_replacement_cli() {
    let mut bench = Bench::new(
        Script {
            steps: vec![
                init_step(),
                new_step(),
                Step::Spawn {
                    argv: vec!["sh".into(), "-c".into(), "trap '' TERM; sleep 5".into()],
                },
            ],
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    let started = bench.start().await;
    let child = loop {
        if let Some(entry) = bench
            .fake
            .entries()
            .into_iter()
            .find(|entry| entry["dir"] == "child")
        {
            break u32::try_from(entry["pid"].as_u64().unwrap()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let _guard = KillChild(child);
    assert!(process_alive(child), "the fake tool child did not start");
    bench.adapter.set_stop_grace(Duration::from_millis(200));
    bench
        .fake
        .rewrite(restart_script(&started.native_conversation_id));

    let mut input = send("after restart");
    input.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    assert!(
        matches!(outcome, TransmitOutcome::Accepted(_)),
        "restart failed; boots={:?}",
        boots(&bench.fake)
    );
    assert!(
        !process_alive(child),
        "the retiring CLI's tool child survived the restart"
    );
    assert_eq!(boots(&bench.fake).len(), 2, "the replacement CLI started");
    bench.close().await;
}

#[tokio::test]
async fn restart_reuses_the_pinned_copy_and_capabilities_after_a_system_change() {
    let mut bench = Bench::new(
        Script {
            version: "agy 1.5.0".into(),
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    let started = bench.start().await;
    assert_eq!(
        bench.adapter.capabilities().questions,
        canterlot_worker::adapter::Support::Supported
    );
    let first_pid = started.cli_pid;
    let session_id = started.native_conversation_id.clone();
    bench.fake.rewrite(restart_script(&session_id));
    let replacement_log = replace_system_binary(&bench.fake, "1.4.12");

    let mut input = send("after restart");
    input.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    assert!(
        matches!(outcome, TransmitOutcome::Accepted(_)),
        "transmit failed; boots={:?} requests={:?}",
        boots(&bench.fake),
        bench.fake.requests()
    );
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::CliRestarted { cli_pid, .. } if *cli_pid != first_pid
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_cli_version_changed",
            ..
        }
    )));
    assert_eq!(
        bench.adapter.capabilities().questions,
        canterlot_worker::adapter::Support::Supported,
        "the capability set stays pinned with the binary"
    );
    let bin = bench.fake.data_dir().join("antigravity/bin");
    assert!(bin.join("agy-1.5.0").is_file());
    assert!(
        !bin.join("agy-1.4.12").is_file(),
        "the replacement source is never pinned during a restart"
    );
    assert!(
        !replacement_log.exists()
            || std::fs::read_to_string(&replacement_log)
                .unwrap()
                .trim()
                .is_empty(),
        "the replacement system binary is never launched"
    );
    assert_eq!(boots(&bench.fake).len(), 2, "the pinned copy boots again");
    bench.close().await;
}

#[tokio::test]
async fn restart_fails_when_the_pinned_copy_disappeared() {
    let mut bench = Bench::new(
        Script {
            steps: handshake(),
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    bench.start().await;
    let pinned = bench.fake.data_dir().join("antigravity/bin/agy-1.4.12");
    assert!(pinned.is_file());
    std::fs::remove_file(&pinned).unwrap();

    let mut input = send("after restart");
    input.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    let outcome = bench.adapter.transmit(&input, &bench.group).await;
    assert!(
        matches!(&outcome, TransmitOutcome::Rejected(reason) if reason == "settings_restart_failed"),
        "{outcome:?}"
    );
    let events = drain(&mut bench.adapter).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "restart_failed",
            required: true,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::CliRestarted { .. }))
    );
    assert_eq!(
        boots(&bench.fake).len(),
        1,
        "a missing pinned copy is not replaced by the system binary"
    );
    assert!(!pinned.is_file(), "the restart does not re-pin");
    assert!(bench.fake.pids().is_empty(), "the old process is stopped");
    bench.close().await;
}

#[tokio::test]
async fn settings_never_restart_the_process_in_the_middle_of_a_turn() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk("long")),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "owner",
    );
    bench.start().await;
    let first = send("long");
    assert!(matches!(
        bench.adapter.transmit(&first, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    bench.fake.rewrite(Script {
        steps: handshake(),
        ..Default::default()
    });
    let mut second = send("later");
    second.settings = Some(SettingsChange {
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    assert!(matches!(
        bench.adapter.transmit(&second, &bench.group).await,
        TransmitOutcome::Rejected(reason) if reason == "turn_active"
    ));
    assert_eq!(boots(&bench.fake).len(), 1);
    bench.close().await;
}

#[tokio::test]
async fn model_text_about_quota_is_not_a_reached_limit() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 2),
        Script::send(user_chunk("go")),
        Script::send(agent_chunk(
            "Individual quota reached for gemini-3.8-flash-high. Resets in 2h 15m — \
             I will describe what remains.",
        )),
        respond(2, end_turn(Some(usage(10, 2, 0)))),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("go");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Limit(_))),
        "model text is never a quota signal: {events:?}"
    );
    bench.close().await;
}

#[tokio::test]
async fn a_quota_error_after_the_echo_fails_the_turn_without_a_limit_until_the_live_check() {
    let steps = vec![
        init_step(),
        new_step(),
        deferred("session/prompt", 4),
        Script::send(user_chunk("quota")),
        respond_error(
            4,
            json!({"code": -32603, "message": "Individual quota reached for gemini-3.8-flash-high. Resets in 2h 15m"}),
        ),
    ];
    let mut bench = Bench::new(
        Script {
            steps,
            ..Default::default()
        },
        "",
        "",
        "bypass",
    );
    bench.start().await;
    let input = send("quota");
    assert!(matches!(
        bench.adapter.transmit(&input, &bench.group).await,
        TransmitOutcome::Accepted(_)
    ));
    let events = drain(&mut bench.adapter).await;
    assert_eq!(finished(&events), vec![input.command_id]);
    assert!(kinds(&events).contains(&BlockKind::Error));
    // Форма ошибки квоты не подтверждена живым кадром (§11): до неё Limit не выдаётся.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Limit(_))),
        "structured quota detection stays behind its constant: {events:?}"
    );
    let run = events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::RunFinished {
                context_window_tokens,
                ..
            } => Some(*context_window_tokens),
            _ => None,
        })
        .unwrap();
    assert_eq!(run, 1_048_576);
    bench.close().await;
}
