#![forbid(unsafe_code)]

//! `/usage` Antigravity на `fake-agy`: закреплённый бинарник, cwd = data_dir, без хода.

use canterlot_proto::worker::v1::{ControlDown, LimitWindow, PollLimits, control_down};
use canterlot_worker::{
    adapter::antigravity::{limits::AntigravityLimitsSource, replay::Script},
    config::AntigravityConfig,
    machine::limits::{LimitsSource, LiveLimits},
    session::{SessionState, Worker},
};
use serde_json::Value;
use std::{path::Path, time::Duration};
use tokio::sync::watch;

use crate::antigravity;

#[allow(dead_code)]
use crate::support;

fn usage_json() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/antigravity/usage-before.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("usage evidence"))
        .expect("usage json")
}

#[tokio::test]
async fn usage_runs_the_pinned_copy_in_the_data_dir_without_a_turn() {
    let fake = antigravity::Fake::new(Script {
        usage: Some(usage_json()),
        ..Default::default()
    });
    let cfg = AntigravityConfig {
        enabled: true,
        binary: Some(fake.exe.clone()),
        data_dir: Some(fake.data_dir()),
    };
    let source = AntigravityLimitsSource::new(cfg, Duration::ZERO, LiveLimits::default());
    let observations = source.fetch().await.expect("usage");
    assert_eq!(observations.len(), 2);
    assert!(observations.iter().all(|o| o.provider == "antigravity"));
    let weekly = observations
        .iter()
        .find(|o| o.window == LimitWindow::Weekly as i32)
        .expect("weekly");
    assert_eq!(weekly.used_permille, 990);
    let five = observations
        .iter()
        .find(|o| o.window == LimitWindow::FiveHour as i32)
        .expect("five hour");
    assert_eq!(five.used_permille, 0);

    let entries = fake.entries();
    let usage = entries
        .iter()
        .find(|entry| {
            entry["boot"] == true
                && entry["args"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|arg| arg == "/usage"))
        })
        .expect("usage boot");
    let mut args: Vec<&str> = usage["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // Двойник запускается обёрткой `fake-agy --script <файл>`; настоящая копия
    // получает только аргументы ниже.
    if args.first() == Some(&"--script") {
        args.drain(..2);
    }
    // Точный argv, принятый `agy` 1.5.0: `--disable-telemetry` у `-p` даёт
    // «flags provided but not defined» и статус 2 (журнал 2026-11-04).
    assert_eq!(args, ["-p", "/usage", "--output-format", "json"]);
    assert_eq!(
        usage["cwd"].as_str(),
        Some(fake.data_dir().to_string_lossy().as_ref())
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry["frame"]["method"] != "session/prompt")
    );
    fake.wait_gone().await;
}

#[tokio::test]
async fn a_disabled_profile_never_starts_a_usage_probe() {
    let fake = antigravity::Fake::new(Script {
        usage: Some(usage_json()),
        ..Default::default()
    });
    let (hub, store) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["pilot_v1".into()],
        ..Default::default()
    })
    .await;
    let mut config = hub.config.clone();
    config.antigravity = Some(AntigravityConfig {
        enabled: false,
        binary: Some(fake.exe.clone()),
        data_dir: Some(fake.data_dir()),
    });
    let worker =
        Worker::new(config.clone(), store).with_runtime_dir(config.data_dir.join("runtime"));
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    state
        .wait_for(|state| *state == SessionState::Live)
        .await
        .unwrap();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::PollLimits(PollLimits {
            provider: "antigravity".into(),
            owner_refresh: false,
        })),
    })
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let entries = fake.entries();
    assert!(
        entries.is_empty(),
        "a disabled profile must not run agy: {entries:?}"
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_foreign_provider_is_not_polled() {
    let fake = antigravity::Fake::new(Script {
        usage: Some(usage_json()),
        ..Default::default()
    });
    let cfg = AntigravityConfig {
        enabled: true,
        binary: Some(fake.exe.clone()),
        data_dir: Some(fake.data_dir()),
    };
    let source = AntigravityLimitsSource::new(cfg, Duration::ZERO, LiveLimits::default());
    assert!(source.poll("codex").await.is_empty());
    assert!(source.poll("antigravity").await.len() == 2);
    fake.wait_gone().await;
}

/// Живая проба `/usage` закреплённой копией `agy` 1.5.0: точный argv без
/// `--disable-telemetry`. Игнорируется: нужна настоящая копия и вход владельца.
#[tokio::test]
#[ignore]
async fn live_usage_probe_accepts_the_argv() {
    let home = std::env::var("HOME").expect("HOME");
    let pinned = Path::new(&home).join(".local/state/canterlot-worker/antigravity/bin/agy-1.5.0");
    let data_dir = tempfile::TempDir::new().unwrap();
    let source = AntigravityLimitsSource::new(
        AntigravityConfig {
            enabled: true,
            binary: Some(pinned),
            data_dir: Some(data_dir.path().to_path_buf()),
        },
        Duration::ZERO,
        LiveLimits::default(),
    );
    let observations = source.fetch().await.expect("the /usage probe must run");
    assert_eq!(observations.len(), 2, "{observations:?}");
    assert!(observations.iter().all(|o| o.provider == "antigravity"));
}
