//! Усыпление простаивающих сессий:-blocking условия, пробуждение вводом и skвозной маршрут.

use std::time::Duration;

use canterlot_hub::{
    delivery::{
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        submit::{ContentBlock, SubmitInput, submit_input},
    },
    executions::{self, StartedExecution},
    hibernate::{self, HibernateSettings, StartupGrace},
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

/// Ни одно из blocking условий не должно дать команду остановки.
macro_rules! blocked {
    ($name:ident, $blocker:expr) => {
        #[sqlx::test(migrations = "./migrations")]
        async fn $name(pool: PgPool) -> TestResult {
            let (seed, _run) = setup(&pool).await;
            $blocker(&pool, &seed).await;
            assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
            assert!(stop_command(&pool, seed.conversation_id).await.is_none());
            Ok(())
        }
    };
}

async fn block_delegate(pool: &PgPool, seed: &Seed) {
    let child = Uuid::now_v7();
    sqlx::query(
        "insert into conversations(id,owner_id,machine_id,harness,workdir,parent_conversation_id,spawn_depth)
         values($1,$2,$3,'synthetic','/tmp/child',$4,1)",
    )
    .bind(child)
    .bind(seed.owner_id)
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("insert into queue_state(conversation_id) values($1)")
        .bind(child)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(child)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_alarm(pool: &PgPool, seed: &Seed) {
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,condition,deadline_at,state,call_id)
         values($1,$2,$3,'agent','condition',$4,clock_timestamp()+interval '1 hour','armed',$5)",
    )
    .bind(Uuid::now_v7())
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind(json!({"session_ids":[seed.conversation_id.to_string()]}))
    .bind(Uuid::now_v7())
    .execute(pool)
    .await
    .unwrap();
}

async fn block_subscribed_to(pool: &PgPool, seed: &Seed) {
    let subscriber = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id)
         values($1,$2,$3,$4)",
    )
    .bind(Uuid::now_v7())
    .bind(seed.owner_id)
    .bind(subscriber)
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_subscriber(pool: &PgPool, seed: &Seed) {
    let other = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id)
         values($1,$2,$3,$4)",
    )
    .bind(Uuid::now_v7())
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind(other)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_nudge(pool: &PgPool, seed: &Seed) {
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state)
         values($1,$2,'agent','watching')",
    )
    .bind(seed.conversation_id)
    .bind(seed.owner_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_plan_task(pool: &PgPool, seed: &Seed) {
    sqlx::query(
        "insert into plan_tasks(id,owner_id,orchestrator_conversation_id,number,title,state,executor_conversation_id)
         values($1,$2,$3,1,'task','in_progress',$4)",
    )
    .bind(Uuid::now_v7())
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_queued_input(pool: &PgPool, seed: &Seed) {
    submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "wake".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
}

async fn block_open_request(pool: &PgPool, seed: &Seed) {
    let (execution, generation): (Uuid, i64) =
        sqlx::query_as("select id,generation from executions where conversation_id=$1 order by generation desc limit 1")
            .bind(seed.conversation_id)
            .fetch_one(pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload)
         values($1,$2,$3,$4,'permission','req','open','{}')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.conversation_id)
    .bind(execution)
    .bind(generation)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_secret(pool: &PgPool, seed: &Seed) {
    sqlx::query(
        "insert into secret_requests(id,conversation_id,owner_id,machine_id,name,purpose,state)
         values($1,$2,$3,$4,'key','test','open')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.conversation_id)
    .bind(seed.owner_id)
    .bind(seed.machine_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_hold(pool: &PgPool, seed: &Seed) {
    sqlx::query(
        "update queue_state set hold_reasons='[{\"reason\":\"quota_exhausted\",\"provider\":\"claude\",\"window\":\"five_hour\",\"wake_at\":null}]'::jsonb where conversation_id=$1",
    )
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn block_unresumable(pool: &PgPool, seed: &Seed) {
    sqlx::query("update conversations set harness='unknown_harness' where id=$1")
        .bind(seed.conversation_id)
        .execute(pool)
        .await
        .unwrap();
}

blocked!(live_delegate_blocks, block_delegate);
blocked!(armed_alarm_blocks, block_alarm);
blocked!(subscription_target_blocks, block_subscribed_to);
blocked!(subscription_source_blocks, block_subscriber);
blocked!(auto_nudge_blocks, block_nudge);
blocked!(plan_task_blocks, block_plan_task);
blocked!(queued_input_blocks, block_queued_input);
blocked!(open_owner_request_blocks, block_open_request);
blocked!(live_secret_blocks, block_secret);
blocked!(quota_hold_blocks, block_hold);
blocked!(unresumable_harness_blocks, block_unresumable);

#[sqlx::test(migrations = "./migrations")]
async fn visible_chat_blocks(pool: PgPool) -> TestResult {
    let (seed, _run) = setup(&pool).await;
    let presence = Presence::default();
    presence.report(seed.conversation_id, "phone", true, testkit::clock_now());
    assert_eq!(hibernate::sweep(&pool, &presence).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn disabled_setting_blocks(pool: PgPool) -> TestResult {
    let (seed, _run) = setup(&pool).await;
    let mut conn = pool.acquire().await?;
    hibernate::replace_settings_in_tx(
        &mut conn,
        seed.owner_id,
        &HibernateSettings { idle_minutes: None },
    )
    .await?;
    drop(conn);
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn unproven_idle_does_not_hibernate(pool: PgPool) -> TestResult {
    let (seed, _run) = setup(&pool).await;
    sqlx::query("update conversation_derived set activity_evidence=$2 where conversation_id=$1")
        .bind(seed.conversation_id)
        .bind(json!({"kind":"idle","background_work":null,"process_alive":false}))
        .execute(&pool)
        .await?;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 0);
    assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    Ok(())
}

#[test]
fn startup_grace_is_over_exactly_at_the_window_end() {
    let started = testkit::clock_now();
    let grace = StartupGrace::new(started, Duration::from_secs(600));
    assert!(!grace.is_over(started));
    assert!(!grace.is_over(started + Duration::from_secs(599)));
    assert!(grace.is_over(started + Duration::from_secs(600)));
    assert!(StartupGrace::new(started, Duration::ZERO).is_over(started));
}

/// Первые десять минут после старта хаба обход ничего не усыпляет: сессия, простоявшая
/// больше часа, не должна уснуть сразу под выкатом новой версии. Часы синтетические,
/// моменты сдвигаются арифметикой, без ожиданий.
#[sqlx::test(migrations = "./migrations")]
async fn startup_grace_window_blocks_hibernation(pool: PgPool) -> TestResult {
    let (seed, _run) = setup(&pool).await;
    let started = testkit::clock_now();
    let grace = StartupGrace::new(started, Duration::from_secs(600));
    let presence = Presence::default();
    for elapsed in [0, 1, 599] {
        let now = started + Duration::from_secs(elapsed);
        assert_eq!(hibernate::sweep_at(&pool, &presence, &grace, now).await?, 0);
        assert!(stop_command(&pool, seed.conversation_id).await.is_none());
    }
    let after = started + Duration::from_secs(600);
    assert_eq!(
        hibernate::sweep_at(&pool, &presence, &grace, after).await?,
        1
    );
    assert_eq!(
        stop_command(&pool, seed.conversation_id).await,
        Some(json!({"force": false, "reason": "hibernated"}))
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn proven_idle_for_an_hour_hibernates(pool: PgPool) -> TestResult {
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
    })
}

#[sqlx::test(migrations = "./migrations")]
async fn input_during_stop_is_delivered_once_after_resume(pool: PgPool) -> TestResult {
    let (seed, run) = setup(&pool).await;
    assert_eq!(hibernate::sweep(&pool, &Presence::default()).await?, 1);
    let submitted = submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "wake".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await?;
    // Пока остановка в полёте, новое исполнение не стартовало.
    let live: i64 = sqlx::query_scalar(
        "select count(*) from executions where conversation_id=$1 and state not in ('stopped','failed')",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(live, 1);

    // Воркер сообщает о конце остановки; хаб должен разбудить сессию и сохранить причину.
    let journal = Uuid::now_v7();
    let ingest = Ingest::new(pool.clone());
    ingest
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
    let outcome: String = sqlx::query_scalar("select outcome from executions where id=$1")
        .bind(run.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(outcome, "hibernated");
    let live: Vec<(Uuid, i64, String)> = sqlx::query_as(
        "select id,generation,state from executions where conversation_id=$1 and state not in ('stopped','failed')",
    )
    .bind(seed.conversation_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(live.len(), 1, "exactly one resumed execution");
    assert_eq!(live[0].1, run.generation + 1);
    let native: String =
        sqlx::query_scalar("select native_conversation_id from executions where id=$1")
            .bind(live[0].0)
            .fetch_one(&pool)
            .await?;
    assert_eq!(native, format!("native-{}", run.execution_id));

    // Ввод доставляется тем же сообщением один раз, новым исполнением.
    let resumed = StartedExecution {
        execution_id: live[0].0,
        generation: live[0].1,
        command_id: Uuid::now_v7(),
    };
    executions::set_state(
        &pool,
        resumed.execution_id,
        ExecutionState::Starting,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        resumed.execution_id,
        ExecutionState::Running,
        None,
        Uuid::now_v7(),
    )
    .await?;
    let outcome = dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await?;
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { input_id, .. } if input_id == submitted.input_id)
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn owner_message_through_http_wakes_and_replies(pool: PgPool) -> TestResult {
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
