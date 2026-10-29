use std::time::Duration;

use canterlot_hub::{
    activity::{self, ActivityState, Evidence, Thresholds},
    delivery::dispatch::{self, BlockReason, DispatchOutcome, GateInputs},
    events::StoredEvent,
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::{ExecutionState, HoldReason},
    requests::{RequestKind, RequestRow, RequestState},
    snapshot,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    DecisionApplied, DecisionTransmissionStarted, JournalOpened, JournalRecord, JournalRecords,
    RecordBody, RequestOpened, record_body,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn schema() -> Value {
    serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap()
}

fn validator(name: &str) -> jsonschema::Validator {
    let mut s = schema();
    s["$ref"] = json!(format!("#/$defs/{name}"));
    s.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&s).unwrap()
}

fn batch(journal_id: Uuid, bodies: Vec<record_body::Body>) -> JournalRecords {
    JournalRecords {
        journal_id: journal_id.to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| JournalRecord {
                seq: i as u64 + 1,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    }
}

fn opened(execution: &executions::StartedExecution) -> record_body::Body {
    record_body::Body::JournalOpened(opened_body(execution))
}

fn opened_body(execution: &executions::StartedExecution) -> JournalOpened {
    JournalOpened {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        supervisor_protocol_version: 1,
    }
}

async fn setup_running_execution(pool: &PgPool) -> (Seed, executions::StartedExecution) {
    let seed = testkit::seed(pool).await;
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    (seed, execution)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ingest_request_opened_inserts_request_row_and_emits_wss_event(pool: PgPool) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let req_id = Uuid::now_v7();
    let question_payload = json!({
        "questions": [
            {
                "id": "q1",
                "text": "Deploy to production?",
                "options": ["yes", "no"]
            }
        ]
    });

    let records = batch(
        journal,
        vec![
            opened(&execution),
            record_body::Body::RequestOpened(RequestOpened {
                request_id: req_id.to_string(),
                execution_id: execution.execution_id.to_string(),
                generation: execution.generation as u64,
                kind: "question".into(),
                payload_json: serde_json::to_vec(&question_payload)?,
            }),
        ],
    );

    let outcome = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 2 });

    // Проверяем строку в таблице requests
    let row: RequestRow = sqlx::query_as("select * from requests where id = $1")
        .bind(req_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.conversation_id, seed.conversation_id);
    assert_eq!(row.execution_id, execution.execution_id);
    assert_eq!(row.generation, execution.generation);
    assert_eq!(row.kind, RequestKind::Question);
    assert_eq!(row.state, RequestState::Open);
    assert_eq!(row.request_payload, question_payload);

    // Проверяем событие WSS
    let ev: StoredEvent = sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'request_opened'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(ev.payload["request_id"], req_id.to_string());
    assert_eq!(ev.payload["kind"], "question");
    assert_eq!(ev.payload["payload"], question_payload);

    // Дедупликация: повторный анонс того же запроса после перезапуска воркера
    let restart_journal = Uuid::now_v7();
    let reannounce_records = batch(
        restart_journal,
        vec![
            opened(&execution),
            record_body::Body::RequestOpened(RequestOpened {
                request_id: req_id.to_string(),
                execution_id: execution.execution_id.to_string(),
                generation: execution.generation as u64,
                kind: "question".into(),
                payload_json: serde_json::to_vec(&question_payload)?,
            }),
        ],
    );

    let reannounce_outcome = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &reannounce_records,
        )
        .await?;
    assert_eq!(
        reannounce_outcome,
        IngestOutcome::Committed { up_to_seq: 2 }
    );

    let count: i64 = sqlx::query_scalar("select count(*) from requests where id = $1")
        .bind(req_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        count, 1,
        "re-announced request must not create duplicate row"
    );

    let event_count: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id = $1 and kind = 'request_opened'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        event_count, 1,
        "re-announced request must be idempotent and not emit duplicate event"
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn open_request_sets_activity_waiting_for_user_with_question_summary(
    pool: PgPool,
) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let req_id = Uuid::now_v7();
    let long_question = "A".repeat(150);
    let question_payload = json!({
        "questions": [
            {
                "id": "q1",
                "text": long_question,
            }
        ]
    });

    let mut tx = pool.begin().await?;
    canterlot_hub::requests::create_request_in_tx(
        &mut tx,
        &canterlot_hub::requests::NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id: execution.execution_id,
            generation: execution.generation,
            kind: RequestKind::Question,
            native_request_id: "q-test".into(),
            request_payload: question_payload,
        },
    )
    .await?;
    tx.commit().await?;

    let mut conn = pool.acquire().await?;
    let inputs = activity::load_inputs(
        &mut conn,
        seed.conversation_id,
        Utc::now(),
        Duration::from_secs(60),
    )
    .await?;
    assert_eq!(inputs.open_requests, 1);
    let expected_truncated = "A".repeat(120);
    assert_eq!(
        inputs.question_summary.as_deref(),
        Some(expected_truncated.as_str())
    );

    let thresholds = Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
    };
    let act = activity::compute(&inputs, &thresholds);
    assert_eq!(act.state, ActivityState::WaitingForUser);
    match act.evidence {
        Evidence::WaitingForUser { question, .. } => {
            assert_eq!(question.as_deref(), Some(expected_truncated.as_str()));
        }
        other => panic!("expected Evidence::WaitingForUser, got {other:?}"),
    }

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn waiting_for_user_with_open_request_is_immune_from_stalled(pool: PgPool) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let mut tx = pool.begin().await?;
    canterlot_hub::requests::create_request_in_tx(
        &mut tx,
        &canterlot_hub::requests::NewRequest {
            id: Uuid::now_v7(),
            conversation_id: seed.conversation_id,
            execution_id: execution.execution_id,
            generation: execution.generation,
            kind: RequestKind::Permission,
            native_request_id: "perm-test".into(),
            request_payload: json!({"tool_name": "Bash", "command": "rm -rf /"}),
        },
    )
    .await?;
    tx.commit().await?;

    let mut conn = pool.acquire().await?;
    let distant_future = Utc::now() + TimeDelta::days(7);
    let mut inputs = activity::load_inputs(
        &mut conn,
        seed.conversation_id,
        distant_future,
        Duration::from_secs(60),
    )
    .await?;
    inputs.now = distant_future;
    assert_eq!(inputs.open_requests, 1);

    let thresholds = Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
    };
    let act = activity::compute(&inputs, &thresholds);
    assert_eq!(
        act.state,
        ActivityState::WaitingForUser,
        "silence duration must not degrade waiting_for_user to stalled"
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn open_request_places_awaiting_user_request_queue_hold(pool: PgPool) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let req_id = Uuid::now_v7();

    let records = batch(
        journal,
        vec![
            opened(&execution),
            record_body::Body::RequestOpened(RequestOpened {
                request_id: req_id.to_string(),
                execution_id: execution.execution_id.to_string(),
                generation: execution.generation as u64,
                kind: "permission".into(),
                payload_json: br#"{"tool_name":"Bash","command":"make build"}"#.to_vec(),
            }),
        ],
    );

    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await?;

    let holds: sqlx::types::Json<Vec<HoldReason>> =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id = $1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert!(
        holds.0.contains(&HoldReason::AwaitingUserRequest),
        "queue_state.hold_reasons must contain AwaitingUserRequest"
    );

    let gate = GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    let outcome =
        dispatch::dispatch_next(&pool, seed.conversation_id, gate, Uuid::now_v7()).await?;
    match outcome {
        DispatchOutcome::Blocked(reasons) => {
            assert!(
                reasons.iter().any(|r| matches!(r, BlockReason::Held(h) if h.contains(&HoldReason::AwaitingUserRequest))),
                "dispatch must be blocked by AwaitingUserRequest hold: {reasons:?}"
            );
        }
        other => panic!("expected Blocked outcome, got {other:?}"),
    }

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_decision_applied_clears_queue_hold_and_emits_request_closed(
    pool: PgPool,
) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let req_id = Uuid::now_v7();

    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                journal,
                vec![
                    opened(&execution),
                    record_body::Body::RequestOpened(RequestOpened {
                        request_id: req_id.to_string(),
                        execution_id: execution.execution_id.to_string(),
                        generation: execution.generation as u64,
                        kind: "permission".into(),
                        payload_json: br#"{"tool_name":"Bash","command":"cargo test"}"#.to_vec(),
                    }),
                ],
            ),
        )
        .await?;

    let mut tx = pool.begin().await?;
    canterlot_hub::requests::save_decision_in_tx(
        &mut tx,
        seed.conversation_id,
        req_id,
        execution.generation,
        &canterlot_proto::requests::DecisionPayload::Permission {
            behavior: canterlot_proto::requests::PermissionBehavior::Allow,
            message: None,
        },
        "idemp-1",
    )
    .await?;
    tx.commit().await?;

    let mut sj2 = batch(
        journal,
        vec![
            record_body::Body::DecisionTransmissionStarted(DecisionTransmissionStarted {
                request_id: req_id.to_string(),
                decision_id: "dec-1".into(),
                machine_time_ms: 100,
            }),
            record_body::Body::DecisionApplied(DecisionApplied {
                request_id: req_id.to_string(),
                decision_id: "dec-1".into(),
                proof_kind: "tool_result".into(),
                proof_payload_json: br#"{"ok":true}"#.to_vec(),
            }),
        ],
    );
    sj2.records[0].seq = 3;
    sj2.records[1].seq = 4;

    let outcome = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &sj2,
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 4 });

    let row: RequestRow = sqlx::query_as("select * from requests where id = $1")
        .bind(req_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.state, RequestState::DecisionApplied);

    let closed_ev: StoredEvent = sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'request_closed'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(closed_ev.payload["request_id"], req_id.to_string());
    assert_eq!(closed_ev.payload["reason"], "applied");

    let cmd_state: String = sqlx::query_scalar(
        "select delivery_state from commands where conversation_id = $1 and kind = 'deliver_decision'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(cmd_state, "completed");

    let holds: sqlx::types::Json<Vec<HoldReason>> =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id = $1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert!(
        !holds.0.contains(&HoldReason::AwaitingUserRequest),
        "AwaitingUserRequest hold must be cleared after request resolution"
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn conversation_snapshot_includes_open_requests(pool: PgPool) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;

    let open_req_id = Uuid::now_v7();
    let closed_req_id = Uuid::now_v7();

    let mut tx = pool.begin().await?;
    canterlot_hub::requests::create_request_in_tx(
        &mut tx,
        &canterlot_hub::requests::NewRequest {
            id: open_req_id,
            conversation_id: seed.conversation_id,
            execution_id: execution.execution_id,
            generation: execution.generation,
            kind: RequestKind::Question,
            native_request_id: "q-open".into(),
            request_payload: json!({"question": "Are you sure?"}),
        },
    )
    .await?;

    let closed = canterlot_hub::requests::create_request_in_tx(
        &mut tx,
        &canterlot_hub::requests::NewRequest {
            id: closed_req_id,
            conversation_id: seed.conversation_id,
            execution_id: execution.execution_id,
            generation: execution.generation,
            kind: RequestKind::Permission,
            native_request_id: "perm-closed".into(),
            request_payload: json!({"tool_name": "Bash"}),
        },
    )
    .await?;
    sqlx::query("update requests set state = 'closed_without_decision' where id = $1")
        .bind(closed.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let snap = snapshot::conversation(&pool, seed.owner_id, seed.conversation_id, 50)
        .await?
        .expect("snapshot must exist");

    assert_eq!(snap.data.open_requests.len(), 1);
    let req_view = &snap.data.open_requests[0];
    assert_eq!(req_view.id, open_req_id);
    assert_eq!(req_view.kind, "question");
    assert_eq!(req_view.state, "open");
    assert_eq!(req_view.native_request_id, "q-open");
    assert_eq!(
        req_view.request_payload,
        json!({"question": "Are you sure?"})
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_transmission_started_emits_request_transmitting_event(
    pool: PgPool,
) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let req_id = Uuid::now_v7();

    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                journal,
                vec![
                    opened(&execution),
                    record_body::Body::RequestOpened(RequestOpened {
                        request_id: req_id.to_string(),
                        execution_id: execution.execution_id.to_string(),
                        generation: execution.generation as u64,
                        kind: "permission".into(),
                        payload_json: br#"{"tool_name":"Bash","command":"cargo test"}"#.to_vec(),
                    }),
                ],
            ),
        )
        .await?;

    let mut tx = pool.begin().await?;
    canterlot_hub::requests::save_decision_in_tx(
        &mut tx,
        seed.conversation_id,
        req_id,
        execution.generation,
        &canterlot_proto::requests::DecisionPayload::Permission {
            behavior: canterlot_proto::requests::PermissionBehavior::Allow,
            message: None,
        },
        "idemp-transmitting",
    )
    .await?;
    tx.commit().await?;

    let count_transmitting = || async {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from events where conversation_id = $1 and kind = 'request_transmitting'",
        )
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await
    };
    assert_eq!(
        count_transmitting().await?,
        0,
        "до передачи агенту события нет"
    );

    let started = |seq: u64| {
        let mut records = batch(
            journal,
            vec![record_body::Body::DecisionTransmissionStarted(
                DecisionTransmissionStarted {
                    request_id: req_id.to_string(),
                    decision_id: "dec-1".into(),
                    machine_time_ms: 100,
                },
            )],
        );
        records.records[0].seq = seq;
        records
    };

    let outcome = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &started(3),
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 3 });

    let row: RequestRow = sqlx::query_as("select * from requests where id = $1")
        .bind(req_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(row.state, RequestState::DecisionTransmitting);

    let event: StoredEvent = sqlx::query_as(
        "select * from events where conversation_id = $1 and kind = 'request_transmitting'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(event.payload, json!({"request_id": req_id.to_string()}));
    assert_eq!(event.conversation_id, Some(seed.conversation_id));
    assert_eq!(event.execution_id, Some(execution.execution_id));
    assert!(
        canterlot_hub::events::is_client_visible("request_transmitting"),
        "событие видно клиентам"
    );
    let wire = canterlot_hub::events::project_for_client(vec![event]);
    assert_eq!(wire[0].kind, "request_transmitting");
    assert!(
        validator("request_transmitting").is_valid(&serde_json::to_value(&wire[0])?),
        "request_transmitting must pass schema"
    );

    // Повтор той же записи (курсор журнала) и новая запись с тем же фактом (состояние
    // уже сдвинулось) не дают второго события.
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &started(3),
        )
        .await?;
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &started(4),
        )
        .await?;
    assert_eq!(
        count_transmitting().await?,
        1,
        "replayed fact must not emit a second request_transmitting"
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_open_requests_include_saved_and_transmitting_requests(
    pool: PgPool,
) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;

    let states = [
        ("transmitting", "decision_transmitting"),
        ("applied", "decision_applied"),
        ("open", "open"),
        ("not-applied", "decision_not_applied"),
        ("saved", "decision_saved"),
        ("closed", "closed_without_decision"),
    ];
    let mut ids = std::collections::HashMap::new();
    let mut tx = pool.begin().await?;
    for (offset, (name, state)) in states.iter().enumerate() {
        let id = Uuid::now_v7();
        canterlot_hub::requests::create_request_in_tx(
            &mut tx,
            &canterlot_hub::requests::NewRequest {
                id,
                conversation_id: seed.conversation_id,
                execution_id: execution.execution_id,
                generation: execution.generation,
                kind: RequestKind::Permission,
                native_request_id: format!("native-{name}"),
                request_payload: json!({"tool_name": "Bash"}),
            },
        )
        .await?;
        sqlx::query(
            "update requests set state = $2, created_at = now() + make_interval(secs => $3) where id = $1",
        )
        .bind(id)
        .bind(state)
        .bind(offset as f64)
        .execute(&mut *tx)
        .await?;
        ids.insert(*name, id);
    }
    tx.commit().await?;

    let snap = snapshot::conversation(&pool, seed.owner_id, seed.conversation_id, 50)
        .await?
        .expect("snapshot must exist");

    let listed: Vec<(Uuid, String)> = snap
        .data
        .open_requests
        .iter()
        .map(|r| (r.id, r.state.clone()))
        .collect();
    assert_eq!(
        listed,
        vec![
            (ids["transmitting"], "decision_transmitting".to_owned()),
            (ids["open"], "open".to_owned()),
            (ids["saved"], "decision_saved".to_owned()),
        ],
        "незавершённые запросы по created_at, завершённые и закрытые исключены"
    );

    Ok(())
}

#[test]
fn events_schema_validates_request_opened_decided_closed() {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conv_id = Uuid::now_v7().to_string();
    let exec_id = Uuid::now_v7().to_string();
    let req_id = Uuid::now_v7().to_string();
    let win_id = Uuid::now_v7().to_string();

    // 1. request_opened
    let valid_opened = json!({
        "v": 1,
        "id": Uuid::now_v7().to_string(),
        "scope": format!("conversation:{conv_id}"),
        "scope_seq": "1",
        "scope_seq_from": "1",
        "committed_at": now.clone(),
        "committed_at_from": now.clone(),
        "conversation_id": conv_id,
        "execution_id": exec_id,
        "correlation_id": Uuid::now_v7().to_string(),
        "kind": "request_opened",
        "payload": {
            "request_id": req_id,
            "kind": "question",
            "payload": {
                "questions": [{"id": "q1", "text": "Deploy?"}]
            }
        }
    });
    assert!(
        validator("request_opened").is_valid(&valid_opened),
        "valid request_opened must pass schema"
    );

    let mut invalid_opened = valid_opened.clone();
    invalid_opened["payload"]["extra_field"] = json!("forbidden");
    assert!(
        !validator("request_opened").is_valid(&invalid_opened),
        "additionalProperties: false must reject extra fields"
    );

    // 2. request_decided
    let valid_decided = json!({
        "v": 1,
        "id": Uuid::now_v7().to_string(),
        "scope": format!("conversation:{conv_id}"),
        "scope_seq": "2",
        "scope_seq_from": "2",
        "committed_at": now.clone(),
        "committed_at_from": now.clone(),
        "conversation_id": conv_id,
        "execution_id": exec_id,
        "correlation_id": Uuid::now_v7().to_string(),
        "kind": "request_decided",
        "payload": {
            "request_id": req_id,
            "winner_decision_id": win_id,
            "decision": {
                "kind": "permission",
                "behavior": "allow"
            }
        }
    });
    assert!(
        validator("request_decided").is_valid(&valid_decided),
        "valid request_decided must pass schema"
    );

    let mut invalid_decided = valid_decided.clone();
    invalid_decided["payload"]
        .as_object_mut()
        .unwrap()
        .remove("winner_decision_id");
    assert!(
        !validator("request_decided").is_valid(&invalid_decided),
        "missing winner_decision_id must fail schema"
    );

    // 3. request_closed
    let valid_closed = json!({
        "v": 1,
        "id": Uuid::now_v7().to_string(),
        "scope": format!("conversation:{conv_id}"),
        "scope_seq": "3",
        "scope_seq_from": "3",
        "committed_at": now.clone(),
        "committed_at_from": now.clone(),
        "conversation_id": conv_id,
        "execution_id": exec_id,
        "correlation_id": Uuid::now_v7().to_string(),
        "kind": "request_closed",
        "payload": {
            "request_id": req_id,
            "reason": "applied"
        }
    });
    assert!(
        validator("request_closed").is_valid(&valid_closed),
        "valid request_closed must pass schema"
    );

    let mut invalid_closed = valid_closed.clone();
    invalid_closed["payload"]
        .as_object_mut()
        .unwrap()
        .remove("reason");
    assert!(
        !validator("request_closed").is_valid(&invalid_closed),
        "missing reason must fail schema"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sj_poison_record_rolls_back_synchronously(pool: PgPool) -> TestResult {
    let (seed, execution) = setup_running_execution(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let records = JournalRecords {
        journal_id: journal.to_string(),
        records: vec![
            JournalRecord {
                seq: 1,
                body: Some(RecordBody {
                    body: Some(record_body::Body::JournalOpened(opened_body(&execution))),
                }),
            },
            JournalRecord { seq: 2, body: None },
        ],
    };
    let mut probe = pool.acquire().await?;
    for _ in 0..10 {
        let result = ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records,
            )
            .await;
        assert!(result.is_err(), "poison record must fail");
        // Курсор блокировался FOR UPDATE: пропуск — висячий откат.
        let free: Option<Uuid> = sqlx::query_scalar(
            "select machine_id from journal_cursors where journal_id=$1 for update skip locked",
        )
        .bind(journal)
        .fetch_optional(&mut *probe)
        .await?;
        assert!(free.is_some(), "cursor row still locked");
    }
    // Частичный прогресс первой записи при этом зафиксирован.
    let committed: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id=$1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(committed, 1);
    Ok(())
}
