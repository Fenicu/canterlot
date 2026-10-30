#![forbid(unsafe_code)]
#![cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]

use canterlot_journal::Journal;
use canterlot_proto::{
    requests::{DecisionPayload, PermissionBehavior},
    worker::{
        ipc::{
            Attach, ControlToSupervisor, Deliver, InterruptRequest, ShipFrom, SjAck, StartCli,
            SupervisorToControl, control_to_supervisor as down, supervisor_to_control as up,
        },
        v1::{
            Command, ContentBlock, DeliverDecision, DeliverInput, Instructions, RecordBody,
            TextBlock, command::Kind, content_block, record_body::Body,
        },
    },
};
use canterlot_worker::supervisor::ipc::{read_message, write_message};
use prost::Message;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{net::UnixStream, process::Child, time::timeout};
use uuid::Uuid;

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/process_guard.rs"]
mod process_guard;

const WAIT: Duration = Duration::from_secs(5);

struct Harness {
    guard: process_guard::ProcessGuard,
    root: tempfile::TempDir,
    dir: PathBuf,
    execution: String,
    child: Child,
    conn: Option<UnixStream>,
    bodies: Vec<Body>,
    last: u64,
}
impl Harness {
    async fn new(fault: Option<&str>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("exec");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("secret"), b"requests-test-secret").unwrap();
        fs::set_permissions(dir.join("secret"), fs::Permissions::from_mode(0o600)).unwrap();
        let execution = Uuid::now_v7().to_string();
        fs::write(dir.join("spec.toml"), format!("execution_id = \"{execution}\"\ngeneration = 7\nconversation_id = \"{}\"\nprotocol_version = 1\n[timers]\nattach_wait = \"100ms\"\ntransmit_deadline = \"200ms\"\n", Uuid::now_v7())).unwrap();
        let child = Self::spawn(&dir, root.path(), fault);
        Self {
            guard: process_guard::ProcessGuard::new(root.path()),
            root,
            dir,
            execution,
            child,
            conn: None,
            bodies: vec![],
            last: 0,
        }
    }
    fn spawn(dir: &std::path::Path, root: &std::path::Path, fault: Option<&str>) -> Child {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"));
        cmd.args(["supervise", "--exec-dir"])
            .arg(dir)
            .env("CANTERLOT_SYNTHETIC_STDIN_LOG", root.join("stdin.jsonl"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(fault) = fault {
            if let Some(point) = fault.strip_prefix("hold:") {
                cmd.env("CANTERLOT_FAULT_HOLD_AT", point);
            } else {
                cmd.env("CANTERLOT_FAULT_ABORT_AT", fault);
            }
        }
        cmd.spawn().unwrap()
    }
    async fn attach(&mut self, committed: u64) {
        let stream = timeout(WAIT, async {
            loop {
                if let Ok(s) = UnixStream::connect(self.dir.join("ipc.sock")).await {
                    break s;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.conn = Some(stream);
        self.send(down::Msg::Attach(Attach {
            protocol_version: 1,
            secret: b"requests-test-secret".to_vec(),
            execution_id: self.execution.clone(),
            generation: 7,
        }))
        .await;
        assert!(matches!(self.recv().await, up::Msg::Attached(_)));
        self.send(down::Msg::ShipFrom(ShipFrom {
            committed_seq: committed,
        }))
        .await;
    }
    async fn send(&mut self, msg: down::Msg) {
        write_message(
            self.conn.as_mut().unwrap(),
            &ControlToSupervisor { msg: Some(msg) },
        )
        .await
        .unwrap();
    }
    async fn recv(&mut self) -> up::Msg {
        let msg: SupervisorToControl = timeout(WAIT, read_message(self.conn.as_mut().unwrap()))
            .await
            .expect("supervisor reply deadline")
            .expect("supervisor connection closed");
        msg.msg.unwrap()
    }
    async fn wait(&mut self, predicate: impl Fn(&Body) -> bool) -> Body {
        timeout(WAIT, async {
            loop {
                if let Some(body) = self.bodies.iter().find(|b| predicate(b)) {
                    return body.clone();
                }
                if let up::Msg::SjBatch(batch) = self.recv().await {
                    self.last = batch.records.last().unwrap().seq;
                    self.bodies
                        .extend(batch.records.into_iter().filter_map(|r| r.body?.body));
                    self.send(down::Msg::SjAck(SjAck {
                        up_to_seq: self.last,
                    }))
                    .await;
                }
            }
        })
        .await
        .expect("expected SJ request fact")
    }
    async fn start(&mut self, steps: Value) -> String {
        self.start_profile(steps, "synthetic").await
    }
    async fn start_profile(&mut self, steps: Value, profile: &str) -> String {
        self.launch(steps, profile).await;
        match self.wait(|b| matches!(b, Body::RequestOpened(_))).await {
            Body::RequestOpened(r) => r.request_id,
            _ => unreachable!(),
        }
    }
    async fn launch(&mut self, steps: Value, profile: &str) {
        self.attach(0).await;
        self.send(down::Msg::StartCli(StartCli {
            profile: profile.into(),
            workdir: self.root.path().to_string_lossy().into_owned(),
            pilot_v1: true,
            instructions: Some(Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
        loop {
            if matches!(self.recv().await, up::Msg::CliStarted(_)) {
                break;
            }
        }
        self.send(down::Msg::Deliver(Deliver {
            command: Some(Command {
                command_id: Uuid::now_v7().to_string(),
                dispatch_attempt: 1,
                execution_id: self.execution.clone(),
                execution_generation: 7,
                kind: Some(Kind::DeliverInput(DeliverInput {
                    content: vec![ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock {
                            text: json!({"synthetic":steps}).to_string(),
                        })),
                    }],
                    ..Default::default()
                })),
            }),
            files: vec![],
        }))
        .await;
    }
    fn decision(request: &str, generation: u64, payload: DecisionPayload) -> DeliverDecision {
        let kind = match payload {
            DecisionPayload::Permission { .. } => "permission",
            DecisionPayload::Question { .. } => "question",
        };
        DeliverDecision {
            request_id: request.into(),
            decision_id: Uuid::now_v7().to_string(),
            generation,
            kind: kind.into(),
            decision_payload_json: serde_json::to_vec(&payload).unwrap(),
        }
    }
    fn allow(request: &str, generation: u64) -> DeliverDecision {
        Self::decision(
            request,
            generation,
            DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: None,
            },
        )
    }
    fn stdin(&self) -> Vec<Value> {
        fs::read_to_string(self.root.path().join("stdin.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn sj(&self) -> Vec<Body> {
        let sj = Journal::open_read_only(&self.dir.join("sj")).unwrap();
        sj.read_from(sj.first_seq(), usize::MAX)
            .unwrap()
            .into_iter()
            .map(|r| {
                RecordBody::decode(r.payload.as_slice())
                    .unwrap()
                    .body
                    .unwrap()
            })
            .collect()
    }
}
fn permission() -> Value {
    json!([{"request_permission":{"tool_name":"Bash","command":"cargo test","args":{"raw":"unchanged"}}},{"delta":"resumed"}])
}

#[tokio::test]
async fn supervisor_fsyncs_transmission_started_before_adapter_delivery() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(Some("after_decision_intention")).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    let status = timeout(WAIT, h.child.wait())
        .await
        .expect("fault must kill supervisor")
        .unwrap();
    assert!(!status.success());
    let bodies = h.sj();
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::DecisionTransmissionStarted(f) if f.request_id == request)),
        "fsync'd intention must survive the crash"
    );
    assert!(
        !h.stdin().iter().any(|f| f["type"] == "decision"),
        "adapter must not receive a byte before durable intention"
    );
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.bodies.clear();
    h.attach(0).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"))
        .await;
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
}

#[tokio::test]
async fn at_most_one_decision_transmission_under_replayed_command() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    let decision = Harness::allow(&request, 7);
    h.send(down::Msg::DeliverDecision(decision.clone())).await;
    h.wait(|b| matches!(b, Body::DecisionApplied(_))).await;
    h.send(down::Msg::DeliverDecision(decision)).await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"))
        .await;
    assert_eq!(
        h.stdin().iter().filter(|f| f["type"] == "decision").count(),
        1
    );
    assert_eq!(
        h.bodies
            .iter()
            .filter(|b| matches!(b, Body::DecisionTransmissionStarted(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn mismatched_generation_rejects_decision_without_calling_adapter() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 6)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "stale_generation"))
        .await;
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionApplied(_))).await;
}

#[tokio::test]
async fn turn_interrupt_while_request_pending_records_request_closed() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::Interrupt(InterruptRequest {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
    }))
    .await;
    h.wait(|b| matches!(b, Body::RequestClosed(f) if f.request_id == request && f.reason == "interrupted")).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "request_closed"))
        .await;
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
}

#[tokio::test]
async fn disconnection_pauses_agent_under_wait_policy_without_timeout() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.conn.take();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let bodies = h.sj();
    assert!(!bodies.iter().any(|b| matches!(
        b,
        Body::DecisionTransmissionStarted(_)
            | Body::DecisionApplied(_)
            | Body::DecisionNotApplied(_)
            | Body::RequestClosed(_)
            | Body::RunFinished(_)
    )));
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
    let committed = h.last;
    h.attach(committed).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
}

#[tokio::test]
async fn supervisor_restart_reannounces_open_requests_from_sj() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    // Запрос уже подтверждён хабом; восстановление обязано объявить его новым фактом SJ.
    let committed = h.last;
    h.child.kill().await.unwrap();
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.bodies.clear();
    h.attach(committed).await;
    h.wait(|b| matches!(b, Body::RequestOpened(r) if r.request_id == request))
        .await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "request_closed"))
        .await;
    let committed = h.last;
    h.conn.take();
    h.bodies.clear();
    h.attach(committed).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "request_closed"))
        .await;
    assert!(
        !h.bodies.iter().any(|b| matches!(b, Body::RequestOpened(_))),
        "not_applied request must stay terminal across recovery"
    );
}

#[tokio::test]
async fn synthetic_adapter_pauses_on_permission_and_resumes_on_allow() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    assert!(!h.sj().iter().any(|b| matches!(b, Body::RunFinished(_))));
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    let applied = h.wait(|b| matches!(b, Body::DecisionApplied(_))).await;
    if let Body::DecisionApplied(f) = applied {
        assert_eq!(f.proof_kind, "tool_result");
    }
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
}

#[tokio::test]
async fn oversized_permission_is_journaled_as_a_marked_preview() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h
        .start(json!([{"request_large":{"tool_name":"Write","repeat":"щ","times":400_000}},{"delta":"resumed"}]))
        .await;
    let Body::RequestOpened(opened) = h.wait(|b| matches!(b, Body::RequestOpened(_))).await else {
        unreachable!()
    };
    assert!(
        opened.payload_json.len() <= 262_144,
        "{} bytes",
        opened.payload_json.len()
    );
    let payload: Value = serde_json::from_slice(&opened.payload_json).unwrap();
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["tool_name"], "Write");
    assert_eq!(payload["tool_use_id"], "toolu_large");
    assert_eq!(payload["input"]["file_path"], "/tmp/x");
    let preview = payload["input"]["content"].as_str().unwrap();
    assert!(!preview.is_empty() && preview.chars().all(|c| c == 'щ'));
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionApplied(_))).await;
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
}

#[tokio::test]
async fn payload_within_the_limit_stays_unmarked() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    h.start(json!([{"request_large":{"tool_name":"Write","repeat":"щ","times":1000}}]))
        .await;
    let Body::RequestOpened(opened) = h.wait(|b| matches!(b, Body::RequestOpened(_))).await else {
        unreachable!()
    };
    let payload: Value = serde_json::from_slice(&opened.payload_json).unwrap();
    assert_eq!(payload.get("truncated"), None);
    assert_eq!(payload["input"]["content"].as_str().unwrap().len(), 2000);
}

#[tokio::test]
async fn payload_without_a_preview_is_denied_and_never_reaches_the_journal() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    h.launch(
        json!([{"request_large":{"tool_name":"Write","repeat":"щ","times":400_000,"bare":true}},{"delta":"resumed"}]),
        "synthetic",
    )
    .await;
    let diagnostic = h
        .wait(|b| matches!(b, Body::AdapterDiagnostic(f) if f.event_type == "request_too_large"))
        .await;
    assert!(matches!(diagnostic, Body::AdapterDiagnostic(_)));
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
    assert!(!h.bodies.iter().any(|b| matches!(b, Body::RequestOpened(_))));
    assert_eq!(
        h.stdin().iter().filter(|f| f["type"] == "decision").count(),
        1
    );
    assert!(
        h.bodies
            .iter()
            .any(|b| matches!(b, Body::BlockFinal(f) if f.text == "denied")),
        "the agent must get an explicit deny"
    );
}

#[tokio::test]
async fn oversized_block_summary_does_not_stop_the_supervisor() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    h.launch(
        json!([{"large_summary":{"repeat":"щ","times":600_000}},{"delta":"after"}]),
        "synthetic",
    )
    .await;
    let Body::BlockStarted(started) = h.wait(|b| matches!(b, Body::BlockStarted(_))).await else {
        unreachable!()
    };
    assert!(started.summary.len() <= 4096, "{}", started.summary.len());
    assert!(started.summary.chars().all(|c| c == 'щ'));
    let Body::BlockFinal(finished) = h
        .wait(|b| matches!(b, Body::BlockFinal(f) if f.block_id == started.block_id))
        .await
    else {
        unreachable!()
    };
    assert!(finished.summary.len() <= 4096, "{}", finished.summary.len());
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
}

#[tokio::test]
async fn synthetic_adapter_pauses_on_multi_question_and_resumes_on_answers() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let questions = json!({"questions":[{"id":"q2","text":"Same text?","options":["Keep (Recommended)","No"]},{"id":"q1","text":"Same text?","options":["A","B"],"multi_select":true},{"id":"q3","text":"Free?","options":[],"allow_custom":true},{"id":"q4","text":"Last?","options":["Raw"]}]});
    let request = h
        .start(json!([{"ask_question":questions},{"delta":"resumed"}]))
        .await;
    assert!(!h.sj().iter().any(|b| matches!(b, Body::RunFinished(_))));
    let payload: DecisionPayload = serde_json::from_value(json!({"kind":"question","answers":{"q1":["B","A"],"q2":"Keep (Recommended)","q3":"Точный ответ","q4":"Raw"}})).unwrap();
    h.send(down::Msg::DeliverDecision(Harness::decision(
        &request,
        7,
        payload.clone(),
    )))
    .await;
    let Body::DecisionApplied(f) = h.wait(|b| matches!(b, Body::DecisionApplied(_))).await else {
        unreachable!()
    };
    let proof: Value = serde_json::from_slice(&f.proof_payload_json).unwrap();
    assert_eq!(proof["decision"], serde_json::to_value(payload).unwrap());
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
}

#[tokio::test]
async fn journal_write_failure_prevents_adapter_decision_delivery() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    // Ошибка открытия активного сегмента при записи намерения.
    for entry in fs::read_dir(h.dir.join("sj")).unwrap().flatten() {
        if entry.file_name().to_string_lossy().starts_with("seg-") {
            fs::remove_file(entry.path()).unwrap();
            fs::create_dir(entry.path()).unwrap();
        }
    }
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    let status = timeout(WAIT, h.child.wait())
        .await
        .expect("journal failure must stop supervisor")
        .unwrap();
    assert!(!status.success());
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
}

#[tokio::test]
async fn invalid_question_answers_do_not_consume_the_request() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(json!([{"ask_question":{"questions":[{"id":"q1","text":"Deploy?","options":["yes","no"]}]}}])).await;
    let invalid: DecisionPayload =
        serde_json::from_value(json!({"kind":"question","answers":{"q1":"arbitrary"}})).unwrap();
    h.send(down::Msg::DeliverDecision(Harness::decision(
        &request, 7, invalid,
    )))
    .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "invalid_decision"))
        .await;
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
    let valid: DecisionPayload =
        serde_json::from_value(json!({"kind":"question","answers":{"q1":"yes"}})).unwrap();
    h.send(down::Msg::DeliverDecision(Harness::decision(
        &request, 7, valid,
    )))
    .await;
    h.wait(|b| matches!(b, Body::DecisionApplied(_))).await;
}

#[tokio::test]
async fn synthetic_codex_enables_default_questions_and_experimental_api() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    h.start_profile(permission(), "synthetic-codex").await;
    let frames = h.stdin();
    assert!(
        frames
            .iter()
            .any(|f| f["type"] == "initialize" && f["capabilities"]["experimentalApi"] == true),
        "runner must initialize experimentalApi"
    );
    let configuration = h.sj();
    assert!(
        configuration
            .iter()
            .any(|f| matches!(f, Body::RequestOpened(_)))
    );
}

#[tokio::test]
async fn applied_decision_without_record_is_never_retransmitted_after_restart() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(Some("after_decision_adapter_before_record")).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    assert!(
        !timeout(WAIT, h.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        h.stdin().iter().filter(|f| f["type"] == "decision").count(),
        1
    );
    assert!(!h.sj().iter().any(|b| matches!(b, Body::DecisionApplied(_))));
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.bodies.clear();
    h.attach(0).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"))
        .await;
    assert_eq!(
        h.stdin().iter().filter(|f| f["type"] == "decision").count(),
        1
    );
}

#[tokio::test]
async fn permission_deny_is_proven_as_applied() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::decision(
        &request,
        7,
        DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message: Some("No execution".into()),
        },
    )))
    .await;
    let Body::DecisionApplied(f) = h.wait(|b| matches!(b, Body::DecisionApplied(_))).await else {
        unreachable!()
    };
    let proof: Value = serde_json::from_slice(&f.proof_payload_json).unwrap();
    assert_eq!(proof["decision"]["behavior"], "deny");
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
    assert!(
        h.bodies
            .iter()
            .any(|b| matches!(b, Body::BlockFinal(f) if f.text == "denied"))
    );
}

#[tokio::test]
async fn synthetic_codex_requires_default_input_flag() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args(["synthetic-cli", "--profile", "synthetic-codex"])
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(
        !output.status.success(),
        "Codex runner must require default_mode_request_user_input"
    );
}

#[tokio::test]
async fn sigkill_between_intention_and_adapter_never_transmits_after_recovery() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(Some("hold:after_decision_intention")).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    timeout(WAIT, async {
        while !h
            .sj()
            .iter()
            .any(|b| matches!(b, Body::DecisionTransmissionStarted(_)))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    h.child.kill().await.unwrap();
    assert!(
        !h.stdin().iter().any(|f| f["type"] == "decision"),
        "gate must hold before adapter invocation"
    );
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.bodies.clear();
    h.attach(0).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"))
        .await;
    assert!(!h.stdin().iter().any(|f| f["type"] == "decision"));
}

#[tokio::test]
async fn terminal_requests_are_not_reannounced_after_supervisor_restart() {
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::RunFinished(_))).await;
    let committed = h.last;
    h.child.kill().await.unwrap();
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.bodies.clear();
    h.attach(committed).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    h.wait(|b| matches!(b, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"))
        .await;
    assert!(!h.bodies.iter().any(|b| matches!(b, Body::RequestOpened(_))));
}

#[tokio::test]
async fn stale_generation_after_journal_closed_does_not_extend_sj() {
    use canterlot_proto::worker::ipc::{StopRequest, decision_outcome::Outcome};
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::Stop(StopRequest {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        force: true,
    }))
    .await;
    // Не подтверждаем JournalClosed, чтобы сопровождение осталось в Draining.
    timeout(WAIT, async {
        loop {
            if let up::Msg::SjBatch(batch) = h.recv().await
                && batch.records.iter().any(|r| {
                    matches!(
                        r.body.as_ref().and_then(|b| b.body.as_ref()),
                        Some(Body::JournalClosed(_))
                    )
                })
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let last = h.sj().len();
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 6)))
        .await;
    timeout(WAIT, async { loop {
        if let up::Msg::DecisionOutcome(outcome) = h.recv().await {
            assert!(matches!(outcome.outcome, Some(Outcome::NotApplied(f)) if f.reason == "stale_generation"));
            break;
        }
    }}).await.unwrap();
    assert_eq!(h.sj().len(), last);
}

#[tokio::test]
async fn closed_session_journal_stays_closed_after_supervisor_restart() {
    use canterlot_proto::worker::ipc::StopRequest;
    let _gate = e2e_gate::acquire().await;
    let mut h = Harness::new(None).await;
    let request = h.start(permission()).await;
    h.send(down::Msg::Stop(StopRequest {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        force: true,
    }))
    .await;
    timeout(WAIT, async {
        loop {
            if let up::Msg::SjBatch(batch) = h.recv().await
                && batch.records.iter().any(|r| {
                    matches!(
                        r.body.as_ref().and_then(|b| b.body.as_ref()),
                        Some(Body::JournalClosed(_))
                    )
                })
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    h.child.kill().await.unwrap();
    h.conn.take();
    h.guard.cleanup();
    fs::remove_file(h.dir.join("ipc.sock")).unwrap();
    let last = h.sj().len();
    h.child = Harness::spawn(&h.dir, h.root.path(), None);
    h.attach(0).await;
    h.send(down::Msg::DeliverDecision(Harness::allow(&request, 7)))
        .await;
    timeout(WAIT, async {
        loop {
            if matches!(h.recv().await, up::Msg::DecisionOutcome(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        h.sj().len(),
        last,
        "JournalClosed must remain the final record after restart"
    );
}
