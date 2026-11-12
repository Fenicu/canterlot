use canterlot_hub::{
    delivery::{
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    events::StoredEvent,
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    outbox::{CommandRow, to_proto_for_session},
    prompt_suggestions::{self, PromptSuggestionSettings},
    session::Registry,
    snapshot,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    CommandStored, InputTransmissionStarted, JournalOpened, JournalRecord, JournalRecords,
    PromptSuggested, RecordBody, command, record_body::Body,
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

fn suggested(execution: &executions::StartedExecution, text: &str) -> Body {
    Body::PromptSuggested(PromptSuggested {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        text: text.into(),
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

/// Журнал открыт, подсказка записана: возвращает журнал для следующих записей.
async fn suggest(
    pool: &PgPool,
    seed: &Seed,
    execution: &executions::StartedExecution,
    text: &str,
) -> Uuid {
    let journal = Uuid::now_v7();
    assert_eq!(
        apply(
            pool,
            seed,
            &records(
                journal,
                1,
                vec![opened(execution), suggested(execution, text)]
            )
        )
        .await,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    journal
}

async fn submit(pool: &PgPool, seed: &Seed) -> SubmittedInput {
    submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "next".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
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

async fn set_enabled(pool: &PgPool, owner: Uuid, enabled: bool) {
    let mut conn = pool.acquire().await.unwrap();
    prompt_suggestions::replace_settings_in_tx(
        &mut conn,
        owner,
        PromptSuggestionSettings { enabled },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

async fn start_payload(pool: &PgPool, execution: &executions::StartedExecution) -> Value {
    sqlx::query_scalar("select payload from commands where id = $1")
        .bind(execution.command_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn suggestion_is_stored_published_and_comes_back_in_the_snapshot(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_the_latest_suggestion_is_kept(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let journal = suggest(&pool, &seed, &execution, "first").await;
    apply(
        &pool,
        &seed,
        &records(journal, 3, vec![suggested(&execution, "second")]),
    )
    .await;
    assert_eq!(stored(&pool, &seed).await.as_deref(), Some("second"));
    let count: i64 = sqlx::query_scalar("select count(*) from prompt_suggestions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_input_clears_the_suggestion_and_publishes_null(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    suggest(&pool, &seed, &execution, "add tests").await;
    submit(&pool, &seed).await;
    assert_eq!(stored(&pool, &seed).await, None);
    let published = events(&pool, seed.conversation_id).await;
    assert_eq!(published.len(), 2);
    assert_eq!(published[1].payload["suggestion"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn suggestion_is_not_stored_while_input_waits_for_its_turn(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    submit(&pool, &seed).await;
    suggest(&pool, &seed, &execution, "stale").await;
    assert_eq!(stored(&pool, &seed).await, None);
    assert!(events(&pool, seed.conversation_id).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_turn_clears_the_suggestion(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let input = submit(&pool, &seed).await;
    assert!(matches!(
        dispatch_next(
            &pool,
            seed.conversation_id,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        DispatchOutcome::Dispatched { attempt: 1, .. }
    ));
    // Подсказка, сохранённая в обход очереди: проверяется только стирание началом хода.
    sqlx::query(
        "insert into prompt_suggestions (conversation_id, execution_id, generation, text)
         values ($1, $2, $3, 'old')",
    )
    .bind(seed.conversation_id)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .execute(&pool)
    .await
    .unwrap();
    let journal = Uuid::now_v7();
    apply(
        &pool,
        &seed,
        &records(
            journal,
            1,
            vec![
                Body::CommandStored(CommandStored {
                    machine_time_ms: 0,
                    command_id: input.command_id.to_string(),
                    dispatch_attempt: 1,
                }),
                Body::TransmissionStarted(InputTransmissionStarted {
                    machine_time_ms: 0,
                    command_id: input.command_id.to_string(),
                    dispatch_attempt: 1,
                }),
            ],
        ),
    )
    .await;
    assert_eq!(stored(&pool, &seed).await, None);
    let published = events(&pool, seed.conversation_id).await;
    assert_eq!(published.last().unwrap().payload["suggestion"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn setting_off_drops_new_suggestions_and_clears_shown_ones(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let execution = running(&pool, &seed).await;
    let journal = suggest(&pool, &seed, &execution, "add tests").await;
    set_enabled(&pool, seed.owner_id, false).await;
    assert_eq!(stored(&pool, &seed).await, None);
    let published = events(&pool, seed.conversation_id).await;
    assert_eq!(published.len(), 2);
    assert_eq!(published[1].payload["suggestion"], Value::Null);
    apply(
        &pool,
        &seed,
        &records(journal, 3, vec![suggested(&execution, "again")]),
    )
    .await;
    assert_eq!(stored(&pool, &seed).await, None);
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        !prompt_suggestions::settings(&mut conn, seed.owner_id)
            .await
            .unwrap()
            .enabled
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delegated_and_other_harness_sessions_get_no_suggestions(pool: PgPool) {
    for (harness, delegated) in [("codex", false), ("claude_code", true)] {
        let seed = testkit::seed(&pool).await;
        set_harness(&pool, seed.conversation_id, harness).await;
        if delegated {
            sqlx::query("update conversations set spawn_depth = 1 where id = $1")
                .bind(seed.conversation_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let execution = running(&pool, &seed).await;
        assert_eq!(
            start_payload(&pool, &execution)
                .await
                .get("prompt_suggestions"),
            None,
            "{harness} {delegated}"
        );
        suggest(&pool, &seed, &execution, "add tests").await;
        assert_eq!(stored(&pool, &seed).await, None, "{harness} {delegated}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_command_carries_the_owner_setting(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let first = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        start_payload(&pool, &first).await["prompt_suggestions"],
        true
    );
    for (state, outcome) in [
        (ExecutionState::Starting, None),
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("user_stop")),
    ] {
        executions::set_state(&pool, first.execution_id, state, outcome, Uuid::now_v7())
            .await
            .unwrap();
    }
    set_enabled(&pool, seed.owner_id, false).await;
    let second = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        start_payload(&pool, &second)
            .await
            .get("prompt_suggestions"),
        None
    );
}

fn start_row(payload: Value) -> CommandRow {
    CommandRow {
        id: Uuid::now_v7(),
        machine_id: Uuid::now_v7(),
        conversation_id: Some(Uuid::now_v7()),
        execution_id: Some(Uuid::now_v7()),
        execution_generation: Some(1),
        kind: "start_execution".into(),
        payload,
        dispatch_attempt: 1,
    }
}

#[tokio::test]
async fn old_worker_without_the_capability_gets_nothing_new() {
    let row = start_row(json!({
        "harness": "claude_code", "workdir": "/tmp/w", "model": "m", "effort": "low",
        "permission_mode": "owner", "native_conversation_id": null, "prompt_suggestions": true,
        "instructions": {"mode": "owner", "preset_id": "", "revision": 0, "title": "", "text": ""},
    }));
    for (capabilities, expected) in [
        (vec!["pilot_v1", "instructions_v1"], false),
        (
            vec!["pilot_v1", "instructions_v1", "prompt_suggestions_v1"],
            true,
        ),
    ] {
        let registry = Registry::new(Default::default());
        let _session = registry.connect_for_tests(
            row.machine_id,
            &capabilities
                .iter()
                .map(|c| (*c).to_owned())
                .collect::<Vec<_>>(),
        );
        let command = to_proto_for_session(&row, &registry).unwrap();
        let Some(command::Kind::StartExecution(start)) = command.kind else {
            panic!("{command:?}");
        };
        assert_eq!(start.prompt_suggestions, expected, "{capabilities:?}");
        assert_eq!(
            start
                .capabilities
                .iter()
                .any(|c| c == "prompt_suggestions_v1"),
            expected
        );
    }
}

fn schema_validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/prompt_suggestion_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn events_schema_describes_prompt_suggestion_changed() {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conversation = Uuid::now_v7().to_string();
    let valid = json!({
        "v": 1,
        "id": Uuid::now_v7().to_string(),
        "scope": format!("conversation:{conversation}"),
        "scope_seq": "1",
        "scope_seq_from": "1",
        "committed_at": now.clone(),
        "committed_at_from": now.clone(),
        "conversation_id": conversation,
        "execution_id": Uuid::now_v7().to_string(),
        "correlation_id": Uuid::now_v7().to_string(),
        "kind": "prompt_suggestion_changed",
        "payload": {
            "conversation_id": conversation,
            "suggestion": {"text": "add tests", "execution_id": Uuid::now_v7().to_string(), "created_at": now},
        }
    });
    let validator = schema_validator();
    assert!(validator.is_valid(&valid));
    let mut cleared = valid.clone();
    cleared["payload"]["suggestion"] = Value::Null;
    cleared["execution_id"] = Value::Null;
    assert!(validator.is_valid(&cleared));
    let mut empty = valid.clone();
    empty["payload"]["suggestion"]["text"] = json!("");
    assert!(!validator.is_valid(&empty));
    let mut extra = valid;
    extra["payload"]["extra"] = json!(1);
    assert!(!validator.is_valid(&extra));
}

#[test]
fn suggestion_view_debug_shows_only_the_length() {
    let marker = "PRIVATE_SUGGESTION_VIEW_MARKER";
    let view = prompt_suggestions::PromptSuggestionView {
        text: marker.into(),
        execution_id: Uuid::now_v7(),
        created_at: Utc::now(),
    };
    let debug = format!("{view:?}");
    assert!(!debug.contains(marker), "{debug}");
    assert!(
        debug.contains(&format!("text_bytes: {}", marker.len())),
        "{debug}"
    );
    let snapshot_field = format!("{:?}", Some(view));
    assert!(!snapshot_field.contains(marker), "{snapshot_field}");
}
