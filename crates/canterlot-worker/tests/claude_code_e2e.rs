#![forbid(unsafe_code)]

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        self, Adapter, AdapterContext, AdapterEvent, BlockKind, CloseMode, InstructionMode,
        InstructionSpec, SettingsChange, StartSpec, Transmission, TransmitOutcome,
        claude_code::version::CliVersion,
    },
    config::ClaudeCodeConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tempfile::TempDir;
use tokio::process::Command;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

static CLI_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAIT: Duration = Duration::from_secs(15);

// Local pipes and Unix sockets only. The script never resolves or invokes claude.
const FAKE: &str = include_str!("support/fake_claude.py");

fn fixture(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/claude-code")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn outgoing(name: &str) -> Vec<Value> {
    fixture(name)
        .into_iter()
        .filter(|v| v["dir"] == "out")
        .map(|v| v["data"].clone())
        .collect()
}

fn tx(id: Uuid) -> Transmission {
    Transmission {
        command_id: id,
        attempt: 1,
        text: "test".into(),
        files: vec![],
        settings: None,
    }
}

struct Fake {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    spec: StartSpec,
    permit: Option<tokio::sync::MutexGuard<'static, ()>>,
}

impl Fake {
    fn new(turns: Vec<Vec<Value>>) -> Self {
        Self::with_timers(turns, WAIT, WAIT)
    }
    fn with_timers(turns: Vec<Vec<Value>>, transmit: Duration, ready: Duration) -> Self {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-cli");
        fake_exec::write_executable(&exe, FAKE, 0o700);
        for (n, turn) in turns.iter().enumerate() {
            std::fs::write(
                dir.path().join(format!("turn{}.json", n + 1)),
                serde_json::to_vec(turn).unwrap(),
            )
            .unwrap();
        }
        let context = AdapterContext {
            transmit_deadline: transmit,
            ready_deadline: ready,
            exec_dir: dir.path().join("exec"),
            claude_code: Some(ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            }),
            codex: None,
            opencode: None,
        };
        let adapter =
            adapter::registry_with("claude_code", &context).expect("Claude Code registered");
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: dir.path().into(),
            native_conversation_id: None,
            profile: "claude_code".into(),
            model: "haiku".into(),
            effort: "low".into(),
            permission_mode: "ask".into(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            isolate_mcp: false,
        };
        Self {
            dir,
            adapter,
            spec,
            group: TreeContainment::new().unwrap(),
            permit: None,
        }
    }
    async fn start(&mut self) -> String {
        self.permit = Some(CLI_GATE.lock().await);
        tokio::time::timeout(WAIT, self.adapter.start(&self.spec, &self.group))
            .await
            .unwrap()
            .unwrap()
            .native_conversation_id
    }
    async fn event(&mut self) -> AdapterEvent {
        tokio::time::timeout(WAIT, self.adapter.next_event())
            .await
            .expect("adapter event timeout")
    }
    async fn send(&mut self, t: &Transmission) {
        match tokio::time::timeout(WAIT, self.adapter.transmit(t, &self.group))
            .await
            .unwrap()
        {
            TransmitOutcome::Accepted(p) => {
                assert_eq!(p.command_id, t.command_id);
                assert!(
                    matches!(
                        p.kind,
                        "claude_code_answer_frame" | "claude_code_command_lifecycle"
                    ),
                    "unexpected proof kind {}",
                    p.kind
                );
            }
            other => panic!("delivery: {other:?}"),
        }
    }
    async fn finish(&mut self) -> Vec<AdapterEvent> {
        let mut events = vec![];
        loop {
            let e = self.event().await;
            let done = matches!(e, AdapterEvent::RunFinished { .. });
            events.push(e);
            if done {
                return events;
            }
        }
    }
    /// Двойник дошёл до `@wait`: SIGINT посреди его print дал бы reentrant-ошибку
    /// Python вместо кадров прерывания.
    async fn wait_point(&self) {
        tokio::time::timeout(WAIT, async {
            while !self.dir.path().join("waiting").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake reached @wait");
    }
    async fn close(&mut self) {
        tokio::time::timeout(WAIT, self.adapter.close(CloseMode::Force, &self.group))
            .await
            .unwrap()
            .unwrap();
        self.permit.take();
    }
}

#[test]
fn catalog_lists_claude_code_with_canterlot_permission_modes_when_configured() {
    let cfg = ClaudeCodeConfig::default();
    let cli = CliVersion::Verified("2.1.311".into());
    assert!(
        !adapter::catalog_with(None, Some(&cli), None, None)
            .iter()
            .any(|c| c.harness == "claude_code")
    );
    assert!(
        !adapter::catalog_with(Some(&cfg), None, None, None)
            .iter()
            .any(|c| c.harness == "claude_code")
    );
    let catalog = adapter::catalog_with(Some(&cfg), Some(&cli), None, None);
    let c = catalog.iter().find(|c| c.harness == "claude_code").unwrap();
    assert_eq!(c.provider, "claude");
    assert_eq!(c.title, "Claude Code");
    assert!(c.images && c.settings_midsession);
    assert!(!c.settings_midsession_effort);
    assert_eq!(c.permission_modes, ["owner", "ask", "bypass", "read_only"]);
    assert_eq!(
        c.models
            .iter()
            .map(|m| (m.id.as_str(), m.title.as_str()))
            .collect::<Vec<_>>(),
        [
            ("opus[1m]", "Opus 1M"),
            ("sonnet[1m]", "Sonnet 1M"),
            ("fable[1m]", "Fable 1M"),
            ("haiku", "Haiku"),
        ]
    );
    // Варианты 1M отдают окно 1M, а не 200K: индикатор считает долю от него.
    assert_eq!(
        c.models
            .iter()
            .map(|m| (m.id.as_str(), m.context_window))
            .collect::<Vec<_>>(),
        [
            ("opus[1m]", Some(1_000_000)),
            ("sonnet[1m]", Some(1_000_000)),
            ("fable[1m]", Some(1_000_000)),
            ("haiku", Some(200_000)),
        ]
    );
    for m in &c.models {
        assert_eq!(
            m.efforts,
            ["low", "medium", "high", "xhigh", "max"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn registry_builds_claude_code_adapter_from_context() {
    let f = Fake::new(vec![]);
    let cfg = ClaudeCodeConfig {
        executable: f.dir.path().join("fake-cli"),
        ..Default::default()
    };
    let mut spec = toml::Value::try_from(&cfg).expect("optional config fields serialize");
    spec = toml::toml! {
        execution_id = (f.spec.execution_id.to_string())
        generation = 1
        conversation_id = (Uuid::now_v7().to_string())
        protocol_version = 1
        claude_code = (spec)
    }
    .into();
    let decoded: canterlot_worker::supervisor::SupervisorSpec = spec.try_into().unwrap();
    assert_eq!(decoded.claude_code.unwrap().executable, cfg.executable);
    let caps = f.adapter.capabilities();
    assert!(matches!(caps.streaming, adapter::Support::Supported));
    assert!(matches!(caps.resume, adapter::Support::Supported));
    assert!(matches!(caps.approvals, adapter::Support::Supported));
    assert!(matches!(caps.questions, adapter::Support::Supported));
    assert!(!caps.interrupt_keeps_process);
    assert!(adapter::known_harness("claude_code"));
    assert!(!adapter::known_harness("unknown"));
}

#[tokio::test]
async fn replay_multi_input_end_to_end_through_the_adapter() {
    let evidence = fixture("multi_input");
    let mut turns = vec![];
    let mut ids = vec![];
    let mut expected_inputs = vec![];
    for v in evidence {
        if v["dir"] == "in" && v["data"]["type"] == "user" {
            if ids.len() == 3 {
                break;
            }
            ids.push(Uuid::parse_str(v["data"]["uuid"].as_str().unwrap()).unwrap());
            expected_inputs.push(v["data"].clone());
            turns.push(vec![]);
        } else if v["dir"] == "out" && !turns.is_empty() {
            turns.last_mut().unwrap().push(v["data"].clone());
        }
    }
    assert_eq!(ids.len(), 3);
    let last = turns[0].len() - 1;
    turns[0].insert(last, json!("@probe"));
    let mut f = Fake::new(turns);
    f.start().await;
    let mut all = vec![];
    for (id, expected) in ids.iter().zip(&expected_inputs) {
        let mut input = tx(*id);
        input.text = expected["message"]["content"].as_str().unwrap().to_owned();
        f.send(&input).await;
    }
    for id in &ids {
        let events = f.finish().await;
        let AdapterEvent::RunFinished {
            input_command_id,
            usage,
            ..
        } = events.last().unwrap()
        else {
            unreachable!()
        };
        assert_eq!(input_command_id, id);
        assert_eq!(usage.iter().map(|u| u.input_tokens).sum::<u64>(), 2);
        all.extend(events);
    }
    let mut written: Vec<Value> = std::fs::read_to_string(f.dir.path().join("stdin.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut notes = 0;
    for message in &mut written {
        if let Some(blocks) = message["message"]["content"].as_array() {
            assert_eq!(blocks.len(), 2);
            assert!(
                blocks[0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("[canterlot] Weekly Claude limit: 80 % used, resets ")
            );
            let text = blocks[1]["text"].clone();
            message["message"]["content"] = text;
            notes += 1;
        }
    }
    assert_eq!(
        notes, 1,
        "Task 3 adds the threshold note once to this evidence"
    );
    assert_eq!(written, expected_inputs);
    assert!(!f.dir.path().join("early").exists());
    assert!(
        all.iter()
            .filter(|e| matches!(e, AdapterEvent::BlockFinal { text, .. } if !text.is_empty()))
            .count()
            >= 3
    );
    f.close().await;
}

#[tokio::test]
async fn replay_permission_and_question_end_to_end() {
    for name in ["permissions", "question"] {
        let trace = fixture(name);
        let calls: Vec<Value> = trace
            .iter()
            .filter_map(|v| {
                let t = &v["data"]["mcp_trace"];
                let frame = &t["frame"];
                (t["dir"] == "mcp_in"
                    && frame["method"] == "tools/call"
                    && frame["params"]["name"] == "approve")
                    .then(|| frame.clone())
            })
            .collect();
        assert_eq!(calls.len(), if name == "permissions" { 2 } else { 1 });
        let expected: Vec<Value> = calls
            .iter()
            .map(|call| {
                trace
                    .iter()
                    .find_map(|v| {
                        let t = &v["data"]["mcp_trace"];
                        let frame = &t["frame"];
                        if t["dir"] == "mcp_out" && frame["id"] == call["id"] {
                            frame["result"]["content"][0]["text"]
                                .as_str()
                                .and_then(|text| serde_json::from_str(text).ok())
                        } else {
                            None
                        }
                    })
                    .expect("recorded decision reply")
            })
            .collect();
        let mut frames = outgoing(name);
        let id = frames
            .iter()
            .find_map(|v| v["user_message_uuid"].as_str())
            .map(|s| Uuid::parse_str(s).unwrap())
            .unwrap();
        for (index, call) in calls.iter().enumerate() {
            let marker = frames
                .iter()
                .position(|v| {
                    v["type"] == "user"
                        && v["message"]["content"].as_array().is_some_and(|blocks| {
                            blocks.iter().any(|b| {
                                b["type"] == "tool_result"
                                    && b["tool_use_id"]
                                        == call["params"]["arguments"]["tool_use_id"]
                            })
                        })
                })
                .expect("recorded tool result");
            frames.insert(marker, json!(format!("@request:{index}")));
        }
        let mut f = Fake::new(vec![frames]);
        std::fs::write(
            f.dir.path().join("relay-exe"),
            env!("CARGO_BIN_EXE_canterlot-worker"),
        )
        .unwrap();
        std::fs::write(
            f.dir.path().join("mcp-calls.json"),
            serde_json::to_vec(&calls).unwrap(),
        )
        .unwrap();
        f.start().await;
        f.send(&tx(id)).await;
        let mut applied = std::collections::HashSet::new();
        let mut events = vec![];
        loop {
            let event = f.event().await;
            if let AdapterEvent::RequestOpened {
                request_id,
                kind,
                payload,
            } = &event
            {
                let index = calls
                    .iter()
                    .position(|c| c["params"]["arguments"]["tool_use_id"] == *request_id)
                    .unwrap();
                assert!(applied.insert(request_id.clone()), "request opened twice");
                let reply = &expected[index];
                let decision = if kind == "question" {
                    let answers: BTreeMap<_, _> = payload["questions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|q| {
                            let recorded =
                                &reply["updatedInput"]["answers"][q["text"].as_str().unwrap()];
                            let answer = if q["multi_select"] == true {
                                AnswerValue::Multiple(
                                    recorded
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .map(|v| v.as_str().unwrap().to_owned())
                                        .collect(),
                                )
                            } else {
                                AnswerValue::Single(recorded.as_str().unwrap().to_owned())
                            };
                            (q["id"].as_str().unwrap().to_owned(), answer)
                        })
                        .collect();
                    DecisionPayload::Question { answers }
                } else {
                    DecisionPayload::Permission {
                        behavior: if reply["behavior"] == "allow" {
                            PermissionBehavior::Allow
                        } else {
                            PermissionBehavior::Deny
                        },
                        message: reply["message"].as_str().map(str::to_owned),
                    }
                };
                assert!(matches!(
                    tokio::time::timeout(WAIT, f.adapter.deliver_decision(request_id, &decision))
                        .await
                        .unwrap(),
                    adapter::DecisionOutcome::Applied(_)
                ));
            }
            let done = matches!(event, AdapterEvent::RunFinished { .. });
            events.push(event);
            if done {
                break;
            }
        }
        assert_eq!(applied.len(), calls.len());
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| matches!(e,AdapterEvent::RequestClosed {reason,..} if reason=="answered")
                )
                .count(),
            calls.len()
        );
        assert!(!events.iter().any(|e| matches!(
            e,
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolCall,
                approved_by: Some(_),
                ..
            }
        )));
        let replies: Vec<Value> = std::fs::read_to_string(f.dir.path().join("mcp-replies.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), calls.len());
        for (actual, expected) in replies.iter().zip(&expected) {
            let answer: Value =
                serde_json::from_str(actual["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(&answer, expected);
            assert_eq!(
                serde_json::to_vec(&answer["updatedInput"]["answers"]).unwrap(),
                serde_json::to_vec(&expected["updatedInput"]["answers"]).unwrap()
            );
        }
        f.close().await;
    }
}

#[tokio::test]
async fn restart_resumes_with_the_same_native_id() {
    let mut f = Fake::new(vec![]);
    let native = f.start().await;
    f.close().await;
    let mut second = Fake::new(vec![]);
    second.spec.native_conversation_id = Some(native.clone());
    assert_eq!(second.start().await, native);
    let args: Vec<String> = tokio::time::timeout(WAIT, async {
        loop {
            // Двойник пишет файл не атомарно: недописанный JSON читается заново.
            if let Ok(bytes) = std::fs::read(second.dir.path().join("args.json"))
                && let Ok(args) = serde_json::from_slice(&bytes)
            {
                break args;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(args.windows(2).any(|a| a == ["--resume", &native]));
    assert!(!args.iter().any(|a| a == "--session-id"));
    assert!(outgoing("resume").iter().any(|v| v["subtype"] == "init"));
    second.close().await;
}

async fn recorded(f: &Fake, name: &str) -> Value {
    tokio::time::timeout(WAIT, async {
        loop {
            // Двойник пишет файл не атомарно: недописанный JSON читается заново.
            if let Ok(bytes) = std::fs::read(f.dir.path().join(name))
                && let Ok(value) = serde_json::from_slice(&bytes)
            {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn delegated_session_starts_the_cli_with_a_strict_mcp_config() {
    let mut owner = Fake::new(vec![]);
    owner.start().await;
    let args = recorded(&owner, "args.json").await;
    assert!(
        !args
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--strict-mcp-config")
    );
    assert_eq!(recorded(&owner, "connectors.json").await, Value::Null);
    owner.close().await;

    let mut delegated = Fake::new(vec![]);
    delegated.spec.isolate_mcp = true;
    delegated.start().await;
    let args = recorded(&delegated, "args.json").await;
    assert!(
        args.as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--strict-mcp-config")
    );
    assert_eq!(recorded(&delegated, "connectors.json").await, "false");
    delegated.close().await;
}

#[tokio::test]
async fn cli_start_failures_have_stable_reasons() {
    let _permit = CLI_GATE.lock().await;
    for (version, expected) in [
        ("missing", "cli_not_found"),
        ("failed", "cli_version_failed"),
        ("2.1.1", "cli_too_old"),
    ] {
        let mut f = Fake::new(vec![]);
        if version == "missing" {
            std::fs::remove_file(f.dir.path().join("fake-cli")).unwrap();
        } else {
            std::fs::write(f.dir.path().join("version"), version).unwrap();
        }
        let error = f.adapter.start(&f.spec, &f.group).await.unwrap_err();
        assert_eq!(error.to_string(), expected);
    }
}

async fn error_turn(mut frames: Vec<Value>) -> Vec<AdapterEvent> {
    let id = Uuid::now_v7();
    for frame in &mut frames {
        if frame.is_object() {
            frame["user_message_uuid"] = json!(id);
            frame["user_message_uuids"] = json!([id]);
        }
    }
    let mut f = Fake::new(vec![frames]);
    f.start().await;
    f.send(&tx(id)).await;
    let events = f.finish().await;
    f.close().await;
    events
}

fn result(status: u16, text: &str) -> Value {
    json!({"type":"result","is_error":true,"api_error_status":status,"result":text,"terminal_reason":"api_error"})
}

#[tokio::test]
async fn authentication_error_requires_login_without_retry() {
    let events = error_turn(vec![result(401, "Please run /login")]).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::Diagnostic {
            kind: "auth_required",
            required: true,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
}

#[tokio::test]
async fn unknown_model_finishes_with_error_block() {
    let frames = outgoing("service_failure");
    let frames = frames
        .into_iter()
        .filter(|v| v["error"] == "model_not_found" || v["api_error_status"] == 404)
        .collect();
    let events = error_turn(frames).await;
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(e, AdapterEvent::BlockFinal { text, .. } if text.to_lowercase().contains("model"))));
}

#[tokio::test]
async fn transient_api_retry_only_reports_generating_phase() {
    let retry = outgoing("service_failure")
        .into_iter()
        .find(|v| v["subtype"] == "api_retry")
        .unwrap();
    let events = error_turn(vec![retry, json!({"type":"result","is_error":false})]).await;
    assert!(events.iter().any(|e| matches!(e, AdapterEvent::Phase { phase: adapter::Phase::Generating, summary: Some(s), .. } if s == "retry 1/1")));
    assert!(!events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
}

#[tokio::test]
async fn exhausted_api_retries_finish_with_status_error() {
    let events = error_turn(vec![result(503, "unavailable")]).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::BlockFinal { text, .. } if text.contains("503")))
    );
}

#[tokio::test]
async fn usage_limit_reports_reached_error_and_finish() {
    let events = error_turn(vec![result(429, "usage_limit_reached")]).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::Limit(adapter::LimitFact::Reached { .. })))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
}

#[tokio::test]
async fn context_overflow_reports_context_exhausted() {
    let events = error_turn(vec![result(400, "Prompt is too long")]).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ContextExhausted))
    );
}

#[tokio::test]
async fn interrupted_turn_finishes_without_error_block() {
    let _sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let id = Uuid::now_v7();
    let mut f = Fake::new(vec![vec![
        json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
        json!("@wait"),
    ]]);
    let mut interrupted = outgoing("interrupt")
        .into_iter()
        .filter(|v| v["type"] == "result" && v["terminal_reason"] == "aborted_tools")
        .take(1)
        .collect::<Vec<_>>();
    assert_eq!(interrupted.len(), 1);
    interrupted[0]["user_message_uuid"] = json!(id);
    interrupted[0]["user_message_uuids"] = json!([id]);
    std::fs::write(
        f.dir.path().join("interrupt.json"),
        serde_json::to_vec(&interrupted).unwrap(),
    )
    .unwrap();
    f.start().await;
    f.send(&tx(id)).await;
    f.wait_point().await;
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.interrupt())
            .await
            .unwrap(),
        adapter::InterruptOutcome::Acknowledged
    ));
    let events = f.finish().await;
    assert!(!events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
    loop {
        if let AdapterEvent::ProcessExited { code, signal } = f.event().await {
            assert_eq!(
                canterlot_worker::supervisor::exit_outcome(code, signal, true),
                ("stopped", "user_stop")
            );
            break;
        }
    }
    f.close().await;
}

fn captured(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_code")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn out_frames(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .filter(|r| r["dir"] == "out")
        .map(|r| r["data"].clone())
        .collect()
}

/// Конец обычного хода и следом начало пробуждающего: так идут кадры записанной сессии.
fn turn_then_wake() -> Vec<Value> {
    vec![
        json!({"type":"stream_event","parent_tool_use_id":null,"user_message_uuids":["@input"],"event":{"type":"message_start","message":{"id":"m1","model":"claude-haiku-test","usage":{}}}}),
        json!({"type":"result","subtype":"success","is_error":false,"user_message_uuids":["@input"],"modelUsage":{}}),
        json!({"type":"system","subtype":"init","session_id":"x","model":"claude-haiku-test","permissionMode":"default","mcp_servers":[{"name":"canterlot","status":"connected"}]}),
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{"id":"m2","model":"claude-haiku-test","usage":{}}}}),
        json!("@wait"),
    ]
}

#[tokio::test]
async fn interrupt_reaches_a_wake_turn_and_finishes_it_on_the_last_command() {
    let _sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let id = Uuid::now_v7();
    let mut f = Fake::new(vec![turn_then_wake()]);
    std::fs::write(
        f.dir.path().join("interrupt.json"),
        serde_json::to_vec(&json!([
            {"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","modelUsage":{}},
        ]))
        .unwrap(),
    )
    .unwrap();
    f.start().await;
    f.send(&tx(id)).await;
    f.wait_point().await;
    let mut seen = f.finish().await;
    loop {
        let event = f.event().await;
        let adopted = matches!(event, AdapterEvent::RunAdopted { .. });
        seen.push(event);
        if adopted {
            break;
        }
    }
    assert!(seen.iter().any(|e| matches!(
        e,
        AdapterEvent::RunAdopted { input_command_id } if *input_command_id == id
    )));
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.interrupt())
            .await
            .unwrap(),
        adapter::InterruptOutcome::Acknowledged
    ));
    let events = f.finish().await;
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == id
    )));
    f.close().await;
}

#[tokio::test]
async fn interrupt_while_streaming_is_acknowledged() {
    let _sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let rows = captured("interrupt_streaming");
    let id: Uuid = rows.iter().find(|r| r["dir"] == "in").unwrap()["data"]["uuid"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let split = rows
        .iter()
        .position(|r| r["data"]["signal"] == "SIGINT")
        .unwrap();
    let after = out_frames(&rows[split..]);
    assert!(
        after
            .iter()
            .any(|v| v["type"] == "result" && v["terminal_reason"] == "aborted_streaming")
    );
    let mut turn = out_frames(&rows[..split]);
    turn.push(json!("@wait"));
    let mut f = Fake::new(vec![turn]);
    std::fs::write(
        f.dir.path().join("interrupt.json"),
        serde_json::to_vec(&after).unwrap(),
    )
    .unwrap();
    f.start().await;
    f.send(&tx(id)).await;
    f.wait_point().await;
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.interrupt())
            .await
            .unwrap(),
        adapter::InterruptOutcome::Acknowledged
    ));
    let events = f.finish().await;
    assert!(!events.iter().any(|e| matches!(
        e,
        AdapterEvent::BlockStarted {
            kind: BlockKind::Error,
            ..
        }
    )));
    loop {
        if let AdapterEvent::ProcessExited { code, signal } = f.event().await {
            assert_eq!(
                canterlot_worker::supervisor::exit_outcome(code, signal, true),
                ("stopped", "user_stop")
            );
            break;
        }
    }
    f.close().await;
}

fn with_mcp_call(call: Value, frames: Vec<Value>) -> Fake {
    let f = Fake::new(vec![frames]);
    std::fs::write(
        f.dir.path().join("relay-exe"),
        env!("CARGO_BIN_EXE_canterlot-worker"),
    )
    .unwrap();
    std::fs::write(
        f.dir.path().join("mcp-calls.json"),
        serde_json::to_vec(&vec![call]).unwrap(),
    )
    .unwrap();
    f
}

fn mcp_reply_text(f: &Fake) -> Value {
    let line = std::fs::read_to_string(f.dir.path().join("mcp-replies.jsonl")).unwrap();
    let frame: Value = serde_json::from_str(&line).unwrap();
    serde_json::from_str(frame["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn approval_of_a_large_request_returns_the_original_input() {
    let id = Uuid::now_v7();
    let input = json!({"file_path": "/tmp/x", "content": "щ".repeat(200_000)});
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"approve",
        "arguments":{"tool_name":"Write","input":input,"tool_use_id":"toolu_large"},
        "_meta":{"claudecode/toolUseId":"toolu_large"}}});
    let mut f = with_mcp_call(
        call,
        vec![
            json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
            json!("@request:0"),
            json!({"type":"result","subtype":"success","is_error":false,"user_message_uuids":[id]}),
        ],
    );
    f.start().await;
    f.send(&tx(id)).await;
    loop {
        match f.event().await {
            AdapterEvent::RequestOpened { request_id, .. } => {
                let decision = DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    message: None,
                };
                assert!(matches!(
                    tokio::time::timeout(WAIT, f.adapter.deliver_decision(&request_id, &decision))
                        .await
                        .unwrap(),
                    adapter::DecisionOutcome::Applied(_)
                ));
            }
            AdapterEvent::RunFinished { .. } => break,
            _ => {}
        }
    }
    let reply = mcp_reply_text(&f);
    assert_eq!(reply["behavior"], "allow");
    assert_eq!(reply["updatedInput"], input);
    f.close().await;
}

async fn phase_after_decision(
    tool_name: &str,
    input: Value,
    decision: DecisionPayload,
) -> adapter::Phase {
    let id = Uuid::now_v7();
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"approve",
        "arguments":{"tool_name":tool_name,"input":input,"tool_use_id":"toolu_phase"},
        "_meta":{"claudecode/toolUseId":"toolu_phase"}}});
    let mut f = with_mcp_call(
        call,
        vec![
            json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
            json!("@request:0"),
            json!("@wait"),
        ],
    );
    f.start().await;
    f.send(&tx(id)).await;
    loop {
        if let AdapterEvent::RequestOpened { request_id, .. } = f.event().await {
            assert!(matches!(
                tokio::time::timeout(WAIT, f.adapter.deliver_decision(&request_id, &decision))
                    .await
                    .unwrap(),
                adapter::DecisionOutcome::Applied(_)
            ));
            break;
        }
    }
    let phase = loop {
        match f.event().await {
            AdapterEvent::Phase {
                phase: adapter::Phase::AwaitingPermission | adapter::Phase::AwaitingAnswer,
                ..
            } => {}
            AdapterEvent::Phase { phase, .. } => break phase,
            AdapterEvent::RunFinished { .. } => panic!("phase never left the request"),
            _ => {}
        }
    };
    f.close().await;
    phase
}

#[tokio::test]
async fn phase_leaves_the_request_after_the_decision() {
    let bash = json!({"command": "sleep 300"});
    let allow = DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    };
    let deny = DecisionPayload::Permission {
        behavior: PermissionBehavior::Deny,
        message: None,
    };
    let answer = DecisionPayload::Question {
        answers: BTreeMap::from([("q0".to_owned(), AnswerValue::Single("да".into()))]),
    };
    let question = json!({"questions":[{"question":"Какой?","header":"Выбор","multiSelect":false,
        "options":[{"label":"да","description":""}]}]});
    assert!(matches!(
        phase_after_decision("Bash", bash.clone(), allow).await,
        adapter::Phase::RunningTool
    ));
    assert!(matches!(
        phase_after_decision("Bash", bash, deny).await,
        adapter::Phase::Generating
    ));
    assert!(matches!(
        phase_after_decision("AskUserQuestion", question, answer).await,
        adapter::Phase::Generating
    ));
}

#[tokio::test]
async fn refused_question_gets_a_deny_and_stays_out_of_the_journal() {
    let id = Uuid::now_v7();
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"approve",
        "arguments":{"tool_name":"AskUserQuestion","tool_use_id":"toolu_question","input":{"questions":[
            {"question":"Какой?","header":"Выбор","multiSelect":false,"options":[{"label":"да","description":""}]}
        ]}},
        "_meta":{"claudecode/toolUseId":"toolu_question"}}});
    let mut f = with_mcp_call(
        call,
        vec![
            json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
            json!("@request:0"),
            json!({"type":"result","subtype":"success","is_error":false,"user_message_uuids":[id]}),
        ],
    );
    f.start().await;
    f.send(&tx(id)).await;
    let mut closed = 0;
    loop {
        match f.event().await {
            AdapterEvent::RequestOpened { request_id, .. } => {
                f.adapter.refuse_request(&request_id, "too large").await;
                let decision = DecisionPayload::Question {
                    answers: BTreeMap::new(),
                };
                assert!(matches!(
                    f.adapter.deliver_decision(&request_id, &decision).await,
                    adapter::DecisionOutcome::NotApplied(reason) if reason == "request_not_pending"
                ));
            }
            AdapterEvent::RequestClosed { .. } => closed += 1,
            AdapterEvent::RunFinished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(
        closed, 0,
        "the supervisor owns the journal of a refused request"
    );
    assert_eq!(
        mcp_reply_text(&f),
        json!({"behavior": "deny", "message": "too large"})
    );
    f.close().await;
}

#[tokio::test]
async fn request_closed_by_interrupt_is_not_closed_again_on_relay_disconnect() {
    let _sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let id = Uuid::now_v7();
    let mut f = Fake::new(vec![vec![
        json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
        json!("@request:0"),
    ]]);
    std::fs::write(
        f.dir.path().join("relay-exe"),
        env!("CARGO_BIN_EXE_canterlot-worker"),
    )
    .unwrap();
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"approve",
        "arguments":{"tool_name":"Bash","input":{"command":"touch held.txt"},"tool_use_id":"toolu_held"},
        "_meta":{"claudecode/toolUseId":"toolu_held"}}});
    std::fs::write(
        f.dir.path().join("mcp-calls.json"),
        serde_json::to_vec(&vec![call]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        f.dir.path().join("interrupt.json"),
        serde_json::to_vec(&vec![json!({"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_tools","user_message_uuids":[id]})]).unwrap(),
    )
    .unwrap();
    f.start().await;
    f.send(&tx(id)).await;
    loop {
        if let AdapterEvent::RequestOpened { request_id, .. } = f.event().await {
            assert_eq!(request_id, "toolu_held");
            break;
        }
    }
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.interrupt())
            .await
            .unwrap(),
        adapter::InterruptOutcome::Acknowledged
    ));
    let mut closed = 0;
    loop {
        match f.event().await {
            AdapterEvent::RequestClosed { request_id, .. } if request_id == "toolu_held" => {
                closed += 1
            }
            AdapterEvent::ProcessExited { .. } => break,
            _ => {}
        }
    }
    // Разрыв relay после выхода CLI шлёт брокерское Cancelled: оно не дублирует закрытие.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while let Ok(event) =
        tokio::time::timeout(Duration::from_millis(200), f.adapter.next_event()).await
    {
        if matches!(&event, AdapterEvent::RequestClosed { request_id, .. } if request_id == "toolu_held")
        {
            closed += 1;
        }
    }
    assert_eq!(closed, 1);
    f.close().await;
}

#[tokio::test]
async fn command_lifecycle_proves_delivery_before_a_slow_first_answer_frame() {
    let id = Uuid::now_v7();
    let mut f = Fake::new(vec![vec![
        json!({"type":"command_lifecycle","command_uuid":"@input","state":"started"}),
        json!("@sleep:900"),
        json!({"type":"stream_event","parent_tool_use_id":null,"user_message_uuids":["@input"],"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
        json!({"type":"result","subtype":"success","is_error":false,"user_message_uuids":["@input"],"modelUsage":{}}),
    ]]);
    // Срок доставки короче паузы двойника перед первым кадром ответа: приём доказывает
    // кадр CLI о старте команды, а не ожидание холодного кэша модели.
    f.adapter = Box::new(adapter::claude_code::ClaudeCodeAdapter::new(
        ClaudeCodeConfig {
            executable: f.dir.path().join("fake-cli"),
            ..Default::default()
        },
        f.dir.path().join("exec"),
        Duration::from_millis(300),
    ));
    f.start().await;
    let outcome = tokio::time::timeout(WAIT, f.adapter.transmit(&tx(id), &f.group))
        .await
        .unwrap();
    assert!(
        matches!(
            outcome,
            TransmitOutcome::Accepted(ref p)
                if p.command_id == id && p.kind == "claude_code_command_lifecycle"
        ),
        "the started frame proves delivery before the answer, got {outcome:?}"
    );
    let _ = f.finish().await;
    f.close().await;
}

#[tokio::test]
async fn late_delivery_proof_after_undetermined_is_reported() {
    let id = Uuid::now_v7();
    let mut f = Fake::new(vec![vec![
        json!("@sleep:900"),
        json!({"type":"stream_event","user_message_uuid":id,"event":{"type":"message_start","message":{"model":"haiku","usage":{}}}}),
        json!({"type":"result","subtype":"success","is_error":false,"result":"ok","user_message_uuids":[id]}),
    ]]);
    // Срок доставки короче паузы двойника перед первым кадром ответа.
    f.adapter = Box::new(adapter::claude_code::ClaudeCodeAdapter::new(
        ClaudeCodeConfig {
            executable: f.dir.path().join("fake-cli"),
            ..Default::default()
        },
        f.dir.path().join("exec"),
        Duration::from_millis(300),
    ));
    f.start().await;
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.transmit(&tx(id), &f.group))
            .await
            .unwrap(),
        TransmitOutcome::Undetermined
    ));
    let events = f.finish().await;
    let accepted = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::InputAccepted { command_id } if *command_id == id))
        .expect("late proof becomes InputAccepted");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::InputAccepted { .. }))
            .count(),
        1
    );
    let finished = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::RunFinished { .. }))
        .unwrap();
    assert!(accepted < finished);
    f.close().await;
}

#[tokio::test]
async fn graceful_close_sends_sigterm_before_sigkill() {
    // Двойник не выходит по EOF (как CLI с фоновой командой): мягкая остановка
    // должна дать ему SIGTERM; упрямый к SIGTERM убивается после stop_grace.
    const STAY: &str = r#"#!/usr/bin/python3
import sys, os, pathlib, signal, time
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
def term(*_):
    (d / 'got-term').write_text('1')
    if not (d / 'ignore-term').exists():
        os._exit(143)
signal.signal(signal.SIGTERM, term)
sys.stdin.read()
(d / 'stdin-eof').write_text('1')
while True:
    time.sleep(0.1)
"#;
    for stubborn in [false, true] {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-cli");
        fake_exec::write_executable(&exe, STAY, 0o700);
        if stubborn {
            std::fs::write(dir.path().join("ignore-term"), "").unwrap();
        }
        let mut adapter: Box<dyn Adapter> = Box::new(adapter::claude_code::ClaudeCodeAdapter::new(
            ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            },
            dir.path().join("exec"),
            WAIT,
        ));
        adapter.set_stop_grace(Duration::from_millis(500));
        let group = TreeContainment::new().unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: dir.path().into(),
            native_conversation_id: None,
            profile: "claude_code".into(),
            model: "haiku".into(),
            effort: "low".into(),
            permission_mode: "ask".into(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            isolate_mcp: false,
        };
        let _permit = CLI_GATE.lock().await;
        adapter.start(&spec, &group).await.unwrap();
        let began = std::time::Instant::now();
        tokio::time::timeout(
            Duration::from_secs(15),
            adapter.close(CloseMode::Graceful, &group),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(dir.path().join("stdin-eof").exists(), "EOF comes first");
        assert!(
            dir.path().join("got-term").exists(),
            "SIGTERM before SIGKILL"
        );
        assert!(began.elapsed() < Duration::from_secs(5));
    }
}

#[tokio::test]
async fn local_command_output_becomes_a_text_block() {
    // `/context` в живом прогоне: один кадр `assistant` с `local_command_source`,
    // модель `<synthetic>`, без потоковых кадров.
    let rows = captured("local_command");
    let events = error_turn(out_frames(&rows)).await;
    let model = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                kind: BlockKind::Text,
                model,
                ..
            } => Some(model.clone()),
            _ => None,
        })
        .expect("text block for the local command output");
    assert_ne!(model, "<synthetic>");
    assert!(events.iter().any(
        |e| matches!(e, AdapterEvent::BlockFinal { text, .. } if text.contains("### Memory Files"))
    ));
}

/// Запись живого прогона с подменой: субагент открывает картинку, а не текстовый файл.
fn subagent_reads_image(path: &str) -> Vec<Value> {
    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let mut frames = out_frames(&captured("subagent_live"));
    let mut swapped = (false, false);
    for frame in &mut frames {
        let in_lane = frame["parent_tool_use_id"].is_string();
        if let Some(blocks) = frame["message"]["content"].as_array_mut() {
            for block in blocks {
                if in_lane
                    && block["name"] == "Read"
                    && block["input"]["file_path"] == "/tmp/canterlot-smoke/data/note.txt"
                {
                    block["input"]["file_path"] = json!(path);
                    swapped.0 = true;
                }
                if in_lane
                    && block["type"] == "tool_result"
                    && block["tool_use_id"] == "toolu_01WWXmcuGU2rr6M4aTK1pt6w"
                {
                    block["content"] = json!([{"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG_1X1}}]);
                    swapped.1 = true;
                }
            }
        }
    }
    assert_eq!(
        swapped,
        (true, true),
        "the capture must hold the subagent Read"
    );
    frames
}

#[tokio::test]
async fn image_read_by_a_subagent_reaches_the_preview_of_its_tool_block() {
    let events = error_turn(subagent_reads_image("/tmp/canterlot-smoke/data/shot.png")).await;
    let call = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::BlockStarted {
                block_id,
                kind: BlockKind::ToolCall,
                tool_name: Some(name),
                agent,
                ..
            } if name == "Read" && agent == "Explore" => Some(*block_id),
            _ => None,
        })
        .expect("subagent Read block");
    let media: Vec<Uuid> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::Media { block_id, .. } => Some(*block_id),
            _ => None,
        })
        .collect();
    assert_eq!(media, [call], "media hangs on the subagent Read block");
}

#[tokio::test]
async fn mid_turn_process_exit_is_reported() {
    let mut f = Fake::new(vec![vec![json!("@exit")]]);
    f.start().await;
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.transmit(&tx(Uuid::now_v7()), &f.group))
            .await
            .unwrap(),
        TransmitOutcome::Undetermined
    ));
    loop {
        if let AdapterEvent::ProcessExited { code, .. } = f.event().await {
            assert_eq!(code, Some(7));
            break;
        }
    }
    f.close().await;
}

#[tokio::test]
async fn malformed_and_oversized_lines_diagnose_and_continue() {
    let events = error_turn(vec![
        json!("not json"),
        json!("x".repeat(64 * 1024 * 1024 + 1)),
        json!({"type":"result","is_error":false}),
    ])
    .await;
    for expected in ["unparsed_line", "oversized_frame"] {
        assert!(
            events
                .iter()
                .any(|e| matches!(e,AdapterEvent::Diagnostic { kind,.. } if *kind==expected))
        );
    }
}

#[tokio::test]
async fn broken_stdin_is_undetermined() {
    let mut f = Fake::new(vec![]);
    std::fs::write(
        f.dir.path().join("fake-cli"),
        r#"#!/usr/bin/python3
import sys, os, pathlib, time
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
os.close(0)
(pathlib.Path(__file__).parent/'stdin-closed').touch()
time.sleep(30)
"#,
    )
    .unwrap();
    f.start().await;
    tokio::time::timeout(WAIT, async {
        while !f.dir.path().join("stdin-closed").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.transmit(&tx(Uuid::now_v7()), &f.group))
            .await
            .unwrap(),
        TransmitOutcome::Undetermined
    ));
    f.close().await;
}

fn smoke_flag(value: Option<&str>) -> bool {
    value == Some("1")
}

fn live_smoke_enabled() -> bool {
    smoke_flag(std::env::var("CANTERLOT_CLAUDE_CODE_SMOKE").ok().as_deref())
}

#[test]
fn live_smoke_is_skipped_without_the_flag() {
    assert!(!smoke_flag(None));
    assert!(!smoke_flag(Some("")));
    assert!(!smoke_flag(Some("true")));
    assert!(smoke_flag(Some("1")));
}

#[tokio::test]
async fn required_diagnostic_kinds_stop_with_matching_reason() {
    let _permit = CLI_GATE.lock().await;
    use canterlot_proto::worker::{
        ipc::{self, control_to_supervisor as down, supervisor_to_control as up},
        v1::{self, record_body::Body},
    };
    use canterlot_worker::supervisor::ipc::{read_message, write_message};
    for (kind, expected) in [
        ("auth_required", "auth_required"),
        ("session_id_mismatch", "cli_incompatible"),
        ("mcp_unavailable", "start_failed"),
        ("unknown_required_frame", "protocol_incompatible"),
        ("cli_too_old", "cli_incompatible"),
    ] {
        let f = Fake::new(vec![]);
        let script = f.dir.path().join("fake-cli");
        // Real supervisor + synthetic local CLI; no worker/hub or network needed.
        let body=match kind {
            "auth_required" => "print(json.dumps({'type':'result','is_error':True,'api_error_status':401,'result':'Please log in','user_message_uuids':[v['uuid']]}),flush=True)".to_owned(),
            "session_id_mismatch" => "print(json.dumps({'type':'system','subtype':'init','session_id':'wrong','mcp_servers':[{'name':'canterlot','status':'connected'}]}),flush=True)".into(),
            "mcp_unavailable" => "print(json.dumps({'type':'system','subtype':'init','session_id':sid,'mcp_servers':[]}),flush=True)".into(),
            "cli_too_old" => "print(json.dumps({'type':'system','subtype':'init','session_id':sid,'claude_code_version':'2.1.1','mcp_servers':[{'name':'canterlot','status':'connected'}]}),flush=True)".into(),
            _ => "print(json.dumps({'type':'system','subtype':'init','session_id':sid,'mcp_servers':[{'name':'canterlot','status':'connected'}]}),flush=True)\n print(json.dumps({'type':'required_test_frame','required':True}),flush=True)".into(),
        };
        fake_exec::write_executable(
            &script,
            format!(
                "#!/usr/bin/python3\nimport sys,json,time\nif '--version' in sys.argv: print('2.1.311');sys.exit(0)\nsid=sys.argv[sys.argv.index('--session-id')+1]\nfor line in sys.stdin:\n v=json.loads(line)\n {body}\n"
            ),
            0o700,
        );
        let dir = f.dir.path().join("supervisor");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.join("secret"), b"local-test-secret").unwrap();
        std::fs::set_permissions(dir.join("secret"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let execution = Uuid::now_v7();
        std::fs::write(dir.join("spec.toml"),format!("execution_id='{execution}'\ngeneration=1\nconversation_id='{}'\nprotocol_version=1\n[timers]\ntransmit_deadline='200ms'\n[claude_code]\nexecutable={}\n",Uuid::now_v7(),toml::Value::String(script.to_string_lossy().into_owned()))).unwrap();
        let mut supervisor = Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
            .args(["supervise", "--exec-dir"])
            .arg(&dir)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stream = tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(s) = tokio::net::UnixStream::connect(dir.join("ipc.sock")).await {
                    break s;
                }
                if let Some(exit) = supervisor.try_wait().unwrap() {
                    panic!("supervisor exited: {exit}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let messages = [
            down::Msg::Attach(ipc::Attach {
                protocol_version: 1,
                secret: b"local-test-secret".to_vec(),
                execution_id: execution.to_string(),
                generation: 1,
            }),
            down::Msg::ShipFrom(ipc::ShipFrom { committed_seq: 0 }),
            down::Msg::StartCli(ipc::StartCli {
                profile: "claude_code".into(),
                workdir: f.dir.path().to_string_lossy().into_owned(),
                model: "haiku".into(),
                effort: "low".into(),
                permission_mode: "ask".into(),
                instructions: Some(canterlot_proto::worker::v1::Instructions {
                    mode: "owner".into(),
                    ..Default::default()
                }),
                pilot_v1: true,
                ..Default::default()
            }),
        ];
        for msg in messages {
            write_message(&mut stream, &ipc::ControlToSupervisor { msg: Some(msg) })
                .await
                .unwrap();
        }
        let id = Uuid::now_v7();
        let mut bodies = vec![];
        let mut delivered = false;
        tokio::time::timeout(WAIT, async {
            loop {
                let response: ipc::SupervisorToControl = read_message(&mut stream).await.unwrap();
                match response.msg.unwrap() {
                    up::Msg::CliStarted(_) if !delivered => {
                        delivered = true;
                        let cmd = v1::Command {
                            command_id: id.to_string(),
                            execution_id: execution.to_string(),
                            execution_generation: 1,
                            dispatch_attempt: 1,
                            kind: Some(v1::command::Kind::DeliverInput(v1::DeliverInput {
                                content: vec![v1::ContentBlock {
                                    block: Some(v1::content_block::Block::Text(v1::TextBlock {
                                        text: "test".into(),
                                    })),
                                }],
                                ..Default::default()
                            })),
                        };
                        write_message(
                            &mut stream,
                            &ipc::ControlToSupervisor {
                                msg: Some(down::Msg::Deliver(ipc::Deliver {
                                    command: Some(cmd),
                                    files: vec![],
                                })),
                            },
                        )
                        .await
                        .unwrap();
                    }
                    up::Msg::SjBatch(batch) => {
                        let last = batch.records.last().unwrap().seq;
                        for r in batch.records {
                            if let Some(record) = r.body
                                && let Some(body) = record.body
                            {
                                bodies.push(body);
                            }
                        }
                        write_message(
                            &mut stream,
                            &ipc::ControlToSupervisor {
                                msg: Some(down::Msg::SjAck(ipc::SjAck { up_to_seq: last })),
                            },
                        )
                        .await
                        .unwrap();
                        if bodies.iter().any(|b| matches!(b, Body::JournalClosed(_))) {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("supervisor closes after diagnostic");
        assert!(
            bodies.iter().any(
                |b| matches!(b,Body::ExecutionState(s) if s.state=="failed" && s.outcome==expected)
            ),
            "{kind}: expected {expected}"
        );
        assert!(bodies.iter().any(|b| matches!(b,Body::AdapterDiagnostic(d) if d.kind=="unknown_required_event" && d.event_type==kind)),"{kind}: diagnostic contract");
        assert!(
            tokio::time::timeout(WAIT, supervisor.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

async fn smoke_turn(f: &mut Fake, input: Transmission) -> Vec<AdapterEvent> {
    smoke_turn_raw(&mut f.adapter, &f.group, input).await
}

async fn smoke_turn_raw(
    adapter: &mut Box<dyn Adapter>,
    group: &TreeContainment,
    input: Transmission,
) -> Vec<AdapterEvent> {
    match tokio::time::timeout(WAIT, adapter.transmit(&input, group))
        .await
        .unwrap()
    {
        TransmitOutcome::Accepted(p) => {
            assert_eq!(p.command_id, input.command_id);
            assert_eq!(p.kind, "claude_code_answer_frame");
        }
        other => panic!("delivery: {other:?}"),
    }
    let mut events = vec![];
    loop {
        let event = tokio::time::timeout(Duration::from_secs(90), adapter.next_event())
            .await
            .expect("live turn deadline");
        if let AdapterEvent::Diagnostic {
            kind,
            required: true,
            ..
        } = &event
        {
            panic!("live diagnostic: {kind}");
        }
        if let AdapterEvent::RequestOpened {
            request_id,
            kind,
            payload,
        } = &event
        {
            let decision = if kind == "question" {
                DecisionPayload::Question {
                    answers: payload["questions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|q| {
                            let answer = q["options"][0]["label"].as_str().unwrap().to_owned();
                            (
                                q["id"].as_str().unwrap().to_owned(),
                                if q["multi_select"] == true {
                                    AnswerValue::Multiple(vec![answer])
                                } else {
                                    AnswerValue::Single(answer)
                                },
                            )
                        })
                        .collect(),
                }
            } else {
                DecisionPayload::Permission {
                    behavior: PermissionBehavior::Allow,
                    message: None,
                }
            };
            assert!(matches!(
                tokio::time::timeout(WAIT, adapter.deliver_decision(request_id, &decision))
                    .await
                    .unwrap(),
                adapter::DecisionOutcome::Applied(_)
            ));
        }
        let done = matches!(event, AdapterEvent::RunFinished { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

/// Удаляет ссылки и каталоги живого прогона при любом исходе.
struct SmokeDirs(Vec<PathBuf>);

impl Drop for SmokeDirs {
    fn drop(&mut self) {
        for path in &self.0 {
            if path.is_symlink() {
                let _ = std::fs::remove_file(path);
            } else {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
}

#[tokio::test]
async fn live_smoke() {
    if !live_smoke_enabled() {
        eprintln!("SKIP: set CANTERLOT_CLAUDE_CODE_SMOKE=1 to run the live turns");
        return;
    }
    let _sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let mut f = Fake::new(vec![]);
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(f.dir.path())
            .status()
            .await
            .unwrap()
            .success()
    );
    let cfg = ClaudeCodeConfig {
        executable: std::env::var_os("CANTERLOT_CLAUDE_CODE_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| "claude".into()),
        ..Default::default()
    };
    f.adapter = Box::new(
        adapter::claude_code::ClaudeCodeAdapter::new(
            cfg,
            f.dir.path().join("exec"),
            Duration::from_secs(60),
        )
        .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))),
    );
    let native = f.start().await;
    let mut input = tx(Uuid::now_v7());
    input.text = "Reply with exactly CANTERLOT-SMOKE. Do not use tools.".into();
    let events = smoke_turn(&mut f, input).await;
    let model = events
        .iter()
        .find_map(|e| {
            if let AdapterEvent::BlockStarted {
                kind: BlockKind::Text,
                model,
                ..
            } = e
            {
                Some(model.clone())
            } else {
                None
            }
        })
        .expect("text answer and actual haiku model");
    assert!(events.iter().any(
        |e| matches!(e,AdapterEvent::BlockFinal {text,..} if text.contains("CANTERLOT-SMOKE"))
    ));
    eprintln!(
        "CONFIRMED: init matches chosen native id, canterlot MCP connected, text streamed; model={model}"
    );
    let five_hour = events.iter().find_map(|e| match e {
        AdapterEvent::Limit(adapter::LimitFact::Observed {
            window: adapter::LimitWindow::FiveHour,
            used_permille,
            resets_at_ms: Some(reset),
        }) => Some((*used_permille, *reset)),
        _ => None,
    });
    let (used, reset) = five_hour.expect("live rate_limit_event gives a five-hour observation");
    eprintln!("CONFIRMED: rate_limit_event observed five-hour {used}‰, resets_at_ms={reset}");

    // Только команда записи: чтение (printf, echo, ls, sleep) CLI разрешает сам,
    // не спрашивая --permission-prompt-tool.
    let mut input = tx(Uuid::now_v7());
    input.text="Use Bash to execute exactly: touch canterlot-permission-smoke.txt. Wait for the owner's approval. Then reply OK.".into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AdapterEvent::RequestOpened {kind,..} if kind=="permission")),
        "manual Bash permission must reach canterlot"
    );
    assert!(
        f.dir.path().join("canterlot-permission-smoke.txt").exists(),
        "allowed Bash call must run"
    );
    eprintln!("CONFIRMED: Bash permission opened and allow delivered, the command ran");

    let mut input = tx(Uuid::now_v7());
    input.text =
        "Call the mcp__canterlot__canterlot_limits tool once, then reply with its exact text."
            .into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        events.iter().any(
            |e| matches!(e, AdapterEvent::BlockFinal {text, ..} if text.contains("five-hour window"))
        ),
        "canterlot_limits answers from the live observations"
    );
    eprintln!("CONFIRMED: canterlot_limits reports the observed windows");

    // Ревью I5: модель не должна открыть владельцу выдуманный запрос через approve.
    let mut input = tx(Uuid::now_v7());
    input.text = "Call the tool mcp__canterlot__approve yourself (load it with ToolSearch if needed) with arguments {\"tool_name\": \"Bash\", \"input\": {\"command\": \"echo fabricated\"}, \"tool_use_id\": \"made-up\"}. If it is unavailable or fails, reply UNAVAILABLE.".into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AdapterEvent::RequestOpened { payload, .. } if payload["tool_name"] != "mcp__canterlot__approve"
        )),
        "a direct approve call must not reach the owner"
    );
    eprintln!("CONFIRMED: a direct approve call opens no fabricated owner request");

    let mut input = tx(Uuid::now_v7());
    input.text="Call AskUserQuestion now, asking 'Smoke choice?' with two options 'Alpha' and 'Bravo'. After the answer, repeat the chosen label.".into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AdapterEvent::RequestOpened {kind,..} if kind=="question"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AdapterEvent::BlockFinal {text,..} if text.contains("Alpha")))
    );
    eprintln!("CONFIRMED: AskUserQuestion answer returned keyed by question text");

    use base64::Engine;
    let png = f.dir.path().join("small.png");
    std::fs::write(&png,base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=").unwrap()).unwrap();
    let mut input = tx(Uuid::now_v7());
    input.text = format!(
        "Use Read to open {} as an image; describe it in one sentence.",
        png.display()
    );
    // The concrete id still names haiku, but differs from the launch alias and exercises set_model.
    input.settings = Some(adapter::SettingsChange {
        model: Some(model),
        ..Default::default()
    });
    let events = smoke_turn(&mut f, input).await;
    let media = events
        .iter()
        .find_map(|e| {
            if let AdapterEvent::Media { source, .. } = e {
                Some(source)
            } else {
                None
            }
        })
        .expect("Read image media");
    eprintln!(
        "CONFIRMED: set_model accepted between turns; Read image source={}",
        match media {
            adapter::MediaSource::Bytes(_) | adapter::MediaSource::Base64(_) => "received bytes",
            adapter::MediaSource::Path(_) => "path fallback",
        }
    );

    let mut input = tx(Uuid::now_v7());
    // CLI сам не пускает `sleep N` на переднем плане и уводит команду в фон;
    // долгий python3 с записью следом спрашивает разрешение и идёт на переднем плане.
    input.text = "Use one Bash call in the foreground (do not set run_in_background) to execute exactly: python3 -c 'import time; time.sleep(60)' && touch canterlot-interrupt-smoke.txt. Do not run anything else.".into();
    f.send(&input).await;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let AdapterEvent::RequestOpened {
                request_id, kind, ..
            } = f.adapter.next_event().await
                && kind == "permission"
            {
                assert!(matches!(
                    f.adapter
                        .deliver_decision(
                            &request_id,
                            &DecisionPayload::Permission {
                                behavior: PermissionBehavior::Allow,
                                message: None
                            }
                        )
                        .await,
                    adapter::DecisionOutcome::Applied(_)
                ));
                break;
            }
        }
    })
    .await
    .expect("sleep permission");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(60), f.adapter.interrupt())
            .await
            .unwrap(),
        adapter::InterruptOutcome::Acknowledged
    ));
    loop {
        if let AdapterEvent::ProcessExited { code, signal } =
            tokio::time::timeout(Duration::from_secs(60), f.adapter.next_event())
                .await
                .unwrap()
        {
            assert_eq!(
                canterlot_worker::supervisor::exit_outcome(code, signal, true),
                ("stopped", "user_stop")
            );
            break;
        }
    }
    f.close().await;
    assert!(
        !f.dir.path().join("canterlot-interrupt-smoke.txt").exists(),
        "interrupted command must not finish"
    );
    eprintln!("CONFIRMED: SIGINT acknowledges aborted_tools and exits as user_stop");

    f.adapter = Box::new(
        adapter::claude_code::ClaudeCodeAdapter::new(
            ClaudeCodeConfig {
                executable: std::env::var_os("CANTERLOT_CLAUDE_CODE_EXECUTABLE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "claude".into()),
                ..Default::default()
            },
            f.dir.path().join("resume-exec"),
            Duration::from_secs(60),
        )
        .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))),
    );
    f.group = TreeContainment::new().unwrap();
    f.spec.native_conversation_id = Some(native.clone());
    assert_eq!(f.start().await, native);
    let mut input = tx(Uuid::now_v7());
    input.text = "Reply with exactly RESUMED, without tools.".into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AdapterEvent::BlockFinal {text,..} if text.contains("RESUMED")))
    );
    f.close().await;
    eprintln!("CONFIRMED: new process resumes same native conversation");

    // План 2c, задача 6, live: пресеты в репозитории под $HOME (предки владельца
    // существуют), настоящее CLI только под флагом smoke.
    let home = std::env::var("HOME").expect("smoke needs HOME");
    let smoke_home = PathBuf::from(format!("{home}/canterlot-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&smoke_home);
    let repo = smoke_home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&repo)
            .status()
            .await
            .unwrap()
            .success()
    );
    std::fs::write(repo.join("CLAUDE.md"), "# canterlot smoke repo\n").unwrap();
    // Ревью M4: рабочий каталог через символическую ссылку вне $HOME. CLI идёт
    // от физического cwd и без исключений по физической цепочке грузит ~/CLAUDE.md.
    let link = PathBuf::from(format!("/tmp/canterlot-smoke-link-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&smoke_home, &link).unwrap();
    // Упавший прогон не оставляет каталог в $HOME и ссылку в /tmp.
    let _cleanup = SmokeDirs(vec![link.clone(), smoke_home.clone()]);
    let linked_repo = link.join("repo");
    let exe = std::env::var_os("CANTERLOT_CLAUDE_CODE_EXECUTABLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| "claude".into());
    let mut preset_adapter: Box<dyn Adapter> = Box::new(
        adapter::claude_code::ClaudeCodeAdapter::new(
            ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            },
            smoke_home.join("exec"),
            Duration::from_secs(60),
        )
        .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))),
    );
    let preset_group = TreeContainment::new().unwrap();
    let preset_spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: linked_repo.clone(),
        native_conversation_id: None,
        profile: "claude_code".into(),
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: InstructionSpec {
            mode: InstructionMode::Clean,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        },
        isolate_mcp: false,
    };
    tokio::time::timeout(WAIT, preset_adapter.start(&preset_spec, &preset_group))
        .await
        .expect("preset start deadline")
        .expect("preset start ok");
    let mut input = tx(Uuid::now_v7());
    input.text = "/context".into();
    let events = smoke_turn_raw(&mut preset_adapter, &preset_group, input).await;
    let context_text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::BlockFinal { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    eprintln!("SMOKE /context: {context_text}");
    assert!(
        !context_text.contains("MEMORY.md"),
        "clean preset must drop auto-memory"
    );
    assert!(
        context_text.contains("CLAUDE.md"),
        "repository file stays in context"
    );
    assert!(
        !context_text.contains(&format!("{home}/CLAUDE.md"))
            && !context_text.contains("~/CLAUDE.md"),
        "owner's home CLAUDE.md must stay out through a symlinked workdir"
    );
    eprintln!(
        "CONFIRMED: clean preset through a symlinked workdir keeps only the repository memory file"
    );

    for canary in ["CANARY-PRESET-A", "CANARY-PRESET-B"] {
        let mut input = tx(Uuid::now_v7());
        input.text =
            "Reply with exactly the canary word from your system prompt or NONE. No tools.".into();
        input.settings = Some(adapter::SettingsChange {
            instructions: Some(InstructionSpec {
                mode: InstructionMode::Preset,
                text: format!("The canary word is {canary}."),
                preset_id: Some(Uuid::now_v7()),
                revision: 1,
                title: canary.into(),
            }),
            ..Default::default()
        });
        let events = smoke_turn_raw(&mut preset_adapter, &preset_group, input).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AdapterEvent::BlockFinal {text, ..} if text.contains(canary))),
            "preset {canary} must reach the model after restart"
        );
    }
    eprintln!("CONFIRMED: preset text reaches the model, CLI restarts between turns");
    let mut input = tx(Uuid::now_v7());
    input.text =
        "Reply with exactly the canary word from your system prompt or NONE. No tools.".into();
    input.settings = Some(adapter::SettingsChange {
        instructions: Some(InstructionSpec {
            mode: InstructionMode::Clean,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        }),
        ..Default::default()
    });
    let events = smoke_turn_raw(&mut preset_adapter, &preset_group, input).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::BlockFinal {text, ..} if text.contains("NONE"))),
        "clean preset must drop the canary"
    );
    for _ in 0..2 {
        let input = tx(Uuid::now_v7());
        let events = smoke_turn_raw(&mut preset_adapter, &preset_group, input).await;
        let cached: u64 = events
            .iter()
            .filter_map(|e| match e {
                AdapterEvent::RunFinished { usage, .. } => {
                    Some(usage.iter().map(|u| u.cache_read_tokens).sum::<u64>())
                }
                _ => None,
            })
            .sum();
        eprintln!("SMOKE cache_read_input_tokens after unchanged turn: {cached}");
    }
    tokio::time::timeout(WAIT, preset_adapter.close(CloseMode::Force, &preset_group))
        .await
        .unwrap()
        .unwrap();
    eprintln!("CONFIRMED: presets smoke done (context, canaries A/B/NONE, cache report)");
}

#[tokio::test]
async fn replay_read_image_and_limits_through_the_adapter() {
    for (name, media, limit) in [
        ("read_image", true, false),
        ("usage_limit_429", false, true),
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude_code")
            .join(format!("{name}.jsonl"));
        let frames = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["dir"] == "out")
            .map(|v| v["data"].clone())
            .collect();
        let events = error_turn(frames).await;
        assert_eq!(events.iter().any(|e|matches!(e,AdapterEvent::Media {source:adapter::MediaSource::Bytes(bytes),..} if !bytes.is_empty())),media);
        assert_eq!(
            events.iter().any(|e| matches!(
                e,
                AdapterEvent::Limit(adapter::LimitFact::Reached {
                    resets_at_ms: Some(_),
                    ..
                })
            )),
            limit
        );
        if media {
            assert_eq!(events.iter().filter(|e|matches!(e,AdapterEvent::BlockStarted {kind:BlockKind::ToolCall,approved_by:Some(mark),..} if mark=="cli_policy")).count(),1);
            let ids: Vec<_> = events
                .iter()
                .filter_map(|e| {
                    if let AdapterEvent::BlockStarted { block_id, .. } = e {
                        Some(*block_id)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(
                ids.len(),
                ids.iter().collect::<std::collections::HashSet<_>>().len(),
                "block starts must be unique"
            );
        }
    }
}

fn answer_turn() -> Vec<Value> {
    vec![
        json!({"type":"result","subtype":"success","is_error":false,"user_message_uuids":["@input"],"modelUsage":{}}),
    ]
}

fn events_of(events: &[AdapterEvent], kind: &str) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AdapterEvent::Diagnostic { kind: k, .. } if *k == kind))
        .count()
}

#[tokio::test]
async fn slow_init_within_ready_deadline_is_not_a_protocol_failure() {
    let mut f = Fake::with_timers(
        vec![answer_turn()],
        Duration::from_millis(300),
        Duration::from_secs(20),
    );
    std::fs::write(f.dir.path().join("init-delay"), "1200").unwrap();
    f.start().await;
    let id = Uuid::now_v7();
    f.send(&tx(id)).await;
    let events = f.finish().await;
    assert_eq!(events_of(&events, "cli_init_missing"), 0, "{events:?}");
    f.close().await;
}

#[tokio::test]
async fn absent_init_at_ready_deadline_stops_as_start_failure_cause() {
    let mut f = Fake::with_timers(
        vec![answer_turn()],
        Duration::from_millis(200),
        Duration::from_millis(900),
    );
    std::fs::write(f.dir.path().join("init-delay"), "30000").unwrap();
    f.start().await;
    let began = std::time::Instant::now();
    let out = f.adapter.transmit(&tx(Uuid::now_v7()), &f.group).await;
    assert!(matches!(out, TransmitOutcome::Undetermined), "{out:?}");
    assert!(
        began.elapsed() >= Duration::from_millis(800),
        "{:?}",
        began.elapsed()
    );
    let mut seen = vec![];
    while let Ok(e) = tokio::time::timeout(Duration::from_millis(200), f.adapter.next_event()).await
    {
        seen.push(e);
    }
    assert_eq!(events_of(&seen, "cli_init_missing"), 1, "{seen:?}");
    f.close().await;
}

#[tokio::test]
async fn restart_waits_for_the_new_process_to_start() {
    let mut f = Fake::with_timers(
        vec![answer_turn()],
        Duration::from_millis(300),
        Duration::from_secs(20),
    );
    f.start().await;
    f.send(&tx(Uuid::now_v7())).await;
    f.finish().await;
    std::fs::write(f.dir.path().join("init-delay"), "1200").unwrap();
    let mut change = tx(Uuid::now_v7());
    change.settings = Some(SettingsChange {
        instructions: Some(InstructionSpec {
            mode: InstructionMode::Clean,
            text: String::new(),
            preset_id: None,
            revision: 1,
            title: String::new(),
        }),
        ..Default::default()
    });
    f.send(&change).await;
    let events = f.finish().await;
    assert_eq!(events_of(&events, "cli_init_missing"), 0, "{events:?}");
    f.close().await;
}

#[tokio::test]
async fn running_turn_answered_before_init_waits_for_init_instead_of_failing() {
    let mut f = Fake::with_timers(
        vec![vec![
            json!({"type":"assistant","message":{"role":"assistant","content":[],"model":"claude-haiku-test"},"user_message_uuids":["@input"]}),
            json!("@sleep:700"),
        ]],
        Duration::from_millis(200),
        Duration::from_secs(20),
    );
    std::fs::write(f.dir.path().join("init-mode"), "after").unwrap();
    f.start().await;
    f.send(&tx(Uuid::now_v7())).await;
    let mut seen = vec![];
    while let Ok(e) = tokio::time::timeout(Duration::from_millis(300), f.adapter.next_event()).await
    {
        seen.push(e);
    }
    assert_eq!(events_of(&seen, "cli_init_missing"), 0, "{seen:?}");
    f.close().await;
}

#[tokio::test]
async fn finished_turn_without_init_does_not_wait_for_the_ready_deadline() {
    let mut f = Fake::with_timers(
        vec![answer_turn()],
        Duration::from_millis(200),
        Duration::from_secs(20),
    );
    std::fs::write(f.dir.path().join("init-mode"), "none").unwrap();
    f.start().await;
    let began = std::time::Instant::now();
    f.send(&tx(Uuid::now_v7())).await;
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    let mut seen = vec![];
    while let Ok(e) = tokio::time::timeout(Duration::from_millis(200), f.adapter.next_event()).await
    {
        seen.push(e);
    }
    assert_eq!(events_of(&seen, "cli_init_missing"), 1, "{seen:?}");
    f.close().await;
}

#[tokio::test]
async fn running_turn_without_init_is_accepted_at_ready_deadline_with_a_diagnostic() {
    let mut f = Fake::with_timers(
        vec![vec![
            json!({"type":"assistant","message":{"role":"assistant","content":[],"model":"claude-haiku-test"},"user_message_uuids":["@input"]}),
            json!("@sleep:5000"),
        ]],
        Duration::from_millis(200),
        Duration::from_millis(900),
    );
    std::fs::write(f.dir.path().join("init-mode"), "none").unwrap();
    f.start().await;
    let began = std::time::Instant::now();
    f.send(&tx(Uuid::now_v7())).await;
    assert!(
        began.elapsed() >= Duration::from_millis(800),
        "{:?}",
        began.elapsed()
    );
    let mut seen = vec![];
    while let Ok(e) = tokio::time::timeout(Duration::from_millis(200), f.adapter.next_event()).await
    {
        seen.push(e);
    }
    assert_eq!(events_of(&seen, "cli_init_missing"), 1, "{seen:?}");
    f.close().await;
}

#[tokio::test]
async fn compact_sends_slash_compact_and_reports_the_manual_boundary() {
    use canterlot_worker::adapter::CompactOutcome;
    let mut f = Fake::new(vec![
        answer_turn(),
        vec![
            json!({"type":"system","subtype":"status","status":"compacting"}),
            json!({"type":"system","subtype":"compact_boundary",
                "compact_metadata":{"pre_tokens":41000,"trigger":"manual"}}),
            json!({"type":"result","subtype":"success","is_error":false,"modelUsage":{}}),
        ],
    ]);
    f.start().await;
    let id = Uuid::now_v7();
    f.send(&tx(id)).await;
    let _ = f.finish().await;
    let outcome = tokio::time::timeout(WAIT, f.adapter.compact(Uuid::now_v7()))
        .await
        .unwrap();
    assert!(matches!(outcome, CompactOutcome::Accepted), "{outcome:?}");
    let mut compacted = None;
    let mut finished = None;
    for _ in 0..100 {
        match tokio::time::timeout(Duration::from_secs(2), f.adapter.next_event()).await {
            Ok(AdapterEvent::Compacted { ok }) => compacted = Some(ok),
            Ok(AdapterEvent::CompactionFinished {
                manual,
                ok,
                pre_tokens,
                post_tokens,
            }) => finished = Some((manual, ok, pre_tokens, post_tokens)),
            Ok(_) => continue,
            Err(_) => break,
        }
        if compacted.is_some() && finished.is_some() {
            break;
        }
    }
    assert_eq!(compacted, Some(true), "command flow must complete");
    assert_eq!(
        finished,
        Some((true, true, 41_000, 0)),
        "manual episode with tokens"
    );
    let written: Vec<Value> = std::fs::read_to_string(f.dir.path().join("stdin.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        written
            .iter()
            .any(|message| message["message"]["content"] == "/compact"),
        "compact goes as a /compact input: {written:?}"
    );
    f.close().await;
}

#[tokio::test]
async fn compacted_waits_for_service_result_after_boundary() {
    use canterlot_worker::adapter::CompactOutcome;
    // Порядок кадров — из tests/fixtures/claude_code/compaction.jsonl: граница
    // ручного сжатия, затем служебный итог без uuids владельца.
    let mut f = Fake::new(vec![
        answer_turn(),
        vec![
            json!({"type":"system","subtype":"status","status":"compacting"}),
            json!({"type":"system","subtype":"compact_boundary",
                "compact_metadata":{"pre_tokens":41000,"trigger":"manual"}}),
            json!({"type":"result","subtype":"success","is_error":false,"modelUsage":{}}),
        ],
        answer_turn(),
    ]);
    f.start().await;
    let id = Uuid::now_v7();
    f.send(&tx(id)).await;
    let _ = f.finish().await;
    let outcome = tokio::time::timeout(WAIT, f.adapter.compact(Uuid::now_v7()))
        .await
        .unwrap();
    assert!(matches!(outcome, CompactOutcome::Accepted), "{outcome:?}");
    // Граница прошла, служебный итог ещё нет: команда сжатия открыта.
    let mut finished = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_secs(2), f.adapter.next_event()).await {
            Ok(AdapterEvent::Compacted { .. }) => {
                panic!("Compacted must wait for the service result, not the boundary");
            }
            Ok(AdapterEvent::CompactionFinished { manual, ok, .. }) => {
                assert!((manual, ok) == (true, true));
                finished = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(finished, "boundary must finish the episode");
    // Служебный итог потреблён: команда сжатия завершена.
    let mut compacted = None;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_secs(2), f.adapter.next_event()).await {
            Ok(AdapterEvent::Compacted { ok }) => {
                compacted = Some(ok);
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert_eq!(
        compacted,
        Some(true),
        "command flow completes after the result"
    );
    // Придержанный ввод уходит по порядку, один раз, с доказательством доставки.
    let owner = Uuid::now_v7();
    f.send(&tx(owner)).await;
    let events = f.finish().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::RunFinished { .. })),
        "held input must finish its turn: {events:?}"
    );
    let written: Vec<Value> = std::fs::read_to_string(f.dir.path().join("stdin.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        written
            .iter()
            .filter(|message| message["uuid"] == owner.to_string())
            .count(),
        1,
        "held input is delivered exactly once: {written:?}"
    );
    f.close().await;
}
