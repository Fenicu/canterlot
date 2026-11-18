use canterlot_proto::worker::v1::{SchedulePrecheck, precheck_result};
use canterlot_worker::machine::precheck::{OUTPUT_TAIL_BYTES, run_with};
use std::{path::PathBuf, time::Duration};
use tokio::sync::oneshot;

fn request(dir: &std::path::Path, command: &str) -> SchedulePrecheck {
    SchedulePrecheck {
        attempt_id: uuid::Uuid::now_v7().to_string(),
        workdir: dir.to_string_lossy().into_owned(),
        command: command.into(),
        timeout_seconds: 30,
    }
}

#[tokio::test]
async fn precheck_inherits_only_the_allowed_environment() {
    let dir = tempfile::tempdir().unwrap();
    let command = "printf '%s\\n' \"$PATH\" \"$HOME\" \"$LANG\" \"$USER\"; env | cut -d= -f1 | sort | tr '\\n' ','";
    let result = run_with(
        request(dir.path(), command),
        vec![PathBuf::from(dir.path())],
        Duration::from_secs(10),
        None,
    )
    .await;
    let outcome = match result.outcome.unwrap() {
        precheck_result::Outcome::Finished(outcome) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output_tail);
    let names: Vec<&str> = outcome
        .output_tail
        .split(',')
        .filter(|s| !s.is_empty())
        .collect();
    for name in ["HOME", "LANG", "PATH", "USER"] {
        assert!(names.contains(&name), "missing {name}: {:?}", names);
    }
    assert!(
        !outcome.output_tail.contains("CANTERLOT"),
        "no inherited canterlot variables: {}",
        outcome.output_tail
    );
    assert!(!outcome.timed_out);
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let command = format!("echo $$ > {}; sleep 300", pid_file.display());
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        run_with(
            request(dir.path(), &command),
            vec![PathBuf::from(dir.path())],
            Duration::from_secs(300),
            Some(rx),
        )
        .await
    });
    // Ждём, пока оболочка запишет свой pid, Without any sleep: read loops on the file.
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        tokio::task::yield_now().await;
    };
    tx.send(()).unwrap();
    let result = task.await.unwrap();
    let outcome = match result.outcome.unwrap() {
        precheck_result::Outcome::Finished(outcome) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert!(outcome.timed_out);
    assert_eq!(outcome.exit_code, -1);
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "process group leader reaped"
    );
}

#[tokio::test]
async fn output_tail_is_capped_at_four_kilobytes() {
    let dir = tempfile::tempdir().unwrap();
    let command = "head -c 20000 /dev/zero | tr '\\0' 'a'; printf 'THE-END'";
    let result = run_with(
        request(dir.path(), command),
        vec![PathBuf::from(dir.path())],
        Duration::from_secs(10),
        None,
    )
    .await;
    let outcome = match result.outcome.unwrap() {
        precheck_result::Outcome::Finished(outcome) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert_eq!(outcome.exit_code, 0);
    assert!(outcome.output_tail.len() <= OUTPUT_TAIL_BYTES);
    assert!(
        outcome.output_tail.ends_with("THE-END"),
        "{}",
        &outcome.output_tail[outcome.output_tail.len().saturating_sub(40)..]
    );
}

#[tokio::test]
async fn timeout_does_not_kill_sibling_worker_subprocess() {
    // Соседний «живой CLI»: свой процесс-группа, ребёнок текущего процесса.
    let mut sibling = tokio::process::Command::new("sleep");
    sibling.arg("300");
    sibling.process_group(0);
    sibling.stdout(std::process::Stdio::null());
    sibling.stderr(std::process::Stdio::null());
    let mut sibling = sibling.spawn().expect("sibling spawns");
    let _sibling_pid = sibling.id().expect("sibling has pid");
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let command = format!("echo $$ > {}; sleep 300", pid_file.display());
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        run_with(
            request(dir.path(), &command),
            vec![PathBuf::from(dir.path())],
            Duration::from_secs(300),
            Some(rx),
        )
        .await
    });
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            break pid;
        }
        tokio::task::yield_now().await;
    };
    tx.send(()).unwrap();
    let result = task.await.unwrap();
    let outcome = match result.outcome.unwrap() {
        precheck_result::Outcome::Finished(outcome) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert!(outcome.timed_out);
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "precheck leader reaped"
    );
    // Сосед обязан пережить таймаут предпроверки (не сигнал, не зомби).
    match sibling.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!("sibling worker subprocess was killed: {status:?}"),
        Err(error) => panic!("sibling wait failed: {error:?}"),
    }
    let _ = sibling.kill().await;
    let _ = sibling.wait().await;
}

#[tokio::test]
async fn workdir_outside_allowed_roots_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_with(
        request(dir.path(), "true"),
        vec![PathBuf::from("/definitely/not/a/root")],
        Duration::from_secs(10),
        None,
    )
    .await;
    match result.outcome.unwrap() {
        precheck_result::Outcome::Error(error) => assert_eq!(error.reason, "workdir_not_allowed"),
        other => panic!("expected a rejected workdir: {other:?}"),
    }
}

#[tokio::test]
async fn precheck_runs_in_the_pinned_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let result = run_with(
        request(dir.path(), "pwd"),
        vec![PathBuf::from(dir.path())],
        Duration::from_secs(10),
        None,
    )
    .await;
    let outcome = match result.outcome.unwrap() {
        precheck_result::Outcome::Finished(outcome) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output_tail);
    assert!(
        outcome.output_tail.trim_end() == canonical.to_string_lossy(),
        "cwd is the validated dir, got: {}",
        outcome.output_tail
    );
}
