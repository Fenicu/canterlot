use axum::http::{Method, StatusCode};
use canterlot_hub::{
    config::SchedulerConfig,
    delivery::dispatch::{DispatchOutcome, GateInputs, dispatch_next},
    http::{self, AppState},
    scheduler::{self, JobState, NewScheduled, PatchScheduled, ScheduleError},
    testkit::{self, TestClient},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;
#[path = "support/task5.rs"]
mod support;
use support::*;

#[sqlx::test(migrations = "./migrations")]
async fn schedule_validates_time_window_content_and_pending_limit(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let _scheduler = scheduler(
        &pool,
        SchedulerConfig {
            max_pending_per_conversation: 1,
            ..Default::default()
        },
    );
    for (seconds, valid) in [
        (0, false),
        (1, true),
        (366 * 86400, true),
        (366 * 86400 + 1, false),
    ] {
        let result = scheduler::schedule_message(
            &pool,
            s.owner_id,
            &format!("time-{seconds}"),
            NewScheduled {
                conversation_id: s.conversation_id,
                run_at: now + TimeDelta::seconds(seconds),
                content: content(),
                attachment_ids: vec![],
            },
            now,
            Uuid::now_v7(),
        )
        .await;
        if valid {
            let m = result.unwrap();
            scheduler::cancel_message(
                &pool,
                s.owner_id,
                m.id,
                &format!("cancel-{seconds}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        } else {
            assert!(matches!(result, Err(ScheduleError::TimeOutOfRange)));
        }
    }
    for content in [
        vec![],
        vec![canterlot_hub::delivery::ContentBlock::Text { text: "\0".into() }],
        vec![canterlot_hub::delivery::ContentBlock::Text {
            text: "x".repeat(262145),
        }],
    ] {
        assert!(matches!(
            scheduler::schedule_message(
                &pool,
                s.owner_id,
                "bad-content",
                NewScheduled {
                    conversation_id: s.conversation_id,
                    run_at: now + TimeDelta::seconds(2),
                    content,
                    attachment_ids: vec![]
                },
                now,
                Uuid::now_v7()
            )
            .await,
            Err(ScheduleError::InvalidContent(_))
        ));
    }
    assert!(matches!(
        scheduler::schedule_message(
            &pool,
            s.owner_id,
            "attachment",
            NewScheduled {
                conversation_id: s.conversation_id,
                run_at: now + TimeDelta::seconds(2),
                content: content(),
                attachment_ids: vec![Uuid::now_v7()]
            },
            now,
            Uuid::now_v7()
        )
        .await,
        Err(ScheduleError::Attachment(_))
    ));
    scheduled(&pool, &s, "ok", now).await;
    assert!(matches!(
        scheduler::schedule_message(
            &pool,
            s.owner_id,
            "over",
            NewScheduled {
                conversation_id: s.conversation_id,
                run_at: now + TimeDelta::seconds(2),
                content: content(),
                attachment_ids: vec![]
            },
            now,
            Uuid::now_v7()
        )
        .await,
        Err(ScheduleError::TooManyPending)
    ));
    assert_eq!(
        count(
            &pool,
            "select count(*) from idempotency_keys where key in ('bad-content','attachment','over')"
        )
        .await,
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn scheduled_message_is_visible_editable_and_cancellable_until_fired(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    let m = scheduled(&pool, &s, "one", now).await;
    let list = scheduler::list_messages(
        &mut pool.acquire().await.unwrap(),
        s.owner_id,
        s.conversation_id,
    )
    .await
    .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, m.id);
    let edited = scheduler::update_message(
        &pool,
        s.owner_id,
        m.id,
        "edit",
        PatchScheduled {
            run_at: Some(now + TimeDelta::hours(1)),
            content: Some(vec![canterlot_hub::delivery::ContentBlock::Text {
                text: "edited".into(),
            }]),
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(edited.run_at, now + TimeDelta::hours(1));
    assert_ne!(edited.content, m.content);
    let cancelled = scheduler::cancel_message(&pool, s.owner_id, m.id, "cancel", Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(matches!(
        scheduler::cancel_message(&pool, s.owner_id, m.id, "again", Uuid::now_v7()).await,
        Err(ScheduleError::NotPending)
    ));
    assert_eq!(sch.tick(now + TimeDelta::hours(2)).await.unwrap().fired, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn due_message_becomes_a_normal_queued_input_once(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    let m = scheduled(&pool, &s, "one", now).await;
    assert_eq!(sch.tick(now).await.unwrap().fired, 0);
    assert_eq!(sch.tick(m.run_at).await.unwrap().fired, 1);
    assert_eq!(sch.tick(m.run_at).await.unwrap().fired, 0);
    let (key, state, at, id): (String, String, chrono::DateTime<Utc>, Uuid) =
        sqlx::query_as("select idempotency_key,state,received_at,id from inputs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(key, format!("sched:{}", m.id));
    assert_eq!(state, "queued");
    assert_eq!(at.timestamp_millis(), m.run_at.timestamp_millis());
    let (job, result): (String, Value) = sqlx::query_as("select state,result from scheduled_jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(job, "fired");
    assert_eq!(result, json!({"input_id":id}));
    assert_eq!(count(&pool, "select count(*) from commands").await, 1);
    assert_eq!(
        count(
            &pool,
            "select count(*) from idempotency_keys where key like 'sched:%'"
        )
        .await,
        1
    );
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrations = "./migrations")]
async fn scheduled_message_fires_exactly_once_with_two_schedulers(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let first = scheduler(&pool, Default::default());
    let second = scheduler(&pool, Default::default());
    for n in 0..50 {
        scheduled(&pool, &s, &format!("job-{n}"), now).await;
    }
    let deadlocks = count(
        &pool,
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .await;
    let one = first.faults.pause_next(Point::SchedulerBeforeSubmit);
    let two = second.faults.pause_next(Point::SchedulerBeforeSubmit);
    let due = now + TimeDelta::seconds(3);
    let a = tokio::spawn(async move {
        let mut fired = 0;
        for _ in 0..4 {
            fired += first.tick(due).await.unwrap().fired;
        }
        fired
    });
    let b = tokio::spawn(async move {
        let mut fired = 0;
        for _ in 0..4 {
            fired += second.tick(due).await.unwrap().fired;
        }
        fired
    });
    let (ra, rb) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(one, two) })
        .await
        .unwrap();
    ra.unwrap().send(()).unwrap();
    rb.unwrap().send(()).unwrap();
    let (a, b) = tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(a, b) })
        .await
        .unwrap();
    let a = a.unwrap();
    let b = b.unwrap();
    assert!(a >= 1 && b >= 1);
    assert_eq!(a + b, 50);
    assert_eq!(count(&pool, "select count(*) from inputs").await, 50);
    assert_eq!(
        count(&pool, "select count(distinct idempotency_key) from inputs").await,
        50
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from idempotency_keys where key like 'sched:%'"
        )
        .await,
        50
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from scheduled_jobs where state='fired'"
        )
        .await,
        50
    );
    assert_eq!(
        count(
            &pool,
            "select deadlocks from pg_stat_database where datname=current_database()"
        )
        .await,
        deadlocks
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn fire_failure_in_the_same_transaction_leaves_the_job_pending(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    scheduled(&pool, &s, "one", now).await;
    sqlx::query("alter table scheduled_jobs add constraint forbid_fired check (state <> 'fired')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(sch.tick(now + TimeDelta::seconds(3)).await.is_err());
    assert_eq!(count(&pool, "select count(*) from inputs").await, 0);
    assert_eq!(count(&pool, "select count(*) from commands").await, 0);
    assert_eq!(
        count(
            &pool,
            "select count(*) from idempotency_keys where key like 'sched:%'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from scheduled_jobs where state='pending' and attempts=0"
        )
        .await,
        1
    );
    sqlx::query("alter table scheduled_jobs drop constraint forbid_fired")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(3)).await.unwrap().fired,
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn queue_full_reschedules_then_fails_after_max_attempts(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(
        &pool,
        SchedulerConfig {
            max_attempts: 2,
            retry_queue_full: Duration::from_secs(10),
            ..Default::default()
        },
    );
    scheduled(&pool, &s, "job", now).await;
    for n in 0..100 {
        input(&pool, &s, &format!("input-{n}")).await;
    }
    let due = now + TimeDelta::seconds(3);
    let report = sch.tick(due).await.unwrap();
    assert_eq!(report.retried, 1);
    assert_eq!(
        report.next_due.unwrap().timestamp_millis(),
        (due + TimeDelta::seconds(10)).timestamp_millis()
    );
    assert_eq!(
        count(&pool, "select attempts::bigint from scheduled_jobs").await,
        1
    );
    assert_eq!(
        sch.tick(due + TimeDelta::seconds(10)).await.unwrap().failed,
        1
    );
    let result: Value =
        sqlx::query_scalar("select result from scheduled_jobs where state='failed' and attempts=2")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(result, json!({"error":"queue_full"}));
    assert_eq!(count(&pool, "select count(*) from inputs").await, 100);
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrations = "./migrations")]
async fn edit_racing_with_fire_returns_not_pending(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    let m = scheduled(&pool, &s, "one", now).await;
    let paused = sch.faults.pause_next(Point::SchedulerBeforeSubmit);
    let fire = tokio::spawn(async move { sch.tick(now + TimeDelta::seconds(3)).await.unwrap() });
    let resume = tokio::time::timeout(Duration::from_secs(3), paused)
        .await
        .unwrap()
        .unwrap();
    let p = pool.clone();
    let owner = s.owner_id;
    let edit = tokio::spawn(async move {
        scheduler::update_message(
            &p,
            owner,
            m.id,
            "edit",
            PatchScheduled {
                run_at: None,
                content: Some(content()),
            },
            now,
            Uuid::now_v7(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!edit.is_finished());
    resume.send(()).unwrap();
    assert_eq!(fire.await.unwrap().fired, 1);
    assert!(matches!(
        edit.await.unwrap(),
        Err(ScheduleError::NotPending)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn fired_message_respects_pause_and_holds(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    scheduled(&pool, &s, "one", now).await;
    sqlx::query("update queue_state set pause_reasons='[{\"reason\":\"user_stop\"}]',hold_reasons='[{\"reason\":\"weekly_reserve\",\"provider\":\"claude\",\"resets_at\":null}]' where conversation_id=$1").bind(s.conversation_id).execute(&pool).await.unwrap();
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(3)).await.unwrap().fired,
        1
    );
    assert!(matches!(
        dispatch_next(
            &pool,
            s.conversation_id,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7()
        )
        .await
        .unwrap(),
        DispatchOutcome::Blocked(_)
    ));
    assert_eq!(
        count(&pool, "select count(*) from inputs where state='queued'").await,
        1
    );
    assert_eq!(count(&pool, "select count(*) from executions").await, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn scheduler_sleeps_until_next_due_and_wakes_on_new_job(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(
        &pool,
        SchedulerConfig {
            idle_poll: Duration::from_secs(3600),
            ..Default::default()
        },
    );
    let late = scheduled(&pool, &s, "late", now).await;
    scheduler::update_message(
        &pool,
        s.owner_id,
        late.id,
        "later",
        PatchScheduled {
            run_at: Some(now + TimeDelta::hours(1)),
            content: None,
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sch.notify();
    sch.wait_for_due(Some(now + TimeDelta::hours(1)), now).await;
    tokio::time::pause();
    let waiter = sch.clone();
    let task = tokio::spawn(async move {
        waiter
            .wait_for_due(Some(now + TimeDelta::seconds(60)), now)
            .await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    task.await.unwrap();
    let waiter = sch.clone();
    let task = tokio::spawn(async move {
        waiter
            .wait_for_due(Some(now + TimeDelta::hours(1)), now)
            .await
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    sch.notify();
    task.await.unwrap();
    tokio::time::resume();
    let (stop, rx) = watch::channel(false);
    let runner = tokio::spawn(sch.run(rx));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(count(&pool, "select count(*) from inputs").await, 0);
    let early = scheduled(&pool, &s, "early", Utc::now()).await;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if count(&pool, "select count(*) from inputs").await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let fired: Uuid = sqlx::query_scalar("select id from scheduled_jobs where state='fired'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(fired, early.id);
    stop.send(true).unwrap();
    runner.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn idempotent_schedule_returns_the_same_job(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    let a = scheduled(&pool, &s, "one", now).await;
    let b = scheduled(&pool, &s, "one", now).await;
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        serde_json::to_value(&b).unwrap()
    );
    sch.tick(now + TimeDelta::seconds(3)).await.unwrap();
    let replay = scheduler::schedule_message(
        &pool,
        s.owner_id,
        "one",
        NewScheduled {
            conversation_id: s.conversation_id,
            run_at: a.run_at,
            content: content(),
            attachment_ids: vec![],
        },
        now + TimeDelta::days(2),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(replay.state, JobState::Pending);
    assert_eq!(replay.id, a.id);
    assert!(matches!(
        scheduler::schedule_message(
            &pool,
            s.owner_id,
            "one",
            NewScheduled {
                conversation_id: s.conversation_id,
                run_at: now + TimeDelta::seconds(5),
                content: content(),
                attachment_ids: vec![]
            },
            now,
            Uuid::now_v7()
        )
        .await,
        Err(ScheduleError::IdempotencyConflict)
    ));
    assert_eq!(count(&pool, "select count(*) from scheduled_jobs").await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn foreign_job_ids_are_not_found(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let other = testkit::seed(&pool).await;
    let now = Utc::now();
    let m = scheduled(&pool, &s, "one", now).await;
    assert!(matches!(
        scheduler::cancel_message(&pool, other.owner_id, m.id, "cancel", Uuid::now_v7()).await,
        Err(ScheduleError::NotFound)
    ));
    assert!(matches!(
        scheduler::update_message(
            &pool,
            other.owner_id,
            m.id,
            "edit",
            PatchScheduled {
                run_at: None,
                content: Some(content())
            },
            now,
            Uuid::now_v7()
        )
        .await,
        Err(ScheduleError::NotFound)
    ));
    assert!(
        scheduler::list_messages(
            &mut pool.acquire().await.unwrap(),
            other.owner_id,
            s.conversation_id
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert!(matches!(
        scheduler::schedule_message(
            &pool,
            other.owner_id,
            "new",
            NewScheduled {
                conversation_id: s.conversation_id,
                run_at: m.run_at,
                content: content(),
                attachment_ids: vec![]
            },
            now,
            Uuid::now_v7()
        )
        .await,
        Err(ScheduleError::NotFound)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn maintenance_purges_old_events_and_snapshots(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default()).with_history_days(10);
    let m = scheduled(&pool, &s, "one", now).await;
    scheduler::cancel_message(&pool, s.owner_id, m.id, "cancel", Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query("update scheduled_jobs set updated_at=clock_timestamp()-interval '31 days'")
        .execute(&pool)
        .await
        .unwrap();
    notice(&pool, s.conversation_id, "old").await;
    sqlx::query(
        "update events set committed_at=clock_timestamp()-interval '8 days' where kind='old'",
    )
    .execute(&pool)
    .await
    .unwrap();
    for days in [5, 11] {
        sqlx::query("insert into limit_snapshots(provider,window_kind,used_permille,observed_at,received_at,source) values('claude','weekly',1,clock_timestamp()-make_interval(days=>$1),clock_timestamp()-make_interval(days=>$1),'poll')").bind(days).execute(&pool).await.unwrap();
    }
    sch.maintenance().await.unwrap();
    assert_eq!(
        count(&pool, "select count(*) from events where kind='old'").await,
        0
    );
    assert_eq!(count(&pool, "select count(*) from scheduled_jobs").await, 0);
    assert_eq!(
        count(&pool, "select count(*) from limit_snapshots").await,
        1
    );
    assert_eq!(
        events::purge_older_than(&pool, Duration::from_secs(7 * 86400))
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn scheduler_restart_with_a_due_job_fires_it_exactly_once(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let sch = scheduler(&pool, Default::default());
    scheduled(&pool, &s, "one", now).await;
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(sch.run(rx));
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(true).unwrap();
    task.await.unwrap();
    sqlx::query("update scheduled_jobs set run_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let sch = scheduler(&pool, Default::default());
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(sch.run(rx));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if count(&pool, "select count(*) from inputs").await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(
        scheduler(&pool, Default::default())
            .tick(Utc::now())
            .await
            .unwrap()
            .fired,
        0
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from idempotency_keys where key like 'sched:%'"
        )
        .await,
        1
    );
}

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(vec![]));
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(Capture(buffer.clone()))
                .finish(),
        )
        .unwrap();
        buffer
    })
}

#[sqlx::test(migrations = "./migrations")]
async fn db_error_log_has_no_row_values(pool: PgPool) {
    logs();
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let marker = format!("private_payload_{}", Uuid::now_v7());
    sqlx::query(
        "alter table scheduled_jobs add constraint forced_payload_check check (attempts<0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let error = scheduler::schedule_message(
        &pool,
        s.owner_id,
        "bad",
        NewScheduled {
            conversation_id: s.conversation_id,
            run_at: now + TimeDelta::seconds(2),
            content: vec![canterlot_hub::delivery::ContentBlock::Text {
                text: marker.clone(),
            }],
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    let debug = format!("{error:?}");
    let _ = http::error::ApiError::from(error);
    let output = String::from_utf8(logs().lock().unwrap().clone()).unwrap();
    assert!(output.contains("23514"));
    assert!(output.contains("forced_payload_check"));
    assert!(!output.contains(&marker));
    assert!(!debug.contains(&marker));
}

#[sqlx::test(migrations = "./migrations")]
async fn scheduled_endpoints_roundtrip_with_events(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let s = testkit::seed(&pool).await;
    let conv = testkit::seed_conversation(&pool, owner, s.machine_id).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{conv}/scheduled");
    let response=client.send(Method::POST,&path,Some("create"),Some(json!({"run_at":Utc::now()+TimeDelta::hours(1),"content":content(),"attachment_ids":[]}))).await;
    assert_eq!(response.status, StatusCode::CREATED);
    let m: Value = response.json();
    let id = m["id"].as_str().unwrap();
    let list = client.send(Method::GET, &path, None, None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json::<Value>()[0]["id"], id);
    let path = format!("/api/v1/scheduled/{id}");
    let edit = client
        .send(
            Method::PATCH,
            &path,
            Some("edit"),
            Some(json!({"content":[{"type":"text","text":"updated"}]})),
        )
        .await;
    assert_eq!(edit.status, StatusCode::OK);
    let cancel = client
        .send(Method::DELETE, &path, Some("cancel"), None)
        .await;
    assert_eq!(cancel.status, StatusCode::OK);
    assert_eq!(cancel.json::<Value>()["state"], "cancelled");
    let replay = client
        .send(Method::DELETE, &path, Some("cancel"), None)
        .await;
    assert_eq!(replay.body, cancel.body);
    assert_eq!(
        client
            .send(Method::DELETE, &path, Some("new-cancel"), None)
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='scheduled_changed'"
        )
        .await,
        3
    );
    assert_eq!(
        count(&pool, "select count(*) from idempotency_keys").await,
        3
    );
    let payload: Value = sqlx::query_scalar(
        "select payload from events where kind='scheduled_changed' order by scope_seq desc limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payload["state"], "cancelled");
}

use canterlot_hub::events;

#[sqlx::test(migrations = "./migrations")]
async fn simultaneous_replays_succeed_at_the_pending_limit_and_after_cancellation(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    let _scheduler = scheduler(
        &pool,
        SchedulerConfig {
            max_pending_per_conversation: 1,
            ..Default::default()
        },
    );
    let deadlocks = count(
        &pool,
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .await;
    let mut tasks = tokio::task::JoinSet::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(10));
    for _ in 0..10 {
        let p = pool.clone();
        let b = barrier.clone();
        let owner = s.owner_id;
        let conversation_id = s.conversation_id;
        tasks.spawn(async move {
            b.wait().await;
            scheduler::schedule_message(
                &p,
                owner,
                "same",
                NewScheduled {
                    conversation_id,
                    run_at: now + TimeDelta::seconds(2),
                    content: content(),
                    attachment_ids: vec![],
                },
                now,
                Uuid::now_v7(),
            )
            .await
        });
    }
    let mut ids = Vec::new();
    while let Some(result) = tasks.join_next().await {
        ids.push(result.unwrap().unwrap().id);
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    let barrier = Arc::new(tokio::sync::Barrier::new(10));
    let id = ids[0];
    for _ in 0..10 {
        let p = pool.clone();
        let b = barrier.clone();
        let owner = s.owner_id;
        tasks.spawn(async move {
            b.wait().await;
            scheduler::cancel_message(&p, owner, id, "same-cancel", Uuid::now_v7()).await
        });
    }
    while let Some(result) = tasks.join_next().await {
        assert_eq!(result.unwrap().unwrap().state, JobState::Cancelled);
    }
    assert_eq!(count(&pool, "select count(*) from scheduled_jobs").await, 1);
    assert_eq!(
        count(&pool, "select count(*) from idempotency_keys").await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "select deadlocks from pg_stat_database where datname=current_database()"
        )
        .await,
        deadlocks
    );
}
