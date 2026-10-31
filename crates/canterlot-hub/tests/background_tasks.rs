use canterlot_hub::{
    activity::{self, Thresholds},
    background_tasks::BackgroundTaskView,
    events::StoredEvent,
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    snapshot,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    BackgroundTask, BackgroundTaskKind, BackgroundTaskState, BackgroundWorkChanged, JournalOpened,
    JournalRecord, JournalRecords, RecordBody, record_body::Body,
};
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

const THRESHOLDS: Thresholds = Thresholds {
    stalled_generating: Duration::from_secs(600),
    stalled_tool: Duration::from_secs(600),
};

fn opened(execution: &executions::StartedExecution) -> Body {
    Body::JournalOpened(JournalOpened {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        supervisor_protocol_version: 1,
    })
}

fn records(journal_id: Uuid, from: u64, bodies: Vec<Body>) -> JournalRecords {
    JournalRecords {
        journal_id: journal_id.to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| JournalRecord {
                seq: from + i as u64,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    }
}

async fn apply(pool: &PgPool, seed: &Seed, batch: &JournalRecords) -> IngestOutcome {
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            batch,
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

fn task(id: &str, block: Option<Uuid>, state: BackgroundTaskState) -> BackgroundTask {
    BackgroundTask {
        task_id: id.into(),
        kind: BackgroundTaskKind::Agent as i32,
        block_id: block.map(|b| b.to_string()).unwrap_or_default(),
        parent_task_id: String::new(),
        description: "bg sub".into(),
        state: state as i32,
    }
}

fn fact(
    execution: &executions::StartedExecution,
    revision: u64,
    tasks: Vec<BackgroundTask>,
    wake_turn: bool,
) -> Body {
    Body::BackgroundWorkChanged(BackgroundWorkChanged {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        revision,
        machine_time_ms: 1_793_000_000_000 + revision,
        tasks,
        omitted: 0,
        wake_turn,
    })
}

async fn background(pool: &PgPool, seed: &Seed) -> Vec<BackgroundTaskView> {
    snapshot::conversation(pool, seed.owner_id, seed.conversation_id, 50)
        .await
        .unwrap()
        .unwrap()
        .data
        .background
}

async fn events(pool: &PgPool, conversation: Uuid) -> Vec<StoredEvent> {
    sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'background_work_changed' order by scope_seq",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn activity_evidence(pool: &PgPool, seed: &Seed) -> (String, Value) {
    activity::refresh(
        pool,
        seed.conversation_id,
        Utc::now(),
        &THRESHOLDS,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sqlx::query_as(
        "select activity_state, activity_evidence from conversation_derived where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ingest_background_work_updates_tasks_and_emits_event(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    let block = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![task("a1", Some(block), BackgroundTaskState::Running)],
                    false,
                ),
            ],
        ),
    )
    .await;
    let tasks = background(&pool, &seed).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, "running");
    assert_eq!(tasks[0].kind, "agent");
    assert_eq!(tasks[0].block_id, Some(block));
    assert_eq!(tasks[0].ended_at, None);
    let emitted = events(&pool, seed.conversation_id).await;
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].payload["tasks"][0]["task_id"], "a1");

    apply(
        &pool,
        &seed,
        &records(
            journal,
            3,
            vec![fact(
                &run,
                3,
                vec![task("a1", None, BackgroundTaskState::Failed)],
                false,
            )],
        ),
    )
    .await;
    let tasks = background(&pool, &seed).await;
    assert_eq!(tasks[0].state, "failed");
    assert_eq!(tasks[0].block_id, Some(block), "the link is kept");
    assert!(tasks[0].ended_at.is_some());
    assert_eq!(events(&pool, seed.conversation_id).await.len(), 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_task_id_runs_again_after_it_ended(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![task("a1", None, BackgroundTaskState::Running)],
                    false,
                ),
                fact(
                    &run,
                    3,
                    vec![task("a1", None, BackgroundTaskState::Completed)],
                    false,
                ),
                fact(
                    &run,
                    4,
                    vec![task("a1", None, BackgroundTaskState::Running)],
                    false,
                ),
            ],
        ),
    )
    .await;
    let tasks = background(&pool, &seed).await;
    assert_eq!(tasks[0].state, "running");
    assert_eq!(tasks[0].ended_at, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn task_missing_from_the_full_list_is_stopped_unless_the_list_was_cut(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    let cut = Body::BackgroundWorkChanged(BackgroundWorkChanged {
        execution_id: run.execution_id.to_string(),
        generation: run.generation as u64,
        revision: 3,
        machine_time_ms: 1_793_000_000_003,
        tasks: vec![task("b1", None, BackgroundTaskState::Running)],
        omitted: 1,
        wake_turn: false,
    });
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![task("a1", None, BackgroundTaskState::Running)],
                    false,
                ),
                cut,
            ],
        ),
    )
    .await;
    let state = |tasks: &[BackgroundTaskView], id: &str| {
        tasks
            .iter()
            .find(|t| t.task_id == id)
            .unwrap()
            .state
            .clone()
    };
    let tasks = background(&pool, &seed).await;
    assert_eq!(state(&tasks, "a1"), "running");
    apply(
        &pool,
        &seed,
        &records(
            journal,
            4,
            vec![fact(
                &run,
                4,
                vec![task("b1", None, BackgroundTaskState::Running)],
                false,
            )],
        ),
    )
    .await;
    let tasks = background(&pool, &seed).await;
    assert_eq!(state(&tasks, "a1"), "stopped");
    assert_eq!(state(&tasks, "b1"), "running");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_session_reports_how_many_tasks_run(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![
                        task("a1", None, BackgroundTaskState::Running),
                        task("b1", None, BackgroundTaskState::Running),
                    ],
                    false,
                ),
            ],
        ),
    )
    .await;
    let (state, evidence) = activity_evidence(&pool, &seed).await;
    assert_eq!(state, "idle");
    assert_eq!(evidence["background_work"], 2);

    apply(
        &pool,
        &seed,
        &records(journal, 3, vec![fact(&run, 3, vec![], false)]),
    )
    .await;
    let (state, evidence) = activity_evidence(&pool, &seed).await;
    assert_eq!(state, "idle");
    assert!(evidence["background_work"].is_null(), "{evidence}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_turn_flag_makes_the_session_working_and_clears(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(journal, 1, vec![opened(&run), fact(&run, 2, vec![], true)]),
    )
    .await;
    assert_eq!(activity_evidence(&pool, &seed).await.0, "working");
    assert_eq!(
        events(&pool, seed.conversation_id).await.len(),
        1,
        "the flag alone is an event: activity refreshes on it"
    );
    apply(
        &pool,
        &seed,
        &records(journal, 3, vec![fact(&run, 3, vec![], false)]),
    )
    .await;
    assert_eq!(activity_evidence(&pool, &seed).await.0, "idle");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn tasks_of_a_finished_execution_are_stopped_for_readers(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![task("a1", None, BackgroundTaskState::Running)],
                    false,
                ),
            ],
        ),
    )
    .await;
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("user_stop")),
    ] {
        executions::set_state(&pool, run.execution_id, state, outcome, Uuid::now_v7())
            .await
            .unwrap();
    }
    assert_eq!(background(&pool, &seed).await[0].state, "stopped");
    let (_, evidence) = activity_evidence(&pool, &seed).await;
    assert!(
        evidence.get("background_work").is_none_or(Value::is_null),
        "{evidence}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleted_conversation_takes_its_tasks(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let run = running(&pool, &seed).await;
    apply(
        &pool,
        &seed,
        &records(
            Uuid::now_v7(),
            1,
            vec![
                opened(&run),
                fact(
                    &run,
                    2,
                    vec![task("a1", None, BackgroundTaskState::Running)],
                    false,
                ),
            ],
        ),
    )
    .await;
    let cascades: Vec<(String,)> = sqlx::query_as(
        "select confrelid::regclass::text from pg_constraint
         where conrelid = 'background_tasks'::regclass and contype = 'f' and confdeltype = 'c'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(cascades, vec![("conversations".to_owned(),)]);
}
