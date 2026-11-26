//! Пресеты инструкций Codex: собственный CODEX_HOME, developerInstructions,
//! перезапуск app-server между ходами. Только локальный двойник app-server и
//! временные каталоги; настоящий codex, сеть и ~/.codex не используются.
#![forbid(unsafe_code)]

use canterlot_worker::adapter::{
    Adapter, AdapterContext, AdapterError, AdapterEvent, ForkSpec, InstructionMode,
    InstructionSpec, SettingsChange, StartSpec, Transmission, TransmitOutcome,
    codex::home::{CodexHome, HomeKind, TempDirPolicy},
};
use canterlot_worker::config::CodexConfig;
use canterlot_worker::containment::TreeContainment;
use serde_json::{Value, json};

use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

use crate::fake_exec;

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
if (d / 'fail-spawn-break-home').exists():
    # Второй prepare запасного запуска упадёт: дом владельца больше не каталог.
    import shutil
    shutil.rmtree(d / 'owner-home')
    (d / 'owner-home').write_text('x')
    sys.exit(7)
if (d / 'stubborn').exists():
    # Не реагирует на EOF и SIGTERM, а stdout держит процесс вне группы сервера.
    import signal, subprocess, time
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    subprocess.Popen(['sleep', '26'], stdout=sys.stdout, stdin=subprocess.DEVNULL, start_new_session=True)
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
models = cfg('models.json', {"data": [{"id": "gpt-6-luna", "model": "gpt-6-luna",
    "displayName": "gpt-6-luna", "description": "d", "hidden": False, "isDefault": True,
    "defaultReasoningEffort": "low", "supportedReasoningEfforts": [
        {"reasoningEffort": e, "description": e} for e in ("low", "medium", "high")],
    "inputModalities": ["text", "image"]}], "nextCursor": None})
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
        if (d / 'die-account-once').exists() and not (d / 'died').exists():
            (d / 'died').write_text(str(pid))
            sys.exit(3)
        emit({'id': i, 'result': {'account': {'type': 'chatgpt'}}})
    elif m == 'config/read':
        emit({'id': i, 'result': config})
    elif m == 'model/list':
        emit({'id': i, 'result': models})
    elif m == 'thread/turns/list':
        emit({'id': i, 'result': {'data': []}})
    elif m in ('thread/start', 'thread/resume', 'thread/fork'):
        if m == 'thread/resume' and (d / 'resume-hang-once').exists() and not (d / 'hung').exists():
            # Сервер отвечает ошибкой и виснет, не реагируя на EOF stdin.
            (d / 'hung').write_text(str(pid))
            emit({'id': i, 'error': {'code': -32000, 'message': 'boom'}})
            import time
            time.sleep(120)
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
            if m == 'thread/fork':
                # Эфемерный форк: свой id на каждый запуск.
                t['thread']['id'] = f'fork-{pid}'
            emit({'id': i, 'result': t})
    elif m == 'turn/start':
        if (d / 'fail-turn-once').exists() and not (d / 'turn-failed').exists():
            (d / 'turn-failed').write_text('1')
            emit({'id': i, 'error': {'code': -32000, 'message': 'rejected'}})
            continue
        turn_no += 1
        tid = f"turn-{pid}-{turn_no}"
        t = p.get('threadId'); cid = p.get('clientUserMessageId')
        emit({'id': i, 'result': {'turn': {'id': tid, 'status': 'inProgress'}}})
        if (d / 'drop-echo-once').exists() and not (d / 'echo-dropped').exists():
            (d / 'echo-dropped').write_text('1')
        else:
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
if (d / 'stubborn').exists():
    time.sleep(120)
"#;

struct PresetHarness {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    old_pid: u32,
}

impl PresetHarness {
    async fn new() -> Self {
        Self::start_with(owner_spec(), None, |_| {}).await
    }

    async fn start_with(
        instructions: InstructionSpec,
        native: Option<&str>,
        setup: impl FnOnce(&std::path::Path),
    ) -> Self {
        Self::start_full(instructions, native, "ask", setup).await
    }

    async fn start_full(
        instructions: InstructionSpec,
        native: Option<&str>,
        permission: &str,
        setup: impl FnOnce(&std::path::Path),
    ) -> Self {
        let dir = TempDir::new().unwrap();
        setup(dir.path());
        Self::start_in(dir, instructions, native, permission, None).await
    }

    /// Запуск в готовом каталоге: второе исполнение видит дом и записи первого.
    async fn start_in(
        dir: TempDir,
        instructions: InstructionSpec,
        native: Option<&str>,
        permission: &str,
        fork: Option<ForkSpec>,
    ) -> Self {
        let exe = dir.path().join("fake-codex");
        if !exe.exists() {
            fake_exec::write_executable(&exe, PRESET_FAKE, 0o700);
        }
        let owner = dir.path().join("owner-home");
        std::fs::create_dir_all(&owner).unwrap();
        std::fs::write(owner.join("config.toml"), "model = \"x\"\n").unwrap();
        let canterlot = dir.path().join("codex-home");
        let context = AdapterContext {
            transmit_deadline: WAIT,
            ready_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: None,
            codex: Some(CodexConfig {
                executable: exe,
                home_dir: Some(canterlot),
                owner_home: Some(owner),
                home_policy: TempDirPolicy::Allow,
                ..Default::default()
            }),
            opencode: None,
            delegate_mcp_servers: Vec::new(),
        };
        let mut adapter = canterlot_worker::adapter::registry_with("codex", &context).unwrap();
        let group = TreeContainment::new().unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: dir.path().into(),
            native_conversation_id: native.map(str::to_owned),
            profile: "codex".into(),
            model: "gpt-6-luna".into(),
            effort: "low".into(),
            permission_mode: permission.into(),
            instructions,
            isolate_mcp: false,
            prompt_suggestions: false,
            disable_claude_auto_memory: false,
            fork,
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

    // Текст первого текстового блока каждого turn/start; порядок файлов pid
    // произволен, поэтому тесты ищут ход по содержимому.
    fn turn_texts(&self) -> Vec<String> {
        self.method_params("turn/start")
            .into_iter()
            .filter_map(|params| {
                params
                    .get("input")?
                    .as_array()?
                    .iter()
                    .find(|block| block["type"] == "text")?
                    .get("text")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    async fn transmit_text(
        &mut self,
        settings: Option<SettingsChange>,
        text: &str,
    ) -> TransmitOutcome {
        let t = Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: text.into(),
            files: vec![],
            settings,
        };
        tokio::time::timeout(WAIT, self.adapter.transmit(&t, &self.group))
            .await
            .expect("transmit deadline")
    }

    async fn transmit_with(
        &mut self,
        command: Uuid,
        attempt: u32,
        text: &str,
        settings: Option<SettingsChange>,
    ) -> TransmitOutcome {
        let t = Transmission {
            command_id: command,
            attempt,
            text: text.into(),
            files: vec![],
            settings,
        };
        tokio::time::timeout(WAIT, self.adapter.transmit(&t, &self.group))
            .await
            .expect("transmit deadline")
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
    assert!(
        !h.dir.path().join("codex-home").exists(),
        "owner start builds no canterlot home"
    );
    assert!(
        !spawns[0].1.contains(&"memories".to_string()),
        "{:?}",
        spawns[0].1
    );
}

#[tokio::test]
async fn clean_and_preset_start_spawns_the_first_app_server_under_canterlot_home() {
    for (mode, spec) in [("clean", clean_spec()), ("preset", preset_spec("текст"))] {
        let h = PresetHarness::start_with(spec, None, |_| {}).await;
        let spawns = h.spawns();
        assert_eq!(spawns.len(), 1, "{mode}: no restart needed");
        let env = h.env_of(h.old_pid);
        let canterlot = h.dir.path().join("codex-home");
        assert_eq!(
            env["CODEX_HOME"].as_str(),
            Some(canterlot.to_str().unwrap()),
            "{mode}: first process runs under the canterlot home: {env}"
        );
        assert!(
            env.get("CODEX_SQLITE_HOME").is_none_or(Value::is_null),
            "{mode}: {env}"
        );
        assert_eq!(
            std::fs::read_to_string(canterlot.join("AGENTS.md")).unwrap(),
            canterlot_worker::adapter::codex::mcp::SERVER_INSTRUCTIONS,
            "{mode}: home prepared before the spawn"
        );
        assert!(
            canterlot.join("config.toml").is_symlink(),
            "{mode}: owner entries linked"
        );
    }
}

// Настоящий ответ codex-cli 0.164: параметр path на рулон другого дома его не меняет.
const NO_ROLLOUT: &str = include_str!("fixtures/codex/thread_resume_no_rollout_live.json");

#[tokio::test]
async fn resume_of_a_missing_rollout_fails_once_without_a_path() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("resume-error.json"), NO_ROLLOUT).unwrap();
    let exe = dir.path().join("fake-codex");
    fake_exec::write_executable(&exe, PRESET_FAKE, 0o700);
    let owner = dir.path().join("owner-home");
    std::fs::create_dir_all(&owner).unwrap();
    let context = AdapterContext {
        transmit_deadline: WAIT,
        ready_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: None,
        codex: Some(CodexConfig {
            executable: exe,
            home_dir: Some(dir.path().join("codex-home")),
            owner_home: Some(owner),
            home_policy: TempDirPolicy::Allow,
            ..Default::default()
        }),
        opencode: None,
        delegate_mcp_servers: Vec::new(),
    };
    let mut adapter = canterlot_worker::adapter::registry_with("codex", &context).unwrap();
    let group = TreeContainment::new().unwrap();
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: dir.path().into(),
        native_conversation_id: Some("thread-1".into()),
        profile: String::new(),
        model: "gpt-6-luna".into(),
        effort: String::new(),
        permission_mode: "ask".into(),
        instructions: clean_spec(),
        isolate_mcp: false,
        prompt_suggestions: false,
        disable_claude_auto_memory: false,
        fork: None,
    };
    let error = adapter.start(&spec, &group).await.expect_err("no rollout");
    assert!(
        matches!(error, AdapterError::StartFailed("resume_failed")),
        "{error:?}"
    );
    let log = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("stdin-"))
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect::<String>();
    let resumes: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["method"] == "thread/resume")
        .collect();
    assert_eq!(resumes.len(), 1, "{resumes:?}");
    assert!(resumes[0]["params"].get("path").is_none(), "{resumes:?}");
}

#[tokio::test]
async fn clean_preset_keeps_project_agents_md_and_drops_global() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let home = CodexHome {
        path: h.dir.path().join("codex-home"),
        owner_home: h.dir.path().join("owner-home"),
        kind: HomeKind::Shared,
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
    let owner = owner_defaults_from(
        &json!({"config": {"developer_instructions": "Отвечай кратко."}, "origins": {}}),
    );
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
async fn missing_rollout_rejects_the_change_and_keeps_the_session_working() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    // Рулона нет: смена не состоялась, а сессия работает дальше на прежнем сервере.
    std::fs::write(h.dir.path().join("resume-error.json"), NO_ROLLOUT).unwrap();
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let resumes = h.method_params("thread/resume");
    assert!(resumes.iter().all(|params| params.get("path").is_none()));
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
}

#[tokio::test]
async fn real_file_in_the_canterlot_home_is_reported_as_a_conflict() {
    // Атомарная запись заменяет ссылку настоящим файлом: конфиг владельца отстаёт.
    let mut h = PresetHarness::start_with(clean_spec(), None, |dir| {
        std::fs::create_dir_all(dir.join("codex-home")).unwrap();
        std::fs::write(dir.join("codex-home/config.toml"), "").unwrap();
    })
    .await;
    let event = tokio::time::timeout(Duration::from_secs(3), h.adapter.next_event())
        .await
        .expect("conflict diagnostic");
    assert!(
        matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "codex_home_conflict",
                required: false,
                ..
            }
        ),
        "{event:?}"
    );
}

#[tokio::test]
async fn failed_home_in_the_fallback_reports_process_exited() {
    let mut h = PresetHarness::start_with(clean_spec(), None, |_| {}).await;
    std::fs::write(h.dir.path().join("fail-spawn-break-home"), "").unwrap();
    let out = h.transmit(Some(change(preset_spec("новый")))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let event = tokio::time::timeout(Duration::from_secs(3), h.adapter.next_event())
        .await
        .expect("the session without a server looks alive");
    assert!(
        matches!(event, AdapterEvent::ProcessExited { .. }),
        "{event:?}"
    );
}

#[tokio::test]
async fn restart_is_not_held_by_a_stray_process_with_the_old_stdout() {
    let mut h = PresetHarness::new().await;
    std::fs::write(h.dir.path().join("stubborn"), "").unwrap();
    // Запуск с флагом идёт уже в перезапуске: новый сервер сам станет упрямым.
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let out = h.transmit(Some(change(preset_spec("новый")))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
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

fn mode_change(mode: &str) -> SettingsChange {
    SettingsChange {
        permission_mode: Some(mode.into()),
        ..Default::default()
    }
}

fn write_json(dir: &std::path::Path, name: &str, value: Value) {
    std::fs::write(dir.join(name), value.to_string()).unwrap();
}

#[tokio::test]
async fn owner_preset_keeps_owner_developer_instructions_from_config_read() {
    let h = PresetHarness::start_with(owner_spec(), None, |dir| {
        write_json(
            dir,
            "config.json",
            json!({"config": {"developer_instructions": "OWNER-DEV-CANARY"},
                "origins": {}, "layers": null}),
        );
    })
    .await;
    let starts = h.method_params("thread/start");
    let dev = starts[0]["developerInstructions"].as_str().unwrap();
    assert!(
        dev.starts_with("OWNER-DEV-CANARY\n\n"),
        "owner text kept before the canterlot block: {dev}"
    );
}

#[tokio::test]
async fn switching_back_to_owner_restores_owner_policy_from_config() {
    let mut h = PresetHarness::start_with(owner_spec(), None, |dir| {
        write_json(
            dir,
            "config.json",
            json!({"config": {
                "approval_policy": "on-request",
                "sandbox_mode": "workspace-write",
                "sandbox_workspace_write": {"network_access": true, "writable_roots": ["/w"]},
            }, "origins": {}}),
        );
    })
    .await;
    for mode in ["bypass", "owner"] {
        let out = h.transmit(Some(mode_change(mode))).await;
        assert!(
            matches!(out, TransmitOutcome::Accepted(_)),
            "{mode}: {out:?}"
        );
        h.until_finished().await;
    }
    let starts = h.method_params("turn/start");
    assert_eq!(starts[0]["approvalPolicy"], "never");
    assert_eq!(starts[1]["approvalPolicy"], "on-request", "{}", starts[1]);
    assert_eq!(
        starts[1]["sandboxPolicy"],
        json!({"type": "workspaceWrite", "networkAccess": true, "writableRoots": ["/w"]}),
        "{}",
        starts[1]
    );
}

#[tokio::test]
async fn switching_back_to_owner_restores_policy_seen_at_owner_thread_start() {
    let mut h = PresetHarness::start_full(owner_spec(), None, "owner", |dir| {
        write_json(dir, "config.json", json!({"config": {}, "origins": {}}));
        write_json(
            dir,
            "thread.json",
            json!({"thread": {"id": "thread-1"}, "model": "gpt-6-luna",
                "approvalPolicy": "untrusted",
                "sandbox": {"type": "workspaceWrite", "networkAccess": false},
                "instructionSources": []}),
        );
    })
    .await;
    assert!(
        h.method_params("thread/start")[0]
            .get("approvalPolicy")
            .is_none()
    );
    for mode in ["bypass", "owner"] {
        let out = h.transmit(Some(mode_change(mode))).await;
        assert!(
            matches!(out, TransmitOutcome::Accepted(_)),
            "{mode}: {out:?}"
        );
        h.until_finished().await;
    }
    let starts = h.method_params("turn/start");
    assert_eq!(starts[1]["approvalPolicy"], "untrusted", "{}", starts[1]);
    assert_eq!(
        starts[1]["sandboxPolicy"],
        json!({"type": "workspaceWrite", "networkAccess": false})
    );
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| !rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn restart_dying_during_handshake_falls_back_to_the_previous_configuration() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    // Новый сервер живёт дольше секунды и умирает на account/read.
    std::fs::write(h.dir.path().join("die-account-once"), "").unwrap();
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    assert!(h.dir.path().join("died").exists(), "restart attempted");
    let mut events = vec![];
    while let Ok(event) =
        tokio::time::timeout(Duration::from_millis(300), h.adapter.next_event()).await
    {
        events.push(event);
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ProcessExited { .. })),
        "fallback with the previous configuration works: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::CliRestarted { .. })),
        "{events:?}"
    );
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
}

#[tokio::test]
async fn failed_restart_stops_the_failed_server_before_the_fallback() {
    let mut h = PresetHarness::new().await;
    std::fs::write(h.dir.path().join("resume-hang-once"), "").unwrap();
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let hung: u32 = std::fs::read_to_string(h.dir.path().join("hung"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        !alive(hung),
        "failed app-server {hung} is gone before the fallback runs"
    );
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
}

// --- Служебный блок инструкций в следующем ходе после смены пресета ---

const BLOCK_OPEN: &str = "<canterlot-instructions>";
const BLOCK_CLOSE: &str = "</canterlot-instructions>";

fn turn_with<'a>(texts: &'a [String], needle: &str) -> &'a str {
    texts
        .iter()
        .find(|text| text.contains(needle))
        .unwrap_or_else(|| panic!("no turn with {needle}: {texts:?}"))
}

fn assert_block_before_owner(text: &str, block_body: &str, owner: &str) {
    let expected = format!("{BLOCK_OPEN}\n{block_body}\n{BLOCK_CLOSE}\n\n{owner}");
    assert_eq!(text, expected);
}

#[tokio::test]
async fn preset_change_reaches_the_model_in_the_next_turn_input() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    let out = h
        .transmit_text(Some(change(preset_spec("CANARY-DEV-A"))), "owner-words")
        .await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    let texts = h.turn_texts();
    let changed = turn_with(&texts, "CANARY-DEV-A");
    assert_block_before_owner(
        changed,
        concat!(
            "[canterlot] The instructions preset of this conversation changed to «тест» ",
            "(revision 7); it replaces any earlier canterlot preset instructions in this thread.\n",
            "CANARY-DEV-A"
        ),
        "owner-words",
    );
    // developerInstructions на resume по-прежнему уходят: будущий CLI их применит.
    let resumes = h.method_params("thread/resume");
    assert_eq!(resumes.len(), 1, "{resumes:?}");
    assert!(
        resumes[0]["developerInstructions"]
            .as_str()
            .is_some_and(|text| text.contains("CANARY-DEV-A")),
        "{}",
        resumes[0]["developerInstructions"]
    );

    // Следующий ход блока уже не несёт.
    let out = h.transmit_text(None, "next-words").await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert_eq!(turn_with(&texts, "next-words"), "next-words");
}

#[tokio::test]
async fn clean_change_block_carries_only_the_note() {
    let mut h = PresetHarness::new().await;
    let out = h
        .transmit_text(Some(change(clean_spec())), "owner-words")
        .await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_block_before_owner(
        turn_with(&texts, "owner-words"),
        concat!(
            "[canterlot] This conversation no longer uses an instructions preset; ",
            "disregard earlier canterlot preset instructions in this thread."
        ),
        "owner-words",
    );
}

#[tokio::test]
async fn failed_change_leaves_no_block_for_the_next_turn() {
    let mut h = PresetHarness::new().await;
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    // Смена не состоялась: сервер вернулся к прежним инструкциям.
    std::fs::write(h.dir.path().join("resume-error.json"), NO_ROLLOUT).unwrap();
    let out = h
        .transmit_text(Some(change(clean_spec())), "rejected-words")
        .await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let out = h.transmit_text(None, "after-words").await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_eq!(turn_with(&texts, "after-words"), "after-words");
    assert!(
        texts.iter().all(|text| !text.contains(BLOCK_OPEN)),
        "{texts:?}"
    );
}

#[tokio::test]
async fn rejected_first_turn_resends_the_block_on_retry() {
    let mut h = PresetHarness::new().await;
    std::fs::write(h.dir.path().join("fail-turn-once"), "").unwrap();
    let command = Uuid::now_v7();
    let spec = preset_spec("CANARY-RETRY");
    let out = h
        .transmit_with(command, 1, "owner-words", Some(change(spec.clone())))
        .await;
    assert!(matches!(&out, TransmitOutcome::Rejected(_)), "{out:?}");
    let texts = h.turn_texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].starts_with(BLOCK_OPEN), "{}", texts[0]);
    assert!(texts[0].contains("CANARY-RETRY"), "{}", texts[0]);

    // Причина отказа не из настроек: хаб держит смену в ожидании и везёт её снова,
    // адаптер вернулся к прежним инструкциям, поэтому перезапуск и блок повторяются.
    let out = h
        .transmit_with(command, 2, "owner-words", Some(change(spec)))
        .await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_eq!(texts.len(), 2, "{texts:?}");
    let retried = turn_with(&texts, "owner-words");
    assert!(retried.starts_with(BLOCK_OPEN), "{retried}");
    assert!(retried.contains("CANARY-RETRY"), "{retried}");
    assert!(retried.ends_with("\n\nowner-words"), "{retried}");

    // Принятый ход снял блок: третий его уже не несёт.
    let out = h.transmit_text(None, "third-words").await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_eq!(turn_with(&texts, "third-words"), "third-words");
}

#[tokio::test]
async fn unconfirmed_first_turn_resends_the_block_on_retry() {
    let mut h = PresetHarness::new().await;
    // Сервер принял ход, но эха userMessage нет: доставка не доказана.
    std::fs::write(h.dir.path().join("drop-echo-once"), "").unwrap();
    let command = Uuid::now_v7();
    let out = h
        .transmit_with(
            command,
            1,
            "owner-words",
            Some(change(preset_spec("CANARY-UNCONFIRMED"))),
        )
        .await;
    assert!(matches!(out, TransmitOutcome::Undetermined), "{out:?}");
    let texts = h.turn_texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].starts_with(BLOCK_OPEN), "{}", texts[0]);
    assert!(texts[0].contains("CANARY-UNCONFIRMED"), "{}", texts[0]);

    // Неподтверждённый ввод повторяется вместе с блоком.
    let out = h.transmit_with(command, 2, "owner-words", None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    assert_eq!(texts.len(), 2, "{texts:?}");
    let retried = turn_with(&texts, "owner-words");
    assert!(retried.starts_with(BLOCK_OPEN), "{retried}");
    assert!(retried.contains("CANARY-UNCONFIRMED"), "{retried}");
}

#[tokio::test]
async fn oversized_joined_text_defers_the_block_to_a_later_turn() {
    use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
    let mut h = PresetHarness::new().await;
    // Ввод владельца сам по себе влезает, а вместе с блоком — уже нет.
    let big = "x".repeat(MAX_INPUT_TEXT_BYTES - 16);
    let out = h
        .transmit_text(Some(change(preset_spec("SMALL-PRESET"))), &big)
        .await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    let big_turn = turn_with(&texts, "xxxx");
    assert_eq!(big_turn.len(), MAX_INPUT_TEXT_BYTES - 16);
    assert!(!big_turn.contains(BLOCK_OPEN), "input not rejected");

    // Блок ждал следующего хода, где место есть.
    let out = h.transmit_text(None, "short").await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let texts = h.turn_texts();
    let short = turn_with(&texts, "short");
    assert!(short.starts_with(BLOCK_OPEN), "{short}");
    assert!(short.contains("SMALL-PRESET"), "{short}");
    assert!(short.ends_with("\n\nshort"), "{short}");
}

// --- Запись «что видел тред»: блок доходит и через новое исполнение ---

const THREAD: &str = "thread-1";

fn record_path(dir: &std::path::Path, thread: &str) -> std::path::PathBuf {
    dir.join("codex-home-threads")
        .join(format!("{thread}.json"))
}

fn read_record(dir: &std::path::Path, thread: &str) -> Option<Value> {
    let text = std::fs::read_to_string(record_path(dir, thread)).ok()?;
    Some(serde_json::from_str(&text).expect("record is json"))
}

fn write_record(dir: &std::path::Path, thread: &str, spec: &InstructionSpec) {
    let path = record_path(dir, thread);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        json!({"mode": spec.mode, "preset_id": spec.preset_id, "revision": spec.revision})
            .to_string(),
    )
    .unwrap();
}

fn assert_record(dir: &std::path::Path, thread: &str, spec: &InstructionSpec) {
    let record = read_record(dir, thread).unwrap_or_else(|| panic!("no record for {thread}"));
    assert_eq!(
        record,
        json!({"mode": spec.mode, "preset_id": spec.preset_id, "revision": spec.revision})
    );
}

impl PresetHarness {
    async fn close(self) -> TempDir {
        let Self {
            dir,
            group,
            mut adapter,
            ..
        } = self;
        tokio::time::timeout(
            WAIT,
            adapter.close(canterlot_worker::adapter::CloseMode::Graceful, &group),
        )
        .await
        .expect("close deadline")
        .expect("close ok");
        dir
    }

    async fn one_turn(&mut self, settings: Option<SettingsChange>, text: &str) -> String {
        let out = self.transmit_text(settings, text).await;
        assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
        self.until_finished().await;
        turn_with(&self.turn_texts(), text).to_owned()
    }
}

#[tokio::test]
async fn fresh_thread_records_the_start_instructions_without_text() {
    let spec = preset_spec("SECRET-PRESET-TEXT");
    let h = PresetHarness::start_with(spec.clone(), None, |_| {}).await;
    assert_record(h.dir.path(), THREAD, &spec);
    let raw = std::fs::read_to_string(record_path(h.dir.path(), THREAD)).unwrap();
    assert!(!raw.contains("SECRET-PRESET-TEXT"), "{raw}");
    assert!(!raw.contains("тест"), "title stays out: {raw}");
}

#[tokio::test]
async fn resume_with_a_stale_record_carries_the_block_in_the_first_turn() {
    // Тред видел «Без инструкций», а хаб уже считает действующим пресет.
    let spec = preset_spec("CANARY-STALE");
    let mut h = PresetHarness::start_with(spec.clone(), Some(THREAD), |dir| {
        write_record(dir, THREAD, &clean_spec());
    })
    .await;
    let first = h.one_turn(None, "owner-words").await;
    assert_block_before_owner(
        &first,
        concat!(
            "[canterlot] The instructions preset of this conversation changed to «тест» ",
            "(revision 7); it replaces any earlier canterlot preset instructions in this thread.\n",
            "CANARY-STALE"
        ),
        "owner-words",
    );
    assert_record(h.dir.path(), THREAD, &spec);
    assert_eq!(h.one_turn(None, "next-words").await, "next-words");
}

#[tokio::test]
async fn resume_with_a_matching_record_sends_no_block() {
    let spec = preset_spec("CANARY-SAME");
    let mut h = PresetHarness::start_with(spec.clone(), Some(THREAD), |dir| {
        write_record(dir, THREAD, &spec);
    })
    .await;
    assert_eq!(h.one_turn(None, "owner-words").await, "owner-words");
}

#[tokio::test]
async fn resume_without_a_record_resends_only_a_preset() {
    // Тред без записи (до обновления воркера): пресет повторяется, «Без инструкций» нет.
    let mut h =
        PresetHarness::start_with(preset_spec("CANARY-UNKNOWN"), Some(THREAD), |_| {}).await;
    let first = h.one_turn(None, "owner-words").await;
    assert!(first.starts_with(BLOCK_OPEN), "{first}");
    assert!(first.contains("CANARY-UNKNOWN"), "{first}");

    let mut h = PresetHarness::start_with(clean_spec(), Some(THREAD), |_| {}).await;
    assert_eq!(h.one_turn(None, "owner-words").await, "owner-words");
    assert!(read_record(h.dir.path(), THREAD).is_none());
}

#[tokio::test]
async fn corrupt_record_counts_as_missing_and_is_overwritten() {
    let spec = preset_spec("CANARY-CORRUPT");
    let mut h = PresetHarness::start_with(spec.clone(), Some(THREAD), |dir| {
        let path = record_path(dir, THREAD);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{not json").unwrap();
    })
    .await;
    let first = h.one_turn(None, "owner-words").await;
    assert!(first.starts_with(BLOCK_OPEN), "{first}");
    assert_record(h.dir.path(), THREAD, &spec);
}

#[tokio::test]
async fn deferred_block_reaches_the_thread_in_the_next_execution() {
    use canterlot_proto::limits::MAX_INPUT_TEXT_BYTES;
    let mut h = PresetHarness::start_with(clean_spec(), None, |_| {}).await;
    assert_record(h.dir.path(), THREAD, &clean_spec());
    // Ввод принят без блока: хаб уже считает пресет действующим, тред его не видел.
    let spec = preset_spec("CANARY-DEFERRED");
    let big = "x".repeat(MAX_INPUT_TEXT_BYTES - 16);
    let out = h.transmit_text(Some(change(spec.clone())), &big).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    assert!(!turn_with(&h.turn_texts(), "xxxx").contains(BLOCK_OPEN));
    assert_record(h.dir.path(), THREAD, &clean_spec());

    // Исполнение кончилось раньше следующего хода; новое стартует с пресетом хаба.
    let dir = h.close().await;
    let mut h = PresetHarness::start_in(dir, spec.clone(), Some(THREAD), "ask", None).await;
    let first = h.one_turn(None, "after-restart").await;
    assert!(first.starts_with(BLOCK_OPEN), "{first}");
    assert!(first.contains("CANARY-DEFERRED"), "{first}");
    assert!(first.ends_with("\n\nafter-restart"), "{first}");
    assert_record(h.dir.path(), THREAD, &spec);
}

#[tokio::test]
async fn side_chat_fork_compares_with_the_parent_record_and_writes_none() {
    let spec = preset_spec("CANARY-FORK");
    let dir = TempDir::new().unwrap();
    write_record(dir.path(), THREAD, &clean_spec());
    let fork = ForkSpec {
        parent_native_id: THREAD.into(),
        side_mcp: true,
    };
    let mut h = PresetHarness::start_in(dir, spec.clone(), None, "ask", Some(fork)).await;
    let first = h.one_turn(None, "side-words").await;
    assert!(first.starts_with(BLOCK_OPEN), "{first}");
    assert!(first.contains("CANARY-FORK"), "{first}");
    // Эфемерный форк записей не оставляет, запись родителя не меняется.
    let names: Vec<_> = std::fs::read_dir(h.dir.path().join("codex-home-threads"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![format!("{THREAD}.json")]);
    assert_record(h.dir.path(), THREAD, &clean_spec());

    // Совпадающая запись родителя: блок не нужен.
    let dir = h.close().await;
    write_record(dir.path(), THREAD, &spec);
    let fork = ForkSpec {
        parent_native_id: THREAD.into(),
        side_mcp: true,
    };
    let mut h = PresetHarness::start_in(dir, spec, None, "ask", Some(fork)).await;
    assert_eq!(h.one_turn(None, "side-again").await, "side-again");
}

// --- Отказ хода после удачного перезапуска возвращает прежние инструкции ---

impl PresetHarness {
    /// Процесс, в чей stdin ушёл ввод с этим текстом.
    fn pid_with_input(&self, needle: &str) -> u32 {
        for entry in std::fs::read_dir(self.dir.path()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            if let Some(pid) = name
                .strip_prefix("stdin-")
                .and_then(|rest| rest.strip_suffix(".log"))
                && std::fs::read_to_string(self.dir.path().join(&name))
                    .unwrap()
                    .contains(needle)
            {
                return pid.parse().unwrap();
            }
        }
        panic!("no process received {needle}");
    }
}

#[tokio::test]
async fn rejected_turn_after_a_restart_returns_to_the_previous_instructions_and_home() {
    let mut h = PresetHarness::new().await;
    std::fs::write(h.dir.path().join("fail-turn-once"), "").unwrap();
    let out = h
        .transmit_text(Some(change(preset_spec("CANARY-REVERT"))), "rejected-words")
        .await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "rejected"),
        "{out:?}"
    );
    // Перезапуск под пресетом, затем запасной запуск с прежними инструкциями.
    assert_eq!(h.spawns().len(), 3, "{:?}", h.spawns());

    let out = h.transmit_text(None, "after-words").await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    assert_eq!(turn_with(&h.turn_texts(), "after-words"), "after-words");
    assert_eq!(h.spawns().len(), 3, "no further restart");
    let env = h.env_of(h.pid_with_input("after-words"));
    assert_eq!(env["CODEX_HOME"], Value::Null, "owner home again: {env}");
    // Адаптер, запись треда и хаб снова согласны: тред видел только «Как у владельца».
    assert_record(h.dir.path(), THREAD, &owner_spec());
}
