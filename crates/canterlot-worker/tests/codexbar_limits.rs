#![forbid(unsafe_code)]

//! Лимиты через CodexBar: разбор вывода по `windowMinutes`, интервалы запуска и запасной источник.
//! Настоящий `codexbar` не нужен: скрипт-заглушка печатает записанный вывод.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::worker::v1::{LimitObservation, LimitWindow};
use canterlot_worker::{
    config::MachineConfig,
    machine::{
        codexbar::{
            CodexBarError, CodexBarLimitsSource, observations_from_output, reading_from_output,
        },
        limits::LimitsSource,
    },
};
use tokio::time::Instant;

#[allow(dead_code)]
#[path = "support/fake_exec.rs"]
mod fake_exec;

const CLAUDE_WEEKLY_ONLY: &str = include_str!("fixtures/codexbar/claude_weekly_only.json");
const CODEX: &str = include_str!("fixtures/codexbar/codex.json");
const OPENCODEGO: &str = include_str!("fixtures/codexbar/opencodego.json");
const NOW_MS: u64 = 1_793_483_400_000; // 2026-10-31T21:50:00Z
const WEEK_RESET_MS: u64 = 1_793_667_600_000; // 2026-11-03T01:00:00Z
const FIVE: i32 = LimitWindow::FiveHour as i32;
const WEEK: i32 = LimitWindow::Weekly as i32;

fn rows(observations: &[LimitObservation]) -> Vec<(i32, u32, u64)> {
    let mut rows: Vec<_> = observations
        .iter()
        .map(|o| (o.window, o.used_permille, o.resets_at_ms))
        .collect();
    rows.sort();
    rows
}

#[test]
fn weekly_slot_named_primary_is_not_taken_for_the_five_hour_window() {
    let observed =
        observations_from_output("claude", CLAUDE_WEEKLY_ONLY.as_bytes(), NOW_MS).unwrap();
    assert_eq!(rows(&observed), vec![(WEEK, 540, WEEK_RESET_MS)]);
    assert!(observed.iter().all(|o| o.provider == "claude"));
}

#[test]
fn codex_windows_are_mapped_by_length() {
    let observed = observations_from_output("codex", CODEX.as_bytes(), NOW_MS).unwrap();
    assert_eq!(
        rows(&observed),
        vec![(FIVE, 0, 1_793_501_361_000), (WEEK, 970, 1_793_774_201_000)]
    );
}

#[test]
fn five_hour_window_of_claude_is_read_when_present() {
    let body = r#"[{"provider":"claude","usage":{
        "primary":{"usedPercent":44.5,"windowMinutes":300,"resetsAt":"2026-11-01T01:20:00Z"},
        "secondary":{"usedPercent":53,"windowMinutes":10080,"resetsAt":"2026-11-03T01:00:00Z"},
        "tertiary":null,
        "extraRateWindows":[{"title":"Fable only","window":{"usedPercent":99,"windowMinutes":300,"resetsAt":"2026-11-01T01:20:00Z"}}],
        "updatedAt":"2026-10-31T21:49:19Z"}}]"#;
    let observed = observations_from_output("claude", body.as_bytes(), NOW_MS).unwrap();
    assert_eq!(
        rows(&observed),
        vec![(FIVE, 445, 1_793_496_000_000), (WEEK, 530, WEEK_RESET_MS)]
    );
    // Время показания берётся из `updatedAt`, а не из часов опроса.
    assert!(
        observed
            .iter()
            .all(|o| o.observed_machine_ms == 1_793_483_359_000)
    );
}

#[test]
fn reading_time_never_runs_ahead_of_the_clock() {
    let body = r#"[{"provider":"codex","usage":{
        "primary":{"usedPercent":1,"windowMinutes":300},
        "updatedAt":"2030-01-01T00:00:00Z"}}]"#;
    let observed = observations_from_output("codex", body.as_bytes(), NOW_MS).unwrap();
    assert_eq!(observed[0].observed_machine_ms, NOW_MS);
    assert_eq!(
        observed[0].resets_at_ms, 0,
        "no resetsAt means unknown reset"
    );
}

#[test]
fn unusable_rows_are_skipped_and_garbage_is_an_error() {
    let body = r#"[{"provider":"claude","usage":{
        "primary":{"usedPercent":null,"windowMinutes":300},
        "secondary":{"usedPercent":10,"windowMinutes":1440},
        "tertiary":{"usedPercent":-1,"windowMinutes":10080}}}]"#;
    assert!(
        observations_from_output("claude", body.as_bytes(), NOW_MS)
            .unwrap()
            .is_empty()
    );
    for bad in [
        "",
        "not json",
        "{}",
        "[]",
        r#"[{"provider":"codex","usage":{}}]"#,
    ] {
        assert_eq!(
            observations_from_output("claude", bad.as_bytes(), NOW_MS).unwrap_err(),
            CodexBarError::Output,
            "{bad}"
        );
    }
}

#[test]
fn percent_above_the_scale_is_clamped() {
    let body =
        r#"[{"provider":"codex","usage":{"primary":{"usedPercent":140,"windowMinutes":300}}}]"#;
    let observed = observations_from_output("codex", body.as_bytes(), NOW_MS).unwrap();
    assert_eq!(observed[0].used_permille, 1000);
}

#[derive(Clone)]
struct TestClock(Arc<Mutex<Instant>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
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

/// Скрипт пишет свои аргументы в `args` и печатает фикстуру.
fn printing(dir: &Path, fixture: &str) -> PathBuf {
    fs::write(dir.join("out.json"), fixture).unwrap();
    executable(
        dir,
        &format!(
            "echo \"$@\" >> '{args}'\ncat '{out}'\n",
            args = dir.join("args").display(),
            out = dir.join("out.json").display()
        ),
    )
}

fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("args"))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn source(
    exe: PathBuf,
    clock: &TestClock,
    fallback: Option<Arc<dyn LimitsSource>>,
) -> CodexBarLimitsSource {
    let time = clock.0.clone();
    CodexBarLimitsSource::new(Some(exe), Duration::from_secs(300), fallback)
        .with_clock(move || *time.lock().unwrap())
}

#[derive(Default)]
struct Fallback(Mutex<Vec<String>>);

impl Fallback {
    fn reading(&self, call: String, provider: &str) -> Vec<LimitObservation> {
        self.0.lock().unwrap().push(call);
        vec![LimitObservation {
            provider: provider.into(),
            window: FIVE,
            used_permille: 7,
            resets_at_ms: 0,
            observed_machine_ms: 1,
        }]
    }
}

#[async_trait::async_trait]
impl LimitsSource for Fallback {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.reading(format!("hub:{provider}"), provider)
    }
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.reading(format!("idle:{provider}"), provider)
    }
}

#[tokio::test]
async fn poll_runs_codexbar_with_the_provider_and_json_format() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = source(printing(dir.path(), CLAUDE_WEEKLY_ONLY), &clock, None);

    assert!(source.poll("antigravity").await.is_empty());
    assert!(
        calls(dir.path()).is_empty(),
        "foreign provider never starts the program"
    );

    let observed = source.poll("claude").await;
    assert_eq!(rows(&observed), vec![(WEEK, 540, WEEK_RESET_MS)]);
    assert_eq!(
        calls(dir.path()),
        vec!["usage --provider claude --format json --no-color"]
    );
}

#[tokio::test]
async fn polls_wait_for_the_interval_and_only_the_owner_refresh_skips_it() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = source(printing(dir.path(), CODEX), &clock, None);

    assert_eq!(source.poll_idle("codex").await.len(), 2);
    clock.advance(Duration::from_secs(120));
    assert!(
        source.poll_idle("codex").await.is_empty(),
        "interval not over"
    );
    assert!(
        source.poll("codex").await.is_empty(),
        "the crunch check keeps the interval of the endpoint"
    );
    // Просьба владельца интервал не ждёт.
    assert_eq!(source.poll_refresh("codex").await.observations.len(), 2);
    clock.advance(Duration::from_secs(5));
    assert!(
        source.poll_refresh("codex").await.observations.is_empty(),
        "debounce"
    );
    clock.advance(Duration::from_secs(200));
    assert!(
        source.poll_idle("codex").await.is_empty(),
        "interval counts from the last run"
    );
    clock.advance(Duration::from_secs(120));
    assert_eq!(source.poll_idle("codex").await.len(), 2);
    assert_eq!(calls(dir.path()).len(), 3);
}

fn counting_failure(dir: &Path) -> PathBuf {
    executable(
        dir,
        &format!(
            "echo run >> '{}'
exit 3
",
            dir.join("args").display()
        ),
    )
}

fn poll_source(exe: PathBuf, clock: &TestClock, interval: u64) -> CodexBarLimitsSource {
    let time = clock.0.clone();
    CodexBarLimitsSource::new(Some(exe), Duration::from_secs(interval), None)
        .with_clock(move || *time.lock().unwrap())
}

#[tokio::test]
async fn failed_polls_back_off_exponentially_up_to_an_hour_and_success_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = poll_source(counting_failure(dir.path()), &clock, 900);
    let runs = || calls(dir.path()).len();

    source.poll_idle("claude").await;
    assert_eq!(runs(), 1);
    clock.advance(Duration::from_secs(900));
    source.poll_idle("claude").await;
    assert_eq!(runs(), 1, "first failure doubles the interval");
    source.poll("claude").await;
    assert_eq!(runs(), 1, "a crunch check waits as well");
    clock.advance(Duration::from_secs(900));
    source.poll_idle("claude").await;
    assert_eq!(runs(), 2);
    clock.advance(Duration::from_secs(1800));
    source.poll_idle("claude").await;
    assert_eq!(runs(), 2, "second failure waits an hour");
    clock.advance(Duration::from_secs(1800));
    source.poll_idle("claude").await;
    assert_eq!(runs(), 3);
    clock.advance(Duration::from_secs(3600));
    source.poll_idle("claude").await;
    assert_eq!(runs(), 4, "the wait stops growing at an hour");
}

#[tokio::test]
async fn success_after_failures_returns_to_the_plain_interval() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let exe = counting_failure(dir.path());
    let source = poll_source(exe.clone(), &clock, 900);
    source.poll_idle("codex").await;
    clock.advance(Duration::from_secs(1800));
    source.poll_idle("codex").await;
    assert_eq!(calls(dir.path()).len(), 2);

    fs::write(dir.path().join("out.json"), CODEX).unwrap();
    fake_exec::write_executable(
        &exe,
        format!(
            "#!/bin/sh
cat '{}'
",
            dir.path().join("out.json").display()
        ),
        0o700,
    );
    clock.advance(Duration::from_secs(3600));
    assert_eq!(source.poll_idle("codex").await.len(), 2);
    clock.advance(Duration::from_secs(900));
    assert_eq!(source.poll_idle("codex").await.len(), 2);
}

#[tokio::test]
async fn owner_refresh_skips_the_backoff_but_not_its_own_limit() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = poll_source(counting_failure(dir.path()), &clock, 900);
    source.poll_idle("claude").await;
    clock.advance(Duration::from_secs(5));
    source.poll_refresh("claude").await;
    assert_eq!(calls(dir.path()).len(), 1, "debounce");
    clock.advance(Duration::from_secs(10));
    source.poll_refresh("claude").await;
    assert_eq!(calls(dir.path()).len(), 2);
}

#[tokio::test]
async fn fallback_answers_while_codexbar_is_backed_off() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let fallback = Arc::new(Fallback::default());
    let time = clock.0.clone();
    let source = CodexBarLimitsSource::new(
        Some(counting_failure(dir.path())),
        Duration::from_secs(900),
        Some(fallback.clone()),
    )
    .with_clock(move || *time.lock().unwrap());
    assert_eq!(source.poll_idle("codex").await.len(), 1);
    clock.advance(Duration::from_secs(900));
    assert_eq!(source.poll_idle("codex").await.len(), 1);
    assert_eq!(calls(dir.path()).len(), 1, "codexbar itself waits");
    assert_eq!(
        *fallback.0.lock().unwrap(),
        vec!["idle:codex", "idle:codex"]
    );
}

#[test]
fn poll_interval_defaults_to_fifteen_minutes_with_the_same_floor() {
    let default = MachineConfig::default();
    assert_eq!(default.limits_poll_interval(), Duration::from_secs(900));
    let low = MachineConfig {
        limits_poll_interval: Duration::from_secs(60),
        ..MachineConfig::default()
    };
    assert_eq!(low.limits_poll_interval(), Duration::from_secs(900));
    let high = MachineConfig {
        limits_poll_interval: Duration::from_secs(1800),
        ..MachineConfig::default()
    };
    assert_eq!(high.limits_poll_interval(), Duration::from_secs(1800));
}

#[tokio::test]
async fn missing_binary_and_failures_fall_back_when_there_is_a_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();

    let none = source(dir.path().join("absent"), &clock, None);
    assert!(none.poll("claude").await.is_empty());
    assert_eq!(
        none.fetch("claude").await.map(|_| ()).unwrap_err(),
        CodexBarError::Missing
    );

    let fallback = Arc::new(Fallback::default());
    let missing = source(dir.path().join("absent"), &clock, Some(fallback.clone()));
    assert_eq!(missing.poll_idle("codex").await.len(), 1);
    assert_eq!(
        missing.poll("codex").await.len(),
        0,
        "debounce also hides the fallback"
    );
    clock.advance(Duration::from_secs(30));
    assert_eq!(missing.poll("codex").await.len(), 1);
    assert_eq!(*fallback.0.lock().unwrap(), vec!["idle:codex", "hub:codex"]);

    let failing = executable(dir.path(), "echo boom >&2\nexit 3\n");
    let failed = source(failing.clone(), &clock, None);
    assert_eq!(
        failed.fetch("claude").await.map(|_| ()).unwrap_err(),
        CodexBarError::Status(3)
    );

    let garbage = executable(dir.path(), "echo '<html>'\n");
    let bad = source(garbage, &clock, None);
    assert_eq!(
        bad.fetch("claude").await.map(|_| ()).unwrap_err(),
        CodexBarError::Output
    );
}

#[tokio::test]
async fn output_of_a_working_program_wins_over_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let fallback = Arc::new(Fallback::default());
    let source = source(printing(dir.path(), CODEX), &clock, Some(fallback.clone()));
    assert_eq!(source.poll_idle("codex").await.len(), 2);
    assert!(fallback.0.lock().unwrap().is_empty());
}

fn absent_windows(body: &str, provider: &str) -> Vec<i32> {
    reading_from_output(provider, body.as_bytes(), NOW_MS)
        .unwrap()
        .absent
        .iter()
        .map(|a| a.window)
        .collect()
}

#[test]
fn a_full_reading_without_a_five_hour_window_marks_it_absent() {
    // Claude без пятичасового лимита: окно только недельное.
    assert_eq!(absent_windows(CLAUDE_WEEKLY_ONLY, "claude"), vec![FIVE]);
    let reading = reading_from_output("claude", CLAUDE_WEEKLY_ONLY.as_bytes(), NOW_MS).unwrap();
    assert_eq!(reading.absent[0].provider, "claude");
    assert_eq!(
        reading.absent[0].observed_machine_ms,
        reading.observations[0].observed_machine_ms
    );
    // Пятичасовое окно есть: отсутствия нет.
    assert!(absent_windows(CODEX, "codex").is_empty());
}

#[test]
fn a_partial_or_empty_reading_never_marks_a_window_absent() {
    // Недельного окна тоже нет: чтение неполное, ничего не скрываем.
    let nothing =
        r#"[{"provider":"claude","usage":{"primary":{"usedPercent":null,"windowMinutes":300}}}]"#;
    assert!(absent_windows(nothing, "claude").is_empty());
    let unknown =
        r#"[{"provider":"claude","usage":{"primary":{"usedPercent":5,"windowMinutes":1440}}}]"#;
    assert!(absent_windows(unknown, "claude").is_empty());
    // Пятичасовая строка есть, но без процента: окно существует, чтение неполное.
    let broken_five = r#"[{"provider":"claude","usage":{
        "primary":{"usedPercent":null,"windowMinutes":300},
        "secondary":{"usedPercent":53,"windowMinutes":10080}}}]"#;
    assert!(absent_windows(broken_five, "claude").is_empty());
}

#[tokio::test]
async fn poll_report_carries_the_absence_and_a_failed_run_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = source(printing(dir.path(), CLAUDE_WEEKLY_ONLY), &clock, None);
    let fetched = source.poll_report("claude", false).await;
    assert_eq!(
        rows(&fetched.observations),
        vec![(WEEK, 540, WEEK_RESET_MS)]
    );
    assert_eq!(
        fetched.absent.iter().map(|a| a.window).collect::<Vec<_>>(),
        vec![FIVE]
    );

    let failing = executable(dir.path(), "exit 3\n");
    let failed = self::source(failing, &clock, None)
        .poll_report("claude", false)
        .await;
    assert!(failed.observations.is_empty() && failed.absent.is_empty());
}

#[test]
fn opencode_reading_uses_window_lengths_and_skips_the_monthly_window() {
    let reading = reading_from_output("opencode", OPENCODEGO.as_bytes(), NOW_MS).unwrap();
    assert_eq!(
        rows(&reading.observations),
        vec![(FIVE, 125, 1_793_501_361_000), (WEEK, 340, 1_794_182_399_000)]
    );
    assert!(reading.absent.is_empty(), "five hour window is present");
    assert!(
        reading.observations.iter().all(|o| o.provider == "opencode"),
        "hub provider, not the codexbar name"
    );
    assert!(
        reading.observations.iter().all(|o| o.observed_machine_ms == 1_793_483_361_000),
        "reading time comes from updatedAt"
    );
}

#[test]
fn opencode_weekly_only_reading_marks_only_its_own_five_hour_window_absent() {
    let body = r#"[{"provider":"opencodego","usage":{
        "primary":{"usedPercent":34,"windowMinutes":10080,"resetsAt":"2026-11-08T23:59:59Z"},
        "updatedAt":"2026-10-31T21:49:21Z"}}]"#;
    let reading = reading_from_output("opencode", body.as_bytes(), NOW_MS).unwrap();
    assert_eq!(rows(&reading.observations), vec![(WEEK, 340, 1_794_182_399_000)]);
    assert_eq!(reading.absent.len(), 1);
    assert_eq!(reading.absent[0].provider, "opencode");
    assert_eq!(reading.absent[0].window, FIVE);
}

#[test]
fn opencode_reading_without_a_usable_percent_marks_nothing_absent() {
    let body = r#"[{"provider":"opencodego","usage":{
        "primary":{"usedPercent":null,"windowMinutes":300},
        "secondary":{"usedPercent":null,"windowMinutes":10080},
        "tertiary":{"usedPercent":9.3,"windowMinutes":43200},
        "updatedAt":"2026-10-31T21:49:21Z"}}]"#;
    let reading = reading_from_output("opencode", body.as_bytes(), NOW_MS).unwrap();
    assert!(reading.observations.is_empty());
    assert!(reading.absent.is_empty());
}

#[tokio::test]
async fn opencode_poll_calls_codexbar_with_the_go_provider_name() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let source = source(printing(dir.path(), OPENCODEGO), &clock, None);

    let fetched = source.poll_report("opencode", false).await;
    assert_eq!(fetched.observations.len(), 2);
    assert!(fetched.absent.is_empty());
    let args = calls(dir.path());
    assert!(
        args.iter().any(|call| call == "usage --provider opencodego --format json --no-color"),
        "unexpected calls: {args:?}"
    );

    let failing = executable(dir.path(), "exit 3\n");
    let failed = self::source(failing, &clock, None)
        .poll_report("opencode", false)
        .await;
    assert!(failed.observations.is_empty() && failed.absent.is_empty());
}
