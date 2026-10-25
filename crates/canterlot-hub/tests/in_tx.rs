use canterlot_hub::{
    delivery::{
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        receipts::{
            ApplyOutcome, InputReceipt, apply_input_receipt_in_tx, apply_run_finished_in_tx,
        },
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    events::StoredEvent,
    executions::{self, StartedExecution, set_state_in_tx},
    model::{ExecutionState, InputState},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn dispatched_input(
    pool: &PgPool,
) -> Result<(Seed, StartedExecution, SubmittedInput), Box<dyn std::error::Error + Send + Sync>> {
    let seed = testkit::seed(pool).await;
    let execution =
        executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7()).await?;
    }
    let input = submit_input(
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
    .await?;
    assert!(matches!(
        dispatch_next(
            pool,
            seed.conversation_id,
            GateInputs {
                machine_available: true,
            },
            Uuid::now_v7(),
        )
        .await?,
        DispatchOutcome::Dispatched { input_id, command_id, attempt: 1, .. }
            if input_id == input.input_id && command_id == input.command_id
    ));
    Ok((seed, execution, input))
}

async fn snapshot(pool: &PgPool, input_id: Uuid) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar(
        "select jsonb_build_object('input', to_jsonb(i), 'command', to_jsonb(c),
                                  'queue', to_jsonb(q), 'events',
                                  (select count(*) from events where conversation_id = i.conversation_id),
                                  'last_seq', (select last_seq from scope_counters
                                               where scope = 'conversation:' || i.conversation_id::text))
         from inputs i join commands c on c.id = i.command_id
         join queue_state q on q.conversation_id = i.conversation_id where i.id = $1",
    )
    .bind(input_id)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrations = "./migrations")]
async fn receipt_in_tx_rolls_back_with_caller(pool: PgPool) -> TestResult {
    let (_, _, input) = dispatched_input(&pool).await?;
    let before = snapshot(&pool, input.input_id).await?;
    assert_eq!(before["input"]["state"], "dispatched");
    let correlation_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    assert_eq!(
        apply_input_receipt_in_tx(
            &mut tx,
            input.command_id,
            1,
            InputReceipt::Stored,
            correlation_id,
        )
        .await?,
        ApplyOutcome::Applied {
            input_state: InputState::OnMachine,
        }
    );
    let state: String = sqlx::query_scalar("select state from inputs where id = $1")
        .bind(input.input_id)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(state, "on_machine");
    let events: i64 = sqlx::query_scalar("select count(*) from events where correlation_id = $1")
        .bind(correlation_id)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(events, 1);
    tx.rollback().await?;
    assert_eq!(snapshot(&pool, input.input_id).await?, before);
    let events: i64 = sqlx::query_scalar("select count(*) from events where correlation_id = $1")
        .bind(correlation_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(events, 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn set_state_in_tx_composes(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let correlation_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        set_state_in_tx(&mut tx, execution.execution_id, state, None, correlation_id).await?;
    }
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "created");
    let events: i64 = sqlx::query_scalar("select count(*) from events where correlation_id = $1")
        .bind(correlation_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(events, 0);
    tx.commit().await?;
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "running");
    let events: Vec<StoredEvent> = sqlx::query_as(
        "select * from events where correlation_id = $1 and kind = 'execution_state_changed'
         order by scope_seq",
    )
    .bind(correlation_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(events.len(), 2);
    for (event, state) in events.iter().zip(["starting", "running"]) {
        assert_eq!(
            event.scope,
            format!("conversation:{}", seed.conversation_id)
        );
        assert_eq!(event.conversation_id, Some(seed.conversation_id));
        assert_eq!(event.execution_id, Some(execution.execution_id));
        assert_eq!(event.correlation_id, correlation_id);
        assert_eq!(event.payload["state"], state);
        assert_eq!(event.payload["generation"], execution.generation);
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn run_finished_in_tx_composes(pool: PgPool) -> TestResult {
    let (seed, execution, input) = dispatched_input(&pool).await?;
    let before = snapshot(&pool, input.input_id).await?;
    let correlation_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    assert_eq!(
        apply_input_receipt_in_tx(
            &mut tx,
            input.command_id,
            1,
            InputReceipt::Accepted,
            correlation_id,
        )
        .await?,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted,
        }
    );
    let slot: (String, Option<Uuid>) = sqlx::query_as(
        "select run_slot, run_slot_input_id from queue_state where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    assert_eq!(slot, ("running".into(), Some(input.input_id)));
    apply_run_finished_in_tx(
        &mut tx,
        execution.execution_id,
        input.command_id,
        correlation_id,
    )
    .await?;
    assert_eq!(snapshot(&pool, input.input_id).await?, before);
    tx.commit().await?;
    let after = snapshot(&pool, input.input_id).await?;
    assert_eq!(after["input"]["state"], "accepted");
    assert_eq!(after["command"]["delivery_state"], "completed");
    assert_eq!(after["queue"]["run_slot"], "free");
    assert!(after["queue"]["run_slot_input_id"].is_null());
    assert!(after["queue"]["run_slot_attempt"].is_null());
    let events: Vec<(String, Value)> = sqlx::query_as(
        "select kind, payload from events where correlation_id = $1
         and kind in ('input_state_changed', 'run_finished') order by scope_seq",
    )
    .bind(correlation_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "input_state_changed");
    assert_eq!(events[0].1["state"], "accepted");
    assert_eq!(events[1].0, "run_finished");
    assert_eq!(events[1].1["input_id"], input.input_id.to_string());
    assert_eq!(
        events[1].1["input_command_id"],
        input.command_id.to_string()
    );
    Ok(())
}
