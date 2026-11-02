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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stuck_fact_pauses_the_queue_once_and_names_the_reason(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    run.stuck(&pool).await;
    assert_eq!(run.pauses(&pool).await, vec![stuck(None)]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn request_makes_one_command_and_a_repeat_changes_nothing(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn finished_compaction_lifts_the_pause(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_compaction_keeps_the_pause_and_may_be_asked_again(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    let command = id_of(run.compact(&pool, KEY).await.unwrap());
    run.status(&pool, command, CompactionState::Failed).await;
    assert_eq!(
        run.pauses(&pool).await,
        vec![stuck(Some(CompactionProgress::Failed))]
    );
    let again = id_of(run.compact(&pool, OTHER_KEY).await.unwrap());
    assert_ne!(again, command);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn compaction_works_without_a_stuck_session(pool: PgPool) {
    let run = Run::start(&pool).await;
    let command = id_of(run.compact(&pool, KEY).await.unwrap());
    // Вне паузы очередь не трогаем: причин как не было, так и нет.
    assert_eq!(run.pauses(&pool).await, vec![]);
    let kind: String = sqlx::query_scalar("select kind from commands where id = $1")
        .bind(command)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "compact_session");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn compaction_needs_a_live_execution(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    executions::set_state(
        &pool,
        run.execution.execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    executions::set_state(
        &pool,
        run.execution.execution_id,
        ExecutionState::Stopped,
        Some("normal_exit"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        run.compact(&pool, KEY).await,
        Err(ControlError::InvalidRequest {
            reason: "no_live_execution"
        })
    ));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dead_execution_releases_a_requested_compaction(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    run.compact(&pool, KEY).await.unwrap();
    executions::set_state(
        &pool,
        run.execution.execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    executions::set_state(
        &pool,
        run.execution.execution_id,
        ExecutionState::Failed,
        Some("crashed"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(
        run.pauses(&pool)
            .await
            .contains(&stuck(Some(CompactionProgress::Failed)))
    );
}

// Без согласованной возможности старый воркер получил бы команду, которой не знает.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn command_goes_only_to_a_session_that_agreed_to_it(pool: PgPool) {
    use canterlot_hub::{
        config::HubTimers,
        grpc::KNOWN_OPTIONAL_CAPABILITIES,
        outbox::{self, CommandRow, OutboxError},
        session::Registry,
    };
    use canterlot_proto::worker::v1::command;
    assert!(KNOWN_OPTIONAL_CAPABILITIES.contains(&"compact_v1"));
    let mut run = Run::start(&pool).await;
    run.stuck(&pool).await;
    let id = id_of(run.compact(&pool, KEY).await.unwrap());
    let row: CommandRow = sqlx::query_as("select * from commands where id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let registry = Registry::new(HubTimers::default());
    let old = testkit::fake_session(&registry, row.machine_id, &["pilot_v1"], true);
    assert!(matches!(
        outbox::to_proto_for_session(&row, &registry),
        Err(OutboxError::CapabilityMissing)
    ));
    drop(old);
    let registry = Registry::new(HubTimers::default());
    let _new = testkit::fake_session(&registry, row.machine_id, &["compact_v1"], true);
    let command = outbox::to_proto_for_session(&row, &registry).unwrap();
    assert!(matches!(
        command.kind,
        Some(command::Kind::CompactSession(_))
    ));
}

fn compaction_event(
    id: Uuid,
    execution: Uuid,
    generation: u64,
    started: bool,
    manual: bool,
    ok: bool,
    pre_tokens: u64,
) -> Body {
    Body::CompactionEvent(canterlot_proto::worker::v1::CompactionEvent {
        id: id.to_string(),
        execution_id: execution.to_string(),
        generation,
        started,
        manual,
        ok,
        pre_tokens,
    })
}

async fn event_kinds(pool: &PgPool, conversation: Uuid) -> Vec<String> {
    sqlx::query_scalar("select kind from events where scope = $1 order by scope_seq")
        .bind(format!("conversation:{conversation}"))
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn compaction_episode_is_a_durable_timeline_row(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    let id = Uuid::now_v7();
    let start = compaction_event(
        id,
        run.execution.execution_id,
        run.execution.generation as u64,
        true,
        false,
        false,
        0,
    );
    run.send(&pool, start).await;
    // Повторный старт эпизода запись не дублирует.
    run.send(
        &pool,
        compaction_event(
            id,
            run.execution.execution_id,
            run.execution.generation as u64,
            true,
            false,
            false,
            0,
        ),
    )
    .await;
    let row: (String, Option<bool>, Option<i64>) =
        sqlx::query_as("select trigger, ok, pre_tokens from compactions where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("auto".to_owned(), None, None));
    run.send(
        &pool,
        compaction_event(
            id,
            run.execution.execution_id,
            run.execution.generation as u64,
            false,
            false,
            true,
            980_000,
        ),
    )
    .await;
    let row: (String, Option<bool>, Option<i64>) =
        sqlx::query_as("select trigger, ok, pre_tokens from compactions where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("auto".to_owned(), Some(true), Some(980_000)));
    // Повторный финиш дубли не пишет.
    run.send(
        &pool,
        compaction_event(
            id,
            run.execution.execution_id,
            run.execution.generation as u64,
            false,
            false,
            true,
            980_000,
        ),
    )
    .await;
    let kinds = event_kinds(&pool, run.seed.conversation_id).await;
    assert_eq!(
        kinds.iter().filter(|k| *k == "compaction_started").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "compaction_finished").count(),
        1,
        "{kinds:?}"
    );
    let page = canterlot_hub::history::page(
        &mut pool.acquire().await.unwrap(),
        run.seed.owner_id,
        run.seed.conversation_id,
        canterlot_hub::history::HistoryCursor::Tail,
        50,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        page.items.iter().any(|item| matches!(
            item,
            canterlot_hub::history::TimelineItem::Compaction(view)
                if view.id == id && view.trigger == "auto" && view.ok == Some(true)
        )),
        "finished compaction must be in the timeline"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn compaction_finish_without_start_opens_and_closes_at_once(pool: PgPool) {
    let mut run = Run::start(&pool).await;
    let id = Uuid::now_v7();
    run.send(
        &pool,
        compaction_event(
            id,
            run.execution.execution_id,
            run.execution.generation as u64,
            false,
            true,
            false,
            0,
        ),
    )
    .await;
    let row: (String, Option<bool>) =
        sqlx::query_as("select trigger, ok from compactions where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row, ("manual".to_owned(), Some(false)));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn context_progress_keeps_only_the_latest(pool: PgPool) {
    use canterlot_proto::worker::v1::ContextProgress;
    let mut run = Run::start(&pool).await;
    let execution = run.execution.execution_id;
    let generation = run.execution.generation as u64;
    let progress = |tokens: u64, window: u64| {
        Body::ContextProgress(ContextProgress {
            execution_id: execution.to_string(),
            generation,
            context_tokens: tokens,
            context_window_tokens: window,
        })
    };
    run.send(&pool, progress(640_000, 1_000_000)).await;
    run.send(&pool, progress(650_000, 1_000_000)).await;
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "select context_tokens, context_window_tokens from executions where id = $1",
    )
    .bind(run.execution.execution_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(650_000), Some(1_000_000)));
    let kinds = event_kinds(&pool, run.seed.conversation_id).await;
    assert_eq!(
        kinds.iter().filter(|k| *k == "context_changed").count(),
        2,
        "{kinds:?}"
    );
    let snapshot = canterlot_hub::snapshot::conversation(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        50,
    )
    .await
    .unwrap()
    .unwrap();
    let context = snapshot.data.context.expect("snapshot carries context");
    assert_eq!(
        (context.tokens, context.window_tokens),
        (650_000, Some(1_000_000))
    );
    // Новый размер после сжатия встаёт сразу, окно при этом не теряется.
    run.send(&pool, progress(15_694, 0)).await;
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "select context_tokens, context_window_tokens from executions where id = $1",
    )
    .bind(run.execution.execution_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(15_694), Some(1_000_000)));
    // Нули без размера гасят токены: индикатор ждёт новые данные.
    run.send(&pool, progress(0, 0)).await;
    let snapshot = canterlot_hub::snapshot::conversation(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        50,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        snapshot.data.context.is_none(),
        "cleared context hides the indicator"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn turn_end_context_is_authoritative_for_the_indicator(pool: PgPool) {
    use canterlot_proto::worker::v1::RunFinished;
    let mut run = Run::start(&pool).await;
    run.send(
        &pool,
        Body::RunFinished(RunFinished {
            execution_id: run.execution.execution_id.to_string(),
            input_command_id: Uuid::now_v7().to_string(),
            context_tokens: 640_000,
            context_window_tokens: 1_000_000,
            ..Default::default()
        }),
    )
    .await;
    let row: (Option<i64>, Option<i64>) = sqlx::query_as(
        "select context_tokens, context_window_tokens from executions where id = $1",
    )
    .bind(run.execution.execution_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(640_000), Some(1_000_000)));
    let kinds = event_kinds(&pool, run.seed.conversation_id).await;
    assert!(kinds.contains(&"context_changed".to_owned()), "{kinds:?}");
}
