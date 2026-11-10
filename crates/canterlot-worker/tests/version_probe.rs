//! Проба `--version` на двойниках: кэш, повтор по таймауту и уборка процесса.

use canterlot_worker::adapter::{
    cli_version::{CliVersion, VersionError},
    opencode::version,
};
use std::time::Duration;

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn count(path: &std::path::Path) -> u32 {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.lines().count() as u32)
        .unwrap_or(0)
}

fn process_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !matches!(
            stat[stat.rfind(')').unwrap() + 1..]
                .trim_start()
                .chars()
                .next(),
            Some('Z' | 'X')
        )
    })
}

#[tokio::test]
async fn a_slow_first_probe_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("opencode");
    let runs = dir.path().join("runs");
    let marker = dir.path().join("slow-once");
    std::fs::write(&marker, "1").unwrap();
    fake_exec::write_executable(
        &exe,
        format!(
            "#!/bin/sh\necho run >> '{}'\nif [ -e '{}' ]; then rm '{}'; sleep 0.4; fi\necho 'opencode v2.1.4'\n",
            runs.display(),
            marker.display(),
            marker.display()
        ),
        0o700,
    );
    let started = std::time::Instant::now();
    let result = version::check_with_deadline(&exe, &[], Duration::from_millis(100)).await;
    assert_eq!(result, Ok(CliVersion::Verified("2.1.4".into())));
    assert!(
        started.elapsed() >= Duration::from_millis(90),
        "the first probe was expected to hit its deadline"
    );
    assert_eq!(count(&runs), 2, "the first probe timed out and was retried");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_hung_probe_times_out_after_bounded_retries_and_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("opencode");
    let pids = dir.path().join("pids");
    fake_exec::write_executable(
        &exe,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo $$ >> '{}'; exec sleep 300; fi\n",
            pids.display()
        ),
        0o700,
    );
    let started = std::time::Instant::now();
    let result = version::check_with_deadline(&exe, &[], Duration::from_millis(100)).await;
    assert_eq!(result, Err(VersionError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the retries are bounded: {:?}",
        started.elapsed()
    );
    let pids: Vec<u32> = std::fs::read_to_string(&pids)
        .unwrap()
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 3, "three attempts in total");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while pids.iter().any(|pid| process_alive(*pid)) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !pids.iter().any(|pid| process_alive(*pid)),
        "a timed-out probe was left behind: {pids:?}"
    );
}

#[tokio::test]
async fn a_failing_probe_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("opencode");
    let runs = dir.path().join("runs");
    fake_exec::write_executable(
        &exe,
        format!("#!/bin/sh\necho run >> '{}'\nexit 1\n", runs.display()),
        0o700,
    );
    let started = std::time::Instant::now();
    let result = version::check_with_deadline(&exe, &[], Duration::from_millis(100)).await;
    assert_eq!(result, Err(VersionError::Failed));
    assert_eq!(count(&runs), 1);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn an_unchanged_binary_is_not_probed_again() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("opencode");
    let runs = dir.path().join("runs");
    let script = format!(
        "#!/bin/sh\necho run >> '{}'\necho 'opencode v2.1.4'\n",
        runs.display()
    );
    fake_exec::write_executable(&exe, &script, 0o700);
    let cache = dir.path().join(".cli-versions.json");
    let first =
        version::check_with_cache(&exe, &[], Duration::from_millis(200), Some(cache.as_path()))
            .await;
    assert_eq!(first, Ok(CliVersion::Verified("2.1.4".into())));
    assert!(cache.exists(), "the shared cache file is written");
    assert_eq!(count(&runs), 1);
    // Второй старт того же неизменённого бинарника пробу не запускает.
    let second =
        version::check_with_cache(&exe, &[], Duration::from_millis(200), Some(cache.as_path()))
            .await;
    assert_eq!(second, first);
    assert_eq!(count(&runs), 1);
    // Бинарник изменился (новый inode и mtime) — проба идёт заново.
    fake_exec::write_executable(&exe, format!("{script}\n# changed\n"), 0o700);
    let third =
        version::check_with_cache(&exe, &[], Duration::from_millis(200), Some(cache.as_path()))
            .await;
    assert_eq!(third, first);
    assert_eq!(count(&runs), 2);
}
