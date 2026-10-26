#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{path::PathBuf, time::Duration};

use canterlot_worker::{
    adapter::{
        Adapter, AdapterEvent, BlockKind, CloseMode, InterruptOutcome, StartSpec, Transmission,
        TransmitOutcome, registry, synthetic::SyntheticAdapter,
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
    let group = TreeContainment::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let _guard = process_guard::ProcessGuard::new(directory.path());
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: directory.path().into(),
        native_conversation_id: None,
        profile: "synthetic".into(),
    };
    let make = |profile: &str| {
        SyntheticAdapter::new(profile)
            .with_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker")))
            .with_transmit_deadline(Duration::from_millis(250))
    };
    let mut adapter = make("synthetic");
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
    };

    match case.as_str() {
        "capabilities_describe_profiles" => {
            use canterlot_worker::adapter::Support;
            let capabilities = adapter.capabilities();
            assert_eq!(capabilities.streaming, Support::Supported);
            assert_eq!(capabilities.resume, Support::Supported);
            assert_eq!(capabilities.interrupt, Support::Supported);
            assert_eq!(capabilities.approvals, Support::Unsupported);
            assert_eq!(capabilities.questions, Support::Unsupported);
            assert!(capabilities.interrupt_keeps_process);
            assert!(
                !make("synthetic-exit-on-interrupt")
                    .capabilities()
                    .interrupt_keeps_process
            );
        }
        "oversized_input_is_rejected_before_writing" => {
            assert!(matches!(
                adapter.transmit(&input(&"x".repeat(262145))).await,
                TransmitOutcome::Rejected(_)
            ));
            assert!(matches!(
                adapter.transmit(&input("valid")).await,
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
                    .transmit(&input(r#"{"synthetic":[{"exit":7}]}"#))
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
                        text: secret.into()
                    }
                )
                .contains(secret)
            );
        }
        "acceptance_requires_the_cli_frame_not_the_write" => {
            let before = tokio::time::Instant::now();
            assert!(matches!(
                adapter
                    .transmit(&input(r#"{"synthetic":[{"hang":true},{"delta":"later"}]}"#))
                    .await,
                TransmitOutcome::Undetermined
            ));
            assert!(before.elapsed() >= Duration::from_millis(200));
            assert!(before.elapsed() < Duration::from_secs(2));
        }
        "resume_keeps_native_id" => {
            let id = started.native_conversation_id;
            adapter.close(CloseMode::Graceful).await.unwrap();
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
            resumed.close(CloseMode::Graceful).await.unwrap();
        }
        "interrupt_keeps_process_by_default_and_exits_in_exit_profile" => {
            for profile in ["synthetic", "synthetic-exit-on-interrupt"] {
                adapter.close(CloseMode::Graceful).await.unwrap();
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
                    adapter.transmit(&transmission).await,
                    TransmitOutcome::Accepted(_)
                ));
                assert!(matches!(
                    adapter.interrupt().await,
                    InterruptOutcome::Acknowledged
                ));
                if profile == "synthetic" {
                    loop {
                        if let AdapterEvent::RunFinished { input_command_id } =
                            event(&mut adapter).await
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
                        adapter.transmit(&input("second")).await,
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
            let proof = adapter.transmit(&transmission).await;
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
                    AdapterEvent::RunFinished { input_command_id } => {
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
    adapter.close(CloseMode::Force).await.unwrap();
    assert!(matches!(
        group.terminate(Duration::from_millis(100)).await.unwrap(),
        Termination::Empty
    ));
}
