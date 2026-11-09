//! Сборщик вложений не застревает на тысяче вложений, которые держат черновики:
//! они не попадают в пачку кандидатов и не закрывают дорогу свободным.

use std::time::Duration;

use canterlot_hub::{attachments::gc, testkit};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn draft_held_attachments_do_not_starve_the_gc(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    for n in 0..100i64 {
        let conversation = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        // Десять готовых вложений черновика с ID меньше свободного.
        sqlx::query(
            "with made as (
               insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at,updated_at)
               select format('00000000-0000-7000-8000-%s', lpad(($3*10+k)::text,12,'0'))::uuid,
                      $1,$2,'d.txt','text/plain',1,repeat('0',64),'ready',now(),now()-interval '2 days'
                 from generate_series(0,9) k
               returning id)
             insert into composer_drafts (conversation_id,owner_id,text,attachment_ids,revision)
             select $2,$1,'',jsonb_agg(id::text),1 from made",
        )
        .bind(seed.owner_id)
        .bind(conversation)
        .bind(n)
        .execute(&pool)
        .await
        .unwrap();
    }
    let free = Uuid::parse_str("ffffffff-ffff-7fff-bfff-ffffffffffff").unwrap();
    sqlx::query(
        "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at,updated_at)
         values ($1,$2,$3,'f.txt','text/plain',1,repeat('0',64),'ready',now(),now()-interval '2 days')",
    )
    .bind(free)
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();

    let directory = tempfile::tempdir().unwrap();
    let (_, blobs) = testkit::test_blob_store(directory.path());
    let report = gc::run_once(
        &pool,
        blobs.as_ref(),
        Utc::now(),
        Duration::from_secs(86400),
    )
    .await
    .unwrap();
    assert_eq!(report.released, 1, "{report:?}");
    let state: String = sqlx::query_scalar("select state from attachments where id=$1")
        .bind(free)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "released");
}
