//! Управление очередью: отмена, отзыв, остановка и продолжение, их завершение
//! квитанциями по попытке, причины пауз и события `queue_state_changed`.

use canterlot_hub::{
    delivery::{
        control::{CancelOutcome, StopOutcome, cancel_input, continue_queue, request_stop},
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        receipts::{ApplyOutcome, InputReceipt, apply_input_receipt, apply_run_finished},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions,
    model::{ExecutionState, InputState, PauseReason, RunSlot, RunSlotView},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn request(seed: &Seed, text: &str) -> SubmitInput {
    SubmitInput {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        idempotency_key: Uuid::now_v7().to_string(),
        content: vec![ContentBlock::Text { text: text.into() }],
        received_at: Utc::now(),
        correlation_id: Uuid::now_v7(),
        origin: None,
    }
}
async fn state(pool: &PgPool, id: Uuid, to: ExecutionState, outcome: Option<&str>) {
    executions::set_state(pool, id, to, outcome, Uuid::now_v7())
        .await
        .unwrap();
}
async fn running(pool: &PgPool, seed: &Seed) -> executions::StartedExecution {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    state(pool, execution.execution_id, ExecutionState::Starting, None).await;
    state(pool, execution.execution_id, ExecutionState::Running, None).await;
    execution
}
async fn dispatch(pool: &PgPool, seed: &Seed) -> DispatchOutcome {
    dispatch_next(
        pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}
async fn setup(pool: &PgPool) -> (Seed, executions::StartedExecution, SubmittedInput) {
    let seed = testkit::seed(pool).await;
    let execution = running(pool, &seed).await;
    let input = submit_input(pool, request(&seed, "synthetic input"))
        .await
        .unwrap();
    assert!(matches!(
        dispatch(pool, &seed).await,
        DispatchOutcome::Dispatched { attempt: 1, .. }
    ));
    (seed, execution, input)
}
async fn receipt(
    pool: &PgPool,
    input: &SubmittedInput,
    attempt: i32,
    value: InputReceipt,
) -> ApplyOutcome {
    apply_input_receipt(pool, input.command_id, attempt, value, Uuid::now_v7())
        .await
        .unwrap()
}
async fn controls(pool: &PgPool, seed: &Seed, input: &SubmittedInput) -> (Uuid, Uuid) {
    let CancelOutcome::CancelRequested { cancel_command_id } = cancel_input(
        pool,
        seed.owner_id,
        input.input_id,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap() else {
        panic!("cancel not requested")
    };
    let StopOutcome::Requested {
        withdraw_command_id: Some(withdraw),
        ..
    } = request_stop(
        pool,
        seed.owner_id,
        seed.conversation_id,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap()
    else {
        panic!("withdraw not requested")
    };
    (cancel_command_id, withdraw)
}
async fn assert_queue_event(pool: &PgPool, seed: &Seed, correlation: Uuid) -> TestResult {
    let rows: Vec<(Value, String)> = sqlx::query_as("select payload, xmin::text from events where correlation_id = $1 and kind = 'queue_state_changed'").bind(correlation).fetch_all(pool).await?;
    assert_eq!(rows.len(), 1, "exactly one queue event per change");
    let (pauses, holds, slot, input, attempt, tx): (Value, Value, String, Option<Uuid>, Option<i32>, String) = sqlx::query_as("select pause_reasons, hold_reasons, run_slot, run_slot_input_id, run_slot_attempt, xmin::text from queue_state where conversation_id = $1").bind(seed.conversation_id).fetch_one(pool).await?;
    let slot = RunSlot::from_columns(&slot, input, attempt)?;
    let command: Option<Uuid> = sqlx::query_scalar("select command_id from inputs where id = $1")
        .bind(input)
        .fetch_optional(pool)
        .await?;
    let slot = RunSlotView::new(&slot, command);
    assert_eq!(
        rows[0].0,
        json!({"pause_reasons": pauses, "hold_reasons": holds, "run_slot": slot})
    );
    assert_eq!(rows[0].1, tx);
    Ok(())
}
#[tokio::test]
async fn resolved_owner_lost_is_removed_but_restore_pending_stays() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    state(
        &pool,
        execution.execution_id,
        ExecutionState::OwnerLost,
        None,
    )
    .await;
    let outcome = continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "unresolved",
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(outcome.remaining, [PauseReason::OwnerLost]);
    state(
        &pool,
        execution.execution_id,
        ExecutionState::Failed,
        Some("admin_resolved"),
    )
    .await;
    sqlx::query("update queue_state set pause_reasons = pause_reasons || '[{\"reason\":\"restore_pending\"}]'::jsonb where conversation_id = $1").bind(seed.conversation_id).execute(&pool).await?;
    let outcome = continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "resolved",
        Uuid::now_v7(),
    )
    .await?;
    assert!(outcome.removed.contains(&PauseReason::OwnerLost));
    assert_eq!(outcome.remaining, [PauseReason::RestorePending]);
    Ok(())
}
#[tokio::test]
async fn queue_events_cover_lifecycle_receipts_controls_and_run_slot() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let correlation = Uuid::now_v7();
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, correlation).await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    for to in [ExecutionState::Starting, ExecutionState::Running] {
        let correlation = Uuid::now_v7();
        executions::set_state(&pool, execution.execution_id, to, None, correlation).await?;
        if to == ExecutionState::Running {
            assert_queue_event(&pool, &seed, correlation).await?;
        } else {
            let count: i64 = sqlx::query_scalar("select count(*) from events where correlation_id = $1 and kind = 'queue_state_changed'").bind(correlation).fetch_one(&pool).await?;
            assert_eq!(count, 0);
        }
    }
    let input = submit_input(&pool, request(&seed, "input")).await?;
    let correlation = Uuid::now_v7();
    dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        correlation,
    )
    .await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    for value in [InputReceipt::AcceptanceUndetermined, InputReceipt::Accepted] {
        let correlation = Uuid::now_v7();
        apply_input_receipt(&pool, input.command_id, 1, value, correlation).await?;
        assert_queue_event(&pool, &seed, correlation).await?;
    }
    // Позднее доказательство вернуло слот ходу: следующий ввод ждёт его конца.
    let correlation = Uuid::now_v7();
    apply_run_finished(&pool, execution.execution_id, input.command_id, correlation).await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    let next = submit_input(&pool, request(&seed, "next")).await?;
    dispatch(&pool, &seed).await;
    let correlation = Uuid::now_v7();
    apply_input_receipt(
        &pool,
        next.command_id,
        1,
        InputReceipt::Accepted,
        correlation,
    )
    .await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    let correlation = Uuid::now_v7();
    apply_run_finished(&pool, execution.execution_id, next.command_id, correlation).await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    // Прерывание не меняет очередь: события `queue_state_changed` у него нет.
    let correlation = Uuid::now_v7();
    request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        correlation,
    )
    .await?;
    let count: i64 = sqlx::query_scalar(
        "select count(*) from events where correlation_id = $1 and kind = 'queue_state_changed'",
    )
    .bind(correlation)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    // Продолжать нечего, но явное действие по-прежнему отвечает `queue_resumed`.
    let correlation = Uuid::now_v7();
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        correlation,
    )
    .await?;
    let count: i64 = sqlx::query_scalar(
        "select count(*) from events where correlation_id = $1 and kind = 'queue_state_changed'",
    )
    .bind(correlation)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    let resumed: i64 = sqlx::query_scalar(
        "select count(*) from events where correlation_id = $1 and kind = 'queue_resumed'",
    )
    .bind(correlation)
    .fetch_one(&pool)
    .await?;
    assert_eq!(resumed, 1);
    let last = submit_input(&pool, request(&seed, "last")).await?;
    dispatch(&pool, &seed).await;
    receipt(&pool, &last, 1, InputReceipt::Accepted).await;
    state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopping,
        None,
    )
    .await;
    let correlation = Uuid::now_v7();
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Failed,
        Some("crashed"),
        correlation,
    )
    .await?;
    assert_queue_event(&pool, &seed, correlation).await?;
    Ok(())
}
#[tokio::test]
async fn queue_event_failure_rolls_back_state_and_control_completion() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, _, input) = setup(&pool).await;
    let (cancel, withdraw) = controls(&pool, &seed, &input).await;
    sqlx::query("alter table events add constraint reject_queue_events check (kind <> 'queue_state_changed') not valid").execute(&pool).await?;
    assert!(
        apply_input_receipt(
            &pool,
            input.command_id,
            1,
            InputReceipt::Withdrawn,
            Uuid::now_v7()
        )
        .await
        .is_err()
    );
    let rows: Vec<String> =
        sqlx::query_scalar("select delivery_state from commands where id = any($1)")
            .bind(vec![cancel, withdraw])
            .fetch_all(&pool)
            .await?;
    assert_eq!(rows, ["pending", "pending"]);
    let state: String = sqlx::query_scalar("select state from inputs where id = $1")
        .bind(input.input_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "dispatched");
    Ok(())
}
#[tokio::test]
async fn receipt_causes_distinguish_withdrawn_and_deferred() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for (value, cause) in [
        (InputReceipt::Withdrawn, "withdrawn"),
        (
            InputReceipt::DispatchDeferred {
                reason: "gone".into(),
            },
            "dispatch_deferred",
        ),
    ] {
        let (seed, _, input) = setup(&pool).await;
        let correlation = Uuid::now_v7();
        apply_input_receipt(&pool, input.command_id, 1, value, correlation).await?;
        let payload: Value = sqlx::query_scalar(
            "select payload from events where correlation_id = $1 and kind = 'input_state_changed'",
        )
        .bind(correlation)
        .fetch_one(&pool)
        .await?;
        assert_eq!(payload["cause"], cause);
        assert_queue_event(&pool, &seed, correlation).await?;
    }
    Ok(())
}
#[tokio::test]
async fn control_payloads_and_completion_are_attempt_scoped() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for value in [
        InputReceipt::Withdrawn,
        InputReceipt::DispatchDeferred {
            reason: "gone".into(),
        },
        InputReceipt::CancelWon,
        InputReceipt::CancelLost,
    ] {
        let (seed, _, input) = setup(&pool).await;
        let (cancel, withdraw) = controls(&pool, &seed, &input).await;
        let payloads: Vec<Value> =
            sqlx::query_scalar("select payload from commands where id = any($1)")
                .bind(vec![cancel, withdraw])
                .fetch_all(&pool)
                .await?;
        for payload in payloads {
            assert_eq!(payload["target_command_id"], json!(input.command_id));
            assert_eq!(payload["target_dispatch_attempt"], 1);
        }
        let other = Uuid::now_v7();
        sqlx::query("insert into commands (id, machine_id, conversation_id, kind, payload, target_command_id, delivery_state) values ($1, $2, $3, 'withdraw_input', '{\"target_dispatch_attempt\":2}', $4, 'pending')").bind(other).bind(seed.machine_id).bind(seed.conversation_id).bind(input.command_id).execute(&pool).await?;
        let correlation = Uuid::now_v7();
        apply_input_receipt(&pool, input.command_id, 1, value, correlation).await?;
        let rows: Vec<(String, bool)> = sqlx::query_as(
            "select delivery_state, completed_at is not null from commands where id = any($1)",
        )
        .bind(vec![cancel, withdraw])
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            rows,
            [("completed".into(), true), ("completed".into(), true)]
        );
        let same_tx: bool = sqlx::query_scalar("select c.xmin = e.xmin from commands c join events e on e.correlation_id = $2 and e.kind = 'input_state_changed' where c.id = $1").bind(cancel).bind(correlation).fetch_one(&pool).await?;
        assert!(same_tx);
        let other_state: String =
            sqlx::query_scalar("select delivery_state from commands where id = $1")
                .bind(other)
                .fetch_one(&pool)
                .await?;
        assert_eq!(other_state, "pending");
        let interrupt_state: String = sqlx::query_scalar(
            "select delivery_state from commands where conversation_id = $1 and kind = 'interrupt'",
        )
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(interrupt_state, "pending");
    }
    Ok(())
}
#[tokio::test]
async fn late_control_receipt_completes_only_its_attempt() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, _, input) = setup(&pool).await;
    let StopOutcome::Requested {
        withdraw_command_id: Some(old),
        ..
    } = request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop1",
        Uuid::now_v7(),
    )
    .await?
    else {
        panic!()
    };
    receipt(&pool, &input, 1, InputReceipt::Withdrawn).await;
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    assert!(matches!(
        dispatch(&pool, &seed).await,
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    sqlx::query("update commands set delivery_state = 'stored_on_machine', completed_at = null where id = $1").bind(old).execute(&pool).await?;
    let (_, current) = controls(&pool, &seed, &input).await;
    assert_ne!(current, old);
    receipt(
        &pool,
        &input,
        1,
        InputReceipt::DispatchDeferred {
            reason: "old".into(),
        },
    )
    .await;
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("select id, delivery_state from commands where id = any($1)")
            .bind(vec![old, current])
            .fetch_all(&pool)
            .await?;
    assert!(rows.contains(&(old, "completed".into())));
    assert!(rows.contains(&(current, "pending".into())));
    Ok(())
}
#[tokio::test]
async fn old_generation_deferral_does_not_hold_running_generation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, execution, input) = setup(&pool).await;
    state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopping,
        None,
    )
    .await;
    state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopped,
        Some("user_stop"),
    )
    .await;
    running(&pool, &seed).await;
    receipt(
        &pool,
        &input,
        1,
        InputReceipt::DispatchDeferred {
            reason: "old generation".into(),
        },
    )
    .await;
    assert!(matches!(
        dispatch(&pool, &seed).await,
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    Ok(())
}
#[tokio::test]
async fn cancelled_withdrawn_or_deferred_input_never_redispatches() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for value in [
        InputReceipt::Withdrawn,
        InputReceipt::DispatchDeferred {
            reason: "deferred".into(),
        },
    ] {
        let (seed, _, input) = setup(&pool).await;
        controls(&pool, &seed, &input).await;
        assert_eq!(
            receipt(&pool, &input, 1, value).await,
            ApplyOutcome::Applied {
                input_state: InputState::Cancelled
            }
        );
        let (state, delivery): (String, String) = sqlx::query_as("select i.state, c.delivery_state from inputs i join commands c on c.id = i.command_id where i.id = $1").bind(input.input_id).fetch_one(&pool).await?;
        assert_eq!(
            (state.as_str(), delivery.as_str()),
            ("cancelled", "completed")
        );
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7(),
        )
        .await?;
        assert!(!matches!(
            dispatch(&pool, &seed).await,
            DispatchOutcome::Dispatched { .. }
        ));
    }
    Ok(())
}
#[tokio::test]
async fn receipt_reasons_are_sanitized_and_bounded() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let reason = format!("a\0{}", "é".repeat(600));
    for value in [
        InputReceipt::ExplicitRejection {
            reason: reason.clone(),
        },
        InputReceipt::RejectedBeforeTransmission {
            reason: reason.clone(),
        },
        InputReceipt::DispatchDeferred {
            reason: reason.clone(),
        },
    ] {
        let (_, _, input) = setup(&pool).await;
        receipt(&pool, &input, 1, value).await;
        let (result, reject): (Value, Option<String>) = sqlx::query_as("select c.result, i.reject_reason from commands c join inputs i on i.command_id = c.id where c.id = $1").bind(input.command_id).fetch_one(&pool).await?;
        let expected = format!("a{}…", "é".repeat(510));
        assert_eq!(expected.len(), 1024);
        assert_eq!(result["reason"], expected);
        if let Some(reject) = reject {
            assert_eq!(reject, expected);
        }
    }
    Ok(())
}
#[tokio::test]
async fn owner_initiated_terminal_outcomes_add_no_pause() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for outcome in ["start_cancelled", "force_stopped"] {
        let seed = testkit::seed(&pool).await;
        let execution = if outcome == "start_cancelled" {
            executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?
        } else {
            let execution = running(&pool, &seed).await;
            state(
                &pool,
                execution.execution_id,
                ExecutionState::Stopping,
                None,
            )
            .await;
            execution
        };
        state(
            &pool,
            execution.execution_id,
            ExecutionState::Stopped,
            Some(outcome),
        )
        .await;
        let pauses: Value =
            sqlx::query_scalar("select pause_reasons from queue_state where conversation_id = $1")
                .bind(seed.conversation_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(pauses, json!([]));
    }
    Ok(())
}
#[tokio::test]
async fn transmission_implies_machine_storage() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for stored_first in [false, true] {
        let (_, _, input) = setup(&pool).await;
        if stored_first {
            receipt(&pool, &input, 1, InputReceipt::Stored).await;
        }
        let before: Option<String> =
            sqlx::query_scalar("select stored_at::text from commands where id = $1")
                .bind(input.command_id)
                .fetch_one(&pool)
                .await?;
        receipt(&pool, &input, 1, InputReceipt::TransmissionStarted).await;
        let (delivery, stored): (String, Option<String>) =
            sqlx::query_as("select delivery_state, stored_at::text from commands where id = $1")
                .bind(input.command_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(delivery, "stored_on_machine");
        assert!(stored.is_some());
        if before.is_some() {
            assert_eq!(stored, before);
        }
    }
    Ok(())
}
#[tokio::test]
async fn repeated_controls_with_different_keys_reuse_commands() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, _, input) = setup(&pool).await;
    let first = controls(&pool, &seed, &input).await;
    let second = controls(&pool, &seed, &input).await;
    assert_eq!(first, second);
    let count: i64 =
        sqlx::query_scalar("select count(*) from commands where target_command_id = $1")
            .bind(input.command_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 2);
    Ok(())
}
#[tokio::test]
async fn terminal_receipts_still_complete_pending_controls() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    for value in [
        InputReceipt::Withdrawn,
        InputReceipt::DispatchDeferred {
            reason: "late".into(),
        },
        InputReceipt::CancelWon,
        InputReceipt::CancelLost,
    ] {
        let (seed, _, input) = setup(&pool).await;
        let (cancel, withdraw) = controls(&pool, &seed, &input).await;
        receipt(&pool, &input, 1, InputReceipt::Accepted).await;
        receipt(&pool, &input, 1, value).await;
        let count: i64 = sqlx::query_scalar("select count(*) from commands where id = any($1) and delivery_state = 'completed' and completed_at is not null").bind(vec![cancel, withdraw]).fetch_one(&pool).await?;
        assert_eq!(count, 2);
        let state: String = sqlx::query_scalar("select state from inputs where id = $1")
            .bind(input.input_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(state, "accepted");
    }
    Ok(())
}
