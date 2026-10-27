use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    delivery::{
        dispatch::{BlockReason, DispatchOutcome, GateInputs, dispatch_next},
        receipts::{ApplyOutcome, InputReceipt, apply_input_receipt, apply_run_finished},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions,
    model::{ExecutionState, HoldReason, InputState, PauseReason, RunSlot},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgListener, types::Json};
use tokio::{sync::Barrier, time::timeout};
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

async fn running(pool: &PgPool, seed: &Seed) -> executions::StartedExecution {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

async fn dispatch(pool: &PgPool, conversation_id: Uuid) -> DispatchOutcome {
    dispatch_next(
        pool,
        conversation_id,
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

async fn slot(pool: &PgPool, conversation_id: Uuid) -> RunSlot {
    let (name, id, attempt): (String, Option<Uuid>, Option<i32>) = sqlx::query_as(
        "select run_slot, run_slot_input_id, run_slot_attempt from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap();
    RunSlot::from_columns(&name, id, attempt).unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn happy_path_dispatches_head_then_next_after_run_finished(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    let correlation_id = Uuid::now_v7();
    assert_eq!(
        dispatch_next(
            &pool,
            seed.conversation_id,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            correlation_id
        )
        .await?,
        DispatchOutcome::Dispatched {
            input_id: first.input_id,
            command_id: first.command_id,
            attempt: 1,
            machine_id: seed.machine_id,
            execution_id: execution.execution_id,
            generation: execution.generation,
        }
    );
    let row: (String, String, i32, Uuid, i64, i32) = sqlx::query_as(
        "select i.state, c.delivery_state, c.dispatch_attempt, c.execution_id,
                c.execution_generation, c.last_status_rank from inputs i
         join commands c on c.id = i.command_id where i.id = $1",
    )
    .bind(first.input_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (
            "dispatched".into(),
            "pending".into(),
            1,
            execution.execution_id,
            1,
            0
        )
    );
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Dispatching {
            input_id: first.input_id,
            attempt: 1
        }
    );
    let event: (String, Uuid, Uuid, Value) = sqlx::query_as(
        "select kind, conversation_id, execution_id, payload from events where correlation_id = $1 and kind = 'input_state_changed'",
    )
    .bind(correlation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        event,
        (
            "input_state_changed".into(),
            seed.conversation_id,
            execution.execution_id,
            json!({"input_id": first.input_id, "state": "dispatched", "attempt": 1})
        )
    );
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply_input_receipt(&pool, first.command_id, 1, receipt, Uuid::now_v7()).await?;
    }
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running {
            input_id: first.input_id
        }
    );
    assert_eq!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(vec![BlockReason::SlotBusy])
    );
    let mut conn = pool.acquire().await?;
    assert_eq!(
        executions::live(&mut conn, seed.conversation_id).await?,
        Some((execution.execution_id, 1, ExecutionState::Running))
    );
    drop(conn);
    apply_run_finished(
        &pool,
        execution.execution_id,
        first.command_id,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    assert!(matches!(dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { input_id, attempt: 1, .. } if input_id == second.input_id));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_deferred_holds_until_new_execution_runs(pool: PgPool) -> TestResult {
    for reason in ["generation_closed", "adapter_busy"] {
        let seed = testkit::seed(&pool).await;
        let old_execution = running(&pool, &seed).await;
        let head = input(&pool, &seed).await;
        let next = input(&pool, &seed).await;
        assert!(matches!(
            dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Dispatched { input_id, attempt: 1, .. } if input_id == head.input_id
        ));
        assert_eq!(
            apply_input_receipt(
                &pool,
                head.command_id,
                1,
                InputReceipt::DispatchDeferred {
                    reason: reason.into()
                },
                Uuid::now_v7(),
            )
            .await?,
            ApplyOutcome::Applied {
                input_state: InputState::Queued
            }
        );
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
        let row: (String, i64, String, i32, Value) = sqlx::query_as(
            "select i.state, i.input_seq, c.delivery_state, c.dispatch_attempt, c.result
             from inputs i join commands c on c.id = i.command_id where i.id = $1",
        )
        .bind(head.input_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            row,
            (
                "queued".into(),
                head.input_seq,
                "held".into(),
                1,
                json!({"reason": reason})
            )
        );
        let old_state: String = sqlx::query_scalar("select state from executions where id = $1")
            .bind(old_execution.execution_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(old_state, "running");
        for _ in 0..2 {
            assert_eq!(
                dispatch(&pool, seed.conversation_id).await,
                DispatchOutcome::Blocked(vec![BlockReason::Held(vec![
                    HoldReason::NoLiveExecution
                ])])
            );
        }
        for (state, outcome) in [
            (ExecutionState::Stopping, None),
            (ExecutionState::Stopped, Some("user_stop")),
        ] {
            executions::set_state(
                &pool,
                old_execution.execution_id,
                state,
                outcome,
                Uuid::now_v7(),
            )
            .await?;
        }
        let new_execution =
            executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(
                &pool,
                new_execution.execution_id,
                state,
                None,
                Uuid::now_v7(),
            )
            .await?;
        }
        assert_eq!(
            dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Dispatched {
                input_id: head.input_id,
                command_id: head.command_id,
                attempt: 2,
                machine_id: seed.machine_id,
                execution_id: new_execution.execution_id,
                generation: new_execution.generation,
            }
        );
        let holds: Json<Vec<HoldReason>> =
            sqlx::query_scalar("select hold_reasons from queue_state where conversation_id = $1")
                .bind(seed.conversation_id)
                .fetch_one(&pool)
                .await?;
        assert!(!holds.0.contains(&HoldReason::NoLiveExecution));
        let next_state: String = sqlx::query_scalar("select state from inputs where id = $1")
            .bind(next.input_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(next_state, "queued");
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gate_reports_all_blocking_reasons(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query(
        "update queue_state set pause_reasons = $2, hold_reasons = '[]' where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .bind(Json(vec![PauseReason::UserStop]))
    .execute(&pool)
    .await?;
    let blocked = dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: false,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        blocked,
        DispatchOutcome::Blocked(vec![
            BlockReason::Paused(vec![PauseReason::UserStop]),
            BlockReason::NoLiveRunningExecution,
            BlockReason::MachineUnavailable
        ])
    );
    sqlx::query("update queue_state set closed = true, hold_reasons = $2, run_slot = 'running', run_slot_input_id = $3 where conversation_id = $1")
        .bind(seed.conversation_id).bind(Json(vec![HoldReason::RunInProgress, HoldReason::ContextMaintenance]))
        .bind(Uuid::now_v7()).execute(&pool).await?;
    assert_eq!(
        dispatch_next(
            &pool,
            seed.conversation_id,
            GateInputs {
                machine_available: false,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7()
        )
        .await?,
        DispatchOutcome::Blocked(vec![
            BlockReason::Closed,
            BlockReason::Paused(vec![PauseReason::UserStop]),
            BlockReason::Held(vec![HoldReason::ContextMaintenance]),
            BlockReason::SlotBusy,
            BlockReason::NoLiveRunningExecution,
            BlockReason::MachineUnavailable
        ])
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_dispatchers_dispatch_one(pool: PgPool) -> TestResult {
    for _ in 0..50 {
        let seed = testkit::seed(&pool).await;
        running(&pool, &seed).await;
        let head = input(&pool, &seed).await;
        input(&pool, &seed).await;
        let barrier = Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            let conversation_id = seed.conversation_id;
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                dispatch(&pool, conversation_id).await
            }));
        }
        barrier.wait().await;
        let mut dispatched = 0;
        let mut blocked = 0;
        for task in tasks {
            match timeout(Duration::from_secs(10), task).await?? {
                DispatchOutcome::Dispatched {
                    input_id,
                    attempt: 1,
                    ..
                } if input_id == head.input_id => dispatched += 1,
                DispatchOutcome::Blocked(reasons) if reasons == vec![BlockReason::SlotBusy] => {
                    blocked += 1
                }
                other => panic!("unexpected dispatch outcome: {other:?}"),
            }
        }
        assert_eq!((dispatched, blocked), (1, 1));
        let states: Vec<String> = sqlx::query_scalar(
            "select state from inputs where conversation_id = $1 order by input_seq",
        )
        .bind(seed.conversation_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(states, ["dispatched", "queued"]);
        let attempts: Vec<i32> = sqlx::query_scalar("select c.dispatch_attempt from inputs i join commands c on c.id = i.command_id where i.conversation_id = $1 order by i.input_seq")
            .bind(seed.conversation_id).fetch_all(&pool).await?;
        assert_eq!(attempts, [1, 0]);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn context_maintenance_hold_blocks_dispatch(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    input(&pool, &seed).await;
    sqlx::query("update queue_state set hold_reasons = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Json(vec![HoldReason::ContextMaintenance]))
        .execute(&pool)
        .await?;
    assert_eq!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(vec![BlockReason::Held(vec![
            HoldReason::ContextMaintenance
        ])])
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gate_filters_machine_holds_and_requires_running_execution(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let head = input(&pool, &seed).await;
    sqlx::query("update queue_state set hold_reasons = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Json(vec![
            HoldReason::RunInProgress,
            HoldReason::WorkdirBusy {
                conflict_conversation_id: Uuid::now_v7(),
            },
            HoldReason::AttachmentsPending {
                input_id: head.input_id,
            },
        ]))
        .execute(&pool)
        .await?;
    assert_eq!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(vec![BlockReason::NoLiveRunningExecution])
    );
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7()).await?;
    }
    assert!(matches!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    let empty = testkit::seed(&pool).await;
    running(&pool, &empty).await;
    assert_eq!(
        dispatch(&pool, empty.conversation_id).await,
        DispatchOutcome::Empty
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_notifies_outbox(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    input(&pool, &seed).await;
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen("canterlot_outbox").await?;
    assert!(matches!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    let notification = timeout(Duration::from_secs(5), listener.recv()).await??;
    assert_eq!(notification.channel(), "canterlot_outbox");
    assert_eq!(notification.payload(), seed.machine_id.to_string());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_failure_rolls_back_dispatch(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    sqlx::query("alter table events add constraint reject_input_events check (kind <> 'input_state_changed') not valid")
        .execute(&pool).await?;
    assert!(
        dispatch_next(
            &pool,
            seed.conversation_id,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7()
        )
        .await
        .is_err()
    );
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    let row: (String, String, i32) = sqlx::query_as("select i.state, c.delivery_state, c.dispatch_attempt from inputs i join commands c on c.id = i.command_id where i.id = $1")
        .bind(head.input_id).fetch_one(&pool).await?;
    assert_eq!(row, ("queued".into(), "held".into(), 0));
    Ok(())
}
