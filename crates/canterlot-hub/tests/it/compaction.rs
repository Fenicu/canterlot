#![forbid(unsafe_code)]

//! Застрявшая сессия: факт воркера ставит очередь на паузу, владелец просит сжатие,
//! итог сжатия снимает паузу.

use canterlot_hub::{
    delivery::control::{self, CompactOutcome, ControlError},
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::{CompactionProgress, ExecutionState, PauseReason},
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    CompactionState, CompactionStatus, JournalOpened, JournalRecord, JournalRecords, RecordBody,
    SessionStuck, record_body::Body,
};
use sqlx::PgPool;
use uuid::Uuid;

fn records(journal_id: Uuid, from: u64, bodies: Vec<Body>) -> JournalRecords {
    JournalRecords {
        journal_id: journal_id.to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(offset, body)| JournalRecord {
                seq: from + offset as u64,
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

struct Run {
    seed: Seed,
    execution: executions::StartedExecution,
    journal: Uuid,
    seq: u64,
}

impl Run {
    async fn start(pool: &PgPool) -> Self {
        let seed = testkit::seed(pool).await;
        let execution =
            executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        let journal = Uuid::now_v7();
        let mut run = Self {
            seed,
            execution,
            journal,
            seq: 0,
        };
        let opened = Body::JournalOpened(JournalOpened {
            execution_id: run.execution.execution_id.to_string(),
            generation: run.execution.generation as u64,
            supervisor_protocol_version: 1,
        });
        run.send(pool, opened).await;
        run
    }

    async fn send(&mut self, pool: &PgPool, body: Body) {
        self.seq += 1;
        let batch = records(self.journal, self.seq, vec![body]);
        assert_eq!(
            apply(pool, &self.seed, &batch).await,
            IngestOutcome::Committed {
                up_to_seq: self.seq
            }
        );
    }

    async fn stuck(&mut self, pool: &PgPool) {
        let fact = Body::SessionStuck(SessionStuck {
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
        });
        self.send(pool, fact).await;
    }

    async fn status(&mut self, pool: &PgPool, command: Uuid, state: CompactionState) {
        let fact = Body::CompactionStatus(CompactionStatus {
            command_id: command.to_string(),
            dispatch_attempt: 0,
            state: state.into(),
        });
        self.send(pool, fact).await;
    }

    async fn pauses(&self, pool: &PgPool) -> Vec<PauseReason> {
        let row: (sqlx::types::Json<Vec<PauseReason>>,) =
            sqlx::query_as("select pause_reasons from queue_state where conversation_id = $1")
                .bind(self.seed.conversation_id)
                .fetch_one(pool)
                .await
                .unwrap();
        row.0.0
    }

    async fn compact(&self, pool: &PgPool, key: &str) -> Result<CompactOutcome, ControlError> {
        control::request_compaction(
            pool,
            self.seed.owner_id,
            self.seed.conversation_id,
            key,
            Uuid::now_v7(),
        )
        .await
    }
}

fn stuck(compaction: Option<CompactionProgress>) -> PauseReason {
    PauseReason::SessionStuck { compaction }
}

fn id_of(outcome: CompactOutcome) -> Uuid {
    match outcome {
        CompactOutcome::Requested { command_id }
        | CompactOutcome::AlreadyRequested { command_id } => command_id,
    }
}

const KEY: &str = "0191d2f0-0000-7000-8000-000000000001";
const OTHER_KEY: &str = "0191d2f0-0000-7000-8000-000000000002";

#[tokio::test]
async fn request_makes_one_command_and_a_repeat_changes_nothing() {
    let pool = canterlot_hub::testkit::db().await;
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    let first = run.compact(&pool, KEY).await.unwrap();
    let CompactOutcome::Requested { command_id } = first else {
        panic!("{first:?}");
    };
    assert_eq!(
        run.pauses(&pool).await,
        vec![stuck(Some(CompactionProgress::Requested))]
    );
    assert_eq!(run.compact(&pool, KEY).await.unwrap(), first, "same key");
    assert_eq!(
        run.compact(&pool, OTHER_KEY).await.unwrap(),
        CompactOutcome::AlreadyRequested { command_id }
    );
    let row: (String, Uuid, i64) = sqlx::query_as(
        "select kind, execution_id, execution_generation from commands where id = $1",
    )
    .bind(command_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "compact_session".to_owned(),
            run.execution.execution_id,
            run.execution.generation
        )
    );
}

#[tokio::test]
async fn finished_compaction_lifts_the_pause() {
    let pool = canterlot_hub::testkit::db().await;
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    let command = id_of(run.compact(&pool, KEY).await.unwrap());
    run.status(&pool, command, CompactionState::SentToAgent)
        .await;
    assert_eq!(
        run.pauses(&pool).await,
        vec![stuck(Some(CompactionProgress::Requested))]
    );
    run.status(&pool, command, CompactionState::Completed).await;
    assert_eq!(run.pauses(&pool).await, vec![]);
    let row: (String, serde_json::Value) =
        sqlx::query_as("select delivery_state, result from commands where id = $1")
            .bind(command)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, "completed");
    assert_eq!(row.1["status"], "completed");
}
