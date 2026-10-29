#![forbid(unsafe_code)]

//! Лимиты Claude Code (план 2a, Task 6): события потока, упор в лимит,
//! редкий опрос OAuth и тикер опроса. Сеть и настоящий `claude` не нужны:
//! записи этапа 0, собранные фикстуры и локальная HTTP-заглушка.

use std::{
    fs,
    path::Path,
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
    machine::limits::{ClaudeOAuthLimitsSource, LimitsSource, LiveLimits, OAuthPollError},
    session::{SessionState, Worker},
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::watch,
    task::JoinHandle,
    time::Instant,
};

#[path = "support/fixtures.rs"]
mod fixtures;
#[allow(dead_code)]
#[path = "support/frozen_clock.rs"]
mod frozen_clock;
#[allow(dead_code)]
mod support;

const FAKE_TOKEN: &str = "sk-ant-oat01-canterlot-fake-token-for-tests";
const FAR_FUTURE_MS: u64 = 4_102_444_800_000;
const HALF_HOUR: Duration = Duration::from_secs(1800);

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

// --- опрос OAuth ---

struct Stub {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Stub {
    async fn start(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/oauth/usage", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            let mut responses = responses.into_iter();
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut head = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    head.extend_from_slice(&buffer[..read]);
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).into_owned());
                let (status, body) = responses.next().unwrap_or((500, String::new()));
                let reply = format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn credentials(dir: &Path, token: &str, expires_at: u64) -> std::path::PathBuf {
    let path = dir.join(".credentials.json");
    fs::write(
        &path,
        json!({"claudeAiOauth": {
            "accessToken": token,
            "refreshToken": "fake-refresh-token-for-tests",
            "expiresAt": expires_at,
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "max"
        }})
        .to_string(),
    )
    .unwrap();
    path
}

struct TestClock(Arc<Mutex<Instant>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

fn source(
    credentials_path: &Path,
    stub: &Stub,
    clock: &TestClock,
    live: LiveLimits,
) -> ClaudeOAuthLimitsSource {
    let config = ClaudeCodeConfig {
        credentials_path: Some(credentials_path.to_path_buf()),
        ..ClaudeCodeConfig::default()
    };
    let time = clock.0.clone();
    ClaudeOAuthLimitsSource::new(&config, live)
        .with_endpoint(&stub.url)
        .with_clock(move || *time.lock().unwrap())
}

fn usage_body(five_hour: f64, seven_day: f64) -> String {
    json!({
        "five_hour": {"utilization": five_hour, "resets_at": "2026-10-28T22:30:00.943648+00:00"},
        "seven_day": {"utilization": seven_day, "resets_at": "2026-11-03T04:00:00+03:00"}
    })
    .to_string()
}

fn observed(observations: &[LimitObservation]) -> Vec<(String, i32, u32, u64)> {
    observations
        .iter()
        .map(|o| {
            assert!(o.observed_machine_ms > 0);
            (
                o.provider.clone(),
                o.window,
                o.used_permille,
                o.resets_at_ms,
            )
        })
        .collect()
}

#[tokio::test]
async fn oauth_poll_parses_windows_from_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = credentials(dir.path(), FAKE_TOKEN, FAR_FUTURE_MS);
    // Формы ответа из тестов CodexBar для той же точки: проценты, ISO-8601 с `Z` или смещением.
    let stub = Stub::start(vec![
        (
            200,
            json!({
                "five_hour": {"utilization": 12.5, "resets_at": "2025-12-25T12:00:00.000Z"},
                "seven_day": {"utilization": 30},
                "seven_day_opus": {"utilization": 99.0, "resets_at": null},
                "seven_day_sonnet": null
            })
            .to_string(),
        ),
        (
            200,
            json!({
                "five_hour": {"utilization": 0.5, "resets_at": "2026-07-03T00:30:00.282668+00:00"},
                "seven_day": {"utilization": null, "resets_at": "2026-07-08T00:30:00+00:00"}
            })
            .to_string(),
        ),
    ])
    .await;
    let clock = TestClock::new();
    let source = source(&path, &stub, &clock, LiveLimits::default());

    assert!(source.poll("codex").await.is_empty());
    assert!(source.poll_idle("codex").await.is_empty());
    assert_eq!(stub.count(), 0);

    assert_eq!(
        observed(&source.poll_idle("claude").await),
        vec![
            (
                "claude".to_owned(),
                ProtoWindow::FiveHour as i32,
                125,
                1_766_664_000_000,
            ),
            // Без `resets_at` сброс неизвестен.
            ("claude".to_owned(), ProtoWindow::Weekly as i32, 300, 0),
        ]
    );
    let request = stub.requests.lock().unwrap()[0].to_lowercase();
    assert!(request.starts_with("get /api/oauth/usage http/1.1\r\n"));
    assert!(request.contains(&format!(
        "authorization: bearer {}\r\n",
        FAKE_TOKEN.to_lowercase()
    )));
    assert!(request.contains("anthropic-version: oauth-2025-04-20\r\n"));

    // 0.5 — это полпроцента; окно без `utilization` пропускается.
    clock.advance(HALF_HOUR);
    assert_eq!(
        observed(&source.poll_idle("claude").await),
        vec![(
            "claude".to_owned(),
            ProtoWindow::FiveHour as i32,
            5,
            1_783_038_600_282,
        )]
    );
    assert_eq!(stub.count(), 2);
}

#[tokio::test]
async fn oauth_poll_respects_interval_and_backs_off_after_429() {
    let dir = tempfile::tempdir().unwrap();
    let path = credentials(dir.path(), FAKE_TOKEN, FAR_FUTURE_MS);
    let ok = || (200, usage_body(10.0, 20.0));
    let limited = || (429, r#"{"error":"rate_limited"}"#.to_owned());
    let stub = Stub::start(vec![ok(), limited(), limited(), limited(), limited(), ok()]).await;
    let live = LiveLimits::default();
    // Запись без машинного времени свежей не считается.
    live.note_observation(&LimitObservation {
        provider: "claude".into(),
        ..Default::default()
    });
    assert_eq!(live.last("claude"), None);
    // Живое исполнение только что прислало лимиты (так отмечает пересылка SJ).
    live.note_observation(&LimitObservation {
        provider: "claude".into(),
        observed_machine_ms: canterlot_worker::machine::now_ms(),
        ..Default::default()
    });
    let clock = TestClock::new();
    let source = source(&path, &stub, &clock, live.clone());
    let start = clock.now();
    let at = |offset: Duration| {
        let target = start + offset;
        let now = clock.now();
        assert!(target >= now);
        clock.advance(target - now);
    };
    let i = HALF_HOUR;

    // Показание свежее интервала: опрос не нужен.
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 0);

    at(i);
    assert_eq!(source.poll_idle("claude").await.len(), 2);
    at(2 * i - Duration::from_secs(1));
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 1, "second poll inside the interval");

    // Первый 429: пауза удваивается от интервала.
    at(2 * i);
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 2);
    at(4 * i - Duration::from_secs(1));
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 2);
    at(4 * i);
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 3);
    // Третий 429 подряд упирается в потолок 4 часа и дальше не растёт.
    at(8 * i);
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 4);
    let cap = Duration::from_secs(4 * 3600);
    at(8 * i + cap);
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 5);
    at(8 * i + 2 * cap - Duration::from_secs(1));
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 5);
    at(8 * i + 2 * cap);
    assert_eq!(source.poll_idle("claude").await.len(), 2);
    assert_eq!(stub.count(), 6);
    // Успех сбрасывает паузу к обычному интервалу.
    at(8 * i + 2 * cap + i - Duration::from_secs(1));
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 6);
}

#[tokio::test]
async fn hub_requested_poll_bypasses_idle_interval_but_honours_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let path = credentials(dir.path(), FAKE_TOKEN, FAR_FUTURE_MS);
    let ok = || (200, usage_body(10.0, 20.0));
    let limited = || (429, r#"{"error":"rate_limited"}"#.to_owned());
    let stub = Stub::start(vec![ok(), ok(), limited(), ok()]).await;
    let clock = TestClock::new();
    let live = LiveLimits::default();
    let source = source(&path, &stub, &clock, live.clone());
    let start = clock.now();
    let at = |offset: Duration| {
        let target = start + offset;
        let now = clock.now();
        assert!(target >= now);
        clock.advance(target - now);
    };
    let s = Duration::from_secs;
    let i = HALF_HOUR;

    assert_eq!(source.poll_idle("claude").await.len(), 2);
    // Проверка пробуждения хаба не ждёт интервала простоя, но не чаще раза в 60 с.
    at(s(59));
    assert!(source.poll("claude").await.is_empty());
    assert_eq!(stub.count(), 1, "debounced within 60 s");
    at(s(60));
    assert_eq!(source.poll("claude").await.len(), 2);
    assert_eq!(stub.count(), 2);
    // Свежее показание живого исполнения запрос хаба тоже не останавливает.
    live.note("claude", clock.now());
    at(s(120));
    assert!(source.poll_idle("claude").await.is_empty());
    assert_eq!(stub.count(), 2, "idle poll keeps its interval");
    assert!(source.poll("claude").await.is_empty());
    assert_eq!(stub.count(), 3, "429");
    // Пауза после 429 действует и на запросы хаба.
    at(s(120) + 2 * i - s(1));
    assert!(source.poll("claude").await.is_empty());
    assert_eq!(stub.count(), 3);
    at(s(120) + 2 * i);
    assert_eq!(source.poll("claude").await.len(), 2);
    assert_eq!(stub.count(), 4);
}

#[tokio::test]
async fn expired_token_skips_request_and_never_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let path = credentials(dir.path(), FAKE_TOKEN, 1_000);
    let before = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let stub = Stub::start(vec![(200, usage_body(10.0, 20.0))]).await;
    let clock = TestClock::new();
    let source = source(&path, &stub, &clock, LiveLimits::default());

    assert!(source.poll_idle("claude").await.is_empty());
    clock.advance(HALF_HOUR);
    assert!(source.poll("claude").await.is_empty());
    assert_eq!(source.fetch().await.unwrap_err(), OAuthPollError::Expired);
    assert_eq!(stub.count(), 0, "no request with an expired token");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

    fs::write(&path, r#"{"other":{}}"#).unwrap();
    assert_eq!(
        source.fetch().await.unwrap_err(),
        OAuthPollError::Credentials
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        source.fetch().await.unwrap_err(),
        OAuthPollError::Credentials
    );
    assert_eq!(stub.count(), 0);
}

#[tokio::test]
async fn token_never_appears_in_debug_or_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = credentials(dir.path(), FAKE_TOKEN, FAR_FUTURE_MS);
    let echo = format!(r#"{{"error":"bad token {FAKE_TOKEN}"}}"#);
    let stub = Stub::start(vec![
        (401, echo.clone()),
        (500, echo.clone()),
        (200, format!("not json {FAKE_TOKEN}")),
        (429, echo),
    ])
    .await;
    let clock = TestClock::new();
    let source = source(&path, &stub, &clock, LiveLimits::default());

    let mut errors = Vec::new();
    for _ in 0..4 {
        errors.push(source.fetch().await.unwrap_err());
    }
    assert_eq!(
        errors,
        vec![
            OAuthPollError::Rejected(401),
            OAuthPollError::Status(500),
            OAuthPollError::Body,
            OAuthPollError::RateLimited,
        ]
    );
    // Токен действительно уходил в запрос, но только туда.
    assert_eq!(stub.count(), 4);
    assert!(stub.requests.lock().unwrap()[0].contains(FAKE_TOKEN));

    fs::write(
        &path,
        format!(r#"{{"claudeAiOauth":{{"accessToken":"{FAKE_TOKEN}","expiresAt":1}}}}"#),
    )
    .unwrap();
    errors.push(source.fetch().await.unwrap_err());
    fs::write(&path, format!("{{broken {FAKE_TOKEN}")).unwrap();
    errors.push(source.fetch().await.unwrap_err());
    assert_eq!(
        &errors[4..],
        &[OAuthPollError::Expired, OAuthPollError::Credentials]
    );

    let mut text = format!("{source:?}");
    for error in &errors {
        text.push_str(&format!("{error} {error:?} "));
    }
    let boxed: Box<dyn std::error::Error> = Box::new(errors[0]);
    text.push_str(&boxed.to_string());
    assert!(!text.contains(FAKE_TOKEN), "{text}");
    assert!(!text.contains("canterlot-fake-token"), "{text}");
}

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
        format!(
            "{base}\n[claude_code]\nallow_paid_model_poll = false\noauth_poll_interval_s = 60\n"
        ),
    )
    .unwrap();
    let config = canterlot_worker::config::load(&path).unwrap();
    let claude = config.claude_code.unwrap();
    assert!(!claude.allow_paid_model_poll);
    assert_eq!(claude.oauth_poll_interval(), Duration::from_secs(900));

    fs::write(&path, format!("{base}\n[claude_code]\n")).unwrap();
    let claude = canterlot_worker::config::load(&path)
        .unwrap()
        .claude_code
        .unwrap();
    assert!(!claude.allow_paid_model_poll);
    assert_eq!(claude.oauth_poll_interval(), HALF_HOUR);
    assert_eq!(claude.credentials_path, None);
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

async fn next_limits_report(hub: &mut support::ProtocolHub) -> Vec<LimitObservation> {
    loop {
        if let support::Observed::Control(message) = hub.event().await
            && let Some(Msg::LimitsReport(report)) = message.msg
        {
            return report.observations;
        }
    }
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
        hub.config.claude_code = Some(ClaudeCodeConfig {
            oauth_poll_interval_s: 900,
            ..ClaudeCodeConfig::default()
        });
        let interval = Duration::from_secs(900);
        let source = Arc::new(CountingSource::default());
        let worker = Worker::new(hub.config.clone(), store).with_limits_source(source.clone());
        let started = Instant::now();
        let mut state = worker.state();
        let (stop, shutdown) = watch::channel(false);
        let join = tokio::spawn(worker.run(shutdown));
        state.wait_for(|s| *s == SessionState::Live).await.unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);

        tokio::time::advance(interval - Duration::from_secs(1)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        assert!(
            source.calls().is_empty(),
            "first tick one interval after start"
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
