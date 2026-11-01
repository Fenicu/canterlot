//! Пресеты инструкций Claude Code: чистый запуск без файлов владельца,
//! текст пресета через файл, перезапуск CLI между ходами.
//! Только локальные двойники и временные каталоги; настоящий CLI и сеть не используются.
#![forbid(unsafe_code)]

use canterlot_worker::{
    adapter::{
        Adapter, AdapterContext, AdapterEvent, BlockKind, InstructionMode, InstructionSpec,
        SettingsChange, StartSpec, Transmission, TransmitOutcome,
        claude_code::process::{
            LaunchSpec, SessionArg, build_args, needs_restart, owner_instruction_excludes,
            owner_instruction_excludes_in, repository_root, settings_json, write_instructions_file,
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
fn symlinked_workdir_excludes_the_physical_ancestor_chain_too() {
    // Живой прогон CLI 2.1.311: через /tmp/link → ~/x CLI грузит ~/CLAUDE.md,
    // идя от физического cwd, а исключения сверяет с физическими путями.
    let td = fake_tree();
    let real = std::fs::canonicalize(td.path()).unwrap();
    let home = real.join("home");
    let alias = real.join("alias");
    std::os::unix::fs::symlink(home.join("projects"), &alias).unwrap();
    let workdir = alias.join("repo").join("sub");
    let excludes = owner_instruction_excludes_in(&workdir, &home, &home.join(".claude"));
    let has = |p: &Path| excludes.iter().any(|e| *e == p.display().to_string());
    // Логическая цепочка: над корнем `alias/repo`.
    assert!(has(&alias.join("CLAUDE.md")));
    // Физическая цепочка: над `home/projects/repo` — файлы владельца.
    assert!(has(&home.join("projects").join("CLAUDE.md")));
    assert!(has(&home.join("CLAUDE.md")));
    assert!(has(&home.join(".claude/rules/**")));
    // Файлы репозитория — ни в каком виде.
    let repo = home.join("projects").join("repo").display().to_string();
    let logical_repo = alias.join("repo").display().to_string();
    assert!(
        !excludes
            .iter()
            .any(|e| e.starts_with(&repo) || e.starts_with(&logical_repo)),
        "{excludes:?}"
    );
}

#[test]
fn claude_config_dir_moves_the_user_level() {
    use canterlot_worker::config::claude_config_dir;
    let td = TempDir::new().unwrap();
    let home = td.path().join("home");
    let config = td.path().join("cfg");
    std::fs::create_dir_all(&config).unwrap();
    assert_eq!(claude_config_dir(&home, Some(config.as_os_str())), config);
    assert_eq!(claude_config_dir(&home, None), home.join(".claude"));
    assert_eq!(
        claude_config_dir(&home, Some(std::ffi::OsStr::new("relative"))),
        home.join(".claude")
    );
    let link = td.path().join("cfg-link");
    std::os::unix::fs::symlink(&config, &link).unwrap();
    let workdir = td.path().join("repo");
    std::fs::create_dir_all(workdir.join(".git")).unwrap();
    let excludes = owner_instruction_excludes_in(&workdir, &home, &link);
    let has = |p: String| excludes.contains(&p);
    assert!(has(format!("{}/CLAUDE.md", link.display())));
    assert!(has(format!("{}/rules/**", link.display())));
    let real = std::fs::canonicalize(&config).unwrap();
    assert!(has(format!("{}/CLAUDE.md", real.display())));
    assert!(has(format!("{}/.claude/CLAUDE.md", home.display())));
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
if (d / 'lock-exec-on-resume').exists() and '--resume' in sys.argv:
    os.chmod(pathlib.Path(sys.argv[sys.argv.index('--mcp-config') + 1]).parent, 0o500)
    sys.exit(1)
if (d / 'fail-session-once').exists() and '--session-id' in sys.argv:
    (d / 'fail-session-once').unlink()
    sys.exit(1)
sid = sys.argv[sys.argv.index('--resume') + 1] if '--resume' in sys.argv else sys.argv[sys.argv.index('--session-id') + 1]
# Как настоящий CLI: --resume требует записанной беседы, повторный --session-id отвергается.
transcript = d / f'transcript-{sid}'
if '--resume' in sys.argv and not transcript.exists():
    print(f'No conversation found with session ID: {sid}', file=sys.stderr)
    sys.exit(1)
if '--session-id' in sys.argv and transcript.exists():
    print(f'Session ID {sid} is already in use.', file=sys.stderr)
    sys.exit(1)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
def emit(v):
    print(json.dumps(v), flush=True)
for line in sys.stdin:
    (d / f'stdin-{pid}.log').open('a').write(line)
    v = json.loads(line)
    if v.get('type') != 'control_request':
        transcript.write_text('1')
    # usage.json читается на каждый ход: файл пишется тестом между запусками,
    # чтение при старте процесса гоняло бы со стартом интерпретатора.
    usage = json.loads((d / 'usage.json').read_text()) if (d / 'usage.json').exists() else {}
    if v.get('type') == 'control_request':
        if (d / 'refuse-control').exists():
            emit({'type': 'control_response', 'response': {'subtype': 'error', 'request_id': v['request_id'], 'error': 'refused'}})
        else:
            emit({'type': 'control_response', 'response': {'subtype': 'success', 'request_id': v['request_id'], 'response': {}}})
        continue
    emit({'type': 'system', 'subtype': 'init', 'session_id': sid, 'model': 'claude-haiku-test',
          'permissionMode': 'default',
          'mcp_servers': [{'name': 'canterlot', 'status': 'connected'}]})
    uid = v.get('uuid')
    frames = d / 'frames.json'
    if frames.exists():
        for frame in json.loads(frames.read_text()):
            emit(frame)
        frames.unlink()
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
        Self::launched(None, false).await
    }

    async fn with_native(native: Option<String>) -> Self {
        Self::launched(native, true).await
    }

    async fn launched(native: Option<String>, transcript: bool) -> Self {
        let dir = TempDir::new().unwrap();
        if let (Some(native), true) = (&native, transcript) {
            std::fs::write(dir.path().join(format!("transcript-{native}")), "1").unwrap();
        }
        let exe = dir.path().join("fake-cli");
        fake_exec::write_executable(&exe, RESTART_FAKE, 0o700);
        let workdir = dir.path().join("repo");
        std::fs::create_dir_all(workdir.join(".git")).unwrap();
        let context = AdapterContext {
            transmit_deadline: WAIT,
            ready_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: Some(ClaudeCodeConfig {
                executable: exe,
                ..Default::default()
            }),
            codex: None,
            opencode: None,
        };
        let mut adapter =
            canterlot_worker::adapter::registry_with("claude_code", &context).unwrap();
        let group = TreeContainment::new().unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: workdir.clone(),
            native_conversation_id: native,
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

fn background_frames(h: &RestartHarness, tasks: serde_json::Value) {
    let frames = serde_json::json!([
        {"type": "system", "subtype": "background_tasks_changed", "tasks": tasks}
    ]);
    std::fs::write(h.dir.path().join("frames.json"), frames.to_string()).unwrap();
}

fn running_task() -> serde_json::Value {
    serde_json::json!([{"task_id": "a1", "task_type": "local_agent", "description": "bg sub"}])
}

fn warnings(events: &[AdapterEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AdapterEvent::BlockStarted {
                    kind: BlockKind::Warning,
                    ..
                }
            )
        })
        .count()
}

// Смена инструкций с работающей фоновой задачей: сообщение идёт сразу со старыми, CLI живёт.
async fn deferred_change(h: &mut RestartHarness) {
    background_frames(h, running_task());
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    assert_eq!(h.spawns().len(), 1, "background work keeps the process");
    assert_eq!(
        h.stdin_lines(h.old_pid),
        2,
        "the message reached the same CLI"
    );
    let events = h.until_finished().await;
    assert_eq!(warnings(&events), 1, "the owner sees the wait");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::CliRestarted { .. }))
    );
}

#[tokio::test]
async fn instructions_change_waits_for_background_work_and_applies_when_the_list_empties() {
    let mut h = RestartHarness::new().await;
    deferred_change(&mut h).await;
    background_frames(&h, serde_json::json!([]));
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(h.spawns().len(), 1, "the list emptied inside this turn");
    h.until_finished().await;
    loop {
        let event = tokio::time::timeout(WAIT, h.adapter.next_event())
            .await
            .expect("the deferred restart must fall due");
        if matches!(event, AdapterEvent::DeferredRestartDue) {
            break;
        }
    }
    tokio::time::timeout(WAIT, h.adapter.apply_deferred(&h.group))
        .await
        .expect("restart deadline");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "one restart expected: {spawns:?}");
    let (new_pid, args) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    assert!(args.iter().any(|a| a == "--resume"));
    assert!(matches!(
        tokio::time::timeout(WAIT, h.adapter.next_event())
            .await
            .unwrap(),
        AdapterEvent::CliRestarted { .. }
    ));
    let new_pid = *new_pid;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(
        h.stdin_lines(new_pid),
        1,
        "the next input reaches the new CLI"
    );
    assert_eq!(h.spawns().len(), 2, "applied once");
}

#[tokio::test]
async fn deferred_change_goes_ahead_of_the_next_input_when_the_work_has_ended() {
    let mut h = RestartHarness::new().await;
    deferred_change(&mut h).await;
    background_frames(&h, serde_json::json!([]));
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(
        h.spawns().len(),
        2,
        "restart before the input, none left over"
    );
}

#[tokio::test]
async fn later_settings_change_replaces_the_deferred_one() {
    let mut h = RestartHarness::new().await;
    deferred_change(&mut h).await;
    let out = h.transmit(Some(change(preset_spec("новый текст")))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)));
    assert_eq!(h.spawns().len(), 1);
    let events = h.until_finished().await;
    assert_eq!(warnings(&events), 0, "said once");
    background_frames(&h, serde_json::json!([]));
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    loop {
        let event = tokio::time::timeout(WAIT, h.adapter.next_event())
            .await
            .unwrap();
        if matches!(event, AdapterEvent::DeferredRestartDue) {
            break;
        }
    }
    tokio::time::timeout(WAIT, h.adapter.apply_deferred(&h.group))
        .await
        .unwrap();
    assert_eq!(h.spawns().len(), 2);
    assert_eq!(
        std::fs::read_to_string(h.dir.path().join("exec/instructions.md")).unwrap(),
        "новый текст",
        "the latest change wins"
    );
}

fn stdin_values(h: &RestartHarness, pid: u32) -> Vec<serde_json::Value> {
    std::fs::read_to_string(h.dir.path().join(format!("stdin-{pid}.log")))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn mode_and_preset(mode: &str) -> SettingsChange {
    SettingsChange {
        permission_mode: Some(mode.into()),
        ..change(clean_spec())
    }
}

// Фон закончился: ход с пустым списком и выдержка, после которой перезапуск уже возможен.
async fn end_background_work(h: &mut RestartHarness) {
    background_frames(h, serde_json::json!([]));
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    tokio::time::sleep(Duration::from_millis(2_200)).await;
}

#[tokio::test]
async fn narrowed_mode_applies_now_and_only_the_preset_restart_waits_for_background_work() {
    let mut h = RestartHarness::new().await;
    background_frames(&h, running_task());
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    let out = h.transmit(Some(mode_and_preset("read_only"))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    assert_eq!(h.spawns().len(), 1, "background work keeps the process");
    let lines = stdin_values(&h, h.old_pid);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[1]["type"], "control_request");
    assert_eq!(lines[1]["request"]["subtype"], "set_permission_mode");
    assert_eq!(lines[1]["request"]["mode"], "plan");
    assert_eq!(lines[2]["type"], "user", "the mode goes before the message");
    let events = h.until_finished().await;
    assert_eq!(warnings(&events), 1, "the preset still waits");

    end_background_work(&mut h).await;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "the restart went ahead: {spawns:?}");
    let (_, args) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    let flag = args.iter().position(|a| a == "--permission-mode").unwrap();
    assert_eq!(args[flag + 1], "plan", "the restart keeps the narrow mode");
}

#[tokio::test]
async fn mode_the_cli_cannot_take_alone_refuses_the_input_and_defers_nothing() {
    let mut h = RestartHarness::new().await;
    background_frames(&h, running_task());
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    let out = h.transmit(Some(mode_and_preset("bypass"))).await;
    match out {
        TransmitOutcome::Rejected(reason) => assert_eq!(reason, "permission_mode_needs_restart"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    assert_eq!(h.stdin_lines(h.old_pid), 1, "nothing reached the CLI");

    end_background_work(&mut h).await;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(h.spawns().len(), 1, "a refused change restarts nothing");
}

#[tokio::test]
async fn mode_refused_by_the_cli_is_not_deferred_with_the_preset() {
    let mut h = RestartHarness::new().await;
    background_frames(&h, running_task());
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    h.until_finished().await;
    std::fs::write(h.dir.path().join("refuse-control"), "1").unwrap();
    let out = h.transmit(Some(mode_and_preset("read_only"))).await;
    match out {
        TransmitOutcome::Rejected(reason) => assert_eq!(reason, "permission_mode_change_failed"),
        other => panic!("unexpected outcome: {other:?}"),
    }
    std::fs::remove_file(h.dir.path().join("refuse-control")).unwrap();

    end_background_work(&mut h).await;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(h.spawns().len(), 1, "a refused change restarts nothing");
}

#[tokio::test]
async fn returning_to_the_running_instructions_cancels_the_deferred_restart() {
    let mut h = RestartHarness::new().await;
    deferred_change(&mut h).await;
    let out = h.transmit(Some(change(owner_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;

    end_background_work(&mut h).await;
    assert!(matches!(
        h.transmit(None).await,
        TransmitOutcome::Accepted(_)
    ));
    assert_eq!(h.spawns().len(), 1, "the owner took the change back");
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
    // Беседа уже записана: запасной запуск продолжает её, повторный
    // --session-id настоящий CLI отвергает («Session ID … is already in use»).
    let (_, fallback) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    assert!(fallback.iter().any(|a| a == "--resume"), "{fallback:?}");
    assert!(
        !fallback.iter().any(|a| a == "--session-id"),
        "{fallback:?}"
    );
}

#[tokio::test]
async fn failed_fallback_restart_stops_with_restart_failed() {
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
        matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "restart_failed",
                required: true,
                ..
            }
        ),
        "fallback failure stops the execution as restart_failed, not a crash: {event:?}"
    );
}

#[tokio::test]
async fn unrestored_previous_instructions_fail_the_fallback() {
    let mut h = RestartHarness::new().await;
    let out = h.transmit(Some(change(preset_spec("PRESET-A")))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    h.until_finished().await;
    let exec_dir = h.dir.path().join("exec");
    assert!(exec_dir.join("instructions.md").exists());
    // Перезапуск падает и закрывает каталог исполнения на запись: прежний файл
    // инструкций не вернуть, запасной CLI с чужим текстом не запускается.
    std::fs::write(h.dir.path().join("lock-exec-on-resume"), "").unwrap();
    let out = h.transmit(Some(change(preset_spec("PRESET-B")))).await;
    std::fs::set_permissions(&exec_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let spawns = h.spawns();
    assert_eq!(
        spawns.len(),
        2,
        "no fallback CLI with the wrong preset: {spawns:?}"
    );
    let event = tokio::time::timeout(WAIT, h.adapter.next_event())
        .await
        .expect("event deadline");
    assert!(
        matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "restart_failed",
                required: true,
                ..
            }
        ),
        "{event:?}"
    );
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .map(|i| args[i + 1].clone())
}

#[tokio::test]
async fn instructions_change_before_the_first_turn_keeps_the_session_id() {
    let mut h = RestartHarness::new().await;
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "one restart expected: {spawns:?}");
    let (_, old_args) = spawns.iter().find(|(pid, _)| *pid == h.old_pid).unwrap();
    let (new_pid, new_args) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    assert!(!new_args.iter().any(|a| a == "--resume"), "{new_args:?}");
    assert_eq!(
        flag_value(new_args, "--session-id"),
        flag_value(old_args, "--session-id")
    );
    assert_eq!(h.stdin_lines(*new_pid), 1, "input reached the new process");
    h.until_finished().await;

    // Беседа записана: дальше перезапуск уже продолжает её.
    let out = h.transmit(Some(change(preset_spec("PRESET")))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 3, "second restart expected: {spawns:?}");
    assert!(spawns[2].1.iter().any(|a| a == "--resume"), "{spawns:?}");
}

#[tokio::test]
async fn failed_change_before_the_first_turn_falls_back_to_the_same_session_id() {
    let mut h = RestartHarness::new().await;
    // Метка должна достаться перезапуску, а не ещё стартующему первому процессу.
    let started = |path: &std::path::Path| {
        std::fs::read_dir(path).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("args-")
        })
    };
    while !started(h.dir.path()) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(h.dir.path().join("fail-session-once"), "").unwrap();

    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(
        matches!(&out, TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"),
        "{out:?}"
    );
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "fallback spawn expected: {spawns:?}");
    let (_, fallback) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    assert!(fallback.iter().any(|a| a == "--session-id"), "{fallback:?}");

    // Беседа жива: следующий ввод проходит в запасной процесс.
    let out = h.transmit(None).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
}

#[tokio::test]
async fn resumed_session_stays_resumed_on_a_change_before_its_first_input() {
    let native = Uuid::now_v7().to_string();
    let mut h = RestartHarness::with_native(Some(native.clone())).await;
    let out = h.transmit(Some(change(clean_spec()))).await;
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    let spawns = h.spawns();
    assert_eq!(spawns.len(), 2, "one restart expected: {spawns:?}");
    let (_, new_args) = spawns.iter().find(|(pid, _)| *pid != h.old_pid).unwrap();
    assert_eq!(flag_value(new_args, "--resume"), Some(native));
}

#[tokio::test]
async fn missing_transcript_is_named_before_the_process_exit() {
    let native = Uuid::now_v7().to_string();
    let mut h = RestartHarness::launched(Some(native), false).await;
    let event = tokio::time::timeout(WAIT, h.adapter.next_event())
        .await
        .expect("event deadline");
    assert!(
        matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "resume_transcript_missing",
                required: false,
                ..
            }
        ),
        "{event:?}"
    );
    let event = tokio::time::timeout(WAIT, h.adapter.next_event())
        .await
        .expect("event deadline");
    assert!(
        matches!(event, AdapterEvent::ProcessExited { code: Some(1), .. }),
        "{event:?}"
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
    // через SIGTERM группе и SIGKILL. Первый запуск висит, процесс после
    // перезапуска отвечает как обычно.
    const STUBBORN: &str = r#"#!/usr/bin/python3
import json, sys, os, pathlib, signal, time
d = pathlib.Path(__file__).parent
pid = os.getpid()
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
if not (d / 'restarted').exists():
    (d / 'restarted').write_text('1')
    signal.signal(signal.SIGTERM, lambda *a: (d / 'got-term').write_text('x'))
    while True:
        time.sleep(0.2)
sid = sys.argv[sys.argv.index('--session-id') + 1]
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
        ready_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: Some(ClaudeCodeConfig {
            executable: exe,
            ..Default::default()
        }),
        codex: None,
        opencode: None,
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
async fn soft_close_reads_stdout_so_the_cli_can_finish_writing() {
    const NOISY: &str = r#"#!/usr/bin/python3
import signal, sys, pathlib
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
for line in sys.stdin:
    pass
sys.stdout.write('x' * (1 << 20) + '\n')
sys.stdout.flush()
(d / 'flushed').write_text('1')
"#;
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-cli");
    fake_exec::write_executable(&exe, NOISY, 0o700);
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(workdir.join(".git")).unwrap();
    let context = AdapterContext {
        transmit_deadline: WAIT,
        ready_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: Some(ClaudeCodeConfig {
            executable: exe,
            ..Default::default()
        }),
        codex: None,
        opencode: None,
    };
    let mut adapter = canterlot_worker::adapter::registry_with("claude_code", &context).unwrap();
    adapter.set_stop_grace(Duration::from_millis(200));
    let group = TreeContainment::new().unwrap();
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir,
        native_conversation_id: None,
        profile: "claude_code".into(),
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: owner_spec(),
    };
    tokio::time::timeout(WAIT, adapter.start(&spec, &group))
        .await
        .expect("start deadline")
        .expect("start ok");
    tokio::time::timeout(
        WAIT,
        adapter.close(canterlot_worker::adapter::CloseMode::Graceful, &group),
    )
    .await
    .expect("close deadline")
    .expect("close ok");
    assert!(
        dir.path().join("flushed").exists(),
        "the CLI was killed while blocked on a full stdout pipe"
    );
}

#[tokio::test]
async fn soft_close_ends_although_a_tool_keeps_printing_to_the_cli_stdout() {
    // Сам CLI выходит по EOF, а потомок в своей сессии держит его stdout и пишет без конца.
    const LEAKY: &str = r#"#!/usr/bin/python3
import subprocess, sys, pathlib
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
tool = subprocess.Popen(['sh', '-c', 'while :; do echo x; done'], start_new_session=True)
(d / 'tool').write_text(str(tool.pid))
for line in sys.stdin:
    pass
"#;
    struct Kill(u32);
    impl Drop for Kill {
        fn drop(&mut self) {
            if let Some(pid) = rustix::process::Pid::from_raw(self.0 as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-cli");
    fake_exec::write_executable(&exe, LEAKY, 0o700);
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(workdir.join(".git")).unwrap();
    let context = AdapterContext {
        transmit_deadline: WAIT,
        ready_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: Some(ClaudeCodeConfig {
            executable: exe,
            ..Default::default()
        }),
        codex: None,
        opencode: None,
    };
    let mut adapter = canterlot_worker::adapter::registry_with("claude_code", &context).unwrap();
    let group = TreeContainment::new().unwrap();
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir,
        native_conversation_id: None,
        profile: "claude_code".into(),
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: owner_spec(),
    };
    tokio::time::timeout(WAIT, adapter.start(&spec, &group))
        .await
        .expect("start deadline")
        .expect("start ok");
    let tool = loop {
        if let Some(pid) = std::fs::read_to_string(dir.path().join("tool"))
            .ok()
            .and_then(|text| text.parse().ok())
        {
            break Kill(pid);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::timeout(
        WAIT,
        adapter.close(canterlot_worker::adapter::CloseMode::Graceful, &group),
    )
    .await
    .expect("soft close must not wait for the end of stdout")
    .expect("close ok");
    drop(tool);
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
