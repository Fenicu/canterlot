#![forbid(unsafe_code)]

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        self, Adapter, AdapterContext, AdapterEvent, BlockKind, CloseMode, StartSpec, Transmission,
        TransmitOutcome, claude_code::version::CliVersion,
    },
    config::ClaudeCodeConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tempfile::TempDir;
use tokio::process::Command;
use uuid::Uuid;

static CLI_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAIT: Duration = Duration::from_secs(15);

// Local pipes and Unix sockets only. The script never resolves or invokes claude.
const FAKE: &str = r#"#!/usr/bin/python3
import json, sys, pathlib, signal, select, time, subprocess
d=pathlib.Path(__file__).parent
if '--version' in sys.argv:
    v=(d/'version').read_text() if (d/'version').exists() else '2.1.311'
    print(v); sys.exit(1 if v=='failed' else 0)
(d/'args.json').write_text(json.dumps(sys.argv[1:]))
sid=sys.argv[sys.argv.index('--resume')+1] if '--resume' in sys.argv else sys.argv[sys.argv.index('--session-id')+1]
def emit(v):
    if isinstance(v,str): print(v,flush=True); return
    if v.get('subtype')=='init': v=dict(v,session_id=sid)
    print(json.dumps(v),flush=True)
def interrupted(*_):
    for v in json.loads((d/'interrupt.json').read_text()): emit(v)
    sys.exit(0)
signal.signal(signal.SIGINT,interrupted)
relay=None
for n,line in enumerate(sys.stdin,1):
    with (d/'stdin.log').open('a') as f: f.write(line)
    v=json.loads(line)
    if v.get('type')=='control_request':
        emit({'type':'control_response','response':{'subtype':'success','request_id':v['request_id'],'response':{}}}); continue
    emit({'type':'system','subtype':'init','session_id':sid,'model':'claude-haiku-test','permissionMode':'default','mcp_servers':[{'name':'canterlot','status':'connected'}]})
    frames=json.loads((d/('turn'+str(n)+'.json')).read_text())
    for frame in frames:
        if frame=='@probe':
            if select.select([sys.stdin],[],[],0.2)[0]: (d/'early').write_text('early input')
        elif isinstance(frame,str) and frame.startswith('@request:'):
            if relay is None:
                exec_dir=pathlib.Path(sys.argv[sys.argv.index('--mcp-config')+1]).parent
                relay=subprocess.Popen([(d/'relay-exe').read_text(),'mcp-relay','--socket',str(exec_dir/'mcp.sock')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
            call=json.loads((d/'mcp-calls.json').read_text())[int(frame.split(':')[1])]
            relay.stdin.write(json.dumps(call)+'\n'); relay.stdin.flush()
            if not select.select([relay.stdout],[],[],10)[0]: raise RuntimeError('MCP reply timeout')
            with (d/'mcp-replies.jsonl').open('a') as replies: replies.write(relay.stdout.readline())
        elif frame=='@wait':
            while True: time.sleep(1)
        elif frame=='@exit': sys.exit(7)
        else: emit(frame)
if relay is not None:
    relay.stdin.close(); relay.wait(timeout=10)
"#;

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
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-cli");
        std::fs::write(&exe, FAKE).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        for (n, turn) in turns.iter().enumerate() {
            std::fs::write(
                dir.path().join(format!("turn{}.json", n + 1)),
                serde_json::to_vec(turn).unwrap(),
            )
            .unwrap();
        }
        let context = AdapterContext {
            transmit_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: Some(ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            }),
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
            instruction_profile: "owner".into(),
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
        match tokio::time::timeout(WAIT, self.adapter.transmit(t))
            .await
            .unwrap()
        {
            TransmitOutcome::Accepted(p) => {
                assert_eq!(p.command_id, t.command_id);
                assert_eq!(p.kind, "claude_code_answer_frame");
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
        !adapter::catalog_with(None, Some(&cli))
            .iter()
            .any(|c| c.harness == "claude_code")
    );
    assert!(
        !adapter::catalog_with(Some(&cfg), None)
            .iter()
            .any(|c| c.harness == "claude_code")
    );
    let catalog = adapter::catalog_with(Some(&cfg), Some(&cli));
    let c = catalog.iter().find(|c| c.harness == "claude_code").unwrap();
    assert_eq!(c.provider, "claude");
    assert_eq!(c.title, "Claude Code");
    assert!(c.images && c.settings_midsession);
    assert!(!c.settings_midsession_effort);
    assert_eq!(c.permission_modes, ["owner", "ask", "bypass", "read_only"]);
    assert_eq!(
        c.models.iter().map(|m| m.id).collect::<Vec<_>>(),
        ["opus", "sonnet", "haiku"]
    );
    for m in &c.models {
        assert_eq!(m.efforts, ["low", "medium", "high", "xhigh", "max"]);
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
            if let Ok(bytes) = std::fs::read(second.dir.path().join("args.json")) {
                break serde_json::from_slice(&bytes).unwrap();
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

#[tokio::test]
async fn mid_turn_process_exit_is_reported() {
    let mut f = Fake::new(vec![vec![json!("@exit")]]);
    f.start().await;
    assert!(matches!(
        tokio::time::timeout(WAIT, f.adapter.transmit(&tx(Uuid::now_v7())))
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
        tokio::time::timeout(WAIT, f.adapter.transmit(&tx(Uuid::now_v7())))
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
        std::fs::write(&script,format!("#!/usr/bin/python3\nimport sys,json,time\nif '--version' in sys.argv: print('2.1.311');sys.exit(0)\nsid=sys.argv[sys.argv.index('--session-id')+1]\nfor line in sys.stdin:\n v=json.loads(line)\n {body}\n")).unwrap();
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
                instruction_profile: "owner".into(),
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
    f.send(&input).await;
    let mut events = vec![];
    loop {
        let event = tokio::time::timeout(Duration::from_secs(90), f.adapter.next_event())
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
                tokio::time::timeout(WAIT, f.adapter.deliver_decision(request_id, &decision))
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

#[tokio::test]
async fn live_smoke() {
    if !live_smoke_enabled() {
        eprintln!("SKIP: set CANTERLOT_CLAUDE_CODE_SMOKE=1 to run six live turns");
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

    let mut input = tx(Uuid::now_v7());
    input.text="Use Bash to execute exactly: printf canterlot-permission-smoke. Wait for the owner's approval. Then reply OK.".into();
    let events = smoke_turn(&mut f, input).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e,AdapterEvent::RequestOpened {kind,..} if kind=="permission")),
        "manual Bash permission must reach canterlot"
    );
    eprintln!("CONFIRMED: Bash permission opened and allow delivered");

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
            adapter::MediaSource::Bytes(_) => "received bytes",
            adapter::MediaSource::Path(_) => "path fallback",
        }
    );

    let mut input = tx(Uuid::now_v7());
    input.text = "Use Bash to execute exactly: sleep 60. Do not run anything else.".into();
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
    eprintln!("CONFIRMED: new process resumes same native conversation; six turns total");
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
