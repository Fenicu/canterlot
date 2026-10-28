#[path = "support/task5.rs"]
mod support;

use canterlot_hub::{
    config::HubTimers,
    conversations::{self, SubmitMessage},
    delivery::{ContentBlock, SubmitError},
    machine_info,
    session::Registry,
    testkit,
};
use chrono::Utc;
use sqlx::PgPool;
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

async fn singleton_pid(pool: &PgPool, previous: i32) -> i32 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar("select pid from pg_locks where locktype='advisory' and granted and database=(select oid from pg_database where datname=current_database()) and classid::bigint=((hashtextextended('canterlot:machine_status',0)>>32)&4294967295) and objid::bigint=(hashtextextended('canterlot:machine_status',0)&4294967295) and pid<>$1")
                .bind(previous).fetch_optional(pool).await.unwrap();
            if let Some(pid) = pid { break pid; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_machine_status_reacquire_does_not_publish_unavailable(pool: PgPool) {
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
        let pid = singleton_pid(&pool, 0).await;
        loop {
            if support::count(&pool, "select count(*) from machines where availability='available'").await == 1 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let cursor = support::count(&pool, "select coalesce(max(scope_seq),0) from events where kind='machine_availability_changed'").await;
        sqlx::query("select pg_terminate_backend($1)").bind(pid).execute(&pool).await.unwrap();
        singleton_pid(&pool, pid).await;
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

#[sqlx::test(migrations = "./migrations")]
async fn rereview_failing_start_projection_keeps_exponential_backoff(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create sequence projection_attempts; create function fail_projection() returns trigger language plpgsql as $$ begin perform nextval('projection_attempts'); raise exception using errcode='57014',message='projection unavailable'; end $$; create trigger fail_projection before update on machines for each row execute function fail_projection();")
        .execute(&pool).await.unwrap();
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        Registry::new(Default::default()),
        rx,
    ));
    tokio::time::sleep(Duration::from_secs(5)).await;
    let _ = stop.send(true);
    task.await.unwrap();
    let attempts: i64 = sqlx::query_scalar("select last_value from projection_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        (2..=5).contains(&attempts),
        "projection reconnected {attempts} times in 5 s"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_input_replay_survives_generic_key_purge(pool: PgPool) {
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
