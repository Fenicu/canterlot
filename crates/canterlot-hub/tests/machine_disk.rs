//! Сторож машины: нехватка места, памяти и пережившие сессии процессы. Подписчики
//! незавершённых сессий и действующие оркестраторы на машине слышат каждое событие
//! по одному разу за эпизод; возврат к норме молчит.

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
async fn low_disk_warns_every_recipient_once_then_critical_once(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let second = conversation(&pool, f.owner, f.machine, "второй подписчик").await;
    subscribe(&pool, f.owner, second, f.target).await;
    let other = conversation(&pool, f.owner, f.machine, "второй делегат").await;
    subscribe(&pool, f.owner, f.subscriber, other).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;

    // Получатель слышит машину один раз, сколько бы его целей на ней ни было.
    let report = sweep(&pool, now).await;
    assert_eq!(report.opened, 1);
    assert_eq!(report.warned, 2);
    assert_eq!(report.critical, 0);
    assert_eq!(job_count(&pool).await, 2);
    assert_eq!(episodes(&pool).await, 1);

    // Повторный обход того же эпизода ничего не добавляет.
    assert_eq!(sweep(&pool, now + TimeDelta::seconds(5)).await.warned, 0);
    assert_eq!(job_count(&pool).await, 2);

    // Место упало ниже критического: по одному второму событию на получателя.
    let later = now + TimeDelta::seconds(10);
    set_disk(&pool, f.machine, 8 * GB, 295 * GB, later).await;
    let report = sweep(&pool, later).await;
    assert_eq!(report.critical, 2);
    assert_eq!(report.warned, 0);
    assert_eq!(job_count(&pool).await, 4);
    assert_eq!(
        sweep(&pool, later + TimeDelta::seconds(5)).await.critical,
        0
    );

    let first = jobs(&pool, f.subscriber).await;
    assert_eq!(first.len(), 2);
    assert_eq!(first[0]["kind"], "disk_low");
    assert_eq!(first[1]["kind"], "disk_critical");
    // Подлежащее — машина, цель — сам получатель.
    assert_eq!(first[0]["machine_id"], json!(f.machine));
    assert_eq!(first[0]["target_id"], json!(f.subscriber));
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

    assert_eq!(
        sweep(&pool, now).await.warned,
        1,
        "the live target keeps its subscriber"
    );
    let all = jobs(&pool, f.subscriber).await;
    assert_eq!(all.len(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscriber_of_only_terminal_sessions_hears_nothing(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    derive(&pool, f.target, "stopped", json!({"outcome": "user_stop"})).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 24 * GB, 295 * GB, now).await;
    assert_eq!(sweep(&pool, now).await.warned, 0);
    assert_eq!(job_count(&pool).await, 0);
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

async fn child_of(pool: &PgPool, parent: Uuid, child: Uuid, created_at: DateTime<Utc>) {
    sqlx::query("update conversations set parent_conversation_id=$2,created_at=$3 where id=$1")
        .bind(child)
        .bind(parent)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
}

async fn derive_since(pool: &PgPool, id: Uuid, state: &str, since: DateTime<Utc>) {
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence,activity_since)
         values($1,$2,'{}',$3)
         on conflict(conversation_id) do update
         set activity_state=excluded.activity_state,activity_since=excluded.activity_since",
    )
    .bind(id)
    .bind(state)
    .bind(since)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_watch(
    pool: &PgPool,
    owner: Uuid,
    machine: Uuid,
    hotspots: Value,
    leaked: Value,
    at: DateTime<Utc>,
) {
    sqlx::query(
        "insert into machine_watch(machine_id,owner_id,hotspots,leaked,unleased_count,observed_at)
         values($1,$2,$3,$4,0,$5)
         on conflict(machine_id) do update set hotspots=excluded.hotspots,leaked=excluded.leaked,
           observed_at=excluded.observed_at",
    )
    .bind(machine)
    .bind(owner)
    .bind(hotspots)
    .bind(leaked)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

async fn set_mem(
    pool: &PgPool,
    machine: Uuid,
    available: u64,
    executions: Value,
    at: DateTime<Utc>,
) {
    sqlx::query(
        "insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,
           mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,
           disks,executions,observed_at)
         values($1,0,1,0,$2,$3,0,0,0,$4,$5,$6)
         on conflict(machine_id) do update set
           mem_available_bytes=excluded.mem_available_bytes,
           executions=excluded.executions,
           observed_at=excluded.observed_at",
    )
    .bind(machine)
    .bind((64 * GB) as i64)
    .bind(available as i64)
    .bind(json!([{"label": "data", "total_bytes": 295 * GB, "free_bytes": 200 * GB}]))
    .bind(executions)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

async fn execution(pool: &PgPool, conversation: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'stopped')",
    )
    .bind(id)
    .bind(conversation)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn recipients_of(pool: &PgPool) -> Vec<Uuid> {
    sqlx::query_scalar(
        "select conversation_id from scheduled_jobs
         where payload->>'action'='machine_event' order by conversation_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn orchestrators_on_the_machine_hear_it_unless_they_went_stale(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let (owner, machine) = (seed.owner_id, seed.machine_id);
    // Работающий оркестратор с делегатом, давно законченным: слышит.
    let working = conversation(&pool, owner, machine, "работает").await;
    let old_child = conversation(&pool, owner, machine, "старый делегат").await;
    child_of(&pool, working, old_child, now - TimeDelta::days(3)).await;
    derive(&pool, old_child, "stopped", json!({})).await;
    derive_since(&pool, working, "working", now - TimeDelta::days(2)).await;
    // Простаивающий, но запускавший делегата сегодня: слышит.
    let recent = conversation(&pool, owner, machine, "недавний").await;
    let fresh_child = conversation(&pool, owner, machine, "свежий делегат").await;
    child_of(&pool, recent, fresh_child, now - TimeDelta::hours(2)).await;
    derive(&pool, fresh_child, "stopped", json!({})).await;
    derive_since(&pool, recent, "idle", now - TimeDelta::days(2)).await;
    // Простаивающий, но закончивший ход сегодня: слышит.
    let turned = conversation(&pool, owner, machine, "ход сегодня").await;
    let turned_child = conversation(&pool, owner, machine, "делегат хода").await;
    child_of(&pool, turned, turned_child, now - TimeDelta::days(5)).await;
    derive(&pool, turned_child, "stopped", json!({})).await;
    derive_since(&pool, turned, "idle", now - TimeDelta::hours(1)).await;
    // Забытый оркестратор: простаивает двое суток, делегат старый — не слышит.
    let stale = conversation(&pool, owner, machine, "забытый").await;
    let stale_child = conversation(&pool, owner, machine, "забытый делегат").await;
    child_of(&pool, stale, stale_child, now - TimeDelta::days(4)).await;
    derive(&pool, stale_child, "stopped", json!({})).await;
    derive_since(&pool, stale, "idle", now - TimeDelta::days(2)).await;
    // Сессия без делегатов — не оркестратор.
    let plain = conversation(&pool, owner, machine, "обычная").await;
    derive_since(&pool, plain, "working", now).await;

    set_disk(&pool, machine, 24 * GB, 295 * GB, now).await;
    let report = sweep(&pool, now).await;
    assert_eq!(report.warned, 3);
    let mut expected = vec![working, recent, turned];
    expected.sort();
    assert_eq!(recipients_of(&pool).await, expected);

    // Отметки не дают второго события тому же получателю, даже когда он стал
    // ещё и подписчиком сессии на машине.
    subscribe(&pool, owner, working, plain).await;
    assert_eq!(sweep(&pool, now + TimeDelta::seconds(5)).await.warned, 0);
    let marks: i64 = sqlx::query_scalar("select count(*) from machine_event_marks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(marks, 3);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn disk_text_names_the_largest_build_caches(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let now = Utc::now();
    let worktree = conversation(&pool, f.owner, f.machine, "Баннер связи").await;
    let at = |h: i64| (now - TimeDelta::hours(h)).timestamp_millis();
    set_watch(
        &pool,
        f.owner,
        f.machine,
        json!([
            {"path": "/home/o/canterlot-2a/target", "kind": "target", "bytes": 77 * GB,
             "incremental_bytes": 31 * GB, "newest_ms": at(2), "measured_at_ms": at(0),
             "conversation_id": null},
            {"path": format!("/data/worktrees/{worktree}/target"), "kind": "target",
             "bytes": 12 * GB, "incremental_bytes": 4 * GB, "newest_ms": at(5),
             "measured_at_ms": at(0), "conversation_id": worktree.to_string()}
        ]),
        json!([]),
        now,
    )
    .await;
    set_disk(&pool, f.machine, 28 * GB, 295 * GB, now).await;
    sweep(&pool, now).await;
    let job = &jobs(&pool, f.subscriber).await[0];
    assert_eq!(job["details"]["hotspots"][1]["title"], "Баннер связи");
    scheduler(&pool).tick(now).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(
        inputs[0].0,
        "[canterlot] Machine \"synthetic\" is low on disk: 28 GB free (9%). Builds and new \
         sessions may fail. Largest build caches: /home/o/canterlot-2a/target 77 GB (31 GB \
         incremental, written 2 h ago); worktree «Баннер связи» target 12 GB (idle 5 h). Run \
         scripts/prune-target.sh or remove target/debug/incremental in idle checkouts."
    );
    assert_eq!(inputs[0].1["event"]["kind"], "machine_disk_low");
    assert_eq!(inputs[0].1["from"], json!(f.subscriber));
    Ok(())
}

async fn mem_episodes(pool: &PgPool, open: bool) -> i64 {
    sqlx::query_scalar(
        "select count(*) from machine_watch_episodes
         where kind='mem_low' and (ended_at is null)=$1",
    )
    .bind(open)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn low_memory_needs_three_samples_and_recovers_with_hysteresis(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let heavy = execution(&pool, f.target).await;
    let executions =
        json!([{"execution_id": heavy, "alive": true, "cpu_permille": 0, "rss_bytes": 6 * GB}]);
    let t0 = Utc::now();
    let step = |n: i64| t0 + TimeDelta::seconds(30 * n);
    // Две низкие выборки, одна нормальная, снова две: серия сброшена, эпизода нет.
    for (n, available) in [
        (0, 2 * GB),
        (1, 2 * GB),
        (2, 5 * GB),
        (3, 2 * GB),
        (4, 2 * GB),
    ] {
        set_mem(&pool, f.machine, available, executions.clone(), step(n)).await;
        sweep(&pool, step(n)).await;
        // Повторный обход той же выборки её не считает.
        sweep(&pool, step(n) + TimeDelta::seconds(5)).await;
    }
    assert_eq!(mem_episodes(&pool, true).await, 0);
    // Третья подряд открывает эпизод и одно событие.
    set_mem(&pool, f.machine, 2 * GB, executions.clone(), step(5)).await;
    let report = sweep(&pool, step(5)).await;
    assert_eq!((report.mem_opened, report.mem_warned), (1, 1));
    assert_eq!(mem_episodes(&pool, true).await, 1);
    // 3,5 ГБ: выше порога, но ниже порога с запасом — эпизод открыт, тишина.
    set_mem(&pool, f.machine, 3_500_000_000, executions.clone(), step(6)).await;
    let report = sweep(&pool, step(6)).await;
    assert_eq!((report.mem_recovered, report.mem_warned), (0, 0));
    // 4,1 ГБ: возврат с запасом закрывает эпизод молча.
    set_mem(&pool, f.machine, 4_100_000_000, executions, step(7)).await;
    assert_eq!(sweep(&pool, step(7)).await.mem_recovered, 1);
    assert_eq!(mem_episodes(&pool, false).await, 1);

    scheduler(&pool).tick(step(7)).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        inputs[0].0,
        "[canterlot] Machine \"synthetic\" is low on memory: 2 GB available of 64 GB. Largest \
         sessions: «делегат» 6 GB. Builds may fail or be killed; avoid starting heavy work \
         there now."
    );
    assert_eq!(inputs[0].1["event"]["kind"], "machine_mem_low");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn leaked_processes_are_announced_once_per_episode(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let gone = execution(&pool, f.target).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 200 * GB, 295 * GB, now).await;
    let leaked = json!([
        {"execution_id": gone, "pid": 10, "comm": "cargo", "rss_bytes": 600_000_000u64, "age_s": 600},
        {"execution_id": gone, "pid": 11, "comm": "cargo", "rss_bytes": 380_000_000u64, "age_s": 600},
        {"execution_id": gone, "pid": 12, "comm": "node", "rss_bytes": 420_000_000u64, "age_s": 60}
    ]);
    set_watch(&pool, f.owner, f.machine, json!([]), leaked.clone(), now).await;
    let report = sweep(&pool, now).await;
    assert_eq!((report.leak_opened, report.leak_warned), (1, 1));
    // Следующий отчёт с теми же процессами — тот же эпизод, тишина.
    let later = now + TimeDelta::minutes(5);
    set_disk(&pool, f.machine, 200 * GB, 295 * GB, later).await;
    set_watch(&pool, f.owner, f.machine, json!([]), leaked.clone(), later).await;
    assert_eq!(sweep(&pool, later).await.leak_warned, 0);
    // Чистый отчёт закрывает эпизод; новая утечка — новый эпизод и новое событие.
    let clean = later + TimeDelta::minutes(5);
    set_disk(&pool, f.machine, 200 * GB, 295 * GB, clean).await;
    set_watch(&pool, f.owner, f.machine, json!([]), json!([]), clean).await;
    assert_eq!(sweep(&pool, clean).await.leak_recovered, 1);
    let again = clean + TimeDelta::minutes(5);
    set_disk(&pool, f.machine, 200 * GB, 295 * GB, again).await;
    set_watch(&pool, f.owner, f.machine, json!([]), leaked, again).await;
    assert_eq!(sweep(&pool, again).await.leak_warned, 1);

    scheduler(&pool).tick(again).await?;
    let inputs = delivered(&pool, f.subscriber).await;
    assert_eq!(inputs.len(), 2);
    assert_eq!(
        inputs[0].0,
        "[canterlot] 3 processes outlived their sessions on \"synthetic\" (cargo 2, node 1; \
         1.4 GB RSS): from «делегат». Nothing is stopped automatically."
    );
    assert_eq!(inputs[0].1["event"]["kind"], "machine_processes_leaked");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_watch_does_not_open_a_leak_episode(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let gone = execution(&pool, f.target).await;
    let now = Utc::now();
    set_disk(&pool, f.machine, 200 * GB, 295 * GB, now).await;
    let leaked =
        json!([{"execution_id": gone, "pid": 10, "comm": "cargo", "rss_bytes": 1, "age_s": 1}]);
    set_watch(
        &pool,
        f.owner,
        f.machine,
        json!([]),
        leaked,
        now - TimeDelta::minutes(30),
    )
    .await;
    assert_eq!(sweep(&pool, now).await.leak_opened, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn watch_report_is_stored_resolved_and_published(pool: PgPool) -> TestResult {
    use canterlot_hub::machine_watch::{self, WatchOutcome};
    use canterlot_proto::worker::v1::{DiskHotspot, LeakedProcess, MachineWatch};
    let f = fx(&pool).await;
    let gone = execution(&pool, f.target).await;
    let now_ms = Utc::now().timestamp_millis() as u64;
    let watch = MachineWatch {
        hotspots: vec![DiskHotspot {
            path: format!("/data/worktrees/{}/target", f.target),
            kind: "target".into(),
            bytes: 12 * GB,
            incremental_bytes: 4 * GB,
            newest_ms: now_ms - 3_600_000,
            measured_at_ms: now_ms,
            conversation_id: f.target.to_string(),
        }],
        leaked: vec![LeakedProcess {
            execution_id: gone.to_string(),
            pid: 7,
            comm: "cargo".into(),
            rss_bytes: GB,
            age_s: 60,
        }],
        unleased_count: 2,
        observed_at_ms: now_ms,
    };
    let stored =
        machine_watch::apply(&pool, f.owner, f.machine, &watch, None, Uuid::now_v7()).await?;
    assert_eq!(stored, WatchOutcome::Stored);
    let mut conn = pool.acquire().await?;
    let view = machine_watch::view(&mut conn, f.owner, f.machine)
        .await?
        .unwrap();
    assert_eq!(view.hotspots[0].title.as_deref(), Some("делегат"));
    assert_eq!(view.leaked[0].conversation_id, Some(f.target));
    assert_eq!(view.unleased_count, 2);
    let events: i64 =
        sqlx::query_scalar("select count(*) from events where kind='machine_watch_changed'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(events, 1);

    // Старый отчёт не заменяет новый, чужой вид кэша отвергается целиком.
    let older = MachineWatch {
        observed_at_ms: now_ms - 60_000,
        unleased_count: 9,
        ..watch.clone()
    };
    assert_eq!(
        machine_watch::apply(&pool, f.owner, f.machine, &older, None, Uuid::now_v7()).await?,
        WatchOutcome::Ignored("stale_watch")
    );
    let mut bad = watch.clone();
    bad.hotspots[0].kind = "home".into();
    assert_eq!(
        machine_watch::apply(&pool, f.owner, f.machine, &bad, None, Uuid::now_v7()).await?,
        WatchOutcome::Ignored("hotspot_kind")
    );
    Ok(())
}

async fn jobs_of(pool: &PgPool, conversation: Uuid, kind: &str) -> i64 {
    sqlx::query_scalar(
        "select count(*) from scheduled_jobs
         where payload->>'action'='machine_event' and payload->>'kind'=$2 and conversation_id=$1",
    )
    .bind(conversation)
    .bind(kind)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Оркестратор, ставший получателем посреди эпизода, слышит его один раз, как и у диска.
async fn late_orchestrator(pool: &PgPool, f: &Fx, now: DateTime<Utc>) -> Uuid {
    let late = conversation(pool, f.owner, f.machine, "поздний").await;
    let child = conversation(pool, f.owner, f.machine, "его делегат").await;
    child_of(pool, late, child, now).await;
    derive_since(pool, late, "working", now).await;
    late
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_orchestrator_hears_open_memory_and_leak_episodes_once(pool: PgPool) -> TestResult {
    let f = fx(&pool).await;
    let gone = execution(&pool, f.target).await;
    let t0 = Utc::now();
    let step = |n: i64| t0 + TimeDelta::seconds(30 * n);
    let leaked =
        json!([{"execution_id": gone, "pid": 10, "comm": "cargo", "rss_bytes": GB, "age_s": 60}]);
    for n in 0..3 {
        set_mem(&pool, f.machine, 2 * GB, json!([]), step(n)).await;
        set_watch(
            &pool,
            f.owner,
            f.machine,
            json!([]),
            leaked.clone(),
            step(n),
        )
        .await;
        sweep(&pool, step(n)).await;
    }
    assert_eq!(mem_episodes(&pool, true).await, 1);
    assert_eq!(jobs_of(&pool, f.subscriber, "mem_low").await, 1);
    assert_eq!(jobs_of(&pool, f.subscriber, "processes_leaked").await, 1);

    let late = late_orchestrator(&pool, &f, step(3)).await;
    for n in 3..6 {
        set_mem(&pool, f.machine, 2 * GB, json!([]), step(n)).await;
        set_watch(
            &pool,
            f.owner,
            f.machine,
            json!([]),
            leaked.clone(),
            step(n),
        )
        .await;
        sweep(&pool, step(n)).await;
        sweep(&pool, step(n) + TimeDelta::seconds(5)).await;
    }
    assert_eq!(jobs_of(&pool, late, "mem_low").await, 1);
    assert_eq!(jobs_of(&pool, late, "processes_leaked").await, 1);
    assert_eq!(jobs_of(&pool, f.subscriber, "mem_low").await, 1);
    assert_eq!(jobs_of(&pool, f.subscriber, "processes_leaked").await, 1);
    assert_eq!(mem_episodes(&pool, true).await, 1);
    Ok(())
}
