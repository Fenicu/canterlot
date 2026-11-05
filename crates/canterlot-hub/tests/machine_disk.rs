//! Нехватка места на машине: подписчики незавершённых сессий слышат предупреждение
//! и критический уровень по одному разу за эпизод; возврат места молчит.

use canterlot_hub::{
    config::MachinesConfig, dispatcher::Dispatcher, scheduler::Scheduler, session_tools::disk,
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const GB: u64 = 1_000_000_000;

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

async fn set_disk(pool: &PgPool, machine: Uuid, free: u64, total: u64, observed_at: DateTime<Utc>) {
    sqlx::query(
        "insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,
           mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,
           disks,executions,observed_at)
         values($1,0,1,0,$2,$2,0,0,0,$3,'[]',$4)
         on conflict(machine_id) do update set
           mem_total_bytes=excluded.mem_total_bytes,
           mem_available_bytes=excluded.mem_available_bytes,
           disks=excluded.disks,
           observed_at=excluded.observed_at",
    )
    .bind(machine)
    .bind((total / 2) as i64)
    .bind(json!([{"label": "data", "total_bytes": total, "free_bytes": free}]))
    .bind(observed_at)
    .execute(pool)
    .await
    .unwrap();
}

fn config() -> MachinesConfig {
    MachinesConfig {
        disk_warn_free_gb: 30,
        disk_critical_free_gb: 10,
        ..MachinesConfig::default()
    }
}

async fn sweep(pool: &PgPool, now: DateTime<Utc>) -> disk::SweepReport {
    disk::sweep(pool, &config(), now).await.unwrap()
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

async fn episodes(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from machine_disk_episodes")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn open_episodes(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from machine_disk_episodes where ended_at is null")
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
async fn low_disk_warns_every_subscriber_once_then_critical_once(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let second = conversation(&pool, f.owner, f.machine, "второй подписчик").await;
    subscribe(&pool, f.owner, second, f.target).await;
    let other = conversation(&pool, f.owner, f.machine, "второй делегат").await;
    subscribe(&pool, f.owner, f.subscriber, other).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;

    let report = sweep(&pool, now).await;
    assert_eq!(report.opened, 1);
    assert_eq!(report.warned, 3);
    assert_eq!(report.critical, 0);
    assert_eq!(job_count(&pool).await, 3);
    assert_eq!(episodes(&pool).await, 1);

    // Повторный обход того же эпизода ничего не добавляет.
    assert_eq!(sweep(&pool, now + TimeDelta::seconds(5)).await.warned, 0);
    assert_eq!(job_count(&pool).await, 3);

    // Место упало ниже критического: по одному второму событию на подписку.
    let later = now + TimeDelta::seconds(10);
    set_disk(&pool, f.machine, 8 * GB, 295 * GB, later).await;
    let report = sweep(&pool, later).await;
    assert_eq!(report.critical, 3);
    assert_eq!(report.warned, 0);
    assert_eq!(job_count(&pool).await, 6);
    assert_eq!(
        sweep(&pool, later + TimeDelta::seconds(5)).await.critical,
        0
    );

    let first = jobs(&pool, f.subscriber).await;
    assert_eq!(first.len(), 4, "two targets on the same machine");
    assert_eq!(
        first.iter().filter(|job| job["kind"] == "disk_low").count(),
        2
    );
    assert_eq!(
        first
            .iter()
            .filter(|job| job["kind"] == "disk_critical")
            .count(),
        2
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn restart_during_the_episode_does_not_repeat_the_event(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;
    sweep(&pool, now).await;
    assert_eq!(job_count(&pool).await, 1);

    // Хаб перезапустился: память пуста, но эпизод и отметка подписки в базе.
    let later = now + TimeDelta::minutes(2);
    set_disk(&pool, f.machine, 23 * GB, 295 * GB, later).await;
    assert_eq!(sweep(&pool, later).await.warned, 0);
    assert_eq!(job_count(&pool).await, 1);
    assert_eq!(open_episodes(&pool).await, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_above_warn_plus_hysteresis_resets_silently(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;
    sweep(&pool, now).await;
    assert_eq!(job_count(&pool).await, 1);

    // 34 ГБ: выше порога, но ниже порога с запасом (35 ГБ) — эпизод ещё открыт.
    let later = now + TimeDelta::seconds(10);
    set_disk(&pool, f.machine, 34 * GB, 295 * GB, later).await;
    let report = sweep(&pool, later).await;
    assert_eq!(report.recovered, 0);
    assert_eq!(open_episodes(&pool).await, 1);
    assert_eq!(job_count(&pool).await, 1);

    // 36 ГБ: возврат с запасом закрывает эпизод и не шлёт сообщения.
    let recovered = later + TimeDelta::seconds(10);
    set_disk(&pool, f.machine, 36 * GB, 295 * GB, recovered).await;
    let report = sweep(&pool, recovered).await;
    assert_eq!(report.recovered, 1);
    assert_eq!(open_episodes(&pool).await, 0);
    assert_eq!(job_count(&pool).await, 1);

    // Новое падение открывает новый эпизод и снова одно событие.
    let again = recovered + TimeDelta::seconds(10);
    set_disk(&pool, f.machine, 20 * GB, 295 * GB, again).await;
    let report = sweep(&pool, again).await;
    assert_eq!(report.opened, 1);
    assert_eq!(report.warned, 1);
    assert_eq!(episodes(&pool).await, 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_sessions_do_not_hear_about_low_disk(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let idle = conversation(&pool, f.owner, f.machine, "живая").await;
    subscribe(&pool, f.owner, f.subscriber, idle).await;
    derive(&pool, f.target, "stopped", json!({"outcome": "user_stop"})).await;
    derive(&pool, idle, "idle", json!({"last_turn_at": null})).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;

    assert_eq!(sweep(&pool, now).await.warned, 1);
    let all = jobs(&pool, f.subscriber).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["target_id"], json!(idle));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_metrics_do_not_open_an_episode(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, GB, 295 * GB, now - TimeDelta::minutes(10)).await;
    let report = sweep(&pool, now).await;
    assert_eq!(report.opened, 0);
    assert_eq!(episodes(&pool).await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delivered_text_names_the_machine_free_gb_and_percent(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;
    sweep(&pool, now).await;
    scheduler(&pool).tick(now).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(inputs.len(), 1);
    let text = &inputs[0].0;
    assert_eq!(
        text,
        "[canterlot] Machine \"synthetic\" is low on disk: 24 GB free (8%). \
         Builds and new sessions may fail."
    );
    assert_eq!(inputs[0].1["event"]["kind"], "machine_disk_low");
    Ok(())
}
