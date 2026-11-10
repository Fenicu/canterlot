//! Отдельный бинарник: `TreeContainment::terminate` завершает всех потомков тестового процесса,
//! поэтому рядом с другими тестами он обрывал бы их дочерние процессы.
use canterlot_worker::adapter::claude_code::process::{CliProcess, LaunchSpec, SessionArg};
use canterlot_worker::adapter::{InstructionMode, InstructionSpec};
use canterlot_worker::config::ClaudeCodeConfig;
use canterlot_worker::containment::Containment;
use tempfile::TempDir;

use crate::fake_exec;

#[tokio::test]
async fn close_kills_detached_grandchild() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    let pidfile = td.path().join("grandchild.pid");
    let cmd = format!(
        "#!/bin/sh
setsid sleep 600 &
echo $! > {}
sleep 60
",
        pidfile.display()
    );
    fake_exec::write_executable(&script, cmd, 0o755);
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let owner = InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    };
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
        isolate_mcp: false,
    };
    let proc = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    // wait for grandchild pid file
    for _ in 0..50 {
        if pidfile.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let gpid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    group
        .terminate(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!std::path::Path::new(&format!("/proc/{}", gpid)).exists());
    let _ = proc;
}
