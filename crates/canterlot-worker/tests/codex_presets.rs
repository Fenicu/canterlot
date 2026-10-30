//! Пресеты инструкций Codex: собственный CODEX_HOME, developerInstructions,
//! перезапуск app-server между ходами. Только локальный двойник app-server и
//! временные каталоги; настоящий codex, сеть и ~/.codex не используются.
#![forbid(unsafe_code)]

use canterlot_worker::adapter::{
    Adapter, AdapterContext, AdapterEvent, InstructionMode, InstructionSpec, SettingsChange,
    StartSpec, Transmission, TransmitOutcome,
    codex::home::{CodexHome, TempDirPolicy},
};
use canterlot_worker::config::CodexConfig;
use canterlot_worker::containment::TreeContainment;
use serde_json::{Value, json};

use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

const WAIT: Duration = Duration::from_secs(20);

fn owner_spec() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn clean_spec() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn preset_spec(text: &str) -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Preset,
        text: text.to_string(),
        preset_id: Some(Uuid::now_v7()),
        revision: 7,
        title: "тест".to_string(),
    }
}

fn change(instructions: InstructionSpec) -> SettingsChange {
    SettingsChange {
        instructions: Some(instructions),
        ..Default::default()
    }
}

// --- Двойник app-server: отвечает по скрипту, пишет args/env/stdin по pid. ---

const PRESET_FAKE: &str = r#"#!/usr/bin/env python3
import json, sys, os, pathlib
d = pathlib.Path(__file__).parent
pid = os.getpid()
if '--version' in sys.argv:
    print('codex-cli 0.164.0'); sys.exit(0)
if (d / 'fail-spawn').exists():
    sys.exit(7)
if (d / 'fail-once').exists() and not (d / 'spawned').exists():
    (d / 'spawned').write_text('1')
    sys.exit(7)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
(d / f'env-{pid}.json').write_text(json.dumps({
    'CODEX_HOME': os.environ.get('CODEX_HOME'),
    'CODEX_SQLITE_HOME': os.environ.get('CODEX_SQLITE_HOME'),
}))
def cfg(name, default):
    p = d / name
    return json.loads(p.read_text()) if p.exists() else default
def emit(obj):
    print(json.dumps(obj), flush=True)
config = cfg('config.json', {})
models = cfg('models.json', {"models": [{"id": "gpt-6-luna", "default": True,
    "efforts": ["low", "medium", "high"]}]})
thread = cfg('thread.json', None)
turn_no = 0
for line in sys.stdin:
    (d / f'stdin-{pid}.log').open('a').write(line)
    try: msg = json.loads(line)
    except: continue
    m = msg.get('method'); i = msg.get('id'); p = msg.get('params') or {}
    if m == 'initialize':
        emit({'id': i, 'result': {'codexHome': '/h'}})
    elif m == 'initialized':
        pass
    elif m == 'account/read':
        emit({'id': i, 'result': {'account': {'type': 'chatgpt'}}})
    elif m == 'config/read':
        emit({'id': i, 'result': config})
    elif m == 'model/list':
        emit({'id': i, 'result': models})
    elif m in ('thread/start', 'thread/resume'):
        err = cfg('resume-error.json', None)
        if m == 'thread/resume' and err is not None and not (d / 'resume-retried').exists():
            (d / 'resume-retried').write_text('1')
            emit({'id': i, 'error': err})
        else:
            t = dict(thread) if isinstance(thread, dict) else {
                "thread": {"id": "thread-1"}, "model": "gpt-6-luna",
                "approvalPolicy": "on-request", "sandbox": {"type": "readOnly"},
                "instructionSources": []}
            if m == 'thread/start' and isinstance(thread, dict):
                t = dict(thread)
            t['thread'] = dict(t.get('thread', {}))
            t['thread']['cliVersion'] = '0.164.0'
            emit({'id': i, 'result': t})
    elif m == 'turn/start':
        turn_no += 1
        tid = f"turn-{pid}-{turn_no}"
        t = p.get('threadId'); cid = p.get('clientUserMessageId')
        emit({'id': i, 'result': {'turn': {'id': tid, 'status': 'inProgress'}}})
        emit({'method': 'item/started', 'params': {'threadId': t, 'turnId': tid,
            'item': {'id': 'u', 'type': 'userMessage', 'clientId': cid, 'content': []}}})
        emit({'method': 'item/completed', 'params': {'threadId': t, 'turnId': tid,
            'completedAtMs': 1,
            'item': {'id': 'u', 'type': 'userMessage', 'clientId': cid, 'content': []}}})
        emit({'method': 'item/started', 'params': {'threadId': t, 'turnId': tid,
            'item': {'id': 'a', 'type': 'agentMessage', 'text': f'TURN-{turn_no}'}}})
        emit({'method': 'item/completed', 'params': {'threadId': t, 'turnId': tid,
            'completedAtMs': 2,
            'item': {'id': 'a', 'type': 'agentMessage', 'text': f'TURN-{turn_no}'}}})
        emit({'method': 'turn/completed', 'params': {'threadId': t,
            'turn': {'id': tid, 'status': 'completed', 'items': []}}})
"#;

struct PresetHarness {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    old_pid: u32,
}

impl PresetHarness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-codex");
        fake_exec::write_executable(&exe, PRESET_FAKE, 0o700);
        let owner = dir.path().join("owner-home");
        std::fs::create_dir_all(&owner).unwrap();
        std::fs::write(owner.join("config.toml"), "model = \"x\"\n").unwrap();
        let canterlot = dir.path().join("codex-home");
        let context = AdapterContext {
            transmit_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: None,
            codex: Some(CodexConfig {
                executable: exe,
                home_dir: Some(canterlot),
                owner_home: Some(owner),
                home_policy: TempDirPolicy::Allow,
                ..Default::default()
            }),
        };
        let mut adapter = canterlot_worker::adapter::registry_with("codex", &context).unwrap();
        let group = TreeContainment::new().unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: dir.path().into(),
            native_conversation_id: None,
            profile: "codex".into(),
            model: "gpt-6-luna".into(),
            effort: "low".into(),
            permission_mode: "ask".into(),
            instructions: owner_spec(),
        };
        let started = tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start deadline")
            .expect("start ok");
        let old_pid = started.cli_pid;
        Self {
            dir,
            group,
            adapter,
            old_pid,
        }
    }

    fn tx(&self, settings: Option<SettingsChange>) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: "test".into(),
            files: vec![],
            settings,
        }
    }

    async fn transmit(&mut self, settings: Option<SettingsChange>) -> TransmitOutcome {
        let t = self.tx(settings);
        tokio::time::timeout(WAIT, self.adapter.transmit(&t, &self.group))
            .await
            .expect("transmit deadline")
    }

    async fn until_finished(&mut self) -> Vec<AdapterEvent> {
        let mut out = vec![];
        loop {
            let event = tokio::time::timeout(WAIT, self.adapter.next_event())
                .await
                .expect("event deadline");
            let done = matches!(event, AdapterEvent::RunFinished { .. });
            out.push(event);
            if done {
                return out;
            }
        }
    }

    fn spawns(&self) -> Vec<(u32, Vec<String>)> {
        let mut out = vec![];
        for entry in std::fs::read_dir(self.dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(pid) = name
                .strip_prefix("args-")
                .and_then(|s| s.strip_suffix(".json"))
                && let Ok(pid) = pid.parse::<u32>()
            {
                let args: Vec<String> =
                    serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
                out.push((pid, args));
            }
        }
        out.sort_by_key(|(pid, _)| *pid);
        out
    }

    fn env_of(&self, pid: u32) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.dir.path().join(format!("env-{pid}.json"))).unwrap(),
        )
        .unwrap()
    }

    fn stdin_lines(&self) -> Vec<Value> {
        let mut out = vec![];
        for entry in std::fs::read_dir(self.dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("stdin-") && name.ends_with(".log") {
                for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    fn method_params(&self, method: &str) -> Vec<Value> {
        self.stdin_lines()
            .into_iter()
            .filter(|v| v.get("method").and_then(Value::as_str) == Some(method))
            .filter_map(|v| v.get("params").cloned())
            .collect()
    }
}

#[tokio::test]
async fn clean_and_preset_launch_set_codex_home_and_drop_sqlite_home() {
    for (mode, spec) in [("clean", clean_spec()), ("preset", preset_spec("текст"))] {
        let mut h = PresetHarness::new().await;
        // Перезапуск со сменой инструкций: новый процесс уже под canterlot-домом.
        let out = h.transmit(Some(change(spec))).await;
        assert!(
            matches!(out, TransmitOutcome::Accepted(_)),
            "{mode}: {out:?}"
        );
        let spawns = h.spawns();
        assert_eq!(spawns.len(), 2, "{mode}");
        let new_pid = spawns
            .iter()
            .find(|(pid, _)| *pid != h.old_pid)
            .expect("new process")
            .0;
        let env = h.env_of(new_pid);
        let home = env["CODEX_HOME"].as_str().expect("CODEX_HOME set");
        assert!(
            home.ends_with("codex-home"),
            "{mode}: canterlot home: {home}"
        );
        assert!(
            env.get("CODEX_SQLITE_HOME").is_none_or(Value::is_null),
            "{mode}: sqlite home dropped: {env}"
        );
    }
}

#[tokio::test]
async fn owner_launch_keeps_environment_untouched() {
    let h = PresetHarness::new().await;
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 1);
    let env = h.env_of(h.old_pid);
    // «Как у владельца»: настоящий CODEX_HOME процесса, без подмены.
    assert_eq!(
        env["CODEX_HOME"],
        Value::Null,
        "owner keeps real environment: {env}"
    );
}

#[tokio::test]
async fn clean_preset_keeps_project_agents_md_and_drops_global() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let home = CodexHome {
        path: h.dir.path().join("codex-home"),
        owner_home: h.dir.path().join("owner-home"),
    };
    // Глобальный AGENTS.md владельца не ссылается; блок canterlot на месте.
    assert!(!home.path.join("AGENTS.md").is_symlink());
    assert_eq!(
        std::fs::read_to_string(home.path.join("AGENTS.md")).unwrap(),
        canterlot_worker::adapter::codex::mcp::SERVER_INSTRUCTIONS
    );
    // Аргументы без project_doc_max_bytes: AGENTS.md репозитория действует.
    let spawns = h.spawns();
    let args = &spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap().1;
    assert!(
        !args.iter().any(|a| a.contains("project_doc_max_bytes")),
        "{args:?}"
    );
}

#[tokio::test]
async fn clean_and_preset_args_disable_memories_only() {
    let mut h = PresetHarness::new().await;
    for (mode, spec) in [("clean", clean_spec()), ("preset", preset_spec("текст"))] {
        // Перезапуск ради флагов нового процесса; смотреть его argv.
        let out = h.transmit(Some(change(spec))).await;
        assert!(
            matches!(out, TransmitOutcome::Accepted(_)),
            "{mode}: {out:?}"
        );
        h.until_finished().await;
    }
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 3, "{spawns:?}");
    for (pid, args) in spawns.iter().filter(|(pid, _)| *pid != h.old_pid) {
        assert!(
            args.contains(&"--disable".to_string()) && args.contains(&"memories".to_string()),
            "memories disabled for restarted {pid}: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a.contains("project_doc_max_bytes")),
            "{args:?}"
        );
    }
}

#[tokio::test]
async fn developer_instructions_by_mode() {
    use canterlot_worker::adapter::codex::thread::{
        ChangeNote, compose_developer_instructions, owner_defaults_from,
    };
    let owner = owner_defaults_from(&json!({"developer_instructions": "Отвечай кратко."}));
    let bare = owner_defaults_from(&json!({}));
    // Владелец: свой текст плюс блок.
    assert_eq!(
        compose_developer_instructions(&owner_spec(), &owner, "BLOCK", ChangeNote::None),
        "Отвечай кратко.\n\nBLOCK"
    );
    assert_eq!(
        compose_developer_instructions(&owner_spec(), &bare, "BLOCK", ChangeNote::None),
        "BLOCK"
    );
    // «Без инструкций»: пусто, перекрывает owner developer_instructions.
    assert_eq!(
        compose_developer_instructions(&clean_spec(), &owner, "BLOCK", ChangeNote::None),
        ""
    );
    // Пресет: только его текст.
    assert_eq!(
        compose_developer_instructions(&preset_spec("текст"), &owner, "BLOCK", ChangeNote::None),
        "текст"
    );
    // Примечания о смене идут первой строкой.
    let noted = compose_developer_instructions(
        &preset_spec("текст"),
        &owner,
        "BLOCK",
        ChangeNote::To {
            title: "T",
            revision: 3,
        },
    );
    assert!(noted.starts_with("[canterlot]"), "{noted}");
    assert!(noted.contains("T") && noted.contains('3'));
    let to_clean =
        compose_developer_instructions(&clean_spec(), &owner, "BLOCK", ChangeNote::ToClean);
    assert!(to_clean.starts_with("[canterlot]"), "{to_clean}");
    let to_owner =
        compose_developer_instructions(&owner_spec(), &owner, "BLOCK", ChangeNote::ToOwner);
    assert!(to_owner.starts_with("[canterlot]"), "{to_owner}");
    // Текст пресета не попадает в Debug.
    let debug = format!("{:?}", preset_spec("секретный текст"));
    assert!(!debug.contains("секретный текст"), "{debug}");
}

#[tokio::test]
async fn instructions_change_restarts_app_server_and_resumes_with_new_developer_instructions() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    let out = h.transmit(Some(change(preset_spec("CANARY-DEV-A")))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "one restart expected");
    let new_pid = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap().0;

    // Новый процесс продолжает тот же тред с новыми инструкциями.
    let resumes = h.method_params("thread/resume");
    assert_eq!(resumes.len(), 1, "resume once");
    assert!(
        resumes[0]["developerInstructions"]
            .as_str()
            .is_some_and(|text| text.starts_with("[canterlot]") && text.contains("CANARY-DEV-A")),
        "preset text reaches resume with a note: {}",
        resumes[0]["developerInstructions"]
    );
    // Turn/start ушёл только в новый процесс.
    let starts: Vec<_> = h
        .method_params("turn/start")
        .into_iter()
        .filter(|p| {
            p.get("clientUserMessageId")
                .and_then(Value::as_str)
                .is_some_and(|id| Uuid::parse_str(id).is_ok())
        })
        .collect();
    assert_eq!(starts.len(), 2, "one turn per process: {starts:?}");

    // CliRestarted — до ответа нового процесса.
    let events = h.until_finished().await;
    let restarted_at = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::CliRestarted { .. }))
        .expect("CliRestarted");
    let first_block_at = events
        .iter()
        .position(|e| matches!(e, AdapterEvent::BlockStarted { .. }));
    assert!(
        first_block_at.is_none_or(|at| restarted_at < at),
        "restart first: {events:?}"
    );
    let restarted = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::CliRestarted { cli_pid, .. } => Some(*cli_pid),
            _ => None,
        })
        .expect("CliRestarted");
    assert_eq!(restarted, new_pid);
}

#[tokio::test]
async fn failed_fallback_restart_reports_process_exited() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    std::fs::write(h.dir.path().join("fail-spawn"), "").unwrap();

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let mut saw_exited = false;
    for _ in 0..8 {
        let event = tokio::time::timeout(WAIT, h.adapter.next_event())
            .await
            .expect("event deadline");
        if matches!(event, AdapterEvent::ProcessExited { .. }) {
            saw_exited = true;
            break;
        }
    }
    assert!(saw_exited, "fallback failure stops the execution");
}

#[tokio::test]
async fn cross_home_resume_falls_back_to_rollout_path() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    // Тред известен только по rollout в доме владельца.
    std::fs::write(
        h.dir.path().join("resume-error.json"),
        r#"{"code": -32000, "message": "Thread thread-1 not found"}"#,
    )
    .unwrap();
    let sessions = h.dir.path().join("owner-home").join("sessions").join("sub");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("rollout-2026-thread-1.jsonl"), "{}\n").unwrap();

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let resumes = h.method_params("thread/resume");
    assert_eq!(resumes.len(), 2, "retry with path: {resumes:?}");
    assert!(
        resumes[1]
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|p| p.contains("rollout-2026-thread-1.jsonl")),
        "second resume uses rollout path: {}",
        resumes[1]
    );
}

#[tokio::test]
async fn failed_restart_respawns_previous_and_rejects_the_input() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    std::fs::write(h.dir.path().join("fail-once"), "").unwrap();

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    // Первый перезапуск упал, запасной со старыми настройками поднялся.
    assert!(
        h.dir.path().join("spawned").exists(),
        "restart was attempted"
    );
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "fallback spawn expected");
    // Ввод не записан ни в упавший, ни в запасной процесс.
    let turn_starts = h.method_params("turn/start");
    assert_eq!(turn_starts.len(), 1, "input not sent: {turn_starts:?}");
}
