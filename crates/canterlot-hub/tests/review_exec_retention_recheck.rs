//! Review: `purge_accepted_retention` selects candidates without locks and then calls
//! `deletion::delete_retained`, which re-checks only `trashed_at` under the lock. If the owner
//! pins or un-archives the delegate between selection and deletion (PATCH sets
//! `pinned=true` / `archived_at=null, accepted_at=null`), the session is still deleted.
//! The test reproduces the post-race state directly: the conversation is no longer a
//! retention candidate when `delete_retained` runs.

use canterlot_hub::{deletion, testkit};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
#[ignore = "bug: уборка принятых делегатов удаляет сессию, которую владелец оставил"]
async fn delete_retained_spares_a_delegate_the_owner_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let id = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    // Accepted delegate, old enough for retention (what the candidate query saw)...
    sqlx::query(
        "update conversations set parent_conversation_id=$2,spawn_depth=1,
                archived_at=now()-interval '10 days',accepted_at=now()-interval '10 days'
          where id=$1",
    )
    .bind(id)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    // ...then the owner keeps it: PATCH {pinned:true, archived:false} (conversations.rs:1555).
    sqlx::query(
        "update conversations set pinned=true,pinned_at=now(),archived_at=null,accepted_at=null
          where id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let _ = deletion::delete_retained(&pool, None, seed.owner_id, id, Uuid::now_v7()).await;

    let exists: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        exists,
        "retention deleted a pinned, un-archived session the owner chose to keep"
    );
}
