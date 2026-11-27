use canterlot_hub::grpc::{KNOWN_CAPABILITIES, KNOWN_OPTIONAL_CAPABILITIES};
use canterlot_worker::session::OPTIONAL_CAPABILITIES;

/// Хаб сохраняет у машины только знакомые ему возможности: предложенная воркером, но не
/// внесённая в список хаба молча пропадает из `machines.capabilities`, и функция не
/// включается (так 27.11 на проде не включилась проба слияния `git_merge_probe_v1`).
#[test]
fn every_offered_capability_is_known_to_the_hub() {
    let unknown: Vec<&str> = OPTIONAL_CAPABILITIES
        .iter()
        .copied()
        .filter(|c| !KNOWN_OPTIONAL_CAPABILITIES.contains(c) && !KNOWN_CAPABILITIES.contains(c))
        .collect();
    assert!(
        unknown.is_empty(),
        "the hub drops these capabilities: {unknown:?}"
    );
}

#[test]
fn capabilities_prints_optional_capabilities_and_production_harnesses() {
    let empty = tempfile::TempDir::new().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .arg("capabilities")
        .current_dir(empty.path())
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with('\n'));

    let mut capabilities = Vec::new();
    let mut harnesses = Vec::new();
    for line in stdout.lines() {
        match line.split_once(' ') {
            Some(("capability", name)) if !name.is_empty() && !name.contains(' ') => {
                assert!(harnesses.is_empty(), "capability после harness: {line}");
                capabilities.push(name);
            }
            Some(("harness", name)) if !name.is_empty() && !name.contains(' ') => {
                harnesses.push(name);
            }
            _ => panic!("неожиданная строка: {line:?}"),
        }
    }
    assert_eq!(capabilities, OPTIONAL_CAPABILITIES);
    assert!(capabilities.contains(&"claude_histories_v1"));
    assert!(capabilities.contains(&"path_probe_v1"));
    assert!(capabilities.contains(&"resource_lease_v1"));
    assert!(capabilities.contains(&"watchdog_v1"));
    assert_eq!(harnesses, ["claude_code", "codex", "opencode"]);
    assert!(std::fs::read_dir(empty.path()).unwrap().next().is_none());
}

#[test]
fn capabilities_is_listed_in_help() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line.trim_start().starts_with("capabilities")),
        "{stdout}"
    );
}
