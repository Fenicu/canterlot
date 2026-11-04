//! Срок хранения принятых делегаций: сессия, закрытая `session_accept`, уходит из архива
//! через `[session_tools].accepted_retention`, но только если владелец не удержал её.

use std::time::Duration;

use canterlot_hub::{
    config::SessionToolsConfig,
    conversations::{self, ListFilter, ListQuery},
    dispatcher::Dispatcher,
    scheduler::Scheduler,
    snapshot, testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;
use uuid::Uuid;

fn scheduler(pool: &PgPool) -> Scheduler {
    let (dispatcher, _) = Dispatcher::channel();
    Scheduler::new(pool.clone(), dispatcher, Default::default())
}

async fn child(pool: &PgPool, owner: Uuid, machine: Uuid, parent: Uuid) -> Uuid {
    let id = testkit::seed_conversation(pool, owner, machine).await;
    sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
        .bind(id)
        .bind(parent)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn accept_at(pool: &PgPool, id: Uuid, at: DateTime<Utc>) {
    sqlx::query("update conversations set archived_at=$2,accepted_at=$2 where id=$1")
        .bind(id)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

async fn exists(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn accepted_delegate_older_than_retention_is_deleted(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(4)).await;

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 1);
    assert!(!exists(&pool, id).await);
    let tombstoned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversation_tombstones where conversation_id=$1)",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(tombstoned, "удаление идёт обычным путём с надгробием");
    let announced: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='conversation_deleted' and conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(announced, 1, "PWA получает обычное событие удаления");
    assert!(exists(&pool, seed.conversation_id).await, "родитель цел");
}

#[sqlx::test(migrations = "./migrations")]
async fn accepted_delegate_younger_than_retention_is_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(2)).await;

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, id).await);
}

#[sqlx::test(migrations = "./migrations")]
async fn owners_own_archived_session_is_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let own = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    accept_at(&pool, own, now - TimeDelta::days(30)).await;

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, own).await, "сессии владельца не трогаются");
}

#[sqlx::test(migrations = "./migrations")]
async fn delegate_that_was_not_accepted_is_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    sqlx::query("update conversations set archived_at=$2 where id=$1")
        .bind(id)
        .bind(now - TimeDelta::days(30))
        .execute(&pool)
        .await
        .unwrap();

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, id).await);
}

#[sqlx::test(migrations = "./migrations")]
async fn pinned_group_pinned_and_grouped_delegates_are_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let pinned = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    let grouped = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    let group_pinned = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    for id in [pinned, grouped, group_pinned] {
        accept_at(&pool, id, now - TimeDelta::days(10)).await;
    }
    sqlx::query("update conversations set pinned=true,pinned_at=clock_timestamp() where id=$1")
        .bind(pinned)
        .execute(&pool)
        .await
        .unwrap();
    let group = Uuid::now_v7();
    sqlx::query("insert into conversation_groups(id,owner_id,name) values($1,$2,'review')")
        .bind(group)
        .bind(seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into conversation_group_members(group_id,conversation_id) values($1,$2)")
        .bind(group)
        .bind(grouped)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into conversation_group_pins(conversation_id,group_id) values($1,$2)")
        .bind(group_pinned)
        .bind(group)
        .execute(&pool)
        .await
        .unwrap();

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    for id in [pinned, grouped, group_pinned] {
        assert!(exists(&pool, id).await, "удержанная сессия осталась: {id}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn delegate_with_a_kept_unique_branch_is_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(10)).await;
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(id)
        .bind(serde_json::json!({
            "state": "removed",
            "branch": "canterlot/abcd1234",
            "path": "/data/worktrees/x",
            "branch_kept": true,
        }))
        .execute(&pool)
        .await
        .unwrap();

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, id).await);
}

#[sqlx::test(migrations = "./migrations")]
async fn delegate_with_a_live_child_is_kept(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(10)).await;
    let grandchild = child(&pool, seed.owner_id, seed.machine_id, id).await;
    sqlx::query("update conversations set spawn_depth=2 where id=$1")
        .bind(grandchild)
        .execute(&pool)
        .await
        .unwrap();

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, id).await);
}

#[sqlx::test(migrations = "./migrations")]
async fn retention_zero_disables_the_purge(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(30)).await;
    let scheduler = scheduler(&pool).with_session_tools(SessionToolsConfig {
        accepted_retention: Duration::ZERO,
        ..Default::default()
    });

    let deleted = scheduler.purge_accepted_retention(now).await.unwrap();

    assert_eq!(deleted, 0);
    assert!(exists(&pool, id).await);
}

#[sqlx::test(migrations = "./migrations")]
async fn purge_is_silent_for_the_closed_parent_subscription(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let id = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, id, now - TimeDelta::days(10)).await;
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id,chain_depth,
                                            closed_at,closed_reason)
         values($1,$2,$3,$4,0,clock_timestamp(),'accepted')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let deleted = scheduler(&pool)
        .purge_accepted_retention(now)
        .await
        .unwrap();

    assert_eq!(deleted, 1);
    let jobs: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs
         where kind='session_task' and payload->>'action'='event'
           and payload->>'target_id'=$1",
    )
    .bind(id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(jobs, 0, "закрытая подписка не получает deleted");
    let notes: i64 = sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(notes, 0, "родителю не приходит новых вводов");
}

#[sqlx::test(migrations = "./migrations")]
async fn maintenance_runs_retention_at_most_every_few_hours(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let first = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, first, now - TimeDelta::days(10)).await;
    let scheduler = scheduler(&pool);
    scheduler.maintenance().await.unwrap();
    assert!(!exists(&pool, first).await, "первая уборка сработала");

    let second = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, second, now - TimeDelta::days(10)).await;
    scheduler.maintenance().await.unwrap();
    assert!(
        exists(&pool, second).await,
        "повторная уборка ждёт своего окна"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn archive_list_excludes_accepted_delegate_but_tree_and_detail_keep_it(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let now = Utc::now();
    let accepted = child(&pool, seed.owner_id, seed.machine_id, seed.conversation_id).await;
    accept_at(&pool, accepted, now - TimeDelta::days(1)).await;
    let archived_owner = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    sqlx::query("update conversations set archived_at=clock_timestamp() where id=$1")
        .bind(archived_owner)
        .execute(&pool)
        .await
        .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let page = conversations::list(
        &mut conn,
        &ListQuery {
            owner_id: seed.owner_id,
            filter: ListFilter::Archived,
            q: None,
            cursor: None,
            limit: 50,
        },
    )
    .await
    .unwrap();
    let ids: Vec<Uuid> = page.items.iter().map(|item| item.attributes.id).collect();
    assert_eq!(
        ids,
        vec![archived_owner],
        "принятая делегация скрыта из архива"
    );
    assert_eq!(page.counts.archived, 1, "счётчик архива её не считает");

    let tree = snapshot::conversations(&pool, seed.owner_id).await.unwrap();
    assert!(
        tree.data
            .items
            .iter()
            .any(|item| item.attributes.id == accepted),
        "снимок списка несёт делегацию для дерева"
    );
    let detail = conversations::detail(&mut conn, seed.owner_id, accepted)
        .await
        .unwrap()
        .expect("принятая делегация открывается по ссылке");
    assert!(detail.attributes.archived);
    assert!(detail.attributes.accepted);
}
