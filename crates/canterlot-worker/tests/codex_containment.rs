//! Отдельный бинарник: `TreeContainment::terminate` завершает всех потомков тестового процесса,
//! поэтому рядом с другими тестами он обрывал бы их дочерние процессы.
use canterlot_worker::adapter::codex::process::{AppServerProcess, LaunchSpec};
use canterlot_worker::containment::Containment;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

#[path = "support/fake_exec.rs"]
mod fake_exec;

#[tokio::test]
async fn close_kills_detached_grandchild() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    let pidfile = td.path().join("grandchild.pid");
    let cmd = format!(
        "#!/bin/sh\nsetsid sleep 600 &\necho $! > {}\nsleep 60\n",
        pidfile.display()
    );
    fake_exec::write_executable(&script, cmd, 0o755);
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let relay = PathBuf::from("/usr/bin/canterlot-worker");
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        relay_exe: &relay,
        relay_secret: None,
    };
    let proc = AppServerProcess::spawn(
        &spec,
        &group,
        canterlot_worker::adapter::InstructionMode::Owner,
        None,
    )
    .unwrap();
    // wait for grandchild pid file
    for _ in 0..50 {
        if pidfile.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let gpid: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    group.terminate(Duration::from_secs(2)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!std::path::Path::new(&format!("/proc/{gpid}")).exists());
    let _ = proc;
}
