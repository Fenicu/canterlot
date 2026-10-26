use canterlot_hub::{
    delivery::{
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions,
    ingest::{Ingest, IngestError, IngestOutcome, JournalKind},
    model::{ExecutionState, HoldReason},
    testkit::{self, Seed},
};
use canterlot_proto::{limits::MAX_EVENT_BATCH_BYTES, worker::v1::*};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::Digest as _;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn wait_for_lock(pool: &PgPool, query_prefix: &str, event: &str) -> TestResult {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname = current_database() and query like $1 and wait_event_type = 'Lock' and wait_event = $2)")
                .bind(format!("{query_prefix}%")).bind(event).fetch_one(pool).await?;
            if waiting { return Ok::<_, sqlx::Error>(()); }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await??;
    Ok(())
}

async fn first_record_racing_credential_change(pool: PgPool, rotate: bool) -> TestResult {
    use canterlot_hub::machines;
    sqlx::query("create function pause_first_record() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(120501); return new; end $$")
        .execute(&pool).await?;
    sqlx::query("create trigger pause_first_record before insert on journal_cursors for each row execute function pause_first_record()")
        .execute(&pool).await?;
    let seed = testkit::seed(&pool).await;
    for _ in 0..50 {
        let machine = machines::add(&pool, seed.owner_id, "lock-order").await?;
        let id = machine.machine_id;
        let journal = Uuid::now_v7();
        let mut gate = pool.begin().await?;
        sqlx::query("select pg_advisory_xact_lock(120501)")
            .execute(&mut *gate)
            .await?;
        let ingest = Ingest::new(pool.clone());
        let record = tokio::spawn(async move {
            ingest
                .apply_batch(
                    id,
                    &sha2::Sha256::digest(machine.token.as_bytes()),
                    JournalKind::Cj,
                    &batch(journal, vec![stored(Uuid::now_v7())]),
                )
                .await
        });
        wait_for_lock(&pool, "insert into journal_cursors", "advisory").await?;
        let task_pool = pool.clone();
        let change = tokio::spawn(async move {
            if rotate {
                machines::rotate_token(&task_pool, id, false, false)
                    .await
                    .map(|token| token.is_some())
            } else {
                machines::revoke(&task_pool, id).await.map_err(Into::into)
            }
        });
        wait_for_lock(&pool, "update machine_credentials", "transactionid").await?;
        gate.commit().await?;
        let (record, change) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(record, change)
        })
        .await?;
        assert_eq!(record??, IngestOutcome::Committed { up_to_seq: 1 });
        assert!(change??);
        assert_eq!(cursor(&pool, journal).await, 1);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_revoke_and_first_record_has_no_deadlock(pool: PgPool) -> TestResult {
    first_record_racing_credential_change(pool, false).await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_rotation_and_first_record_has_no_deadlock(pool: PgPool) -> TestResult {
    first_record_racing_credential_change(pool, true).await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rotated_session_cannot_ingest_before_session_close(pool: PgPool) -> TestResult {
    use canterlot_hub::machines;
    let seed = testkit::seed(&pool).await;
    let machine = machines::add(&pool, seed.owner_id, "rotated-session").await?;
    let new_token = machines::rotate_token(&pool, machine.machine_id, false, false)
        .await?
        .unwrap();
    assert_eq!(
        machines::verify(&pool, &new_token).await?,
        Some(machine.machine_id)
    );
    let journal = Uuid::now_v7();
    let records = batch(journal, vec![stored(Uuid::now_v7())]);
    let result = Ingest::new(pool.clone())
        .apply_batch(
            machine.machine_id,
            &sha2::Sha256::digest(machine.token.as_bytes()),
            JournalKind::Cj,
            &records,
        )
        .await;
    assert!(matches!(result, Err(IngestError::CredentialRevoked)));
    let count: i64 =
        sqlx::query_scalar("select count(*) from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                machine.machine_id,
                &sha2::Sha256::digest(new_token.as_bytes()),
                JournalKind::Cj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 1 }
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rotated_session_cannot_ingest_after_batch_check(pool: PgPool) -> TestResult {
    use canterlot_hub::machines;
    let seed = testkit::seed(&pool).await;
    let machine = machines::add(&pool, seed.owner_id, "rotation-after-check").await?;
    let id = machine.machine_id;
    let journal = Uuid::now_v7();
    let mut gate = pool.begin().await?;
    sqlx::query("lock table journal_cursors in access exclusive mode")
        .execute(&mut *gate)
        .await?;
    let ingest = Ingest::new(pool.clone());
    let record = tokio::spawn(async move {
        ingest
            .apply_batch(
                id,
                &sha2::Sha256::digest(machine.token.as_bytes()),
                JournalKind::Cj,
                &batch(journal, vec![stored(Uuid::now_v7())]),
            )
            .await
    });
    wait_for_lock(&pool, "select machine_id, kind, committed_seq", "relation").await?;
    machines::rotate_token(&pool, id, false, false)
        .await?
        .unwrap();
    gate.commit().await?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), record).await??;
    assert!(matches!(result, Err(IngestError::CredentialRevoked)));
    let count: i64 =
        sqlx::query_scalar("select count(*) from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 0);
    Ok(())
}

async fn setup(pool: &PgPool) -> (Seed, executions::StartedExecution, SubmittedInput) {
    let seed = testkit::seed(pool).await;
    let (execution, input) = setup_for(pool, &seed).await;
    (seed, execution, input)
}

async fn setup_for(pool: &PgPool, seed: &Seed) -> (executions::StartedExecution, SubmittedInput) {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let input = submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "synthetic input".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        dispatch_next(
            pool,
            seed.conversation_id,
            GateInputs {
                machine_available: true
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        DispatchOutcome::Dispatched { attempt: 1, .. }
    ));
    (execution, input)
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

fn stored(command_id: Uuid) -> record_body::Body {
    record_body::Body::CommandStored(CommandStored {
        machine_time_ms: 0,
        command_id: command_id.to_string(),
        dispatch_attempt: 1,
    })
}

fn accepted(command_id: Uuid) -> record_body::Body {
    record_body::Body::InputAccepted(InputAccepted {
        machine_time_ms: 0,
        command_id: command_id.to_string(),
        dispatch_attempt: 1,
    })
}

async fn cursor(pool: &PgPool, journal_id: Uuid) -> i64 {
    sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
        .bind(journal_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn snapshot(pool: &PgPool, input_id: Uuid) -> Value {
    sqlx::query_scalar(
        "select jsonb_build_object('input', to_jsonb(i), 'command', to_jsonb(c),
            'queue', to_jsonb(q), 'events', (select count(*) from events where conversation_id = i.conversation_id))
         from inputs i join commands c on c.id = i.command_id
         join queue_state q on q.conversation_id = i.conversation_id where i.id = $1",
    ).bind(input_id).fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stored_record_moves_input_on_machine(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let outcome = Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(journal, vec![stored(input.command_id)]),
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 1 });
    let state = snapshot(&pool, input.input_id).await;
    assert_eq!(state["input"]["state"], "on_machine");
    assert_eq!(state["command"]["delivery_state"], "stored_on_machine");
    assert!(!state["command"]["stored_at"].is_null());
    assert_eq!(state["queue"]["run_slot"], "dispatching");
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn duplicate_records_are_skipped_but_acked(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let records = batch(journal, vec![stored(input.command_id)]);
    let first = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &records,
        )
        .await?;
    let before = snapshot(&pool, input.input_id).await;
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await?,
        first
    );
    assert_eq!(first, IngestOutcome::Committed { up_to_seq: 1 });
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    let transitions: i64 = sqlx::query_scalar(
        "select count(*) from events where kind = 'input_state_changed' and payload->>'cause' = 'stored' and conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(transitions, 1);
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gap_stops_before_the_hole(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let mut records = batch(
        journal,
        vec![stored(input.command_id), accepted(input.command_id)],
    );
    records.records[1].seq = 3;
    let ingest = Ingest::new(pool.clone());
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await?,
        IngestOutcome::Gap {
            committed_seq: 1,
            got_seq: 3
        }
    );
    assert_eq!(cursor(&pool, journal).await, 1);
    assert_eq!(
        snapshot(&pool, input.input_id).await["input"]["state"],
        "on_machine"
    );
    records.records[1].seq = 2;
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    assert_eq!(
        snapshot(&pool, input.input_id).await["input"]["state"],
        "accepted"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ack_never_exceeds_commit(pool: PgPool) -> TestResult {
    let (seed, execution, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let mut records = batch(
        journal,
        vec![
            stored(input.command_id),
            accepted(input.command_id),
            stored(input.command_id),
            record_body::Body::RunFinished(RunFinished {
                execution_id: execution.execution_id.to_string(),
                input_command_id: input.command_id.to_string(),
            }),
        ],
    );
    records.records[2].body = None;
    let ingest = Ingest::new(pool.clone());
    assert!(matches!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await,
        Err(IngestError::UnknownRecord)
    ));
    assert_eq!(cursor(&pool, journal).await, 2);
    let state = snapshot(&pool, input.input_id).await;
    assert_eq!(state["input"]["state"], "accepted");
    assert_eq!(state["queue"]["run_slot"], "running");
    let mut replay = records.clone();
    replay.records.truncate(2);
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &replay
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    assert_eq!(snapshot(&pool, input.input_id).await, state);
    records.records[2].body = Some(RecordBody::default());
    assert!(matches!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await,
        Err(IngestError::UnknownRecord)
    ));
    assert_eq!(cursor(&pool, journal).await, 2);
    assert_eq!(snapshot(&pool, input.input_id).await, state);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_journal_is_rejected(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let other = testkit::seed(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(journal, vec![stored(input.command_id)]),
        )
        .await?;
    let before = snapshot(&pool, input.input_id).await;
    assert!(matches!(
        ingest
            .apply_batch(
                other.machine_id,
                &other.credential_digest(),
                JournalKind::Cj,
                &batch(journal, vec![accepted(input.command_id)])
            )
            .await,
        Err(IngestError::ForeignJournal)
    ));
    let owners: i64 =
        sqlx::query_scalar("select count(*) from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(owners, 1);
    assert_eq!(cursor(&pool, journal).await, 1);
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rejected_start_fails_execution(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let journal = Uuid::now_v7();
    let outcome = Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(
                journal,
                vec![record_body::Body::CommandRejected(CommandRejected {
                    command_id: execution.command_id.to_string(),
                    dispatch_attempt: 0,
                    reason: "resource_limit".into(),
                })],
            ),
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 1 });
    let state: (String, String, String, bool) = sqlx::query_as(
        "select e.state, e.outcome, c.delivery_state, c.completed_at is not null
         from executions e join commands c on c.execution_id = e.id where c.id = $1",
    )
    .bind(execution.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        state,
        (
            "failed".into(),
            "start_rejected".into(),
            "completed".into(),
            true
        )
    );
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_deferred_requeues(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &batch(
                    journal,
                    vec![
                        stored(input.command_id),
                        record_body::Body::DispatchDeferred(InputDispatchDeferred {
                            command_id: input.command_id.to_string(),
                            dispatch_attempt: 1,
                            reason: "generation_closed".into(),
                        })
                    ]
                )
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    let state = snapshot(&pool, input.input_id).await;
    assert_eq!(state["input"]["state"], "queued");
    assert_eq!(state["input"]["input_seq"], input.input_seq);
    assert_eq!(state["command"]["delivery_state"], "held");
    assert_eq!(state["queue"]["run_slot"], "free");
    assert_eq!(
        state["queue"]["hold_reasons"],
        json!([HoldReason::NoLiveExecution])
    );
    assert_eq!(cursor(&pool, journal).await, 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn receipt_mapping_and_run_finished(pool: PgPool) -> TestResult {
    for (body, expected, cause) in [
        (
            "transmission_started",
            "transmitting",
            "transmission_started",
        ),
        (
            "acceptance_unconfirmed",
            "acceptance_unconfirmed",
            "acceptance_undetermined",
        ),
        ("cancel_won", "cancelled", "cancel_won"),
        ("cancel_lost", "on_machine", "cancel_lost"),
        ("input_withdrawn", "queued", "withdrawn"),
        ("input_rejected", "rejected", "explicit_rejection"),
        (
            "command_rejected",
            "rejected",
            "rejected_before_transmission",
        ),
        ("run_finished", "accepted", "accepted"),
    ] {
        let (seed, execution, input) = setup(&pool).await;
        let command_id = input.command_id.to_string();
        let dispatch_attempt = 1;
        let reason = "synthetic_reason".to_owned();
        let fact = match body {
            "transmission_started" => {
                record_body::Body::TransmissionStarted(InputTransmissionStarted {
                    machine_time_ms: 0,
                    command_id,
                    dispatch_attempt,
                })
            }
            "acceptance_unconfirmed" => {
                record_body::Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                    command_id,
                    dispatch_attempt,
                    reason,
                })
            }
            "cancel_won" => record_body::Body::CancelWon(InputCancelWon {
                command_id,
                dispatch_attempt,
            }),
            "cancel_lost" => {
                sqlx::query("update inputs set cancel_requested = true where id = $1")
                    .bind(input.input_id)
                    .execute(&pool)
                    .await?;
                record_body::Body::CancelLost(InputCancelLost {
                    command_id,
                    dispatch_attempt,
                    reason,
                })
            }
            "input_withdrawn" => record_body::Body::InputWithdrawn(InputWithdrawn {
                command_id,
                dispatch_attempt,
            }),
            "input_rejected" => record_body::Body::InputRejected(InputRejected {
                command_id,
                dispatch_attempt,
                reason,
            }),
            "command_rejected" => record_body::Body::CommandRejected(CommandRejected {
                command_id,
                dispatch_attempt,
                reason,
            }),
            "run_finished" => record_body::Body::RunFinished(RunFinished {
                execution_id: execution.execution_id.to_string(),
                input_command_id: command_id,
            }),
            _ => unreachable!(),
        };
        let mut bodies = vec![stored(input.command_id)];
        if body == "run_finished" {
            bodies.push(accepted(input.command_id));
        }
        bodies.push(fact);
        let journal = Uuid::now_v7();
        let records = batch(journal, bodies);
        assert_eq!(
            Ingest::new(pool.clone())
                .apply_batch(
                    seed.machine_id,
                    &seed.credential_digest(),
                    JournalKind::Sj,
                    &records
                )
                .await?,
            IngestOutcome::Committed {
                up_to_seq: records.records.len() as u64
            }
        );
        let state = snapshot(&pool, input.input_id).await;
        assert_eq!(state["input"]["state"], expected, "{body}");
        assert_eq!(
            state["command"]["last_status"],
            if body == "cancel_lost" {
                "stored"
            } else {
                cause
            },
            "{body}"
        );
        if body == "run_finished" {
            assert_eq!(state["queue"]["run_slot"], "free");
        }
        if body == "cancel_lost" {
            assert_eq!(state["input"]["cancel_requested"], false);
        }
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn execution_facts_and_ignored_bodies_are_consumed(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let fact = |state: &str| {
        record_body::Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
            native_conversation_id: String::new(),
            execution_id: execution.execution_id.to_string(),
            generation: execution.generation as u64,
            state: state.into(),
            outcome: String::new(),
        })
    };
    let journal = Uuid::now_v7();
    let records = batch(
        journal,
        vec![
            fact("starting"),
            fact("running"),
            fact("created"),
            record_body::Body::JournalClosed(JournalClosed::default()),
            record_body::Body::BlockStarted(BlockStarted::default()),
            record_body::Body::BlockDelta(BlockDelta::default()),
            record_body::Body::BlockFinal(BlockFinal::default()),
            record_body::Body::BlockIncomplete(BlockIncomplete::default()),
            record_body::Body::PhaseChanged(PhaseChanged::default()),
        ],
    );
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 9 }
    );
    assert_eq!(cursor(&pool, journal).await, 9);
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "running");
    let diagnostic: Value = sqlx::query_scalar(
        "select payload from events where kind = 'execution_fact_rejected' and execution_id = $1",
    )
    .bind(execution.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(diagnostic["from"], "running");
    assert_eq!(diagnostic["to"], "created");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_record_rolls_back_effect_and_cursor(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let records = batch(
        journal,
        vec![stored(input.command_id), accepted(input.command_id)],
    );
    sqlx::query("alter table events add constraint reject_acceptance check (kind <> 'input_state_changed' or payload->>'state' <> 'accepted') not valid")
        .execute(&pool).await?;
    let error = ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("synthetic"));
    assert_eq!(cursor(&pool, journal).await, 1);
    let state = snapshot(&pool, input.input_id).await;
    assert_eq!(state["input"]["state"], "on_machine");
    assert_eq!(state["command"]["last_status"], "stored");
    sqlx::query("alter table events drop constraint reject_acceptance")
        .execute(&pool)
        .await?;
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_are_checked_before_any_record(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let records = batch(
        journal,
        vec![
            stored(input.command_id),
            record_body::Body::BlockFinal(BlockFinal {
                text: "private_record_text".repeat(MAX_EVENT_BATCH_BYTES / 10),
                ..BlockFinal::default()
            }),
        ],
    );
    let before = snapshot(&pool, input.input_id).await;
    let error = Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "batch_limit");
    assert!(!format!("{error:?} {error}").contains("private_record_text"));
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    let count: i64 = sqlx::query_scalar("select count(*) from journal_cursors")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_registration_has_one_owner(pool: PgPool) -> TestResult {
    let first = testkit::seed(&pool).await;
    let second = testkit::seed(&pool).await;
    let journal = Uuid::now_v7();
    let records = batch(
        journal,
        vec![record_body::Body::JournalClosed(JournalClosed::default())],
    );
    let ingest = Ingest::new(pool.clone());
    let first_credential = first.credential_digest();
    let second_credential = second.credential_digest();
    let (a, b) = tokio::join!(
        ingest.apply_batch(
            first.machine_id,
            &first_credential,
            JournalKind::Cj,
            &records
        ),
        ingest.apply_batch(
            second.machine_id,
            &second_credential,
            JournalKind::Cj,
            &records
        ),
    );
    assert!(matches!(
        (a, b),
        (
            Ok(IngestOutcome::Committed { up_to_seq: 1 }),
            Err(IngestError::ForeignJournal)
        ) | (
            Err(IngestError::ForeignJournal),
            Ok(IngestOutcome::Committed { up_to_seq: 1 })
        )
    ));
    let owners: i64 =
        sqlx::query_scalar("select count(*) from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(owners, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_and_unknown_receipts_are_consumed(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let before = snapshot(&pool, input.input_id).await;
    let journal = Uuid::now_v7();
    let records = batch(
        journal,
        vec![
            record_body::Body::CommandStored(CommandStored {
                machine_time_ms: 0,
                command_id: input.command_id.to_string(),
                dispatch_attempt: 0,
            }),
            stored(Uuid::now_v7()),
        ],
    );
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    assert_eq!(cursor(&pool, journal).await, 2);
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn other_commands_are_stored_and_rejected(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let command = Uuid::now_v7();
    sqlx::query("insert into commands (id, machine_id, conversation_id, kind, payload, delivery_state) values ($1, $2, $3, 'interrupt', '{}', 'pending')")
        .bind(command).bind(seed.machine_id).bind(seed.conversation_id).execute(&pool).await?;
    let before = snapshot(&pool, input.input_id).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let stored = record_body::Body::CommandStored(CommandStored {
        machine_time_ms: 0,
        command_id: command.to_string(),
        dispatch_attempt: 0,
    });
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(journal, vec![stored.clone()]),
        )
        .await?;
    let state: (String, bool) =
        sqlx::query_as("select delivery_state, stored_at is not null from commands where id = $1")
            .bind(command)
            .fetch_one(&pool)
            .await?;
    assert_eq!(state, ("stored_on_machine".into(), true));
    let records = batch(
        journal,
        vec![
            stored.clone(),
            record_body::Body::CommandRejected(CommandRejected {
                command_id: command.to_string(),
                dispatch_attempt: 0,
                reason: "unsupported\0command".into(),
            }),
            stored,
        ],
    );
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 3 }
    );
    let state: (String, Value, bool) = sqlx::query_as(
        "select delivery_state, result, completed_at is not null from commands where id = $1",
    )
    .bind(command)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        state,
        (
            "completed".into(),
            json!({"rejected": "unsupportedcommand"}),
            true
        )
    );
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cursor_is_locked_before_queue_state(pool: PgPool) -> TestResult {
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    sqlx::query("insert into journal_cursors (machine_id, journal_id, kind) values ($1, $2, 'cj')")
        .bind(seed.machine_id)
        .bind(journal)
        .execute(&pool)
        .await?;
    let mut queue_lock = pool.begin().await?;
    sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
        .bind(seed.conversation_id)
        .execute(&mut *queue_lock)
        .await?;
    let ingest = Ingest::new(pool.clone());
    let task = tokio::spawn(async move {
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &batch(journal, vec![stored(input.command_id)]),
            )
            .await
    });
    timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock' and query like 'select q.%queue_state%')")
                .fetch_one(&pool).await.unwrap();
            if waiting { break; }
            sleep(Duration::from_millis(10)).await;
        }
    }).await?;
    let error = sqlx::query(
        "select committed_seq from journal_cursors where journal_id = $1 for update nowait",
    )
    .bind(journal)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("55P03")
    );
    assert_eq!(cursor(&pool, journal).await, 0);
    queue_lock.commit().await?;
    assert_eq!(
        timeout(Duration::from_secs(2), task).await???,
        IngestOutcome::Committed { up_to_seq: 1 }
    );
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

async fn world(pool: &PgPool, conversation_id: Uuid) -> Value {
    sqlx::query_scalar(
        "select jsonb_build_object(
            'executions', (select coalesce(jsonb_agg(to_jsonb(e) order by e.id), '[]') from executions e where e.conversation_id = $1),
            'commands', (select coalesce(jsonb_agg(to_jsonb(c) order by c.id), '[]') from commands c where c.conversation_id = $1),
            'inputs', (select coalesce(jsonb_agg(to_jsonb(i) order by i.id), '[]') from inputs i where i.conversation_id = $1),
            'queue', (select to_jsonb(q) from queue_state q where q.conversation_id = $1),
            'events', (select count(*) from events where conversation_id = $1))",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn execution_fact(execution_id: Uuid, generation: i64, state: &str) -> record_body::Body {
    record_body::Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
        native_conversation_id: String::new(),
        execution_id: execution_id.to_string(),
        generation: generation as u64,
        state: state.into(),
        outcome: String::new(),
    })
}

async fn start_only(pool: &PgPool, seed: &Seed) -> executions::StartedExecution {
    executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_start_rejection_for_stopped_execution_is_consumed(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = start_only(&pool, &seed).await;
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopped,
        Some("start_cancelled"),
        Uuid::now_v7(),
    )
    .await?;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let records = batch(
        journal,
        vec![
            record_body::Body::CommandRejected(CommandRejected {
                command_id: execution.command_id.to_string(),
                dispatch_attempt: 0,
                reason: "resource_limit".into(),
            }),
            record_body::Body::JournalClosed(JournalClosed::default()),
        ],
    );
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 2 }
    );
    assert_eq!(cursor(&pool, journal).await, 2);
    let state: (String, String, String) = sqlx::query_as(
        "select e.state, e.outcome, c.delivery_state
         from executions e join commands c on c.execution_id = e.id where c.id = $1",
    )
    .bind(execution.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        state,
        (
            "stopped".into(),
            "start_cancelled".into(),
            "completed".into()
        )
    );
    let diagnostic: Value = sqlx::query_scalar(
        "select payload from events where kind = 'execution_fact_rejected' and execution_id = $1",
    )
    .bind(execution.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(diagnostic["from"], "stopped");
    assert_eq!(diagnostic["to"], "failed");
    assert_eq!(diagnostic["generation"], execution.generation);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_rejection_for_completed_command_leaves_execution_untouched(
    pool: PgPool,
) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = start_only(&pool, &seed).await;
    sqlx::query("update commands set delivery_state = 'completed', completed_at = now(), result = '{\"done\": true}' where id = $1")
        .bind(execution.command_id)
        .execute(&pool)
        .await?;
    let before = world(&pool, seed.conversation_id).await;
    let journal = Uuid::now_v7();
    let outcome = Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(
                journal,
                vec![record_body::Body::CommandRejected(CommandRejected {
                    command_id: execution.command_id.to_string(),
                    dispatch_attempt: 0,
                    reason: "late".into(),
                })],
            ),
        )
        .await?;
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 1 });
    assert_eq!(world(&pool, seed.conversation_id).await, before);
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_rejection_for_other_attempt_is_ignored(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = start_only(&pool, &seed).await;
    let before = world(&pool, seed.conversation_id).await;
    let journal = Uuid::now_v7();
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(
                journal,
                vec![record_body::Body::CommandRejected(CommandRejected {
                    command_id: execution.command_id.to_string(),
                    dispatch_attempt: 7,
                    reason: "stale".into(),
                })],
            ),
        )
        .await?;
    assert_eq!(world(&pool, seed.conversation_id).await, before);
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_rejection_without_execution_or_queue_is_consumed(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = start_only(&pool, &seed).await;
    sqlx::query("update commands set execution_id = null where id = $1")
        .bind(execution.command_id)
        .execute(&pool)
        .await?;
    let other = testkit::seed(&pool).await;
    let second = start_only(&pool, &other).await;
    sqlx::query("delete from queue_state where conversation_id = $1")
        .bind(other.conversation_id)
        .execute(&pool)
        .await?;
    let journal = Uuid::now_v7();
    let rejected = |command: Uuid| {
        record_body::Body::CommandRejected(CommandRejected {
            command_id: command.to_string(),
            dispatch_attempt: 0,
            reason: "resource_limit".into(),
        })
    };
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &batch(journal, vec![rejected(execution.command_id)]),
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 1 }
    );
    let state: String = sqlx::query_scalar("select delivery_state from commands where id = $1")
        .bind(execution.command_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "completed");
    let journal = Uuid::now_v7();
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                other.machine_id,
                &other.credential_digest(),
                JournalKind::Cj,
                &batch(journal, vec![rejected(second.command_id)]),
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 1 }
    );
    let state: (String, String) = sqlx::query_as(
        "select e.state, c.delivery_state from executions e join commands c on c.execution_id = e.id where c.id = $1",
    )
    .bind(second.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(state, ("created".into(), "completed".into()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unchanged_execution_state_emits_no_event(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution = start_only(&pool, &seed).await;
    let before = world(&pool, seed.conversation_id).await;
    let journal = Uuid::now_v7();
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(
                journal,
                vec![execution_fact(
                    execution.execution_id,
                    execution.generation,
                    "created",
                )],
            ),
        )
        .await?;
    assert_eq!(world(&pool, seed.conversation_id).await, before);
    assert_eq!(cursor(&pool, journal).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn duplicates_are_skipped_without_locking_the_cursor(pool: PgPool) -> TestResult {
    use std::time::Duration;
    use tokio::time::timeout;

    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let records = batch(
        journal,
        vec![stored(input.command_id), accepted(input.command_id)],
    );
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &records,
        )
        .await?;
    let mut holder = pool.begin().await?;
    sqlx::query("select committed_seq from journal_cursors where journal_id = $1 for update")
        .bind(journal)
        .execute(&mut *holder)
        .await?;
    let replay = timeout(
        Duration::from_secs(2),
        ingest.apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &records,
        ),
    )
    .await??;
    assert_eq!(replay, IngestOutcome::Committed { up_to_seq: 2 });
    holder.rollback().await?;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn seq_zero_is_an_invalid_record(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let mut records = batch(journal, vec![stored(input.command_id)]);
    records.records[0].seq = 0;
    let before = snapshot(&pool, input.input_id).await;
    assert!(matches!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await,
        Err(IngestError::InvalidRecord)
    ));
    let mut with_valid_first = batch(
        journal,
        vec![stored(input.command_id), accepted(input.command_id)],
    );
    with_valid_first.records[1].seq = 0;
    assert!(matches!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &with_valid_first
            )
            .await,
        Err(IngestError::InvalidRecord)
    ));
    let cursors: i64 = sqlx::query_scalar("select count(*) from journal_cursors")
        .fetch_one(&pool)
        .await?;
    assert_eq!(cursors, 0);
    assert_eq!(snapshot(&pool, input.input_id).await, before);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cursor_row_exists_only_after_the_first_record_is_applied(pool: PgPool) -> TestResult {
    let (seed, _, input) = setup(&pool).await;
    let ingest = Ingest::new(pool.clone());
    let count = |journal: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from journal_cursors where journal_id = $1",
            )
            .bind(journal)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let gapped = Uuid::now_v7();
    let mut records = batch(gapped, vec![stored(input.command_id)]);
    records.records[0].seq = 2;
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await?,
        IngestOutcome::Gap {
            committed_seq: 0,
            got_seq: 2
        }
    );
    assert_eq!(count(gapped).await, 0);
    let unknown = Uuid::now_v7();
    let mut records = batch(unknown, vec![stored(input.command_id)]);
    records.records[0].body = None;
    assert!(matches!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &records
            )
            .await,
        Err(IngestError::UnknownRecord)
    ));
    assert_eq!(count(unknown).await, 0);
    let empty = batch(Uuid::now_v7(), vec![]);
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &empty
            )
            .await?,
        IngestOutcome::Committed { up_to_seq: 0 }
    );
    let applied = Uuid::now_v7();
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Cj,
            &batch(applied, vec![stored(input.command_id)]),
        )
        .await?;
    assert_eq!(count(applied).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn journal_id_has_a_single_owner_in_the_schema(pool: PgPool) -> TestResult {
    let first = testkit::seed(&pool).await;
    let second = testkit::seed(&pool).await;
    let journal = Uuid::now_v7();
    let insert = |machine: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "insert into journal_cursors (machine_id, journal_id, kind) values ($1, $2, 'cj')",
            )
            .bind(machine)
            .bind(journal)
            .execute(&pool)
            .await
        }
    };
    insert(first.machine_id).await?;
    let error = insert(second.machine_id).await.unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ignored_records_are_counted_per_ingest(pool: PgPool) -> TestResult {
    let (seed, _, _) = setup(&pool).await;
    let records = batch(
        Uuid::now_v7(),
        vec![
            record_body::Body::JournalClosed(JournalClosed::default()),
            record_body::Body::PhaseChanged(PhaseChanged::default()),
        ],
    );
    let first = Ingest::new(pool.clone());
    let second = Ingest::new(pool.clone());
    first
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await?;
    assert_eq!(first.ignored_record_count(), 2);
    assert_eq!(second.ignored_record_count(), 0);
    first
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await?;
    assert_eq!(first.ignored_record_count(), 2);
    Ok(())
}

struct Foreign {
    seed: Seed,
    execution: executions::StartedExecution,
    input: Option<SubmittedInput>,
}

#[derive(Clone, Copy)]
enum Stage {
    Created,
    Dispatched,
    Running,
}

async fn foreign_world(pool: &PgPool, stage: Stage) -> Foreign {
    let seed = testkit::seed(pool).await;
    if matches!(stage, Stage::Created) {
        let execution = start_only(pool, &seed).await;
        return Foreign {
            seed,
            execution,
            input: None,
        };
    }
    let (execution, input) = setup_for(pool, &seed).await;
    if matches!(stage, Stage::Running) {
        let own = batch(
            Uuid::now_v7(),
            vec![stored(input.command_id), accepted(input.command_id)],
        );
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &own,
            )
            .await
            .unwrap();
    }
    Foreign {
        seed,
        execution,
        input: Some(input),
    }
}

async fn assert_isolated(
    pool: &PgPool,
    stage: Stage,
    name: &str,
    body: impl Fn(&Foreign) -> Vec<record_body::Body>,
) {
    let victim = foreign_world(pool, stage).await;
    let attacker = testkit::seed(pool).await;
    let before = world(pool, victim.seed.conversation_id).await;
    let journal = Uuid::now_v7();
    let mut bodies = body(&victim);
    bodies.push(record_body::Body::JournalClosed(JournalClosed::default()));
    let count = bodies.len() as u64;
    let outcome = Ingest::new(pool.clone())
        .apply_batch(
            attacker.machine_id,
            &attacker.credential_digest(),
            JournalKind::Sj,
            &batch(journal, bodies),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        IngestOutcome::Committed { up_to_seq: count },
        "{name}"
    );
    assert_eq!(cursor(pool, journal).await, count as i64, "{name}");
    assert_eq!(
        world(pool, victim.seed.conversation_id).await,
        before,
        "{name} affected another machine"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn records_of_one_machine_never_affect_another_machine(pool: PgPool) -> TestResult {
    let start = |v: &Foreign| v.execution.command_id.to_string();
    let input = |v: &Foreign| v.input.as_ref().unwrap().command_id.to_string();
    assert_isolated(
        &pool,
        Stage::Created,
        "command_stored(start_execution)",
        |v| {
            vec![record_body::Body::CommandStored(CommandStored {
                machine_time_ms: 0,
                command_id: start(v),
                dispatch_attempt: 0,
            })]
        },
    )
    .await;
    assert_isolated(
        &pool,
        Stage::Created,
        "command_rejected(start_execution)",
        |v| {
            vec![record_body::Body::CommandRejected(CommandRejected {
                command_id: start(v),
                dispatch_attempt: 0,
                reason: "resource_limit".into(),
            })]
        },
    )
    .await;
    assert_isolated(
        &pool,
        Stage::Dispatched,
        "command_stored(deliver_input)",
        |v| vec![stored(v.input.as_ref().unwrap().command_id)],
    )
    .await;
    assert_isolated(
        &pool,
        Stage::Dispatched,
        "command_rejected(deliver_input)",
        |v| {
            vec![record_body::Body::CommandRejected(CommandRejected {
                command_id: input(v),
                dispatch_attempt: 1,
                reason: "resource_limit".into(),
            })]
        },
    )
    .await;
    assert_isolated(&pool, Stage::Dispatched, "dispatch_deferred", |v| {
        vec![record_body::Body::DispatchDeferred(InputDispatchDeferred {
            command_id: input(v),
            dispatch_attempt: 1,
            reason: "generation_closed".into(),
        })]
    })
    .await;
    assert_isolated(&pool, Stage::Dispatched, "input_accepted", |v| {
        vec![accepted(v.input.as_ref().unwrap().command_id)]
    })
    .await;
    assert_isolated(&pool, Stage::Dispatched, "input_rejected", |v| {
        vec![record_body::Body::InputRejected(InputRejected {
            command_id: input(v),
            dispatch_attempt: 1,
            reason: "refused".into(),
        })]
    })
    .await;
    assert_isolated(&pool, Stage::Running, "run_finished", |v| {
        vec![record_body::Body::RunFinished(RunFinished {
            execution_id: v.execution.execution_id.to_string(),
            input_command_id: input(v),
        })]
    })
    .await;
    assert_isolated(&pool, Stage::Running, "execution_state", |v| {
        vec![execution_fact(
            v.execution.execution_id,
            v.execution.generation,
            "stopping",
        )]
    })
    .await;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn latest_record_replays_compare_committed_body_digest(pool: PgPool) -> TestResult {
    use prost::Message;
    use sha2::{Digest, Sha256};

    let (seed, _, input) = setup(&pool).await;
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    let original = batch(journal, vec![stored(input.command_id)]);
    let expected = IngestOutcome::Committed { up_to_seq: 1 };
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &original
            )
            .await?,
        expected
    );
    let digest: Vec<u8> =
        sqlx::query_scalar("select last_record_digest from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        digest,
        Sha256::digest(original.records[0].body.as_ref().unwrap().encode_to_vec()).to_vec()
    );
    let before: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &original
            )
            .await?,
        expected
    );
    let after: i64 = sqlx::query_scalar("select count(*) from events")
        .fetch_one(&pool)
        .await?;
    assert_eq!(before, after);
    let different = batch(journal, vec![accepted(input.command_id)]);
    assert_eq!(
        ingest
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Cj,
                &different
            )
            .await?,
        expected
    );
    let diagnostics: Vec<Value> =
        sqlx::query_scalar("select payload from events where kind = 'journal_record_mismatch'")
            .fetch_all(&pool)
            .await?;
    assert_eq!(diagnostics, vec![json!({"journal_id": journal, "seq": 1})]);
    assert_eq!(cursor(&pool, journal).await, 1);
    let state: String = sqlx::query_scalar("select state from inputs where id = $1")
        .bind(input.input_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "on_machine");
    let retained: Vec<u8> =
        sqlx::query_scalar("select last_record_digest from journal_cursors where journal_id = $1")
            .bind(journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(retained, digest);
    Ok(())
}
