use crate::support::task5 as support;
use crate::support::task9 as files;

use axum::http::Method;
use canterlot_hub::{
    activity,
    attachments::{self, NewAttachment},
    config::SchedulerConfig,
    conversations,
    delivery::control,
    events::{self, NewEvent, Scope},
    groups,
    http::{self, AppState},
    idempotency,
    media::MediaError,
    scheduler::{self, NewScheduled},
    testkit::{self, TestClient},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::sync::watch;
use uuid::Uuid;

async fn blocked_by(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname=current_database() and $1=any(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }).await.unwrap();
}
async fn lock_pid(pool: &PgPool, service: &str, previous: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar("select pid from pg_locks where locktype='advisory' and granted and database=(select oid from pg_database where datname=current_database()) and classid::bigint=((hashtextextended($1,0)>>32)&4294967295) and objid::bigint=(hashtextextended($1,0)&4294967295) and pid<>$2")
                .bind(format!("canterlot:{service}")).bind(previous).fetch_optional(pool).await.unwrap();
            if let Some(pid)=pid { break pid; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn singleton_scheduler_reacquires_after_backend_termination(pool: PgPool) {
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
    let pid = lock_pid(&pool, "scheduler", 0).await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(12), async {
        let new_pid = lock_pid(&pool, "scheduler", pid).await;
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

#[sqlx::test(migrations = "./migrations")]
async fn poisoned_message_cannot_block_later_due_jobs(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let sch = scheduler(
        &pool,
        SchedulerConfig {
            max_attempts: 2,
            retry_queue_full: Duration::from_secs(1),
            ..Default::default()
        },
    );
    let now = Utc::now();
    let poison = scheduled(&pool, &s, "poison", now).await;
    let valid = scheduled(&pool, &s, "valid", now + TimeDelta::seconds(1)).await;
    sqlx::query("update scheduled_jobs set payload='{}' where id=$1")
        .bind(poison.id)
        .execute(&pool)
        .await
        .unwrap();
    for n in 0..3 {
        assert!(
            sch.tick(valid.run_at + TimeDelta::seconds(n * 60))
                .await
                .is_ok()
        );
    }
    let (state, result): (String, Value) =
        sqlx::query_as("select state,result from scheduled_jobs where id=$1")
            .bind(poison.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert!(result["error"].is_string());
    assert_eq!(count(&pool, "select count(*) from inputs").await, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn deterministic_database_failure_has_bounded_attempts_and_yields_to_other_jobs(
    pool: PgPool,
) {
    let s = testkit::seed(&pool).await;
    let sch = scheduler(
        &pool,
        SchedulerConfig {
            max_attempts: 2,
            retry_queue_full: Duration::from_secs(1),
            ..Default::default()
        },
    );
    let now = Utc::now();
    let bad = scheduled(&pool, &s, "bad", now).await;
    let good = scheduled(&pool, &s, "good", now + TimeDelta::seconds(1)).await;
    sqlx::query(
        "update scheduled_jobs set payload=payload||'{\"poison\":true}'::jsonb where id=$1",
    )
    .bind(bad.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql("create function poison_input() returns trigger language plpgsql as $$ begin if new.idempotency_key=(select 'sched:'||id::text from scheduled_jobs where payload->>'poison'='true') then raise exception using errcode='23514',message='rejected'; end if; return new; end $$; create trigger poison_input before insert on inputs for each row execute function poison_input();").execute(&pool).await.unwrap();
    assert!(sch.tick(good.run_at).await.is_ok());
    assert_eq!(count(&pool, "select count(*) from inputs").await, 1);
    assert!(sch.tick(good.run_at + TimeDelta::minutes(1)).await.is_ok());
    let (state, attempts): (String, i32) =
        sqlx::query_as("select state,attempts from scheduled_jobs where id=$1")
            .bind(bad.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(attempts, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn schedule_and_update_check_metadata_with_attachment_refs(pool: PgPool) {
    let f = files::Fixture::new(pool).await;
    let mut ids = Vec::new();
    for n in 0..10 {
        let id = f
            .create(
                &format!("{n}{}.bin", "x".repeat(240)),
                "application/octet-stream",
                b"x",
            )
            .await;
        assert_eq!(f.patch_bytes(id, 0, b"x").await.status, 204);
        assert_eq!(
            f.complete(id, &format!("complete-{n}"), None).await.status,
            200
        );
        ids.push(id);
    }
    let now = Utc::now();
    let sch = scheduler(&f.pool, Default::default());
    let crowded = vec![canterlot_hub::delivery::ContentBlock::Text { text: "x".into() }; 10500];
    let request = NewScheduled {
        conversation_id: f.conversation,
        run_at: now + TimeDelta::seconds(2),
        content: crowded.clone(),
        attachment_ids: ids.clone(),
    };
    let result =
        scheduler::schedule_message(&f.pool, f.owner, "too-large", request, now, Uuid::now_v7())
            .await;
    assert!(matches!(
        result,
        Err(scheduler::ScheduleError::InvalidContent(_))
    ));
    let m = scheduler::schedule_message(
        &f.pool,
        f.owner,
        "small",
        NewScheduled {
            conversation_id: f.conversation,
            run_at: now + TimeDelta::seconds(2),
            content: content(),
            attachment_ids: ids,
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let result = scheduler::update_message(
        &f.pool,
        f.owner,
        m.id,
        "too-large-update",
        scheduler::PatchScheduled {
            run_at: None,
            content: Some(crowded.clone()),
        },
        now,
        Uuid::now_v7(),
    )
    .await;
    assert!(matches!(
        result,
        Err(scheduler::ScheduleError::InvalidContent(_))
    ));
    sqlx::query("update scheduled_jobs set payload=jsonb_set(payload,'{content}',$2) where id=$1")
        .bind(m.id)
        .bind(json!(crowded))
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(sch.tick(m.run_at).await.unwrap().failed, 1);
    assert_eq!(count(&f.pool, "select count(*) from inputs").await, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn settings_promotion_and_attachment_fk_have_no_deadlocks_50_times(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    input(&pool, &s, "settings").await;
    let command: Uuid = sqlx::query_scalar("select id from commands where kind='deliver_input'")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update commands set payload=payload||'{\"settings\":{\"model\":\"synthetic-small\"}}'::jsonb where id=$1").bind(command).execute(&pool).await.unwrap();
    let before = count(
        &pool,
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .await;
    for n in 0..50 {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
            .bind(s.conversation_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        events::append(
            &mut tx,
            NewEvent {
                scope: Scope::Conversation {
                    id: s.conversation_id,
                },
                kind: "block_started",
                conversation_id: Some(s.conversation_id),
                execution_id: None,
                payload: json!({}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        let p = pool.clone();
        let owner = s.owner_id;
        let conversation = s.conversation_id;
        let b = tokio::spawn(async move {
            attachments::create(
                &p,
                owner,
                &format!("fk-{n}"),
                NewAttachment {
                    conversation_id: conversation,
                    name: "f.bin".into(),
                    mime: Some("application/octet-stream".into()),
                    size: 1,
                    sha256: "a".repeat(64),
                },
                Uuid::now_v7(),
            )
            .await
        });
        blocked_by(&pool, pid).await;
        let a = conversations::promote_settings_in_tx(&mut tx, command, Uuid::now_v7()).await;
        if a.is_ok() {
            tx.commit().await.unwrap();
        } else {
            tx.rollback().await.unwrap();
        }
        let b = b.await.unwrap();
        assert!(a.is_ok(), "promotion failed on iteration {n}");
        assert!(b.is_ok(), "attachment failed on iteration {n}");
    }
    sqlx::query("select pg_stat_clear_snapshot()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "select deadlocks from pg_stat_database where datname=current_database()"
        )
        .await,
        before
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn unchanged_refresh_advances_repair_watermark_without_events(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    notice(&pool, s.conversation_id, "conversation_updated").await;
    let events = count(&pool, "select count(*) from events").await;
    assert_eq!(
        refresh(&pool, s.conversation_id).await,
        activity::Refresh::Unchanged
    );
    let repaired:bool=sqlx::query_scalar("select d.updated_at >= (select max(committed_at) from events where kind='conversation_updated') from conversation_derived d where conversation_id=$1").bind(s.conversation_id).fetch_one(&pool).await.unwrap();
    assert!(repaired);
    assert_eq!(count(&pool, "select count(*) from events").await, events);
}

#[sqlx::test(migrations = "./migrations")]
async fn mark_read_uses_configured_activity_thresholds(pool: PgPool) {
    let (s, _) = running(&pool).await;
    let (mut deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    deps.installation.owner_id = s.owner_id;
    deps.activity.stalled_generating = Duration::from_secs(3600);
    activity::refresh(
        &pool,
        s.conversation_id,
        Utc::now(),
        &activity::Thresholds::from(&deps.activity),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let before = count(
        &pool,
        "select count(*) from events where kind='derived_changed'",
    )
    .await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let response = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/read", s.conversation_id),
            None,
            Some(json!({"seq":"0"})),
        )
        .await;
    assert_eq!(response.status, 204);
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_state"],
        "working"
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='derived_changed'"
        )
        .await,
        before
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn auto_title_counts_follow_prior_list_commit(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let other = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set title_source='auto' where id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut prior = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *prior)
        .await
        .unwrap();
    sqlx::query("update conversations set archived_at=clock_timestamp() where id=$1")
        .bind(other)
        .execute(&mut *prior)
        .await
        .unwrap();
    events::append(
        &mut prior,
        NewEvent {
            scope: Scope::Conversations {
                owner_id: s.owner_id,
            },
            kind: "conversation_updated",
            conversation_id: Some(other),
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let p = pool.clone();
    let writer = tokio::spawn(async move {
        conversations::submit_message(
            &p,
            conversations::SubmitMessage {
                owner_id: s.owner_id,
                conversation_id: s.conversation_id,
                key: "auto-title".into(),
                content: content(),
                attachment_ids: vec![],
                retry_of: None,
                received_at: Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    });
    blocked_by(&pool, pid).await;
    prior.commit().await.unwrap();
    writer.await.unwrap();
    let counts:Value=sqlx::query_scalar("select payload->'counts' from events where scope like 'owner:%:conversations' order by scope_seq desc limit 1").fetch_one(&pool).await.unwrap();
    assert_eq!(counts["archived"], 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn group_delete_and_queue_resume_conform_to_public_schema(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let g = groups::create_group(&pool, s.owner_id, "group", "g", None, Uuid::now_v7())
        .await
        .unwrap();
    groups::delete_group(&pool, s.owner_id, g.id, "delete-group", Uuid::now_v7())
        .await
        .unwrap();
    control::request_stop(&pool, s.owner_id, s.conversation_id, "stop", Uuid::now_v7())
        .await
        .unwrap();
    control::continue_queue(
        &pool,
        s.owner_id,
        s.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    let rows: Vec<events::StoredEvent> = sqlx::query_as(
        "select * from events where kind in ('group_changed','queue_resumed') order by scope_seq",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for e in rows {
        assert!(events::is_client_visible(&e.kind), "{} is hidden", e.kind);
        let mut validator = schema.clone();
        validator["$ref"] = json!(format!("#/$defs/{}", e.kind));
        validator.as_object_mut().unwrap().remove("oneOf");
        let validator = jsonschema::validator_for(&validator).unwrap();
        let value = serde_json::to_value(events::project_for_client(vec![e]).remove(0)).unwrap();
        assert!(validator.is_valid(&value));
        let mut server = schema.clone();
        server["$ref"] = json!("#/$defs/ServerOp");
        server.as_object_mut().unwrap().remove("oneOf");
        let server = jsonschema::validator_for(&server).unwrap();
        assert!(server.is_valid(&json!({"op":"event","id":"s","event":value})));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn maintenance_purges_failed_jobs_and_expired_keys_but_keeps_pending_and_recent(
    pool: PgPool,
) {
    let s = testkit::seed(&pool).await;
    let sch = scheduler(&pool, Default::default());
    let now = Utc::now();
    for (key, state, age) in [
        ("old-failed", "failed", 31),
        ("recent-failed", "failed", 29),
        ("old-pending", "pending", 31),
    ] {
        let m = scheduled(&pool, &s, key, now).await;
        sqlx::query("update scheduled_jobs set state=$2,updated_at=clock_timestamp()-$3*interval '1 day' where id=$1").bind(m.id).bind(state).bind(age as f64).execute(&pool).await.unwrap();
    }
    sqlx::query("update idempotency_keys set created_at=clock_timestamp()-interval '8 days' where key='old-failed'").execute(&pool).await.unwrap();
    sch.maintenance().await.unwrap();
    assert_eq!(count(&pool, "select count(*) from scheduled_jobs").await, 2);
    assert_eq!(
        count(&pool, "select count(*) from idempotency_keys").await,
        2
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn maintenance_continues_after_one_purge_fails(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let sch = scheduler(&pool, Default::default());
    let m = scheduled(&pool, &s, "purge", Utc::now()).await;
    sqlx::query("update scheduled_jobs set state='cancelled',updated_at=clock_timestamp()-interval '31 days'").execute(&pool).await.unwrap();
    sqlx::query("update idempotency_keys set created_at=clock_timestamp()-interval '8 days'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create function fail_event_purge() returns trigger language plpgsql as $$ begin raise exception 'purge_failed'; end $$; create trigger fail_event_purge before delete on events for each statement execute function fail_event_purge();").execute(&pool).await.unwrap();
    assert!(sch.maintenance().await.is_err());
    let remains: bool =
        sqlx::query_scalar("select exists(select 1 from scheduled_jobs where id=$1)")
            .bind(m.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!remains);
    assert_eq!(
        count(&pool, "select count(*) from idempotency_keys").await,
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn sensitive_debug_redacts_stored_response_and_database_detail(pool: PgPool) {
    let marker = "private_regression_marker";
    let replay = idempotency::Begin::Replay(json!({"content":marker}));
    assert!(!format!("{replay:?}").contains(marker));
    let error=sqlx::raw_sql("do $$ begin raise exception using errcode='23514',message='rejected',detail='private_regression_marker'; end $$").execute(&pool).await.unwrap_err();
    let debug = format!("{:?}", MediaError::Db(error));
    assert!(!debug.contains(marker));
    assert!(debug.contains("23514"));
}

#[test]
fn blob_shards_use_random_uuid_tail() {
    let dir = tempfile::tempdir().unwrap();
    let config = canterlot_hub::config::AttachmentsConfig {
        blob_dir: dir.path().into(),
        ..Default::default()
    };
    let store =
        attachments::LocalBlobStore::open(&config, Arc::new(attachments::blobstore::NoTrace))
            .unwrap();
    let a = Uuid::parse_str("019a0000-0000-7000-8000-0000000000ab").unwrap();
    let b = Uuid::parse_str("019a0000-0000-7000-8000-0000000000cd").unwrap();
    assert_ne!(store.blob_path(a).parent(), store.blob_path(b).parent());
}

#[sqlx::test(migrations = "./migrations")]
async fn readyz_names_an_unexpectedly_stopped_singleton(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let sch = deps.scheduler.clone();
    let client = TestClient::in_process(http::router(AppState::new(deps)));
    let (_stop, rx) = watch::channel(false);
    let task = tokio::spawn(sch.run(rx));
    lock_pid(&pool, "scheduler", 0).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let response = client.send(Method::GET, "/readyz", None, None).await;
    assert_eq!(response.status, 503);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.body).unwrap()["services"],
        json!(["scheduler"])
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn orphan_gc_batches_catalog_queries(pool: PgPool) {
    use canterlot_hub::attachments::BlobStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = files::Fixture::new(pool.clone()).await;
    let queries = Arc::new(AtomicUsize::new(0));
    let counter = queries.clone();
    let probe = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .before_acquire(move |_, _| {
            counter.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(true) })
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    for _ in 0..200 {
        let id = Uuid::now_v7();
        let path = f.staging_path(id);
        std::fs::write(&path, b"x").unwrap();
        std::fs::File::open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(90000))
            .unwrap();
    }
    let report =
        attachments::gc::run_once(&probe, &*f.store, Utc::now(), Duration::from_secs(86400))
            .await
            .unwrap();
    assert_eq!(report.orphan_staging, 200);
    assert!(f.store.scan().await.unwrap().is_empty());
    let total = queries.load(Ordering::Relaxed);
    assert!(
        total < 15,
        "orphan scan used {total} pool queries for 200 files"
    );
    probe.close().await;
}

#[sqlx::test(migrations = "./migrations")]
async fn singleton_activity_recreates_projection_after_lock_loss(pool: PgPool) {
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
    let pid = lock_pid(&pool, "activity", 0).await;
    until_state(&pool, s.conversation_id, "idle").await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    lock_pid(&pool, "activity", pid).await;
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

#[sqlx::test(migrations = "./migrations")]
async fn singleton_gc_stays_passive_then_recovers_and_collects(pool: PgPool) {
    let f = files::Fixture::new(pool.clone()).await;
    let held =
        canterlot_hub::singleton::Singleton::acquire(&pool.connect_options(), "attachment-gc")
            .await
            .unwrap()
            .unwrap();
    let id = Uuid::now_v7();
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
    let pid = lock_pid(&pool, "attachment-gc", 0).await;
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
    lock_pid(&pool, "attachment-gc", pid).await;
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

#[sqlx::test(migrations = "./migrations")]
async fn singleton_status_retries_failed_initial_projection(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create function fail_initial_status() returns trigger language plpgsql as $$ begin raise exception 'projection_failed'; end $$; create trigger fail_initial_status before update on machines for each row execute function fail_initial_status();").execute(&pool).await.unwrap();
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(canterlot_hub::machine_info::run_status_projection(
        pool.clone(),
        canterlot_hub::session::Registry::new(Default::default()),
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
    let pid = lock_pid(&pool, "machine_status", 0).await;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    lock_pid(&pool, "machine_status", pid).await;
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

#[sqlx::test(migrations = "./migrations")]
async fn scheduled_history_keeps_the_newest_terminal_jobs(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let ids: Vec<_> = (0..205).map(|_| Uuid::now_v7()).collect();
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,state,payload,updated_at) select id,'message',$2,$3,clock_timestamp()+n*interval '1 second','failed','{\"content\":[],\"attachment_ids\":[]}',clock_timestamp()+n*interval '1 second' from unnest($1::uuid[]) with ordinality t(id,n)")
        .bind(&ids).bind(s.owner_id).bind(s.conversation_id).execute(&pool).await.unwrap();
    let list = scheduler::list_messages(
        &mut pool.acquire().await.unwrap(),
        s.owner_id,
        s.conversation_id,
    )
    .await
    .unwrap();
    assert_eq!(list.len(), 200);
    assert!(list.iter().any(|job| job.id == ids[204]));
    assert!(!list.iter().any(|job| job.id == ids[0]));
}

#[sqlx::test(migrations = "./migrations")]
async fn mark_read_preserves_configured_metrics_freshness(pool: PgPool) {
    let (s, execution) = running(&pool).await;
    sqlx::query("update executions set phase_at=clock_timestamp()-interval '31 minutes',last_event_at=clock_timestamp()-interval '31 minutes' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '31 minutes' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let (mut deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    deps.installation.owner_id = s.owner_id;
    deps.machines.metrics_stale_after = Duration::from_secs(3600);
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp()-interval '5 minutes',1,1,50,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id).bind(json!([{"execution_id":execution,"alive":true,"cpu_permille":0,"descendants":0}])).execute(&pool).await.unwrap();
    activity::refresh_with_metrics(
        &pool,
        s.conversation_id,
        Utc::now(),
        &activity::Thresholds::from(&deps.activity),
        deps.machines.metrics_stale_after,
        true,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let before = row(&pool, s.conversation_id).await["activity_evidence"].clone();
    assert_eq!(before["cpu_busy"], false);
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    assert_eq!(
        client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{}/read", s.conversation_id),
                None,
                Some(json!({"seq":"0"}))
            )
            .await
            .status,
        204
    );
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_evidence"],
        before
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn changed_machine_limits_fail_scheduled_attachments_without_blocking_text(pool: PgPool) {
    let f = files::Fixture::new(pool).await;
    let id = f.ready("f.bin", "application/octet-stream", b"x").await;
    let now = Utc::now();
    let sch = scheduler(&f.pool, Default::default());
    let bad = scheduler::schedule_message(
        &f.pool,
        f.owner,
        "attachment",
        NewScheduled {
            conversation_id: f.conversation,
            run_at: now + TimeDelta::seconds(2),
            content: content(),
            attachment_ids: vec![id],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    scheduler::schedule_message(
        &f.pool,
        f.owner,
        "text",
        NewScheduled {
            conversation_id: f.conversation,
            run_at: now + TimeDelta::seconds(3),
            content: content(),
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sqlx::query("update machines set limits_ok=false where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let report = sch.tick(now + TimeDelta::seconds(3)).await.unwrap();
    assert_eq!((report.failed, report.fired), (1, 1));
    let reason: String =
        sqlx::query_scalar("select result->>'error' from scheduled_jobs where id=$1")
            .bind(bad.id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(reason, "limits_mismatch");
}

#[sqlx::test(migrations = "./migrations")]
async fn malformed_wake_and_message_without_conversation_do_not_block_valid_message(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let sch = scheduler(&pool, Default::default());
    let now = Utc::now();
    let broken = scheduled(&pool, &s, "no-conversation", now).await;
    sqlx::query("update scheduled_jobs set conversation_id=null where id=$1")
        .bind(broken.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'wake',$2,$3,$4,'{}')").bind(Uuid::now_v7()).bind(s.owner_id).bind(s.conversation_id).bind(now).execute(&pool).await.unwrap();
    scheduled(&pool, &s, "valid", now).await;
    let report = sch.tick(now + TimeDelta::seconds(3)).await.unwrap();
    assert_eq!((report.failed, report.fired), (2, 1));
}

#[tokio::test]
async fn legacy_blob_shards_remain_readable_and_removable() {
    use canterlot_hub::attachments::BlobStore;
    let dir = tempfile::tempdir().unwrap();
    let config = canterlot_hub::config::AttachmentsConfig {
        blob_dir: dir.path().into(),
        ..Default::default()
    };
    let store =
        attachments::LocalBlobStore::open(&config, Arc::new(attachments::blobstore::NoTrace))
            .unwrap();
    let id = Uuid::parse_str("019a0000-0000-7000-8000-0000000000ab").unwrap();
    let legacy = config.blob_dir.join("blobs/01").join(id.to_string());
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"x").unwrap();
    let (_, len) = store.open(id, 0).await.unwrap();
    assert_eq!(len, 1);
    store.delete_blob(id).await.unwrap();
    assert!(!legacy.exists());
}
