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

#[tokio::test]
async fn low_disk_warns_every_recipient_once_then_critical_once() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

async fn child_of(pool: &PgPool, parent: Uuid, child: Uuid, created_at: DateTime<Utc>) {
    sqlx::query("update conversations set parent_conversation_id=$2,created_at=$3 where id=$1")
        .bind(child)
        .bind(parent)
        .bind(created_at)
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

#[tokio::test]
async fn watch_report_is_stored_resolved_and_published() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

/// На машине работают оба домена памяти. Сторож смотрит на всю машину, но каждому получателю
/// называет только сессии и каталоги его домена: у остальных остаются числа.
#[tokio::test]
async fn watchdog_events_hide_sessions_of_another_domain() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let f = fx(&pool).await;
    let now = Utc::now();
    sqlx::query(
        "update machines set capabilities=array['memory_scope_v1'],
           memory_work_roots='[\"/srv/work\"]',worktree_root='/data/worktrees' where id=$1",
    )
    .bind(f.machine)
    .execute(&pool)
    .await?;
    // Личный оркестратор с делегатом на той же машине: тоже получатель событий.
    let personal = conversation(&pool, f.owner, f.machine, "Личное").await;
    let kid = conversation(&pool, f.owner, f.machine, "Личное: налоги").await;
    child_of(&pool, personal, kid, now).await;
    sqlx::query("update conversations set memory_domain='personal' where id=any($1)")
        .bind([personal, kid])
        .execute(&pool)
        .await?;
    derive(&pool, personal, "working", json!({})).await;
    let work_run = execution(&pool, f.target).await;
    let personal_run = execution(&pool, kid).await;

    let at = |h: i64| (now - TimeDelta::hours(h)).timestamp_millis();
    let spot = |path: String, gb: u64, owner: Option<Uuid>| {
        json!({"path": path, "kind": "target", "bytes": gb * GB, "incremental_bytes": 0,
               "newest_ms": at(5), "measured_at_ms": at(0),
               "conversation_id": owner.map(|id| id.to_string())})
    };
    let leaked = json!([
        {"execution_id": work_run, "pid": 10, "comm": "cargo", "rss_bytes": 600_000_000u64, "age_s": 600},
        {"execution_id": personal_run, "pid": 11, "comm": "node", "rss_bytes": 400_000_000u64, "age_s": 600}
    ]);
    let hotspots = json!([
        spot("/srv/work/acme/target".into(), 77, None),
        spot(
            format!("/data/worktrees/{}/target", f.target),
            40,
            Some(f.target)
        ),
        spot(format!("/data/worktrees/{kid}/target"), 12, Some(kid)),
        spot("/srv/hobby/target".into(), 5, None),
    ]);
    let executions = json!([
        {"execution_id": work_run, "alive": true, "cpu_permille": 0, "rss_bytes": 6 * GB},
        {"execution_id": personal_run, "alive": true, "cpu_permille": 0, "rss_bytes": 3 * GB}
    ]);
    set_disk(&pool, f.machine, 28 * GB, 295 * GB, now).await;
    // Три низкие выборки памяти подряд открывают эпизод `mem_low`.
    for n in 0..3 {
        let step = now + TimeDelta::seconds(30 * n);
        set_mem(&pool, f.machine, 2 * GB, executions.clone(), step).await;
        set_watch(
            &pool,
            f.owner,
            f.machine,
            hotspots.clone(),
            leaked.clone(),
            step,
        )
        .await;
        sweep(&pool, step).await;
    }
    let details = |jobs: &[Value], kind: &str| {
        jobs.iter()
            .find(|job| job["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} job"))["details"]
            .clone()
    };

    // Рабочий получатель: свои названия видит, личная сессия для него безымянна.
    let work = jobs(&pool, f.subscriber).await;
    let disk = details(&work, "disk_low");
    assert_eq!(disk["hotspots"][0]["path"], "/srv/work/acme/target");
    assert_eq!(disk["hotspots"][1]["title"], "делегат");
    assert_eq!(
        disk["hotspots"][2],
        json!({"path": null, "kind": "target", "bytes": 12 * GB, "incremental_bytes": 0,
               "newest_at": disk["hotspots"][2]["newest_at"], "conversation_id": null,
               "title": null, "hidden": true})
    );
    let mem = details(&work, "mem_low");
    assert_eq!(mem["heaviest"][0]["title"], "делегат");
    assert_eq!(
        mem["heaviest"][1],
        json!({"execution_id": null, "title": null, "rss_bytes": 3 * GB, "hidden": true})
    );
    let leak = details(&work, "processes_leaked");
    assert_eq!(leak["leaked"][0]["title"], "делегат");
    assert_eq!(
        leak["leaked"][1],
        json!({"execution_id": null, "title": null, "comm": "node",
               "rss_bytes": 400_000_000u64, "hidden": true})
    );

    // Личный получатель: ни названий, ни идентификаторов, ни путей рабочего домена.
    let mine = jobs(&pool, personal).await;
    let all = serde_json::to_string(&mine)?;
    for secret in [
        "делегат".to_owned(),
        "/srv/work".to_owned(),
        f.target.to_string(),
        work_run.to_string(),
    ] {
        assert!(!all.contains(&secret), "{secret} leaked: {all}");
    }
    let disk = details(&mine, "disk_low");
    assert_eq!(disk["hotspots"][0]["hidden"], true);
    assert_eq!(disk["hotspots"][0]["bytes"], 77 * GB);
    assert_eq!(disk["hotspots"][2]["title"], "Личное: налоги");
    assert_eq!(disk["hotspots"][3]["path"], "/srv/hobby/target");
    assert_eq!(
        details(&mine, "mem_low")["heaviest"][1]["title"],
        "Личное: налоги"
    );

    scheduler(&pool).tick(now + TimeDelta::seconds(90)).await?;
    let texts: Vec<String> = delivered(&pool, personal)
        .await
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    let disk_text = texts
        .iter()
        .find(|text| text.contains("low on disk"))
        .expect("disk text");
    assert!(
        disk_text.contains(
            "Largest build caches: another checkout's target 77 GB; another checkout's target \
             40 GB; worktree «Личное: налоги» target 12 GB (idle 5 h)."
        ),
        "{disk_text}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text
                .contains("Largest sessions: another session 6 GB, «Личное: налоги» 3 GB.")),
        "{texts:#?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("from another session, «Личное: налоги».")),
        "{texts:#?}"
    );
    Ok(())
}
