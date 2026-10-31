#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Адаптер OpenCode на двойнике `fake-opencode` (L1): настоящий процесс, HTTP и SSE.
//! Сценарии собраны из записей этапа 0.

use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, CloseMode, InstructionMode, InstructionSpec,
        StartSpec, Started,
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
        let adapter = OpenCodeAdapter::new(cfg, exec_dir.clone(), Duration::from_secs(2))
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

    fn env(&self, key: &str) -> Option<String> {
        self.fake.boot()["env"][key].as_str().map(str::to_owned)
    }
}

fn ready() -> Script {
    Script::from_capture("start_stream", None).with_canterlot_mcp("connected")
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
