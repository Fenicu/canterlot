#![forbid(unsafe_code)]
// Помощники с тестами (process_guard, pilot, attachments) остаются локальными копиями
// своих модулей, как были отдельными бинарниками: так сохраняются их имена тестов.
#![allow(clippy::duplicate_mod)]

mod support;

#[cfg(feature = "synthetic-adapter")]
#[path = "support/antigravity.rs"]
mod antigravity;
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
#[cfg(feature = "synthetic-adapter")]
#[path = "support/trace.rs"]
mod trace;

#[cfg(feature = "synthetic-adapter")]
mod adapter;
#[cfg(feature = "synthetic-adapter")]
mod antigravity_adapter;
mod antigravity_config;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod antigravity_e2e;
#[cfg(feature = "synthetic-adapter")]
mod antigravity_limits;
mod attachments;
mod canterlot_mcp_presets;
mod canterlot_mcp_sessions;
mod capabilities_cli;
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
mod git_facts;
mod instructions;
mod lease;
mod lease_scripts;
mod machine;
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
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod pilot_e2e;
mod prune_target_script;
mod session;
#[cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]
mod session_tools_e2e;
mod spec_compat;
mod store;
#[cfg(feature = "synthetic-adapter")]
mod supervisor;
#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
mod supervisor_requests;
mod version;
mod version_probe;
mod worktree;
