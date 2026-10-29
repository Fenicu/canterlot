#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{path::PathBuf, time::Duration};

use canterlot_worker::{
    adapter::{
        Adapter, AdapterEvent, BlockKind, CloseMode, InstructionMode, InstructionSpec,
        InterruptOutcome, StartSpec, Transmission, TransmitOutcome, registry,
        synthetic::SyntheticAdapter,
    },
    containment::{Containment, Termination, TreeContainment},
};
use uuid::Uuid;

#[path = "support/process_guard.rs"]
mod process_guard;

fn isolated(name: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "adapter_child", "--nocapture"])
        .env("CANTERLOT_ADAPTER_CASE", name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

macro_rules! cases {
    ($($name:ident),* $(,)?) => {$ (
        #[test]
        fn $name() { isolated(stringify!($name)); }
    )*};
}

cases!(
    synthetic_streams_scripted_deltas_in_order,
    acceptance_requires_the_cli_frame_not_the_write,
    unknown_optional_event_is_diagnosed_not_fatal,
    unknown_required_event_is_reported_required,
    interrupt_keeps_process_by_default_and_exits_in_exit_profile,
    resume_keeps_native_id,
    error_script_yields_error_block_and_run_finished,
    deltas_are_chunked_to_the_bound,
    registry_knows_only_synthetic_profiles,
    next_event_is_cancel_safe,
    debug_redacts_payloads,
    tool_result_links_to_call,
    plain_text_echoes_three_utf8_deltas,
    exit_script_reports_code,
    one_adapter_never_starts_a_second_cli,
    oversized_input_is_rejected_before_writing,
    capabilities_describe_profiles,
    wave_w_member_signals_use_supplied_containment,
    blocks_carry_agent_and_model,
    model_step_switches_model_for_following_blocks,
    subagent_step_emits_child_blocks_with_parent_agent_and_model,
    subagent_call_block_carries_task_summary_and_children_carry_the_name,
    usage_step_accumulates_per_model_and_agent,
    default_usage_is_derived_from_text,
    limit_steps_emit_observed_and_reached_with_resets_at,
    context_exhausted_step_ends_the_turn_with_the_marker,
    settings_in_transmission_change_the_model_before_the_turn,
    unsupported_setting_is_rejected_before_any_byte_reaches_the_cli,
    read_attachment_step_echoes_the_file_sha256,
    catalog_lists_synthetic_harnesses_with_models_and_efforts,
    synthetic_codex_profile_has_provider_codex,
);

async fn event(adapter: &mut SyntheticAdapter) -> AdapterEvent {
    tokio::time::timeout(Duration::from_secs(5), adapter.next_event())
        .await
        .unwrap()
}

#[tokio::test]
async fn adapter_child() {
    let Ok(case) = std::env::var("CANTERLOT_ADAPTER_CASE") else {
        return;
    };
    let group = ObservedContainment {
        tree: TreeContainment::new().unwrap(),
        signals: std::sync::atomic::AtomicUsize::new(0),
    };
    let directory = tempfile::tempdir().unwrap();
    let _guard = process_guard::ProcessGuard::new(directory.path());
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: directory.path().into(),
        native_conversation_id: None,
        profile: "synthetic".into(),
        model: if case == "blocks_carry_agent_and_model" {
            "synthetic-small"
        } else {
            "synthetic-large"
        }
        .into(),
        effort: "medium".into(),
        permission_mode: "owner".into(),
        instructions: InstructionSpec {
            mode: InstructionMode::Owner,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        },
    };
    let make = |profile: &str| {
        SyntheticAdapter::new(profile)
            .with_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker")))
            .with_transmit_deadline(Duration::from_millis(250))
    };
    let mut adapter = make(if case == "synthetic_codex_profile_has_provider_codex" {
        "synthetic-codex"
    } else {
        "synthetic"
    });
    let started = adapter.start(&spec, &group).await.unwrap();
    assert!(started.native_conversation_id.starts_with("synthetic-"));
    assert!(
        group
            .members()
            .unwrap()
            .iter()
            .any(|m| m.pid == started.cli_pid && m.start_ticks == started.cli_start_ticks)
    );
    let input = |text: &str| Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: text.into(),
        files: vec![],
        settings: None,
    };

    match case.as_str() {
        "wave_w_member_signals_use_supplied_containment" => {
            assert!(matches!(
                adapter
                    .transmit(
                        &input(r#"{"synthetic":[{"delta":"x"},{"hang":true}]}"#),
                        &group
                    )
                    .await,
                TransmitOutcome::Accepted(_)
            ));
            adapter.close(CloseMode::Force, &group).await.unwrap();
            assert!(group.signals.load(std::sync::atomic::Ordering::Relaxed) > 0);
        }
        "capabilities_describe_profiles" => {
            use canterlot_worker::adapter::Support;
            let capabilities = adapter.capabilities();
            assert_eq!(capabilities.streaming, Support::Supported);
            assert_eq!(capabilities.resume, Support::Supported);
            assert_eq!(capabilities.interrupt, Support::Supported);
            assert_eq!(capabilities.approvals, Support::Supported);
            assert_eq!(capabilities.questions, Support::Supported);
            assert!(capabilities.interrupt_keeps_process);
            assert!(
                !make("synthetic-exit-on-interrupt")
                    .capabilities()
                    .interrupt_keeps_process
            );
        }
        "oversized_input_is_rejected_before_writing" => {
            assert!(matches!(
                adapter.transmit(&input(&"x".repeat(262145)), &group).await,
                TransmitOutcome::Rejected(_)
            ));
            assert!(matches!(
                adapter.transmit(&input("valid"), &group).await,
                TransmitOutcome::Accepted(_)
            ));
        }
        "one_adapter_never_starts_a_second_cli" => {
            assert!(adapter.start(&spec, &group).await.is_err());
            let mut second = make("synthetic");
            assert!(second.start(&spec, &group).await.is_err());
            assert_eq!(group.members().unwrap().len(), 1);
        }
        "exit_script_reports_code" => {
            assert!(matches!(
                adapter
                    .transmit(&input(r#"{"synthetic":[{"exit":7}]}"#), &group)
                    .await,
                TransmitOutcome::Undetermined
            ));
            loop {
                if let AdapterEvent::ProcessExited { code, signal } = event(&mut adapter).await {
                    assert_eq!(code, Some(7));
                    assert_eq!(signal, None);
                    break;
                }
            }
        }
        "registry_knows_only_synthetic_profiles" => {
            assert!(registry("synthetic").is_some());
            assert!(registry("synthetic-exit-on-interrupt").is_some());
            assert!(registry("codex").is_none());
            assert!(registry("other").is_none());
        }
        "debug_redacts_payloads" => {
            let secret = "private payload";
            assert!(!format!("{:?}", input(secret)).contains(secret));
            assert!(
                !format!(
                    "{:?}",
                    AdapterEvent::BlockDelta {
                        block_id: Uuid::now_v7(),
                        append: secret.into()
                    }
                )
                .contains(secret)
            );
            assert!(
                !format!(
                    "{:?}",
                    AdapterEvent::BlockFinal {
                        block_id: Uuid::now_v7(),
                        text: secret.into(),
                        summary: Some(secret.into())
                    }
                )
                .contains(secret)
            );
        }
        "acceptance_requires_the_cli_frame_not_the_write" => {
            let before = tokio::time::Instant::now();
            assert!(matches!(
                adapter
                    .transmit(
                        &input(r#"{"synthetic":[{"hang":true},{"delta":"later"}]}"#),
                        &group
                    )
                    .await,
                TransmitOutcome::Undetermined
            ));
            assert!(before.elapsed() >= Duration::from_millis(200));
            assert!(before.elapsed() < Duration::from_secs(2));
        }
        "resume_keeps_native_id" => {
            let id = started.native_conversation_id;
            adapter.close(CloseMode::Graceful, &group).await.unwrap();
            let mut resumed = make("synthetic");
            let resumed_spec = StartSpec {
                native_conversation_id: Some(id.clone()),
                ..spec
            };
            assert_eq!(
                resumed
                    .start(&resumed_spec, &group)
                    .await
                    .unwrap()
                    .native_conversation_id,
                id
            );
            resumed.close(CloseMode::Graceful, &group).await.unwrap();
        }
        "interrupt_keeps_process_by_default_and_exits_in_exit_profile" => {
            for profile in ["synthetic", "synthetic-exit-on-interrupt"] {
                adapter.close(CloseMode::Graceful, &group).await.unwrap();
                adapter = make(profile);
                let started = adapter
                    .start(
                        &StartSpec {
                            profile: profile.into(),
                            ..spec.clone()
                        },
                        &group,
                    )
                    .await
                    .unwrap();
                let transmission = input(r#"{"synthetic":[{"delta":"begin"},{"hang":true}]}"#);
                assert!(matches!(
                    adapter.transmit(&transmission, &group).await,
                    TransmitOutcome::Accepted(_)
                ));
                assert!(matches!(
                    adapter.interrupt().await,
                    InterruptOutcome::Acknowledged
                ));
                if profile == "synthetic" {
                    loop {
                        if let AdapterEvent::RunFinished {
                            input_command_id, ..
                        } = event(&mut adapter).await
                        {
                            assert_eq!(input_command_id, transmission.command_id);
                            break;
                        }
                    }
                    assert!(
                        group
                            .members()
                            .unwrap()
                            .iter()
                            .any(|m| m.pid == started.cli_pid)
                    );
                    assert!(matches!(
                        adapter.transmit(&input("second"), &group).await,
                        TransmitOutcome::Accepted(_)
                    ));
                } else {
                    loop {
                        if let AdapterEvent::ProcessExited { code, signal } =
                            event(&mut adapter).await
                        {
                            assert_eq!(code, Some(0));
                            assert_eq!(signal, None);
                            break;
                        }
                    }
                }
            }
        }
        name if PILOT_CASES.contains(&name) => {
            pilot_case(name, &mut adapter, &group, &directory, &input).await;
        }
        _ => {
            let text = match case.as_str() {
                "synthetic_streams_scripted_deltas_in_order" => r#"{"synthetic":[{"delta":"one"},{"delay_ms":5},{"delta":"два"},{"delta":"three"}]}"#.into(),
                "unknown_optional_event_is_diagnosed_not_fatal" => r#"{"synthetic":[{"unknown":{"event_type":"new_optional","required":false}},{"delta":"after"}]}"#.into(),
                "unknown_required_event_is_reported_required" => r#"{"synthetic":[{"unknown":{"event_type":"new_required","required":true}}]}"#.into(),
                "error_script_yields_error_block_and_run_finished" => r#"{"synthetic":[{"error":"failed"}]}"#.into(),
                "deltas_are_chunked_to_the_bound" => serde_json::json!({"synthetic":[{"delta":"я🦄".repeat(12000)}]}).to_string(),
                "tool_result_links_to_call" => r#"{"synthetic":[{"tool":{"name":"probe"}}]}"#.into(),
                "next_event_is_cancel_safe" => r#"{"synthetic":[{"delta":"before"},{"delay_ms":100},{"delta":"after"}]}"#.into(),
                "plain_text_echoes_three_utf8_deltas" => "один🦄два".into(),
                _ => panic!("unknown case"),
            };
            let transmission = input(&text);
            let proof = adapter.transmit(&transmission, &group).await;
            assert!(
                matches!(proof, TransmitOutcome::Accepted(ref p) if p.command_id == transmission.command_id && p.kind == "cli_accepted")
            );
            let mut deltas = Vec::new();
            let mut diagnostic = None;
            let mut error = false;
            let mut call = None;
            let mut result_parent = None;
            let mut final_text = String::new();
            loop {
                match event(&mut adapter).await {
                    AdapterEvent::BlockDelta { append, .. } => {
                        assert!(append.len() <= canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES);
                        deltas.push(append);
                    }
                    AdapterEvent::BlockStarted {
                        block_id,
                        kind,
                        parent,
                        ..
                    } => {
                        assert_eq!(block_id.get_version_num(), 7);
                        match kind {
                            BlockKind::Error => error = true,
                            BlockKind::ToolCall => call = Some(block_id),
                            BlockKind::ToolResult => result_parent = parent,
                            _ => {}
                        }
                    }
                    AdapterEvent::BlockFinal { text, .. } => final_text = text,
                    AdapterEvent::Diagnostic {
                        kind,
                        event_type,
                        required,
                    } => {
                        assert_eq!(kind, "unknown_event");
                        diagnostic = Some((event_type, required));
                    }
                    AdapterEvent::RunFinished {
                        input_command_id, ..
                    } => {
                        assert_eq!(input_command_id, transmission.command_id);
                        break;
                    }
                    AdapterEvent::ProcessExited { .. } => panic!("unexpected exit"),
                    _ => {}
                }
                if case == "next_event_is_cancel_safe" && deltas == ["before"] {
                    let _ =
                        tokio::time::timeout(Duration::from_millis(1), adapter.next_event()).await;
                }
            }
            match case.as_str() {
                "synthetic_streams_scripted_deltas_in_order" => {
                    assert_eq!(deltas, ["one", "два", "three"])
                }
                "unknown_optional_event_is_diagnosed_not_fatal" => {
                    assert_eq!(diagnostic, Some(("new_optional".into(), false)));
                    assert_eq!(deltas, ["after"]);
                }
                "unknown_required_event_is_reported_required" => {
                    assert_eq!(diagnostic, Some(("new_required".into(), true)))
                }
                "error_script_yields_error_block_and_run_finished" => {
                    assert!(error);
                    assert_eq!(final_text, "failed");
                }
                "deltas_are_chunked_to_the_bound" => {
                    assert_eq!(deltas.concat(), "я🦄".repeat(12000));
                    assert_eq!(final_text, deltas.concat());
                    assert!(deltas.len() > 1);
                }
                "tool_result_links_to_call" => {
                    assert!(call.is_some());
                    assert_eq!(result_parent, call);
                }
                "next_event_is_cancel_safe" => assert_eq!(deltas, ["before", "after"]),
                "plain_text_echoes_three_utf8_deltas" => {
                    assert_eq!(deltas.len(), 3);
                    assert_eq!(deltas.concat(), "один🦄два");
                }
                _ => unreachable!(),
            }
        }
    }
    adapter.close(CloseMode::Force, &group).await.unwrap();
    assert!(matches!(
        group.terminate(Duration::from_millis(100)).await.unwrap(),
        Termination::Empty
    ));
}

struct ObservedContainment {
    tree: TreeContainment,
    signals: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl Containment for ObservedContainment {
    fn kind(&self) -> &'static str {
        self.tree.kind()
    }
    fn prepare_command(&self, command: &mut tokio::process::Command) {
        self.tree.prepare_command(command);
    }
    fn members(
        &self,
    ) -> Result<
        Vec<canterlot_worker::containment::Member>,
        canterlot_worker::containment::ContainmentError,
    > {
        self.tree.members()
    }
    fn populated(&self) -> Result<bool, canterlot_worker::containment::ContainmentError> {
        self.tree.populated()
    }
    fn signal_member(
        &self,
        member: &canterlot_worker::containment::Member,
        signal: rustix::process::Signal,
    ) -> Result<bool, canterlot_worker::containment::ContainmentError> {
        self.signals
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.tree.signal_member(member, signal)
    }
    async fn terminate(
        &self,
        grace: Duration,
    ) -> Result<Termination, canterlot_worker::containment::ContainmentError> {
        self.tree.terminate(grace).await
    }
}

const PILOT_CASES: &[&str] = &[
    "blocks_carry_agent_and_model",
    "model_step_switches_model_for_following_blocks",
    "subagent_step_emits_child_blocks_with_parent_agent_and_model",
    "subagent_call_block_carries_task_summary_and_children_carry_the_name",
    "usage_step_accumulates_per_model_and_agent",
    "default_usage_is_derived_from_text",
    "limit_steps_emit_observed_and_reached_with_resets_at",
    "context_exhausted_step_ends_the_turn_with_the_marker",
    "settings_in_transmission_change_the_model_before_the_turn",
    "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli",
    "read_attachment_step_echoes_the_file_sha256",
    "catalog_lists_synthetic_harnesses_with_models_and_efforts",
    "synthetic_codex_profile_has_provider_codex",
];

async fn pilot_case(
    name: &str,
    adapter: &mut SyntheticAdapter,
    group: &dyn Containment,
    directory: &tempfile::TempDir,
    input: &impl Fn(&str) -> Transmission,
) {
    use canterlot_worker::adapter::{LimitFact, LimitWindow, LocalFile, SettingsChange, catalog};
    use serde_json::json;
    if name == "catalog_lists_synthetic_harnesses_with_models_and_efforts" {
        let entries = catalog();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert!(!entry.title.is_empty());
            assert!(entry.images && entry.settings_midsession);
            assert!(!entry.permission_modes.is_empty());
            assert!(
                entry
                    .models
                    .iter()
                    .any(|m| m.id == "synthetic-large" && m.efforts.iter().any(|e| e == "high"))
            );
        }
        return;
    }
    if name == "synthetic_codex_profile_has_provider_codex" {
        assert!(registry("synthetic-codex").is_some());
        assert_eq!(
            catalog()
                .iter()
                .find(|e| e.harness == "synthetic-codex")
                .unwrap()
                .provider,
            "codex"
        );
        return;
    }
    let script = match name {
        "model_step_switches_model_for_following_blocks" => {
            json!({"synthetic":[{"delta":"first"},{"model":"synthetic-small"},{"delta":"second"}]})
        }
        "subagent_step_emits_child_blocks_with_parent_agent_and_model"
        | "subagent_call_block_carries_task_summary_and_children_carry_the_name" => {
            json!({"synthetic":[{"subagent":{"name":"Explore","model":"synthetic-small","task":"private task","steps":[{"delay_ms":200},{"delta":"child"}]}}]})
        }
        "usage_step_accumulates_per_model_and_agent" => {
            json!({"synthetic":[{"usage":{"model":"synthetic-small","agent":"Explore","input":120,"output":40,"cache_read":10,"cache_write":2}},{"usage":{"model":"synthetic-small","agent":"Explore","input":4,"output":8,"cache_read":1,"cache_write":3}},{"usage":{"model":"synthetic-large","agent":"main","input":2,"output":3}}]})
        }
        "limit_steps_emit_observed_and_reached_with_resets_at" => {
            json!({"synthetic":[{"limit_observed":{"window":"weekly","used_permille":960,"resets_in_ms":3600000}},{"limit_reached":{"window":"five_hour","resets_in_ms":5000}}]})
        }
        "context_exhausted_step_ends_the_turn_with_the_marker" => {
            json!({"synthetic":[{"context_exhausted":true},{"delta":"must not run"}]})
        }
        "read_attachment_step_echoes_the_file_sha256" => {
            json!({"synthetic":[{"read_attachment":0}]})
        }
        _ => json!("abcd🦄efg"),
    };
    let text = script
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| script.to_string());
    let mut transmission = input(&text);
    if name == "settings_in_transmission_change_the_model_before_the_turn" {
        transmission.settings = Some(SettingsChange {
            model: Some("synthetic-small".into()),
            instructions: None,
            ..Default::default()
        });
    }
    if name == "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli" {
        for setting in [
            SettingsChange {
                model: Some("invalid".into()),
                instructions: None,
                ..Default::default()
            },
            SettingsChange {
                effort: Some("invalid".into()),
                instructions: None,
                ..Default::default()
            },
            SettingsChange {
                permission_mode: Some("invalid".into()),
                instructions: None,
                ..Default::default()
            },
            SettingsChange {
                model: Some("synthetic-small".into()),
                effort: Some("high".into()),
                permission_mode: None,
                instructions: None,
            },
        ] {
            transmission.settings = Some(setting);
            assert!(
                matches!(adapter.transmit(&transmission, group).await, TransmitOutcome::Rejected(reason) if reason == "settings_unavailable")
            );
        }
        transmission.settings = None;
    }
    let payload = b"private attachment";
    if name == "read_attachment_step_echoes_the_file_sha256" {
        let path = directory.path().join("file");
        std::fs::write(&path, payload).unwrap();
        transmission.files.push(LocalFile {
            path,
            name: "private".into(),
            mime: "text/plain".into(),
            size: payload.len() as u64,
            image: false,
        });
        assert!(!format!("{:?}", transmission.files).contains("private"));
    }
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(matches!(
        adapter.transmit(&transmission, group).await,
        TransmitOutcome::Accepted(_)
    ));
    let mut events = vec![];
    let mut stamps = vec![];
    loop {
        let e = event(adapter).await;
        stamps.push(std::time::Instant::now());
        let done = matches!(
            e,
            AdapterEvent::RunFinished { .. } | AdapterEvent::ContextExhausted
        );
        events.push(e);
        if done {
            break;
        }
    }
    let blocks: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AdapterEvent::BlockStarted {
                block_id,
                agent,
                model,
                parent,
                tool_name,
                summary,
                ..
            } = e
            {
                Some((block_id, agent, model, parent, tool_name, summary))
            } else {
                None
            }
        })
        .collect();
    match name {
        "blocks_carry_agent_and_model" => {
            assert!(!blocks.is_empty());
            assert!(
                blocks
                    .iter()
                    .all(|b| b.1 == "main" && b.2 == "synthetic-small")
            );
        }
        "model_step_switches_model_for_following_blocks" => {
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].2, "synthetic-large");
            assert_eq!(blocks[1].2, "synthetic-small");
        }
        "settings_in_transmission_change_the_model_before_the_turn" => {
            assert!(blocks.iter().all(|b| b.2 == "synthetic-small"))
        }
        "subagent_step_emits_child_blocks_with_parent_agent_and_model"
        | "subagent_call_block_carries_task_summary_and_children_carry_the_name" => {
            let call = blocks
                .iter()
                .find(|b| b.4.as_deref() == Some("Agent"))
                .unwrap();
            assert_eq!(call.1, "main");
            assert_eq!(call.5.as_deref(), Some("private task"));
            let child = blocks.iter().find(|b| b.1 == "Explore").unwrap();
            assert_eq!(child.2, "synthetic-small");
            assert_eq!(*child.3, Some(*call.0));
            let call_pos = events
                .iter()
                .position(
                    |e| matches!(e,AdapterEvent::BlockStarted {block_id,..} if block_id == call.0),
                )
                .unwrap();
            assert!(
                matches!(&events[call_pos+1], AdapterEvent::BlockStarted {agent,model,..} if agent == "Explore" && model == "synthetic-small")
            );
            // Первый дочерний блок виден до того, как вложенные шаги (задержка 200 мс) начнут выдавать вывод.
            let delta_pos = events
                .iter()
                .position(
                    |e| matches!(e, AdapterEvent::BlockDelta { append, .. } if append == "child"),
                )
                .unwrap();
            assert!(stamps[delta_pos] - stamps[call_pos + 1] >= Duration::from_millis(150));
        }
        "usage_step_accumulates_per_model_and_agent" => {
            let AdapterEvent::RunFinished {
                usage,
                context_tokens,
                context_window_tokens,
                ..
            } = events.last().unwrap()
            else {
                panic!()
            };
            assert_eq!(usage.len(), 2);
            let row = usage.iter().find(|u| u.agent == "Explore").unwrap();
            assert_eq!(
                (
                    row.input_tokens,
                    row.output_tokens,
                    row.cache_read_tokens,
                    row.cache_write_tokens
                ),
                (124, 48, 11, 5)
            );
            assert!(*context_tokens > 0);
            assert_eq!(*context_window_tokens, 200000);
        }
        "default_usage_is_derived_from_text" => {
            let AdapterEvent::RunFinished {
                usage,
                context_tokens,
                ..
            } = events.last().unwrap()
            else {
                panic!()
            };
            assert_eq!(usage.len(), 1);
            assert_eq!(usage[0].input_tokens, 2);
            assert_eq!(usage[0].output_tokens, 2);
            assert_eq!(*context_tokens, 4);
        }
        "limit_steps_emit_observed_and_reached_with_resets_at" => {
            let facts: Vec<_> = events
                .iter()
                .filter_map(|e| {
                    if let AdapterEvent::Limit(f) = e {
                        Some(f)
                    } else {
                        None
                    }
                })
                .collect();
            assert!(
                matches!(facts[0],LimitFact::Observed{window:LimitWindow::Weekly,used_permille:960,resets_at_ms:Some(at)} if *at>=before+3600000)
            );
            assert!(
                matches!(facts[1],LimitFact::Reached{window:LimitWindow::FiveHour,resets_at_ms:Some(at)} if *at>=before+5000)
            );
            assert!(events.iter().any(
                |e| matches!(e,AdapterEvent::BlockFinal{text,..} if text=="usage limit reached")
            ));
        }
        "context_exhausted_step_ends_the_turn_with_the_marker" => {
            assert!(matches!(
                events.last(),
                Some(AdapterEvent::ContextExhausted)
            ));
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, AdapterEvent::RunFinished { .. }))
            );
        }
        "read_attachment_step_echoes_the_file_sha256" => {
            use sha2::{Digest, Sha256};
            let hash = format!(
                "sha256:{}",
                Sha256::digest(payload)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e,AdapterEvent::BlockDelta{append,..} if append == &hash))
            );
        }
        "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli" => {
            assert!(
                events
                    .iter()
                    .all(|e| !matches!(e, AdapterEvent::Diagnostic { .. }))
            );
        }
        _ => unreachable!(),
    }
}
