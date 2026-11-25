use std::time::Duration;

use canterlot_hub::{
    activity::{self, Thresholds},
    config::SchedulerConfig,
    dispatcher::Dispatcher,
    executions,
    model::ExecutionState,
    scheduler::Scheduler,
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        hung_rescue: canterlot_hub::config::HungRescue::Auto,
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
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
    answer_as(pool, f, text, "main").await
}

async fn answer_as(pool: &PgPool, f: &Fx, text: &str, agent: &str) -> Uuid {
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
         values($1,$2,$3,'text','final',1,$4,$5,$6,'m',(select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),clock_timestamp()-interval '10 minutes',clock_timestamp()-interval '10 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(f.execution.execution_id)
    .bind(f.child)
    .bind(text)
    .bind(command)
    .bind(agent)
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

fn scheduler(pool: &PgPool) -> Scheduler {
    Scheduler::new(
        pool.clone(),
        Dispatcher::channel().0,
        SchedulerConfig::default(),
    )
}

async fn rule_row(pool: &PgPool, id: Uuid) -> (String, i32, i32) {
    sqlx::query_as("select state,consecutive,total from nudge_rules where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn nudge_is_delivered_with_origin_and_depth() {
    let pool = canterlot_hub::testkit::db().await;
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

// Доставка ввода владельца — тот же путь, что у обычного сообщения.
