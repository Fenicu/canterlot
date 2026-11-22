#![forbid(unsafe_code)]
// Помощники с тестами (process_guard, pilot, attachments) остаются локальными копиями
// своих модулей, как были отдельными бинарниками: так сохраняются их имена тестов.
#![allow(clippy::duplicate_mod)]

mod support;

// Помощник с проверкой чистки процессов включается один раз на весь бинарник:
// локальные копии модуля нужны только самим сценариям.
#[path = "support/process_guard.rs"]
mod process_guard;

#[test]
fn process_guard_cleans_orphaned_cli_on_panic() {
    let root = tempfile::tempdir().unwrap();
    let check = process_guard::ProcessGuard::new(root.path());
    let result = std::panic::catch_unwind(|| {
        let _guard = process_guard::ProcessGuard::new(root.path());
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
            .args(["synthetic-cli", "--profile", "__detach_session"])
            .current_dir(root.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!check.members().is_empty());
        panic!("exercise process cleanup during unwind");
    });
    assert!(result.is_err());
    assert!(check.members().is_empty(), "orphan survived guard unwind");
}

#[path = "support/fake_exec.rs"]
mod fake_exec;
#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/frozen_clock.rs"]
mod frozen_clock;
#[cfg(feature = "synthetic-adapter")]
#[path = "support/opencode.rs"]
mod opencode;
#[cfg(feature = "opencode-live")]
#[path = "support/opencode_stub.rs"]
mod opencode_stub;
#[path = "support/sshd.rs"]
mod sshd;
#[cfg(feature = "synthetic-adapter")]
#[path = "support/trace.rs"]
mod trace;

#[cfg(feature = "synthetic-adapter")]
mod adapter;
mod attachments;
mod canterlot_mcp_presets;
mod canterlot_mcp_sessions;
mod capabilities_cli;
mod carrier;
mod carrier_commands;
mod carrier_hub;
mod carrier_stage;
mod claude_code_containment;
mod claude_code_e2e;
mod claude_code_input;
mod claude_code_large_result;
mod claude_code_limits;
mod claude_code_mcp;
mod claude_code_media;
mod claude_code_parser;
mod claude_code_presets;
mod claude_code_process;
mod claude_code_restart_stuck;
mod claude_ignored;
mod codex_containment;
mod codex_e2e;
mod codex_events;
mod codex_home;
mod codex_input;
mod codex_limits;
mod codex_mcp;
mod codex_presets;
mod codex_process;
mod codex_requests;
mod codex_review_findings;
mod codex_version_reap;
mod codexbar_limits;
mod codexbar_logging;
#[cfg(feature = "synthetic-adapter")]
mod containment;
mod delegated_mcp;
mod delegated_mcp_live;
#[cfg(feature = "fault-injection")]
mod e2e;
#[cfg(feature = "synthetic-adapter")]
mod exec;
#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
mod exec_attachments;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod exec_e2e;
#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
mod exec_media;
mod fatal_exit;
mod git_facts;
mod hub_dial;
mod instructions;
mod jobs;
mod lease;
mod lease_scripts;
mod limit_account;
mod machine;
mod memory_import;
#[cfg(feature = "synthetic-adapter")]
mod opencode_adapter;
mod opencode_config;
#[cfg(feature = "synthetic-adapter")]
mod opencode_e2e;
#[cfg(feature = "synthetic-adapter")]
mod opencode_events;
#[cfg(feature = "opencode-live")]
mod opencode_live;
mod opencode_process;
#[cfg(feature = "synthetic-adapter")]
mod opencode_review_findings;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod opencode_worker;
mod owner_jobs;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod pilot_e2e;
mod precheck;
mod prune_target_script;
mod secrets_guard;
mod session;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod session_tools_e2e;
mod spec_compat;
mod spec_shape;
mod store;
#[cfg(feature = "synthetic-adapter")]
mod supervisor;
#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
mod supervisor_requests;
mod version;
mod version_probe;
mod wake_turn_e2e;
mod worktree;
