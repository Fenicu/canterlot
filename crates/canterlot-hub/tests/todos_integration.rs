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
use chrono::Utc;
use serde_json::{Value, json};
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_is_stored_published_and_comes_back_in_the_snapshot(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replay_of_a_shipped_batch_changes_nothing(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    let batch = records(
        journal,
        1,
        vec![
            opened(&execution),
            fact(&execution, 2, None, vec![item("а", TodoStatus::Pending)]),
        ],
    );
    apply(&pool, &seed, &batch).await;
    apply(&pool, &seed, &batch).await;
    assert_eq!(todo_events(&pool, seed.conversation_id).await.len(), 1);
    assert_eq!(lists(&pool, &seed).await[0].revision, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_revision_of_the_same_generation_is_ignored(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                opened(&execution),
                fact(
                    &execution,
                    9,
                    None,
                    vec![item("новый", TodoStatus::Pending)],
                ),
                fact(
                    &execution,
                    5,
                    None,
                    vec![item("старый", TodoStatus::Pending)],
                ),
                fact(
                    &execution,
                    9,
                    None,
                    vec![item("дубль", TodoStatus::Pending)],
                ),
            ],
        ),
    )
    .await;
    assert_eq!(todo_events(&pool, seed.conversation_id).await.len(), 1);
    let stored = lists(&pool, &seed).await;
    assert_eq!(stored[0].items, vec![view("новый", ViewStatus::Pending)]);
    assert_eq!(stored[0].revision, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn newer_generation_wins_even_with_a_lower_revision_and_older_stays_stale(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let first = running(&pool, &seed).await;
    apply(
        &pool,
        &seed,
        &records(
            Uuid::now_v7(),
            1,
            vec![
                opened(&first),
                fact(&first, 500, None, vec![item("первое", TodoStatus::Pending)]),
            ],
        ),
    )
    .await;
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("user_stop")),
    ] {
        executions::set_state(&pool, first.execution_id, state, outcome, Uuid::now_v7())
            .await
            .unwrap();
    }
    let second = running(&pool, &seed).await;
    assert!(second.generation > first.generation);
    apply(
        &pool,
        &seed,
        &records(
            Uuid::now_v7(),
            1,
            vec![
                opened(&second),
                fact(&second, 2, None, vec![item("второе", TodoStatus::Pending)]),
            ],
        ),
    )
    .await;
    let stored = lists(&pool, &seed).await;
    assert_eq!(stored[0].items, vec![view("второе", ViewStatus::Pending)]);
    assert_eq!(stored[0].revision, 2);

    // Хвост прежнего журнала доехал после нового исполнения.
    let late = sqlx::query_scalar::<_, Uuid>(
        "select journal_id from journal_cursors where execution_id = $1",
    )
    .bind(first.execution_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    apply(
        &pool,
        &seed,
        &records(
            late,
            3,
            vec![fact(
                &first,
                501,
                None,
                vec![item("поздно", TodoStatus::Pending)],
            )],
        ),
    )
    .await;
    assert_eq!(
        lists(&pool, &seed).await[0].items,
        vec![view("второе", ViewStatus::Pending)]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lanes_are_kept_apart_and_each_counts_its_own_revision(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let lane = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            Uuid::now_v7(),
            1,
            vec![
                opened(&execution),
                fact(
                    &execution,
                    2,
                    None,
                    vec![item("главный", TodoStatus::Pending)],
                ),
                fact(
                    &execution,
                    3,
                    Some(lane),
                    vec![item("суб", TodoStatus::Pending)],
                ),
                fact(&execution, 4, None, vec![]),
            ],
        ),
    )
    .await;
    let stored = lists(&pool, &seed).await;
    assert_eq!(stored.len(), 2, "{stored:?}");
    assert_eq!(
        (
            stored[0].lane_block_id,
            stored[0].revision,
            stored[0].items.len()
        ),
        (None, 2, 0),
        "empty list replaces the previous one"
    );
    assert_eq!(
        (
            stored[1].lane_block_id,
            stored[1].revision,
            stored[1].items.len()
        ),
        (Some(lane), 1, 1)
    );
    let events = todo_events(&pool, seed.conversation_id).await;
    assert_eq!(events.len(), 3);
    assert_eq!(events[1].payload["lane_block_id"], json!(lane));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_and_unknown_status_do_not_poison_the_journal(pool: PgPool) {
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
                    item("а\0б", TodoStatus::InProgress),
                    TodoItem {
                        text: "в".into(),
                        status: 99,
                    },
                ],
            ),
        ],
    );
    assert_eq!(
        apply(&pool, &seed, &batch).await,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    assert_eq!(
        lists(&pool, &seed).await[0].items,
        vec![
            view("аб", ViewStatus::InProgress),
            view("в", ViewStatus::Pending)
        ]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_of_a_foreign_execution_is_refused(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let journal = Uuid::now_v7();
    apply(&pool, &seed, &records(journal, 1, vec![opened(&execution)])).await;
    let mut body = fact(&execution, 2, None, vec![]);
    if let Body::TodoUpdated(fact) = &mut body {
        fact.execution_id = Uuid::now_v7().to_string();
    }
    let result = Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records(journal, 2, vec![body]),
        )
        .await;
    assert!(result.is_err(), "{result:?}");
    assert!(lists(&pool, &seed).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn table_goes_with_its_conversation(pool: PgPool) {
    let cascades: Vec<(String,)> = sqlx::query_as(
        "select confrelid::regclass::text from pg_constraint
         where conrelid = 'agent_todos'::regclass and contype = 'f' and confdeltype = 'c'
         order by 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(cascades, vec![("conversations".to_owned(),)]);
}

fn schema_validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/todo_updated");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn events_schema_describes_todo_updated() {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conversation = Uuid::now_v7().to_string();
    let valid = json!({
        "v": 1,
        "id": Uuid::now_v7().to_string(),
        "scope": format!("conversation:{conversation}"),
        "scope_seq": "1",
        "scope_seq_from": "1",
        "committed_at": now.clone(),
        "committed_at_from": now,
        "conversation_id": conversation,
        "execution_id": Uuid::now_v7().to_string(),
        "correlation_id": Uuid::now_v7().to_string(),
        "kind": "todo_updated",
        "payload": {
            "lane_block_id": null,
            "revision": 3,
            "items": [{"text": "а", "status": "in_progress"}],
            "omitted": 0,
        }
    });
    let validator = schema_validator();
    assert!(validator.is_valid(&valid));
    let mut wrong = valid.clone();
    wrong["payload"]["items"][0]["status"] = json!("done");
    assert!(!validator.is_valid(&wrong));
    let mut extra = valid;
    extra["payload"]["items"][0]["active"] = json!("x");
    assert!(!validator.is_valid(&extra));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_command_offers_todo_v1_only_on_a_session_that_agreed_to_it(pool: PgPool) {
    use canterlot_hub::{
        config::HubTimers,
        grpc::KNOWN_OPTIONAL_CAPABILITIES,
        outbox::{self, CommandRow},
        session::Registry,
    };
    use canterlot_proto::worker::v1::command;
    assert!(KNOWN_OPTIONAL_CAPABILITIES.contains(&"todo_v1"));
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set instruction_profile = 'owner' where id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    let row: CommandRow = sqlx::query_as("select * from commands where id = $1")
        .bind(execution.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let capabilities = |session: &[&str]| {
        let registry = Registry::new(HubTimers::default());
        let _session = testkit::fake_session(&registry, seed.machine_id, session, true);
        let command = outbox::to_proto_for_session(&row, &registry).unwrap();
        let Some(command::Kind::StartExecution(start)) = command.kind else {
            panic!("start_execution expected");
        };
        start.capabilities
    };
    assert_eq!(capabilities(&["pilot_v1"]), ["pilot_v1"]);
    assert_eq!(
        capabilities(&["pilot_v1", "todo_v1"]),
        ["pilot_v1", "todo_v1"]
    );
}

#[test]
fn debug_of_a_list_shows_no_item_text() {
    let marker = "PRIVATE_TODO_MARKER";
    let list = TodoListView {
        lane_block_id: None,
        revision: 1,
        items: vec![view(marker, ViewStatus::Pending)],
        omitted: 0,
    };
    let debug = format!("{list:?}");
    assert!(!debug.contains(marker), "{debug}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_update_counts_as_a_sign_of_life_for_the_stalled_check(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let last_event_at = || async {
        sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>(
            "select last_event_at from executions where id = $1",
        )
        .bind(execution.execution_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let journal = Uuid::now_v7();
    apply(&pool, &seed, &records(journal, 1, vec![opened(&execution)])).await;
    assert_eq!(last_event_at().await, None);
    apply(
        &pool,
        &seed,
        &records(
            journal,
            2,
            vec![fact(
                &execution,
                2,
                None,
                vec![item("а", TodoStatus::Pending)],
            )],
        ),
    )
    .await;
    assert!(last_event_at().await.is_some());
}
