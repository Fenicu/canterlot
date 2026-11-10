#![forbid(unsafe_code)]

//! Лимиты Claude Code (план 2a, Task 6): события потока, упор в лимит,
//! и тикер опроса. Сеть и настоящий `claude` не нужны: записи этапа 0 и собранные фикстуры.

use std::{
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::worker::v1::{
    ControlDown, LimitObservation, LimitWindow as ProtoWindow, PollLimits, control_down,
    control_up::Msg,
};
use canterlot_worker::{
    adapter::{
        LimitFact, LimitWindow,
        claude_code::{
            limits::{LimitSnapshot, LimitTracker},
            parser::{Parsed, Parser},
        },
    },
    config::{ClaudeCodeConfig, ConfigErrorCategory},
    machine::limits::LimitsSource,
    session::{SessionState, Worker},
};
use serde_json::json;
use tokio::{sync::watch, time::Instant};

#[path = "support/fixtures.rs"]
mod fixtures;
#[allow(dead_code)]
#[path = "support/frozen_clock.rs"]
mod frozen_clock;
#[allow(dead_code)]
mod support;

#[path = "support/fake_exec.rs"]
mod fake_exec;

const FIRST_TICK: Duration = Duration::from_secs(60);

type Fact = (&'static str, LimitWindow, Option<u16>, Option<u64>);

fn key(fact: &LimitFact) -> Fact {
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

// Как адаптер (Task 7): кадры идут через разборщик, лимитные — в трекер.
fn track(tracker: &mut LimitTracker, lines: &[String]) -> Vec<Fact> {
    let mut parser = Parser::new();
    let mut facts = Vec::new();
    for line in lines {
        for parsed in parser.feed(line) {
            match parsed {
                Parsed::RateLimit(info) => facts.extend(tracker.on_rate_limit(&info)),
                Parsed::TurnError { status, error, .. } => {
                    facts.extend(tracker.on_turn_error(status, error.as_deref()))
                }
                _ => {}
            }
        }
    }
    facts.iter().map(key).collect()
}

fn reached(facts: &[Fact]) -> Vec<(LimitWindow, Option<u64>)> {
    facts
        .iter()
        .filter(|f| f.0 == "reached")
        .map(|f| (f.1, f.3))
        .collect()
}

#[test]
fn rate_limit_event_becomes_observed_permille_and_reset_ms() {
    let replay = fixtures::replay_evidence("resume");
    let mut tracker = LimitTracker::default();
    let facts = track(&mut tracker, &replay.out);
    assert_eq!(
        &facts[..2],
        &[
            (
                "observed",
                LimitWindow::FiveHour,
                Some(500),
                Some(1_792_919_400_000)
            ),
            (
                "observed",
                LimitWindow::Weekly,
                Some(810),
                Some(1_793_062_800_000)
            ),
        ]
    );
    // Четыре события в записи, `overageStatus: "rejected"` упором не считается.
    assert_eq!(facts.len(), 8);
    assert!(reached(&facts).is_empty());
    assert_eq!(
        tracker.snapshot(),
        LimitSnapshot {
            five_hour: Some((500, Some(1_792_919_400_000))),
            weekly: Some((810, Some(1_793_062_800_000))),
            weekly_fable: None,
        }
    );
}

#[test]
fn rejected_window_becomes_reached_with_its_reset() {
    let frames = fixtures::replay_fixture("limit_rejected");
    let mut tracker = LimitTracker::default();
    let facts = track(&mut tracker, &frames);
    assert_eq!(
        facts,
        vec![
            (
                "observed",
                LimitWindow::FiveHour,
                Some(420),
                Some(1_793_226_600_000)
            ),
            (
                "observed",
                LimitWindow::Weekly,
                Some(1000),
                Some(1_793_667_600_000)
            ),
            (
                "reached",
                LimitWindow::Weekly,
                None,
                Some(1_793_667_600_000)
            ),
        ]
    );
    // Повтор того же упора и проваленный ход второго факта не дают.
    assert!(reached(&track(&mut tracker, &frames)).is_empty());
    assert!(tracker.on_turn_error(Some(429), None).is_none());

    // Только верхний `status` с `rateLimitType`: окно и сброс берутся из него.
    let mut tracker = LimitTracker::default();
    let facts: Vec<Fact> = tracker
        .on_rate_limit(&json!({
            "status": "rejected",
            "rateLimitType": "five_hour",
            "resetsAt": 1_793_227_200_u64,
            "unifiedWindows": {
                "five_hour": {"resetsAt": 1_793_227_200_u64, "utilization": 1.0},
                "seven_day": {"resetsAt": 1_793_667_600_u64, "utilization": 0.3}
            }
        }))
        .iter()
        .map(key)
        .collect();
    assert_eq!(
        reached(&facts),
        vec![(LimitWindow::FiveHour, Some(1_793_227_200_000))]
    );
}

const FABLE_FRAME: &str =
    include_str!("../../canterlot-proto/tests/fixtures/fable_rate_limit.json");

#[test]
fn fable_weekly_window_is_observed_separate_from_the_weekly_one() {
    let mut tracker = LimitTracker::default();
    let info: serde_json::Value = serde_json::from_str(FABLE_FRAME).unwrap();
    let raw = tracker.on_rate_limit(&info);
    let facts: Vec<Fact> = raw.iter().map(key).collect();
    assert_eq!(
        facts,
        vec![
            (
                "observed",
                LimitWindow::Weekly,
                Some(30),
                Some(1_794_877_200_000)
            ),
            (
                "observed",
                LimitWindow::WeeklyFable,
                Some(50),
                Some(1_794_877_200_000)
            ),
        ],
        "no five-hour fact when the frame has no five_hour row"
    );
    assert_eq!(
        tracker.snapshot(),
        LimitSnapshot {
            five_hour: None,
            weekly: Some((30, Some(1_794_877_200_000))),
            weekly_fable: Some((50, Some(1_794_877_200_000))),
        }
    );
    let observations: Vec<LimitObservation> = raw
        .into_iter()
        .filter_map(|fact| match fact {
            LimitFact::Observed {
                window,
                used_permille,
                resets_at_ms,
            } => Some(LimitObservation {
                provider: "claude".into(),
                window: window.into(),
                used_permille: u32::from(used_permille),
                resets_at_ms: resets_at_ms.unwrap_or_default(),
                ..Default::default()
            }),
            LimitFact::Reached { .. } => None,
        })
        .collect();
    assert_eq!(
        observations,
        vec![
            LimitObservation {
                provider: "claude".into(),
                window: ProtoWindow::Weekly as i32,
                used_permille: 30,
                resets_at_ms: 1_794_877_200_000,
                ..Default::default()
            },
            LimitObservation {
                provider: "claude".into(),
                window: ProtoWindow::WeeklyFable as i32,
                used_permille: 50,
                resets_at_ms: 1_794_877_200_000,
                ..Default::default()
            },
        ]
    );
}

#[test]
fn rejected_fable_window_is_reached_only_for_fable() {
    let info = json!({
        "status": "rejected",
        "rateLimitType": "seven_day_overage_included",
        "resetsAt": 1_794_877_200_u64,
        "unifiedWindows": {
            "seven_day": {"utilization": 0.3, "resetsAt": 1_794_877_200_u64, "status": "allowed"},
            "seven_day_overage_included": {"utilization": 1.0, "resetsAt": 1_794_877_200_u64, "status": "rejected"}
        }
    });
    let mut tracker = LimitTracker::default();
    let facts: Vec<Fact> = tracker.on_rate_limit(&info).iter().map(key).collect();
    assert_eq!(
        reached(&facts),
        vec![(LimitWindow::WeeklyFable, Some(1_794_877_200_000))]
    );
    assert!(reached(&track(&mut tracker, &[info.to_string()])).is_empty());
}

#[test]
fn opus_frame_does_not_clear_a_known_fable_reading() {
    let mut tracker = LimitTracker::default();
    let fable: serde_json::Value = serde_json::from_str(FABLE_FRAME).unwrap();
    tracker.on_rate_limit(&fable);
    let opus = json!({
        "status": "allowed",
        "rateLimitType": "seven_day",
        "unifiedWindows": {
            "seven_day": {"utilization": 0.4, "resetsAt": 1_794_877_200_u64, "status": "allowed"}
        }
    });
    let facts: Vec<Fact> = tracker.on_rate_limit(&opus).iter().map(key).collect();
    assert_eq!(
        facts,
        vec![(
            "observed",
            LimitWindow::Weekly,
            Some(400),
            Some(1_794_877_200_000)
        )]
    );
    assert_eq!(
        tracker.snapshot().weekly_fable,
        Some((50, Some(1_794_877_200_000)))
    );
}

#[test]
fn usage_limit_429_becomes_reached_with_reset_from_rate_limit_info() {
    let frames = fixtures::replay_fixture("usage_limit_429");
    let mut tracker = LimitTracker::default();
    let facts = track(&mut tracker, &frames);
    assert_eq!(
        reached(&facts),
        vec![(LimitWindow::FiveHour, Some(1_793_227_200_000))],
        "one Reached, reset from the error's rate_limit_info: {facts:?}"
    );
    assert_eq!(
        tracker.snapshot().five_hour,
        Some((1000, Some(1_793_227_200_000)))
    );

    // Без кадра ошибки сброс берётся из последнего события потока.
    let without_error: Vec<String> = frames
        .iter()
        .filter(|line| !line.contains("usage_limit_reached"))
        .cloned()
        .collect();
    assert_eq!(without_error.len(), frames.len() - 1);
    let mut tracker = LimitTracker::default();
    assert_eq!(
        reached(&track(&mut tracker, &without_error)),
        vec![(LimitWindow::FiveHour, Some(1_793_226_600_000))]
    );

    // Признак `usage_limit_reached` без статуса тоже упор.
    let mut tracker = LimitTracker::default();
    let warn: Vec<String> = frames
        .iter()
        .filter(|line| line.contains("allowed_warning"))
        .cloned()
        .collect();
    track(&mut tracker, &warn);
    let fact = tracker
        .on_turn_error(None, Some("api_error: usage_limit_reached"))
        .map(|f| key(&f));
    assert_eq!(
        fact,
        Some((
            "reached",
            LimitWindow::FiveHour,
            None,
            Some(1_793_226_600_000)
        ))
    );
}

#[test]
fn retried_429_without_usage_limit_is_not_a_limit_fact() {
    let replay = fixtures::replay_evidence("service_failure");
    let mut tracker = LimitTracker::default();
    assert!(track(&mut tracker, &replay.out).is_empty());

    let retry = replay
        .out
        .iter()
        .find(|line| line.contains("\"api_retry\""))
        .expect("recorded api_retry");
    let mut retry: serde_json::Value = serde_json::from_str(retry).unwrap();
    retry["error_status"] = json!(429);
    retry["error"] = json!("rate_limit");
    let success = json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "OK", "modelUsage": {}, "session_id": retry["session_id"]
    });
    let lines = vec![retry.to_string(), retry.to_string(), success.to_string()];
    assert!(track(&mut tracker, &lines).is_empty());
    assert_eq!(tracker.snapshot(), LimitSnapshot::default());
    assert!(
        tracker
            .on_turn_error(Some(500), Some("overloaded"))
            .is_none()
    );
    assert!(tracker.on_turn_error(None, None).is_none());
}

// --- флаг платного опроса ---

#[test]
fn paid_model_poll_flag_is_rejected_in_pilot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    let base = format!(
        r#"hub_url = "https://hub.example:8443"
ca_cert = "ca.pem"
token_file = "token"
machine_id = "{0}"
installation_id = "{0}"
restore_generation = "{0}"
data_dir = "data"
"#,
        uuid::Uuid::nil()
    );

    fs::write(
        &path,
        format!("{base}\n[claude_code]\nallow_paid_model_poll = true\n"),
    )
    .unwrap();
    let error = canterlot_worker::config::load(&path).unwrap_err();
    assert_eq!(
        error.category,
        ConfigErrorCategory::PaidModelPollUnsupported
    );
    let message = error.to_string();
    assert!(message.contains("allow_paid_model_poll"), "{message}");
    assert!(message.contains("not supported in the pilot"), "{message}");

    fs::write(
        &path,
        format!("{base}\n[claude_code]\nallow_paid_model_poll = false\n"),
    )
    .unwrap();
    let config = canterlot_worker::config::load(&path).unwrap();
    assert!(!config.claude_code.unwrap().allow_paid_model_poll);
}

// --- тикер опроса без живых сессий ---

#[derive(Default)]
struct CountingSource(Mutex<Vec<String>>);

impl CountingSource {
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl CountingSource {
    fn observe(&self, call: String, provider: &str) -> Vec<LimitObservation> {
        self.0.lock().unwrap().push(call);
        vec![LimitObservation {
            provider: provider.to_owned(),
            window: ProtoWindow::FiveHour as i32,
            used_permille: 123,
            resets_at_ms: 0,
            observed_machine_ms: 1,
            ..Default::default()
        }]
    }
}

#[async_trait::async_trait]
impl LimitsSource for CountingSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        self.observe(format!("hub:{provider}"), provider)
    }
    async fn poll_idle(&self, provider: &str) -> Vec<LimitObservation> {
        self.observe(format!("idle:{provider}"), provider)
    }
}

/// Claude отдаёт показание, Codex молчит: как опрос, отсечённый интервалом.
struct ClaudeOnlySource;

#[async_trait::async_trait]
impl LimitsSource for ClaudeOnlySource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        if provider != "claude" {
            return Vec::new();
        }
        vec![LimitObservation {
            provider: provider.to_owned(),
            window: ProtoWindow::Weekly as i32,
            used_permille: 260,
            resets_at_ms: 0,
            observed_machine_ms: 1,
            ..Default::default()
        }]
    }
}

async fn next_limits_report(hub: &mut support::ProtocolHub) -> Vec<LimitObservation> {
    loop {
        if let support::Observed::Control(message) = hub.event().await
            && let Some(Msg::LimitsReport(report)) = message.msg
        {
            return report.observations;
        }
    }
}

// Хаб держит один необработанный отчёт: пустой, идущий следом, вытеснил бы настоящий.
#[tokio::test(start_paused = true)]
async fn empty_poll_sends_no_report() {
    frozen_clock::run(async {
        let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
            capabilities: vec!["pilot_v1".into()],
            ..Default::default()
        })
        .await;
        let worker = Worker::new(hub.config.clone(), store)
            .with_runtime_dir(hub.config.data_dir.join("runtime"))
            .with_limits_source(Arc::new(ClaudeOnlySource));
        let mut state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        state.wait_for(|s| *s == SessionState::Live).await.unwrap();

        for provider in ["codex", "claude"] {
            hub.send(ControlDown {
                msg: Some(control_down::Msg::PollLimits(PollLimits {
                    provider: provider.into(),
                    owner_refresh: false,
                })),
            })
            .await;
        }
        let report = next_limits_report(&mut hub).await;
        assert_eq!(report.len(), 1, "first report carries the reading");
        assert_eq!(report[0].provider, "claude");

        stop.send(true).unwrap();
        join.await.unwrap().unwrap();
    })
    .await;
}

/// Источник, который знает только об отсутствии пятичасового окна.
struct AbsenceOnlySource;

#[async_trait::async_trait]
impl LimitsSource for AbsenceOnlySource {
    async fn poll(&self, _provider: &str) -> Vec<LimitObservation> {
        Vec::new()
    }
    async fn poll_report(
        &self,
        provider: &str,
        _idle: bool,
    ) -> canterlot_worker::machine::limits::LimitsFetch {
        canterlot_worker::machine::limits::LimitsFetch {
            observations: Vec::new(),
            absent: vec![canterlot_proto::worker::v1::LimitAbsence {
                provider: provider.to_owned(),
                window: ProtoWindow::FiveHour as i32,
                observed_machine_ms: 7,
            }],
        }
    }
}

#[tokio::test(start_paused = true)]
async fn absence_alone_is_reported_to_the_hub() {
    frozen_clock::run(async {
        let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
            capabilities: vec!["pilot_v1".into()],
            ..Default::default()
        })
        .await;
        let worker = Worker::new(hub.config.clone(), store)
            .with_runtime_dir(hub.config.data_dir.join("runtime"))
            .with_limits_source(Arc::new(AbsenceOnlySource));
        let mut state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        state.wait_for(|s| *s == SessionState::Live).await.unwrap();
        hub.send(ControlDown {
            msg: Some(control_down::Msg::PollLimits(PollLimits {
                provider: "claude".into(),
                owner_refresh: false,
            })),
        })
        .await;
        let absent = loop {
            if let support::Observed::Control(message) = hub.event().await
                && let Some(Msg::LimitsReport(report)) = message.msg
            {
                assert!(report.observations.is_empty());
                break report.absent;
            }
        };
        assert_eq!(absent.len(), 1);
        assert_eq!(absent[0].window, ProtoWindow::FiveHour as i32);
        stop.send(true).unwrap();
        join.await.unwrap().unwrap();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn idle_ticker_requests_a_claude_poll_each_interval() {
    frozen_clock::run(async {
        let (mut hub, store) = support::ProtocolHub::start(support::Behavior {
            capabilities: vec!["pilot_v1".into()],
            ..Default::default()
        })
        .await;
        let quiet = Duration::from_secs(10 * 3600);
        hub.config.timers.heartbeat = quiet;
        hub.config.timers.heartbeat_ack = quiet;
        hub.config.timers.http2_keepalive_interval = quiet;
        hub.config.timers.http2_keepalive_timeout = quiet;
        hub.config.machine.metrics_interval = quiet;
        let cli_dir = tempfile::tempdir().unwrap();
        let executable = cli_dir.path().join("fake-cli");
        fake_exec::write_executable(&executable, "#!/bin/sh\nprintf '2.1.311\\n'\n", 0o700);
        hub.config.claude_code = Some(ClaudeCodeConfig {
            executable,
            ..ClaudeCodeConfig::default()
        });
        hub.config.machine.limits_poll_interval = Duration::from_secs(900);
        let interval = Duration::from_secs(900);
        let source = Arc::new(CountingSource::default());
        let worker = Worker::new(hub.config.clone(), store)
            .with_runtime_dir(hub.config.data_dir.join("runtime"))
            .with_limits_source(source.clone());
        let started = Instant::now();
        let mut state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        state.wait_for(|s| *s == SessionState::Live).await.unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);

        tokio::time::advance(FIRST_TICK - Duration::from_secs(1)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert!(
            source.calls().is_empty(),
            "first tick one minute after start"
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        let report = next_limits_report(&mut hub).await;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].provider, "claude");
        assert_eq!(report[0].used_permille, 123);
        assert_eq!(source.calls(), vec!["idle:claude".to_owned()]);

        tokio::time::advance(interval).await;
        next_limits_report(&mut hub).await;
        assert_eq!(
            source.calls(),
            vec!["idle:claude".to_owned(), "idle:claude".to_owned()]
        );
        // Запрос хаба (проверка пробуждения «кранча») идёт своим путём.
        hub.send(ControlDown {
            msg: Some(control_down::Msg::PollLimits(PollLimits {
                provider: "claude".into(),
                owner_refresh: false,
            })),
        })
        .await;
        next_limits_report(&mut hub).await;
        assert_eq!(source.calls().last().unwrap(), "hub:claude");

        stop.send(true).unwrap();
        join.await.unwrap().unwrap();
    })
    .await;
}
