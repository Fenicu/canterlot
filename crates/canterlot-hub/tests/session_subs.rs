use std::time::Duration;

use canterlot_hub::{
    activity::{
        self, Activity, ActivityState as S, Evidence as E, QueueFacts, Refresh, Thresholds,
    },
    config::{ActivityConfig, MachinesConfig, SchedulerConfig},
    crunch::{self, SpendOutcome},
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
        subs, summary,
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
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    }
}

fn config() -> SettleConfig {
    SettleConfig {
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
        nudge_enabled: true,
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
        phase_since: None,
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
fn settled_ignores_idle_with_a_busy_slot() {
    let now = Utc::now();
    let input_id = Uuid::now_v7();
    for slot in [
        RunSlot::Dispatching {
            input_id,
            attempt: 1,
        },
        RunSlot::Running { input_id },
    ] {
        let busy = QueueFacts {
            slot,
            ..queue(0, vec![])
        };
        assert_eq!(
            settle::settled(&idle_activity(None), &busy, &old_facts(now), &config()),
            None
        );
    }
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
            lease_wait: None,
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
fn fingerprint_is_state_mark_and_last_turn() {
    let since = DateTime::parse_from_rfc3339("2026-11-01T10:00:00.123456789Z")
        .unwrap()
        .to_utc();
    assert_eq!(
        settle::fingerprint(Settled::Idle, since, None, None),
        "idle|2026-11-01T10:00:00.123456Z|"
    );
    assert_eq!(
        settle::fingerprint(Settled::Stalled, since, Some(since), None),
        "stalled|2026-11-01T10:00:00.123456Z|2026-11-01T10:00:00.123456Z"
    );
    let phase = since + TimeDelta::minutes(5);
    assert_eq!(
        settle::fingerprint(Settled::Stalled, since, Some(since), Some(phase)),
        "stalled|2026-11-01T10:05:00.123456Z|2026-11-01T10:00:00.123456Z",
        "stalled держится на начале фазы, а не на since"
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

async fn alive_metrics(pool: &PgPool, machine: Uuid, execution: Uuid, at: DateTime<Utc>) {
    metrics_with(pool, machine, execution, at, 0, 0).await;
}

async fn metrics_with(
    pool: &PgPool,
    machine: Uuid,
    execution: Uuid,
    at: DateTime<Utc>,
    cpu_permille: u32,
    descendants: u32,
) {
    sqlx::query("delete from machine_metrics where machine_id=$1")
        .bind(machine)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,1,1,100,100,50,0,0,0,'[]',$3)")
        .bind(machine)
        .bind(at)
        .bind(json!([{"execution_id":execution,"alive":true,"cpu_permille":cpu_permille,"descendants":descendants}]))
        .execute(pool)
        .await
        .unwrap();
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
    assert_eq!(inputs[0].1["event"]["kind"], "session_result");
    assert_eq!(inputs[0].1["event"]["state"], "idle");

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
    alive_metrics(&pool, p.machine, execution.execution_id, now).await;
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(now - TimeDelta::minutes(10))
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
    alive_metrics(&pool, p.machine, execution.execution_id, again).await;
    refresh_at(&pool, p.target, again).await;
    refresh_at(&pool, p.target, again + TimeDelta::seconds(15)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 2, "a new episode");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_liveness_never_fires_a_stall(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    sqlx::query(
        "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(now - TimeDelta::minutes(10))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(now - TimeDelta::minutes(10))
    .execute(&pool)
    .await
    .unwrap();
    subscribe(&pool, &p).await;
    // Метрик нет: воркер не смог снять живость, значит и `stalled` не утверждаем.
    for step in 0..3 {
        refresh_at(&pool, p.target, now + TimeDelta::seconds(step * 900)).await;
    }
    assert!(jobs(&pool, p.subscriber).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cpu_busy_generation_never_fires_a_stall(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let phase_at = now - TimeDelta::minutes(10);
    sqlx::query(
        "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    // Реальный случай: модель ещё ничего не написала, но процесс занят работой.
    metrics_with(&pool, p.machine, execution.execution_id, now, 500, 0).await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());
    let later = now + TimeDelta::minutes(20);
    metrics_with(&pool, p.machine, execution.execution_id, later, 500, 0).await;
    refresh_at(&pool, p.target, later).await;
    assert!(
        jobs(&pool, p.subscriber).await.is_empty(),
        "занятый CPU — работа, а не зависание"
    );
    let state: String = sqlx::query_scalar(
        "select activity_state from conversation_derived where conversation_id=$1",
    )
    .bind(p.target)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "working");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn live_tool_descendants_never_fire_a_stall(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let phase_at = now - TimeDelta::hours(2);
    sqlx::query(
        "update executions set phase='running_tool',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    // Живой потомок инструмента — работа, даже если CLI сам не тратит CPU.
    metrics_with(&pool, p.machine, execution.execution_id, now, 0, 2).await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());
    let state: String = sqlx::query_scalar(
        "select activity_state from conversation_derived where conversation_id=$1",
    )
    .bind(p.target)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "working");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_metrics_never_fire_a_stall(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let phase_at = now - TimeDelta::hours(2);
    sqlx::query(
        "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    // Снимок старше `metrics_stale_after`: живость неизвестна, значит не утверждаем простой.
    metrics_with(
        &pool,
        p.machine,
        execution.execution_id,
        now - TimeDelta::hours(1),
        0,
        0,
    )
    .await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());
    let state: String = sqlx::query_scalar(
        "select activity_state from conversation_derived where conversation_id=$1",
    )
    .bind(p.target)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "working");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn execution_cpu_above_one_core_is_accepted_and_busy(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let phase_at = now - TimeDelta::minutes(20);
    sqlx::query(
        "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    metrics_with(&pool, p.machine, execution.execution_id, now, 2400, 0).await;
    let mut conn = pool.acquire().await.unwrap();
    let inputs = activity::load_inputs(
        &mut conn,
        p.target,
        now,
        MachinesConfig::default().metrics_stale_after,
    )
    .await
    .unwrap();
    let process = inputs.process.expect("metric above one core is accepted");
    assert_eq!(process.cpu_permille, 1000, "clamped only for comparison");
    assert_eq!(activity::compute(&inputs, &thresholds()).state, S::Working);
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert!(jobs(&pool, p.subscriber).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stalled_is_not_repeated_when_the_state_flaps_in_the_same_phase(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    let now = Utc::now();
    let phase_at = now - TimeDelta::minutes(10);
    sqlx::query(
        "update executions set phase='generating',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution.execution_id)
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
                run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(p.target)
    .bind(Uuid::now_v7())
    .bind(phase_at)
    .execute(&pool)
    .await
    .unwrap();
    alive_metrics(&pool, p.machine, execution.execution_id, now).await;
    subscribe(&pool, &p).await;
    refresh_at(&pool, p.target, now).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    // Короткое пробуждение: отметка события сдвинулась, фаза та же.
    sqlx::query("update executions set last_event_at=$2 where id=$1")
        .bind(execution.execution_id)
        .bind(now + TimeDelta::seconds(1))
        .execute(&pool)
        .await
        .unwrap();
    refresh_at(&pool, p.target, now + TimeDelta::seconds(2)).await;
    let later = now + TimeDelta::seconds(302);
    alive_metrics(&pool, p.machine, execution.execution_id, later).await;
    refresh_at(&pool, p.target, later).await;
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        1,
        "та же фаза и то же подозрение не повторяются"
    );
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

async fn lost(pool: &PgPool, p: &Pair, execution: Uuid) {
    executions::set_state(
        pool,
        execution,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    refresh_at(pool, p.target, Utc::now()).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_lost_keeps_the_subscription_for_the_result_after_a_reclaim(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    subscribe(&pool, &p).await;
    lost(&pool, &p, execution.execution_id).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1, "the subscriber hears about the loss");
    assert_eq!(fired[0]["state"], "failed");
    assert!(
        subscription(&pool, p.subscriber, p.target).await["closed_at"].is_null(),
        "a reclaim can still bring the session back"
    );
    let mut conn = pool.acquire().await.unwrap();
    let current = subs::current_in_tx(&mut conn, p.target, Utc::now(), &config())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.state.as_str(), "failed");
    assert!(!current.closes, "session_wait must not close it either");
    drop(conn);

    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Running,
        Some(canterlot_proto::OWNER_RECLAIMED),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let now = Utc::now();
    refresh_at(&pool, p.target, now).await;
    finish_turn(&pool, &p, &execution, now).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(10)).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 2, "the result after the reclaim gets through");
    assert_eq!(fired[1]["state"], "idle");
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "idle"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_exit_of_a_lost_session_gets_its_own_report(pool: PgPool) {
    let p = pair(&pool).await;
    let execution = start(&pool, p.owner, p.target).await;
    subscribe(&pool, &p).await;
    lost(&pool, &p, execution.execution_id).await;
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Failed,
        Some("admin_resolved"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    refresh_at(&pool, p.target, Utc::now()).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(
        fired.len(),
        2,
        "the state name is the same, the news is not"
    );
    assert_eq!(fired[1]["state"], "failed");
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "failed"
    );
}

/// Ввод вышел из счётных состояний, а пересчёт проекции ещё не случился: она всё говорит `idle`.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn poll_of_a_stale_idle_does_not_settle_a_turn_in_progress(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    start(&pool, p.owner, p.target).await;
    long_idle(&pool, p.target, now).await;
    let submitted = submit_input(
        &pool,
        SubmitInput {
            owner_id: p.owner,
            conversation_id: p.target,
            idempotency_key: "in-flight".into(),
            content: vec![ContentBlock::Text { text: "go".into() }],
            received_at: now,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("update inputs set state='transmitting' where id=$1")
        .bind(submitted.input_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(p.target)
    .bind(submitted.input_id)
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let current = subs::current_in_tx(&mut conn, p.target, now, &config())
        .await
        .unwrap();
    assert!(current.is_none(), "the turn has only just started");
}

/// Цель осела, подписка закрыта итогом `idle`.
async fn reported_idle(pool: &PgPool, p: &Pair, now: DateTime<Utc>) {
    start(pool, p.owner, p.target).await;
    long_idle(pool, p.target, now).await;
    subscribe(pool, p).await;
    refresh_at(pool, p.target, now).await;
    assert_eq!(
        subscription(pool, p.subscriber, p.target).await["closed_reason"],
        "idle"
    );
}

async fn wake_turn(pool: &PgPool, id: Uuid, running: bool) {
    sqlx::query(
        "update executions set wake_turn=$2,last_event_at=clock_timestamp()
         where conversation_id=$1",
    )
    .bind(id)
    .bind(running)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_turn_after_the_report_reopens_the_subscription(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    reported_idle(&pool, &p, now).await;

    wake_turn(&pool, p.target, true).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(10)).await;
    assert!(subscription(&pool, p.subscriber, p.target).await["closed_at"].is_null());
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    wake_turn(&pool, p.target, false).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(20)).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(30)).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 2, "the wake turn gets its own report");
    assert_eq!(fired[1]["state"], "idle");
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "idle"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_turn_does_not_reopen_what_the_subscriber_gave_up(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    reported_idle(&pool, &p, now).await;
    let mut tx = pool.begin().await.unwrap();
    subs::unsubscribe_in_tx(&mut tx, p.subscriber, p.target, "unsubscribed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    wake_turn(&pool, p.target, true).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(10)).await;
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "unsubscribed"
    );

    sqlx::query("update session_subscriptions set closed_reason='idle'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let others = subs::close_for_accept_in_tx(&mut tx, p.target, Some(p.subscriber), "accepted")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(others.is_empty(), "a closed subscription is told nothing");
    refresh_at(&pool, p.target, now + TimeDelta::seconds(20)).await;
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "accepted"
    );
}

async fn pending_jobs(pool: &PgPool, subscriber: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from scheduled_jobs
         where kind='session_task' and state='pending' and conversation_id=$1",
    )
    .bind(subscriber)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn child_event_after_the_idle_report_reopens_the_subscription(pool: PgPool) {
    let p = pair(&pool).await;
    let child = testkit::seed_conversation(&pool, p.owner, p.machine).await;
    let now = Utc::now();
    let execution = start(&pool, p.owner, p.target).await;
    long_idle(&pool, p.target, now).await;
    long_idle(&pool, child, now).await;
    // О ждёт итог Д, Д ждёт итог своего ребёнка.
    subscribe(&pool, &p).await;
    let mut tx = pool.begin().await.unwrap();
    subs::subscribe_in_tx(&mut tx, p.owner, p.target, child, 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    refresh_at(&pool, p.target, now).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "idle"
    );

    // Ребёнок осел: Д получает событие, а подписка О на Д уже закрыта прошлым idle.
    refresh_at(&pool, child, now).await;
    assert_eq!(pending_jobs(&pool, p.target).await, 1);
    scheduler(&pool)
        .tick(now + TimeDelta::seconds(10))
        .await
        .unwrap();
    let inputs = delivered(&pool, p.target).await;
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].1["kind"], "event");
    assert_eq!(inputs[0].1["from"], json!(child));
    assert!(
        subscription(&pool, p.subscriber, p.target).await["closed_at"].is_null(),
        "the child event is not the end of D"
    );

    // Д продолжил работу по событию и снова осел: О слышит второй итог.
    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(p.target)
        .execute(&pool)
        .await
        .unwrap();
    finish_turn(&pool, &p, &execution, now + TimeDelta::seconds(15)).await;
    refresh_at(&pool, p.target, now + TimeDelta::seconds(30)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 2);
    assert_eq!(
        subscription(&pool, p.subscriber, p.target).await["closed_reason"],
        "idle"
    );

    refresh_at(&pool, p.target, now + TimeDelta::seconds(40)).await;
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        2,
        "one report per settling, not one per poll"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_message_from_the_subscriber_does_not_use_the_automation_reopen(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    reported_idle(&pool, &p, now).await;
    let closed = subscription(&pool, p.subscriber, p.target).await;

    submit_input(
        &pool,
        SubmitInput {
            owner_id: p.owner,
            conversation_id: p.target,
            idempotency_key: "from-subscriber".into(),
            content: vec![ContentBlock::Text {
                text: "continue".into(),
            }],
            received_at: now,
            correlation_id: Uuid::now_v7(),
            origin: Some(json!({
                "kind": "message",
                "from": p.subscriber,
                "chain_depth": 0,
                "sha256": "00",
            })),
        },
    )
    .await
    .unwrap();
    let after_input = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(
        after_input["closed_reason"], "idle",
        "session_send re-arms itself in its own transaction"
    );
    assert_eq!(after_input["armed_at"], closed["armed_at"]);

    // Так делает session_send: взводит подписку заново с новым окном.
    subscribe(&pool, &p).await;
    let armed = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(armed["closed_at"], Value::Null);
    assert_eq!(armed["last_fingerprint"], Value::Null);
    assert_ne!(armed["armed_at"], closed["armed_at"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn automation_input_does_not_reopen_unsubscribed_or_accepted(pool: PgPool) {
    let a = pair(&pool).await;
    let b = pair(&pool).await;
    let now = Utc::now();
    reported_idle(&pool, &a, now).await;
    let mut tx = pool.begin().await.unwrap();
    subs::unsubscribe_in_tx(&mut tx, a.subscriber, a.target, "unsubscribed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    reported_idle(&pool, &b, now).await;
    let mut tx = pool.begin().await.unwrap();
    let others = subs::close_for_accept_in_tx(&mut tx, b.target, Some(b.subscriber), "accepted")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(others.is_empty());

    for (p, key) in [(&a, "alarm-a"), (&b, "alarm-b")] {
        submit_input(
            &pool,
            SubmitInput {
                owner_id: p.owner,
                conversation_id: p.target,
                idempotency_key: key.into(),
                content: vec![ContentBlock::Text {
                    text: "[canterlot] Alarm.".into(),
                }],
                received_at: now,
                correlation_id: Uuid::now_v7(),
                origin: Some(json!({"kind": "alarm", "chain_depth": 0})),
            },
        )
        .await
        .unwrap();
    }

    assert_eq!(
        subscription(&pool, a.subscriber, a.target).await["closed_reason"],
        "unsubscribed"
    );
    assert_eq!(
        subscription(&pool, b.subscriber, b.target).await["closed_reason"],
        "accepted"
    );
    assert_eq!(pending_jobs(&pool, a.subscriber).await, 0);
    assert_eq!(pending_jobs(&pool, b.subscriber).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_summary_names_armed_alarms_without_their_notes(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    let first = now + TimeDelta::hours(2);
    let second = now + TimeDelta::hours(5);
    for (key, fire_at) in [("gate", first), ("disk", second)] {
        sqlx::query(
            "insert into alarms(id,owner_id,conversation_id,created_by,key,kind,fire_at,note,call_id)
             values($1,$2,$3,'agent',$4,'at',$5,'secret alarm note',$6)",
        )
        .bind(Uuid::now_v7())
        .bind(p.owner)
        .bind(p.target)
        .bind(key)
        .bind(fire_at)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut conn = pool.acquire().await.unwrap();
    let summary = summary::build(&mut conn, p.owner, p.target, "idle", Some(now))
        .await
        .unwrap()
        .unwrap();
    let text = summary::render(&summary);
    assert!(
        text.contains(&format!(
            "Alarms: 2 armed, next at {}",
            first.format("%Y-%m-%d %H:%M UTC")
        )),
        "{text}"
    );
    assert!(!text.contains("secret alarm note"), "{text}");

    sqlx::query("update alarms set state='cancelled' where conversation_id=$1")
        .bind(p.target)
        .execute(&pool)
        .await
        .unwrap();
    let summary = summary::build(&mut conn, p.owner, p.target, "idle", Some(now))
        .await
        .unwrap()
        .unwrap();
    assert!(!summary::render(&summary).contains("Alarms:"));
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
    assert_eq!(inputs[0].1["event"]["kind"], "deleted");
    assert_eq!(inputs[0].1["event"]["session_id"], json!(p.target));
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

    // Другие отказы считаются по-прежнему: терпение относится только к полной очереди
    // и отсчитывается от первой блокировки, а не от создания задания.
    sqlx::query("update scheduled_jobs set blocked_since=$1 where kind='session_task'")
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
        true,
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

fn background_facts(now: DateTime<Utc>, idle_for: TimeDelta) -> SettleFacts {
    SettleFacts {
        now,
        since: now - idle_for,
        last_turn_at: Some(now - idle_for),
        last_event_at: Some(now - TimeDelta::minutes(1)),
        phase_since: None,
    }
}

#[test]
fn idle_background_waits_for_the_threshold() {
    let now = Utc::now();
    let busy = idle_activity(Some(2));
    let empty = queue(0, vec![]);
    let under = background_facts(now, TimeDelta::minutes(14));
    assert_eq!(settle::settled(&busy, &empty, &under, &config()), None);
    assert_eq!(
        settle::idle_background(&busy, &empty, &under, &config()),
        None
    );
    let past = background_facts(now, TimeDelta::minutes(15));
    assert_eq!(
        settle::settled(&busy, &empty, &past, &config()),
        None,
        "the warning is not a settled state"
    );
    assert_eq!(
        settle::idle_background(&busy, &empty, &past, &config()),
        Some(settle::IdleBackground {
            jobs: 2,
            idle_since: now - TimeDelta::minutes(15)
        })
    );
    let custom = SettleConfig {
        idle_background_max: Duration::from_secs(40 * 60),
        ..config()
    };
    assert_eq!(
        settle::idle_background(&busy, &empty, &past, &custom),
        None,
        "the threshold comes from the config"
    );
}

#[test]
fn idle_background_counts_from_the_last_turn_and_ignores_other_states() {
    let now = Utc::now();
    let busy = idle_activity(Some(1));
    let empty = queue(0, vec![]);
    let mut facts = background_facts(now, TimeDelta::hours(1));
    facts.last_turn_at = Some(now - TimeDelta::minutes(3));
    assert_eq!(
        settle::idle_background(&busy, &empty, &facts, &config()),
        None,
        "a new turn starts a new idle episode"
    );
    let facts = background_facts(now, TimeDelta::hours(1));
    assert_eq!(
        settle::idle_background(&idle_activity(None), &empty, &facts, &config()),
        None,
        "without background work idle settles normally"
    );
    assert_eq!(
        settle::idle_background(&busy, &queue(1, vec![]), &facts, &config()),
        None,
        "a queued input is the continuation"
    );
    let mut silent = facts;
    silent.last_event_at = Some(now - TimeDelta::minutes(31));
    assert_eq!(
        settle::settled(&busy, &empty, &silent, &config()),
        Some(Settled::Idle)
    );
    assert_eq!(
        settle::idle_background(&busy, &empty, &silent, &config()),
        None,
        "a settled session gets the normal event instead"
    );
    let working = Activity {
        state: S::Working,
        evidence: E::Working {
            phase: "running_tool".into(),
            tool_name: None,
            summary: None,
            phase_since: None,
            lease_wait: None,
        },
    };
    assert_eq!(
        settle::idle_background(&working, &empty, &facts, &config()),
        None
    );
    let since = now - TimeDelta::hours(1);
    assert_ne!(
        settle::idle_background_fingerprint(since, Some(since)),
        settle::fingerprint(Settled::Idle, since, Some(since), None),
        "the real idle report must not be swallowed by the warning"
    );
}

async fn background_job(
    pool: &PgPool,
    p: &Pair,
    execution: &executions::StartedExecution,
    state: &str,
) {
    sqlx::query(
        "insert into background_tasks(conversation_id,task_id,execution_id,generation,source_revision,
           kind,description,state,started_at)
         values($1,'tail',$2,$3,1,'bash','tail -F build.log',$4,now())
         on conflict(conversation_id,task_id) do update set state=excluded.state",
    )
    .bind(p.target)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

async fn last_event(pool: &PgPool, id: Uuid, at: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(id)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

/// Ход кончился в `t0`, сессия простаивает с работающим `tail -F`; подписчик взведён.
async fn idle_with_background(
    pool: &PgPool,
    t0: DateTime<Utc>,
) -> (Pair, executions::StartedExecution) {
    let p = pair(pool).await;
    let execution = start(pool, p.owner, p.target).await;
    finish_turn(pool, &p, &execution, t0).await;
    background_job(pool, &p, &execution, "running").await;
    last_event(pool, p.target, t0).await;
    refresh_at(pool, p.target, t0 + TimeDelta::seconds(1)).await;
    subscribe(pool, &p).await;
    (p, execution)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_with_background_warns_once_past_the_threshold(pool: PgPool) {
    let t0 = Utc::now() - TimeDelta::minutes(20);
    let (p, _) = idle_with_background(&pool, t0).await;
    for minutes in [1, 5, 10, 14] {
        refresh_at(&pool, p.target, t0 + TimeDelta::minutes(minutes)).await;
    }
    assert!(
        jobs(&pool, p.subscriber).await.is_empty(),
        "under the threshold the subscriber hears nothing"
    );

    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(16)).await;
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(17)).await;
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(18)).await;
    let queued = jobs(&pool, p.subscriber).await;
    assert_eq!(queued.len(), 1, "one warning per idle episode: {queued:?}");
    assert_eq!(queued[0]["state"], "idle_background");
    assert_eq!(queued[0]["background_jobs"], 1);
    let open = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(
        open["closed_at"],
        Value::Null,
        "the warning is not a result"
    );

    let (state,): (String,) = sqlx::query_as(
        "select activity_state::text from conversation_derived where conversation_id=$1",
    )
    .bind(p.target)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "idle", "the state shown to the owner stays as is");

    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1);
    let (text, origin, _) = &delivered[0];
    assert!(
        text.starts_with("[canterlot] Session \"")
            && text.contains("has been idle for ")
            && text.contains(" min with 1 background job still running; it is not doing anything until it gets a new input. Check its background job or send it a message."),
        "{text}"
    );
    assert!(!text.contains("is now idle"), "{text}");
    assert_eq!(origin["event"]["kind"], "idle_background");
    assert_eq!(origin["event"]["state"], "idle");
    assert_eq!(origin["event"]["session_id"], json!(p.target));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_turn_starts_a_new_idle_background_episode(pool: PgPool) {
    let t0 = Utc::now() - TimeDelta::minutes(50);
    let (p, execution) = idle_with_background(&pool, t0).await;
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(16)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);

    // Ход пробуждения: тот же фон, но простой начинается заново.
    finish_turn(&pool, &p, &execution, t0 + TimeDelta::minutes(17)).await;
    last_event(&pool, p.target, t0 + TimeDelta::minutes(17)).await;
    for minutes in [18, 25, 31] {
        refresh_at(&pool, p.target, t0 + TimeDelta::minutes(minutes)).await;
    }
    assert_eq!(
        jobs(&pool, p.subscriber).await.len(),
        1,
        "the new episode is under the threshold"
    );
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(33)).await;
    let queued = jobs(&pool, p.subscriber).await;
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[1]["state"], "idle_background");
    assert_ne!(queued[0]["fingerprint"], queued[1]["fingerprint"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn finished_background_still_reports_the_normal_idle_once(pool: PgPool) {
    let t0 = Utc::now() - TimeDelta::minutes(30);
    let (p, execution) = idle_with_background(&pool, t0).await;
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(16)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);
    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);

    background_job(&pool, &p, &execution, "completed").await;
    last_event(&pool, p.target, t0 + TimeDelta::minutes(20)).await;
    for seconds in [10, 30, 60] {
        refresh_at(
            &pool,
            p.target,
            t0 + TimeDelta::minutes(20) + TimeDelta::seconds(seconds),
        )
        .await;
    }
    let queued = jobs(&pool, p.subscriber).await;
    assert_eq!(queued.len(), 2, "{queued:?}");
    assert_eq!(queued[1]["state"], "idle");
    let closed = subscription(&pool, p.subscriber, p.target).await;
    assert_eq!(closed["closed_reason"], "idle");

    assert_eq!(scheduler(&pool).tick(Utc::now()).await.unwrap().fired, 1);
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 2);
    assert!(
        delivered[0].0.contains("background job still running"),
        "{}",
        delivered[0].0
    );
    assert!(delivered[1].0.contains("is now idle"), "{}", delivered[1].0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stale_idle_background_warning_is_not_delivered(pool: PgPool) {
    let t0 = Utc::now() - TimeDelta::minutes(30);
    let (p, execution) = idle_with_background(&pool, t0).await;
    refresh_at(&pool, p.target, t0 + TimeDelta::minutes(16)).await;
    assert_eq!(jobs(&pool, p.subscriber).await.len(), 1);
    // Фон кончился, пока задание ждало доставки: о нём скажет обычный итог.
    background_job(&pool, &p, &execution, "completed").await;
    refresh_at(
        &pool,
        p.target,
        t0 + TimeDelta::minutes(16) + TimeDelta::seconds(1),
    )
    .await;
    scheduler(&pool).tick(Utc::now()).await.unwrap();
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1, "only the normal report");
    assert!(delivered[0].0.contains("is now idle"), "{}", delivered[0].0);
    let (result,): (Value,) = sqlx::query_as(
        "select result from scheduled_jobs where kind='session_task'
           and payload->>'state'='idle_background'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        result["reason"], "no_longer_idle_with_background",
        "{result}"
    );
}

fn quota_hold(provider: &str, window: &str, wake_at: DateTime<Utc>) -> HoldReason {
    HoldReason::QuotaExhausted {
        provider: provider.into(),
        window: window.into(),
        wake_at,
    }
}

/// Подписчик сам стоит на паузе лимита: удержание в очереди и ожидающее пробуждение.
async fn hold_subscriber(
    pool: &PgPool,
    p: &Pair,
    provider: &str,
    window: &str,
    wake_at: DateTime<Utc>,
    wake_run_at: DateTime<Utc>,
) {
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(p.subscriber)
        .bind(json!([quota_hold(provider, window, wake_at)]))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("update conversations set resume_pending=true where id=$1")
        .bind(p.subscriber)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'wake',$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(p.owner)
    .bind(p.subscriber)
    .bind(wake_run_at)
    .bind(json!({
        "provider": provider,
        "window": window,
        "hold_key": format!("quota:{provider}:{window}"),
    }))
    .execute(pool)
    .await
    .unwrap();
}

/// Подписчик стоит на паузе недельного резерва: удержание и ожидающее пробуждение.
async fn hold_subscriber_weekly(
    pool: &PgPool,
    p: &Pair,
    wake_at: DateTime<Utc>,
    wake_run_at: DateTime<Utc>,
) {
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(p.subscriber)
        .bind(json!([HoldReason::WeeklyReserve {
            provider: "claude".into(),
            resets_at: Some(wake_at),
        }]))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("update conversations set resume_pending=true where id=$1")
        .bind(p.subscriber)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'wake',$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(p.owner)
    .bind(p.subscriber)
    .bind(wake_run_at)
    .bind(json!({
        "provider": "claude",
        "window": "weekly",
        "hold_key": "weekly:claude",
        "verified": true,
    }))
    .execute(pool)
    .await
    .unwrap();
}

/// Цель устоялась в `quota_paused`: проектор ставит подписчику задание события.
async fn paused_target(pool: &PgPool, p: &Pair, now: DateTime<Utc>) -> DateTime<Utc> {
    let wake_at = now + TimeDelta::hours(1);
    // Удержание видно в состоянии активности только при живом исполнении: без него цель
    // сразу считается простаивающей.
    start(pool, p.owner, p.target).await;
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(p.target)
        .bind(json!([quota_hold("claude", "five_hour", wake_at)]))
        .execute(pool)
        .await
        .unwrap();
    refresh_at(pool, p.target, now).await;
    wake_at
}

#[derive(sqlx::FromRow)]
struct ScheduledTask {
    state: String,
    run_at: DateTime<Utc>,
    attempts: i32,
}

async fn scheduled_tasks(pool: &PgPool, subscriber: Uuid) -> Vec<ScheduledTask> {
    sqlx::query_as(
        "select state,run_at,attempts from scheduled_jobs
         where kind='session_task' and conversation_id=$1 order by created_at,id",
    )
    .bind(subscriber)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn notifications_wait_for_a_limit_held_subscriber(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    let wake_at = now + TimeDelta::hours(1);
    let wake_run_at = wake_at + TimeDelta::seconds(20);
    hold_subscriber(&pool, &p, "claude", "five_hour", wake_at, wake_run_at).await;
    subscribe(&pool, &p).await;
    let paused_at = paused_target(&pool, &p, now).await;
    assert_eq!(paused_at, wake_at);
    assert_eq!(scheduled_tasks(&pool, p.subscriber).await.len(), 1);

    scheduler(&pool).tick(now).await.unwrap();
    assert!(
        delivered(&pool, p.subscriber).await.is_empty(),
        "a notification must not enter a queue held by a limit"
    );
    let job = &scheduled_tasks(&pool, p.subscriber).await[0];
    assert_eq!(job.state, "pending");
    assert_eq!(
        job.run_at.timestamp_millis(),
        wake_run_at.timestamp_millis(),
        "the wake time plus the grace"
    );
    assert_eq!(job.attempts, 0);

    // Срок пробуждения прошёл, но удержание ещё не снято: откладывание не тратит попытки.
    scheduler(&pool).tick(wake_run_at).await.unwrap();
    assert!(delivered(&pool, p.subscriber).await.is_empty());
    let job = &scheduled_tasks(&pool, p.subscriber).await[0];
    assert_eq!(job.state, "pending");
    assert_eq!(job.attempts, 0, "max_attempts must not be spent on a hold");

    sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
        .bind(p.subscriber)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update scheduled_jobs set state='cancelled' where kind='wake' and conversation_id=$1",
    )
    .bind(p.subscriber)
    .execute(&pool)
    .await
    .unwrap();
    let at = scheduled_tasks(&pool, p.subscriber).await[0].run_at;
    scheduler(&pool)
        .tick(at + TimeDelta::seconds(1))
        .await
        .unwrap();
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1, "exactly one delivery after the hold");
    assert_eq!(delivered[0].1["event"]["kind"], "quota_paused");
    assert_eq!(scheduled_tasks(&pool, p.subscriber).await[0].state, "fired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hold_removed_early_delivers_postponed_notifications_right_away(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    let wake_at = now + TimeDelta::hours(1);
    let wake_run_at = wake_at + TimeDelta::seconds(20);
    hold_subscriber_weekly(&pool, &p, wake_at, wake_run_at).await;
    subscribe(&pool, &p).await;
    paused_target(&pool, &p, now).await;

    scheduler(&pool).tick(now).await.unwrap();
    assert!(delivered(&pool, p.subscriber).await.is_empty());
    let job = &scheduled_tasks(&pool, p.subscriber).await[0];
    assert_eq!(
        job.run_at.timestamp_millis(),
        wake_run_at.timestamp_millis(),
        "the notification waits for the reserve reset"
    );

    // Владелец тратит резерв раньше срока: держать уведомление больше нечем.
    let outcome = crunch::spend_reserve(
        &pool,
        p.owner,
        p.subscriber,
        "spend-before-reset",
        now,
        &Dispatcher::channel().0,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, SpendOutcome::Continued);
    let job = &scheduled_tasks(&pool, p.subscriber).await[0];
    assert!(
        job.run_at <= now,
        "the postponed notification is pulled forward: {}",
        job.run_at
    );

    scheduler(&pool).tick(now).await.unwrap();
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1, "exactly one delivery after the hold");
    assert_eq!(delivered[0].1["event"]["kind"], "quota_paused");
    assert_eq!(scheduled_tasks(&pool, p.subscriber).await[0].state, "fired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_postponed_event_never_arrives_after_a_newer_one(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    let wake_at = now + TimeDelta::hours(1);
    let wake_run_at = wake_at + TimeDelta::seconds(20);
    hold_subscriber_weekly(&pool, &p, wake_at, wake_run_at).await;
    subscribe(&pool, &p).await;
    let execution = start(&pool, p.owner, p.target).await;

    // A: цель стоит на паузе лимита.
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(p.target)
        .bind(json!([quota_hold("claude", "five_hour", wake_at)]))
        .execute(&pool)
        .await
        .unwrap();
    refresh_at(&pool, p.target, now).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0]["state"], "quota_paused");
    scheduler(&pool).tick(now).await.unwrap();
    assert!(delivered(&pool, p.subscriber).await.is_empty());

    // B: исполнение потеряно, цель устоялась в `failed`.
    sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
        .bind(p.target)
        .execute(&pool)
        .await
        .unwrap();
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    refresh_at(&pool, p.target, now + TimeDelta::seconds(1)).await;
    let fired = jobs(&pool, p.subscriber).await;
    assert_eq!(fired.len(), 2);
    assert_eq!(fired[1]["state"], "failed");
    scheduler(&pool)
        .tick(now + TimeDelta::seconds(1))
        .await
        .unwrap();
    assert!(delivered(&pool, p.subscriber).await.is_empty());

    crunch::spend_reserve(
        &pool,
        p.owner,
        p.subscriber,
        "spend-after-two-events",
        now,
        &Dispatcher::channel().0,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    scheduler(&pool)
        .tick(now + TimeDelta::seconds(2))
        .await
        .unwrap();

    let delivered = delivered(&pool, p.subscriber).await;
    assert!(delivered.len() <= 2, "{delivered:?}");
    let last = delivered
        .last()
        .and_then(|(_, origin, _)| origin["event"]["kind"].as_str());
    assert_ne!(
        last,
        Some("quota_paused"),
        "the old state must not be the last word: {delivered:?}"
    );
    assert_eq!(last, Some("failed"), "{delivered:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn notifications_are_delivered_immediately_without_a_hold(pool: PgPool) {
    let p = pair(&pool).await;
    let now = Utc::now();
    subscribe(&pool, &p).await;
    paused_target(&pool, &p, now).await;
    scheduler(&pool).tick(now).await.unwrap();
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].1["event"]["kind"], "quota_paused");
    assert_eq!(scheduled_tasks(&pool, p.subscriber).await[0].state, "fired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_deleted_event_reaches_a_limit_held_subscriber(pool: PgPool) {
    let p = pair(&pool).await;
    subscribe(&pool, &p).await;
    let now = Utc::now();
    let mut tx = pool.begin().await.unwrap();
    subs::on_target_deleted_in_tx(&mut tx, p.target, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    hold_subscriber(
        &pool,
        &p,
        "claude",
        "five_hour",
        now + TimeDelta::hours(1),
        now + TimeDelta::hours(1) + TimeDelta::seconds(20),
    )
    .await;
    scheduler(&pool).tick(now).await.unwrap();
    let delivered = delivered(&pool, p.subscriber).await;
    assert_eq!(delivered.len(), 1, "deletion is not a notification to hold");
    assert!(delivered[0].0.contains("was deleted"), "{}", delivered[0].0);
}
