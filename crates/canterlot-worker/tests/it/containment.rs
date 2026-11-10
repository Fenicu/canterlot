#![forbid(unsafe_code)]

use canterlot_worker::containment::{Containment, Member, Termination, TreeContainment};
use rustix::process::Signal;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "support/process_guard.rs"]
mod process_guard;

fn ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap()
}

fn isolated(case: &str) {
    let mut unrelated = std::process::Command::new("sh")
        .args(["-c", "read -r line"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "containment::containment_child", "--nocapture"])
        .env("CANTERLOT_CONTAINMENT_CASE", case)
        .env("CANTERLOT_UNRELATED_PID", unrelated.id().to_string())
        .output()
        .unwrap();
    let alive = unrelated.try_wait().unwrap().is_none();
    drop(unrelated.stdin.take());
    unrelated.wait().unwrap();
    assert!(alive, "unrelated process was signalled");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

macro_rules! cases {
    ($($name:ident),* $(,)?) => {$ (
        #[test] fn $name() { isolated(stringify!($name)); }
    )*};
}
cases!(
    escaped_grandchild_is_tracked_and_terminated,
    unrelated_process_is_never_a_member_or_signalled,
    pid_reuse_guard_checks_start_ticks,
    terminate_reports_survivors_when_deadline_expires,
    terminate_sends_one_sigterm_to_a_member,
    orphan_zombie_is_reaped_but_the_cli_keeps_its_exit_status,
    zombies_are_not_members
);

#[tokio::test]
async fn containment_child() {
    let Ok(case) = std::env::var("CANTERLOT_CONTAINMENT_CASE") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let _guard = process_guard::ProcessGuard::new(directory.path());
    let group = TreeContainment::new().unwrap();
    assert_eq!(group.kind(), "tree");
    let unrelated: u32 = std::env::var("CANTERLOT_UNRELATED_PID")
        .unwrap()
        .parse()
        .unwrap();
    assert!(!group.members().unwrap().iter().any(|m| m.pid == unrelated));
    if case == "orphan_zombie_is_reaped_but_the_cli_keeps_its_exit_status" {
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "read -r line; exit 7"])
            .stdin(Stdio::piped());
        group.prepare_command(&mut command);
        let mut cli = command.spawn().unwrap();
        let pid = cli.id().unwrap();
        group.register_cli(&Member {
            pid,
            start_ticks: ticks(pid),
        });
        // Лидер группы выходит сразу, и его фоновый потомок усыновляется тестовым процессом.
        let mut detached = tokio::process::Command::new("sh");
        detached
            .args(["-c", "sleep 0.05 >/dev/null 2>&1 & echo $!"])
            .stdout(Stdio::piped());
        group.prepare_command(&mut detached);
        let output = detached.spawn().unwrap().wait_with_output().await.unwrap();
        let orphan: u32 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap();
        let mut stdin = cli.stdin.take().unwrap();
        stdin.write_all(b"exit\n").await.unwrap();
        stdin.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let stat = std::fs::read_to_string(format!("/proc/{orphan}/stat")).unwrap();
        assert!(
            stat.contains(") Z "),
            "orphan must be a zombie by now: {stat}"
        );
        group.reap_orphans().unwrap();
        assert!(
            !std::path::Path::new(&format!("/proc/{orphan}")).exists(),
            "orphan zombie stays until the execution ends"
        );
        assert_eq!(cli.wait().await.unwrap().code(), Some(7));
    } else if case == "terminate_sends_one_sigterm_to_a_member" {
        let script = "import signal, sys, time\n\
            log = open(sys.argv[1], 'a', buffering=1)\n\
            signal.signal(signal.SIGTERM, lambda *_: log.write('term\\n'))\n\
            log.write('ready\\n')\n\
            while True: time.sleep(0.01)\n";
        let log = directory.path().join("signals");
        let mut command = tokio::process::Command::new("/usr/bin/python3");
        command.args(["-c", script]).arg(&log);
        group.prepare_command(&mut command);
        let mut child = command.spawn().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !std::fs::read_to_string(&log).is_ok_and(|text| text.contains("ready")) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        group.terminate(Duration::from_millis(300)).await.unwrap();
        child.wait().await.unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().filter(|line| *line == "term").count(), 1);
    } else if case == "escaped_grandchild_is_tracked_and_terminated" {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"));
        command
            .current_dir(directory.path())
            .args(["synthetic-cli", "--profile", "synthetic"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        group.prepare_command(&mut command);
        let mut cli = command.spawn().unwrap();
        let pid = cli.id().unwrap();
        assert!(group.members().unwrap().iter().any(|m| m.pid == pid));
        let mut lines = BufReader::new(cli.stdout.take().unwrap()).lines();
        lines.next_line().await.unwrap().unwrap();
        let frame = serde_json::json!({"type":"input", "command_id":uuid::Uuid::now_v7(), "attempt":1,
            "text":r#"{"synthetic":[{"detach_child":true},{"exit":0}]}"#});
        let mut stdin = cli.stdin.take().unwrap();
        stdin
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), cli.wait())
            .await
            .unwrap()
            .unwrap();
        let members = group.members().unwrap();
        assert!(!members.is_empty());
        assert!(members.iter().all(|m| m.pid != pid));
        assert!(members.iter().any(|m| {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", m.pid)).unwrap();
            let fields: Vec<_> = stat[stat.rfind(')').unwrap() + 2..]
                .split_whitespace()
                .collect();
            fields[3] != pid.to_string()
                && fields[2] != pid.to_string()
                && fields[1] == std::process::id().to_string()
        }));
        assert!(matches!(
            group.terminate(Duration::from_secs(1)).await.unwrap(),
            Termination::Empty
        ));
        for member in members {
            assert!(!std::path::Path::new(&format!("/proc/{}", member.pid)).exists());
        }
    } else {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"));
        command
            .args(["synthetic-cli", "--profile", "synthetic"])
            .current_dir(directory.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        group.prepare_command(&mut command);
        let mut child = command.spawn().unwrap();
        if case == "zombies_are_not_members" {
            let frame = serde_json::json!({"type":"input", "command_id":uuid::Uuid::now_v7(), "attempt":1,
                "text":r#"{"synthetic":[{"exit":0}]}"#});
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
        }
        let pid = child.id().unwrap();
        if case == "zombies_are_not_members" {
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!group.members().unwrap().iter().any(|m| m.pid == pid));
            assert_eq!(child.wait().await.unwrap().code(), Some(0));
        } else {
            let member = group
                .members()
                .unwrap()
                .into_iter()
                .find(|m| m.pid == pid)
                .unwrap();
            if case == "pid_reuse_guard_checks_start_ticks" {
                assert!(
                    !group
                        .signal_member(
                            &Member {
                                start_ticks: member.start_ticks + 1,
                                ..member.clone()
                            },
                            Signal::KILL
                        )
                        .unwrap()
                );
                assert!(child.try_wait().unwrap().is_none());
                assert!(
                    !group
                        .signal_member(
                            &Member {
                                pid: unrelated,
                                start_ticks: ticks(unrelated)
                            },
                            Signal::KILL
                        )
                        .unwrap()
                );
            }
            if case == "terminate_reports_survivors_when_deadline_expires" {
                assert!(group.signal_member(&member, Signal::STOP).unwrap());
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert!(
                    matches!(group.terminate(Duration::ZERO).await.unwrap(), Termination::Survivors(m) if m.contains(&member))
                );
                let _ = group.signal_member(&member, Signal::CONT).unwrap();
                let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
                    .await
                    .unwrap()
                    .unwrap();
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(9));
            } else {
                assert!(matches!(
                    group.terminate(Duration::from_millis(100)).await.unwrap(),
                    Termination::Empty
                ));
                assert!(child.wait().await.is_ok());
            }
        }
    }
    assert!(matches!(
        group.terminate(Duration::from_secs(1)).await.unwrap(),
        Termination::Empty
    ));
    assert!(!group.populated().unwrap());
    assert!(!group.members().unwrap().iter().any(|m| m.pid == unrelated));
}
