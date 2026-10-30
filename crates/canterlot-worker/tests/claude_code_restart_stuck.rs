//! Перезапуск CLI, чей stdout держит процесс вне группы CLI. Отдельный тестовый
//! бинарь: контейнер гасит всех потомков процесса, соседние тесты задел бы.
//! Только локальный двойник; настоящий CLI и сеть не используются.
#![forbid(unsafe_code)]

use canterlot_worker::{
    adapter::{
        AdapterContext, AdapterEvent, InstructionMode, InstructionSpec, SettingsChange, StartSpec,
        Transmission, TransmitOutcome,
    },
    config::ClaudeCodeConfig,
    containment::TreeContainment,
};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

// Первый запуск игнорирует EOF и SIGTERM и оставляет потомка в своей сессии
// с унаследованным stdout: после SIGKILL группы CLI EOF не наступает.
const STUCK: &str = r#"#!/usr/bin/python3
import json, sys, os, pathlib, signal, time, subprocess
d = pathlib.Path(__file__).parent
pid = os.getpid()
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
(d / f'args-{pid}.json').write_text(json.dumps(sys.argv[1:]))
if not (d / 'restarted').exists():
    (d / 'restarted').write_text('1')
    holder = subprocess.Popen(['sleep', '300'], start_new_session=True)
    (d / 'holder').write_text(str(holder.pid))
    signal.signal(signal.SIGTERM, lambda *a: None)
    while True:
        time.sleep(0.2)
sid = sys.argv[sys.argv.index('--session-id') + 1]
def emit(v):
    print(json.dumps(v), flush=True)
for line in sys.stdin:
    v = json.loads(line)
    emit({'type': 'system', 'subtype': 'init', 'session_id': sid, 'model': 'claude-haiku-test',
          'permissionMode': 'default',
          'mcp_servers': [{'name': 'canterlot', 'status': 'connected'}]})
    emit({'type': 'result', 'subtype': 'success', 'session_id': sid, 'is_error': False,
          'user_message_uuids': [v.get('uuid')], 'modelUsage': {}, 'result': 'ok'})
"#;

fn instructions(mode: InstructionMode) -> InstructionSpec {
    InstructionSpec {
        mode,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rfind(')')
            .and_then(|end| stat[end + 1..].split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X")
    })
}

/// Держатель stdout живёт в своей сессии: при провале теста его никто не убьёт.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

#[tokio::test]
async fn restart_is_bounded_when_stdout_outlives_the_killed_cli() {
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-cli");
    fake_exec::write_executable(&exe, STUCK, 0o700);
    let workdir = dir.path().join("repo");
    std::fs::create_dir_all(workdir.join(".git")).unwrap();
    let context = AdapterContext {
        transmit_deadline: Duration::from_secs(20),
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
        workdir,
        native_conversation_id: None,
        profile: "claude_code".into(),
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: instructions(InstructionMode::Owner),
    };
    adapter.start(&spec, &group).await.expect("start ok");
    let holder: u32 = loop {
        if let Ok(text) = std::fs::read_to_string(dir.path().join("holder"))
            && let Ok(pid) = text.parse()
        {
            break pid;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let _holder = KillOnDrop(holder);
    let began = Instant::now();
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        adapter.transmit(
            &Transmission {
                command_id: Uuid::now_v7(),
                attempt: 1,
                text: "test".into(),
                files: vec![],
                settings: Some(SettingsChange {
                    instructions: Some(instructions(InstructionMode::Clean)),
                    ..Default::default()
                }),
            },
            &group,
        ),
    )
    .await
    .expect("restart must not wait for stdout EOF forever");
    assert!(matches!(out, TransmitOutcome::Accepted(_)), "{out:?}");
    assert!(began.elapsed() < Duration::from_secs(20));
    assert!(!running(holder), "the stdout holder is terminated");
    let event = tokio::time::timeout(Duration::from_secs(10), adapter.next_event())
        .await
        .unwrap();
    assert!(
        matches!(event, AdapterEvent::CliRestarted { .. }),
        "{event:?}"
    );
}
