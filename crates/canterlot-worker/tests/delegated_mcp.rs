//! D24: делегированная сессия не грузит MCP-серверы владельца ни в одном адаптере.
//! Здесь проверки без настоящих CLI; с настоящими бинарниками их повторяет
//! `delegated_mcp_live.rs`.

use canterlot_worker::adapter::claude_code::process::{
    LaunchSpec, SessionArg, apply_mcp_isolation, build_args, build_env, write_exec_files,
};
use canterlot_worker::adapter::{InstructionMode, InstructionSpec};
use canterlot_worker::config::ClaudeCodeConfig;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

fn instructions(mode: InstructionMode) -> InstructionSpec {
    InstructionSpec {
        mode,
        text: "text".into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn claude_args(td: &TempDir, isolate_mcp: bool, mode: InstructionMode) -> Vec<String> {
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&exec_dir).unwrap();
    fs::create_dir_all(&workdir).unwrap();
    let instructions = instructions(mode);
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp,
    };
    build_args(&spec, &ClaudeCodeConfig::default())
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn claude_delegated_session_loads_only_the_canterlot_config() {
    for mode in [
        InstructionMode::Owner,
        InstructionMode::Clean,
        InstructionMode::Preset,
    ] {
        let td = TempDir::new().unwrap();
        let isolated = claude_args(&td, true, mode);
        assert!(
            isolated.iter().any(|arg| arg == "--strict-mcp-config"),
            "{mode:?}"
        );
        assert!(isolated.iter().any(|arg| arg == "--mcp-config"));
        // Плагины, навыки и настройки пресета остаются: флаг трогает только MCP.
        assert!(!isolated.iter().any(|arg| arg == "--bare"));
        assert!(!isolated.iter().any(|arg| arg == "--setting-sources"));
        let owner_started = claude_args(&td, false, mode);
        assert!(
            !owner_started.iter().any(|arg| arg == "--strict-mcp-config"),
            "{mode:?}: an owner-started session changes in no way"
        );
        let without_flag: Vec<_> = isolated
            .iter()
            .filter(|arg| *arg != "--strict-mcp-config")
            .collect();
        assert_eq!(
            without_flag,
            owner_started.iter().collect::<Vec<_>>(),
            "{mode:?}"
        );
    }
}

#[test]
fn claude_mcp_config_names_only_canterlot() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let instructions = instructions(InstructionMode::Clean);
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp: true,
    };
    write_exec_files(
        &spec,
        &ClaudeCodeConfig::default(),
        Path::new("/usr/bin/relay"),
    )
    .unwrap();
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(exec_dir.join("mcp.json")).unwrap()).unwrap();
    let names: Vec<_> = config["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["canterlot"]);
}

#[test]
fn claude_account_connectors_are_switched_off_over_any_inherited_value() {
    let parent = [
        ("PATH", "/usr/bin"),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "true"),
    ]
    .map(|(key, value)| (OsString::from(key), OsString::from(value)));
    let mut env = build_env(parent.into_iter());
    apply_mcp_isolation(&mut env);
    let values: Vec<_> = env
        .iter()
        .filter(|(key, _)| key == "ENABLE_CLAUDEAI_MCP_SERVERS")
        .collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].1, "false");
}
