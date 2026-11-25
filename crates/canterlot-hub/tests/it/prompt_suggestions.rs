use canterlot_hub::{
    events::StoredEvent,
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    snapshot,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    JournalOpened, JournalRecord, JournalRecords, PromptSuggested, RecordBody, record_body::Body,
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

fn suggested(execution: &executions::StartedExecution, origin: Uuid, text: &str) -> Body {
    Body::PromptSuggested(PromptSuggested {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        text: text.into(),
        input_command_id: origin.to_string(),
    })
}

/// Принятый ход разговора; возвращает команду его ввода, к которой привязывается подсказка.
async fn turn(pool: &PgPool, seed: &Seed) -> Uuid {
    testkit::seed_recorded_turn(pool, seed.conversation_id).await;
    // Следующий ввод владельца получает номер после записанного хода.
    sqlx::query(
        "update queue_state set next_input_seq =
           (select max(input_seq) + 1 from inputs where conversation_id = $1)
         where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
    latest_turn(pool, seed).await
}

async fn latest_turn(pool: &PgPool, seed: &Seed) -> Uuid {
    sqlx::query_scalar(
        "select command_id from inputs where conversation_id = $1
         order by input_seq desc limit 1",
    )
    .bind(seed.conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
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

async fn claude_seed(pool: &PgPool) -> Seed {
    let seed = testkit::seed(pool).await;
    set_harness(pool, seed.conversation_id, "claude_code").await;
    seed
}

async fn set_harness(pool: &PgPool, conversation: Uuid, harness: &str) {
    sqlx::query("update conversations set harness = $2 where id = $1")
        .bind(conversation)
        .bind(harness)
        .execute(pool)
        .await
        .unwrap();
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

/// Ход принят, журнал открыт, подсказка записана: возвращает журнал для следующих записей.
async fn suggest(
    pool: &PgPool,
    seed: &Seed,
    execution: &executions::StartedExecution,
    text: &str,
) -> Uuid {
    let origin = turn(pool, seed).await;
    let journal = Uuid::now_v7();
    assert_eq!(
        apply(
            pool,
            seed,
            &records(
                journal,
                1,
                vec![opened(execution), suggested(execution, origin, text)]
            )
        )
        .await,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    journal
}

async fn events(pool: &PgPool, conversation: Uuid) -> Vec<StoredEvent> {
    sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'prompt_suggestion_changed'
         order by scope_seq",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn stored(pool: &PgPool, seed: &Seed) -> Option<String> {
    snapshot::conversation(pool, seed.owner_id, seed.conversation_id, 50)
        .await
        .unwrap()
        .unwrap()
        .data
        .prompt_suggestion
        .map(|view| view.text)
}

#[tokio::test]
async fn suggestion_is_stored_published_and_comes_back_in_the_snapshot() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    suggest(&pool, &seed, &execution, "  add tests  ").await;
    assert_eq!(stored(&pool, &seed).await.as_deref(), Some("add tests"));
    let published = events(&pool, seed.conversation_id).await;
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].execution_id, Some(execution.execution_id));
    assert_eq!(published[0].payload["suggestion"]["text"], "add tests");
    assert_eq!(
        published[0].payload["conversation_id"],
        json!(seed.conversation_id)
    );
}
