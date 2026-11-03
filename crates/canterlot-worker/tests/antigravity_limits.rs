#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! `/usage` Antigravity на `fake-agy`: закреплённый бинарник, cwd = data_dir, без хода.

use canterlot_proto::worker::v1::LimitWindow;
use canterlot_worker::{
    adapter::antigravity::{limits::AntigravityLimitsSource, replay::Script},
    config::AntigravityConfig,
    machine::limits::{LimitsSource, LiveLimits},
};
use serde_json::Value;
use std::{path::Path, time::Duration};

#[path = "support/antigravity.rs"]
mod antigravity;

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
    let args: Vec<&str> = usage["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(args.contains(&"-p"));
    assert!(args.contains(&"/usage"));
    assert!(args.contains(&"--output-format"));
    assert!(args.contains(&"json"));
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
