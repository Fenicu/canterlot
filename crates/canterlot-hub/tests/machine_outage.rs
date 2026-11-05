//! Машина пропала дольше грации: подписчики сессий на ней слышат это один раз и один раз
//! о возврате. Событие идёт заданием `session_task`, как обычный итог подписки.

use canterlot_hub::{
    config::SessionToolsConfig, dispatcher::Dispatcher, scheduler::Scheduler,
    session_tools::outage, testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Fx {
    owner: Uuid,
    machine: Uuid,
    subscriber: Uuid,
    target: Uuid,
}

async fn conversation(pool: &PgPool, owner: Uuid, machine: Uuid, title: &str) -> Uuid {
    let id = testkit::seed_conversation(pool, owner, machine).await;
    sqlx::query("update conversations set title=$2 where id=$1")
        .bind(id)
        .bind(title)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn subscribe(pool: &PgPool, owner: Uuid, subscriber: Uuid, target: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id,chain_depth)
         values($1,$2,$3,$4,0)",
    )
    .bind(id)
    .bind(owner)
    .bind(subscriber)
    .bind(target)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn fx(pool: &PgPool) -> Fx {
    let seed = testkit::seed(pool).await;
    let target = conversation(pool, seed.owner_id, seed.machine_id, "делегат").await;
    subscribe(pool, seed.owner_id, seed.conversation_id, target).await;
    Fx {
        owner: seed.owner_id,
        machine: seed.machine_id,
        subscriber: seed.conversation_id,
        target,
    }
}

async fn set_availability(pool: &PgPool, machine: Uuid, state: &str, at: DateTime<Utc>) {
    sqlx::query("update machines set availability=$2,availability_at=$3 where id=$1")
        .bind(machine)
        .bind(state)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

async fn sweep(pool: &PgPool, now: DateTime<Utc>) -> outage::SweepReport {
    outage::sweep(pool, &SessionToolsConfig::default(), now)
        .await
        .unwrap()
}

async fn jobs(pool: &PgPool, subscriber: Uuid) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from scheduled_jobs
         where kind='session_task' and payload->>'action'='machine_event' and conversation_id=$1
         order by id",
    )
    .bind(subscriber)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn job_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "select count(*) from scheduled_jobs where payload->>'action'='machine_event'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn outage_rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from machine_outages")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn derive(pool: &PgPool, id: Uuid, state: &str, evidence: Value) {
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence)
         values($1,$2,$3)
         on conflict(conversation_id) do update
         set activity_state=excluded.activity_state,activity_evidence=excluded.activity_evidence",
    )
    .bind(id)
    .bind(state)
    .bind(evidence)
    .execute(pool)
    .await
    .unwrap();
}

fn scheduler(pool: &PgPool) -> Scheduler {
    Scheduler::new(pool.clone(), Dispatcher::channel().0, Default::default())
}

async fn delivered(pool: &PgPool, conversation: Uuid) -> Vec<(String, Value)> {
    sqlx::query_as::<_, (Value, Option<Value>)>(
        "select content,origin from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(conversation)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(content, origin)| {
        (
            content[0]["text"].as_str().unwrap_or_default().to_owned(),
            origin.unwrap_or(Value::Null),
        )
    })
    .collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn outage_beyond_grace_notifies_every_subscriber_once_per_session(
    pool: PgPool,
) -> TestResult {
    let f = fx(&pool).await;
    let second = conversation(&pool, f.owner, f.machine, "второй подписчик").await;
    subscribe(&pool, f.owner, second, f.target).await;
    let other = conversation(&pool, f.owner, f.machine, "второй делегат").await;
    subscribe(&pool, f.owner, f.subscriber, other).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;

    let report = sweep(&pool, now).await;
    assert_eq!(report.opened, 1);
    assert_eq!(report.announced, 3);
    assert_eq!(job_count(&pool).await, 3);
    assert_eq!(outage_rows(&pool).await, 1);

    // Повторный обход того же простоя ничего не добавляет.
    assert_eq!(sweep(&pool, now + TimeDelta::seconds(5)).await.announced, 0);
    assert_eq!(job_count(&pool).await, 3);

    let first = jobs(&pool, f.subscriber).await;
    assert_eq!(first.len(), 2, "two targets on the same machine");
    assert!(first.iter().all(|job| job["kind"] == "unavailable"));
    assert!(first.iter().any(|job| job["target_id"] == json!(f.target)));

    // Доставка идёт обычным заданием подписки и говорит, что состояние неизвестно.
    scheduler(&pool).tick(now).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(inputs.len(), 2);
    let text = inputs
        .iter()
        .map(|(text, _)| text.as_str())
        .find(|text| text.contains(&format!("id {}", f.target)))
        .expect("target message");
    assert!(text.contains("Machine \"synthetic\""), "{text}");
    assert!(text.contains("is unavailable for 10 min"), "{text}");
    assert!(text.contains(&format!("id {}", f.target)), "{text}");
    assert!(text.contains("state is unknown"), "{text}");
    assert!(
        text.contains("may resume when the machine reconnects"),
        "{text}"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn outage_shorter_than_grace_is_silent(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_availability(&pool, f.machine, "unavailable", now - TimeDelta::minutes(1)).await;

    assert_eq!(sweep(&pool, now).await.opened, 0);
    assert_eq!(job_count(&pool).await, 0);
    assert_eq!(outage_rows(&pool).await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn restart_during_outage_does_not_repeat_the_event(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;
    sweep(&pool, now).await;
    assert_eq!(job_count(&pool).await, 1);

    // Хаб перезапустился: память пуста, но открытый простой и отметки подписки в базе.
    let later = now + TimeDelta::minutes(2);
    assert_eq!(sweep(&pool, later).await.announced, 0);
    assert_eq!(job_count(&pool).await, 1);
    assert_eq!(outage_rows(&pool).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconnect_sends_one_available_event_with_the_current_state(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;
    sweep(&pool, now).await;
    assert_eq!(job_count(&pool).await, 1);

    derive(&pool, f.target, "failed", json!({"reason": "owner_lost"})).await;
    set_availability(&pool, f.machine, "available", now).await;

    let report = sweep(&pool, now + TimeDelta::seconds(5)).await;
    assert_eq!(report.recovered, 1);
    let all = jobs(&pool, f.subscriber).await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[1]["kind"], "available");

    assert_eq!(
        sweep(&pool, now + TimeDelta::seconds(10)).await.recovered,
        0
    );
    assert_eq!(jobs(&pool, f.subscriber).await.len(), 2);

    scheduler(&pool).tick(now + TimeDelta::seconds(10)).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(inputs.len(), 2);
    let text = &inputs[1].0;
    assert!(
        text.contains("Machine \"synthetic\" is available again"),
        "{text}"
    );
    assert!(text.contains("делегат"), "{text}");
    assert!(text.contains("is now failed (owner_lost)"), "{text}");
    assert_eq!(inputs[1].1["event"]["kind"], "machine_available");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_sessions_are_not_told_about_the_outage(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let idle = conversation(&pool, f.owner, f.machine, "живая").await;
    subscribe(&pool, f.owner, f.subscriber, idle).await;
    derive(&pool, f.target, "stopped", json!({"outcome": "user_stop"})).await;
    derive(&pool, idle, "idle", json!({"last_turn_at": null})).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;

    assert_eq!(sweep(&pool, now).await.announced, 1);
    let all = jobs(&pool, f.subscriber).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["target_id"], json!(idle));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_execution_still_hears_about_the_outage(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    derive(&pool, f.target, "failed", json!({"reason": "owner_lost"})).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;

    assert_eq!(sweep(&pool, now).await.announced, 1);
    assert_eq!(jobs(&pool, f.subscriber).await.len(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn outages_of_two_machines_stay_isolated(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let other_machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'other')")
        .bind(other_machine)
        .bind(f.owner)
        .execute(&pool)
        .await?;
    let other_target = conversation(&pool, f.owner, other_machine, "чужая").await;
    let other_subscriber = conversation(&pool, f.owner, other_machine, "чужой подписчик").await;
    subscribe(&pool, f.owner, other_subscriber, other_target).await;

    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;
    set_availability(
        &pool,
        other_machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;

    assert_eq!(sweep(&pool, now).await.announced, 2);
    let mine = jobs(&pool, f.subscriber).await;
    let theirs = jobs(&pool, other_subscriber).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(theirs.len(), 1);
    assert_eq!(mine[0]["target_id"], json!(f.target));
    assert_eq!(theirs[0]["target_id"], json!(other_target));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unavailable_event_waits_for_a_new_subscriber_during_the_same_outage(
    pool: PgPool,
) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_availability(
        &pool,
        f.machine,
        "unavailable",
        now - TimeDelta::minutes(10),
    )
    .await;
    sweep(&pool, now).await;
    assert_eq!(job_count(&pool).await, 1);

    let newcomer = conversation(&pool, f.owner, f.machine, "новичок").await;
    subscribe(&pool, f.owner, f.subscriber, newcomer).await;
    assert_eq!(sweep(&pool, now + TimeDelta::seconds(5)).await.announced, 1);
    let all = jobs(&pool, f.subscriber).await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[1]["target_id"], json!(newcomer));
    Ok(())
}
