//! Службы-одиночки хаба на advisory-замке: повторный захват после потери соединения,
//! пауза между захватами, пассивность без замка и отчёт `readyz` об остановленной службе.

use crate::support::task5::{count, scheduled, scheduler, until_state};
use crate::support::task9 as files;

use axum::http::Method;
use canterlot_hub::{
    activity, attachments,
    config::{HubTimers, SchedulerConfig},
    http::{self, AppState},
    machine_info,
    session::Registry,
    testkit::{self, TestClient},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

async fn lock_pid(pool: &PgPool) -> Option<i32> {
    sqlx::query_scalar(
        "select pid from pg_locks where locktype='advisory' and granted
         and database=(select oid from pg_database where datname=current_database())
         and classid::bigint=((hashtextextended('canterlot:machine_status',0)>>32)&4294967295)
         and objid::bigint=(hashtextextended('canterlot:machine_status',0)&4294967295)
         and objsubid=1",
    )
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn machine_status_reacquire_preserves_connected_machine_availability() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let registry = Registry::new(HubTimers {
        degraded_after: Duration::from_secs(3600),
        unavailable_after: Duration::from_secs(7200),
        ..Default::default()
    });
    let _session = testkit::fake_session(&registry, seed.machine_id, &[], true);
    let projections = Arc::new(AtomicUsize::new(0));
    let completed = projections.clone();
    let service_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_release(move |_, _| {
            completed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(true) })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let (stop, rx) = watch::channel(false);
    // Вторая проекция приходит по такту пересчёта: боевой такт 5 с, здесь короткий.
    let task = tokio::spawn(machine_info::run_status_projection_with(
        service_pool.clone(),
        registry,
        machine_info::StatusProjectionTimers {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
        rx,
    ));
    let observed = tokio::time::timeout(Duration::from_secs(20), async {
        let pid = loop {
            let availability: String =
                sqlx::query_scalar("select availability from machines where id=$1")
                    .bind(seed.machine_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if projections.load(Ordering::SeqCst) >= 2 && availability == "available" {
                break lock_pid(&pool).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let cursor: i64 = sqlx::query_scalar(
            "select max(scope_seq) from events where kind='machine_availability_changed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let terminated: bool = sqlx::query_scalar("select pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(terminated);
        let replacement = loop {
            if let Some(next) = lock_pid(&pool).await.filter(|next| *next != pid) {
                break next;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        // Дождаться завершённой проекции после повторного захвата.
        let baseline = projections.load(Ordering::SeqCst);
        while projections.load(Ordering::SeqCst) == baseline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let changes: Vec<String> = sqlx::query_scalar(
            "select payload->>'availability' from events
             where kind='machine_availability_changed' and scope_seq>$1
             and payload->>'machine_id'=$2 order by scope_seq",
        )
        .bind(cursor)
        .bind(seed.machine_id.to_string())
        .fetch_all(&pool)
        .await
        .unwrap();
        let availability: String =
            sqlx::query_scalar("select availability from machines where id=$1")
                .bind(seed.machine_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        (pid, replacement, changes, availability)
    })
    .await;
    let _ = stop.send(true);
    task.await.unwrap();
    service_pool.close().await;
    let (pid, replacement, changes, availability) = observed.unwrap();
    eprintln!("lock backend {pid} -> {replacement}; availability events: {changes:?}");
    assert!(changes.is_empty(), "live machine flapped: {changes:?}");
    assert_eq!(availability, "available");
}

const BACKOFF: Duration = Duration::from_millis(125);

#[tokio::test]
async fn failing_start_projection_backs_off_lock_acquisitions_and_readyz() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let client = TestClient::in_process(http::router(AppState::new(deps)));
    assert_eq!(
        client.send(Method::GET, "/readyz", None, None).await.status,
        200
    );
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    // Последовательность сохраняет число попыток даже при откате проекции.
    sqlx::raw_sql(
        "create sequence projection_attempts;
         create function fail_projection() returns trigger language plpgsql as $$
         begin
             perform nextval('projection_attempts');
             perform pg_sleep(0.12);
             raise exception 'projection unavailable';
         end $$;
         create trigger fail_projection before update on machines
         for each row execute function fail_projection();",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (stop, rx) = watch::channel(false);
    // Пауза вдвое короче боевой: рост в два раза на попытку тот же, окно 3 с вместо 6 с.
    let task = tokio::spawn(machine_info::run_status_projection_with(
        pool.clone(),
        Registry::new(Default::default()),
        canterlot_hub::machine_info::StatusProjectionTimers {
            backoff: canterlot_hub::singleton::Backoff {
                initial: BACKOFF,
                ..Default::default()
            },
            ..Default::default()
        },
        rx,
    ));
    let first_pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(pid) = lock_pid(&pool).await {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let start = Instant::now();
    let mut backends = BTreeSet::new();
    if let Ok(pid) = first_pid {
        backends.insert(pid);
    }
    let mut transitions = Vec::new();
    let mut bad_responses = Vec::new();
    while start.elapsed() < Duration::from_secs(3) {
        if let Some(pid) = lock_pid(&pool).await {
            backends.insert(pid);
        }
        let response = client.send(Method::GET, "/readyz", None, None).await;
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        match response.status.as_u16() {
            200 => {}
            503 if body["reason"] == "service" && body["services"] == json!(["machine_status"]) => {
            }
            _ => bad_responses.push((response.status, body)),
        }
        if transitions
            .last()
            .is_none_or(|&(_, status)| status != response.status)
        {
            transitions.push((start.elapsed(), response.status));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = stop.send(true);
    task.await.unwrap();
    first_pid.unwrap();
    let attempts: i64 = sqlx::query_scalar("select last_value from projection_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    eprintln!(
        "projection attempts: {attempts}; lock backends: {}; readyz transitions: {transitions:?}",
        backends.len()
    );
    assert!(
        bad_responses.is_empty(),
        "unexpected readyz responses: {bad_responses:?}"
    );
    assert!(
        (4..=5).contains(&attempts),
        "projection acquired {attempts} times in 3 s"
    );
    assert!(
        (4..=5).contains(&backends.len()),
        "lock backends: {backends:?}"
    );
    let ready: Vec<_> = transitions
        .iter()
        .filter(|(_, status)| *status == 200)
        .collect();
    assert!(
        ready.len() >= 4,
        "readyz cycles were not observed: {transitions:?}"
    );
    for (index, cycle) in ready.windows(2).enumerate() {
        let minimum = BACKOFF * (1u32 << index);
        let elapsed = cycle[1].0 - cycle[0].0;
        assert!(
            elapsed + Duration::from_millis(75) >= minimum,
            "readyz flapped after {elapsed:?}, expected at least {minimum:?}: {transitions:?}"
        );
    }
}

async fn service_lock_pid(pool: &PgPool, service: &str, previous: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar("select pid from pg_locks where locktype='advisory' and granted and database=(select oid from pg_database where datname=current_database()) and classid::bigint=((hashtextextended($1,0)>>32)&4294967295) and objid::bigint=(hashtextextended($1,0)&4294967295) and pid<>$2")
                .bind(format!("canterlot:{service}")).bind(previous).fetch_optional(pool).await.unwrap();
            if let Some(pid)=pid { break pid; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}
#[tokio::test]
async fn singleton_scheduler_reacquires_after_backend_termination() {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    let sch = scheduler(
        &pool,
        SchedulerConfig {
            idle_poll: Duration::from_millis(20),
            ..Default::default()
        },
    );
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(sch.run(rx));
    let pid = service_lock_pid(&pool, "scheduler", 0).await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(12), async {
        let new_pid = service_lock_pid(&pool, "scheduler", pid).await;
        assert_ne!(pid, new_pid);
        let now = Utc::now();
        let m = scheduled(&pool, &s, "after-loss", now).await;
        sqlx::query(
            "update scheduled_jobs set run_at=clock_timestamp()-interval '1 second' where id=$1",
        )
        .bind(m.id)
        .execute(&pool)
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if count(&pool, "select count(*) from inputs").await == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    })
    .await;
    stop.send(true).unwrap();
    task.await.unwrap();
    assert!(recovered.is_ok(), "scheduler did not resume");
}
#[tokio::test]
async fn readyz_names_an_unexpectedly_stopped_singleton() {
    let pool = canterlot_hub::testkit::db().await;
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let sch = deps.scheduler.clone();
    let client = TestClient::in_process(http::router(AppState::new(deps)));
    let (_stop, rx) = watch::channel(false);
    let task = tokio::spawn(sch.run(rx));
    service_lock_pid(&pool, "scheduler", 0).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let response = client.send(Method::GET, "/readyz", None, None).await;
    assert_eq!(response.status, 503);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap()["services"],
        json!(["scheduler"])
    );
}
#[tokio::test]
async fn singleton_activity_recreates_projection_after_lock_loss() {
    let pool = canterlot_hub::testkit::db().await;
    use canterlot_hub::{
        bus::EventBus,
        config::{ActivityConfig, MachinesConfig},
    };
    let s = testkit::seed(&pool).await;
    let (stop, rx) = watch::channel(false);
    let bus = EventBus::spawn(pool.clone(), false, rx.clone());
    let task = tokio::spawn(activity::run(
        pool.clone(),
        bus.clone(),
        ActivityConfig {
            tick: Duration::from_millis(20),
            ..Default::default()
        },
        MachinesConfig::default(),
        true,
        rx,
    ));
    let pid = service_lock_pid(&pool, "activity", 0).await;
    until_state(&pool, s.conversation_id, "idle").await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    service_lock_pid(&pool, "activity", pid).await;
    sqlx::query("delete from conversation_derived where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    until_state(&pool, s.conversation_id, "idle").await;
    stop.send(true).unwrap();
    task.await.unwrap();
    bus.join().await;
}
#[tokio::test]
async fn singleton_gc_stays_passive_then_recovers_and_collects() {
    let pool = canterlot_hub::testkit::db().await;
    let f = files::Fixture::new(pool.clone()).await;
    let held =
        canterlot_hub::singleton::Singleton::acquire(&pool.connect_options(), "attachment-gc")
            .await
            .unwrap()
            .unwrap();
    let id = uuid::Uuid::now_v7();
    let path = f.staging_path(id);
    std::fs::write(&path, b"x").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(90000))
        .unwrap();
    let mut config = f.config.clone();
    config.gc_interval = Duration::from_secs(3600);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(attachments::gc::run(
        pool.clone(),
        f.store.clone(),
        config,
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(path.exists());
    held.release().await;
    let pid = service_lock_pid(&pool, "attachment-gc", 0).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(&path, b"x").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(90000))
        .unwrap();
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    service_lock_pid(&pool, "attachment-gc", pid).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn singleton_status_retries_failed_initial_projection() {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create function fail_initial_status() returns trigger language plpgsql as $$ begin raise exception 'projection_failed'; end $$; create trigger fail_initial_status before update on machines for each row execute function fail_initial_status();").execute(&pool).await.unwrap();
    let (stop, rx) = watch::channel(false);
    // Короткие пауза захвата и такт пересчёта: боевые после двух сбоев и потери замка
    // растут до секунд, а проверяется здесь только сам повтор проекции.
    let task = tokio::spawn(canterlot_hub::machine_info::run_status_projection_with(
        pool.clone(),
        canterlot_hub::session::Registry::new(Default::default()),
        canterlot_hub::machine_info::StatusProjectionTimers {
            backoff: canterlot_hub::singleton::Backoff {
                initial: Duration::from_millis(25),
                ..Default::default()
            },
            tick: Duration::from_millis(50),
        },
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    sqlx::query("drop trigger fail_initial_status on machines")
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state: String = sqlx::query_scalar("select availability from machines where id=$1")
                .bind(s.machine_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "unavailable" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = service_lock_pid(&pool, "machine_status", 0).await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    service_lock_pid(&pool, "machine_status", pid).await;
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let unavailable: bool =
                sqlx::query_scalar("select availability='unavailable' from machines where id=$1")
                    .bind(s.machine_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if unavailable {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn rereview_machine_status_reacquire_does_not_publish_unavailable() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let registry = Registry::new(HubTimers {
        degraded_after: Duration::from_secs(3600),
        unavailable_after: Duration::from_secs(7200),
        ..Default::default()
    });
    let _session = testkit::fake_session(&registry, seed.machine_id, &[], true);
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        registry,
        rx,
    ));
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let pid = service_lock_pid(&pool, "machine_status", 0).await;
        loop {
            if count(&pool, "select count(*) from machines where availability='available'").await == 1 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let cursor = count(&pool, "select coalesce(max(scope_seq),0) from events where kind='machine_availability_changed'").await;
        sqlx::query("select pg_terminate_backend($1)").bind(pid).execute(&pool).await.unwrap();
        service_lock_pid(&pool, "machine_status", pid).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        sqlx::query_scalar::<_, String>("select payload->>'availability' from events where kind='machine_availability_changed' and scope_seq>$1 order by scope_seq")
            .bind(cursor).fetch_all(&pool).await.unwrap()
    }).await;
    let _ = stop.send(true);
    task.await.unwrap();
    assert!(
        result.unwrap().is_empty(),
        "live machine flapped on reacquire"
    );
}
#[tokio::test]
async fn rereview_failing_start_projection_keeps_exponential_backoff() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create sequence projection_attempts; create function fail_projection() returns trigger language plpgsql as $$ begin perform nextval('projection_attempts'); raise exception using errcode='57014',message='projection unavailable'; end $$; create trigger fail_projection before update on machines for each row execute function fail_projection();")
        .execute(&pool).await.unwrap();
    let (stop, rx) = watch::channel(false);
    // Пауза в десять раз короче боевой: те же пять удвоений укладываются в 0,5 с, а без
    // роста паузы попыток было бы два десятка.
    let task = tokio::spawn(machine_info::run_status_projection_with(
        pool.clone(),
        Registry::new(Default::default()),
        canterlot_hub::machine_info::StatusProjectionTimers {
            backoff: canterlot_hub::singleton::Backoff {
                initial: Duration::from_millis(25),
                ..Default::default()
            },
            ..Default::default()
        },
        rx,
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = stop.send(true);
    task.await.unwrap();
    let attempts: i64 = sqlx::query_scalar("select last_value from projection_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        (2..=5).contains(&attempts),
        "projection reconnected {attempts} times in 0.5 s"
    );
}
