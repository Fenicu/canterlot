//! Усыпление простаивающих сессий:-blocking условия, пробуждение вводом и skвозной маршрут.

use std::time::Duration;

use canterlot_hub::{
    delivery::{
        control::request_stop_execution,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        receipts::{InputReceipt, apply_input_receipt},
        submit::{ContentBlock, SubmitInput, submit_input},
    },
    executions::{self, StartedExecution},
    hibernate::{self},
    http,
    ingest::{Ingest, JournalKind},
    model::ExecutionState,
    presence::Presence,
    testkit::{self, Seed, TestClient},
};
use canterlot_proto::worker::v1::{
    ExecutionState as ExecutionFact, JournalRecord, JournalRecords, RecordBody, record_body::Body,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn start_running(pool: &PgPool, seed: &Seed) -> StartedExecution {
    let run = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for next in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, run.execution_id, next, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query("update executions set native_conversation_id=$2 where id=$1")
        .bind(run.execution_id)
        .bind(format!("native-{}", run.execution_id))
        .execute(pool)
        .await
        .unwrap();
    testkit::seed_recorded_turn(pool, seed.conversation_id).await;
    sqlx::query(
        "update queue_state set next_input_seq =
           (select coalesce(max(input_seq),0)+1 from inputs where conversation_id=$1)
         where conversation_id=$1",
    )
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
    run
}

/// Доказанный простой длительностью больше часа (или заданной давности).
async fn idle_since(pool: &PgPool, conversation: Uuid, quiet: Duration) {
    let since = Utc::now() - TimeDelta::from_std(quiet).unwrap();
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence,activity_since,queue_queued)
         values($1,'idle',$2,$3,0)
         on conflict(conversation_id) do update set activity_state='idle',
           activity_evidence=excluded.activity_evidence,activity_since=excluded.activity_since,
           queue_queued=0,hold_reasons='[]'::jsonb",
    )
    .bind(conversation)
    .bind(json!({"kind":"idle","last_turn_at":null,"background_work":null,"process_alive":true}))
    .bind(since)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("update queue_state set hold_reasons='[]'::jsonb where conversation_id=$1")
        .bind(conversation)
        .execute(pool)
        .await
        .unwrap();
}

async fn setup(pool: &PgPool) -> (Seed, StartedExecution) {
    let seed = testkit::seed(pool).await;
    let run = start_running(pool, &seed).await;
    idle_since(pool, seed.conversation_id, Duration::from_secs(61 * 60)).await;
    (seed, run)
}

async fn stop_command(pool: &PgPool, conversation: Uuid) -> Option<Value> {
    sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='stop_execution'
           and payload->>'reason'='hibernated' order by created_at desc limit 1",
    )
    .bind(conversation)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn proven_idle_for_an_hour_hibernates() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 1);
    assert_eq!(
        stop_command(&pool, seed.conversation_id).await,
        Some(json!({"force": false, "reason": "hibernated"}))
    );
    let requested: bool =
        sqlx::query_scalar("select hibernate_requested from executions where id=$1")
            .bind(run.execution_id)
            .fetch_one(&pool)
            .await?;
    assert!(requested);
    let pauses: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(pauses, json!([{"reason":"hibernated"}]));
    // Повторный обход не создаёт вторую команду.
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    Ok(())
}

fn batch(journal: Uuid, bodies: Vec<Body>) -> JournalRecords {
    JournalRecords {
        journal_id: journal.to_string(),
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

fn execution_fact(run: &StartedExecution, state: &str, outcome: &str) -> Body {
    Body::ExecutionState(ExecutionFact {
        native_conversation_id: String::new(),
        execution_id: run.execution_id.to_string(),
        generation: run.generation as u64,
        state: state.into(),
        outcome: outcome.into(),
        fresh_fork: false,
        canonical_workdir: String::new(),
        canonical_source: String::new(),
        interrupted_git: None,
    })
}

#[tokio::test]
async fn owner_message_through_http_wakes_and_replies() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (deps, dir) = testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner_id = deps.installation.owner_id;
    let machine_id = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine_id)
        .bind(owner_id)
        .execute(&pool)
        .await?;
    use sha2::{Digest, Sha256};
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(machine_id)
        .bind(Sha256::digest(machine_id.as_bytes()).as_slice())
        .execute(&pool)
        .await?;
    testkit::seed_catalog(&pool, machine_id).await;
    let conversation_id = testkit::seed_conversation(&pool, owner_id, machine_id).await;
    let seed = Seed {
        owner_id,
        machine_id,
        conversation_id,
    };
    let run = start_running(&pool, &seed).await;
    idle_since(&pool, conversation_id, Duration::from_secs(61 * 60)).await;

    let state = http::AppState::new(deps);
    let client = TestClient::in_process(http::router(state)).as_owner();
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 1);

    // Владелец пишет в усыплённую сессию через HTTP.
    let response = client
        .send(
            axum::http::Method::POST,
            &format!("/api/v1/conversations/{conversation_id}/inputs"),
            Some(Uuid::now_v7().to_string()).as_deref(),
            Some(json!({"content":[{"type":"text","text":"wake"}]})),
        )
        .await;
    let submitted: Value = response.json();
    assert_eq!(response.status.as_u16(), 201, "{submitted:?}");
    let input_id: Uuid = serde_json::from_value(submitted["input_id"].clone())?;

    // Синтетический воркер подтверждает остановку; сессия возобновляется по нативному id.
    let journal = Uuid::now_v7();
    Ingest::new(pool.clone())
        .apply_batch(
            machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                journal,
                vec![
                    execution_fact(&run, "stopping", ""),
                    execution_fact(&run, "stopped", "user_stop"),
                ],
            ),
        )
        .await?;
    let native: String = sqlx::query_scalar(
        "select native_conversation_id from executions where conversation_id=$1
         order by generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(native, format!("native-{}", run.execution_id));
    let delivered: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(delivered, 2, "the original start and the wake-up start");
    let resumed_id: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_one(&pool)
    .await?;
    for next in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, resumed_id, next, None, Uuid::now_v7()).await?;
    }
    let dispatched = dispatch_next(
        &pool,
        conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await?;
    assert!(
        matches!(dispatched, DispatchOutcome::Dispatched { input_id: id, .. } if id == input_id),
        "the owner's message reaches the resumed execution"
    );
    drop(dir);
    Ok(())
}

/// Ввод после уже записанной остановки перезагрузкой будит сессию тем же путём, что
/// и усыплённую: новое исполнение с нативным id, паузы `process_exited` и `owner_lost` сняты.
#[tokio::test]
async fn owner_message_after_a_reboot_stop_wakes_the_session() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let run = start_running(&pool, &seed).await;
    stop_by_reboot(&pool, &seed, &run).await;
    assert!(
        queued_pauses(&pool, seed.conversation_id)
            .await?
            .as_array()
            .is_some_and(|pauses| !pauses.is_empty()),
        "a reboot stops the queue"
    );
    let submitted = owner_input(&pool, seed.conversation_id, "проснись").await?;
    let resumed = resumed_execution(&pool, &seed, &run).await;
    assert_eq!(queued_pauses(&pool, seed.conversation_id).await?, json!([]));
    deliver_once(&pool, &seed, resumed, submitted.input_id).await
}

async fn owner_input(
    pool: &PgPool,
    conversation: Uuid,
    text: &str,
) -> Result<canterlot_hub::delivery::SubmittedInput, Box<dyn std::error::Error + Send + Sync>> {
    Ok(submit_input(
        pool,
        SubmitInput {
            owner_id: sqlx::query_scalar("select owner_id from conversations where id=$1")
                .bind(conversation)
                .fetch_one(pool)
                .await?,
            conversation_id: conversation,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text { text: text.into() }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await?)
}

/// Перезагрузка: воркер сообщает `stopped(machine_rebooted)` при следующем старте.
async fn stop_by_reboot(pool: &PgPool, seed: &Seed, run: &StartedExecution) {
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                Uuid::now_v7(),
                vec![execution_fact(run, "stopped", "machine_rebooted")],
            ),
        )
        .await
        .expect("machine reboot fact");
}

async fn queued_pauses(pool: &PgPool, conversation: Uuid) -> Result<Value, sqlx::Error> {
    sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
        .bind(conversation)
        .fetch_one(pool)
        .await
}

/// Новое исполнение продолжило ту же нативную беседу; возвращает его id и поколение.
async fn resumed_execution(pool: &PgPool, seed: &Seed, run: &StartedExecution) -> (Uuid, i64) {
    let live: Vec<(Uuid, i64, String)> = sqlx::query_as(
        "select id,generation,state from executions where conversation_id=$1 and state not in ('stopped','failed')",
    )
    .bind(seed.conversation_id)
    .fetch_all(pool)
    .await
    .expect("executions");
    assert_eq!(live.len(), 1, "exactly one resumed execution");
    assert_eq!(live[0].1, run.generation + 1);
    let native: String =
        sqlx::query_scalar("select native_conversation_id from executions where id=$1")
            .bind(live[0].0)
            .fetch_one(pool)
            .await
            .expect("native id");
    assert_eq!(native, format!("native-{}", run.execution_id));
    (live[0].0, live[0].1)
}

/// Сообщение уходит новому исполнению ровно один раз.
async fn deliver_once(
    pool: &PgPool,
    seed: &Seed,
    resumed: (Uuid, i64),
    input_id: Uuid,
) -> TestResult {
    for next in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, resumed.0, next, None, Uuid::now_v7()).await?;
    }
    let gate = || GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    let delivered = dispatch_next(pool, seed.conversation_id, gate(), Uuid::now_v7()).await?;
    assert!(
        matches!(delivered, DispatchOutcome::Dispatched { input_id: id, .. } if id == input_id),
        "the queued message reaches the resumed execution"
    );
    let again = dispatch_next(pool, seed.conversation_id, gate(), Uuid::now_v7()).await?;
    assert!(
        !matches!(again, DispatchOutcome::Dispatched { input_id: id, .. } if id == input_id),
        "the message is delivered once, not again"
    );
    let deliveries: i64 = sqlx::query_scalar(
        "select count(*) from commands where execution_id=$1 and kind='deliver_input'",
    )
    .bind(resumed.0)
    .fetch_one(pool)
    .await?;
    assert_eq!(deliveries, 1, "exactly one delivery of the queued message");
    Ok(())
}

fn gate() -> GateInputs {
    GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    }
}

/// Выдаёт голову очереди и проводит ввод по квитанциям воркера.
async fn dispatch_with_receipts(
    pool: &PgPool,
    seed: &Seed,
    receipts: Vec<InputReceipt>,
) -> Result<Uuid, Box<dyn std::error::Error + Send + Sync>> {
    let (input, command, attempt) =
        match dispatch_next(pool, seed.conversation_id, gate(), Uuid::now_v7()).await? {
            DispatchOutcome::Dispatched {
                input_id,
                command_id,
                attempt,
                ..
            } => (input_id, command_id, attempt),
            other => panic!("not dispatched: {other:?}"),
        };
    for receipt in receipts {
        apply_input_receipt(pool, command, attempt, receipt, Uuid::now_v7()).await?;
    }
    Ok(input)
}

async fn slot_and_state(pool: &PgPool, seed: &Seed, input: Uuid) -> (String, String) {
    let slot: String =
        sqlx::query_scalar("select run_slot from queue_state where conversation_id=$1")
            .bind(seed.conversation_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(input)
        .fetch_one(pool)
        .await
        .unwrap();
    (slot, state)
}

/// Ввод уже передаётся агенту, а проектор активности ещё не пересчитал сессию: проекция
/// `idle` с часовой давностью. Обход не должен останавливать процесс посреди хода.
#[tokio::test]
async fn transmitting_input_blocks_despite_a_stale_idle_projection() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, _run) = setup(&pool).await;
    owner_input(&pool, seed.conversation_id, "работай").await?;
    let input = dispatch_with_receipts(
        &pool,
        &seed,
        vec![InputReceipt::Stored, InputReceipt::TransmissionStarted],
    )
    .await?;
    assert_eq!(
        slot_and_state(&pool, &seed, input).await,
        ("dispatching".to_owned(), "transmitting".to_owned())
    );
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    Ok(())
}

/// Ввод принят, слот `running`: ход идёт, хотя проекция ещё говорит `idle`.
#[tokio::test]
async fn running_turn_blocks_despite_a_stale_idle_projection() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    owner_input(&pool, seed.conversation_id, "работай").await?;
    let input = dispatch_with_receipts(
        &pool,
        &seed,
        vec![
            InputReceipt::Stored,
            InputReceipt::TransmissionStarted,
            InputReceipt::Accepted,
        ],
    )
    .await?;
    assert_eq!(
        slot_and_state(&pool, &seed, input).await,
        ("running".to_owned(), "accepted".to_owned())
    );
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    let requested: bool =
        sqlx::query_scalar("select hibernate_requested from executions where id=$1")
            .bind(run.execution_id)
            .fetch_one(&pool)
            .await?;
    assert!(!requested);
    Ok(())
}

/// Исход передачи не определён: слот освобождён паузой `unconfirmed_delivery`, а CLI мог
/// ввод получить и работать. Ни обход, ни перепроверка под замком очереди такую сессию
/// не усыпляют, пока исход не прояснился.
#[tokio::test]
async fn undetermined_acceptance_blocks_hibernation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    owner_input(&pool, seed.conversation_id, "работай").await?;
    let input = dispatch_with_receipts(
        &pool,
        &seed,
        vec![
            InputReceipt::Stored,
            InputReceipt::TransmissionStarted,
            InputReceipt::AcceptanceUndetermined,
        ],
    )
    .await?;
    assert_eq!(
        slot_and_state(&pool, &seed, input).await,
        ("free".to_owned(), "acceptance_unconfirmed".to_owned())
    );
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    let mut conn = pool.acquire().await?;
    assert!(
        !hibernate::hibernate_in_tx(&mut conn, seed.conversation_id).await?,
        "the recheck under the queue lock refuses too"
    );
    drop(conn);
    // Пауза держит и без строки ввода в этом состоянии.
    sqlx::query("update inputs set state='rejected' where id=$1")
        .bind(input)
        .execute(&pool)
        .await?;
    let mut conn = pool.acquire().await?;
    assert!(!hibernate::hibernate_in_tx(&mut conn, seed.conversation_id).await?);
    drop(conn);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    let requested: bool =
        sqlx::query_scalar("select hibernate_requested from executions where id=$1")
            .bind(run.execution_id)
            .fetch_one(&pool)
            .await?;
    assert!(!requested);
    Ok(())
}

/// Ход без ввода (агент проснулся сам): слот свободен, но фаза исполнения рабочая.
#[tokio::test]
async fn self_started_turn_blocks_despite_a_stale_idle_projection() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    sqlx::query("update executions set phase='generating', phase_at=clock_timestamp() where id=$1")
        .bind(run.execution_id)
        .execute(&pool)
        .await?;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    Ok(())
}

async fn set_worktree(pool: &PgPool, seed: &Seed, state: &str) {
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(seed.conversation_id)
        .bind(
            json!({"state": state, "source_workdir": "/src", "branch": "b", "base": "main",
                     "path": "/wt"}),
        )
        .execute(pool)
        .await
        .unwrap();
}

async fn live_executions(pool: &PgPool, conversation: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from executions where conversation_id=$1
         and state not in ('stopped','failed')",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
}

async fn input_state(pool: &PgPool, input: Uuid) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("select state from inputs where id=$1")
        .bind(input)
        .fetch_one(pool)
        .await
}

/// Усыплённую сессию приняли, рабочая копия убрана. Следующее сообщение разбудить её не
/// может, но и теряться не должно: ввод встаёт в очередь, как у любой остановленной сессии.
#[tokio::test]
async fn message_to_a_hibernated_session_without_worktree_stays_queued() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 1);
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                Uuid::now_v7(),
                vec![
                    execution_fact(&run, "stopping", ""),
                    execution_fact(&run, "stopped", "user_stop"),
                ],
            ),
        )
        .await?;
    set_worktree(&pool, &seed, "removed").await;
    let submitted = owner_input(&pool, seed.conversation_id, "ещё одно").await?;
    assert_eq!(input_state(&pool, submitted.input_id).await?, "queued");
    assert_eq!(live_executions(&pool, seed.conversation_id).await?, 0);
    assert_eq!(
        queued_pauses(&pool, seed.conversation_id).await?,
        json!([{"reason":"hibernated"}]),
        "nothing woke up, so the pause stays"
    );
    Ok(())
}

/// Остановка усыпления в полёте, в сессию пришёл ввод, и её приняли: рабочая копия уже
/// убирается. Запись `stopped` обязана примениться — иначе встаёт журнал всей машины.
#[tokio::test]
async fn stop_fact_applies_when_the_wake_cannot_start() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let (seed, run) = setup(&pool).await;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 1);
    let submitted = owner_input(&pool, seed.conversation_id, "wake").await?;
    set_worktree(&pool, &seed, "removing").await;
    let journal = Uuid::now_v7();
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                journal,
                vec![
                    execution_fact(&run, "stopping", ""),
                    execution_fact(&run, "stopped", "user_stop"),
                ],
            ),
        )
        .await?;
    let (state, outcome, committed): (String, Option<String>, i64) = sqlx::query_as(
        "select e.state, e.outcome, j.committed_seq from executions e, journal_cursors j
         where e.id=$1 and j.journal_id=$2",
    )
    .bind(run.execution_id)
    .bind(journal)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        (state.as_str(), outcome.as_deref(), committed),
        ("stopped", Some("hibernated"), 2)
    );
    assert_eq!(live_executions(&pool, seed.conversation_id).await?, 0);
    assert_eq!(input_state(&pool, submitted.input_id).await?, "queued");
    let stops: i64 = sqlx::query_scalar("select count(*) from journal_stops")
        .fetch_one(&pool)
        .await?;
    assert_eq!(stops, 0, "the journal went on without a stop");
    Ok(())
}

/// Владелец остановил сессию, а машина перезагрузилась раньше, чем остановка применилась.
/// Ввод в очереди процесс не поднимает: остановка владельца ждёт владельца.
#[tokio::test]
async fn reboot_does_not_restart_a_session_the_owner_stopped() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let run = start_running(&pool, &seed).await;
    request_stop_execution(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        false,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await?;
    let submitted = owner_input(&pool, seed.conversation_id, "потом").await?;
    stop_by_reboot(&pool, &seed, &run).await;
    assert_eq!(live_executions(&pool, seed.conversation_id).await?, 0);
    assert_eq!(input_state(&pool, submitted.input_id).await?, "queued");
    assert!(
        queued_pauses(&pool, seed.conversation_id)
            .await?
            .as_array()
            .is_some_and(|pauses| pauses.iter().any(|pause| pause["reason"] == "user_stop")),
        "the owner's stop is still in force"
    );
    // И следующее сообщение не будит: решение владельца не отменяет ни перезагрузка, ни ввод.
    owner_input(&pool, seed.conversation_id, "ещё").await?;
    assert_eq!(live_executions(&pool, seed.conversation_id).await?, 0);
    Ok(())
}
