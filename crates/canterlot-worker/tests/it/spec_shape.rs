//! Форма spec.toml, которую пишет ТЕКУЩИЙ воркер, обязана совпадать с известной фикстурой.
//!
//! Репетиция выкатки проверяет живые спеки через scripts/deploy/spec-shape.py. Если релиз
//! начал писать новую обязательную кнопку, а фикстуру никто не добавил, репетиция
//! упадёт уже на станции. Этот тест прогоняет тот же скрипт над спеками, собранными
//! production-сборщиком `exec::supervisor_spec_toml`, чтобы поломка нашлась обычным
//! `scripts/check.sh --fast`, а не в день выкатки.

use canterlot_worker::{
    config::{ClaudeCodeConfig, CodexConfig, OpenCodeConfig, SupervisorTimers},
    exec::{SupervisorSpecParams, supervisor_spec_toml},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/it/fixtures/spec");

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(FIXTURES)
        .unwrap_or_else(|e| panic!("read {FIXTURES}: {e}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no *.toml fixtures in {FIXTURES}");
    paths
}

/// Собирает спеку тем же кодом, что воркер, и гоняет её через тот же скрипт, что gate 2.
fn matches_a_fixture(
    case: &str,
    agent_nice: u32,
    claude_code: Option<ClaudeCodeConfig>,
    codex: Option<CodexConfig>,
    opencode: Option<OpenCodeConfig>,
    delegate_mcp_servers: &[String],
) {
    let root = tempfile::Builder::new()
        .prefix("spec-shape-")
        .tempdir_in("/tmp")
        .unwrap();
    let data_dir = root.path().join("data");
    fs::create_dir(&data_dir).unwrap();
    let timers = SupervisorTimers::default();
    let spec = supervisor_spec_toml(&SupervisorSpecParams {
        execution_id: Uuid::now_v7(),
        generation: 1,
        conversation_id: Uuid::now_v7(),
        agent_nice,
        timers: &timers,
        data_dir: &data_dir,
        runtime_dir: None,
        claude_code,
        codex,
        opencode,
        delegate_mcp_servers,
    })
    .unwrap();
    let spec_path = root.path().join("spec.toml");
    fs::write(&spec_path, &spec).unwrap();

    let script = Path::new(ROOT).join("scripts/deploy/spec-shape.py");
    let mut command = Command::new("python3");
    command
        .arg("-I")
        .arg(&script)
        .arg(Path::new(ROOT).join("crates/canterlot-worker/src/supervisor/mod.rs"))
        .arg(Path::new(ROOT).join("crates/canterlot-worker/src/config.rs"))
        .arg(&spec_path);
    for fixture in fixtures() {
        command.arg(fixture);
    }
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("run spec-shape.py: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("MATCH"),
        "case {case}: the shape the worker writes matches no spec fixture.\n\
         Add a fixture of this shape (named like the existing ones) to \
         crates/canterlot-worker/tests/it/fixtures/spec/ and rerun this test.\n\
         --- generated spec.toml ---\n{spec}\n\
         --- spec-shape.py stdout ---\n{stdout}\n\
         --- spec-shape.py stderr ---\n{stderr}"
    );
}

fn opencode() -> OpenCodeConfig {
    OpenCodeConfig {
        default_model: Some("opencode-go/fixture".into()),
        ..Default::default()
    }
}

#[test]
fn all_harnesses_match_a_fixture() {
    matches_a_fixture(
        "all harnesses",
        10,
        Some(ClaudeCodeConfig::default()),
        Some(CodexConfig::default()),
        Some(opencode()),
        &[],
    );
}

#[test]
fn optional_sections_do_not_change_the_shape() {
    let mut opencode = opencode();
    opencode
        .provider_models
        .insert("opencode-go".into(), vec!["fixture".into()]);
    let codex = CodexConfig {
        home_dir: Some(PathBuf::from("/data/codex-home")),
        ..Default::default()
    };
    matches_a_fixture(
        "provider models, codex home, delegate MCP, agent_nice = 0",
        0,
        Some(ClaudeCodeConfig::default()),
        Some(codex),
        Some(opencode),
        &["readonly".into(), "notes".into()],
    );
}

#[test]
fn opencode_without_default_model_matches_a_fixture() {
    matches_a_fixture(
        "opencode without default_model",
        10,
        Some(ClaudeCodeConfig::default()),
        Some(CodexConfig::default()),
        Some(OpenCodeConfig::default()),
        &[],
    );
}

#[test]
fn each_harness_alone_matches_a_fixture() {
    matches_a_fixture(
        "claude_code only",
        10,
        Some(ClaudeCodeConfig::default()),
        None,
        None,
        &[],
    );
    matches_a_fixture(
        "codex only",
        10,
        None,
        Some(CodexConfig::default()),
        None,
        &[],
    );
    matches_a_fixture("opencode only", 10, None, None, Some(opencode()), &[]);
    matches_a_fixture(
        "opencode only, no default_model",
        10,
        None,
        None,
        Some(OpenCodeConfig::default()),
        &[],
    );
}
