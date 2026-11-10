//! Уборка корзины проходит мимо застрявших кандидатов: сотня сессий, которые нельзя
//! удалить, не закрывает дорогу следующей.

use canterlot_hub::{
    dispatcher::Dispatcher, executions, model::ExecutionState, scheduler::Scheduler, testkit,
};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stuck_trash_candidates_do_not_block_later_ones(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    for _ in 0..100 {
        let stuck = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        let started = executions::start(&pool, seed.owner_id, stuck, Uuid::now_v7())
            .await
            .unwrap();
        executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Starting,
            None,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        // Живое исполнение: окончательная уборка отказывает (`Live`) на каждом проходе.
        sqlx::query("update conversations set trashed_at=now()-interval '3 days' where id=$1")
            .bind(stuck)
            .execute(&pool)
            .await
            .unwrap();
    }
    let later = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    sqlx::query("update conversations set trashed_at=now()-interval '2 days' where id=$1")
        .bind(later)
        .execute(&pool)
        .await
        .unwrap();

    let (dispatcher, _) = Dispatcher::channel();
    let scheduler = Scheduler::new(pool.clone(), dispatcher, Default::default());
    let deleted = scheduler.purge_trashed(chrono::Utc::now()).await.unwrap();
    assert_eq!(deleted, 1);
    let left: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(later)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!left, "the candidate after a hundred stuck ones is purged");
}
