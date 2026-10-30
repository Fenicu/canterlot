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
fn usage_limit_turn_becomes_reached_with_reset_of_exhausted_window() {
    let frames = fixture_turn("usage_limit_turn.jsonl");
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(snapshot_from_notification(&frames[0]));
    let decision = tracker.on_turn_failed(&TurnErrorInfo {
        codex_error_info: Some("usageLimitExceeded".into()),
    });
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
        tracker.on_turn_failed(&TurnErrorInfo {
            codex_error_info: Some("usageLimitExceeded".into()),
        }),
        ReachedDecision::NeedFreshRead(CrunchContext::UsageLimitExceeded)
    );
    let read = fixture_json("usage_limit_read.json");
    let facts: Vec<_> = tracker
        .on_fresh_read(&read, CrunchContext::UsageLimitExceeded)
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
        tracker.on_turn_failed(&TurnErrorInfo {
            codex_error_info: Some("rateLimitExceeded".into()),
        }),
        ReachedDecision::NeedFreshRead(CrunchContext::RateLimitExceeded)
    );
    let read = fixture_json("rate_limits_read.json");
    let facts: Vec<_> = tracker
        .on_fresh_read(&read, CrunchContext::RateLimitExceeded)
        .iter()
        .map(fact_key)
        .collect();
    assert!(!facts.iter().any(|f| f.0 == "reached"), "{facts:?}");
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
