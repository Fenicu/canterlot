#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Адаптер OpenCode на двойнике `fake-opencode` (L1): настоящий процесс, HTTP и SSE.
//! Сценарии собраны из записей этапа 0.

use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, BlockKind, CloseMode, InstructionMode,
        InstructionSpec, Phase, SettingsChange, StartSpec, Started, Transmission, TransmitOutcome,
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
use opencode::{Fake, find, path_of};

const WAIT: Duration = Duration::from_secs(15);

fn instructions(mode: InstructionMode, text: &str) -> InstructionSpec {
    InstructionSpec {
        mode,
        text: text.into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

struct Bench {
    fake: Fake,
    adapter: OpenCodeAdapter,
    group: TreeContainment,
    spec: StartSpec,
    exec_dir: PathBuf,
}

impl Bench {
    fn new(script: Script) -> Self {
        Self::with_config(script, |_| {})
    }

    fn with_config(script: Script, change: impl FnOnce(&mut OpenCodeConfig)) -> Self {
        let fake = Fake::new(script);
        let exec_dir = fake.dir.path().join("data/executions/one");
        std::fs::create_dir_all(&exec_dir).unwrap();
        let mut cfg = OpenCodeConfig {
            executable: fake.exe.clone(),
            ..Default::default()
        };
        change(&mut cfg);
        let adapter = OpenCodeAdapter::new(cfg, exec_dir.clone(), Duration::from_millis(700))
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
            instructions: instructions(InstructionMode::Owner, ""),
        };
        Self {
            fake,
            adapter,
            group: TreeContainment::new().unwrap(),
            spec,
            exec_dir,
        }
    }

    async fn start(&mut self) -> Result<Started, AdapterError> {
        tokio::time::timeout(WAIT, self.adapter.start(&self.spec, &self.group))
            .await
            .expect("start must finish")
    }

    async fn start_ok(&mut self) -> Started {
        self.start().await.expect("start")
    }

    async fn fail_reason(&mut self) -> &'static str {
        match self.start().await {
            Err(AdapterError::StartFailed(reason)) => reason,
            other => panic!("start must fail: {other:?}"),
        }
    }

    async fn event(&mut self) -> AdapterEvent {
        tokio::time::timeout(WAIT, self.adapter.next_event())
            .await
            .expect("adapter event")
    }

    async fn close(&mut self, mode: CloseMode) {
        tokio::time::timeout(WAIT, self.adapter.close(mode, &self.group))
            .await
            .expect("close must finish")
            .expect("close");
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

    async fn transmit(&mut self, input: &Transmission) -> TransmitOutcome {
        tokio::time::timeout(WAIT, self.adapter.transmit(input, &self.group))
            .await
            .expect("transmit must finish")
    }

    async fn accepted(&mut self, input: &Transmission) {
        match self.transmit(input).await {
            TransmitOutcome::Accepted(proof) => {
                assert_eq!(proof.command_id, input.command_id);
                assert_eq!(proof.kind, "opencode_inbox_admitted");
            }
            other => panic!("{other:?}"),
        }
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

    fn env(&self, key: &str) -> Option<String> {
        self.fake.boot()["env"][key].as_str().map(str::to_owned)
    }
}

fn ready() -> Script {
    Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
}

/// Запись `resume`: два хода подряд в одном процессе.
fn two_turns() -> Script {
    let mut script = Script::from_capture("resume", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let third = script
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .nth(2)
        .map(|(index, _)| index)
        .unwrap();
    script.steps.truncate(third);
    let agents = Script::from_capture("start_stream", None)
        .route("GET", "/api/agent")
        .unwrap()
        .replies
        .clone();
    script.set_route("GET", "/api/agent", agents);
    script
}

fn text_of(events: &[AdapterEvent], kind: &str) -> Vec<String> {
    let ids: Vec<Uuid> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                kind: found,
                ..
            } if format!("{found:?}") == kind => Some(*block_id),
            _ => None,
        })
        .collect();
    ids.iter()
        .filter_map(|id| {
            events.iter().find_map(|event| match event {
                AdapterEvent::BlockFinal { block_id, text, .. } if block_id == id => {
                    Some(text.clone())
                }
                _ => None,
            })
        })
        .collect()
}

fn set_prompt_reply(script: &mut Script, change: impl FnOnce(&mut Reply)) {
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    let Step::Expect { reply, .. } = &mut script.steps[position] else {
        unreachable!();
    };
    change(reply);
}

/// Без `/prompt`: запись дальше шага создания сессии не нужна.
fn session_only() -> Script {
    let mut script = ready();
    script.steps.truncate(
        script
            .steps
            .iter()
            .position(|step| step.expected_path().is_some())
            .unwrap()
            + 1,
    );
    script
}

#[tokio::test]
async fn start_path_runs_in_the_recorded_order() {
    let mut bench = Bench::new(session_only());
    let started = bench.start_ok().await;
    assert!(started.native_conversation_id.starts_with("ses_"));
    assert_eq!(
        started.native_conversation_id.len(),
        "ses_".len() + 32,
        "{}",
        started.native_conversation_id
    );
    assert_eq!(bench.fake.pids(), vec![started.cli_pid]);
    assert!(started.cli_start_ticks > 0);
    let requests = bench.fake.requests();
    assert_eq!(
        path_of(&requests[0]),
        "/api/event",
        "SSE first: {requests:?}"
    );
    assert!(requests.iter().all(|request| request["authorized"] == true));
    let position = |tail: &str| {
        requests
            .iter()
            .position(|request| path_of(request) == tail)
            .unwrap_or_else(|| panic!("{tail}: {requests:?}"))
    };
    assert!(position("/api/info") < position("/api/session"));
    assert!(position("/api/session") < position("/api/mcp"));
    assert!(position("/api/mcp") < position("/api/agent"));
    let create = find(&requests, "POST", "/api/session").unwrap();
    assert_eq!(create["body"]["id"], started.native_conversation_id);
    assert!(
        create["body"]["title"]
            .as_str()
            .is_some_and(|t| !t.is_empty())
    );
    assert_eq!(
        create["body"]["location"]["directory"],
        bench.spec.workdir.to_str().unwrap()
    );
    assert_eq!(
        create["body"]["model"],
        json!({"id": "fake-1", "providerID": "fake", "variant": "high"})
    );
    assert_eq!(
        create["body"]["permissions"],
        json!([{"action": "*", "resource": "*", "effect": "allow"}])
    );
    assert_eq!(
        Value::from(bench.fake.boot()["cwd"].as_str().unwrap()),
        Value::from(bench.spec.workdir.to_str().unwrap())
    );
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_gets_its_own_data_and_the_canterlot_layer() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let data = bench.fake.dir.path().join("data/opencode");
    assert_eq!(bench.fake.boot()["password_set"], true);
    assert_eq!(
        bench.env("XDG_DATA_HOME").unwrap(),
        data.join("data").to_str().unwrap()
    );
    assert_eq!(
        bench.env("XDG_STATE_HOME").unwrap(),
        data.join("state").to_str().unwrap()
    );
    assert_eq!(
        bench.env("XDG_CACHE_HOME").unwrap(),
        data.join("cache").to_str().unwrap()
    );
    assert_eq!(
        bench.env("TMPDIR").unwrap(),
        bench.exec_dir.join("tmp").to_str().unwrap()
    );
    assert_eq!(
        bench.env("OPENCODE_DISABLE_AUTOUPDATE").as_deref(),
        Some("1")
    );
    assert_eq!(bench.env("OPENCODE_CONFIG_DIR"), None);
    let content: Value =
        serde_json::from_str(&bench.env("OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
    assert_eq!(content["update"], "disable");
    assert_eq!(content["share"], "disabled");
    assert_eq!(content["snapshots"], false);
    assert_eq!(content["experimental"]["todo"], true);
    let server = &content["mcp"]["servers"]["canterlot"];
    assert_eq!(server["type"], "local");
    assert_eq!(server["codemode"], false);
    assert_eq!(
        server["command"],
        json!([
            "/opt/relay",
            "mcp-relay",
            "--socket",
            bench.exec_dir.join("mcp.sock").to_str().unwrap()
        ])
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn preset_text_goes_to_the_execution_config_dir() {
    let mut bench = Bench::new(session_only());
    bench.spec.instructions = instructions(InstructionMode::Preset, "Отвечай кратко");
    bench.start_ok().await;
    let dir = bench.exec_dir.join("opencode-config");
    assert_eq!(
        bench.env("OPENCODE_CONFIG_DIR").unwrap(),
        dir.to_str().unwrap()
    );
    assert_eq!(
        bench.env("OPENCODE_DISABLE_CLAUDE_CODE").as_deref(),
        Some("1")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
        "Отвечай кратко"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn clean_mode_replaces_the_config_dir_with_an_empty_one() {
    let mut bench = Bench::new(session_only());
    bench.spec.instructions = instructions(InstructionMode::Clean, "");
    bench.start_ok().await;
    let dir = bench.exec_dir.join("opencode-config");
    assert_eq!(
        bench.env("OPENCODE_CONFIG_DIR").unwrap(),
        dir.to_str().unwrap()
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn modes_become_session_rules() {
    for (mode, rules) in [
        ("owner", Value::Null),
        (
            "read_only",
            json!([
                {"action": "*", "resource": "*", "effect": "deny"},
                {"action": "read", "resource": "*", "effect": "allow"},
                {"action": "glob", "resource": "*", "effect": "allow"},
                {"action": "grep", "resource": "*", "effect": "allow"},
                {"action": "question", "resource": "*", "effect": "allow"},
                {"action": "subagent", "resource": "*", "effect": "allow"},
            ]),
        ),
        (
            "ask",
            json!([
                {"action": "*", "resource": "*", "effect": "ask"},
                {"action": "question", "resource": "*", "effect": "allow"},
                {"action": "canterlot_canterlot_limits", "resource": "*", "effect": "allow"},
                {"action": "todowrite", "resource": "*", "effect": "allow"},
            ]),
        ),
    ] {
        let mut bench = Bench::new(session_only());
        bench.spec.permission_mode = mode.into();
        bench.start_ok().await;
        let requests = bench.fake.requests();
        let create = find(&requests, "POST", "/api/session").unwrap();
        assert_eq!(create["body"]["permissions"], rules, "{mode}");
        bench.close(CloseMode::Graceful).await;
    }
}

#[tokio::test]
async fn unknown_mode_stops_before_the_cli_starts() {
    let mut bench = Bench::new(session_only());
    bench.spec.permission_mode = "yolo".into();
    assert_eq!(bench.fail_reason().await, "unknown_permission_mode");
    assert!(bench.fake.requests().is_empty());
}

#[tokio::test]
async fn database_of_the_owner_is_refused() {
    let home = std::env::var("HOME").unwrap();
    let root = format!("{home}/.local/share/opencode/canterlot-never-created");
    let mut bench = Bench::with_config(session_only(), |cfg| {
        cfg.data_root = Some(root.clone().into());
    });
    assert_eq!(bench.fail_reason().await, "data_root_refused");
    assert!(bench.fake.requests().is_empty());
    assert!(!std::path::Path::new(&root).exists());
}

#[tokio::test]
async fn old_cli_is_incompatible() {
    let mut script = session_only();
    script.version = "opencode v2.1.3".into();
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "cli_too_old");
}

#[tokio::test]
async fn missing_cli_is_named() {
    let mut bench = Bench::with_config(session_only(), |cfg| {
        cfg.executable = "/nonexistent/opencode".into();
    });
    assert_eq!(bench.fail_reason().await, "cli_not_found");
}

#[tokio::test]
async fn non_loopback_ready_line_is_refused() {
    for line in [
        r#"{"url":"http://10.0.0.1:4096"}"#,
        r#"{"url":"https://127.0.0.1:4096"}"#,
        r#"{"url":"http://evil.example:4096"}"#,
        r#"{"url":"http://user:pw@127.0.0.1:4096"}"#,
    ] {
        let mut script = session_only();
        script.ready.line = Some(line.into());
        let mut bench = Bench::new(script);
        assert_eq!(bench.fail_reason().await, "server_not_loopback", "{line}");
        bench.fake.wait_gone().await;
    }
}

#[tokio::test]
async fn garbled_ready_line_fails_the_start() {
    let mut script = session_only();
    script.ready.line = Some("Update available".into());
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_not_ready");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn silent_server_hits_the_ready_deadline() {
    let mut script = session_only();
    script.ready.silent = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    assert_eq!(bench.fail_reason().await, "server_not_ready");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_dying_before_the_ready_line_fails_the_start() {
    let mut script = session_only();
    script.ready.exit = Some(7);
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_not_ready");
}

// Строка готовности старого процесса: порт уже никто не слушает.
#[tokio::test]
async fn stale_ready_line_fails_to_connect() {
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    let mut script = session_only();
    script.ready.line = Some(format!(r#"{{"url":"http://127.0.0.1:{port}"}}"#));
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_unreachable");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn other_server_on_the_port_is_refused_by_pid() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.1.4", "pid": 1})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_identity_mismatch");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn version_of_the_api_must_match_the_cli() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.1.9", "pid": "$PID"})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_identity_mismatch");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn untested_version_runs_with_diagnostics() {
    let mut script = session_only();
    script.version = "opencode v2.2.0".into();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.2.0", "pid": "$PID"})),
            ..Default::default()
        }],
    );
    script.set_route(
        "GET",
        "/openapi.json",
        vec![Reply {
            body: Some(json!({"openapi": "changed"})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut kinds = Vec::new();
    for _ in 0..2 {
        match bench.event().await {
            AdapterEvent::Diagnostic { kind, required, .. } => {
                assert!(!required);
                kinds.push(kind);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(kinds, ["untested_cli_version", "openapi_changed"]);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn failed_mcp_is_a_diagnostic_not_a_failed_start() {
    let script = Script::from_capture("start_stream", None).with_canterlot_mcp("failed");
    let mut script = script;
    script.steps.truncate(
        script
            .steps
            .iter()
            .position(|step| step.expected_path().is_some())
            .unwrap()
            + 1,
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    match bench.event().await {
        AdapterEvent::Diagnostic { kind, required, .. } => {
            assert_eq!(kind, "mcp_unavailable");
            assert!(!required);
        }
        other => panic!("{other:?}"),
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn pending_mcp_past_the_deadline_is_a_diagnostic() {
    let mut bench = Bench::new({
        let mut script = session_only();
        let pending = script.route("GET", "/api/mcp").unwrap().replies[0].clone();
        script.set_route("GET", "/api/mcp", vec![pending]);
        script
    });
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    bench.start_ok().await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Diagnostic {
            kind: "mcp_unavailable",
            required: false,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn agents_are_polled_until_the_location_is_up() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let polls = bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request) == "/api/agent")
        .count();
    assert!(polls >= 2, "the capture starts with an empty list: {polls}");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn empty_agent_list_past_the_deadline_fails_the_start() {
    let mut script = session_only();
    let empty = script.route("GET", "/api/agent").unwrap().replies[0].clone();
    script.set_route("GET", "/api/agent", vec![empty]);
    let mut bench = Bench::new(script);
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    assert_eq!(bench.fail_reason().await, "agents_unavailable");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn refused_session_fails_the_start() {
    let mut script = session_only();
    let last = script.steps.len() - 1;
    if let Step::Expect { reply, .. } = &mut script.steps[last] {
        *reply = Reply {
            status: 400,
            body: Some(json!({"_tag": "InvalidRequestError", "message": "секрет"})),
            ..Default::default()
        };
    }
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "session_create_failed");
    bench.fake.wait_gone().await;
}

fn resume_script(found: bool) -> (Script, String) {
    let id = "ses_11112222333344445555666677778888".to_owned();
    let mut script = ready();
    script.steps.clear();
    let session = if found {
        Reply {
            body: Some(json!({"data": {"id": id, "title": "canterlot"}})),
            ..Default::default()
        }
    } else {
        Reply {
            status: 404,
            body: Some(json!({"_tag": "SessionNotFoundError", "sessionID": id})),
            ..Default::default()
        }
    };
    script.set_route("GET", &format!("/api/session/{id}"), vec![session]);
    for (method, path) in [
        ("PATCH", format!("/api/session/{id}")),
        ("POST", format!("/api/session/{id}/model")),
    ] {
        script.set_route(
            method,
            &path,
            vec![Reply {
                status: 204,
                ..Default::default()
            }],
        );
    }
    (script, id)
}

#[tokio::test]
async fn resume_reapplies_rules_and_model_to_the_same_session() {
    let (script, id) = resume_script(true);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id.clone());
    bench.spec.permission_mode = "read_only".into();
    let started = bench.start_ok().await;
    assert_eq!(started.native_conversation_id, id);
    let requests = bench.fake.requests();
    assert!(find(&requests, "POST", "/api/session").is_none());
    let patch = find(&requests, "PATCH", &id).unwrap();
    assert_eq!(patch["body"]["permissions"][0]["effect"], "deny");
    let model = find(&requests, "POST", "/model").unwrap();
    assert_eq!(
        model["body"]["model"],
        json!({"id": "fake-1", "providerID": "fake", "variant": "high"})
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn resume_of_a_lost_session_fails_the_start() {
    let (script, id) = resume_script(false);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    assert_eq!(bench.fail_reason().await, "resume_failed");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn graceful_close_uses_the_stdin_lease() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    assert!(!bench.fake.pids().is_empty());
    let started = std::time::Instant::now();
    bench.close(CloseMode::Graceful).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn graceful_close_escalates_to_term_then_kill() {
    let mut script = session_only();
    script.ignore_eof = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_millis(100));
    bench.start_ok().await;
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;

    let mut script = session_only();
    script.ignore_eof = true;
    script.ignore_term = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_millis(100));
    bench.start_ok().await;
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn force_close_kills_at_once() {
    let mut script = session_only();
    script.ignore_eof = true;
    script.ignore_term = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_secs(30));
    bench.start_ok().await;
    let started = std::time::Instant::now();
    bench.close(CloseMode::Force).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn dropped_adapter_leaves_no_server_behind() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let Bench { fake, adapter, .. } = bench;
    drop(adapter);
    fake.wait_gone().await;
}

#[tokio::test]
async fn cancelled_start_leaves_no_server_behind() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/mcp",
        vec![Reply {
            stall_ms: 30_000,
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    let cut = tokio::time::timeout(
        Duration::from_millis(500),
        bench.adapter.start(&bench.spec, &bench.group),
    )
    .await;
    assert!(cut.is_err());
    let Bench { fake, adapter, .. } = bench;
    drop(adapter);
    fake.wait_gone().await;
}

#[tokio::test]
async fn server_exit_reaches_the_supervisor() {
    let mut script = session_only();
    script.steps.push(Step::Stall { ms: 1500 });
    script.steps.push(Step::Die {
        code: 9,
        kill: false,
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    loop {
        match bench.event().await {
            AdapterEvent::ProcessExited { code, signal } => {
                assert_eq!((code, signal), (Some(9), None));
                break;
            }
            AdapterEvent::Diagnostic { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn second_start_is_refused() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    assert!(matches!(
        bench.start().await,
        Err(AdapterError::AlreadyStarted)
    ));
    bench.close(CloseMode::Graceful).await;
}

#[test]
fn registry_builds_the_adapter_from_context_with_supervisor_paths() {
    use canterlot_worker::adapter::{AdapterContext, registry_with};
    let context = AdapterContext {
        transmit_deadline: Duration::from_secs(1),
        exec_dir: PathBuf::from("/tmp/x"),
        claude_code: None,
        codex: None,
        opencode: Some(OpenCodeConfig::default()),
    };
    let adapter = registry_with("opencode", &context).unwrap();
    assert!(adapter.capabilities().interrupt_keeps_process);
}

#[tokio::test]
async fn first_turn_runs_from_prompt_to_run_finished() {
    let mut bench = Bench::new(ready());
    let started = bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(text_of(&events, "Reasoning"), ["think more"]);
    assert_eq!(text_of(&events, "Text"), ["Hello world"]);
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        context_window_tokens,
        ..
    } = events.last().unwrap()
    else {
        unreachable!();
    };
    assert_eq!(usage[0].input_tokens, 70);
    assert_eq!(usage[0].output_tokens, 20);
    assert_eq!(*context_tokens, 100);
    assert_eq!(*context_window_tokens, 200_000);
    let requests = bench.fake.requests();
    let prompt = find(&requests, "POST", "/prompt").unwrap();
    assert_eq!(
        path_of(prompt),
        format!("/api/session/{}/prompt", started.native_conversation_id)
    );
    assert_eq!(
        prompt["body"],
        json!({
            "id": format!("msg_{}", input.command_id.simple()),
            "text": "say hello",
            "delivery": "queue",
            "metadata": {"canterlot_command": input.command_id.to_string()},
        })
    );
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn two_turns_in_one_process_each_get_their_end() {
    let mut bench = Bench::new(two_turns());
    bench.start_ok().await;
    let first = bench.input("remember");
    bench.accepted(&first).await;
    let events = bench.until_finished(first.command_id).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::RunFinished { .. }))
            .count(),
        1
    );
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    let second = bench.input("what did I say");
    bench.accepted(&second).await;
    bench.until_finished(second.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn second_input_during_a_turn_is_rejected_not_steered() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(position, Step::Stall { ms: 300 });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("one");
    bench.accepted(&first).await;
    let second = bench.input("two");
    assert!(matches!(
        bench.transmit(&second).await,
        TransmitOutcome::Rejected(reason) if reason == "turn_active"
    ));
    bench.until_finished(first.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn refused_prompt_names_the_error_and_leaves_the_session_usable() {
    for (status, tag) in [
        (400, "InvalidRequestError"),
        (409, "ConflictError"),
        (404, "SessionNotFoundError"),
    ] {
        let mut script = ready();
        set_prompt_reply(&mut script, |reply| {
            *reply = Reply {
                status,
                body: Some(json!({"_tag": tag, "message": "текст для модели"})),
                ..Default::default()
            };
        });
        let position = script
            .steps
            .iter()
            .position(|step| {
                step.expected_path()
                    .is_some_and(|path| path.ends_with("/prompt"))
            })
            .unwrap();
        script.steps.truncate(position + 1);
        let mut bench = Bench::new(script);
        bench.start_ok().await;
        let input = bench.input("x");
        match bench.transmit(&input).await {
            TransmitOutcome::Rejected(reason) => assert_eq!(reason, tag),
            other => panic!("{other:?}"),
        }
        let next = bench.input("y");
        assert!(!matches!(
            bench.transmit(&next).await,
            TransmitOutcome::Rejected(reason) if reason == "turn_active"
        ));
        bench.close(CloseMode::Graceful).await;
    }
}

#[tokio::test]
async fn foreign_id_in_the_reply_is_not_a_proof() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| {
        reply.body = Some(json!({"data": {"id": "msg_someone_else"}}));
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    bench.close(CloseMode::Force).await;
}

// 500 ничего не доказывает, а автоматического повтора нет (D-6): запись не найдена,
// исполнение освобождается, ввод остаётся неподтверждённым.
#[tokio::test]
async fn server_error_on_prompt_stays_unconfirmed_and_frees_the_execution() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| {
        *reply = Reply {
            status: 500,
            body: Some(json!({"_tag": "UnknownError"})),
            ..Default::default()
        };
    });
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let id = started.native_conversation_id;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    let events = bench.until_finished(input.command_id).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::InputAccepted { .. }))
    );
    let requests = bench.fake.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| path_of(request).ends_with("/prompt"))
            .count(),
        1,
        "no automatic resend"
    );
    assert!(find(&requests, "GET", &format!("{id}/inbox")).is_some());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn timed_out_prompt_is_proven_late_by_the_event_stream() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| reply.stall_ms = 30_000);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    let events = bench.until_finished(input.command_id).await;
    let accepted = events
        .iter()
        .filter(|event| {
            matches!(event, AdapterEvent::InputAccepted { command_id } if *command_id == input.command_id)
        })
        .count();
    assert_eq!(accepted, 1, "{events:?}");
    bench.close(CloseMode::Force).await;
}

// Потока нет, доказательство только в проекции.
#[tokio::test]
async fn timed_out_prompt_is_proven_late_by_the_inbox_projection() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| reply.stall_ms = 30_000);
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let input = bench.input("x");
    bench_route_inbox(&bench, &started.native_conversation_id, &input);
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    assert!(matches!(
        bench.event().await,
        AdapterEvent::InputAccepted { command_id } if command_id == input.command_id
    ));
    bench.close(CloseMode::Force).await;
}

fn bench_route_inbox(bench: &Bench, session: &str, input: &Transmission) {
    // Маршрут пишется в журнал двойника файлом: сценарий уже загружен.
    let id = format!("msg_{}", input.command_id.simple());
    let body = json!({"data": [{"id": id, "sessionID": session, "type": "user"}]});
    bench
        .fake
        .write_override("GET", &format!("/api/session/{session}/inbox"), &body);
}

#[tokio::test]
async fn repeated_attempt_checks_history_and_never_resends() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let mut input = bench.input("x");
    input.attempt = 2;
    let session = &started.native_conversation_id;
    let id = format!("msg_{}", input.command_id.simple());
    bench.fake.write_override(
        "GET",
        &format!("/api/session/{session}/message/{id}"),
        &json!({"data": {"id": id, "type": "user"}}),
    );
    match bench.transmit(&input).await {
        TransmitOutcome::Accepted(proof) => assert_eq!(proof.kind, "opencode_history"),
        other => panic!("{other:?}"),
    }
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    assert!(find(&requests, "POST", "/prompt").is_none());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn repeated_attempt_without_history_is_sent_once() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("say hello");
    input.attempt = 2;
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn cyrillic_text_survives_the_stream_and_the_input() {
    let mut script = ready();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if let Some(delta) = event["data"].get_mut("delta")
                && let Some(value) = delta.as_str()
            {
                *delta = value.replace("Hello ", "Привет, ").into();
            }
            if let Some(text) = event["data"].get_mut("text")
                && let Some(value) = text.as_str()
            {
                *text = value.replace("Hello ", "Привет, ").into();
            }
        }
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("Скажи привет");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(text_of(&events, "Text"), ["Привет, world"]);
    let requests = bench.fake.requests();
    assert_eq!(
        find(&requests, "POST", "/prompt").unwrap()["body"]["text"],
        "Скажи привет"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn attachments_go_as_paths_in_the_text_until_their_slice() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("смотри");
    input.files = vec![canterlot_worker::adapter::LocalFile {
        path: "/tmp/отчёт.pdf".into(),
        name: "отчёт.pdf".into(),
        mime: "application/pdf".into(),
        size: 5,
        image: false,
    }];
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    let body = &find(&requests, "POST", "/prompt").unwrap()["body"];
    assert!(body["text"].as_str().unwrap().contains("/tmp/отчёт.pdf"));
    assert!(body.get("files").is_none());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn settings_change_is_refused_until_its_slice() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(SettingsChange {
        model: Some("fake/other".into()),
        ..Default::default()
    });
    assert_eq!(
        bench.adapter.validate_transmission(&input),
        Err("settings_unavailable")
    );
    let mut same = bench.input("x");
    same.settings = Some(SettingsChange {
        model: Some("fake/fake-1".into()),
        effort: Some("high".into()),
        permission_mode: Some("bypass".into()),
        instructions: None,
    });
    assert_eq!(bench.adapter.validate_transmission(&same), Ok(()));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn failed_run_shows_an_error_block_and_still_finishes() {
    let mut script = ready();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if event["type"] == "session.execution.succeeded" {
                event["type"] = "session.execution.failed".into();
                event["data"]["error"] =
                    json!({"type": "provider.internal", "message": "упал провайдер"});
            }
        }
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let errors = text_of(&events, "Error");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("упал провайдер"));
    assert!(errors[0].contains("provider.internal"));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { kind: BlockKind::Error, input_command_id, .. }
            if *input_command_id == Some(input.command_id)
    )));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn unknown_event_does_not_stop_the_turn() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(
        position,
        Step::Emit {
            events: vec![
                json!({"type": "session.brand.new", "data": {"sessionID": "ses_x"}}),
                json!({"type": "brand.new", "data": {}}),
            ],
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic { kind: "unknown_event", required: false, event_type }
            if event_type == "brand.new"
    )));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn garbled_terminal_frame_stops_the_execution() {
    let mut script = ready();
    script.steps.push(Step::Frame {
        raw: "data: {\"type\":\"session.execution.succ\n\n".into(),
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    loop {
        match bench.event().await {
            AdapterEvent::Diagnostic { kind, required, .. } => {
                assert_eq!(kind, "unparsed_event");
                assert!(required);
                break;
            }
            AdapterEvent::Phase { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn lost_stream_stops_the_server_instead_of_waiting_forever() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::DropSse);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let mut lost = false;
    loop {
        match bench.event().await {
            AdapterEvent::Diagnostic {
                kind: "event_stream_lost",
                ..
            } => lost = true,
            AdapterEvent::ProcessExited { code, .. } => {
                assert_eq!(code, Some(130));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(lost);
    assert!(matches!(
        bench.transmit(&bench.input("y")).await,
        TransmitOutcome::Undetermined
    ));
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_death_during_a_turn_keeps_the_last_events() {
    let mut script = ready();
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::Die {
        code: 3,
        kill: true,
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let mut finished = false;
    loop {
        match bench.event().await {
            AdapterEvent::RunFinished { .. } => finished = true,
            AdapterEvent::ProcessExited { signal, .. } => {
                assert_eq!(signal, Some(9));
                break;
            }
            _ => {}
        }
    }
    assert!(
        finished,
        "the stream tail reaches the journal before the exit"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn next_event_is_cancel_safe() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, AdapterEvent::RunFinished { .. }))
    {
        if let Ok(event) =
            tokio::time::timeout(Duration::from_millis(1), bench.adapter.next_event()).await
        {
            events.push(event);
        }
    }
    assert_eq!(text_of(&events, "Text"), ["Hello world"]);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn debug_of_events_hides_the_conversation() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let shown = format!("{events:?}");
    assert!(!shown.contains("Hello"), "{shown}");
    assert!(!shown.contains("think more"), "{shown}");
    bench.close(CloseMode::Graceful).await;
}
