//! Уборка принятых делегаций отбирает кандидатов без блокировок. Если владелец закрепил
//! или вернул сессию из архива между отбором и удалением, условие перепроверяется под
//! блокировкой и сессия остаётся. Тест воспроизводит состояние после такой гонки.

use canterlot_hub::{deletion, testkit};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_retained_spares_a_delegate_the_owner_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let id = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    // Принятая делегация старше срока: такой её увидел отбор кандидатов...
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
    // ...а затем владелец её оставил: PATCH {pinned:true, archived:false}.
    sqlx::query(
        "update conversations set pinned=true,pinned_at=now(),archived_at=null,accepted_at=null
          where id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let outcome = deletion::delete_accepted_retained(
        &pool,
        None,
        seed.owner_id,
        id,
        chrono::Utc::now() - chrono::Duration::days(7),
        Uuid::now_v7(),
    )
    .await;
    assert!(
        matches!(outcome, Err(deletion::DeleteError::NotTrashed)),
        "{outcome:?}"
    );

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
