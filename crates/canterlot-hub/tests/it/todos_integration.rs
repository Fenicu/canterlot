use canterlot_hub::{
    events::StoredEvent,
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    snapshot,
    testkit::{self, Seed},
    todos::{TodoItemView, TodoListView, TodoStatus as ViewStatus},
};
use canterlot_proto::worker::v1::{
    JournalOpened, JournalRecord, JournalRecords, RecordBody, TodoItem, TodoStatus, TodoUpdated,
    record_body::Body,
};
use serde_json::json;
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

fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.into(),
        status: status as i32,
    }
}

fn fact(
    execution: &executions::StartedExecution,
    revision: u64,
    lane: Option<Uuid>,
    items: Vec<TodoItem>,
) -> Body {
    Body::TodoUpdated(TodoUpdated {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        revision,
        lane_block_id: lane.map(|id| id.to_string()).unwrap_or_default(),
        items,
        omitted: 0,
    })
}

async fn todo_events(pool: &PgPool, conversation: Uuid) -> Vec<StoredEvent> {
    sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'todo_updated' order by scope_seq",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn lists(pool: &PgPool, seed: &Seed) -> Vec<TodoListView> {
    snapshot::conversation(pool, seed.owner_id, seed.conversation_id, 50)
        .await
        .unwrap()
        .unwrap()
        .data
        .todos
}

fn view(text: &str, status: ViewStatus) -> TodoItemView {
    TodoItemView {
        text: text.into(),
        status,
    }
}

#[tokio::test]
async fn list_is_stored_published_and_comes_back_in_the_snapshot() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let batch = records(
        Uuid::now_v7(),
        1,
        vec![
            opened(&execution),
            fact(
                &execution,
                2,
                None,
                vec![
                    item("план", TodoStatus::Completed),
                    item("тесты", TodoStatus::InProgress),
                    item("выкатка", TodoStatus::Pending),
                ],
            ),
        ],
    );
    assert_eq!(
        apply(&pool, &seed, &batch).await,
        IngestOutcome::Committed { up_to_seq: 2 }
    );

    let events = todo_events(&pool, seed.conversation_id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].execution_id, Some(execution.execution_id));
    assert_eq!(
        events[0].payload,
        json!({
            "lane_block_id": null,
            "revision": 1,
            "items": [
                {"text": "план", "status": "completed"},
                {"text": "тесты", "status": "in_progress"},
                {"text": "выкатка", "status": "pending"},
            ],
            "omitted": 0,
        })
    );
    assert_eq!(
        lists(&pool, &seed).await,
        vec![TodoListView {
            lane_block_id: None,
            revision: 1,
            items: vec![
                view("план", ViewStatus::Completed),
                view("тесты", ViewStatus::InProgress),
                view("выкатка", ViewStatus::Pending),
            ],
            omitted: 0,
        }]
    );
}
