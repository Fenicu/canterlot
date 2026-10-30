//! Пресеты инструкций Claude Code: чистый запуск без файлов владельца,
//! текст пресета через файл, перезапуск CLI между ходами.
//! Только локальные двойники и временные каталоги; настоящий CLI и сеть не используются.
#![forbid(unsafe_code)]

use canterlot_worker::{
    adapter::{
        Adapter, AdapterContext, AdapterEvent, InstructionMode, InstructionSpec, SettingsChange,
        StartSpec, Transmission, TransmitOutcome,
        claude_code::process::{
            LaunchSpec, SessionArg, build_args, needs_restart, owner_instruction_excludes,
            repository_root, settings_json, write_instructions_file,
        },
    },
    config::ClaudeCodeConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
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
        revision: 3,
        title: "тест".to_string(),
    }
}

fn change(instructions: InstructionSpec) -> SettingsChange {
    SettingsChange {
        instructions: Some(instructions),
        ..Default::default()
    }
}

/// Дерево для проверки excludes: home/CLAUDE.md, home/projects/CLAUDE.md,
/// home/projects/repo/.git, repo/CLAUDE.md, repo/sub/CLAUDE.md.
fn fake_tree() -> TempDir {
    let td = TempDir::new().unwrap();
    let home = td.path().join("home");
    let repo = home.join("projects").join("repo");
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join("CLAUDE.md"), "owner").unwrap();
    std::fs::write(home.join("projects").join("CLAUDE.md"), "ancestor").unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("CLAUDE.md"), "repo").unwrap();
    std::fs::write(sub.join("CLAUDE.md"), "nested").unwrap();
    td
}

#[test]
fn clean_preset_excludes_ancestor_and_user_files_but_keeps_repository_claude_md() {
    let td = fake_tree();
    let home = td.path().join("home");
    let repo = home.join("projects").join("repo");
    let excludes = owner_instruction_excludes(&repo.join("sub"), &home);
    let has = |p: &str| excludes.iter().any(|e| e == p);
    // Файлы владельца строго выше корня репозитория — в списке.
    assert!(has(&format!(
        "{}/CLAUDE.md",
        home.join("projects").display()
    )));
    assert!(has(&format!(
        "{}/CLAUDE.local.md",
        home.join("projects").display()
    )));
    assert!(has(&format!(
        "{}/.claude/CLAUDE.md",
        home.join("projects").display()
    )));
    assert!(has(&format!(
        "{}/AGENTS.md",
        home.join("projects").display()
    )));
    assert!(has(&format!(
        "{}/.claude/rules/**",
        home.join("projects").display()
    )));
    // Пользовательский уровень — в списке.
    assert!(has(&format!("{}/.claude/CLAUDE.md", home.display())));
    assert!(has(&format!("{}/.claude/rules/**", home.display())));
    // Файлы самого репозитория — никогда в списке.
    assert!(
        !excludes
            .iter()
            .any(|e| e.starts_with(&repo.display().to_string()))
    );
    assert!(!excludes.is_empty());
}

#[test]
fn repository_root_is_git_toplevel_or_workdir() {
    let td = TempDir::new().unwrap();
    // Каталог .git: корень — его родитель.
    let repo = td.path().join("repo");
    let sub = repo.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    assert_eq!(repository_root(&sub), repo);
    // Файл .git (worktree, субмодуль): корень — его каталог.
    let wt = td.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /srv/x").unwrap();
    assert_eq!(repository_root(&wt), wt);
    // Без git: корень — сам рабочий каталог.
    let plain = td.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(repository_root(&plain), plain);
}

#[test]
fn home_as_repository_root_still_excludes_owner_personal_files() {
    let td = TempDir::new().unwrap();
    let home = td.path().join("home");
    std::fs::create_dir_all(home.join(".git")).unwrap();
    std::fs::create_dir_all(home.join("sub")).unwrap();
    std::fs::write(home.join("CLAUDE.md"), "owner").unwrap();
    std::fs::write(home.join("CLAUDE.local.md"), "owner").unwrap();
    std::fs::write(home.join("AGENTS.md"), "owner").unwrap();
    std::fs::write(home.join("sub").join("CLAUDE.md"), "repo").unwrap();
    let excludes = owner_instruction_excludes(&home.join("sub"), &home);
    let has = |p: &str| excludes.iter().any(|e| e == p);
    // Личные файлы владельца — в списке, даже если корень репозитория это $HOME.
    assert!(has(&format!("{}/CLAUDE.md", home.display())));
    assert!(has(&format!("{}/CLAUDE.local.md", home.display())));
    assert!(has(&format!("{}/AGENTS.md", home.display())));
    // Файлы ниже корня — файлы репозитория, их нет.
    assert!(!excludes.iter().any(|e| e.contains("sub/")));
}

#[test]
fn exclude_paths_escape_glob_metacharacters() {
    let td = TempDir::new().unwrap();
    let home = td.path().join("home");
    let weird = home.join("we*ird?name[x]");
    let repo = weird.join("repo");
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let excludes = owner_instruction_excludes(&repo.join("sub"), &home);
    let escaped = "we[*]ird[?]name[[]x[]]/CLAUDE.md";
    assert!(
        excludes
            .iter()
            .any(|e| e.contains(escaped) && e.ends_with("CLAUDE.md")),
        "escaped ancestor missing: {excludes:?}"
    );
    assert!(
        !excludes.iter().any(|e| e.contains("we*ird")),
        "raw metacharacters leaked: {excludes:?}"
    );
}

#[test]
fn clean_preset_args_exclude_owner_files_and_auto_memory() {
    let td = fake_tree();
    let home = td.path().join("home");
    let repo = home.join("projects").join("repo");
    let workdir = repo.join("sub");
    let exec_dir = td.path().join("exec");
    std::fs::create_dir_all(&exec_dir).unwrap();
    // build_args берёт $HOME из окружения воркера: тот же дом для ожидания.
    let home_env = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let instructions = clean_spec();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &instructions,
    };
    let args = build_args(&spec, &ClaudeCodeConfig::default()).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    // Точный JSON: вычисленный список плюс отказ от автопамяти.
    let expected = settings_json(
        InstructionMode::Clean,
        &owner_instruction_excludes(&workdir, &home_env),
    );
    let mut full = expected.clone();
    full["crossSessionInbound"] = json!("refuse");
    assert_eq!(s[idx + 1], full.to_string());
    let parsed: Value = serde_json::from_str(&s[idx + 1]).unwrap();
    assert_eq!(parsed["autoMemoryEnabled"], json!(false));
    assert!(parsed["claudeMdExcludes"].as_array().unwrap().len() > 5);
    // Файла с текстом нет, моментальный снимок выключен сразу после --settings.
    assert!(!s.contains(&"--append-system-prompt-file".to_string()));
    assert_eq!(s[idx + 2], "--system-prompt-snapshot");
    assert_eq!(s[idx + 3], "off");
}

#[test]
fn needs_restart_detects_instruction_changes() {
    let effective = clean_spec();
    assert!(!needs_restart(&effective, &SettingsChange::default()));
    let same = SettingsChange {
        instructions: Some(clean_spec()),
        ..Default::default()
    };
    assert!(!needs_restart(&effective, &same));
    let changed = SettingsChange {
        instructions: Some(preset_spec("текст")),
        ..Default::default()
    };
    assert!(needs_restart(&effective, &changed));
}

#[test]
fn settings_json_modes_match_the_plan() {
    let excludes = vec!["/x/CLAUDE.md".to_string()];
    assert_eq!(
        settings_json(InstructionMode::Owner, &excludes),
        json!({"crossSessionInbound": "refuse"})
    );
    let clean = settings_json(InstructionMode::Clean, &excludes);
    assert_eq!(clean["autoMemoryEnabled"], json!(false));
    assert_eq!(clean["claudeMdExcludes"], json!(["/x/CLAUDE.md"]));
    assert_eq!(
        settings_json(InstructionMode::Preset, &excludes),
        settings_json(InstructionMode::Clean, &excludes)
    );
}

#[test]
fn write_instructions_file_stores_preset_text_0600_and_removes_stale() {
    let td = TempDir::new().unwrap();
    let exec = td.path().join("exec");
    std::fs::create_dir_all(&exec).unwrap();
    let spec = preset_spec("секретный текст");
    let path = write_instructions_file(&exec, &spec).unwrap().unwrap();
    assert_eq!(path, exec.join("instructions.md"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "секретный текст");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Clean убирает устаревший файл.
    assert!(
        write_instructions_file(&exec, &clean_spec())
            .unwrap()
            .is_none()
    );
    assert!(!path.exists());
    // Owner тоже убирает.
    std::fs::write(&path, "x").unwrap();
    assert!(
        write_instructions_file(&exec, &owner_spec())
            .unwrap()
            .is_none()
    );
    assert!(!path.exists());
}

// --- Двойник CLI для проверки перезапуска: логи по pid, init, ответ, usage. ---

const RESTART_FAKE: &str = r#"#!/usr/bin/python3
import json, sys, os, pathlib
d = pathlib.Path(__file__).parent
pid = os.getpid()
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
if (d / 'fail-all').exists():
    sys.exit(1)
if (d / 'fail-resume-once').exists() and '--resume' in sys.argv and not (d / 'retried').exists():
    (d / 'retried').write_text('1')
    sys.exit(1)
if (d / 'fail-resume-always').exists() and '--resume' in sys.argv:
    sys.exit(1)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
sid = sys.argv[sys.argv.index('--resume') + 1] if '--resume' in sys.argv else sys.argv[sys.argv.index('--session-id') + 1]
usage = json.loads((d / 'usage.json').read_text()) if (d / 'usage.json').exists() else {}
def emit(v):
    print(json.dumps(v), flush=True)
for line in sys.stdin:
    (d / f'stdin-{pid}.log').open('a').write(line)
    v = json.loads(line)
    if v.get('type') == 'control_request':
        emit({'type': 'control_response', 'response': {'subtype': 'success', 'request_id': v['request_id'], 'response': {}}})
        continue
    emit({'type': 'system', 'subtype': 'init', 'session_id': sid, 'model': 'claude-haiku-test',
          'permissionMode': 'default',
          'mcp_servers': [{'name': 'canterlot', 'status': 'connected'}]})
    uid = v.get('uuid')
    emit({'type': 'result', 'subtype': 'success', 'session_id': sid, 'is_error': False,
          'user_message_uuids': [uid], 'modelUsage': usage, 'result': 'ok'})
"#;

struct RestartHarness {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    old_pid: u32,
}

impl RestartHarness {
    async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-cli");
        fake_exec::write_executable(&exe, RESTART_FAKE, 0o700);
        let workdir = dir.path().join("repo");
        std::fs::create_dir_all(workdir.join(".git")).unwrap();
        let context = AdapterContext {
            transmit_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: Some(ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            }),
            codex: None,
        };
        let mut adapter =
            canterlot_worker::adapter::registry_with("claude_code", &context).unwrap();
        let group = TreeContainment::new().unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: workdir.clone(),
            native_conversation_id: None,
            profile: "claude_code".into(),
            model: "haiku".into(),
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

    fn tx(&self, id: Uuid, settings: Option<SettingsChange>) -> Transmission {
        Transmission {
            command_id: id,
            attempt: 1,
            text: "test".into(),
            files: vec![],
            settings,
        }
    }

    async fn transmit(&mut self, settings: Option<SettingsChange>) -> TransmitOutcome {
        let id = Uuid::now_v7();
        tokio::time::timeout(
            WAIT,
            self.adapter.transmit(&self.tx(id, settings), &self.group),
        )
        .await
        .expect("transmit deadline")
    }

    /// События до первого RunFinished включительно.
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

    fn stdin_lines(&self, pid: u32) -> usize {
        let path = self.dir.path().join(format!("stdin-{pid}.log"));
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

#[tokio::test]
async fn instructions_change_restarts_cli_with_resume_before_the_input() {
    let mut h = RestartHarness::new().await;
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    h.until_finished().await;
    assert_eq!(h.spawns().len(), 1);

    // Смена инструкций: новый процесс, ввод только в него.
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)));
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "one restart expected: {spawns:?}");
    let (new_pid, new_args) = spawns
        .iter()
        .find(|(pid, _)| *pid != h.old_pid)
        .expect("new process");
    let sid = new_args
        .iter()
        .position(|a| a == "--resume")
        .expect("resume flag");
    assert!(new_args[sid + 1].len() > 8, "native id resumed");
    assert_eq!(h.stdin_lines(h.old_pid), 1, "old process got EOF");
    assert_eq!(h.stdin_lines(*new_pid), 1, "input reached the new process");

    // CliRestarted — первое событие после перезапуска, до ответа.
    let events = h.until_finished().await;
    match &events[0] {
        AdapterEvent::CliRestarted {
            cli_pid,
            cli_start_ticks,
        } => {
            assert_eq!(*cli_pid, *new_pid);
            assert_ne!(*cli_start_ticks, 0);
        }
        other => panic!("CliRestarted expected first, got {other:?}"),
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::RunFinished { .. }))
    );
}

#[tokio::test]
async fn restart_applies_model_effort_and_mode_by_flags() {
    let mut h = RestartHarness::new().await;
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    h.until_finished().await;

    let mut settings = SettingsChange {
        model: Some("sonnet".into()),
        effort: Some("high".into()),
        permission_mode: Some("bypass".into()),
        instructions: Some(clean_spec()),
    };
    let _ = &mut settings;
    let out = h.transmit(Some(settings)).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2);
    let args = &spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap().1;
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    assert_eq!(flag("--model").as_deref(), Some("sonnet"));
    assert_eq!(flag("--effort").as_deref(), Some("high"));
    assert_eq!(
        flag("--permission-mode").as_deref(),
        Some("bypassPermissions")
    );
}

#[tokio::test]
async fn failed_restart_respawns_previous_and_rejects_the_input() {
    let mut h = RestartHarness::new().await;
    std::fs::write(h.dir.path().join("fail-resume-once"), "").unwrap();
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    h.until_finished().await;

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    // Первый перезапуск упал (маркер fake), запасной со старыми настройками поднялся.
    assert!(
        h.dir.path().join("retried").exists(),
        "resume was attempted"
    );
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "fallback spawn expected: {spawns:?}");
    // Ввод не записан ни в упавший, ни в запасной процесс.
    for (pid, _) in spawns.iter().filter(|(pid, _)| *pid != h.old_pid) {
        assert_eq!(h.stdin_lines(*pid), 0, "input must not be sent");
    }
}

#[tokio::test]
async fn failed_fallback_restart_reports_process_exited() {
    let mut h = RestartHarness::new().await;
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    h.until_finished().await;
    std::fs::write(h.dir.path().join("fail-all"), "").unwrap();

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let event = tokio::time::timeout(WAIT, h.adapter.next_event())
        .await
        .expect("event deadline");
    assert!(
        matches!(event, AdapterEvent::ProcessExited { .. }),
        "fallback failure stops the execution: {event:?}"
    );
}

#[tokio::test]
async fn same_revision_does_not_restart() {
    let mut h = RestartHarness::new().await;
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    h.until_finished().await;

    let out = h.transmit(Some(change(owner_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)));
    assert_eq!(h.spawns().len(), 1, "no restart for same instructions");
    h.until_finished().await;
}

#[tokio::test]
async fn cli_ignoring_stdin_eof_is_terminated_then_killed() {
    // Двойник игнорирует EOF stdin и перехватывает SIGTERM: перезапуск идёт
    // через SIGTERM группе и SIGKILL. Первый запуск без --resume висит,
    // процесс после перезапуска отвечает как обычно.
    const STUBBORN: &str = r#"#!/usr/bin/python3
import json, sys, os, pathlib, signal, time
d = pathlib.Path(__file__).parent
pid = os.getpid()
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
if '--resume' not in sys.argv:
    signal.signal(signal.SIGTERM, lambda *a: (d / 'got-term').write_text('x'))
    while True:
        time.sleep(0.2)
sid = sys.argv[sys.argv.index('--resume') + 1]
def emit(v):
    print(json.dumps(v), flush=True)
for line in sys.stdin:
    (d / f'stdin-{pid}.log').open('a').write(line)
    v = json.loads(line)
    emit({'type': 'system', 'subtype': 'init', 'session_id': sid, 'model': 'claude-haiku-test',
          'permissionMode': 'default',
          'mcp_servers': [{'name': 'canterlot', 'status': 'connected'}]})
    uid = v.get('uuid')
    emit({'type': 'result', 'subtype': 'success', 'session_id': sid, 'is_error': False,
          'user_message_uuids': [uid], 'modelUsage': {}, 'result': 'ok'})
"#;
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-cli");
    fake_exec::write_executable(&exe, STUBBORN, 0o700);
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(workdir.join(".git")).unwrap();
    let context = AdapterContext {
        transmit_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: Some(ClaudeCodeConfig {
            executable: exe,
            ..Default::default()
        }),
        codex: None,
    };
    let mut adapter = canterlot_worker::adapter::registry_with("claude_code", &context).unwrap();
    let group = TreeContainment::new().unwrap();
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: workdir.clone(),
        native_conversation_id: None,
        profile: "claude_code".into(),
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: owner_spec(),
    };
    let started = tokio::time::timeout(WAIT, adapter.start(&spec, &group))
        .await
        .expect("start deadline")
        .expect("start ok");
    let old_pid = started.cli_pid;

    let id = Uuid::now_v7();
    let out = tokio::time::timeout(
        WAIT,
        adapter.transmit(
            &Transmission {
                command_id: id,
                attempt: 1,
                text: "test".into(),
                files: vec![],
                settings: Some(change(clean_spec())),
            },
            &group,
        ),
    )
    .await
    .expect("transmit deadline");
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    // SIGTERM дошёл (маркер), процесс всё равно пришлось убивать.
    assert!(dir.path().join("got-term").exists());
    let mut spawns = vec![];
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(pid) = name
            .strip_prefix("args-")
            .and_then(|s| s.strip_suffix(".json"))
            && let Ok(pid) = pid.parse::<u32>()
        {
            spawns.push(pid);
        }
    }
    assert_eq!(spawns.len(), 2);
    assert!(spawns.contains(&old_pid));
}

#[tokio::test]
async fn usage_baseline_resets_after_restart() {
    let mut h = RestartHarness::new().await;
    std::fs::write(
        h.dir.path().join("usage.json"),
        r#"{"claude-haiku-test":{"inputTokens":100,"outputTokens":10,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}"#,
    )
    .unwrap();
    let first = h.transmit(None).await;
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    let events = h.until_finished().await;
    let before = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::RunFinished { usage, .. } => usage.first().map(|u| u.input_tokens),
            _ => None,
        })
        .expect("usage before restart");
    assert_eq!(before, 100);

    // Новый процесс считает usage с нуля: 30, а не max(0, 30-100).
    std::fs::write(
        h.dir.path().join("usage.json"),
        r#"{"claude-haiku-test":{"inputTokens":30,"outputTokens":3,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}"#,
    )
    .unwrap();
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)));
    let events = h.until_finished().await;
    let after = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::RunFinished { usage, .. } => usage.first().map(|u| u.input_tokens),
            _ => None,
        })
        .expect("usage after restart");
    assert_eq!(after, 30);
}
