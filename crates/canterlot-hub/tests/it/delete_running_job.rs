//! Удаление разговора не ждёт строку задания, которую держит планировщик: под блокировкой
//! очереди такое ожидание замыкает цикл с заданием, которое ждёт ту же очередь.

use std::time::Duration;

use canterlot_hub::{deletion, testkit};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_backs_off_from_a_job_the_scheduler_holds(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let id = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let job = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'message',$2,$3,now(),'{}')",
    )
    .bind(job)
    .bind(seed.owner_id)
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let (_, blobs) = testkit::test_blob_store(directory.path());

    // Планировщик захватил задание и потом возьмёт очередь разговора.
    let mut scheduler = pool.begin().await.unwrap();
    sqlx::query("select id from scheduled_jobs where id=$1 for update")
        .bind(job)
        .execute(&mut *scheduler)
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        deletion::delete(
            &pool,
            blobs.as_ref(),
            seed.owner_id,
            id,
            "busy-delete",
            Uuid::now_v7(),
        ),
    )
    .await
    .expect("delete must not wait for the job row");
    assert!(
        matches!(outcome, Err(deletion::DeleteError::Busy)),
        "{outcome:?}"
    );
    // Очередь разговора свободна: задание доходит до неё без взаимоблокировки.
    sqlx::query(
        "select conversation_id from queue_state where conversation_id=$1 for update nowait",
    )
    .bind(id)
    .execute(&mut *scheduler)
    .await
    .unwrap();
    scheduler.commit().await.unwrap();

    deletion::delete(
        &pool,
        blobs.as_ref(),
        seed.owner_id,
        id,
        "busy-delete",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let left: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!left);
}
