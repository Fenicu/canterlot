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

/// Этот процесс усыновляет осиротевших потомков `bash`: без снятия статуса
/// они остаются зомби, и тест «убит» не отличил бы их от настоящей уборки.
fn adopt_orphans() {
    rustix::process::set_child_subreaper(rustix::process::Pid::from_raw(1)).unwrap();
}

/// Статус снят: записи в `/proc` больше нет (зомби её ещё держит).
fn reaped(pid: u32) -> bool {
    !std::path::Path::new(&format!("/proc/{pid}")).exists()
}

async fn wait_for_gone(pid: u32, what: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reaped(pid) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect(what);
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
    // Единственная своя переменная — метка области уборки этой проверки.
    assert!(
        !outcome
            .output_tail
            .replace("CANTERLOT_PRECHECK_GROUP", "")
            .contains("CANTERLOT"),
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
async fn background_descendant_is_killed_on_normal_exit() {
    adopt_orphans();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("bg.pid");
    let command = format!("sleep 30 & echo $! > {}", pid_file.display());
    let result = run_with(
        request(dir.path(), &command),
        vec![PathBuf::from(dir.path())],
        Duration::from_secs(10),
        None,
    )
    .await;
    let outcome = match result.outcome {
        Some(precheck_result::Outcome::Finished(outcome)) => outcome,
        other => panic!("expected a finished precheck: {other:?}"),
    };
    assert_eq!(outcome.exit_code, 0, "{}", outcome.output_tail);
    assert!(!outcome.timed_out);
    let pid: u32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background pid appears");
    wait_for_gone(
        pid,
        "background descendant is reaped (not a zombie) after normal exit",
    )
    .await;
}

#[tokio::test]
async fn background_descendant_is_killed_on_cancel() {
    adopt_orphans();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("bg-cancel.pid");
    let command = format!("sleep 30 & echo $! > {}", pid_file.display());
    let task = tokio::spawn(async move {
        run_with(
            request(dir.path(), &command),
            vec![PathBuf::from(dir.path())],
            Duration::from_secs(30),
            None,
        )
        .await
    });
    let pid: u32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background pid appears");
    task.abort();
    let _ = task.await;
    let gone = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reaped(pid) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    if !gone {
        let _ = tokio::process::Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .output()
            .await;
    }
    assert!(gone, "background descendant is reaped after cancel");
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

/// Дренажи остаются под охраной набора до конца итогового ожидания: отмена
/// посреди него закрывает трубы, а не оставляет задачи жить в фоне.
#[tokio::test]
async fn cancel_during_the_final_drain_join_aborts_the_drains() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("keeper.pid");
    let leader_file = dir.path().join("leader.pid");
    // Хранитель труб уходит в свою сессию: группа предпроверки его не убьёт,
    // и дренаж не дождётся EOF, пока итоговое ожидание не истечёт.
    let command = format!(
        "echo $$ > {}; setsid sleep 30 & echo $! > {}",
        leader_file.display(),
        pid_file.display()
    );
    let pipes_before = pipe_fds();
    let task = tokio::spawn({
        let dir = dir.path().to_path_buf();
        async move {
            run_with(
                request(&dir, &command),
                vec![dir.clone()],
                Duration::from_secs(30),
                None,
            )
            .await
        }
    });
    let keeper = read_pid(&pid_file).await;
    let leader = read_pid(&leader_file).await;
    // Лидер ушёл: предпроверка в итоговом ожидании дренажей.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !reaped(leader) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("leader exits");
    task.abort();
    let _ = task.await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while pipe_fds() > pipes_before {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let _ = tokio::process::Command::new("kill")
        .arg("-KILL")
        .arg(keeper.to_string())
        .output()
        .await;
    assert!(closed, "drain tasks are aborted with the cancelled future");
}

async fn read_pid(path: &std::path::Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pid file appears")
}

fn pipe_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    std::fs::read_link(entry.path())
                        .is_ok_and(|target| target.to_string_lossy().starts_with("pipe:"))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Процесс жив: записи нет или он зомби — значит, ушёл.
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text
            .rfind(')')
            .and_then(|end| text[end + 1..].split_whitespace().next().map(str::to_owned))
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
    }
}

/// Помощник в своей сессии (`setsid`) не переживает проверку. Тест ничего
/// не убивает сам: уборка идёт только из боевой точки входа `run_with`.
/// `sleep 40` истекает сам, если уборка сломана, и не засоряет машину.
async fn setsid_helper_is_contained(how: &'static str) {
    adopt_orphans();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("helper.pid");
    // Лидер выходит сразу или висит: помощник живёт отдельно от группы лидера.
    let tail = if how == "exit" { "exit 0" } else { "sleep 300" };
    let command = format!(
        "setsid sleep 40 >/dev/null 2>&1 & echo $! > {}; {tail}",
        pid_file.display()
    );
    let (tx, rx) = oneshot::channel::<()>();
    let root = dir.path().to_path_buf();
    let task = tokio::spawn(async move {
        run_with(
            request(&root, &command),
            vec![root.clone()],
            Duration::from_secs(30),
            if how == "timeout" { Some(rx) } else { None },
        )
        .await
    });
    let helper = read_pid(&pid_file).await;
    assert!(alive(helper), "the helper runs before the check ends");
    match how {
        "timeout" => {
            let _ = tx.send(());
            let _ = task.await;
        }
        "cancel" => {
            task.abort();
            let _ = task.await;
        }
        _ => {
            let _ = task.await;
        }
    }
    let gone = tokio::time::timeout(Duration::from_secs(5), async {
        while alive(helper) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    assert!(gone, "a setsid helper does not survive the check ({how})");
}

#[tokio::test]
async fn setsid_helper_does_not_survive_a_finished_check() {
    setsid_helper_is_contained("exit").await;
}

#[tokio::test]
async fn setsid_helper_does_not_survive_a_timeout() {
    setsid_helper_is_contained("timeout").await;
}

#[tokio::test]
async fn setsid_helper_does_not_survive_a_cancel() {
    setsid_helper_is_contained("cancel").await;
}
