#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Review findings for the OpenCode adapter (fake-opencode, L1). Each test asserts the
//! CORRECT behaviour, so it fails while the defect is present.

use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, BlockKind, CloseMode, InstructionMode,
        InstructionSpec, InterruptOutcome, StartSpec, Started, Transmission, TransmitOutcome,
        opencode::{
            OpenCodeAdapter,
            replay::{Reply, Script, Step},
        },
    },
    config::OpenCodeConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[path = "support/opencode.rs"]
mod opencode;
use opencode::{Fake, path_of};

const WAIT: Duration = Duration::from_secs(15);

struct Bench {
    fake: Fake,
    adapter: OpenCodeAdapter,
    group: TreeContainment,
    spec: StartSpec,
}

impl Bench {
    fn new(script: Script) -> Self {
        let fake = Fake::new(script);
        let exec_dir = fake.dir.path().join("data/executions/one");
        std::fs::create_dir_all(&exec_dir).unwrap();
        let cfg = OpenCodeConfig {
            executable: fake.exe.clone(),
            owner_config: Some(fake.dir.path().join("owner-config")),
            ..Default::default()
        };
        let adapter = OpenCodeAdapter::new(cfg, exec_dir, Duration::from_millis(700))
            .with_relay_executable(PathBuf::from("/opt/relay"));
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: opencode::workdir(&fake),
            native_conversation_id: None,
            profile: "opencode".into(),
            model: "fake/fake-1".into(),
            effort: "high".into(),
            permission_mode: "bypass".into(),
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
            fake,
            adapter,
            group: TreeContainment::new().unwrap(),
            spec,
        }
    }

    async fn start_ok(&mut self) -> Started {
        let started: Result<Started, AdapterError> =
            tokio::time::timeout(WAIT, self.adapter.start(&self.spec, &self.group))
                .await
                .expect("start must finish");
        started.expect("start")
    }

    async fn event(&mut self) -> AdapterEvent {
        tokio::time::timeout(WAIT, self.adapter.next_event())
            .await
            .expect("adapter event")
    }

    fn input(&self, text: &str) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: text.into(),
            files: vec![],
            settings: None,
        }
    }

    async fn accepted(&mut self, input: &Transmission) {
        let outcome = tokio::time::timeout(WAIT, self.adapter.transmit(input, &self.group))
            .await
            .expect("transmit must finish");
        assert!(
            matches!(outcome, TransmitOutcome::Accepted(_)),
            "{outcome:?}"
        );
    }

    async fn until_finished(&mut self, command: Uuid) -> Vec<AdapterEvent> {
        let mut events = Vec::new();
        loop {
            let event = self.event().await;
            let done = matches!(
                &event,
                AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command
            );
            events.push(event);
            if done {
                return events;
            }
        }
    }

    /// Events for `window`; `true` if a RunFinished for `command` arrived.
    async fn finished_within(&mut self, command: Uuid, window: Duration) -> bool {
        tokio::time::timeout(window, async {
            loop {
                if let AdapterEvent::RunFinished {
                    input_command_id, ..
                } = self.adapter.next_event().await
                    && input_command_id == command
                {
                    return;
                }
            }
        })
        .await
        .is_ok()
    }

    async fn close(&mut self) {
        tokio::time::timeout(WAIT, self.adapter.close(CloseMode::Graceful, &self.group))
            .await
            .expect("close must finish")
            .expect("close");
    }

    fn count(&self, method: &str, tail: &str) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|request| request["method"] == method && path_of(request).ends_with(tail))
            .count()
    }
}

fn session_of(script: &Script) -> String {
    script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()?
                .pointer("/data/id")?
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .unwrap()
}

fn emitted(script: &Script, kinds: &[&str]) -> Vec<Value> {
    script
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Emit { events } => Some(events.clone()),
            _ => None,
        })
        .flatten()
        .filter(|event| kinds.contains(&event["type"].as_str().unwrap_or_default()))
        .collect()
}

fn replace_events_after_prompt(script: &mut Script, events: Vec<Step>) {
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.extend(events);
}

fn ready() -> Script {
    Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
}

// ---------------------------------------------------------------------------------------
// Finding: a permission asked by a (background) child session is left open on the server
// when the root period ends. `on_run_ended` drops only root-session requests, the
// supervisor closes every card on RunFinished without calling `refuse_request`, so nobody
// ever answers the child: the subagent blocks until the execution is closed.
// ---------------------------------------------------------------------------------------
#[tokio::test]
async fn child_request_open_at_the_end_of_the_root_period_is_refused() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let child = "ses_child00000000000000000000000".to_owned();
    let mut asked = emitted(&capture, &["permission.asked"]).remove(0);
    asked["data"]["sessionID"] = child.clone().into();
    let request = asked["data"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit {
                events: vec![
                    json!({"type": "session.created",
                        "data": {"sessionID": child, "parentID": session}}),
                    asked,
                ],
            },
            Step::Stall { ms: 200 },
            // Background child: the parent period ends while the child still waits.
            Step::Emit {
                events: vec![json!({"type": "session.execution.succeeded",
                    "data": {"sessionID": session}})],
            },
        ],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{child}/permission/{request}/reply"),
        vec![Reply {
            status: 204,
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestOpened { .. })),
        "{events:?}"
    );
    // The supervisor closes the card on RunFinished and never answers it.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let tail = format!("{child}/permission/{request}/reply");
    assert_eq!(
        bench.count("POST", &tail),
        1,
        "the child permission is still pending on the server and nobody will answer it"
    );
    bench.close().await;
}

// ---------------------------------------------------------------------------------------
// Finding: an interrupt during the DB-lock pause is answered NotApplicable (the session is
// idle) and the scheduled `continue` still fires, so the owner's stop is ignored and the
// turn resumes.
// ---------------------------------------------------------------------------------------
fn locked_prompt_expect(session: &str, id: &str) -> Step {
    Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/prompt"),
        reply: Reply {
            status: 200,
            body: Some(json!({"data": {"id": id}})),
            ..Default::default()
        },
        bind: [(id.to_owned(), "/id".to_owned())].into_iter().collect(),
    }
}

fn script_with_one_locked_run(script: &mut Script) {
    let started = emitted(script, &["session.execution.started"]).remove(0);
    let succeeded = emitted(script, &["session.execution.succeeded"]).remove(0);
    let mut failed = succeeded.clone();
    failed["type"] = json!("session.execution.failed");
    failed["data"]["error"] = json!({"type": "unknown", "message": "Failed to execute statement"});
    let position = script
        .steps
        .iter()
        .position(|step| {
            matches!(step, Step::Emit { events }
                if events.iter().any(|event| event["type"] == "session.execution.succeeded"))
        })
        .unwrap();
    let Step::Emit { events } = &mut script.steps[position] else {
        unreachable!();
    };
    for event in events {
        if event["type"] == "session.execution.succeeded" {
            *event = failed.clone();
        }
    }
    let session = started["data"]["sessionID"].as_str().unwrap().to_owned();
    script
        .steps
        .insert(position + 1, locked_prompt_expect(&session, "msg_done"));
    script.steps.insert(
        position + 2,
        Step::Emit {
            events: vec![started, succeeded],
        },
    );
}

#[tokio::test]
async fn interrupt_during_the_db_lock_pause_stops_the_turn() {
    let mut script = ready();
    let session = session_of(&script);
    script_with_one_locked_run(&mut script);
    script.set_route(
        "POST",
        &format!("/api/session/{session}/interrupt"),
        vec![Reply {
            body: Some(json!({"interrupted": false})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench
        .adapter
        .set_db_lock_delays(vec![Duration::from_millis(800), Duration::from_millis(800)]);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    // The lock notice means the `continue` is scheduled and the server is idle.
    loop {
        if let AdapterEvent::BlockStarted {
            kind: BlockKind::Warning,
            ..
        } = bench.event().await
        {
            break;
        }
    }
    let outcome = tokio::time::timeout(WAIT, bench.adapter.interrupt())
        .await
        .expect("interrupt must finish");
    // The owner pressed stop; let the pause run out.
    let finished = bench
        .finished_within(input.command_id, Duration::from_secs(3))
        .await;
    assert!(finished, "the turn must end");
    assert_eq!(
        bench.count("POST", "/prompt"),
        1,
        "interrupt ({outcome:?}) did not cancel the scheduled `continue`: the turn resumed"
    );
    assert!(
        !matches!(outcome, InterruptOutcome::NotApplicable),
        "a turn is running from the supervisor's view; NotApplicable keeps it alive"
    );
    bench.close().await;
}

// ---------------------------------------------------------------------------------------
// Finding: a lost frame (undecodable / oversized) triggers exactly one projection reread.
// If that single reread fails (transient HTTP error), nothing retries it: a lost terminal
// leaves the turn open forever although the stream and the server are healthy.
// ---------------------------------------------------------------------------------------
fn recorded(scenario: &str) -> Script {
    Script::from_capture("recovery", Some(scenario))
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

#[tokio::test]
async fn failed_reread_after_a_lost_terminal_is_retried() {
    let mut script = recorded("cut_terminal_lost");
    let drop_at = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::DropSse))
        .unwrap();
    // The terminal frame arrives broken instead of a cut stream.
    script.steps[drop_at] = Step::Frame {
        raw: "data: {\"type\":\"session.execution.succ\n\n".to_owned(),
    };
    script.steps.remove(drop_at + 1);
    // First projection read fails once, then the server answers normally.
    let active = script
        .route("GET", "/api/session/active")
        .map(|route| route.replies.last().cloned().unwrap())
        .unwrap_or(Reply {
            body: Some(json!({"data": {}})),
            ..Default::default()
        });
    script.set_route(
        "GET",
        "/api/session/active",
        vec![
            Reply {
                status: 500,
                body: Some(json!({"_tag": "UnknownError"})),
                ..Default::default()
            },
            active,
        ],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let finished = bench
        .finished_within(input.command_id, Duration::from_secs(5))
        .await;
    let reads = bench.count("GET", "/api/session/active");
    assert!(
        finished,
        "the turn stays open forever after one failed reread ({reads} reads of /api/session/active)"
    );
    bench.close().await;
}

#[tokio::test]
async fn rereads_exhausted_fall_back_to_a_full_resync() {
    let mut script = recorded("cut_terminal_lost");
    let drop_at = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::DropSse))
        .unwrap();
    // The terminal frame arrives broken instead of a cut stream.
    script.steps[drop_at] = Step::Frame {
        raw: "data: {\"type\":\"session.execution.succ\n\n".to_owned(),
    };
    script.steps.remove(drop_at + 1);
    // Every reread attempt fails; only the read after resubscribing succeeds.
    let active = script
        .route("GET", "/api/session/active")
        .map(|route| route.replies.last().cloned().unwrap())
        .unwrap_or(Reply {
            body: Some(json!({"data": {}})),
            ..Default::default()
        });
    script.set_route(
        "GET",
        "/api/session/active",
        (0..4)
            .map(|_| Reply {
                status: 500,
                body: Some(json!({"_tag": "UnknownError"})),
                ..Default::default()
            })
            .chain([active])
            .collect(),
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let finished = bench
        .finished_within(input.command_id, Duration::from_secs(12))
        .await;
    let reads = bench.count("GET", "/api/session/active");
    assert!(
        finished,
        "rereads ran out and the resubscribe fallback did not finish the turn ({reads} reads)"
    );
    assert!(
        reads >= 5,
        "four failed rereads and a read after resubscribing ({reads})"
    );
    assert!(
        bench.count("GET", "/api/event") >= 2,
        "the fallback must subscribe to the event stream again"
    );
    bench.close().await;
}

/// Control for the test above: with a healthy first reread the same broken terminal is
/// recovered from the projection, so the 500 alone is what keeps the turn open.
#[tokio::test]
async fn control_lost_terminal_with_a_healthy_reread_finishes() {
    let mut script = recorded("cut_terminal_lost");
    let drop_at = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::DropSse))
        .unwrap();
    script.steps[drop_at] = Step::Frame {
        raw: "data: {\"type\":\"session.execution.succ\n\n".to_owned(),
    };
    script.steps.remove(drop_at + 1);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    assert!(
        bench
            .finished_within(input.command_id, Duration::from_secs(5))
            .await
    );
    bench.close().await;
}

// ---------------------------------------------------------------------------------------
// An interrupt the server answers `interrupted: false` leaves the supervisor's turn and
// cards as they were; the adapter must keep the request answerable too, otherwise the
// owner's later decision hits `request_not_pending`.
// ---------------------------------------------------------------------------------------
#[tokio::test]
async fn not_applicable_interrupt_keeps_the_open_request_answerable() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let asked = emitted(&capture, &["permission.asked"]).remove(0);
    let request = asked["data"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit {
                events: vec![asked],
            },
            Step::Stall { ms: 3000 },
        ],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{session}/interrupt"),
        vec![Reply {
            body: Some(json!({"interrupted": false})),
            ..Default::default()
        }],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{session}/permission/{request}/reply"),
        vec![Reply {
            status: 204,
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    loop {
        if let AdapterEvent::RequestOpened { .. } = bench.event().await {
            break;
        }
    }
    let outcome = tokio::time::timeout(WAIT, bench.adapter.interrupt())
        .await
        .expect("interrupt must finish");
    assert!(
        matches!(outcome, InterruptOutcome::NotApplicable),
        "{outcome:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tail = format!("/permission/{request}/reply");
    assert_eq!(
        bench.count("POST", &tail),
        0,
        "a not-applicable interrupt must not refuse the request behind the owner's card"
    );
    let allow = canterlot_proto::requests::DecisionPayload::Permission {
        behavior: canterlot_proto::requests::PermissionBehavior::Allow,
        message: None,
    };
    let decided = tokio::time::timeout(WAIT, bench.adapter.deliver_decision(&request, &allow))
        .await
        .expect("decision must finish");
    assert!(
        matches!(
            decided,
            canterlot_worker::adapter::DecisionOutcome::Applied(_)
        ),
        "{decided:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bench.count("POST", &tail), 1);
    bench.close().await;
}
