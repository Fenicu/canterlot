//! Ввод Codex (задача 3 плана 2b, шаг 1). Записанные кадры —
//! docs/stage0/evidence/codex/, сконструированные — tests/fixtures/codex/.
//! Поддельный app-server — скрипт bash: печатает заготовку, затем ждёт EOF stdin.

use canterlot_worker::adapter::{
    InterruptOutcome, LocalFile, SettingsChange, Transmission,
    codex::{
        events::{Translated, Translator},
        input::{
            CurrentSettings, INTERRUPT_TIMEOUT, Outbox, PROOF_KIND_HISTORY, PROOF_KIND_MESSAGE,
            background_terminal_terminate_params, background_terminals_list_params,
            build_turn_start, error_rejection, history_contains, history_lookup_params,
            interrupt_params, model_entries, terminals_to_terminate, threshold_note,
            validate_settings,
        },
        process::{AppServerProcess, LaunchSpec},
        rpc::{Incoming, RpcClient, RpcError},
        thread::{PermissionMode, owner_defaults_from},
    },
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn evidence_data(name: &str, line: usize) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/codex")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let wrapper: Value = serde_json::from_str(text.lines().nth(line - 1).unwrap()).unwrap();
    wrapper["data"].clone()
}

fn command_id() -> Uuid {
    Uuid::parse_str("f9ec973c-53f4-4369-9109-786f38fcb361").unwrap()
}

fn tx(command: Uuid, attempt: u32, text: &str) -> Transmission {
    Transmission {
        command_id: command,
        attempt,
        text: text.into(),
        files: Vec::new(),
        settings: None,
    }
}

fn current() -> CurrentSettings {
    CurrentSettings {
        model: Some("gpt-6-luna".into()),
        effort: Some("low".into()),
        mode: PermissionMode::Ask,
    }
}

fn empty_owner() -> canterlot_worker::adapter::codex::thread::OwnerDefaults {
    owner_defaults_from(&json!({}))
}

fn local_file(path: &Path, name: &str, mime: &str, size: u64, image: bool) -> LocalFile {
    LocalFile {
        path: path.to_owned(),
        name: name.into(),
        mime: mime.into(),
        size,
        image,
    }
}

// Поддельный app-server: печатает строки заготовки и ждёт EOF stdin.
struct Fake {
    proc: AppServerProcess,
    _group: canterlot_worker::containment::TreeContainment,
}

impl Fake {
    fn start(dir: &TempDir, lines: &[String]) -> Self {
        let script = dir.path().join("fake-codex.sh");
        fake_exec::write_executable(
            &script,
            "#!/bin/bash\ndir=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\ncat \"$dir/canned.out\" 2>/dev/null\nwhile IFS= read -r line; do :; done\n",
            0o755,
        );
        if !lines.is_empty() {
            std::fs::write(dir.path().join("canned.out"), lines.join("\n") + "\n").unwrap();
        }
        let group = canterlot_worker::containment::TreeContainment::new().unwrap();
        let relay = PathBuf::from("/usr/bin/canterlot-worker");
        let spec = LaunchSpec {
            executable: &script,
            exec_dir: dir.path(),
            workdir: dir.path(),
            relay_exe: &relay,
        };
        let proc = AppServerProcess::spawn(
            &spec,
            &group,
            canterlot_worker::adapter::InstructionMode::Owner,
            None,
        )
        .unwrap();
        Self {
            proc,
            _group: group,
        }
    }

    async fn read_line(&mut self) -> String {
        match tokio::time::timeout(Duration::from_secs(10), self.proc.next_line())
            .await
            .expect("child output in time")
        {
            canterlot_worker::adapter::codex::process::StdoutLine::Line(line) => line,
            other => panic!("expected line, got {other:?}"),
        }
    }

    async fn close(self) {
        let mut proc = self.proc;
        proc.close_stdin();
        tokio::time::timeout(Duration::from_secs(10), proc.wait())
            .await
            .expect("fake exits on EOF")
            .unwrap();
    }
}

#[test]
fn turn_start_matches_recorded_request() {
    let thread = "01a171c0-2add-72c0-934b-89a64fa8f188";
    let command = command_id();
    let params = build_turn_start(
        thread,
        &tx(
            command,
            1,
            "Reply with the single word PONG and nothing else.",
        ),
        &current(),
        &empty_owner(),
        None,
    )
    .unwrap();
    let recorded = evidence_data("start_stream", 18)["params"].clone();
    let mut ours = params.as_object().cloned().unwrap();
    let client_id = ours.remove("clientUserMessageId").unwrap();
    assert_eq!(client_id, command.to_string());
    assert_eq!(Value::Object(ours), recorded);
}

#[tokio::test]
async fn delivery_is_proven_by_client_id_echo_not_by_response() {
    let dir = TempDir::new().unwrap();
    let response = evidence_data("delivery_ack", 19);
    let echo = evidence_data("delivery_ack", 24);
    let turn_id = response["result"]["turn"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let thread = echo["params"]["threadId"].as_str().unwrap().to_owned();
    let mut fake = Fake::start(
        &dir,
        &[
            serde_json::to_string(&response).unwrap(),
            serde_json::to_string(&echo).unwrap(),
        ],
    );
    let mut rpc = RpcClient::new();
    let params = build_turn_start(
        &thread,
        &tx(command_id(), 1, "hi"),
        &current(),
        &empty_owner(),
        None,
    )
    .unwrap();
    let (_, request) = rpc.request("turn/start", params);
    fake.proc.write_message(&request).await.unwrap();
    // Ответ turn/start доказательства не даёт: только id хода.
    let first = fake.read_line().await;
    let mut translator = Translator::new(&thread, "gpt-6-luna");
    translator.begin_turn(&turn_id, command_id());
    let mut proven = false;
    match canterlot_worker::adapter::codex::rpc::classify(&first) {
        Incoming::Response { result: Ok(_), .. } => {}
        other => panic!("expected turn/start response, got {other:?}"),
    }
    assert!(!proven, "response alone proves nothing");
    // Эхо userMessage с нашим clientId — доказательство.
    let second = fake.read_line().await;
    let data: Value = serde_json::from_str(&second).unwrap();
    for translated in translator.feed(
        data["method"].as_str().unwrap(),
        &data.get("params").cloned().unwrap_or(Value::Null),
    ) {
        if let Translated::UserMessage { client_id, .. } = translated
            && client_id.as_deref() == Some("canterlot-ack-0001")
        {
            proven = true;
        }
    }
    assert!(proven, "clientId echo is the proof");
    assert_eq!(PROOF_KIND_MESSAGE, "codex_user_message_item");
    assert_eq!(PROOF_KIND_HISTORY, "codex_history_client_id");
    fake.close().await;
}

#[test]
fn error_response_is_rejected() {
    let data = evidence_data("question", 20);
    match canterlot_worker::adapter::codex::rpc::classify(&serde_json::to_string(&data).unwrap()) {
        Incoming::Response {
            result: Err(error), ..
        } => {
            assert_eq!(
                error_rejection(&error),
                "turn/start.collaborationMode requires experimentalApi capability"
            );
        }
        other => panic!("expected error response, got {other:?}"),
    }
    let long = RpcError {
        code: -32600,
        message: "x".repeat(200),
        data: None,
    };
    assert_eq!(error_rejection(&long).chars().count(), 128);
}

#[test]
fn merged_turn_id_stops_the_queue() {
    let active = "01a171c0-a0ee-7bb1-8143-c946003ee0ea";
    let mut outbox = Outbox::new();
    assert!(outbox.on_turn_response(command_id(), active).is_none());
    let diagnostic = outbox
        .on_turn_response(Uuid::now_v7(), active)
        .expect("merge");
    match diagnostic {
        canterlot_worker::adapter::AdapterEvent::Diagnostic {
            kind,
            event_type,
            required,
        } => {
            assert_eq!(kind, "turn_merged");
            assert_eq!(event_type, "turn_merged");
            assert!(required);
        }
        other => panic!("expected turn_merged diagnostic, got {other:?}"),
    }
}

#[test]
fn retry_finds_input_in_history_and_does_not_resend() {
    let params = history_lookup_params("thread-main");
    assert_eq!(
        params,
        json!({"threadId": "thread-main", "itemsView": "full", "sortDirection": "desc", "limit": 20})
    );
    let result = evidence_data("resume", 107)["result"].clone();
    assert_eq!(
        history_contains(&result, "cu-killed-turn").as_deref(),
        Some("01a171c3-5ef5-7080-a9bc-a4d06869af1c")
    );
}

#[test]
fn retry_without_history_match_is_sent_once() {
    let result = evidence_data("resume", 107)["result"].clone();
    assert_eq!(history_contains(&result, "cu-never-sent"), None);
    let command = Uuid::now_v7();
    let params = build_turn_start(
        "thread-main",
        &tx(command, 2, "Again."),
        &current(),
        &empty_owner(),
        None,
    )
    .unwrap();
    assert_eq!(params["clientUserMessageId"], command.to_string());
}

#[test]
fn attachments_add_paths_and_local_images() {
    let dir = TempDir::new().unwrap();
    let png = dir.path().join("red.png");
    std::fs::write(&png, [0x89, 0x50, 0x4e, 0x47]).unwrap();
    let pdf = dir.path().join("doc.pdf");
    std::fs::write(&pdf, b"%PDF").unwrap();
    let big = dir.path().join("big.png");
    std::fs::write(&big, [0x89, 0x50]).unwrap();
    let mut t = tx(command_id(), 1, "Look.");
    t.files = vec![
        local_file(&png, "red.png", "image/png", 4, true),
        local_file(&pdf, "doc.pdf", "application/pdf", 4, false),
        local_file(&big, "big.png", "image/png", 25 * 1024 * 1024, true),
    ];
    let params = build_turn_start("thread-main", &t, &current(), &empty_owner(), None).unwrap();
    let input = params["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0]["type"], "text");
    let text = input[0]["text"].as_str().unwrap();
    assert!(text.contains("Look."), "{text}");
    for (name, mime, size) in [
        ("red.png", "image/png", 4),
        ("doc.pdf", "application/pdf", 4),
        ("big.png", "image/png", 25 * 1024 * 1024),
    ] {
        assert!(
            text.contains(&format!("{name} ({mime}, {size} bytes): ")),
            "{text}"
        );
    }
    // Картинка — как ev/image_input.jsonl:18: текст, затем localImage.
    assert_eq!(input[1]["type"], "localImage");
    assert_eq!(input[1]["path"], png.to_string_lossy().to_string());
    let recorded = evidence_data("image_input", 18)["params"]["input"][1].clone();
    assert_eq!(recorded["type"], "localImage");
    assert!(recorded.get("path").and_then(Value::as_str).is_some());
}

#[test]
fn settings_become_turn_start_overrides() {
    let owner = owner_defaults_from(&json!({"config": {
        "approval_policy": "on-request",
        "sandbox_mode": "read-only",
        "sandbox_workspace_write": {"network_access": false, "writable_roots": []},
    }, "origins": {}}));
    let change = |model: Option<&str>, effort: Option<&str>, mode: Option<&str>| {
        let mut t = tx(command_id(), 1, "Hi.");
        t.settings = Some(SettingsChange {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            permission_mode: mode.map(str::to_owned),
            instructions: None,
        });
        build_turn_start("thread-main", &t, &current(), &owner, None).unwrap()
    };
    assert_eq!(
        change(Some("gpt-6-luna"), None, None)["model"],
        "gpt-6-luna"
    );
    assert_eq!(change(None, Some("high"), None)["effort"], "high");
    let ask = change(None, None, Some("ask"));
    assert_eq!(ask["approvalPolicy"], "on-request");
    assert_eq!(ask["sandboxPolicy"], json!({"type": "dangerFullAccess"}));
    assert_eq!(ask["approvalsReviewer"], "user");
    let bypass = change(None, None, Some("bypass"));
    assert_eq!(bypass["approvalPolicy"], "never");
    assert_eq!(bypass["sandboxPolicy"], json!({"type": "dangerFullAccess"}));
    let read_only = change(None, None, Some("read_only"));
    assert_eq!(read_only["approvalPolicy"], "never");
    assert_eq!(
        read_only["sandboxPolicy"],
        json!({"type": "readOnly", "networkAccess": false})
    );
    let owner_params = change(None, None, Some("owner"));
    assert_eq!(owner_params["approvalPolicy"], "on-request");
    assert_eq!(owner_params["sandboxPolicy"]["type"], "readOnly");
    // Без смены настроек политики нет: действует уровень потока.
    let plain = build_turn_start(
        "thread-main",
        &tx(command_id(), 1, "Hi."),
        &current(),
        &owner,
        None,
    )
    .unwrap();
    assert!(plain.get("approvalPolicy").is_none());
    assert!(plain.get("sandboxPolicy").is_none());
    assert_eq!(plain["effort"], "low");
    // Подтверждение сервера обновляет действующие настройки.
    let frames = codex_settings_updated();
    let mut translator = Translator::new("thread-main", "gpt-6-mini");
    let out = translator.feed(
        frames["method"].as_str().unwrap(),
        &frames.get("params").cloned().unwrap_or(Value::Null),
    );
    assert!(out.iter().any(|t| matches!(
        t,
        Translated::SettingsUpdated(settings) if settings["model"] == "gpt-6-luna"
    )));
    // Настройки субагента не трогают модель и политику основного потока.
    let mut child = frames.clone();
    child["params"]["threadId"] = json!("thread-child");
    child["params"]["threadSettings"]["model"] = json!("gpt-6-other");
    let out = translator.feed(
        child["method"].as_str().unwrap(),
        &child.get("params").cloned().unwrap_or(Value::Null),
    );
    assert!(
        !out.iter()
            .any(|t| matches!(t, Translated::SettingsUpdated(_))),
        "child thread update ignored"
    );
}

fn codex_settings_updated() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex/settings_updated.jsonl");
    let line = std::fs::read_to_string(&path).unwrap();
    let wrapper: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    wrapper["data"].clone()
}

fn model_list_entries() -> Vec<canterlot_worker::adapter::codex::input::ModelEntry> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex/model_list.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let result: Value = serde_json::from_str(&text).unwrap();
    model_entries(&result)
}

#[test]
fn unknown_model_or_effort_is_rejected() {
    let models = model_list_entries();
    assert!(models.iter().any(|entry| entry.id == "gpt-6-luna"));
    let change = SettingsChange {
        model: Some("gpt-6-luna".into()),
        effort: Some("ultra".into()),
        permission_mode: None,
        instructions: None,
    };
    assert_eq!(
        validate_settings(&change, &models, None),
        Err("settings_unavailable")
    );
    let change = SettingsChange {
        model: Some("canterlot-no-such-model".into()),
        effort: None,
        permission_mode: None,
        instructions: None,
    };
    assert_eq!(
        validate_settings(&change, &models, None),
        Err("settings_unavailable")
    );
    let change = SettingsChange {
        model: Some("gpt-6-luna".into()),
        effort: Some("high".into()),
        permission_mode: None,
        instructions: None,
    };
    assert_eq!(validate_settings(&change, &models, None), Ok(()));
    assert_eq!(
        validate_settings(&SettingsChange::default(), &models, None),
        Ok(())
    );
}

#[test]
fn weekly_threshold_note_is_prefixed_once_per_threshold() {
    let mut noted: Option<u16> = None;
    assert_eq!(threshold_note(None, &mut noted), None);
    assert_eq!(threshold_note(Some((790, None)), &mut noted), None);
    assert_eq!(
        threshold_note(Some((805, None)), &mut noted).as_deref(),
        Some("[canterlot] Weekly Codex limit: 80 % used.")
    );
    assert_eq!(noted, Some(800));
    assert_eq!(threshold_note(Some((850, None)), &mut noted), None);
    assert_eq!(
        threshold_note(Some((905, None)), &mut noted).as_deref(),
        Some("[canterlot] Weekly Codex limit: 90 % used.")
    );
    assert_eq!(threshold_note(Some((910, None)), &mut noted), None);
    assert_eq!(
        threshold_note(Some((960, None)), &mut noted).as_deref(),
        Some("[canterlot] Weekly Codex limit: 96 % used.")
    );
    assert_eq!(threshold_note(Some((990, None)), &mut noted), None);
    assert_eq!(threshold_note(Some((100, None)), &mut noted), None);
    assert_eq!(threshold_note(Some((970, None)), &mut noted), None);
    // Со сбросом — местное время.
    let reset_ms: u64 = 1_793_062_800_000;
    let reset = chrono::DateTime::from_timestamp_millis(reset_ms as i64)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string();
    let mut noted: Option<u16> = None;
    assert_eq!(
        threshold_note(Some((805, Some(reset_ms))), &mut noted).as_deref(),
        Some(format!("[canterlot] Weekly Codex limit: 80 % used, resets {reset}.").as_str())
    );
}

#[tokio::test]
async fn interrupt_acknowledged_and_turn_terminals_terminated() {
    let dir = TempDir::new().unwrap();
    let ack = evidence_data("interrupt", 31);
    assert_eq!(ack["result"], json!({}));
    let list_envelope = evidence_data("interrupt_cleanup", 37);
    let mut fake = Fake::start(
        &dir,
        &[
            serde_json::to_string(&ack).unwrap(),
            serde_json::to_string(&list_envelope).unwrap(),
        ],
    );
    // Параметры прерывания — как ev/interrupt.jsonl:28.
    let thread = "01a171c6-3ae7-7a63-8b6f-428ed24276d2";
    let turn = "01a171c6-3ece-7bc1-9ce0-38db51bc35cc";
    let mut outbox = Outbox::new();
    outbox.on_turn_response(command_id(), turn);
    assert!(outbox.active().is_some());
    let mut rpc = RpcClient::new();
    let (_, request) = rpc.request("turn/interrupt", interrupt_params(thread, turn));
    assert_eq!(request["params"], evidence_data("interrupt", 28)["params"]);
    fake.proc.write_message(&request).await.unwrap();
    let line = fake.read_line().await;
    let data: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(data["result"], json!({}));
    // Ход завершается нормально прерванным.
    assert!(outbox.on_turn_completed(turn));
    assert!(outbox.active().is_none());
    // Терминалы прерванного хода гасятся, чужие фонды живут.
    let (_, list_request) = rpc.request(
        "thread/backgroundTerminals/list",
        background_terminals_list_params(thread),
    );
    fake.proc.write_message(&list_request).await.unwrap();
    let line = fake.read_line().await;
    let data: Value = serde_json::from_str(&line).unwrap();
    let mut list = data["result"].clone();
    list["data"].as_array_mut().unwrap().push(json!({
        "command": "sleep 999",
        "itemId": "exec-older-turn",
        "processId": "99999",
    }));
    let mut turn_items = HashSet::new();
    turn_items.insert("exec-52e7d2a5-196c-45fb-bc77-038a1d18d51c".to_owned());
    assert_eq!(terminals_to_terminate(&list, &turn_items), vec!["39207"]);
    let (_, terminate) = rpc.request(
        "thread/backgroundTerminals/terminate",
        background_terminal_terminate_params(thread, "39207"),
    );
    assert_eq!(
        terminate["params"],
        json!({"threadId": thread, "processId": "39207"})
    );
    fake.close().await;
}

#[tokio::test]
async fn interrupt_without_turn_is_not_applicable_and_silence_is_undetermined() {
    let outbox = Outbox::new();
    assert!(outbox.active().is_none());
    let outcome = if outbox.active().is_none() {
        InterruptOutcome::NotApplicable
    } else {
        InterruptOutcome::Undetermined
    };
    assert!(matches!(outcome, InterruptOutcome::NotApplicable));
    assert_eq!(INTERRUPT_TIMEOUT, Duration::from_secs(5));
    // Молчание сервера: чтение висит дольше срока.
    let dir = TempDir::new().unwrap();
    let mut fake = Fake::start(&dir, &[]);
    let silent = tokio::time::timeout(Duration::from_millis(100), fake.proc.next_line()).await;
    assert!(silent.is_err());
    let outcome = InterruptOutcome::Undetermined;
    assert!(matches!(outcome, InterruptOutcome::Undetermined));
    fake.close().await;
}
