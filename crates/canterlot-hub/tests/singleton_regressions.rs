use axum::http::Method;
use canterlot_hub::{
    config::HubTimers,
    http::{self, AppState},
    machine_info,
    session::Registry,
    testkit::{self, TestClient},
};
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

#[sqlx::test(migrations = "./migrations")]
async fn machine_status_reacquire_preserves_connected_machine_availability(pool: PgPool) {
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
    let task = tokio::spawn(machine_info::run_status_projection(
        service_pool.clone(),
        registry,
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

#[sqlx::test(migrations = "./migrations")]
async fn failing_start_projection_backs_off_lock_acquisitions_and_readyz(pool: PgPool) {
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
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        Registry::new(Default::default()),
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
    while start.elapsed() < Duration::from_secs(6) {
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
        "projection acquired {attempts} times in 6 s"
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
        let minimum = Duration::from_millis(250 * (1 << index));
        let elapsed = cycle[1].0 - cycle[0].0;
        assert!(
            elapsed + Duration::from_millis(75) >= minimum,
            "readyz flapped after {elapsed:?}, expected at least {minimum:?}: {transitions:?}"
        );
    }
}
