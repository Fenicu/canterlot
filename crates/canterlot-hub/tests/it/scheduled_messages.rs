//! Отложенные сообщения планировщика: отказ одного задания не держит остальные,
//! пределы содержимого и вложений, уборка истории и ключей, миграция заданий.

use crate::support::task5 as support;
use crate::support::task9 as files;

use canterlot_hub::{
    config::SchedulerConfig,
    conversations::{self, SubmitMessage},
    delivery::{ContentBlock, SubmitError},
    scheduler::{self, NewScheduled},
    testkit,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use support::*;
use uuid::Uuid;

#[tokio::test]
async fn poisoned_message_cannot_block_later_due_jobs() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn deterministic_database_failure_has_bounded_attempts_and_yields_to_other_jobs() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn schedule_and_update_check_metadata_with_attachment_refs() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn maintenance_purges_failed_jobs_and_expired_keys_but_keeps_pending_and_recent() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn maintenance_continues_after_one_purge_fails() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn scheduled_history_keeps_the_newest_terminal_jobs() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn changed_machine_limits_fail_scheduled_attachments_without_blocking_text() {
    let pool = canterlot_hub::testkit::db().await;
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
#[tokio::test]
async fn malformed_wake_and_message_without_conversation_do_not_block_valid_message() {
    let pool = canterlot_hub::testkit::db().await;
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
async fn rereview_input_replay_survives_generic_key_purge() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let request = || SubmitMessage {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        key: "lost-response".into(),
        content: support::content(),
        attachment_ids: vec![],
        retry_of: None,
        received_at: Utc::now(),
    };
    let original = conversations::submit_message(&pool, request(), Uuid::now_v7())
        .await
        .unwrap();
    sqlx::raw_sql(
        "update inputs set state='accepted'; update commands set delivery_state='completed'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update idempotency_keys set created_at=clock_timestamp()-interval '8 days'")
        .execute(&pool)
        .await
        .unwrap();
    support::scheduler(&pool, Default::default())
        .maintenance()
        .await
        .unwrap();
    assert_eq!(
        support::count(&pool, "select count(*) from idempotency_keys").await,
        0
    );
    let replay = conversations::submit_message(&pool, request(), Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        support::count(&pool, "select count(*) from inputs").await,
        1
    );
    assert_eq!(replay, original);
    let replays = futures_util::future::join_all(
        (0..8).map(|_| conversations::submit_message(&pool, request(), Uuid::now_v7())),
    )
    .await;
    for replay in replays {
        assert_eq!(replay.unwrap(), original);
    }
    for change in 0..3 {
        let mut conflict = request();
        match change {
            0 => conflict.attachment_ids = vec![Uuid::now_v7()],
            1 => conflict.retry_of = Some(Uuid::now_v7()),
            _ => {
                conflict.conversation_id =
                    testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await
            }
        }
        assert!(matches!(
            conversations::submit_message(&pool, conflict, Uuid::now_v7()).await,
            Err(SubmitError::IdempotencyConflict)
        ));
    }
    let mut conflict = request();
    conflict.content = vec![ContentBlock::Text {
        text: "changed".into(),
    }];
    assert!(matches!(
        conversations::submit_message(&pool, conflict, Uuid::now_v7()).await,
        Err(SubmitError::IdempotencyConflict)
    ));
}
#[sqlx::test(migrations = false)]
async fn rereview_migration_preserves_old_inputs_and_revives_transient_limit_jobs(pool: PgPool) {
    let old = sqlx::migrate::Migrator {
        migrations: canterlot_hub::db::MIGRATOR
            .iter()
            .filter(|m| m.version < 17)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    let seed = testkit::seed(&pool).await;
    let mut first = Uuid::nil();
    for seq in 1..=2i64 {
        let command = Uuid::now_v7();
        let input = Uuid::now_v7();
        if seq == 1 {
            first = input;
        }
        sqlx::query("insert into commands(id,machine_id,conversation_id,kind,payload,delivery_state) values($1,$2,$3,'deliver_input','{}','completed')")
            .bind(command).bind(seed.machine_id).bind(seed.conversation_id).execute(&pool).await.unwrap();
        sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at) values($1,$2,$3,$4,'legacy-input',$5,'accepted',$6,clock_timestamp())")
            .bind(input).bind(seed.conversation_id).bind(seed.owner_id).bind(seq).bind(serde_json::json!(support::content())).bind(command).execute(&pool).await.unwrap();
    }
    let failed_job = Uuid::now_v7();
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload,state,attempts,result) values($1,'wake',$2,$3,clock_timestamp()+interval '1 day','{}','failed',20,'{\"error\":\"job_error\"}')")
        .bind(failed_job).bind(seed.owner_id).bind(seed.conversation_id).execute(&pool).await.unwrap();
    for (state, hold_key) in [
        ("failed", "already-replaced"),
        ("failed", "already-replaced"),
        ("pending", "already-replaced"),
        ("failed", "failed-twice"),
        ("failed", "failed-twice"),
    ] {
        sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload,state,attempts,result) values($1,'wake',$2,$3,clock_timestamp()+interval '1 day',$4,$5,20,'{\"error\":\"job_error\"}')")
            .bind(Uuid::now_v7()).bind(seed.owner_id).bind(seed.conversation_id).bind(serde_json::json!({"hold_key":hold_key})).bind(state).execute(&pool).await.unwrap();
    }
    canterlot_hub::db::MIGRATOR.run(&pool).await.unwrap();
    let replay = conversations::submit_message(
        &pool,
        SubmitMessage {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            key: "legacy-input".into(),
            content: support::content(),
            attachment_ids: vec![],
            retry_of: None,
            received_at: Utc::now(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(replay.input_id, first);
    assert_eq!(
        support::count(&pool, "select count(*) from inputs").await,
        2
    );
    assert_eq!(
        support::count(
            &pool,
            "select count(*) from inputs where client_idempotency_key='legacy-input'"
        )
        .await,
        1
    );
    let restored: bool = sqlx::query_scalar("select state='pending' and attempts=19 and failures=1 and run_at<=clock_timestamp()+interval '60 seconds' from scheduled_jobs where id=$1").bind(failed_job).fetch_one(&pool).await.unwrap();
    assert!(restored);
    assert_eq!(
        support::count(
            &pool,
            "select count(*) from scheduled_jobs where state='pending'"
        )
        .await,
        3
    );
    let error = sqlx::query("update inputs set client_idempotency_key='legacy-input' where client_idempotency_key is null").execute(&pool).await.unwrap_err();
    assert!(error.as_database_error().unwrap().is_unique_violation());
}
