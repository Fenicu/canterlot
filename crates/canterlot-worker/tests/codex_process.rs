//! Задача 1 плана 2b: запуск Codex app-server, версия, окружение, транспорт
//! JSON-RPC, рукопожатие, профили и параметры потока. Настоящий `codex`
//! не запускается, сети нет: версии проверяет скрипт-обманка, кадры берутся
//! из docs/stage0/evidence/codex/.
use canterlot_worker::adapter::codex::process::{
    AppServerProcess, LaunchSpec, StdoutLine, build_args, build_env,
};
use canterlot_worker::adapter::codex::rpc::{Incoming, RpcClient, RpcId, classify};
use canterlot_worker::adapter::codex::thread::{
    ChangeNote, PermissionMode, StartError, account_read_params, check_account,
    compose_developer_instructions, config_read_params, initialize_params, owner_defaults_from,
    parse_thread_result, thread_resume_params, thread_start_params, turn_policy,
};
use canterlot_worker::adapter::codex::version::{CliVersion, VersionError, check, parse_version};
use canterlot_worker::adapter::{InstructionMode, InstructionSpec};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn evidence(name: &str) -> Vec<serde_json::Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/codex")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

// Строка свидетельства нумеруется с единицы, как в плане; нужно поле `data`.
fn data(name: &str, line: usize) -> serde_json::Value {
    evidence(name)[line - 1]["data"].clone()
}

fn fixture_lines() -> Vec<serde_json::Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex/handshake_owner.jsonl");
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn fake_version_script(dir: &TempDir, output: &str) -> PathBuf {
    let script = dir.path().join("codex");
    fake_exec::write_executable(&script, format!("#!/bin/sh\necho \"{output}\"\n"), 0o755);
    script
}

fn spec<'a>(exec_dir: &'a Path, workdir: &'a Path, relay: &'a Path) -> LaunchSpec<'a> {
    LaunchSpec {
        executable: Path::new("/usr/bin/codex"),
        exec_dir,
        workdir,
        relay_exe: relay,
    }
}

#[tokio::test]
async fn version_parsing_and_classification() {
    assert_eq!(parse_version("codex-cli 0.164.0").unwrap(), (0, 164, 0));
    assert_eq!(parse_version("0.164.0").unwrap(), (0, 164, 0));
    assert!(parse_version("garbage").is_err());
    let td = TempDir::new().unwrap();
    let exe = fake_version_script(&td, "codex-cli 0.164.0");
    assert_eq!(
        check(&exe).await.unwrap(),
        CliVersion::Verified("0.164.0".into())
    );
    let exe = fake_version_script(&td, "codex-cli 0.165.2");
    assert_eq!(
        check(&exe).await.unwrap(),
        CliVersion::UntestedNewer("0.165.2".into())
    );
    let exe = fake_version_script(&td, "codex-cli 0.163.9");
    assert_eq!(
        check(&exe).await.unwrap_err(),
        VersionError::BelowMinimum("0.163.9".into())
    );
    assert_eq!(
        check(Path::new("/nonexistent/codex-test-binary"))
            .await
            .unwrap_err(),
        VersionError::NotFound
    );
}

#[test]
fn launch_args_add_only_feature_flag_and_canterlot_mcp() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let relay = PathBuf::from("/usr/bin/canterlot-worker");
    let args = build_args(&spec(&exec_dir, &workdir, &relay), InstructionMode::Owner).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let sock = exec_dir.join("mcp.sock").to_string_lossy().into_owned();
    assert_eq!(
        s,
        vec![
            "app-server".to_string(),
            "--listen".to_string(),
            "stdio://".to_string(),
            "--enable".to_string(),
            "default_mode_request_user_input".to_string(),
            "-c".to_string(),
            "mcp_servers.canterlot.command=\"/usr/bin/canterlot-worker\"".to_string(),
            "-c".to_string(),
            format!("mcp_servers.canterlot.args=[\"mcp-relay\", \"--socket\", \"{sock}\"]"),
        ]
    );
    let joined = s.join(" ");
    for forbidden in [
        "approval_policy",
        "sandbox_mode",
        "model",
        "--disable",
        "project_doc_max_bytes",
        "history.persistence",
    ] {
        assert!(!joined.contains(forbidden), "forbidden {forbidden}");
    }
}

#[test]
fn mcp_override_values_are_valid_toml_for_awkward_paths() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("dir with space \"q\"");
    let workdir = td.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    let relay = td.path().join("bin \"dir/canterlot-worker");
    let args = build_args(&spec(&exec_dir, &workdir, &relay), InstructionMode::Owner).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let mut seen = 0;
    for i in 0..s.len() {
        if s[i] != "-c" {
            continue;
        }
        let arg = &s[i + 1];
        let (key, value) = arg.split_once('=').unwrap();
        let doc: toml::Table = toml::from_str(&format!("k = {value}")).unwrap();
        match key {
            "mcp_servers.canterlot.command" => {
                assert_eq!(doc["k"].as_str().unwrap(), relay.to_string_lossy());
                seen += 1;
            }
            "mcp_servers.canterlot.args" => {
                let arr = doc["k"].as_array().unwrap();
                let strs: Vec<_> = arr.iter().map(|v| v.as_str().unwrap()).collect();
                assert_eq!(
                    strs,
                    vec![
                        "mcp-relay",
                        "--socket",
                        exec_dir.join("mcp.sock").to_string_lossy().as_ref() as &str,
                    ]
                );
                seen += 1;
            }
            other => panic!("unexpected -c key {other}"),
        }
    }
    assert_eq!(seen, 2);
}

#[test]
fn environment_drops_billing_and_leaked_variables() {
    let pairs = vec![
        ("OPENAI_API_KEY", "x"),
        ("CODEX_API_KEY", "x"),
        ("OPENAI_BASE_URL", "x"),
        ("CLAUDECODE", "x"),
        ("CLAUDE_CODE_ENTRYPOINT", "x"),
        ("CANTERLOT_X", "x"),
        ("RUST_LOG", "x"),
        ("HOME", "/h"),
        ("PATH", "/p"),
        ("SSH_AUTH_SOCK", "/s"),
        ("HTTPS_PROXY", "http://p"),
        ("CODEX_HOME", "/c"),
    ];
    let it = pairs
        .into_iter()
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let out = build_env(it, InstructionMode::Owner, None);
    let keys: Vec<String> = out
        .iter()
        .map(|(k, _)| k.to_string_lossy().to_string())
        .collect();
    assert_eq!(
        keys,
        vec!["HOME", "PATH", "SSH_AUTH_SOCK", "HTTPS_PROXY", "CODEX_HOME"]
    );
}

#[test]
fn rpc_classifies_responses_notifications_and_server_requests() {
    // Ответ turn/start.
    let line = serde_json::to_string(&data("permissions.jsonl", 18)).unwrap();
    match classify(&line) {
        Incoming::Response { id, result } => {
            assert_eq!(id, RpcId::Int(3));
            assert!(result.is_ok());
        }
        other => panic!("expected Response, got {other:?}"),
    }
    // Уведомление turn/started.
    let line = serde_json::to_string(&data("permissions.jsonl", 21)).unwrap();
    assert!(matches!(
        classify(&line),
        Incoming::Notification { method, .. } if method == "turn/started"
    ));
    // Запрос сервера с тем же числовым id 0, что и ответ config/read (:6).
    let response = serde_json::to_string(&data("permissions.jsonl", 6)).unwrap();
    assert!(matches!(
        classify(&response),
        Incoming::Response {
            id: RpcId::Int(0),
            ..
        }
    ));
    let request = serde_json::to_string(&data("permissions.jsonl", 27)).unwrap();
    match classify(&request) {
        Incoming::ServerRequest { id, method, .. } => {
            assert_eq!(id, RpcId::Int(0));
            assert_eq!(method, "item/commandExecution/requestApproval");
        }
        other => panic!("expected ServerRequest, got {other:?}"),
    }
    // Ответ-ошибка.
    let line = serde_json::to_string(&data("question.jsonl", 20)).unwrap();
    match classify(&line) {
        Incoming::Response {
            result: Err(err), ..
        } => assert_eq!(err.code, -32600),
        other => panic!("expected error Response, got {other:?}"),
    }
    assert!(matches!(classify("not json"), Incoming::Invalid));
}

#[test]
fn initialize_then_initialized_with_experimental_api() {
    let mut rpc = RpcClient::new();
    let (id, msg) = rpc.request("initialize", initialize_params("0.1.0-test"));
    assert_eq!(id, 0);
    let recorded = data("question.jsonl", 74);
    assert_eq!(msg["id"], recorded["id"]);
    assert_eq!(msg["method"], recorded["method"]);
    let mut ours = msg["params"].as_object().cloned().unwrap();
    let mut theirs = recorded["params"].as_object().cloned().unwrap();
    let client = ours.remove("clientInfo").unwrap();
    assert_eq!(client["name"], "canterlot");
    assert_eq!(client["title"], "canterlot");
    let caps = ours
        .get_mut("capabilities")
        .unwrap()
        .as_object_mut()
        .unwrap();
    assert_eq!(
        caps.remove("experimentalApi"),
        Some(serde_json::json!(true))
    );
    assert_eq!(
        caps.remove("requestAttestation"),
        Some(serde_json::json!(false))
    );
    assert_eq!(
        caps.remove("optOutNotificationMethods"),
        Some(serde_json::json!([
            "turn/moderationMetadata",
            "remoteControl/status/changed",
            "thread/readState/changed",
            "turn/diff/updated",
        ]))
    );
    theirs.remove("clientInfo");
    theirs
        .get_mut("capabilities")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("experimentalApi");
    assert_eq!(
        serde_json::Value::Object(ours),
        serde_json::Value::Object(theirs)
    );
    // `initialized` пишется только после ответа; байты совпадают с записью.
    let initialized = RpcClient::notification("initialized", serde_json::json!({}));
    assert_eq!(initialized, data("question.jsonl", 77));
}

#[test]
fn owner_defaults_from_config_read_and_redacted_debug() {
    let lines = fixture_lines();
    let owner = owner_defaults_from(&lines[0]);
    assert_eq!(
        owner
            .approval_policy
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .unwrap(),
        "never"
    );
    assert_eq!(owner.sandbox_mode.as_deref().unwrap(), "danger-full-access");
    assert_eq!(
        owner.sandbox_workspace_write.as_ref().unwrap()["network_access"],
        true
    );
    assert_eq!(
        owner.developer_instructions.as_deref().unwrap(),
        "Отвечай кратко."
    );
    let debug = format!("{owner:?}");
    assert!(!debug.contains("Отвечай кратко."));
    assert!(!debug.contains("danger-full-access"));
}

#[test]
fn not_logged_in_fails_start_with_auth_required() {
    let lines = fixture_lines();
    assert_eq!(
        check_account(&lines[1]).unwrap_err(),
        StartError::AuthRequired
    );
    assert_eq!(
        check_account(&serde_json::json!({"account": null, "requiresOpenaiAuth": false}))
            .unwrap_err(),
        StartError::AuthRequired
    );
    check_account(&serde_json::json!({"account": {"type": "chatgpt"}})).unwrap();
}

#[test]
fn developer_instructions_by_mode() {
    let lines = fixture_lines();
    let owner = owner_defaults_from(&lines[0]);
    let bare = owner_defaults_from(&serde_json::json!({}));
    let owner_spec = InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    };
    let clean_spec = InstructionSpec {
        mode: InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    };
    let preset_spec = InstructionSpec {
        mode: InstructionMode::Preset,
        text: "текст".to_string(),
        preset_id: Some(Uuid::now_v7()),
        revision: 3,
        title: "t".to_string(),
    };
    // Владелец: свой текст плюс блок; без текста — один блок.
    assert_eq!(
        compose_developer_instructions(&owner_spec, &owner, "BLOCK", ChangeNote::None),
        "Отвечай кратко.\n\nBLOCK"
    );
    assert_eq!(
        compose_developer_instructions(&owner_spec, &bare, "BLOCK", ChangeNote::None),
        "BLOCK"
    );
    // «Без инструкций»: пусто, перекрывает developer_instructions владельца.
    assert_eq!(
        compose_developer_instructions(&clean_spec, &owner, "BLOCK", ChangeNote::None),
        ""
    );
    // Пресет: только его текст.
    assert_eq!(
        compose_developer_instructions(&preset_spec, &owner, "BLOCK", ChangeNote::None),
        "текст"
    );
    // Примечание о смене — первой строкой.
    let noted = compose_developer_instructions(
        &preset_spec,
        &owner,
        "BLOCK",
        ChangeNote::To {
            title: "T",
            revision: 3,
        },
    );
    assert!(noted.starts_with("[canterlot]"), "{noted}");
    assert!(noted.contains("T") && noted.contains('3') && noted.ends_with("текст"));
    let to_clean =
        compose_developer_instructions(&clean_spec, &owner, "BLOCK", ChangeNote::ToClean);
    assert!(to_clean.starts_with("[canterlot]"), "{to_clean}");
    let to_owner =
        compose_developer_instructions(&owner_spec, &owner, "BLOCK", ChangeNote::ToOwner);
    assert!(to_owner.starts_with("[canterlot]"), "{to_owner}");
}

#[test]
fn permission_modes_map_to_thread_and_turn_policy() {
    assert_eq!(
        PermissionMode::parse("owner").unwrap(),
        PermissionMode::Owner
    );
    assert_eq!(PermissionMode::parse("ask").unwrap(), PermissionMode::Ask);
    assert_eq!(
        PermissionMode::parse("bypass").unwrap(),
        PermissionMode::Bypass
    );
    assert_eq!(
        PermissionMode::parse("read_only").unwrap(),
        PermissionMode::ReadOnly
    );
    assert!(PermissionMode::parse("nope").is_err());
    let workdir = Path::new("/tmp/work");
    for (mode, policy, sandbox) in [
        (
            PermissionMode::Ask,
            Some("on-request"),
            Some("danger-full-access"),
        ),
        (
            PermissionMode::Bypass,
            Some("never"),
            Some("danger-full-access"),
        ),
        (PermissionMode::ReadOnly, Some("never"), Some("read-only")),
        (PermissionMode::Owner, None, None),
    ] {
        let params = thread_start_params(mode, Some("gpt-6-luna"), workdir, "BLOCK");
        assert_eq!(
            params.get("approvalPolicy").and_then(|v| v.as_str()),
            policy
        );
        assert_eq!(params.get("sandbox").and_then(|v| v.as_str()), sandbox);
        let resume = thread_resume_params("tid", mode, None, workdir, "BLOCK", None);
        assert_eq!(
            resume.get("approvalPolicy").and_then(|v| v.as_str()),
            policy
        );
        assert_eq!(resume.get("sandbox").and_then(|v| v.as_str()), sandbox);
    }
    // turn/start: строки у не-владельца, значения владельца у owner.
    let empty = owner_defaults_from(&serde_json::json!({}));
    assert_eq!(
        turn_policy(PermissionMode::Ask, &empty),
        (
            Some(serde_json::json!("on-request")),
            Some(serde_json::json!({"type": "dangerFullAccess"}))
        )
    );
    assert_eq!(
        turn_policy(PermissionMode::Bypass, &empty),
        (
            Some(serde_json::json!("never")),
            Some(serde_json::json!({"type": "dangerFullAccess"}))
        )
    );
    assert_eq!(
        turn_policy(PermissionMode::ReadOnly, &empty),
        (
            Some(serde_json::json!("never")),
            Some(serde_json::json!({"type": "readOnly", "networkAccess": false}))
        )
    );
    // Владелец без политики в конфигурации и без наблюдения треда: умолчания
    // Codex для недоверенного проекта, никогда не меньше вопросов.
    assert_eq!(
        turn_policy(PermissionMode::Owner, &empty),
        (
            Some(serde_json::json!("on-request")),
            Some(serde_json::json!({"type": "readOnly", "networkAccess": false}))
        )
    );
    // Значения config/read лежат в обёртке `config` (схема ConfigReadResponse).
    let owner = owner_defaults_from(&serde_json::json!({"config": {
        "approval_policy": "on-request",
        "sandbox_mode": "read-only",
        "sandbox_workspace_write": {"network_access": true, "writable_roots": ["/w"]},
    }, "origins": {}}));
    assert_eq!(
        turn_policy(PermissionMode::Owner, &owner),
        (
            Some(serde_json::json!("on-request")),
            Some(serde_json::json!({"type": "readOnly"}))
        )
    );
    // Песочница записи — в форме SandboxPolicy (camelCase), не ключами TOML.
    let write = owner_defaults_from(&serde_json::json!({"config": {
        "approval_policy": "untrusted",
        "sandbox_mode": "workspace-write",
        "sandbox_workspace_write": {"network_access": true, "writable_roots": ["/w"],
            "exclude_tmpdir_env_var": true, "exclude_slash_tmp": false},
    }, "origins": {}}));
    assert_eq!(
        turn_policy(PermissionMode::Owner, &write),
        (
            Some(serde_json::json!("untrusted")),
            Some(serde_json::json!({"type": "workspaceWrite", "networkAccess": true,
                "writableRoots": ["/w"], "excludeTmpdirEnvVar": true, "excludeSlashTmp": false}))
        )
    );
    let full = owner_defaults_from(&serde_json::json!({"config": {
        "approval_policy": "never", "sandbox_mode": "danger-full-access",
    }, "origins": {}}));
    assert_eq!(
        turn_policy(PermissionMode::Owner, &full).1,
        Some(serde_json::json!({"type": "dangerFullAccess"}))
    );
    // Без политики в конфигурации — то, что сервер показал при старте треда владельцем.
    let mut observed = owner_defaults_from(&serde_json::json!({"config": {}, "origins": {}}));
    observed.observe_thread(
        &serde_json::json!("untrusted"),
        &serde_json::json!({"type": "workspaceWrite", "networkAccess": false}),
    );
    assert_eq!(
        turn_policy(PermissionMode::Owner, &observed),
        (
            Some(serde_json::json!("untrusted")),
            Some(serde_json::json!({"type": "workspaceWrite", "networkAccess": false}))
        )
    );
    // Плоский ответ без обёртки — не конфигурация владельца.
    let flat = owner_defaults_from(&serde_json::json!({"approval_policy": "never"}));
    assert!(flat.approval_policy.is_none());
}

#[test]
fn thread_start_params_and_effective_settings() {
    let workdir = Path::new("/tmp/canterlot-stage0/codex/start_stream/run-1");
    let params = thread_start_params(PermissionMode::Owner, Some("gpt-6-luna"), workdir, "BLOCK");
    // Форма записи ev/start_stream.jsonl:13: те же ключи, кроме политики зонда,
    // плюс developerInstructions; имя сервиса наше.
    let recorded = &data("start_stream.jsonl", 13)["params"];
    let mut ours: Vec<String> = params.as_object().unwrap().keys().cloned().collect();
    ours.sort();
    let mut theirs: Vec<String> = recorded.as_object().unwrap().keys().cloned().collect();
    theirs.retain(|k| k != "approvalPolicy" && k != "sandbox");
    theirs.push("developerInstructions".to_owned());
    theirs.sort();
    assert_eq!(ours, theirs);
    assert_eq!(params["approvalsReviewer"], "user");
    assert_eq!(params["ephemeral"], false);
    assert_eq!(params["model"], "gpt-6-luna");
    assert_eq!(params["serviceName"], "canterlot");
    assert_eq!(params["developerInstructions"], "BLOCK");
    // Ответ :14 даёт действующие настройки.
    let effective = parse_thread_result(&data("start_stream.jsonl", 14)["result"]).unwrap();
    assert_eq!(effective.thread_id, "01a171c0-2add-72c0-934b-89a64fa8f188");
    assert_eq!(effective.model, "gpt-6-luna");
    assert_eq!(effective.approval_policy, serde_json::json!("on-request"));
    assert_eq!(
        effective.sandbox,
        serde_json::json!({"networkAccess": false, "type": "readOnly"})
    );
    assert_eq!(effective.instruction_sources, 1);
    assert_eq!(effective.cli_version, "0.164.0");
}

#[test]
fn resume_params_exclude_turns() {
    let workdir = Path::new("/tmp/canterlot-stage0/codex/resume/run-1");
    let params = thread_resume_params(
        "01a171c3-4681-7b81-bd8b-be81cac722c8",
        PermissionMode::Ask,
        Some("gpt-6-luna"),
        workdir,
        "BLOCK",
        None,
    );
    // Форма записи ev/resume.jsonl:49 плюс developerInstructions.
    let recorded = &data("resume.jsonl", 49)["params"];
    assert_eq!(params["threadId"], recorded["threadId"]);
    assert_eq!(params["excludeTurns"], true);
    assert_eq!(params["approvalPolicy"], recorded["approvalPolicy"]);
    assert_eq!(params["approvalsReviewer"], recorded["approvalsReviewer"]);
    assert_eq!(params["cwd"], workdir.to_string_lossy().to_string());
    assert_eq!(params["model"], recorded["model"]);
    // У зонда изоляция read-only; у нас в ask песочницы нет (решение 29.10).
    assert_eq!(params["sandbox"], "danger-full-access");
    assert_eq!(params["developerInstructions"], "BLOCK");
    assert_eq!(
        account_read_params(),
        serde_json::json!({"refreshToken": false})
    );
    assert_eq!(
        config_read_params(workdir),
        serde_json::json!({"cwd": workdir.to_string_lossy(), "includeLayers": false})
    );
}

#[tokio::test]
async fn oversized_stdout_line_is_skipped_and_reading_continues() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        "#!/bin/sh\nhead -c 68157440 /dev/zero | tr '\\0' a; echo\nprintf '%s\\n' '{}'\n",
        0o755,
    );
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let relay = PathBuf::from("/usr/bin/canterlot-worker");
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        relay_exe: &relay,
    };
    let mut proc = AppServerProcess::spawn(
        &spec,
        &group,
        canterlot_worker::adapter::InstructionMode::Owner,
        None,
    )
    .unwrap();
    match tokio::time::timeout(Duration::from_secs(60), proc.next_line()).await {
        Ok(StdoutLine::Oversized(n)) => assert!(n > 64 * 1024 * 1024),
        other => panic!("expected Oversized, got {other:?}"),
    }
    match tokio::time::timeout(Duration::from_secs(60), proc.next_line()).await {
        Ok(StdoutLine::Line(l)) => assert_eq!(l, "{}"),
        other => panic!("expected Line, got {other:?}"),
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(60), proc.next_line())
            .await
            .unwrap(),
        StdoutLine::Eof
    ));
}
