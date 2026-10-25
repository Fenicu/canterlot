use canterlot_hub::{
    delivery::{
        dispatch::{BlockReason, DispatchOutcome, GateInputs, dispatch_next},
        receipts::{ApplyOutcome, Ignored, InputReceipt, apply_input_receipt, apply_run_finished},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions,
    model::{ExecutionState, InputState, PauseReason, RunSlot},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, types::Json};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn input(pool: &PgPool, seed: &Seed) -> SubmittedInput {
    submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "synthetic input".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
}

async fn setup(pool: &PgPool) -> (Seed, executions::StartedExecution, SubmittedInput) {
    let seed = testkit::seed(pool).await;
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let head = input(pool, &seed).await;
    assert!(matches!(
        dispatch(pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { attempt: 1, .. }
    ));
    (seed, execution, head)
}

async fn dispatch(pool: &PgPool, conversation_id: Uuid) -> DispatchOutcome {
    dispatch_next(
        pool,
        conversation_id,
        GateInputs {
            machine_available: true,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

async fn apply(
    pool: &PgPool,
    command_id: Uuid,
    attempt: i32,
    receipt: InputReceipt,
) -> ApplyOutcome {
    apply_input_receipt(pool, command_id, attempt, receipt, Uuid::now_v7())
        .await
        .unwrap()
}

async fn snapshot(pool: &PgPool, head: &SubmittedInput) -> Value {
    sqlx::query_scalar(
        "select jsonb_build_object('input', to_jsonb(i), 'command', to_jsonb(c),
                                  'queue', to_jsonb(q), 'events',
                                  (select count(*) from events where conversation_id = i.conversation_id))
         from inputs i join commands c on c.id = i.command_id
         join queue_state q on q.conversation_id = i.conversation_id where i.id = $1",
    ).bind(head.input_id).fetch_one(pool).await.unwrap()
}

async fn slot(pool: &PgPool, conversation_id: Uuid) -> RunSlot {
    let (name, id, attempt): (String, Option<Uuid>, Option<i32>) = sqlx::query_as(
        "select run_slot, run_slot_input_id, run_slot_attempt from queue_state where conversation_id = $1",
    ).bind(conversation_id).fetch_one(pool).await.unwrap();
    RunSlot::from_columns(&name, id, attempt).unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn receipts_are_monotonic_and_attempt_scoped(pool: PgPool) -> TestResult {
    let (_, _, first) = setup(&pool).await;
    assert_eq!(
        apply(&pool, first.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    );
    let accepted = snapshot(&pool, &first).await;
    assert_eq!(
        apply(&pool, first.command_id, 1, InputReceipt::Stored).await,
        ApplyOutcome::Ignored(Ignored::NotMonotonic)
    );
    assert_eq!(
        apply(&pool, first.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::NotMonotonic)
    );
    assert_eq!(snapshot(&pool, &first).await, accepted);

    let (seed, _, second) = setup(&pool).await;
    apply(&pool, second.command_id, 1, InputReceipt::Stored).await;
    apply(&pool, second.command_id, 1, InputReceipt::Withdrawn).await;
    assert!(matches!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    let before = snapshot(&pool, &second).await;
    assert_eq!(before["command"]["last_status_rank"], 0);
    assert_eq!(
        apply(&pool, second.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::StaleAttempt)
    );
    assert_eq!(
        apply(&pool, second.command_id, 3, InputReceipt::Stored).await,
        ApplyOutcome::Ignored(Ignored::StaleAttempt)
    );
    assert_eq!(snapshot(&pool, &second).await, before);
    assert_eq!(
        apply(&pool, second.command_id, 2, InputReceipt::Stored).await,
        ApplyOutcome::Applied {
            input_state: InputState::OnMachine
        }
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn undetermined_pauses_and_never_redispatches(pool: PgPool) -> TestResult {
    let (seed, _, head) = setup(&pool).await;
    input(&pool, &seed).await;
    let unrelated = PauseReason::UnconfirmedDelivery {
        input_id: Uuid::now_v7(),
    };
    sqlx::query("update queue_state set pause_reasons = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Json(vec![PauseReason::UserStop, unrelated.clone()]))
        .execute(&pool)
        .await?;
    apply(&pool, head.command_id, 1, InputReceipt::TransmissionStarted).await;
    assert_eq!(
        apply(
            &pool,
            head.command_id,
            1,
            InputReceipt::AcceptanceUndetermined
        )
        .await,
        ApplyOutcome::Applied {
            input_state: InputState::AcceptanceUnconfirmed
        }
    );
    let pause = PauseReason::UnconfirmedDelivery {
        input_id: head.input_id,
    };
    for _ in 0..3 {
        assert_eq!(
            dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Blocked(vec![BlockReason::Paused(vec![
                PauseReason::UserStop,
                unrelated.clone(),
                pause.clone()
            ])])
        );
        let state = snapshot(&pool, &head).await;
        assert_eq!(state["input"]["state"], "acceptance_unconfirmed");
        assert_eq!(state["command"]["delivery_state"], "completed");
        assert_eq!(state["command"]["dispatch_attempt"], 1);
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    }
    assert_eq!(
        apply(&pool, head.command_id, 0, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::StaleAttempt)
    );
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::Withdrawn).await,
        ApplyOutcome::Ignored(Ignored::NotMonotonic)
    );
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    );
    let state = snapshot(&pool, &head).await;
    assert_eq!(state["input"]["state"], "accepted");
    assert!(!state["input"]["agent_confirmed_at"].is_null());
    assert_eq!(
        state["queue"]["pause_reasons"],
        json!([PauseReason::UserStop, unrelated])
    );
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::NotMonotonic)
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn withdrawn_returns_to_same_place(pool: PgPool) -> TestResult {
    for receipt in [
        InputReceipt::Withdrawn,
        InputReceipt::DispatchDeferred {
            reason: "generation_closed".into(),
        },
    ] {
        let (seed, _, head) = setup(&pool).await;
        let next = input(&pool, &seed).await;
        apply(&pool, head.command_id, 1, InputReceipt::Stored).await;
        assert_eq!(
            apply(&pool, head.command_id, 1, receipt).await,
            ApplyOutcome::Applied {
                input_state: InputState::Queued
            }
        );
        let state = snapshot(&pool, &head).await;
        assert_eq!(state["input"]["input_seq"], head.input_seq);
        assert_eq!(state["input"]["command_id"], head.command_id.to_string());
        assert_eq!(state["command"]["delivery_state"], "held");
        assert_eq!(state["command"]["dispatch_attempt"], 1);
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
        assert!(matches!(dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Dispatched { input_id, command_id, attempt: 2, .. } if input_id == head.input_id && command_id == head.command_id));
        let next_state: String = sqlx::query_scalar("select state from inputs where id = $1")
            .bind(next.input_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(next_state, "queued");
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn receipt_mapping_and_events(pool: PgPool) -> TestResult {
    let (seed, execution, head) = setup(&pool).await;
    for (receipt, expected_state, delivery, rank, expected_slot) in [
        (
            InputReceipt::Stored,
            InputState::OnMachine,
            "stored_on_machine",
            1,
            RunSlot::Dispatching {
                input_id: head.input_id,
                attempt: 1,
            },
        ),
        (
            InputReceipt::TransmissionStarted,
            InputState::Transmitting,
            "stored_on_machine",
            2,
            RunSlot::Dispatching {
                input_id: head.input_id,
                attempt: 1,
            },
        ),
        (
            InputReceipt::Accepted,
            InputState::Accepted,
            "completed",
            3,
            RunSlot::Running {
                input_id: head.input_id,
            },
        ),
    ] {
        let correlation_id = Uuid::now_v7();
        assert_eq!(
            apply_input_receipt(&pool, head.command_id, 1, receipt, correlation_id).await?,
            ApplyOutcome::Applied {
                input_state: expected_state
            }
        );
        let state = snapshot(&pool, &head).await;
        assert_eq!(state["input"]["state"], expected_state.as_str());
        assert_eq!(state["command"]["delivery_state"], delivery);
        assert_eq!(state["command"]["last_status_rank"], rank);
        assert_eq!(slot(&pool, seed.conversation_id).await, expected_slot);
        let event: (String, String, Uuid, Uuid, Value) = sqlx::query_as("select kind, scope, conversation_id, execution_id, payload from events where correlation_id = $1")
            .bind(correlation_id).fetch_one(&pool).await?;
        assert_eq!(
            event,
            (
                "input_state_changed".into(),
                format!("conversation:{}", seed.conversation_id),
                seed.conversation_id,
                execution.execution_id,
                json!({"input_id": head.input_id, "state": expected_state, "attempt": 1})
            )
        );
    }
    assert!(!snapshot(&pool, &head).await["input"]["agent_confirmed_at"].is_null());
    for (receipt, expected_state, reason) in [
        (
            InputReceipt::ExplicitRejection {
                reason: "adapter_rejected".into(),
            },
            InputState::Rejected,
            Some("adapter_rejected"),
        ),
        (
            InputReceipt::RejectedBeforeTransmission {
                reason: "unsupported_attachment".into(),
            },
            InputState::Rejected,
            Some("unsupported_attachment"),
        ),
        (InputReceipt::CancelWon, InputState::Cancelled, None),
    ] {
        let (seed, _, head) = setup(&pool).await;
        assert_eq!(
            apply(&pool, head.command_id, 1, receipt).await,
            ApplyOutcome::Applied {
                input_state: expected_state
            }
        );
        let state = snapshot(&pool, &head).await;
        assert_eq!(state["input"]["state"], expected_state.as_str());
        assert_eq!(state["input"]["reject_reason"], json!(reason));
        assert_eq!(state["command"]["delivery_state"], "completed");
        assert_eq!(state["command"]["last_status_rank"], 3);
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancel_lost_only_clears_request(pool: PgPool) -> TestResult {
    let (seed, _, head) = setup(&pool).await;
    apply(&pool, head.command_id, 1, InputReceipt::TransmissionStarted).await;
    sqlx::query("update inputs set cancel_requested = true where id = $1")
        .bind(head.input_id)
        .execute(&pool)
        .await?;
    let before = snapshot(&pool, &head).await;
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::CancelLost).await,
        ApplyOutcome::Applied {
            input_state: InputState::Transmitting
        }
    );
    let after = snapshot(&pool, &head).await;
    assert_eq!(after["input"]["cancel_requested"], false);
    assert_eq!(
        after["command"]["delivery_state"],
        before["command"]["delivery_state"]
    );
    assert_eq!(after["command"]["last_status_rank"], 3);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Dispatching {
            input_id: head.input_id,
            attempt: 1
        }
    );
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::NotMonotonic)
    );
    assert_eq!(snapshot(&pool, &head).await, after);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn transmitting_never_returns_to_queue_or_cancellation(pool: PgPool) -> TestResult {
    for receipt in [
        InputReceipt::Withdrawn,
        InputReceipt::DispatchDeferred {
            reason: "stopped".into(),
        },
        InputReceipt::CancelWon,
        InputReceipt::RejectedBeforeTransmission {
            reason: "late".into(),
        },
    ] {
        let (_, _, head) = setup(&pool).await;
        apply(&pool, head.command_id, 1, InputReceipt::TransmissionStarted).await;
        let before = snapshot(&pool, &head).await;
        assert_eq!(
            apply(&pool, head.command_id, 1, receipt).await,
            ApplyOutcome::Ignored(Ignored::NotMonotonic)
        );
        assert_eq!(snapshot(&pool, &head).await, before);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_and_terminal_receipts_are_ignored(pool: PgPool) -> TestResult {
    assert_eq!(
        apply(&pool, Uuid::now_v7(), 1, InputReceipt::Stored).await,
        ApplyOutcome::Ignored(Ignored::UnknownCommand)
    );
    let (_, execution, head) = setup(&pool).await;
    assert_eq!(
        apply(&pool, execution.command_id, 0, InputReceipt::Stored).await,
        ApplyOutcome::Ignored(Ignored::UnknownCommand)
    );
    for state in ["accepted", "rejected", "cancelled"] {
        sqlx::query("update inputs set state = $2 where id = $1")
            .bind(head.input_id)
            .bind(state)
            .execute(&pool)
            .await?;
        let before = snapshot(&pool, &head).await;
        assert_eq!(
            apply(&pool, head.command_id, 1, InputReceipt::Accepted).await,
            ApplyOutcome::Ignored(Ignored::Terminal)
        );
        assert_eq!(snapshot(&pool, &head).await, before);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_acceptance_preserves_another_running_slot(pool: PgPool) -> TestResult {
    let (seed, execution, head) = setup(&pool).await;
    apply(
        &pool,
        head.command_id,
        1,
        InputReceipt::AcceptanceUndetermined,
    )
    .await;
    let next = input(&pool, &seed).await;
    sqlx::query("update queue_state set pause_reasons = '[]' where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, next.command_id, 1, InputReceipt::Accepted).await;
    assert_eq!(
        apply(&pool, head.command_id, 1, InputReceipt::Accepted).await,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    );
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running {
            input_id: next.input_id
        }
    );
    apply_run_finished(
        &pool,
        execution.execution_id,
        head.command_id,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running {
            input_id: next.input_id
        }
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_finished_matches_execution_command_and_running_slot(pool: PgPool) -> TestResult {
    let (seed, execution, head) = setup(&pool).await;
    let (other, other_execution, other_head) = setup(&pool).await;
    apply_run_finished(
        &pool,
        execution.execution_id,
        head.command_id,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Dispatching {
            input_id: head.input_id,
            attempt: 1
        }
    );
    apply(&pool, head.command_id, 1, InputReceipt::Accepted).await;
    let before = snapshot(&pool, &head).await;
    for (execution_id, command_id) in [
        (Uuid::now_v7(), head.command_id),
        (other_execution.execution_id, head.command_id),
        (execution.execution_id, other_head.command_id),
        (execution.execution_id, Uuid::now_v7()),
        (execution.execution_id, execution.command_id),
    ] {
        apply_run_finished(&pool, execution_id, command_id, Uuid::now_v7()).await?;
        assert_eq!(snapshot(&pool, &head).await, before);
    }
    assert_eq!(
        slot(&pool, other.conversation_id).await,
        RunSlot::Dispatching {
            input_id: other_head.input_id,
            attempt: 1
        }
    );
    let correlation_id = Uuid::now_v7();
    apply_run_finished(
        &pool,
        execution.execution_id,
        head.command_id,
        correlation_id,
    )
    .await?;
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    let event: (String, Uuid, Value) =
        sqlx::query_as("select kind, execution_id, payload from events where correlation_id = $1")
            .bind(correlation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        event,
        (
            "run_finished".into(),
            execution.execution_id,
            json!({"input_id": head.input_id, "input_command_id": head.command_id})
        )
    );
    let finished = snapshot(&pool, &head).await;
    apply_run_finished(
        &pool,
        execution.execution_id,
        head.command_id,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(snapshot(&pool, &head).await, finished);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_failure_rolls_back_receipt_and_run_finished(pool: PgPool) -> TestResult {
    let (seed, execution, head) = setup(&pool).await;
    let before = snapshot(&pool, &head).await;
    sqlx::query("alter table events add constraint reject_receipt_events check (kind <> 'input_state_changed') not valid").execute(&pool).await?;
    assert!(
        apply_input_receipt(
            &pool,
            head.command_id,
            1,
            InputReceipt::AcceptanceUndetermined,
            Uuid::now_v7()
        )
        .await
        .is_err()
    );
    assert_eq!(snapshot(&pool, &head).await, before);
    sqlx::query("alter table events drop constraint reject_receipt_events")
        .execute(&pool)
        .await?;
    apply(&pool, head.command_id, 1, InputReceipt::Accepted).await;
    let before = snapshot(&pool, &head).await;
    sqlx::query("alter table events add constraint reject_run_events check (kind <> 'run_finished') not valid").execute(&pool).await?;
    assert!(
        apply_run_finished(
            &pool,
            execution.execution_id,
            head.command_id,
            Uuid::now_v7()
        )
        .await
        .is_err()
    );
    assert_eq!(snapshot(&pool, &head).await, before);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running {
            input_id: head.input_id
        }
    );
    Ok(())
}
