#![forbid(unsafe_code)]

//! Журнал CodexBar: одно предупреждение на начало сбоя и одно сообщение о восстановлении
//! на провайдера, без повторов на каждой попытке. Отдельный тестовый бинарник, чтобы
//! подписчик `tracing` не делил точки вызова с другими тестами.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_worker::machine::{codexbar::CodexBarLimitsSource, limits::LimitsSource};

#[allow(dead_code)]
use crate::fake_exec;

const CLAUDE_WEEKLY_ONLY: &str = include_str!("fixtures/codexbar/claude_weekly_only.json");
const OPENCODEGO: &str = include_str!("fixtures/codexbar/opencodego.json");

#[derive(Clone)]
struct TestClock(Arc<Mutex<tokio::time::Instant>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(tokio::time::Instant::now())))
    }
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

fn executable(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("codexbar");
    fake_exec::write_executable(&path, format!("#!/bin/sh\n{body}"), 0o700);
    path
}

/// OpenCode падает, пока в `mode` нет `ok`; Claude читается всегда.
fn per_provider(dir: &Path) -> PathBuf {
    fs::write(dir.join("claude.json"), CLAUDE_WEEKLY_ONLY).unwrap();
    fs::write(dir.join("opencode.json"), OPENCODEGO).unwrap();
    executable(
        dir,
        &format!(
            r#"provider=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--provider" ]; then shift; provider="$1"; fi
  shift
done
case "$provider" in
  claude) cat '{claude}' ;;
  opencodego)
    if [ "$(cat '{mode}' 2>/dev/null)" = "ok" ]; then cat '{opencode}'; else exit 3; fi ;;
  *) exit 3 ;;
esac
"#,
            claude = dir.join("claude.json").display(),
            opencode = dir.join("opencode.json").display(),
            mode = dir.join("mode").display(),
        ),
    )
}

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn one_warning_per_failure_and_one_recovery_per_provider() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let time = clock.0.clone();
    let source = CodexBarLimitsSource::new(
        Some(per_provider(dir.path())),
        Duration::from_secs(900),
        None,
    )
    .with_clock(move || *time.lock().unwrap());

    let output = Output::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_target(false)
        .with_writer(move || writer.clone())
        .finish();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            assert!(
                source
                    .poll_report("opencode", true)
                    .await
                    .observations
                    .is_empty()
            );
            // Успех другого провайдера не считается восстановлением OpenCode.
            clock.advance(Duration::from_secs(15));
            assert_eq!(
                source.poll_report("claude", true).await.observations.len(),
                2
            );
            // Повторный сбой не пишет второе предупреждение.
            clock.advance(Duration::from_secs(1800));
            assert!(
                source
                    .poll_report("opencode", true)
                    .await
                    .observations
                    .is_empty()
            );
            fs::write(dir.path().join("mode"), "ok").unwrap();
            clock.advance(Duration::from_secs(3600));
            assert!(
                !source
                    .poll_report("opencode", true)
                    .await
                    .observations
                    .is_empty()
            );
        })
    });

    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert_eq!(
        log.matches("codexbar_poll_failed").count(),
        1,
        "one warning per failure episode: {log}"
    );
    assert_eq!(
        log.matches("codexbar_poll_recovered").count(),
        1,
        "one recovery line: {log}"
    );
}
