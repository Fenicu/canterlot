#![forbid(unsafe_code)]

//! Лимиты Codex (план 2b, Task 6): снимки потока, «кранч», probe и опрос без сессии.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::worker::v1::LimitObservation;
use canterlot_worker::{
    adapter::{
        LimitFact, LimitWindow,
        codex::limits::{
            CrunchContext, LimitTracker, ReachedDecision, TurnErrorInfo, windows_from_snapshot,
        },
        codex::probe::{self, ProbeError},
    },
    config::CodexConfig,
    machine::limits::{
        CodexRateLimitsSource, HubLimitsAge, LimitsSource, LiveLimits, RoutedLimitsSource,
    },
};
use serde_json::{Value, json};
use tokio::time::Instant;

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn codex_evidence_line(file: &str, line: usize) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/codex")
        .join(file);
    let lines: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    lines[line - 1]["data"].clone()
}

fn fixture_json(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(name);
    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap()
}

// Раньше сбросов в фикстурах.
const NOW: u64 = 1_792_900_000_000;

fn fixture_turn(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex")
        .join(name);
    fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["data"].clone())
        .collect()
}

fn snapshot_from_notification(data: &Value) -> &Value {
    &data["params"]["rateLimits"]
}

fn fact_key(fact: &LimitFact) -> (&'static str, LimitWindow, Option<u16>, Option<u64>) {
    match fact {
        LimitFact::Observed {
            window,
            used_permille,
            resets_at_ms,
        } => ("observed", *window, Some(*used_permille), *resets_at_ms),
        LimitFact::Reached {
            window,
            resets_at_ms,
        } => ("reached", *window, None, *resets_at_ms),
    }
}

#[test]
fn rate_limits_updated_becomes_observed_by_window_duration() {
    let data = codex_evidence_line("tools.jsonl", 28);
    let snapshot = snapshot_from_notification(&data);
    let mut tracker = LimitTracker::default();
    let facts: Vec<_> = tracker.on_snapshot(snapshot).iter().map(fact_key).collect();
    assert_eq!(
        facts,
        vec![
            (
                "observed",
                LimitWindow::FiveHour,
                Some(0),
                Some(1_792_927_784_000)
            ),
            (
                "observed",
                LimitWindow::Weekly,
                Some(0),
                Some(1_793_514_584_000)
            ),
        ]
    );
}

#[test]
fn identical_snapshots_emit_nothing_new() {
    let line28 = codex_evidence_line("tools.jsonl", 28);
    let snap = snapshot_from_notification(&line28);
    let mut tracker = LimitTracker::default();
    assert_eq!(tracker.on_snapshot(snap).len(), 2);
    assert!(tracker.on_snapshot(snap).is_empty());
    let line32 = codex_evidence_line("tools.jsonl", 32);
    let line39 = codex_evidence_line("tools.jsonl", 39);
    let snap2 = snapshot_from_notification(&line32);
    let snap3 = snapshot_from_notification(&line39);
    assert!(tracker.on_snapshot(snap2).is_empty());
    assert!(tracker.on_snapshot(snap3).is_empty());
}

#[test]
fn other_limit_ids_and_unknown_durations_are_ignored() {
    let snapshot = json!({
        "limitId": "other-bucket",
        "primary": {"usedPercent": 50, "resetsAt": 1, "windowDurationMins": 300},
        "secondary": {"usedPercent": 50, "resetsAt": 1, "windowDurationMins": 10080}
    });
    assert!(windows_from_snapshot(&snapshot).is_empty());
    let snapshot = json!({
        "limitId": "codex",
        "primary": {"usedPercent": 10, "resetsAt": 1, "windowDurationMins": 60},
        "secondary": {"usedPercent": 10, "resetsAt": 1, "windowDurationMins": 10080}
    });
    let windows = windows_from_snapshot(&snapshot);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].0, LimitWindow::Weekly);
}

#[test]
fn window_past_its_reset_is_not_exhausted() {
    let frames = fixture_turn("usage_limit_turn.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    let error = TurnErrorInfo {
        codex_error_info: Some("usageLimitExceeded".into()),
    };
    // Сброс уже прошёл: окно устарело, нужен свежий снимок.
    assert_eq!(
        tracker.on_turn_failed(&error, 1_792_929_900_001),
        ReachedDecision::NeedFreshRead(CrunchContext::UsageLimitExceeded)
    );
    assert!(matches!(
        tracker.on_turn_failed(&error, NOW),
        ReachedDecision::Reached(_)
    ));
}

#[test]
fn failed_read_still_reports_the_usage_limit() {
    let frames = fixture_turn("usage_limit_need_read.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    let facts = tracker.on_read_failed(CrunchContext::UsageLimitExceeded, NOW);
    assert_eq!(
        facts,
        vec![LimitFact::Reached {
            window: LimitWindow::FiveHour,
            resets_at_ms: Some(1_792_927_784_000),
        }]
    );
    // Прошедший сброс хабу не нужен: он разбудил бы очередь сразу.
    let facts = tracker.on_read_failed(CrunchContext::UsageLimitExceeded, 1_792_927_784_001);
    assert_eq!(
        facts,
        vec![LimitFact::Reached {
            window: LimitWindow::FiveHour,
            resets_at_ms: None,
        }]
    );
    assert_eq!(
        LimitTracker::default().on_read_failed(CrunchContext::UsageLimitExceeded, NOW),
        vec![LimitFact::Reached {
            window: LimitWindow::FiveHour,
            resets_at_ms: None,
        }]
    );
    assert!(
        tracker
            .on_read_failed(CrunchContext::RateLimitExceeded, NOW)
            .is_empty()
    );
}

#[test]
fn window_without_reset_keeps_the_usage_reading() {
    // В схеме RateLimitWindow обязателен только usedPercent.
    let snapshot = json!({"limitId": "codex",
        "primary": {"usedPercent": 42, "resetsAt": null, "windowDurationMins": 300}});
    assert_eq!(windows_from_snapshot(&snapshot).len(), 1);
    let mut tracker = LimitTracker::default();
    assert_eq!(
        tracker.on_snapshot(&snapshot),
        vec![LimitFact::Observed {
            window: LimitWindow::FiveHour,
            used_permille: 420,
            resets_at_ms: None,
        }]
    );
    assert_eq!(tracker.snapshot().five_hour, Some((420, None)));
    // Известный сброс не теряется, когда он пропал из следующего снимка.
    tracker.on_snapshot(&json!({"limitId": "codex",
        "primary": {"usedPercent": 42, "resetsAt": 1792927784, "windowDurationMins": 300}}));
    tracker.on_snapshot(&snapshot);
    assert_eq!(
        tracker.snapshot().five_hour,
        Some((420, Some(1_792_927_784_000)))
    );
}

#[test]
fn usage_limit_turn_becomes_reached_with_reset_of_exhausted_window() {
    let frames = fixture_turn("usage_limit_turn.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    let decision = tracker.on_turn_failed(
        &TurnErrorInfo {
            codex_error_info: Some("usageLimitExceeded".into()),
        },
        NOW,
    );
    assert_eq!(
        decision,
        ReachedDecision::Reached(LimitFact::Reached {
            window: LimitWindow::FiveHour,
            resets_at_ms: Some(1_792_929_900_000),
        })
    );
}

#[test]
fn usage_limit_without_exhausted_window_reads_limits_first() {
    let frames = fixture_turn("usage_limit_need_read.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    assert_eq!(
        tracker.on_turn_failed(
            &TurnErrorInfo {
                codex_error_info: Some("usageLimitExceeded".into()),
            },
            NOW,
        ),
        ReachedDecision::NeedFreshRead(CrunchContext::UsageLimitExceeded)
    );
    let read = fixture_json("usage_limit_read.json");
    let facts: Vec<_> = tracker
        .on_fresh_read(&read, CrunchContext::UsageLimitExceeded, NOW)
        .iter()
        .map(fact_key)
        .collect();
    assert!(
        facts.iter().any(|f| f.0 == "reached"
            && f.1 == LimitWindow::FiveHour
            && f.3 == Some(1_792_929_900_000)),
        "{facts:?}"
    );
}

#[test]
fn rate_limit_exceeded_without_exhausted_window_is_not_reached() {
    let frames = fixture_turn("rate_limit_turn.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    assert_eq!(
        tracker.on_turn_failed(
            &TurnErrorInfo {
                codex_error_info: Some("rateLimitExceeded".into()),
            },
            NOW,
        ),
        ReachedDecision::NeedFreshRead(CrunchContext::RateLimitExceeded)
    );
    let read = fixture_json("rate_limits_read.json");
    let facts: Vec<_> = tracker
        .on_fresh_read(&read, CrunchContext::RateLimitExceeded, NOW)
        .iter()
        .map(fact_key)
        .collect();
    assert!(!facts.iter().any(|f| f.0 == "reached"), "{facts:?}");
}

#[test]
fn ordinary_usage_blocked_is_read_from_the_response_top_level() {
    let window = |percent: f64| {
        json!({"limitId": "codex",
            "primary": {"windowDurationMins": 300, "usedPercent": percent, "resetsAt": 1792927784},
            "secondary": {"windowDurationMins": 10080, "usedPercent": 10.0, "resetsAt": 1793514584}})
    };
    let reached = |read: Value| {
        let mut tracker = LimitTracker::default();
        tracker
            .on_fresh_read(&read, CrunchContext::RateLimitExceeded, NOW)
            .iter()
            .any(|fact| matches!(fact, LimitFact::Reached { .. }))
    };
    // Схема GetAccountRateLimitsResponse: ordinaryUsageAllowed — поле ответа.
    assert!(reached(
        json!({"rateLimits": window(40.0), "ordinaryUsageAllowed": false})
    ));
    assert!(!reached(
        json!({"rateLimits": window(40.0), "ordinaryUsageAllowed": true})
    ));
    let mut nested = window(40.0);
    nested["ordinaryUsageAllowed"] = false.into();
    assert!(
        !reached(json!({"rateLimits": nested})),
        "not a snapshot field"
    );
}

struct FakeProbeServer {
    executable: PathBuf,
    methods_log: PathBuf,
    _dir: tempfile::TempDir,
}

impl FakeProbeServer {
    fn new(response: &Value, hang_on_read: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let response_path = dir.path().join("response.json");
        fs::write(&response_path, serde_json::to_string(response).unwrap()).unwrap();
        let methods_log = dir.path().join("methods.log");
        fs::write(&methods_log, "").unwrap();
        let handler = dir.path().join("handler.py");
        fake_exec::write_executable(
            &handler,
            r#"#!/usr/bin/env python3
import json, sys
response = json.load(open(sys.argv[1]))
hang = sys.argv[2] == "hang"
log = sys.argv[3]
for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method:
        with open(log, "a") as f:
            f.write(method + "\n")
    rid = msg.get("id")
    if method == "initialize":
        print(json.dumps({"id": rid, "result": {}}), flush=True)
    elif method == "account/rateLimits/read":
        if hang:
            import time
            time.sleep(3600)
        print(json.dumps({"id": rid, "result": response}), flush=True)
"#,
            0o755,
        );
        let wrapper = dir.path().join("codex");
        fake_exec::write_executable(
            &wrapper,
            format!(
                "#!/bin/sh\nif [ \"$1\" = app-server ]; then exec \"{}\" \"{}\" \"{}\" \"{}\"; fi\nexit 1\n",
                handler.display(),
                response_path.display(),
                if hang_on_read { "hang" } else { "ok" },
                methods_log.display(),
            ),
            0o755,
        );
        Self {
            executable: wrapper,
            methods_log,
            _dir: dir,
        }
    }

    fn methods_seen(&self) -> Vec<String> {
        fs::read_to_string(&self.methods_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[tokio::test]
async fn idle_source_reads_limits_through_a_short_lived_app_server() {
    let response = fixture_json("rate_limits_read.json");
    let server = FakeProbeServer::new(&response, false);
    let config = CodexConfig::default();
    let source = CodexRateLimitsSource::new(&config, LiveLimits::default(), Arc::new(|_| None))
        .with_executable(server.executable.clone());
    let observations = source.fetch().await.unwrap();
    assert_eq!(observations.len(), 4);
    assert!(observations.iter().all(|o| o.provider == "codex"));
    let methods = server.methods_seen();
    assert!(methods.contains(&"initialize".to_owned()));
    assert!(methods.contains(&"account/rateLimits/read".to_owned()));
    assert!(!methods.iter().any(|m| m.starts_with("thread/")));
}

#[tokio::test]
async fn idle_source_times_out_and_kills_the_probe() {
    let server = FakeProbeServer::new(&json!({}), true);
    let err = probe::probe(
        &server.executable,
        "account/rateLimits/read",
        json!({}),
        Duration::from_millis(800),
    )
    .await
    .unwrap_err();
    assert_eq!(err, ProbeError::Timeout);
}

#[derive(Default)]
struct CountingSource(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl LimitsSource for CountingSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.0.lock().unwrap().push(format!("hub:{provider}"));
        Vec::new()
    }
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.0.lock().unwrap().push(format!("idle:{provider}"));
        Vec::new()
    }
}

#[tokio::test]
async fn routed_source_dispatches_by_provider() {
    let mut routes = std::collections::HashMap::new();
    let codex = Arc::new(CountingSource::default());
    let claude = Arc::new(CountingSource::default());
    routes.insert("codex".into(), codex.clone() as Arc<dyn LimitsSource>);
    routes.insert("claude".into(), claude.clone() as Arc<dyn LimitsSource>);
    let source = RoutedLimitsSource::new(
        routes,
        Arc::new(canterlot_worker::machine::limits::NoLimitsSource),
    );
    source.poll("codex").await;
    source.poll_idle("claude").await;
    source.poll("other").await;
    assert_eq!(*codex.0.lock().unwrap(), vec!["hub:codex".to_owned()]);
    assert_eq!(*claude.0.lock().unwrap(), vec!["idle:claude".to_owned()]);
}

struct TestClock(Arc<Mutex<Instant>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

#[tokio::test]
async fn idle_ticker_polls_only_without_a_recent_observation() {
    let response = fixture_json("rate_limits_read.json");
    let server = FakeProbeServer::new(&response, false);
    let clock = TestClock::new();
    let hub_received = Arc::new(Mutex::new(*clock.0.lock().unwrap()));
    let hub_age: HubLimitsAge = {
        let hub = hub_received.clone();
        Arc::new(move |provider| (provider == "codex").then(|| *hub.lock().unwrap()))
    };
    let config = CodexConfig::default();
    let time = clock.0.clone();
    let source = CodexRateLimitsSource::new(&config, LiveLimits::default(), hub_age)
        .with_clock(move || *time.lock().unwrap())
        .with_executable(server.executable.clone());
    assert!(
        source.poll_idle("codex").await.is_empty(),
        "hub snapshot is still within the interval"
    );
    clock.advance(Duration::from_secs(301));
    assert_eq!(source.poll_idle("codex").await.len(), 4);
}

const ENV_DUMP_CODEX: &str = r#"#!/usr/bin/env python3
import json, os, sys, pathlib
d = pathlib.Path(__file__).parent
name = 'version' if '--version' in sys.argv else 'probe'
(d / f'{name}-env.json').write_text(json.dumps(dict(os.environ)))
if name == 'version':
    print('codex-cli 0.164.0'); sys.exit(0)
for line in sys.stdin:
    msg = json.loads(line)
    if 'id' in msg:
        print(json.dumps({'id': msg['id'], 'result': {}}), flush=True)
"#;

const LEAKED: [&str; 6] = [
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "OPENAI_BASE_URL",
    "RUST_LOG",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDECODE",
];

// Тест перезапускает сам себя с отравленным окружением: так родительское
// окружение проверки содержит ключи биллинга без изменения env процесса тестов.
#[tokio::test]
async fn probes_drop_billing_and_leaked_environment() {
    if std::env::var_os("CANTERLOT_PROBE_ENV_CHILD").is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "probes_drop_billing_and_leaked_environment",
                "--exact",
                "--nocapture",
            ])
            .env("CANTERLOT_PROBE_ENV_CHILD", "1");
        for key in LEAKED {
            child.env(key, "canary-value");
        }
        let status = child.status().unwrap();
        assert!(status.success(), "child run failed: {status}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex");
    fs::write(&exe, ENV_DUMP_CODEX).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    probe::probe(
        &exe,
        "account/rateLimits/read",
        json!({}),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    canterlot_worker::adapter::codex::version::check(&exe)
        .await
        .unwrap();
    for name in ["probe-env.json", "version-env.json"] {
        let env: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join(name)).unwrap()).unwrap();
        for key in LEAKED {
            assert!(env.get(key).is_none(), "{name}: {key} leaked");
        }
        assert!(
            env.get("CANTERLOT_PROBE_ENV_CHILD").is_none(),
            "{name}: CANTERLOT_* leaked"
        );
        assert!(env.get("PATH").is_some(), "{name}: PATH kept");
    }
}

fn process_alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| !rest.starts_with('Z'))
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn probe_timeout_kills_the_whole_app_server_group() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex");
    // Обёртка как node /usr/bin/codex: родной сервер — её ребёнок в той же группе.
    fs::write(
        &exe,
        "#!/bin/sh\nsleep 300 &\necho $! > \"$(dirname \"$0\")/native.pid\"\nexec sleep 300\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let err = probe::probe(
        &exe,
        "account/rateLimits/read",
        json!({}),
        Duration::from_millis(800),
    )
    .await
    .unwrap_err();
    assert_eq!(err, ProbeError::Timeout);
    let native: u32 = fs::read_to_string(dir.path().join("native.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(native) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let alive = process_alive(native);
    if alive {
        let _ = std::process::Command::new("kill")
            .arg(native.to_string())
            .status();
    }
    assert!(!alive, "native server {native} survived the probe timeout");
}

#[tokio::test]
async fn version_timeout_kills_the_whole_cli_group() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex");
    // Обёртка висит на --version, её ребёнок — в той же группе.
    fs::write(
        &exe,
        "#!/bin/sh\nsleep 300 &\necho $! > \"$(dirname \"$0\")/native.pid\"\nexec sleep 300\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let err =
        canterlot_worker::adapter::codex::version::check_within(&exe, Duration::from_millis(800))
            .await
            .unwrap_err();
    assert_eq!(
        err,
        canterlot_worker::adapter::codex::version::VersionError::Failed
    );
    let native: u32 = fs::read_to_string(dir.path().join("native.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(native) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let alive = process_alive(native);
    if alive {
        let _ = std::process::Command::new("kill")
            .arg(native.to_string())
            .status();
    }
    assert!(!alive, "native child {native} survived the version timeout");
}
