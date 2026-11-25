use canterlot_hub::{
    background_tasks::BackgroundTaskView,
    events::StoredEvent,
    executions, history,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    snapshot,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    BackgroundTask, BackgroundTaskKind, BackgroundTaskState, BackgroundWorkChanged, BlockFinal,
    BlockStarted, JournalOpened, JournalRecord, JournalRecords, RecordBody, record_body::Body,
};
use sqlx::PgPool;
use uuid::Uuid;

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

#[tokio::test]
async fn ingest_background_work_updates_tasks_and_emits_event() {
    let pool = canterlot_hub::testkit::db().await;
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

// --- состояние вызова субагента ---

async fn claude_session(pool: &PgPool, seed: &Seed) {
    sqlx::query("update conversations set harness = 'claude_code' where id = $1")
        .bind(seed.conversation_id)
        .execute(pool)
        .await
        .unwrap();
}

fn block(id: Uuid, parent: Option<Uuid>, kind: &str, tool: &str, summary: &str) -> Vec<Body> {
    vec![
        Body::BlockStarted(BlockStarted {
            block_id: id.to_string(),
            rev: 1,
            kind: kind.into(),
            parent_block_id: parent.map(|p| p.to_string()).unwrap_or_default(),
            agent: "orchestrator".into(),
            model: "m".into(),
            tool_name: tool.into(),
            ..Default::default()
        }),
        Body::BlockFinal(BlockFinal {
            block_id: id.to_string(),
            rev: 2,
            checksum: canterlot_hub::blocks::checksum(""),
            summary: summary.into(),
            ..Default::default()
        }),
    ]
}

async fn node(pool: &PgPool, seed: &Seed, id: Uuid) -> (String, bool) {
    let mut conn = pool.acquire().await.unwrap();
    let nodes = history::delegation(&mut conn, seed.owner_id, seed.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let node = nodes.iter().find(|n| n.block_id == id).unwrap();
    (node.state.clone(), node.finished_at.is_some())
}

#[tokio::test]
async fn delegation_state_is_running_for_launched_background_agent() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    claude_session(&pool, &seed).await;
    let run = running(&pool, &seed).await;
    let (call, journal) = (Uuid::now_v7(), Uuid::now_v7());
    let mut bodies = vec![opened(&run)];
    bodies.extend(block(call, None, "tool_call", "Agent", "Agent: bg"));
    bodies.extend(block(
        Uuid::now_v7(),
        Some(call),
        "tool_result",
        "",
        "async_launched",
    ));
    bodies.push(fact(
        &run,
        6,
        vec![task("a1", Some(call), BackgroundTaskState::Running)],
        false,
    ));
    apply(&pool, &seed, &records(journal, 1, bodies)).await;
    assert_eq!(node(&pool, &seed, call).await, ("running".into(), false));
}
