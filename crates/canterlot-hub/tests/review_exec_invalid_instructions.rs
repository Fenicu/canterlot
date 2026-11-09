//! Review: the worker ends an execution with `stopping -> failed(invalid_instructions)`
//! (supervisor/run.rs `StopCause::InvalidInstructions`), but the hub's transition table
//! does not list that outcome, so the execution stays `stopping` (live) forever.

use canterlot_hub::{
    executions::{self, ExecutionError},
    model::ExecutionState,
    testkit,
};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
#[ignore = "bug: исход failed(invalid_instructions) отвергается, исполнение висит в stopping"]
async fn worker_invalid_instructions_outcome_is_accepted(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let started = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    // Worker order from begin_stop + finish: starting, stopping, failed(invalid_instructions).
    for next in [ExecutionState::Starting, ExecutionState::Stopping] {
        executions::set_state(&pool, started.execution_id, next, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let result = executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Failed,
        Some("invalid_instructions"),
        Uuid::now_v7(),
    )
    .await;
    assert!(
        !matches!(result, Err(ExecutionError::InvalidTransition { .. })),
        "hub rejected the worker's terminal fact: {result:?}"
    );
    let state: String = sqlx::query_scalar("select state from executions where id=$1")
        .bind(started.execution_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "failed", "execution is stuck live in `{state}`");
}
