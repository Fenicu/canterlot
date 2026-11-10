//! Отдельный бинарник: обход `TreeContainment` забирает зомби всех детей процесса,
//! поэтому рядом с другими тестами он обрывал бы их ожидание.
#![forbid(unsafe_code)]

use canterlot_worker::adapter::codex::version;
use canterlot_worker::containment::{Containment, TreeContainment};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

use crate::fake_exec;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_check_survives_a_scan_that_takes_its_zombie() {
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-codex");
    // Фоновый sleep держит stdout: двойник успевает стать зомби до `wait`.
    fake_exec::write_executable(
        &exe,
        "#!/bin/sh\necho 'codex-cli 0.164.0'\nsleep 0.1 &\nexit 0\n",
        0o700,
    );
    let group = Arc::new(TreeContainment::new().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let scanner = {
        let (group, stop) = (group.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                group.members().unwrap();
            }
        })
    };
    let mut failed = 0;
    for _ in 0..5 {
        if version::check(&exe).await.is_err() {
            failed += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    scanner.join().unwrap();
    assert_eq!(
        failed, 0,
        "the version was printed, only its exit was taken"
    );
}
