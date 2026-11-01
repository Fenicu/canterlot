use std::time::Duration;

use canterlot_hub::{
    activity::{
        self, Activity, ActivityState as S, Evidence as E, QueueFacts, Refresh, Thresholds,
    },
    config::{ActivityConfig, MachinesConfig, SchedulerConfig},
    deletion,
    delivery::{
        ContentBlock, SubmitInput,
        dispatch::{self, BlockReason, DispatchOutcome, GateInputs},
        submit_input,
    },
    dispatcher::Dispatcher,
    executions,
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
    requests::{self, RequestKind},
    scheduler::Scheduler,
    session_tools::{
        settle::{self, SettleConfig, SettleFacts, Settled},
        subs,
    },
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
    }
}

fn config() -> SettleConfig {
    SettleConfig {
        idle_quiet: Duration::from_secs(5),
    }
}

fn queue(queued: u32, pause_reasons: Vec<PauseReason>) -> QueueFacts {
    QueueFacts {
        slot: RunSlot::Free,
        queued,
        pause_reasons,
        hold_reasons: vec![],
    }
}

fn idle_activity(background_work: Option<u32>) -> Activity {
    Activity {
        state: S::Idle,
        evidence: E::Idle {
            last_turn_at: None,
            background_work,
        },
    }
}

fn old_facts(now: DateTime<Utc>) -> SettleFacts {
    SettleFacts {
        now,
        since: now - TimeDelta::hours(1),
        last_turn_at: None,
        last_event_at: Some(now - TimeDelta::hours(1)),
    }
}

#[test]
fn settled_ignores_idle_with_queued_input() {
    let now = Utc::now();
    assert_eq!(
        settle::settled(
            &idle_activity(None),
            &queue(1, vec![]),
            &old_facts(now),
            &config()
        ),
        None
    );
}

#[test]
fn settled_reports_paused_queue_as_waiting() {
    let now = Utc::now();
    assert_eq!(
        settle::settled(
            &idle_activity(None),
            &queue(1, vec![PauseReason::UserStop]),
            &old_facts(now),
            &config()
        ),
        Some(Settled::WaitingForUser)
    );
}

#[test]
fn settled_holds_idle_with_background_work() {
    let now = Utc::now();
    let mut facts = old_facts(now);
    facts.last_event_at = Some(now - TimeDelta::minutes(29));
    assert_eq!(
        settle::settled(
            &idle_activity(Some(1)),
            &queue(0, vec![]),
            &facts,
            &config()
        ),
        None
    );
    facts.last_event_at = Some(now - TimeDelta::minutes(31));
    assert_eq!(
        settle::settled(
            &idle_activity(Some(1)),
            &queue(0, vec![]),
            &facts,
            &config()
        ),
        Some(Settled::Idle),
        "a silent background task must not hold the session forever"
    );
}

#[test]
fn settled_holds_idle_inside_quiet_window() {
    let now = Utc::now();
    let mut facts = old_facts(now);
    facts.since = now - TimeDelta::seconds(2);
    let idle = idle_activity(None);
    let empty = queue(0, vec![]);
    assert_eq!(settle::settled(&idle, &empty, &facts, &config()), None);
    facts.since = now - TimeDelta::hours(1);
    facts.last_turn_at = Some(now - TimeDelta::seconds(2));
    assert_eq!(
        settle::settled(&idle, &empty, &facts, &config()),
        None,
        "idle->idle: a fresh turn restarts the window"
    );
    facts.last_turn_at = Some(now - TimeDelta::seconds(6));
    assert_eq!(
        settle::settled(&idle, &empty, &facts, &config()),
        Some(Settled::Idle)
    );
}

#[test]
fn settled_restarts_the_quiet_window_on_any_event() {
    let now = Utc::now();
    let mut facts = old_facts(now);
    facts.last_event_at = Some(now - TimeDelta::seconds(2));
    assert_eq!(
        settle::settled(&idle_activity(None), &queue(0, vec![]), &facts, &config()),
        None,
        "background work has just ended: a wake turn may still start"
    );
    facts.last_event_at = Some(now - TimeDelta::seconds(6));
    assert_eq!(
        settle::settled(&idle_activity(None), &queue(0, vec![]), &facts, &config()),
        Some(Settled::Idle)
    );
}

#[test]
fn settled_maps_the_other_states_directly() {
    let now = Utc::now();
    let working = Activity {
        state: S::Working,
        evidence: E::Working {
            phase: "generating".into(),
            tool_name: None,
            summary: None,
            phase_since: None,
        },
    };
    let empty = queue(0, vec![]);
    assert_eq!(
        settle::settled(&working, &empty, &old_facts(now), &config()),
        None
    );
    for (state, expected) in [
        (S::Stalled, Settled::Stalled),
        (S::WaitingForUser, Settled::WaitingForUser),
        (S::QuotaPaused, Settled::QuotaPaused),
        (S::Failed, Settled::Failed),
        (S::Stopped, Settled::Stopped),
    ] {
        let activity = Activity {
            state,
            evidence: E::Failed {
                reason: "any".into(),
            },
        };
        assert_eq!(
            settle::settled(&activity, &queue(3, vec![]), &old_facts(now), &config()),
            Some(expected),
            "{state:?}"
        );
    }
}

#[test]
fn fingerprint_is_state_since_and_last_turn() {
    let since = DateTime::parse_from_rfc3339("2026-11-01T10:00:00.123456789Z")
        .unwrap()
        .to_utc();
    assert_eq!(
        settle::fingerprint(Settled::Idle, since, None),
        "idle|2026-11-01T10:00:00.123456Z|"
    );
    assert_eq!(
        settle::fingerprint(Settled::Stalled, since, Some(since)),
        "stalled|2026-11-01T10:00:00.123456Z|2026-11-01T10:00:00.123456Z"
    );
}

struct Pair {
    owner: Uuid,
    machine: Uuid,
    subscriber: Uuid,
    target: Uuid,
}

async fn pair(pool: &PgPool) -> Pair {
    let s = testkit::seed(pool).await;
    let target = testkit::seed_conversation(pool, s.owner_id, s.machine_id).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(s.machine_id)
        .execute(pool)
        .await
        .unwrap();
    Pair {
        owner: s.owner_id,
        machine: s.machine_id,
        subscriber: s.conversation_id,
        target,
    }
}

async fn start(pool: &PgPool, owner: Uuid, conversation: Uuid) -> executions::StartedExecution {
    let execution = executions::start(pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

async fn subscribe(pool: &PgPool, p: &Pair) {
    let mut tx = pool.begin().await.unwrap();
    subs::subscribe_in_tx(&mut tx, p.owner, p.subscriber, p.target, 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn refresh_at(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Refresh {
    activity::refresh(pool, id, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap()
}

async fn finish_turn(
    pool: &PgPool,
    p: &Pair,
    execution: &executions::StartedExecution,
    at: DateTime<Utc>,
) {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(p.machine)
    .bind(p.target)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at)
         values($1,$2,$3,$4,$4)",
    )
    .bind(command)
    .bind(execution.execution_id)
    .bind(p.target)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

async fn jobs(pool: &PgPool, subscriber: Uuid) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from scheduled_jobs where kind='session_task' and conversation_id=$1
         order by id",
    )
    .bind(subscriber)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn subscription(pool: &PgPool, subscriber: Uuid, target: Uuid) -> Value {
    sqlx::query_scalar(
        "select to_jsonb(s) from session_subscriptions s where subscriber_id=$1 and target_id=$2",
    )
    .bind(subscriber)
    .bind(target)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn scheduler(pool: &PgPool) -> Scheduler {
    Scheduler::new(pool.clone(), Dispatcher::channel().0, Default::default())
}

async fn delivered(pool: &PgPool, conversation: Uuid) -> Vec<(String, Value, String)> {
    sqlx::query_as::<_, (Value, Option<Value>, String)>(
        "select content, origin, state from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(content, origin, state)| {
        (
            content[0]["text"].as_str().unwrap_or_default().to_owned(),
            origin.unwrap_or(Value::Null),
            state,
        )
    })
    .collect()
}

/// Разговор давно простаивает: окно тишины уже прошло.
async fn long_idle(pool: &PgPool, id: Uuid, now: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(id)
        .bind(now - TimeDelta::hours(1))
        .execute(pool)
        .await
        .unwrap();
    refresh_at(pool, id, now - TimeDelta::hours(1)).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn turn_shorter_than_debounce_still_fires(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;

    // Ход начался и кончился между двумя обновлениями: состояние осталось idle.
    finish_turn(&pool, &p, &execution, now - TimeDelta::seconds(1)).await;
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty(), "quiet window");

    refresh_at(&pool, p.target, now + TimeDelta::seconds(5)).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["action"], "event");
    assert_eq!(fired[0]["state"], "idle");
    assert_eq!(fired[0]["target_id"], json!(p.target));
    assert_eq!(fired[0]["chain_depth"], 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn refresh_results_do_not_depend_on_subscriptions(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    assert_eq!(refresh_at(&pool, p.target, now).await, Refresh::Unchanged);
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        1,
        "evaluated on the unchanged branch too"
    );
    let events: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='derived_changed' and conversation_id=$1",
    )
    .bind(p.target)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 2, "one per scope: the job adds no event");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscription_survives_subscriber_restart(pool: PgPool) {
    let p = pair(&pool).await;
    let subscriber = start(&pool, p.owner, p.subscriber).await;
    let target = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;

    executions::set_state(
        &pool,
        subscriber.execution_id,
        ExecutionState::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    finish_turn(&pool, &p, &target, now - TimeDelta::seconds(10)).await;
    refresh_at(&pool, p.target, now).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    // Новый планировщик: хаб перезапущен, задание лежит в базе.
    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);
    let inputs = delivered(&pool, p.subscriber).await;
    assert_eq!(inputs.len(), 1, "stopped subscriber still gets the event");
    assert!(inputs[0].0.starts_with("[canterlot] Session"));
    assert!(inputs[0].0.contains("is now idle."));
    assert_eq!(inputs[0].2, "queued");
    assert_eq!(inputs[0].1["kind"], "event");
    assert_eq!(inputs[0].1["from"], json!(p.target));
    assert_eq!(inputs[0].1["chain_depth"], 0);

    // Владелец продолжил сессию: новое исполнение забирает ввод, его гибель ввод не теряет.
    let next = start(&pool, p.owner, p.subscriber).await;
    sqlx::query(
        "update queue_state set hold_reasons='[]',pause_reasons='[]' where conversation_id=$1",
    )
    .bind(p.subscriber)
    .execute(&pool)
    .await
    .unwrap();
    let gate = GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    let outcome = dispatch::dispatch_next(&pool, p.subscriber, gate, Uuid::now_v7())
        .await
        .unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { .. }),
        "{outcome:?}"
    );
    executions::set_state(
        &pool,
        next.execution_id,
        ExecutionState::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(delivered(&pool, p.subscriber).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stalled_fires_once_per_episode(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let quiet_at = |at: DateTime<Utc>| {
        sqlx::query(
            "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
        )
        .bind(execution.execution_id)
        .bind(at)
    };
    quiet_at(now - TimeDelta::minutes(10))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    subscribe(&pool, &p).await;

    // Первый проход пишет since из часов, следующий читает его из базы: отпечаток один.
    refresh_at(&pool, p.target, now).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(15)).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(30)).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["state"], "stalled");
    let row = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(row["closed_at"], Value::Null, "stalled keeps it open");

    let back = now + TimeDelta::seconds(40);
    quiet_at(back).execute(&pool).await.unwrap();
    refresh_at(&pool, p.target, back).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    let again = back + TimeDelta::minutes(6);
    refresh_at(&pool, p.target, again).await;
    refresh_at(&pool, p.target, again + TimeDelta::seconds(15)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 2, "a new episode");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_closes_and_send_rearms(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    finish_turn(&pool, &p, &execution, now - TimeDelta::seconds(10)).await;
    refresh_at(&pool, p.target, now).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(15)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);
    let closed = subscription(&pool, p.subscriber, p.target).await;
    assert_ne!(closed["closed_at"], Value::Null);
    assert_eq!(closed["closed_reason"], "idle");

    // session_send: ввод и подписка в одной транзакции; пока ввод в очереди, сессия не осела.
    submit_input(
        &pool,
        SubmitInput {
            owner_id: p.owner,
            conversation_id: p.target,
            idempotency_key: "agent-call".into(),
            content: vec![ContentBlock::Text { text: "go".into() }],
            received_at: now,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    subscribe(&pool, &p).await;
    let armed = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(armed["closed_at"], Value::Null);
    assert_eq!(armed["closed_reason"], Value::Null);
    assert_eq!(armed["last_fingerprint"], Value::Null);
    assert_ne!(armed["armed_at"], closed["armed_at"]);
    refresh_at(&pool, p.target, now + TimeDelta::seconds(30)).await;
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        1,
        "input still queued"
    );

    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(p.target)
        .execute(&pool)
        .await
        .unwrap();
    finish_turn(&pool, &p, &execution, now + TimeDelta::seconds(35)).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(45)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn explicit_subscribe_skips_the_state_already_settled(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;

    let mut tx = pool.begin().await.unwrap();
    let current =
        subs::subscribe_explicit_in_tx(&mut tx, p.owner, p.subscriber, p.target, 0, now, &config())
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(current, Some(Settled::Idle));
    assert_ne!(
        subscription(&pool, p.subscriber, p.target).await["last_fingerprint"],
        Value::Null
    );
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());

    finish_turn(&pool, &p, &execution, now + TimeDelta::seconds(1)).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(10)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stopped_target_fires(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    subscribe(&pool, &p).await;
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    refresh_at(&pool, p.target, Utc::now()).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1, "no quiet window for a stop");
    assert_eq!(fired[0]["state"], "stopped");
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "stopped"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stopped_target_with_a_queued_input_keeps_the_subscription(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    subscribe(&pool, &p).await;
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    submit_input(
        &pool,
        SubmitInput {
            owner_id: p.owner,
            conversation_id: p.target,
            idempotency_key: "waits-for-resume".into(),
            content: vec![ContentBlock::Text {
                text: "after resume".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    refresh_at(&pool, p.target, Utc::now()).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1, "the sender still hears about the stop");
    assert_eq!(fired[0]["state"], "stopped");
    let stored = subscription(&pool, p.subscriber, p.target).await;
    assert!(
        stored["closed_at"].is_null(),
        "the queued input runs after a resume, and its result has to reach the sender"
    );
    refresh_at(&pool, p.target, Utc::now()).await;
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        1,
        "one report per stop"
    );
}

async fn delete(pool: &PgPool, p: &Pair, id: Uuid, key: &str) -> Result<(), deletion::DeleteError> {
    let directory = tempfile::tempdir().unwrap();
    let (_, blobs) = testkit::test_blob_store(directory.path());
    deletion::delete(pool, blobs.as_ref(), p.owner, id, key, Uuid::now_v7()).await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleted_target_fires_and_delete_is_not_blocked(pool: PgPool) {
    let p = pair(&pool).await;
    let closed = testkit::seed_conversation(&pool, p.owner, p.machine).await;
    sqlx::query("update conversations set title='private title' where id=$1")
        .bind(p.target)
        .execute(&pool)
        .await
        .unwrap();
    subscribe(&pool, &p).await;
    let mut tx = pool.begin().await.unwrap();
    subs::subscribe_in_tx(&mut tx, p.owner, closed, p.target, 0)
        .await
        .unwrap();
    subs::subscribe_in_tx(&mut tx, p.owner, p.target, p.subscriber, 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("update session_subscriptions set closed_at=now() where subscriber_id=$1")
        .bind(closed)
        .execute(&pool)
        .await
        .unwrap();

    delete(&pool, &p, p.target, "delete-target").await.unwrap();

    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["state"], "deleted");
    assert_eq!(fired[0]["target_id"], json!(p.target));
    assert!(jobs(&pool, closed).await.is_empty(), "closed: already told");
    let left: i64 = sqlx::query_scalar("select count(*) from session_subscriptions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);

    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);
    let inputs = delivered(&pool, p.subscriber).await;
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        inputs[0].0,
        format!("[canterlot] Session {} was deleted.", p.target)
    );
    assert!(!inputs[0].1.to_string().contains("private title"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_pending_when_the_target_is_deleted_says_so(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    delete(&pool, &p, p.target, "delete-target").await.unwrap();

    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);
    let inputs = delivered(&pool, p.subscriber).await;
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].0.ends_with("was deleted."), "{}", inputs[0].0);
}

async fn fill_queue(pool: &PgPool, p: &Pair) {
    for n in 0..100 {
        submit_input(
            pool,
            SubmitInput {
                owner_id: p.owner,
                conversation_id: p.subscriber,
                idempotency_key: format!("fill-{n}"),
                content: vec![ContentBlock::Text { text: "x".into() }],
                received_at: Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap();
    }
}

async fn job_row(pool: &PgPool) -> (String, i32, i32, DateTime<Utc>, Option<Value>) {
    sqlx::query_as(
        "select state,attempts,failures,run_at,result from scheduled_jobs
         where kind='session_task'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_full_retries(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    fill_queue(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    let sch = Scheduler::new(
        pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig {
            retry_queue_full: Duration::from_secs(10),
            ..Default::default()
        },
    );
    let due = now + TimeDelta::seconds(1);
    let report = sch.tick(due).await.unwrap();
    assert_eq!(report.retried, 1);
    let (state, attempts, failures, run_at, result) = job_row(&pool).await;
    assert_eq!((state.as_str(), attempts, failures), ("pending", 1, 0));
    assert_eq!(
        run_at.timestamp_millis(),
        (due + TimeDelta::seconds(10)).timestamp_millis()
    );
    assert_eq!(result, Some(json!({"error":"queue_full"})));

    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(p.subscriber)
        .execute(&pool)
        .await
        .unwrap();
    let report = sch.tick(due + TimeDelta::seconds(10)).await.unwrap();
    assert_eq!(report.fired, 1);
    let inputs = delivered(&pool, p.subscriber).await;
    assert_eq!(inputs.len(), 101);
    assert!(inputs[100].0.contains("is now idle."));
    assert_eq!(job_row(&pool).await.0, "fired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_full_keeps_the_event_for_a_day(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    fill_queue(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    let sch = Scheduler::new(
        pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig {
            retry_queue_full: Duration::from_secs(10),
            max_attempts: 2,
            ..Default::default()
        },
    );
    let mut at = now + TimeDelta::seconds(1);
    for _ in 0..6 {
        assert_eq!(sch.tick(at).await.unwrap().retried, 1);
        at += TimeDelta::seconds(10);
    }
    let (state, attempts, failures, _, result) = job_row(&pool).await;
    assert_eq!((state.as_str(), attempts, failures), ("pending", 6, 0));
    assert_eq!(result, Some(json!({"error":"queue_full"})));

    // Другие отказы считаются по-прежнему: терпение относится только к полной очереди.
    sqlx::query("update scheduled_jobs set created_at=$1 where kind='session_task'")
        .bind(at - TimeDelta::hours(24) + TimeDelta::seconds(1))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(sch.tick(at).await.unwrap().retried, 1);
    at += TimeDelta::seconds(10);
    assert_eq!(sch.tick(at).await.unwrap().failed, 1, "the day is over");
    let (state, attempts, failures, _, result) = job_row(&pool).await;
    assert_eq!((state.as_str(), attempts, failures), ("failed", 8, 0));
    assert_eq!(result, Some(json!({"error":"queue_full"})));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replayed_job_does_not_duplicate_the_input(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    let sch = scheduler(&pool);
    assert_eq!(sch.tick(Utc::now()).await.unwrap().fired, 1);
    sqlx::query(
        "update scheduled_jobs set state='pending',fired_at=null where kind='session_task'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(sch.tick(Utc::now()).await.unwrap().fired, 1);
    assert_eq!(delivered(&pool, p.subscriber).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn note_job_queues_its_text(pool: PgPool) {
    let p = pair(&pool).await;
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,now(),$4)",
    )
    .bind(Uuid::now_v7())
    .bind(p.owner)
    .bind(p.subscriber)
    .bind(json!({"action":"note","text":"[canterlot] done"}))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        scheduler(&pool)
            .tick(Utc::now() + TimeDelta::seconds(1))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(
        delivered(&pool, p.subscriber).await[0].0,
        "[canterlot] done"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn malformed_job_fails_without_retries(pool: PgPool) {
    let p = pair(&pool).await;
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,now(),$4)",
    )
    .bind(Uuid::now_v7())
    .bind(p.owner)
    .bind(p.subscriber)
    .bind(json!({"action":"event"}))
    .execute(&pool)
    .await
    .unwrap();
    let report = scheduler(&pool)
        .tick(Utc::now() + TimeDelta::seconds(1))
        .await
        .unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(job_row(&pool).await.0, "failed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn mutual_subscriptions_do_not_deadlock(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    long_idle(&pool, p.subscriber, now).await;
    let limits = thresholds();
    for round in 1..=30 {
        let mut tx = pool.begin().await.unwrap();
        subs::subscribe_in_tx(&mut tx, p.owner, p.subscriber, p.target, 0)
            .await
            .unwrap();
        subs::subscribe_in_tx(&mut tx, p.owner, p.target, p.subscriber, 0)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let (a, b) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(
                activity::refresh(&pool, p.target, now, &limits, Uuid::now_v7()),
                activity::refresh(&pool, p.subscriber, now, &limits, Uuid::now_v7()),
            )
        })
        .await
        .expect("refresh must not hang");
        a.unwrap();
        b.unwrap();
        assert_eq!(jobs(&pool, p.subscriber).await.len(), round);
        assert_eq!(jobs(&pool, p.target).await.len(), round);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_subscriber_during_refresh_does_not_deadlock(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    long_idle(&pool, p.target, now).await;
    let limits = thresholds();
    for round in 0..30 {
        let subscriber = testkit::seed_conversation(&pool, p.owner, p.machine).await;
        let mut tx = pool.begin().await.unwrap();
        subs::subscribe_in_tx(&mut tx, p.owner, subscriber, p.target, 0)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let key = format!("delete-{round}");
        let (refreshed, deleted) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(
                activity::refresh(&pool, p.target, now, &limits, Uuid::now_v7()),
                delete(&pool, &p, subscriber, &key),
            )
        })
        .await
        .expect("must not hang");
        refreshed.unwrap();
        deleted.unwrap();
    }
    let left: i64 = sqlx::query_scalar("select count(*) from scheduled_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "a job never outlives its subscriber");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_approval_does_not_hold_the_queue_or_activity(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let mut tx = pool.begin().await.unwrap();
    requests::create_request_in_tx(
        &mut tx,
        &requests::NewRequest {
            id: Uuid::now_v7(),
            conversation_id: p.target,
            execution_id: execution.execution_id,
            generation: execution.generation,
            kind: RequestKind::SpawnApproval,
            native_request_id: "spawn:1".into(),
            request_payload: json!({"harness": "claude_code"}),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let inputs = activity::load_inputs(&mut conn, p.target, Utc::now(), Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(inputs.open_requests, 0);
    assert_eq!(activity::compute(&inputs, &thresholds()).state, S::Idle);
    assert!(
        !executions::turn_in_progress(&mut conn, p.target)
            .await
            .unwrap()
    );
    drop(conn);

    // Решённая карточка разрешения снимает удержание, открытое одобрение его не держит.
    submit_input(
        &pool,
        SubmitInput {
            owner_id: p.owner,
            conversation_id: p.target,
            idempotency_key: "after-approval".into(),
            content: vec![ContentBlock::Text { text: "go".into() }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(p.target)
        .bind(json!([HoldReason::AwaitingUserRequest]))
        .execute(&pool)
        .await
        .unwrap();
    let gate = GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    let outcome = dispatch::dispatch_next(&pool, p.target, gate, Uuid::now_v7())
        .await
        .unwrap();
    assert!(
        !matches!(&outcome, DispatchOutcome::Blocked(reasons) if reasons.iter().any(|r| matches!(r, BlockReason::Held(h) if h.contains(&HoldReason::AwaitingUserRequest)))),
        "{outcome:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn projector_sweep_fires_after_the_quiet_window(pool: PgPool) {
    // Без исполнения: обход берёт такую цель только из-за открытой подписки.
    let p = pair(&pool).await;
    let config = ActivityConfig {
        tick: Duration::from_millis(20),
        idle_quiet: Duration::from_millis(100),
        ..Default::default()
    };
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, Utc::now()).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());

    let (stop, shutdown) = watch::channel(false);
    let bus = canterlot_hub::bus::EventBus::spawn(pool.clone(), false, shutdown.clone());
    let task = tokio::spawn(activity::run(
        pool.clone(),
        bus,
        config,
        MachinesConfig::default(),
        shutdown,
    ));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while jobs(&pool, p.subscriber).await.is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "sweep never fired");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = stop.send(true);
    task.abort();
}
