use std::time::Duration;

use canterlot_hub::{
    config::HubTimers,
    delivery::{
        control::{ControlError, StopExecutionOutcome, request_stop_execution},
        dispatch::{BlockReason, DispatchOutcome, GateInputs, dispatch_next},
        receipts::{InputReceipt, apply_input_receipt},
        submit::{ContentBlock, SubmitInput, SubmittedInput, submit_input},
    },
    executions::{self, StartedExecution},
    ingest::Ingest,
    model::{ExecutionState, HoldReason, PauseReason},
    session::Availability,
    testkit::{self, Seed},
};
use canterlot_proto::{limits::MAX_MESSAGE_BYTES, worker::v1::*};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, types::Json};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{
    Request,
    transport::{Certificate, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn start(pool: &PgPool, seed: &Seed, state: ExecutionState) -> StartedExecution {
    let run = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for next in [
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Stopping,
    ] {
        if state == ExecutionState::Created {
            break;
        }
        executions::set_state(pool, run.execution_id, next, None, Uuid::now_v7())
            .await
            .unwrap();
        if next == state {
            break;
        }
    }
    run
}

async fn stop(pool: &PgPool, seed: &Seed, force: bool, key: &str) -> StopExecutionOutcome {
    request_stop_execution(
        pool,
        seed.owner_id,
        seed.conversation_id,
        force,
        key,
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

async fn input(pool: &PgPool, seed: &Seed) -> SubmittedInput {
    submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "synthetic".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
}

async fn dispatch(pool: &PgPool, seed: &Seed, machine_reconciled: bool) -> DispatchOutcome {
    dispatch_next(
        pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

fn observation(run: &StartedExecution, state: &str, outcome: &str) -> ExecutionObservation {
    ExecutionObservation {
        execution_id: run.execution_id.to_string(),
        generation: run.generation.try_into().unwrap(),
        state: state.into(),
        outcome: outcome.into(),
        supervisor_attached: true,
        sj_journal_id: Uuid::now_v7().to_string(),
    }
}

async fn execution_state(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select state from executions where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn pauses(pool: &PgPool, seed: &Seed) -> Vec<PauseReason> {
    sqlx::query_scalar::<_, Json<Vec<PauseReason>>>(
        "select pause_reasons from queue_state where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(pool)
    .await
    .unwrap()
    .0
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_execution_pauses_queue_before_command(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Running).await;
    let head = input(&pool, &seed).await;
    dispatch(&pool, &seed, true).await;
    sqlx::query("create function require_stop_pause() returns trigger language plpgsql as $$ begin if new.kind = 'stop_execution' and not exists(select 1 from queue_state where conversation_id = new.conversation_id and pause_reasons @> '[{\"reason\":\"user_stop\"}]'::jsonb) then raise exception 'queue must be paused'; end if; return new; end $$").execute(&pool).await?;
    sqlx::query("create trigger require_stop_pause before insert on commands for each row execute function require_stop_pause() ").execute(&pool).await?;
    let correlation = Uuid::now_v7();
    let StopExecutionOutcome::Requested {
        command_id,
        force: false,
    } = request_stop_execution(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        false,
        "stop",
        correlation,
    )
    .await?
    else {
        panic!("expected requested");
    };
    assert_eq!(pauses(&pool, &seed).await, [PauseReason::UserStop]);
    let row: (Uuid, i64, String, Value, String, String, String, String) = sqlx::query_as(
        "select c.execution_id, c.execution_generation, c.delivery_state, c.payload, q.xmin::text, c.xmin::text, e.xmin::text, k.xmin::text from commands c join queue_state q on q.conversation_id = c.conversation_id join events e on e.correlation_id = $2 and e.kind = 'queue_paused' join idempotency_keys k on k.owner_id = $3 and k.key = 'stop' where c.id = $1")
        .bind(command_id).bind(correlation).bind(seed.owner_id).fetch_one(&pool).await?;
    assert_eq!(
        (row.0, row.1, row.2.as_str(), row.3),
        (
            run.execution_id,
            run.generation,
            "pending",
            json!({"force": false})
        )
    );
    assert_eq!(row.4, row.5);
    assert_eq!(row.5, row.6);
    assert_eq!(row.6, row.7);
    let target: Uuid = sqlx::query_scalar("select target_command_id from commands where conversation_id = $1 and kind = 'withdraw_input'").bind(seed.conversation_id).fetch_one(&pool).await?;
    assert_eq!(target, head.command_id);
    assert_eq!(execution_state(&pool, run.execution_id).await, "running");
    assert!(
        matches!(dispatch(&pool, &seed, true).await, DispatchOutcome::Blocked(reasons) if reasons.contains(&BlockReason::Paused(vec![PauseReason::UserStop])))
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_execution_is_created_once_per_key(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Created).await;
    let first = stop(&pool, &seed, false, "stop").await;
    assert_eq!(stop(&pool, &seed, false, "stop").await, first);
    let StopExecutionOutcome::Requested { command_id, .. } = first else {
        panic!("expected request");
    };
    assert_eq!(
        stop(&pool, &seed, false, "other-key").await,
        StopExecutionOutcome::AlreadyRequested { command_id }
    );
    let count: i64 = sqlx::query_scalar(
        "select count(*) from commands where execution_id = $1 and kind = 'stop_execution'",
    )
    .bind(run.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 1);
    assert_eq!(execution_state(&pool, run.execution_id).await, "created");
    assert!(matches!(
        request_stop_execution(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            true,
            "stop",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::IdempotencyConflict)
    ));
    let foreign = testkit::seed(&pool).await;
    assert!(matches!(
        request_stop_execution(
            &pool,
            foreign.owner_id,
            seed.conversation_id,
            false,
            "foreign",
            Uuid::now_v7()
        )
        .await,
        Err(ControlError::NotFound)
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn force_stop_is_a_second_command(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Running).await;
    let graceful = stop(&pool, &seed, false, "graceful").await;
    let forced = stop(&pool, &seed, true, "force").await;
    let StopExecutionOutcome::Requested {
        command_id: graceful,
        force: false,
    } = graceful
    else {
        panic!("expected graceful");
    };
    let StopExecutionOutcome::Requested {
        command_id: forced,
        force: true,
    } = forced
    else {
        panic!("expected forced");
    };
    assert_ne!(graceful, forced);
    assert_eq!(
        stop(&pool, &seed, true, "force").await,
        StopExecutionOutcome::Requested {
            command_id: forced,
            force: true
        }
    );
    assert_eq!(
        stop(&pool, &seed, true, "force-again").await,
        StopExecutionOutcome::AlreadyRequested { command_id: forced }
    );
    let payloads: Vec<Value> = sqlx::query_scalar("select payload from commands where execution_id = $1 and kind = 'stop_execution' order by created_at, id").bind(run.execution_id).fetch_all(&pool).await?;
    assert_eq!(payloads, [json!({"force": false}), json!({"force": true})]);
    assert_eq!(execution_state(&pool, run.execution_id).await, "running");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_without_live_execution_is_nothing_to_stop(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    assert_eq!(
        stop(&pool, &seed, false, "stop").await,
        StopExecutionOutcome::NothingToStop
    );
    assert_eq!(pauses(&pool, &seed).await, [PauseReason::UserStop]);
    let run = start(&pool, &seed, ExecutionState::Created).await;
    assert_eq!(
        stop(&pool, &seed, false, "stop").await,
        StopExecutionOutcome::NothingToStop
    );
    assert_eq!(execution_state(&pool, run.execution_id).await, "created");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'stop_execution'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_refreshes_observed_execution(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Running).await;
    sqlx::query("update executions set observed_at = now() - interval '1 day' where id = $1")
        .bind(run.execution_id)
        .execute(&pool)
        .await?;
    let before = Utc::now();
    Ingest::new(pool.clone())
        .reconcile(
            seed.machine_id,
            &ReconcileReport {
                executions: vec![observation(&run, "running", "")],
            },
        )
        .await?;
    let observed: DateTime<Utc> =
        sqlx::query_scalar("select observed_at from executions where id = $1")
            .bind(run.execution_id)
            .fetch_one(&pool)
            .await?;
    assert!(observed >= before);
    assert_eq!(execution_state(&pool, run.execution_id).await, "running");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_applies_worker_terminal_state_as_late_fact(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Stopping).await;
    let report = ReconcileReport {
        executions: vec![observation(&run, "stopped", "user_stop")],
    };
    let ingest = Ingest::new(pool.clone());
    ingest.reconcile(seed.machine_id, &report).await?;
    ingest.reconcile(seed.machine_id, &report).await?;
    let row: (String, Option<String>) =
        sqlx::query_as("select state, outcome from executions where id = $1")
            .bind(run.execution_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, ("stopped".into(), Some("user_stop".into())));
    let count: i64 = sqlx::query_scalar("select count(*) from events where execution_id = $1 and kind = 'execution_state_changed' and payload->>'state' = 'stopped'").bind(run.execution_id).fetch_one(&pool).await?;
    assert_eq!(count, 1);
    ingest
        .reconcile(
            seed.machine_id,
            &ReconcileReport {
                executions: vec![observation(&run, "running", "")],
            },
        )
        .await?;
    assert_eq!(execution_state(&pool, run.execution_id).await, "stopped");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_marks_unknown_nonterminal_execution_owner_lost_with_divergence_event(
    pool: PgPool,
) -> TestResult {
    for state in [
        ExecutionState::Created,
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Stopping,
    ] {
        let seed = testkit::seed(&pool).await;
        let run = start(&pool, &seed, state).await;
        let ingest = Ingest::new(pool.clone());
        ingest
            .reconcile(seed.machine_id, &ReconcileReport::default())
            .await?;
        ingest
            .reconcile(seed.machine_id, &ReconcileReport::default())
            .await?;
        let expected = if state == ExecutionState::Created {
            "created"
        } else {
            "owner_lost"
        };
        assert_eq!(execution_state(&pool, run.execution_id).await, expected);
        let events: Vec<Value> = sqlx::query_scalar(
            "select payload from events where execution_id = $1 and kind = 'state_divergence'",
        )
        .bind(run.execution_id)
        .fetch_all(&pool)
        .await?;
        if state == ExecutionState::Created {
            assert!(events.is_empty());
        } else {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["reason"], "execution_unknown_to_worker");
            assert!(pauses(&pool, &seed).await.contains(&PauseReason::OwnerLost));
        }
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_does_not_create_unknown_executions(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let unknown = Uuid::now_v7();
    Ingest::new(pool.clone())
        .reconcile(
            seed.machine_id,
            &ReconcileReport {
                executions: vec![ExecutionObservation {
                    execution_id: unknown.to_string(),
                    generation: 1,
                    state: "running".into(),
                    ..Default::default()
                }],
            },
        )
        .await?;
    let count: i64 = sqlx::query_scalar("select count(*) from executions where id = $1")
        .bind(unknown)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    let (scope, conversation, payload): (String, Option<Uuid>, Value) = sqlx::query_as(
        "select scope, conversation_id, payload from events where kind = 'state_divergence'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(scope, format!("owner:{}:conversations", seed.owner_id));
    assert_eq!(conversation, None);
    assert_eq!(payload["reason"], "unknown_execution");
    assert_eq!(payload["execution_id"], unknown.to_string());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn dispatch_waits_for_reconciliation(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Running).await;
    let head = input(&pool, &seed).await;
    let hub = testkit::spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_secs(3600),
            ..Default::default()
        },
    )
    .await;
    let endpoint = Endpoint::from_shared(format!("https://{}", hub.addr))?.tls_config(
        ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(&hub.pki.ca_pem))
            .domain_name("localhost"),
    )?;
    let hello = Hello {
        installation_id: hub.installation.installation_id.to_string(),
        machine_id: seed.machine_id.to_string(),
        restore_generation: hub.installation.restore_generation.to_string(),
        protocol_version: canterlot_hub::grpc::PROTOCOL_VERSION,
        connection_id: Uuid::now_v7().to_string(),
        cj_journal_id: Uuid::now_v7().to_string(),
        boot_id: Uuid::now_v7().to_string(),
        stream_kind: StreamKind::Control as i32,
        optional_capabilities: vec!["exec_v1".into()],
        ..Default::default()
    };
    let token = canterlot_hub::machines::rotate_token(&pool, seed.machine_id, false, false)
        .await?
        .unwrap();
    let (tx, rx) = mpsc::channel(8);
    tx.send(ControlUp {
        msg: Some(control_up::Msg::Hello(hello.clone())),
    })
    .await?;
    let mut req = Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let mut client =
        worker_control_client::WorkerControlClient::new(endpoint.clone().connect().await?)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
    let mut control = client.control(req).await?.into_inner();
    assert!(matches!(
        control.message().await?.unwrap().msg,
        Some(control_down::Msg::HelloReply(HelloReply {
            result: Some(hello_reply::Result::Accepted(Accepted { capabilities, .. })),
        })) if capabilities == ["exec_v1"]
    ));
    let (data_tx, rx) = mpsc::channel(8);
    data_tx
        .send(DataUp {
            msg: Some(data_up::Msg::Hello(Hello {
                stream_kind: StreamKind::Data as i32,
                ..hello
            })),
        })
        .await?;
    let mut req = Request::new(ReceiverStream::new(rx));
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse()?);
    let mut data = worker_data_client::WorkerDataClient::new(endpoint.connect().await?)
        .data(req)
        .await?
        .into_inner();
    assert!(matches!(
        data.message().await?.unwrap().msg,
        Some(data_down::Msg::HelloReply(HelloReply {
            result: Some(hello_reply::Result::Accepted(Accepted { capabilities, .. })),
        })) if capabilities == ["exec_v1"]
    ));
    assert_eq!(
        hub.registry.availability(seed.machine_id),
        Availability::Available
    );
    assert!(!hub.registry.is_reconciled(seed.machine_id));
    assert_eq!(
        dispatch(&pool, &seed, hub.registry.is_reconciled(seed.machine_id)).await,
        DispatchOutcome::Blocked(vec![BlockReason::MachineNotReconciled])
    );
    tx.send(ControlUp {
        msg: Some(control_up::Msg::ReconcileReport(ReconcileReport {
            executions: vec![observation(&run, "running", "")],
        })),
    })
    .await?;
    timeout(Duration::from_secs(3), async {
        loop {
            let state: String = sqlx::query_scalar("select state from inputs where id = $1")
                .bind(head.input_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "dispatched" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert!(hub.registry.is_reconciled(seed.machine_id));
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn dispatch_deferred_generation_closed_requeues_with_hold(pool: PgPool) -> TestResult {
    for (reason, hold) in [
        ("generation_closed", HoldReason::NoLiveExecution),
        ("execution_stopped", HoldReason::NoLiveExecution),
        ("storage_low", HoldReason::StorageLow),
        ("other", HoldReason::NoLiveExecution),
    ] {
        let seed = testkit::seed(&pool).await;
        start(&pool, &seed, ExecutionState::Running).await;
        let head = input(&pool, &seed).await;
        dispatch(&pool, &seed, true).await;
        apply_input_receipt(
            &pool,
            head.command_id,
            1,
            InputReceipt::DispatchDeferred {
                reason: reason.into(),
            },
            Uuid::now_v7(),
        )
        .await?;
        let row: (String, String, Json<Vec<HoldReason>>) = sqlx::query_as("select i.state, q.run_slot, q.hold_reasons from inputs i join queue_state q on q.conversation_id = i.conversation_id where i.id = $1").bind(head.input_id).fetch_one(&pool).await?;
        assert_eq!((row.0.as_str(), row.1.as_str()), ("queued", "free"));
        assert_eq!(row.2.0, vec![hold.clone()]);
        assert_eq!(
            dispatch(&pool, &seed, true).await,
            DispatchOutcome::Blocked(vec![BlockReason::Held(vec![hold])])
        );
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_preserves_generation_and_machine_boundaries(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let own = start(&pool, &seed, ExecutionState::Created).await;
    let foreign_seed = testkit::seed(&pool).await;
    let foreign = start(&pool, &foreign_seed, ExecutionState::Running).await;
    let mut stale = observation(&own, "running", "");
    stale.generation += 1;
    Ingest::new(pool.clone())
        .reconcile(
            seed.machine_id,
            &ReconcileReport {
                executions: vec![stale, observation(&foreign, "stopped", "user_stop")],
            },
        )
        .await?;
    assert_eq!(execution_state(&pool, own.execution_id).await, "created");
    assert_eq!(
        execution_state(&pool, foreign.execution_id).await,
        "running"
    );
    let observed: Option<DateTime<Utc>> =
        sqlx::query_scalar("select observed_at from executions where id = $1")
            .bind(own.execution_id)
            .fetch_one(&pool)
            .await?;
    assert!(observed.is_none());
    let foreign_events: i64 = sqlx::query_scalar(
        "select count(*) from events where execution_id = $1 and kind = 'state_divergence'",
    )
    .bind(foreign.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(foreign_events, 0);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn reconcile_diagnoses_illegal_terminal_transition(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let run = start(&pool, &seed, ExecutionState::Running).await;
    Ingest::new(pool.clone())
        .reconcile(
            seed.machine_id,
            &ReconcileReport {
                executions: vec![observation(&run, "stopped", "user_stop")],
            },
        )
        .await?;
    assert_eq!(execution_state(&pool, run.execution_id).await, "running");
    let diagnostic: Value = sqlx::query_scalar(
        "select payload from events where execution_id = $1 and kind = 'execution_fact_rejected'",
    )
    .bind(run.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(diagnostic["from"], "running");
    assert_eq!(diagnostic["to"], "stopped");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stop_execution_rolls_back_pause_command_and_idempotency_on_event_failure(
    pool: PgPool,
) -> TestResult {
    let seed = testkit::seed(&pool).await;
    start(&pool, &seed, ExecutionState::Running).await;
    sqlx::query("create function reject_stop_event() returns trigger language plpgsql as $$ begin if new.kind = 'queue_paused' then raise exception 'event rejected'; end if; return new; end $$").execute(&pool).await?;
    sqlx::query("create trigger reject_stop_event before insert on events for each row execute function reject_stop_event()").execute(&pool).await?;
    assert!(
        request_stop_execution(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            false,
            "stop",
            Uuid::now_v7()
        )
        .await
        .is_err()
    );
    assert!(pauses(&pool, &seed).await.is_empty());
    let count: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'stop_execution'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar(
        "select count(*) from idempotency_keys where owner_id = $1 and key = 'stop'",
    )
    .bind(seed.owner_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 0);
    Ok(())
}
