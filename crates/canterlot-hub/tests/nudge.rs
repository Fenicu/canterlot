use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{SchedulerConfig, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions, machine_info,
    model::ExecutionState,
    scheduler::Scheduler,
    session_tools::{self, Caller, ToolContext, ToolErrorCode, nudge, settle::SettleConfig, subs},
    testkit,
};
use canterlot_proto::worker::v1::{ExecutionProcess, MachineMetrics};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
    }
}

struct Fx {
    owner: Uuid,
    machine: Uuid,
    parent: Uuid,
    child: Uuid,
    execution: executions::StartedExecution,
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

/// Родитель и его делегат: видимость и запись разрешены, оба давно простаивают.
async fn fx(pool: &PgPool) -> Fx {
    let seed = testkit::seed(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    let child = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
    sqlx::query(
        "update conversations set parent_conversation_id=$2,spawn_depth=1,title='child' where id=$1",
    )
    .bind(child)
    .bind(seed.conversation_id)
    .execute(pool)
    .await
    .unwrap();
    let execution = start(pool, seed.owner_id, child).await;
    Fx {
        owner: seed.owner_id,
        machine: seed.machine_id,
        parent: seed.conversation_id,
        child,
        execution,
    }
}

async fn refresh_at(pool: &PgPool, id: Uuid, now: DateTime<Utc>) {
    activity::refresh(pool, id, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
}

/// Проекция цели ещё до окна тишины, затем рабочие факты поверх неё.
async fn prime_idle(pool: &PgPool, f: &Fx, now: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(f.child)
        .bind(now - TimeDelta::hours(1))
        .execute(pool)
        .await
        .unwrap();
    refresh_at(pool, f.child, now - TimeDelta::hours(1)).await;
}

async fn answer(pool: &PgPool, f: &Fx, text: &str) -> Uuid {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(f.child)
    .bind(f.execution.execution_id)
    .bind(f.execution.generation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,agent,model,created_seq,created_at,updated_at)
         values($1,$2,$3,'text','final',1,$4,$5,'main','m',(select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),clock_timestamp()-interval '10 minutes',clock_timestamp()-interval '10 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(f.execution.execution_id)
    .bind(f.child)
    .bind(text)
    .bind(command)
    .execute(pool)
    .await
    .unwrap();
    command
}

async fn finish_run(
    pool: &PgPool,
    f: &Fx,
    command: Uuid,
    at: DateTime<Utc>,
    head: &str,
    dirty: bool,
    tools: &[&str],
) {
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms,git)
         values($1,$2,$3,$4,$4,1000,$5)",
    )
    .bind(command)
    .bind(f.execution.execution_id)
    .bind(f.child)
    .bind(at)
    .bind(json!({
        "available": true,
        "head_before": head,
        "head_after": head,
        "dirty": dirty,
        "truncated": false,
        "commits": [],
        "files": [{"path": "src/lib.rs", "status": "M"}]
    }))
    .execute(pool)
    .await
    .unwrap();
    for tool in tools {
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,agent,model,created_seq,created_at,updated_at)
             values($1,$2,$3,'tool_call','final',1,$4,$5,'main','m',(select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),clock_timestamp()-interval '10 minutes',clock_timestamp()-interval '10 minutes')",
        )
        .bind(Uuid::now_v7())
        .bind(f.execution.execution_id)
        .bind(f.child)
        .bind(tool)
        .bind(command)
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn rule(
    pool: &PgPool,
    f: &Fx,
    author: Uuid,
    max_consecutive: i32,
    idle_minutes: i32,
    background_minutes: i32,
) {
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,set_by_conversation,
             notify_conversation_id,idle_minutes,background_minutes,max_consecutive,state)
         values($1,$2,'agent',$3,$3,$4,$5,$6,'watching')",
    )
    .bind(f.child)
    .bind(f.owner)
    .bind(author)
    .bind(idle_minutes)
    .bind(background_minutes)
    .bind(max_consecutive)
    .execute(pool)
    .await
    .unwrap();
}

async fn jobs(pool: &PgPool, conversation: Uuid) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from scheduled_jobs where kind='session_task' and conversation_id=$1
         order by id",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn scheduler(pool: &PgPool) -> Scheduler {
    Scheduler::new(
        pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig::default(),
    )
}

async fn subscribe(pool: &PgPool, f: &Fx) {
    let mut tx = pool.begin().await.unwrap();
    subs::subscribe_in_tx(&mut tx, f.owner, f.parent, f.child, 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn accept_inputs(pool: &PgPool, conversation: Uuid) {
    sqlx::query(
        "update inputs set state='accepted',feed_seq=coalesce(feed_seq,input_seq)
         where conversation_id=$1 and state='queued'",
    )
    .bind(conversation)
    .execute(pool)
    .await
    .unwrap();
}

async fn rule_row(pool: &PgPool, id: Uuid) -> (String, i32, i32) {
    sqlx::query_as("select state,consecutive,total from nudge_rules where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

fn caller(f: &Fx) -> Caller {
    Caller {
        owner_id: f.owner,
        conversation_id: f.parent,
        execution_id: Uuid::now_v7(),
        machine_id: f.machine,
        call_id: Uuid::now_v7(),
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_without_marker_is_held_and_with_marker_is_delivered(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;

    let command = answer(&pool, &f, "Implemented the parser; tests still running.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "aaa",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    assert!(
        jobs(&pool, f.parent).await.is_empty(),
        "idle without a marker is held, the subscriber hears nothing yet"
    );
    let held = jobs(&pool, f.child).await;
    assert_eq!(held.len(), 1, "one nudge job per fingerprint");
    assert_eq!(held[0]["action"], "nudge");
    let state: String =
        sqlx::query_scalar("select state from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "pending");

    // Тот же отпечаток повторно: второго задания нет.
    refresh_at(&pool, f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(jobs(&pool, f.child).await.len(), 1);

    // Маркер в последнем ответе — завершение, подписчик получает итог.
    let f2 = fx(&pool).await;
    subscribe(&pool, &f2).await;
    prime_idle(&pool, &f2, now).await;
    rule(&pool, &f2, f2.parent, 3, 3, 10).await;
    let command = answer(&pool, &f2, "All done.\nSTATUS: DONE").await;
    finish_run(
        &pool,
        &f2,
        command,
        now - TimeDelta::minutes(1),
        "bbb",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f2.child, now).await;
    let events = jobs(&pool, f2.parent).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["action"], "event");
    assert!(jobs(&pool, f2.child).await.is_empty(), "no nudge needed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_question_in_the_text_is_not_nudged(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Parser is ready.\n\nShould I merge it?").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "ccc",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    assert!(jobs(&pool, f.child).await.is_empty());
    let events = jobs(&pool, f.parent).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["state"], "idle");
    assert_eq!(events[0]["asks_in_text"], true);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn background_suspected_defers_the_nudge(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 2, 10).await;
    let command = answer(&pool, &f, "Started the long build.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "ddd",
        false,
        &["bash -lc 'cargo test &'"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    let held = jobs(&pool, f.child).await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["signals"]["background_suspected"], true);
    // `run_at` не виден в payload; проверяем по сроку задания в базе.
    let (run_at, created): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
        "select run_at,created_at from scheduled_jobs where kind='session_task'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        (run_at - created).num_minutes() >= 9,
        "background grace wins over idle_minutes: {run_at} vs {created}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_is_delivered_with_origin_and_depth(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway through the parser.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "eee",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    let before: Vec<(String, Option<Value>)> =
        sqlx::query_as("select state,payload from scheduled_jobs where conversation_id=$1")
            .bind(f.child)
            .fetch_all(&pool)
            .await
            .unwrap();
    eprintln!("jobs before tick: {before:?}");
    eprintln!("rule: {:?}", rule_row(&pool, f.child).await);
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();

    let debug: Vec<(String, Option<Value>)> =
        sqlx::query_as("select state,payload from scheduled_jobs where conversation_id=$1")
            .bind(f.child)
            .fetch_all(&pool)
            .await
            .unwrap();
    eprintln!("jobs after tick: {debug:?}");
    let (text, origin): (Value, Option<Value>) = sqlx::query_as(
        "select content,origin from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    let text = text[0]["text"].as_str().unwrap();
    assert!(text.starts_with("[canterlot] Automatic reminder"));
    assert!(text.contains("STATUS: DONE"));
    assert!(text.contains("Observed:"));
    let origin = origin.unwrap();
    assert_eq!(origin["kind"], "nudge");
    assert_eq!(origin["chain_depth"], 0);
    assert_eq!(origin["attempt"], 1);
    assert_eq!(origin["max"], 3);
    assert_eq!(origin["signals"]["no_marker"], true);
    assert_eq!(rule_row(&pool, f.child).await, ("watching".into(), 1, 1));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn bounded_nudges_exhaust_and_release_the_held_result_once(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 1, 3, 10).await;
    let command = answer(&pool, &f, "Still working.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "fff",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    assert_eq!(rule_row(&pool, f.child).await, ("watching".into(), 1, 1));

    // Исполнитель принял толчок, ничего не сделал и снова встал без маркера.
    accept_inputs(&pool, f.child).await;
    let again = answer(&pool, &f, "Nothing changed.").await;
    finish_run(
        &pool,
        &f,
        again,
        now + TimeDelta::minutes(4),
        "fff",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(9))
        .await
        .unwrap();

    assert_eq!(rule_row(&pool, f.child).await.0, "exhausted");
    let events = jobs(&pool, f.parent).await;
    assert_eq!(events.len(), 1, "exactly one released result");
    assert_eq!(events[0]["nudge_exhausted"], true);
    assert_eq!(events[0]["nudges"], 1);

    // Повторный обход и доставка события не воскрешают удержание.
    refresh_at(&pool, f.child, now + TimeDelta::minutes(10)).await;
    assert_eq!(jobs(&pool, f.parent).await.len(), 1);
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(11))
        .await
        .unwrap();
    let inputs: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='event'",
    )
    .bind(f.parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inputs, 1, "the subscriber sees the result once");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn progress_resets_the_series(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 1, 3, 10).await;
    let command = answer(&pool, &f, "First attempt.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "111",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    accept_inputs(&pool, f.child).await;

    let next = answer(&pool, &f, "Committed the first half.").await;
    finish_run(
        &pool,
        &f,
        next,
        now + TimeDelta::minutes(4),
        "222",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(9))
        .await
        .unwrap();

    let (state, consecutive, total) = rule_row(&pool, f.child).await;
    assert_eq!(state, "watching", "a new HEAD is progress, not exhaustion");
    assert_eq!(consecutive, 1);
    assert_eq!(total, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn changed_fingerprint_cancels_the_pending_nudge(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Waiting on the gate.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "333",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    // Владелец написал исполнителю: отпечаток сменится, задание отменится.
    let mut tx = pool.begin().await.unwrap();
    deliver_owner_input(&mut tx, &f, "continue").await;
    tx.commit().await.unwrap();
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where state='pending'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(rule_row(&pool, f.child).await.0, "watching");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn permission_recheck_blocks_a_nudge_from_a_non_ancestor(pool: PgPool) {
    let f = fx(&pool).await;
    let stranger = testkit::seed_conversation(&pool, f.owner, f.machine).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, stranger, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "444",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();

    assert_eq!(rule_row(&pool, f.child).await.0, "exhausted");
    let events = jobs(&pool, f.parent).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["nudge_exhausted"], true);
    let owner_inputs: i64 =
        sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(owner_inputs, 0, "nothing was written into the executor");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn abandon_exhausts_and_releases_the_held_result(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "555",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    let fingerprint: String =
        sqlx::query_scalar("select held_fingerprint from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut tx = pool.begin().await.unwrap();
    jobs_abandon(
        &mut tx,
        f.child,
        &json!({"action":"nudge","fingerprint":fingerprint}),
        now + TimeDelta::minutes(1),
    )
    .await;
    tx.commit().await.unwrap();

    assert_eq!(rule_row(&pool, f.child).await.0, "exhausted");
    let events = jobs(&pool, f.parent).await;
    assert_eq!(events.len(), 1, "held result is released, not lost");
    let line: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='nudge_changed' and conversation_id=$1
           and payload->>'detail'='nudge_failed'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(line, 1, "the timeline says the nudge failed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn orchestrator_cannot_set_a_rule_on_a_non_descendant(pool: PgPool) {
    let f = fx(&pool).await;
    let stranger = testkit::seed_conversation(&pool, f.owner, f.machine).await;
    let ctx = context(&pool);
    let error = nudge::set(&ctx, &caller(&f), json!({"session_id": stranger}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ToolErrorCode::NotParent);

    let answer = nudge::set(
        &ctx,
        &caller(&f),
        json!({"session_id": f.child, "idle_minutes": 5, "max_nudges": 2}),
    )
    .await
    .unwrap();
    assert_eq!(answer["state"], "watching");
    assert_eq!(answer["rule"]["idle_minutes"], 5);
    assert_eq!(answer["rule"]["max_consecutive"], 2);

    // Выключение снимает правило и отпускает удержанное.
    let cleared = nudge::clear(&ctx, &caller(&f), json!({"session_id": f.child}))
        .await
        .unwrap();
    assert_eq!(cleared["state"], "cleared");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn restart_between_hold_and_nudge_does_not_duplicate_it(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "666",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(jobs(&pool, f.child).await.len(), 1);

    // «Перезапуск»: новый пул того же теста и новый планировщик.
    let restarted = pool.clone();
    scheduler(&restarted)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stalled_never_holds_and_never_nudges(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let mut tx = pool.begin().await.unwrap();
    let held = session_tools::completion::gate_in_tx(
        &mut tx,
        f.child,
        canterlot_hub::session_tools::settle::Settled::Stalled,
        "stalled|x|",
        now,
        true,
    )
    .await
    .unwrap();
    assert!(!held);
    tx.commit().await.unwrap();
    assert!(jobs(&pool, f.child).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn batch_and_watchdog_fire_once_after_the_marker(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    // Пачка и сторож взведены до правила; условие уже истинно, но аренда пустая.
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,condition,deadline_at,batch,call_id)
         values($1,$2,$3,'agent','condition',$4,now()+interval '1 day',true,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(f.owner)
    .bind(f.parent)
    .bind(json!({
        "kind":"session_state",
        "session_ids":[f.child],
        "states":["idle"],
        "all":true,
        "armed":{}
    }))
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let watchdog = Uuid::now_v7();
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,condition,deadline_at,only_on_timeout,call_id)
         values($1,$2,$3,'agent','condition',$4,now()+interval '2 hours',true,$5)",
    )
    .bind(watchdog)
    .bind(f.owner)
    .bind(f.parent)
    .bind(json!({
        "kind":"session_state",
        "session_ids":[f.child],
        "states":["idle"],
        "all":false,
        "armed":{}
    }))
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Still working.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "777",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert!(jobs(&pool, f.parent).await.is_empty(), "held result");

    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    accept_inputs(&pool, f.child).await;
    let done = answer(&pool, &f, "Finished.\nSTATUS: DONE").await;
    finish_run(
        &pool,
        &f,
        done,
        now + TimeDelta::minutes(4),
        "777",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;

    let deliveries: Vec<Value> = sqlx::query_scalar(
        "select payload from scheduled_jobs where kind='session_task' and conversation_id=$1
           and payload->>'action'='alarm_deliver'",
    )
    .bind(f.parent)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(deliveries.len(), 1, "the batch says goodbye once");
    let events: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where kind='session_task' and conversation_id=$1
           and payload->>'action'='event'",
    )
    .bind(f.parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        events, 0,
        "the batch owns the result, not the plain subscription"
    );
    let state: String = sqlx::query_scalar("select state from alarms where id=$1")
        .bind(watchdog)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        state, "cancelled",
        "the watchdog stays silent after the marker"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn permission_recheck_blocks_widened_modes_and_open_owner_requests(pool: PgPool) {
    // Режимы расширились после взвода: автор в read_only, цель в bypass.
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    sqlx::query("update conversations set permission_mode='read_only' where id=$1")
        .bind(f.parent)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set permission_mode='bypass' where id=$1")
        .bind(f.child)
        .execute(&pool)
        .await
        .unwrap();
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "888",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    assert_eq!(rule_row(&pool, f.child).await.0, "exhausted");
    assert_eq!(jobs(&pool, f.parent).await.len(), 1);

    // Открытый запрос владельцу любого вида блокирует отложенный толчок.
    let f2 = fx(&pool).await;
    subscribe(&pool, &f2).await;
    prime_idle(&pool, &f2, now).await;
    rule(&pool, &f2, f2.parent, 3, 3, 10).await;
    let command = answer(&pool, &f2, "Halfway.").await;
    finish_run(
        &pool,
        &f2,
        command,
        now - TimeDelta::minutes(1),
        "999",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f2.child, now).await;
    assert_eq!(rule_row(&pool, f2.child).await.0, "pending");
    // Запрос открылся уже после удержания: перепроверка перед толчком обязана сдаться.
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload)
         values($1,$2,$3,1,'permission','nudge-open','open','{}')",
    )
    .bind(Uuid::now_v7())
    .bind(f2.child)
    .bind(f2.execution.execution_id)
    .execute(&pool)
    .await
    .unwrap();
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    assert_eq!(rule_row(&pool, f2.child).await.0, "watching");
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f2.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0, "an owner request blocks the nudge");
    // Проектор видит открытый запрос и отдаёт подписчику waiting_for_user один раз.
    refresh_at(&pool, f2.child, now + TimeDelta::minutes(5)).await;
    assert_eq!(jobs(&pool, f2.parent).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stale_job_clears_the_hold_so_a_new_one_can_be_scheduled(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "aaa1",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    // Проекция сдвинулась до выполнения: отпечаток уже не тот, что в задании.
    sqlx::query(
        "update conversation_derived set activity_since=activity_since - interval '1 second'
         where conversation_id=$1",
    )
    .bind(f.child)
    .execute(&pool)
    .await
    .unwrap();
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    let (state, _, _) = rule_row(&pool, f.child).await;
    assert_eq!(state, "watching", "the stale hold is released, not stuck");
    let held: Option<String> =
        sqlx::query_scalar("select held_fingerprint from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(held.is_none());

    // Следующий обход снова удерживает итог и ставит новое задание.
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where state='pending'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn disabled_after_hold_gives_one_completion_and_no_nudge(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "dis1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    // Запуск хаба с выключателем: снимаем удержание и ожидающее задание.
    nudge::disable_all_on_startup(&pool, now + TimeDelta::minutes(1))
        .await
        .unwrap();
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where state='pending'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(jobs(&pool, f.parent).await.len(), 1, "held result released");

    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0);
    let completions: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='event'",
    )
    .bind(f.parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(completions, 1);

    // Задание, дожившее до выполнения с выключенным автопинком, тоже не толкает.
    let f2 = fx(&pool).await;
    subscribe(&pool, &f2).await;
    prime_idle(&pool, &f2, now).await;
    rule(&pool, &f2, f2.parent, 3, 3, 10).await;
    let command = answer(&pool, &f2, "Halfway.").await;
    finish_run(
        &pool,
        &f2,
        command,
        now - TimeDelta::minutes(1),
        "dis2",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f2.child, now).await;
    let disabled = scheduler(&pool);
    let disabled = disabled.with_settle(SettleConfig {
        nudge_enabled: false,
        ..Default::default()
    });
    disabled.tick(now + TimeDelta::minutes(4)).await.unwrap();
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f2.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0);
    assert_eq!(jobs(&pool, f2.parent).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_concurrent_settings_change_is_seen_before_the_nudge(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "mode1",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    // Настройки идут тем же порядком, что и в хабе: очередь, затем строки.
    let mut settings = pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.child)
        .execute(&mut *settings)
        .await
        .unwrap();
    sqlx::query("update conversations set permission_mode='read_only' where id=$1")
        .bind(f.parent)
        .execute(&mut *settings)
        .await
        .unwrap();
    sqlx::query("update conversations set permission_mode='bypass' where id=$1")
        .bind(f.child)
        .execute(&mut *settings)
        .await
        .unwrap();

    // Задание успевает прочитать старые режимы и встаёт в очередь за настройками.
    let pooled = pool.clone();
    let job = tokio::spawn(async move {
        scheduler(&pooled)
            .tick(now + TimeDelta::minutes(4))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    settings.commit().await.unwrap();
    job.await.unwrap();

    assert_eq!(
        rule_row(&pool, f.child).await.0,
        "exhausted",
        "the job must reread modes after taking the queue"
    );
    assert_eq!(jobs(&pool, f.parent).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_cancellation_does_not_wait_for_a_claimed_job(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "lock1",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    // Планировщик уже выбрал задание и держит его строку.
    let mut claimed = pool.begin().await.unwrap();
    sqlx::query(
        "select id from scheduled_jobs where state='pending' and conversation_id=$1
           and payload->>'action'='nudge' for update",
    )
    .bind(f.child)
    .execute(&mut *claimed)
    .await
    .unwrap();

    // Ввод владельца берёт очередь и отменяет ожидающие задания, не дожидаясь выбранного.
    let pooled = pool.clone();
    let child = f.child;
    let input = tokio::spawn(async move {
        let mut tx = pooled.begin().await.unwrap();
        sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
            .bind(child)
            .execute(&mut *tx)
            .await
            .unwrap();
        nudge::on_input_in_tx(&mut tx, child, Utc::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(2), input)
        .await
        .expect("input must not wait for a claimed job")
        .unwrap();
    claimed.commit().await.unwrap();

    assert_eq!(rule_row(&pool, f.child).await.0, "watching");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stop_between_hold_and_job_cancels_the_nudge_and_releases_once(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "stop1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    sqlx::query("update executions set state='stopped',outcome='user_stop' where id=$1")
        .bind(f.execution.execution_id)
        .execute(&pool)
        .await
        .unwrap();
    refresh_at(&pool, f.child, now + TimeDelta::minutes(1)).await;

    assert_eq!(rule_row(&pool, f.child).await.0, "paused");
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where state='pending'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
    let events = jobs(&pool, f.parent).await;
    assert_eq!(events.len(), 1, "one completion, not idle plus stopped");
    assert_eq!(events[0]["state"], "stopped");

    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accept_removes_the_rule_and_pending_nudges(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "acc1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;

    let mut tx = pool.begin().await.unwrap();
    session_tools::accept::begin_in_tx(&mut tx, f.owner, f.child, false, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let rules: i64 =
        sqlx::query_scalar("select count(*) from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rules, 0);
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where state='pending'
           and conversation_id=$1 and payload->>'action'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn release_waits_for_a_locked_subscription_instead_of_a_fallback_note(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "rel1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    let fingerprint: String =
        sqlx::query_scalar("select held_fingerprint from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();

    // Подписку держит чужая транзакция: отпускание должно дождаться её, а не
    // решить, что подписчиков нет, и не ставить заметку.
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("select id from session_subscriptions where target_id=$1 for update")
        .bind(f.child)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pooled = pool.clone();
    let child = f.child;
    let release = tokio::spawn(async move {
        let mut tx = pooled.begin().await.unwrap();
        session_tools::jobs::abandon_in_tx(
            &mut tx,
            Uuid::nil(),
            Some(child),
            &json!({"action":"nudge","fingerprint":fingerprint}),
            Utc::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    blocker.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), release)
        .await
        .expect("release completes after the lock")
        .unwrap();

    let jobs = jobs(&pool, f.parent).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["action"], "event", "no fallback note");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn two_busy_cpu_samples_after_the_turn_mark_background_work(pool: PgPool) {
    let f = fx(&pool).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 2, 10).await;
    let command = answer(&pool, &f, "Started the long build.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "cpu1",
        false,
        &[],
    )
    .await;

    let execution_id = f.execution.execution_id.to_string();
    for step in 0..2 {
        let metrics = MachineMetrics {
            machine_time_ms: now.timestamp_millis() as u64 + step + 1,
            cpu_cores: 4,
            executions: vec![ExecutionProcess {
                execution_id: execution_id.clone(),
                alive: true,
                cpu_permille: 200,
                rss_bytes: 1,
                descendants: Some(1),
            }],
            ..Default::default()
        };
        machine_info::apply_metrics(&pool, f.owner, f.machine, &metrics, Uuid::now_v7())
            .await
            .unwrap();
    }
    let (streak, first): (i32, Option<DateTime<Utc>>) =
        sqlx::query_as("select cpu_busy_streak,cpu_busy_first_at from executions where id=$1")
            .bind(f.execution.execution_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(streak, 2);
    assert!(first.is_some());

    refresh_at(&pool, f.child, now).await;
    let held = jobs(&pool, f.child).await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["signals"]["background_suspected"], true);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_nudged_summary_keeps_the_substantive_report(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    sqlx::query(
        "update session_subscriptions set armed_at=clock_timestamp()-interval '2 hours'
         where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.parent)
    .bind(f.child)
    .execute(&pool)
    .await
    .unwrap();
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let report = format!("DETAILED-REPORT-MARKER\n{}", "r".repeat(700));
    let first = answer(&pool, &f, &report).await;
    finish_run(
        &pool,
        &f,
        first,
        now - TimeDelta::minutes(1),
        "sum1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    accept_inputs(&pool, f.child).await;

    let done = answer(&pool, &f, "STATUS: DONE").await;
    finish_run(
        &pool,
        &f,
        done,
        now + TimeDelta::minutes(4),
        "sum1",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(6))
        .await
        .unwrap();

    let text: String = sqlx::query_scalar(
        "select content->0->>'text' from inputs where conversation_id=$1
           and origin->>'kind'='event' order by input_seq desc limit 1",
    )
    .bind(f.parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(text.contains("Auto-nudge: 1 nudge"), "{text}");
    assert!(text.contains("Answer before the nudges"), "{text}");
    assert!(text.contains("DETAILED-REPORT-MARKER"), "{text}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_batched_summary_keeps_nudge_metadata_and_the_report(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    sqlx::query(
        "update session_subscriptions set armed_at=clock_timestamp()-interval '2 hours'
         where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.parent)
    .bind(f.child)
    .execute(&pool)
    .await
    .unwrap();
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,condition,deadline_at,batch,call_id)
         values($1,$2,$3,'agent','condition',$4,now()+interval '1 day',true,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(f.owner)
    .bind(f.parent)
    .bind(json!({
        "kind":"session_state",
        "session_ids":[f.child],
        "states":["idle"],
        "all":true,
        "armed":{}
    }))
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let report = format!("BATCH-REPORT-MARKER\n{}", "b".repeat(700));
    let first = answer(&pool, &f, &report).await;
    finish_run(
        &pool,
        &f,
        first,
        now - TimeDelta::minutes(1),
        "sum2",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    accept_inputs(&pool, f.child).await;

    let done = answer(&pool, &f, "STATUS: DONE").await;
    finish_run(
        &pool,
        &f,
        done,
        now + TimeDelta::minutes(4),
        "sum2",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now + TimeDelta::minutes(5)).await;
    let dbg: Vec<(String, Option<Value>)> =
        sqlx::query_as("select state,payload from scheduled_jobs where conversation_id=$1")
            .bind(f.parent)
            .fetch_all(&pool)
            .await
            .unwrap();
    eprintln!("batch jobs: {dbg:?}");
    let alarm: Vec<(String, Value)> = sqlx::query_as("select state,collected from alarms")
        .fetch_all(&pool)
        .await
        .unwrap();
    eprintln!("alarms: {alarm:?}");
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(6))
        .await
        .unwrap();

    let text: String = sqlx::query_scalar(
        "select content->0->>'text' from inputs where conversation_id=$1
         order by input_seq desc limit 1",
    )
    .bind(f.parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(text.contains("Auto-nudge: 1 nudge"), "{text}");
    assert!(text.contains("Answer before the nudges"), "{text}");
    assert!(text.contains("BATCH-REPORT-MARKER"), "{text}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_concurrent_author_read_only_change_blocks_the_nudge(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "mode2",
        false,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    // Смена настроек автора идёт под очередью автора; цель остаётся свободной.
    let mut settings = pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.parent)
        .execute(&mut *settings)
        .await
        .unwrap();
    sqlx::query("update conversations set permission_mode='read_only' where id=$1")
        .bind(f.parent)
        .execute(&mut *settings)
        .await
        .unwrap();
    sqlx::query("update conversations set permission_mode='bypass' where id=$1")
        .bind(f.child)
        .execute(&mut *settings)
        .await
        .unwrap();

    let pooled = pool.clone();
    let job = tokio::spawn(async move {
        scheduler(&pooled)
            .tick(now + TimeDelta::minutes(4))
            .await
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    settings.commit().await.unwrap();
    job.await.unwrap();

    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0, "author became read_only before the nudge");
    assert_eq!(rule_row(&pool, f.child).await.0, "exhausted");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn release_and_subscriber_deletion_complete_with_one_outcome(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let other = testkit::seed_conversation(&pool, f.owner, f.machine).await;
    let mut tx = pool.begin().await.unwrap();
    subs::subscribe_in_tx(&mut tx, f.owner, other, f.child, 0)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "del1",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    let fingerprint: String =
        sqlx::query_scalar("select held_fingerprint from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();

    // Удаление второго подписчика держит его строку разговора и затем удаляет подписку.
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("select id from conversations where id=$1 for update")
        .bind(other)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pooled = pool.clone();
    let child = f.child;
    let release = tokio::spawn(async move {
        let mut tx = pooled.begin().await.unwrap();
        session_tools::jobs::abandon_in_tx(
            &mut tx,
            Uuid::nil(),
            Some(child),
            &json!({"action":"nudge","fingerprint":fingerprint}),
            Utc::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    sqlx::query("delete from session_subscriptions where subscriber_id=$1 and target_id=$2")
        .bind(other)
        .bind(f.child)
        .execute(&mut *blocker)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), release)
        .await
        .expect("release waits, not deadlocks")
        .unwrap();

    let jobs = jobs(&pool, f.parent).await;
    assert_eq!(jobs.len(), 1, "one outcome for the live subscriber");
    assert_eq!(jobs[0]["action"], "event");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn disabled_refresh_before_cleanup_reports_once(pool: PgPool) {
    let f = fx(&pool).await;
    subscribe(&pool, &f).await;
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    rule(&pool, &f, f.parent, 3, 3, 10).await;
    let command = answer(&pool, &f, "Halfway.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "dis3",
        true,
        &[],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    assert_eq!(rule_row(&pool, f.child).await.0, "pending");

    // HTTP-обновление с выключателем успевает раньше уборки на старте.
    activity::refresh_with_metrics(
        &pool,
        f.child,
        now + TimeDelta::seconds(1),
        &thresholds(),
        Duration::from_secs(60),
        false,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let held: Option<String> =
        sqlx::query_scalar("select held_fingerprint from nudge_rules where conversation_id=$1")
            .bind(f.child)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(held.is_none(), "the gate consumes the hold when disabled");
    assert_eq!(jobs(&pool, f.parent).await.len(), 1);

    nudge::disable_all_on_startup(&pool, now + TimeDelta::minutes(1))
        .await
        .unwrap();
    assert_eq!(
        jobs(&pool, f.parent).await.len(),
        1,
        "cleanup must not add a fallback note"
    );

    scheduler(&pool)
        .tick(now + TimeDelta::minutes(2))
        .await
        .unwrap();
    let reports: i64 = sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(f.parent)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(reports, 1, "exactly one report");
}

// Доставка ввода владельца — тот же путь, что у обычного сообщения.
async fn deliver_owner_input(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, f: &Fx, text: &str) {
    canterlot_hub::delivery::submit_input_in_tx(
        tx,
        &canterlot_hub::delivery::SubmitInput {
            owner_id: f.owner,
            conversation_id: f.child,
            idempotency_key: format!("owner:{}", Uuid::now_v7()),
            content: vec![canterlot_hub::delivery::ContentBlock::Text {
                text: text.to_owned(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
}

async fn jobs_abandon(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    child: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
) {
    assert!(
        !session_tools::jobs::abandon_in_tx(tx, Uuid::nil(), Some(child), payload, now)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nudge_text_comes_from_a_nudge_preset_and_set_rejects_instructions(pool: PgPool) {
    let f = fx(&pool).await;
    let custom = canterlot_hub::presets::create(
        &pool,
        f.owner,
        "nudge_preset_text",
        canterlot_hub::presets::NewPreset {
            title: "Свой толчок".into(),
            description: None,
            text: "[canterlot] PRESET-TEXT {markers} | {signals}".into(),
            purpose: canterlot_hub::presets::PresetPurpose::Nudge,
        },
        canterlot_hub::presets::Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let instruction = canterlot_hub::presets::create(
        &pool,
        f.owner,
        "nudge_instruction_text",
        canterlot_hub::presets::NewPreset {
            title: "Обычная инструкция".into(),
            description: None,
            text: "обычный текст".into(),
            purpose: canterlot_hub::presets::PresetPurpose::Instructions,
        },
        canterlot_hub::presets::Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let ctx = context(&pool);
    let error = nudge::set(
        &ctx,
        &caller(&f),
        json!({"session_id": f.child, "text_preset": instruction.summary.id}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ToolErrorCode::ProfileUnknown);

    let set_answer = nudge::set(
        &ctx,
        &caller(&f),
        json!({"session_id": f.child, "text_preset": custom.summary.id}),
    )
    .await
    .unwrap();
    assert_eq!(set_answer["state"], "watching");
    let now = Utc::now();
    prime_idle(&pool, &f, now).await;
    let command = answer(&pool, &f, "Halfway through the parser.").await;
    finish_run(
        &pool,
        &f,
        command,
        now - TimeDelta::minutes(1),
        "eee",
        true,
        &["bash"],
    )
    .await;
    refresh_at(&pool, f.child, now).await;
    scheduler(&pool)
        .tick(now + TimeDelta::minutes(4))
        .await
        .unwrap();
    let text: Value = sqlx::query_scalar(
        "select content from inputs where conversation_id=$1 order by input_seq desc limit 1",
    )
    .bind(f.child)
    .fetch_one(&pool)
    .await
    .unwrap();
    let text = text[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("[canterlot] PRESET-TEXT STATUS: DONE or STATUS: BLOCKED"),
        "{text}"
    );
    assert!(text.contains("| idle"), "{text}");
}
