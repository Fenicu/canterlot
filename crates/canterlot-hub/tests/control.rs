use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    delivery::{
        control::{
            CancelOutcome, ContinueOutcome, ControlError, SendNowOutcome, StopOutcome,
            cancel_input, continue_queue, request_send_now, request_stop,
        },
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
use sqlx::{PgPool, types::Json};
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
            origin: None,
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

async fn apply(pool: &PgPool, command_id: Uuid, receipt: InputReceipt) -> ApplyOutcome {
    apply_input_receipt(pool, command_id, 1, receipt, Uuid::now_v7())
        .await
        .unwrap()
}

async fn send_now(
    pool: &PgPool,
    seed: &Seed,
    input_id: Uuid,
) -> Result<SendNowOutcome, ControlError> {
    request_send_now(
        pool,
        seed.owner_id,
        seed.conversation_id,
        input_id,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
        &["send_now_v1".to_owned()],
    )
    .await
}

async fn accept_turn(pool: &PgPool, command_id: Uuid) {
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply(pool, command_id, receipt).await;
    }
}

async fn steer(pool: &PgPool, input_command_id: Uuid) -> (Uuid, String, Value, Option<Value>) {
    sqlx::query_as(
        "select id, delivery_state, payload, result from commands
         where kind = 'steer_input' and target_command_id = $1",
    )
    .bind(input_command_id)
    .fetch_one(pool)
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

async fn pauses(pool: &PgPool, conversation_id: Uuid) -> Vec<PauseReason> {
    sqlx::query_scalar::<_, Json<Vec<PauseReason>>>(
        "select pause_reasons from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
    .0
}

async fn state(pool: &PgPool, input_id: Uuid) -> InputState {
    sqlx::query_scalar::<_, String>("select state from inputs where id = $1")
        .bind(input_id)
        .fetch_one(pool)
        .await
        .unwrap()
        .parse()
        .unwrap()
}

async fn command_count(pool: &PgPool, conversation_id: Uuid, kind: &str) -> i64 {
    sqlx::query_scalar("select count(*) from commands where conversation_id = $1 and kind = $2")
        .bind(conversation_id)
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn snapshot(pool: &PgPool, conversation_id: Uuid) -> Value {
    sqlx::query_scalar(
        "select jsonb_build_object(
             'queue', to_jsonb(q),
             'inputs', (select jsonb_agg(to_jsonb(i) order by i.id) from inputs i where i.conversation_id = $1),
             'commands', (select jsonb_agg(to_jsonb(c) order by c.id) from commands c where c.conversation_id = $1),
             'events', (select jsonb_agg(to_jsonb(e) order by e.scope_seq) from events e where e.conversation_id = $1),
             'keys', (select jsonb_agg(to_jsonb(k) order by k.key) from idempotency_keys k where k.conversation_id = $1))
         from queue_state q where q.conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_queued_input(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    let correlation_id = Uuid::now_v7();
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            correlation_id
        )
        .await?,
        CancelOutcome::Cancelled
    );
    let before = snapshot(&pool, seed.conversation_id).await;
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::Cancelled
    );
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert_eq!(state(&pool, head.input_id).await, InputState::Cancelled);
    for _ in 0..2 {
        assert_eq!(
            dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Empty
        );
    }
    // Отменённый ввод не оставляет за собой удержанную выдачу: команда
    // завершена в той же транзакции и не выглядит ждущей доставки.
    let (delivery, attempt, completed): (String, i32, bool) = sqlx::query_as(
        "select delivery_state, dispatch_attempt, completed_at is not null
         from commands where id = $1",
    )
    .bind(head.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!((delivery.as_str(), attempt), ("completed", 0));
    assert!(completed);
    let (kind, payload): (String, Value) =
        sqlx::query_as("select kind, payload from events where correlation_id = $1")
            .bind(correlation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(kind, "input_state_changed");
    assert_eq!(payload["input_id"], json!(head.input_id));
    assert_eq!(payload["state"], "cancelled");
    assert_eq!(
        command_count(&pool, seed.conversation_id, "cancel_input").await,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_queued_input_cancels_pending_steer(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set harness = 'codex' where id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first.command_id).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second.input_id).await?
    else {
        panic!("send now must create a steer");
    };
    assert_eq!(steer(&pool, second.command_id).await.1, "pending");

    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            second.input_id,
            "cancel-steered",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::Cancelled
    );
    assert_eq!(state(&pool, second.input_id).await, InputState::Cancelled);
    let (id, delivery, _, result) = steer(&pool, second.command_id).await;
    assert_eq!(id, steer_command_id);
    assert_eq!(delivery, "completed");
    assert_eq!(result.unwrap()["cancelled"], json!(true));
    // Отменённый ввод не уходит ни влитым, ни следующим ходом.
    assert!(!matches!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_refuses_steer_already_handed_to_machine(pool: PgPool) -> TestResult {
    for claimed in [true, false] {
        let seed = testkit::seed(&pool).await;
        sqlx::query("update conversations set harness = 'codex' where id = $1")
            .bind(seed.conversation_id)
            .execute(&pool)
            .await?;
        running(&pool, &seed).await;
        let first = input(&pool, &seed).await;
        let second = input(&pool, &seed).await;
        dispatch(&pool, seed.conversation_id).await;
        accept_turn(&pool, first.command_id).await;
        let SendNowOutcome::Requested { steer_command_id } =
            send_now(&pool, &seed, second.input_id).await?
        else {
            panic!("send now must create a steer");
        };
        let handed = if claimed {
            sqlx::query("update commands set dispatch_claimed_at = now() where id = $1")
                .bind(steer_command_id)
                .execute(&pool)
                .await?;
            "pending"
        } else {
            sqlx::query("update commands set delivery_state = 'stored_on_machine' where id = $1")
                .bind(steer_command_id)
                .execute(&pool)
                .await?;
            "stored_on_machine"
        };

        assert!(matches!(
            cancel_input(
                &pool,
                seed.owner_id,
                second.input_id,
                "cancel-steered",
                Uuid::now_v7()
            )
            .await,
            Err(ControlError::InvalidRequest {
                reason: "steer_in_flight"
            })
        ));
        assert_eq!(state(&pool, second.input_id).await, InputState::Queued);
        assert_eq!(steer(&pool, second.command_id).await.1, handed);
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_dispatched_requests_machine_cancel(pool: PgPool) -> TestResult {
    for stored in [false, true] {
        let seed = testkit::seed(&pool).await;
        let execution = running(&pool, &seed).await;
        let head = input(&pool, &seed).await;
        dispatch(&pool, seed.conversation_id).await;
        if stored {
            apply(&pool, head.command_id, InputReceipt::Stored).await;
        }
        let result = cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7(),
        )
        .await?;
        let CancelOutcome::CancelRequested { cancel_command_id } = result else {
            panic!("unexpected cancel outcome: {result:?}");
        };
        let command: (String, String, Uuid, Uuid, i64, Uuid) = sqlx::query_as(
            "select kind, delivery_state, target_command_id, execution_id, execution_generation, machine_id
             from commands where id = $1",
        ).bind(cancel_command_id).fetch_one(&pool).await?;
        assert_eq!(
            command,
            (
                "cancel_input".into(),
                "pending".into(),
                head.command_id,
                execution.execution_id,
                execution.generation,
                seed.machine_id
            )
        );
        assert_eq!(
            state(&pool, head.input_id).await,
            if stored {
                InputState::OnMachine
            } else {
                InputState::Dispatched
            }
        );
        assert!(
            sqlx::query_scalar::<_, bool>("select cancel_requested from inputs where id = $1")
                .bind(head.input_id)
                .fetch_one(&pool)
                .await?
        );
        let before = snapshot(&pool, seed.conversation_id).await;
        assert_eq!(
            cancel_input(
                &pool,
                seed.owner_id,
                head.input_id,
                "cancel",
                Uuid::now_v7()
            )
            .await?,
            result
        );
        assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
        assert_eq!(
            apply(&pool, head.command_id, InputReceipt::CancelWon).await,
            ApplyOutcome::Applied {
                input_state: InputState::Cancelled
            }
        );
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
        assert_eq!(
            cancel_input(
                &pool,
                seed.owner_id,
                head.input_id,
                "cancel",
                Uuid::now_v7()
            )
            .await?,
            result
        );
        assert_eq!(
            command_count(&pool, seed.conversation_id, "cancel_input").await,
            1
        );
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_after_transmission_is_too_late(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, head.command_id, InputReceipt::TransmissionStarted).await;
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::TooLate {
            state: InputState::Transmitting
        }
    );
    apply(&pool, head.command_id, InputReceipt::Accepted).await;
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::TooLate {
            state: InputState::Transmitting
        }
    );
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel-accepted",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::TooLate {
            state: InputState::Accepted
        }
    );
    assert_eq!(
        command_count(&pool, seed.conversation_id, "cancel_input").await,
        0
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("select cancel_requested from inputs where id = $1")
            .bind(head.input_id)
            .fetch_one(&pool)
            .await?
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_races_dispatch_without_false_cancel(pool: PgPool) -> TestResult {
    let mut hub_cancelled = 0;
    let mut machine_cancel_requested = 0;
    for _ in 0..100 {
        let seed = testkit::seed(&pool).await;
        running(&pool, &seed).await;
        let head = input(&pool, &seed).await;
        let barrier = Arc::new(Barrier::new(2));
        let cancel = {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                cancel_input(
                    &pool,
                    seed.owner_id,
                    head.input_id,
                    "cancel",
                    Uuid::now_v7(),
                )
                .await
            })
        };
        let dispatch = {
            let pool = pool.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                dispatch(&pool, seed.conversation_id).await
            })
        };
        let (cancelled, dispatched) = timeout(Duration::from_secs(10), async {
            tokio::join!(cancel, dispatch)
        })
        .await?;
        let row: (String, bool, String, i32) = sqlx::query_as(
            "select i.state, i.cancel_requested, c.delivery_state, c.dispatch_attempt
             from inputs i join commands c on c.id = i.command_id where i.id = $1",
        )
        .bind(head.input_id)
        .fetch_one(&pool)
        .await?;
        match (cancelled??, dispatched?) {
            (CancelOutcome::Cancelled, DispatchOutcome::Empty | DispatchOutcome::Blocked(_)) => {
                hub_cancelled += 1;
                assert_eq!(row, ("cancelled".into(), false, "completed".into(), 0));
                let dispatched_events: i64 = sqlx::query_scalar(
                    "select count(*) from events where conversation_id = $1
                     and payload->>'state' = 'dispatched'",
                )
                .bind(seed.conversation_id)
                .fetch_one(&pool)
                .await?;
                assert_eq!(dispatched_events, 0);
                assert_eq!(
                    command_count(&pool, seed.conversation_id, "cancel_input").await,
                    0
                );
            }
            (
                CancelOutcome::CancelRequested { cancel_command_id },
                DispatchOutcome::Dispatched {
                    input_id,
                    command_id,
                    attempt: 1,
                    ..
                },
            ) => {
                machine_cancel_requested += 1;
                assert_eq!((input_id, command_id), (head.input_id, head.command_id));
                assert_eq!(row, ("dispatched".into(), true, "pending".into(), 1));
                let target: Uuid = sqlx::query_scalar(
                    "select target_command_id from commands where id = $1 and kind = 'cancel_input'",
                ).bind(cancel_command_id).fetch_one(&pool).await?;
                assert_eq!(target, head.command_id);
            }
            other => panic!("invalid cancel/dispatch race outcome: {other:?}"),
        }
        assert!(!(row.0 == "cancelled" && row.2 == "pending"));
    }
    assert_eq!(hub_cancelled + machine_cancel_requested, 100);
    eprintln!("100 races: cancelled={hub_cancelled}, cancel_requested={machine_cancel_requested}");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_pauses_before_interrupt(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, first.command_id, InputReceipt::Accepted).await;
    let correlation_id = Uuid::now_v7();
    let result = request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        correlation_id,
    )
    .await?;
    let StopOutcome::Requested {
        interrupt_command_id: Some(interrupt),
        withdraw_command_id: None,
    } = result
    else {
        panic!("unexpected stop outcome: {result:?}");
    };
    assert_eq!(
        pauses(&pool, seed.conversation_id).await,
        [PauseReason::UserStop]
    );
    let command: (String, String, Uuid, i64, Uuid) = sqlx::query_as(
        "select kind, delivery_state, execution_id, execution_generation, machine_id from commands where id = $1",
    ).bind(interrupt).fetch_one(&pool).await?;
    assert_eq!(
        command,
        (
            "interrupt".into(),
            "pending".into(),
            execution.execution_id,
            execution.generation,
            seed.machine_id
        )
    );
    let (queue_tx, command_tx, event_tx, key_tx): (String, String, String, String) =
        sqlx::query_as(
            "select q.xmin::text, c.xmin::text, e.xmin::text, k.xmin::text
         from queue_state q join commands c on c.id = $2
         join events e on e.correlation_id = $3
         join idempotency_keys k on k.owner_id = $4 and k.key = 'stop'
         where q.conversation_id = $1 and e.kind = 'queue_paused'",
        )
        .bind(seed.conversation_id)
        .bind(interrupt)
        .bind(correlation_id)
        .bind(seed.owner_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(queue_tx, command_tx);
    assert_eq!(queue_tx, event_tx);
    assert_eq!(queue_tx, key_tx);
    assert_eq!(state(&pool, first.input_id).await, InputState::Accepted);
    assert_eq!(state(&pool, second.input_id).await, InputState::Queued);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running {
            input_id: first.input_id
        }
    );
    apply_run_finished(
        &pool,
        execution.execution_id,
        first.command_id,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(vec![BlockReason::Paused(vec![PauseReason::UserStop])])
    );
    assert_eq!(state(&pool, second.input_id).await, InputState::Queued);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_withdraws_undelivered_dispatch(pool: PgPool) -> TestResult {
    for stored in [false, true] {
        let seed = testkit::seed(&pool).await;
        let execution = running(&pool, &seed).await;
        let head = input(&pool, &seed).await;
        dispatch(&pool, seed.conversation_id).await;
        if stored {
            apply(&pool, head.command_id, InputReceipt::Stored).await;
        }
        let result = request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7(),
        )
        .await?;
        let StopOutcome::Requested {
            interrupt_command_id: Some(_),
            withdraw_command_id: Some(withdraw),
        } = result
        else {
            panic!("unexpected stop outcome: {result:?}");
        };
        let command: (String, String, Uuid, Uuid, i64) = sqlx::query_as(
            "select kind, delivery_state, target_command_id, execution_id, execution_generation from commands where id = $1",
        ).bind(withdraw).fetch_one(&pool).await?;
        assert_eq!(
            command,
            (
                "withdraw_input".into(),
                "pending".into(),
                head.command_id,
                execution.execution_id,
                execution.generation
            )
        );
        let before = snapshot(&pool, seed.conversation_id).await;
        assert_eq!(
            request_stop(
                &pool,
                seed.owner_id,
                seed.conversation_id,
                "stop",
                Uuid::now_v7()
            )
            .await?,
            result
        );
        assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
        assert_eq!(
            apply(&pool, head.command_id, InputReceipt::Withdrawn).await,
            ApplyOutcome::Applied {
                input_state: InputState::Queued
            }
        );
        assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
        assert_eq!(
            pauses(&pool, seed.conversation_id).await,
            [PauseReason::UserStop]
        );
        let seq: i64 = sqlx::query_scalar("select input_seq from inputs where id = $1")
            .bind(head.input_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(seq, head.input_seq);
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn continue_releases_user_pause_and_dispatches_once(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, first.command_id, InputReceipt::Accepted).await;
    request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await?;
    apply_run_finished(
        &pool,
        execution.execution_id,
        first.command_id,
        Uuid::now_v7(),
    )
    .await?;
    let correlation_id = Uuid::now_v7();
    let result = continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        correlation_id,
    )
    .await?;
    assert_eq!(
        result,
        ContinueOutcome {
            removed: vec![PauseReason::UserStop],
            remaining: vec![]
        }
    );
    assert_eq!(state(&pool, second.input_id).await, InputState::Queued);
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    let event: String = sqlx::query_scalar(
        "select kind from events where correlation_id = $1 and kind <> 'queue_state_changed'",
    )
    .bind(correlation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(event, "queue_resumed");
    assert!(matches!(dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { input_id, attempt: 1, .. } if input_id == second.input_id));
    let before = snapshot(&pool, seed.conversation_id).await;
    assert_eq!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
        .await?,
        result
    );
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert_eq!(
        dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(vec![BlockReason::SlotBusy])
    );
    let attempt: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
        .bind(second.command_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(attempt, 1);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn continue_clears_resolved_owner_lost_and_keeps_unconfirmed_input(
    pool: PgPool,
) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    let next = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, head.command_id, InputReceipt::AcceptanceUndetermined).await;
    let unconfirmed = PauseReason::UnconfirmedDelivery {
        input_id: head.input_id,
    };
    sqlx::query("update queue_state set pause_reasons = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Json(vec![PauseReason::OwnerLost, unconfirmed.clone()]))
        .execute(&pool)
        .await?;
    let before: Value = sqlx::query_scalar("select to_jsonb(i) from inputs i where id = $1")
        .bind(head.input_id)
        .fetch_one(&pool)
        .await?;
    let correlation_id = Uuid::now_v7();
    assert_eq!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            correlation_id
        )
        .await?,
        ContinueOutcome {
            removed: vec![PauseReason::OwnerLost, unconfirmed],
            remaining: vec![]
        }
    );
    assert_eq!(
        state(&pool, head.input_id).await,
        InputState::AcceptanceUnconfirmed
    );
    let after: Value = sqlx::query_scalar("select to_jsonb(i) from inputs i where id = $1")
        .bind(head.input_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(before, after);
    assert_eq!(pauses(&pool, seed.conversation_id).await, []);
    let (kind, payload): (String, Value) =
        sqlx::query_as("select kind, payload from events where correlation_id = $1 and kind <> 'queue_state_changed'")
            .bind(correlation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(kind, "queue_resumed");
    assert_eq!(payload["pause_reasons"], json!([]));
    assert!(matches!(dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { input_id, .. } if input_id == next.input_id));
    let attempt: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
        .bind(head.command_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(attempt, 1);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_and_continue_are_idempotent(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let stop = request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await?;
    let before = snapshot(&pool, seed.conversation_id).await;
    assert_eq!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await?,
        stop
    );
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert_eq!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop-again",
            Uuid::now_v7()
        )
        .await?,
        stop
    );
    assert_eq!(
        command_count(&pool, seed.conversation_id, "interrupt").await,
        1
    );
    assert_eq!(
        pauses(&pool, seed.conversation_id).await,
        [PauseReason::UserStop]
    );
    let resumed = continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    let before = snapshot(&pool, seed.conversation_id).await;
    assert_eq!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
        .await?,
        resumed
    );
    assert_eq!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await?,
        stop
    );
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert!(pauses(&pool, seed.conversation_id).await.is_empty());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn continue_removes_only_releasable_reasons_and_preserves_holds(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let head = input(&pool, &seed).await;
    let removed = vec![
        PauseReason::UserStop,
        PauseReason::FatalError,
        PauseReason::ProcessExited,
        PauseReason::LimitReached {
            provider: None,
            window: None,
            resets_at: None,
        },
        PauseReason::OwnerLost,
        PauseReason::UnconfirmedDelivery {
            input_id: head.input_id,
        },
        PauseReason::UnconfirmedDelivery {
            input_id: Uuid::now_v7(),
        },
    ];
    let remaining = vec![PauseReason::RestorePending];
    let holds = vec![HoldReason::NoLiveExecution, HoldReason::ContextMaintenance];
    let all = [removed.clone(), remaining.clone()].concat();
    sqlx::query(
        "update queue_state set pause_reasons = $2, hold_reasons = $3 where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .bind(Json(all))
    .bind(Json(&holds))
    .execute(&pool)
    .await?;
    assert_eq!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
        .await?,
        ContinueOutcome {
            removed,
            remaining: remaining.clone()
        }
    );
    assert_eq!(pauses(&pool, seed.conversation_id).await, remaining);
    let stored: Json<Vec<HoldReason>> =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id = $1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(stored.0, holds);
    assert_eq!(state(&pool, head.input_id).await, InputState::Queued);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_without_live_execution_and_after_transmission(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    assert_eq!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop-empty",
            Uuid::now_v7()
        )
        .await?,
        StopOutcome::Requested {
            interrupt_command_id: None,
            withdraw_command_id: None
        }
    );
    assert_eq!(
        pauses(&pool, seed.conversation_id).await,
        [PauseReason::UserStop]
    );
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    apply(&pool, head.command_id, InputReceipt::TransmissionStarted).await;
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop-transmitting",
            Uuid::now_v7()
        )
        .await?,
        StopOutcome::Requested {
            interrupt_command_id: Some(_),
            withdraw_command_id: None
        }
    ));
    assert_eq!(
        command_count(&pool, seed.conversation_id, "withdraw_input").await,
        0
    );
    assert_eq!(state(&pool, head.input_id).await, InputState::Transmitting);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn control_checks_ownership_and_missing_objects(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let stranger = testkit::seed(&pool).await;
    let head = input(&pool, &seed).await;
    let before = snapshot(&pool, seed.conversation_id).await;
    for owner in [stranger.owner_id, Uuid::now_v7()] {
        assert!(matches!(
            cancel_input(&pool, owner, head.input_id, "cancel", Uuid::now_v7()).await,
            Err(ControlError::NotFound)
        ));
        assert!(matches!(
            request_stop(&pool, owner, seed.conversation_id, "stop", Uuid::now_v7()).await,
            Err(ControlError::NotFound)
        ));
        assert!(matches!(
            continue_queue(
                &pool,
                owner,
                seed.conversation_id,
                "continue",
                Uuid::now_v7()
            )
            .await,
            Err(ControlError::NotFound)
        ));
    }
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            Uuid::now_v7(),
            "missing",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::NotFound)
    ));
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            Uuid::now_v7(),
            "missing",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::NotFound)
    ));
    assert!(matches!(
        continue_queue(
            &pool,
            seed.owner_id,
            Uuid::now_v7(),
            "missing",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::NotFound)
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotency_conflicts_do_not_change_any_queue(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let other_conversation =
        testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let other = Seed {
        conversation_id: other_conversation,
        ..seed
    };
    let first = input(&pool, &seed).await;
    let second = input(&pool, &other).await;
    cancel_input(
        &pool,
        seed.owner_id,
        first.input_id,
        "cancel",
        Uuid::now_v7(),
    )
    .await?;
    request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await?;
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    let before = snapshot(&pool, seed.conversation_id).await;
    let other_before = snapshot(&pool, other_conversation).await;
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            second.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            other_conversation,
            "stop",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert!(matches!(
        continue_queue(
            &pool,
            seed.owner_id,
            other_conversation,
            "continue",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert!(matches!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert!(matches!(
        cancel_input(&pool, seed.owner_id, first.input_id, "stop", Uuid::now_v7()).await,
        Err(ControlError::IdempotencyConflict)
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert_eq!(snapshot(&pool, other_conversation).await, other_before);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn event_failure_rolls_back_controls_and_does_not_bind_keys(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    let before = snapshot(&pool, seed.conversation_id).await;
    sqlx::query("alter table events add constraint reject_control_events check (kind not in ('input_state_changed', 'queue_paused', 'queue_resumed')) not valid")
        .execute(&pool).await?;
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::Db(_))
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::Db(_))
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert!(matches!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::Db(_))
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    sqlx::query("alter table events drop constraint reject_control_events")
        .execute(&pool)
        .await?;
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::Cancelled
    );
    request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await?;
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_replays_create_one_command_per_operation(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    let (first, second) = tokio::join!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        ),
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
    );
    assert_eq!(first?, second?);
    let (first, second) = tokio::join!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        ),
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
    );
    assert_eq!(first?, second?);
    let (first, second) = tokio::join!(
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        ),
        continue_queue(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "continue",
            Uuid::now_v7()
        )
    );
    assert_eq!(first?, second?);
    for kind in ["cancel_input", "interrupt", "withdraw_input"] {
        assert_eq!(command_count(&pool, seed.conversation_id, kind).await, 1);
    }
    let operations: Vec<String> = sqlx::query_scalar(
        "select operation from idempotency_keys where owner_id = $1 and key in ('cancel', 'stop', 'continue') order by operation",
    ).bind(seed.owner_id).fetch_all(&pool).await?;
    assert_eq!(
        operations,
        ["cancel_input", "continue_queue", "request_stop"]
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn control_command_failure_rolls_back_flags_pause_and_commands(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let head = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    let before = snapshot(&pool, seed.conversation_id).await;
    sqlx::query("alter table events add constraint reject_control_events check (kind not in ('input_state_changed', 'queue_paused')) not valid")
        .execute(&pool).await?;
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::Db(_))
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::Db(_))
    ));
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    sqlx::query("alter table events drop constraint reject_control_events")
        .execute(&pool)
        .await?;
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            head.input_id,
            "cancel",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::CancelRequested { .. }
    ));
    assert!(matches!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "stop",
            Uuid::now_v7()
        )
        .await?,
        StopOutcome::Requested {
            interrupt_command_id: Some(_),
            withdraw_command_id: Some(_)
        }
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_conflicts_across_queues_roll_back_loser(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let other = Seed {
        conversation_id: testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await,
        ..seed
    };
    running(&pool, &seed).await;
    running(&pool, &other).await;
    let before = snapshot(&pool, seed.conversation_id).await;
    let other_before = snapshot(&pool, other.conversation_id).await;
    let (first, second) = tokio::join!(
        request_stop(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "same-key",
            Uuid::now_v7()
        ),
        request_stop(
            &pool,
            seed.owner_id,
            other.conversation_id,
            "same-key",
            Uuid::now_v7()
        )
    );
    let (winner, loser, loser_before) = match (first, second) {
        (Ok(_), Err(ControlError::IdempotencyConflict)) => {
            (seed.conversation_id, other.conversation_id, other_before)
        }
        (Err(ControlError::IdempotencyConflict), Ok(_)) => {
            (other.conversation_id, seed.conversation_id, before)
        }
        other => panic!("unexpected key race outcome: {other:?}"),
    };
    assert_eq!(pauses(&pool, winner).await, [PauseReason::UserStop]);
    assert_eq!(command_count(&pool, winner, "interrupt").await, 1);
    assert_eq!(snapshot(&pool, loser).await, loser_before);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn replays_wait_for_queue_lock(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let head = input(&pool, &seed).await;
    let cancel_result = cancel_input(
        &pool,
        seed.owner_id,
        head.input_id,
        "cancel",
        Uuid::now_v7(),
    )
    .await?;
    let stop_result = request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await?;
    let continue_result = continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await?;
    let before = snapshot(&pool, seed.conversation_id).await;
    let mut tx = pool.begin().await?;
    sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
        .bind(seed.conversation_id)
        .execute(&mut *tx)
        .await?;
    let mut cancel = {
        let pool = pool.clone();
        tokio::spawn(async move {
            cancel_input(
                &pool,
                seed.owner_id,
                head.input_id,
                "cancel",
                Uuid::now_v7(),
            )
            .await
        })
    };
    let stop = {
        let pool = pool.clone();
        tokio::spawn(async move {
            request_stop(
                &pool,
                seed.owner_id,
                seed.conversation_id,
                "stop",
                Uuid::now_v7(),
            )
            .await
        })
    };
    let resume = {
        let pool = pool.clone();
        tokio::spawn(async move {
            continue_queue(
                &pool,
                seed.owner_id,
                seed.conversation_id,
                "continue",
                Uuid::now_v7(),
            )
            .await
        })
    };
    assert!(
        timeout(Duration::from_millis(100), &mut cancel)
            .await
            .is_err()
    );
    assert!(!stop.is_finished());
    assert!(!resume.is_finished());
    tx.commit().await?;
    let (cancel, stop, resume) = timeout(Duration::from_secs(10), async {
        tokio::join!(cancel, stop, resume)
    })
    .await?;
    assert_eq!(cancel??, cancel_result);
    assert_eq!(stop??, stop_result);
    assert_eq!(resume??, continue_result);
    assert_eq!(snapshot(&pool, seed.conversation_id).await, before);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn closed_outbox_channel_releases_a_steer_so_cancel_succeeds(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set harness = 'codex' where id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first.command_id).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second.input_id).await?
    else {
        panic!("send now must create a steer");
    };
    let row: canterlot_hub::outbox::CommandRow = sqlx::query_as(
        "select id, machine_id, conversation_id, execution_id, execution_generation,
                kind, payload, dispatch_attempt
         from commands where id = $1",
    )
    .bind(steer_command_id)
    .fetch_one(&pool)
    .await?;
    let claim = canterlot_hub::outbox::claim_for_send(&pool, &row)
        .await?
        .expect("steer is still pending");
    assert!(claim.newly_claimed);
    let claimed = claim.row;
    let command = canterlot_hub::outbox::to_proto(&claimed)?;
    // Канал уже закрыт: команда точно не попала в транспорт.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    assert!(matches!(
        canterlot_hub::outbox::hand_off(&pool, &claimed, command, &tx, claim.newly_claimed).await,
        canterlot_hub::outbox::HandOff::Closed
    ));
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            second.input_id,
            "cancel-closed",
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::Cancelled
    );
    let (delivery, result): (String, Option<Value>) =
        sqlx::query_as("select delivery_state, result from commands where id = $1")
            .bind(steer_command_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(delivery, "completed");
    assert_eq!(result.unwrap()["cancelled"], json!(true));
    assert!(
        canterlot_hub::outbox::claim_for_send(&pool, &row)
            .await?
            .is_none(),
        "cancelled steer is never sent"
    );
    Ok(())
}

/// Повтор после переподключения или рестарта хаба застаёт захват прошлой
/// отправки: переполненный канал его не снимает, отмена видит вливание в пути.
#[sqlx::test(migrations = "./migrations")]
async fn full_channel_on_resend_keeps_an_earlier_claim(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set harness = 'codex' where id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    running(&pool, &seed).await;
    let first = input(&pool, &seed).await;
    let second = input(&pool, &seed).await;
    dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first.command_id).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second.input_id).await?
    else {
        panic!("send now must create a steer");
    };
    let row: canterlot_hub::outbox::CommandRow = sqlx::query_as(
        "select id, machine_id, conversation_id, execution_id, execution_generation,
                kind, payload, dispatch_attempt
         from commands where id = $1",
    )
    .bind(steer_command_id)
    .fetch_one(&pool)
    .await?;
    // Первое соединение: команда ушла в транспорт.
    let first_claim = canterlot_hub::outbox::claim_for_send(&pool, &row)
        .await?
        .expect("steer is still pending");
    assert!(first_claim.newly_claimed);
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let command = canterlot_hub::outbox::to_proto(&first_claim.row)?;
    assert!(matches!(
        canterlot_hub::outbox::hand_off(&pool, &first_claim.row, command, &tx, true).await,
        canterlot_hub::outbox::HandOff::Sent
    ));
    // Новое соединение, канал переполнен повтором остальных команд.
    let resend = canterlot_hub::outbox::claim_for_send(&pool, &row)
        .await?
        .expect("steer is still pending");
    assert!(!resend.newly_claimed, "claim set by an earlier send");
    let (full, _full_rx) = tokio::sync::mpsc::channel(1);
    full.try_send(Default::default()).unwrap();
    let command = canterlot_hub::outbox::to_proto(&resend.row)?;
    assert!(matches!(
        canterlot_hub::outbox::hand_off(&pool, &resend.row, command, &full, resend.newly_claimed)
            .await,
        canterlot_hub::outbox::HandOff::Full
    ));
    assert!(matches!(
        cancel_input(
            &pool,
            seed.owner_id,
            second.input_id,
            "cancel-full",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::InvalidRequest {
            reason: "steer_in_flight"
        })
    ));
    Ok(())
}
