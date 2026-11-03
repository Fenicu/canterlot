#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Контракт адаптера Antigravity на `fake-agy`: кадры этапа 0, запуск, resume и каталог.

use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, BlockKind, CloseMode, CompactOutcome, InstructionMode,
        InstructionSpec, InterruptOutcome, LocalFile, Phase, SettingsChange, StartSpec,
        SteerOutcome, Transmission, TransmitOutcome,
        antigravity::{
            AntigravityAdapter, catalog, input,
            process::ENV_ALLOWLIST,
            replay::{Script, Step, evidence_result, session_list},
        },
    },
    config::AntigravityConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
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
            .with_turn_stall_deadline(stall);
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
    assert_eq!(
        new[0]["frame"]["params"]["mcpServers"],
        Value::Array(vec![])
    );
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
    initialized["agentInfo"]["version"] = "1.5.0".into();
    let script = Script {
        version: "agy 1.5.0".into(),
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
            .join("antigravity/bin/agy-1.5.0")
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
    assert!(entry.models.iter().all(|model| model.efforts.is_empty()));
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
async fn tool_turn_holds_the_call_and_reports_the_result() {
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
    let events = drain(&mut bench.adapter).await;
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
    let denied = bench
        .fake
        .requests()
        .into_iter()
        .find(|entry| entry["dir"] == "in" && entry["frame"]["id"] == json!("1"))
        .expect("permission reply");
    assert_eq!(
        denied["frame"]["result"]["outcome"]["optionId"],
        json!("deny")
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "antigravity_permission_denied",
            required: false,
            ..
        }
    )));
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

#[tokio::test]
async fn ask_question_cancels_with_a_diagnostic_instead_of_hanging() {
    let mut steps = handshake();
    steps.extend(from_prompt("t6-question"));
    steps.push(Script::notification("session/cancel"));
    steps.push(respond(4, cancelled(Some(usage(1, 1, 0)))));
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
        "Before doing anything, ask me a clarifying question with options: which banner color should I use — red, green or blue? Use the question tool. Do not run any commands.",
    );
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
