//! Корзина владельца: ручное удаление прячет сессию на срок хранения, возврат и
//! окончательная уборка идут через тот же путь, что и прежнее удаление.

#[path = "support/task9.rs"]
mod support;

use std::{sync::Arc, time::Duration};

use axum::http::Method;
use canterlot_hub::{
    config::SessionToolsConfig,
    deletion,
    dispatcher::Dispatcher,
    scheduler::Scheduler,
    session_tools::{self, Caller, ToolContext, ToolErrorCode as Code, read, settle::SettleConfig},
    session_usage,
    testkit::TestResponse,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn method(f: &Fixture, method: Method, path: &str, key: &str) -> TestResponse {
    f.client.send(method, path, Some(key), None).await
}

async fn trash(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::DELETE,
        &format!("/api/v1/conversations/{id}"),
        key,
    )
    .await
}

async fn restore(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::POST,
        &format!("/api/v1/conversations/{id}/restore"),
        key,
    )
    .await
}

async fn purge(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    method(
        f,
        Method::DELETE,
        &format!("/api/v1/conversations/{id}/trash"),
        key,
    )
    .await
}

fn code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn trashed_at(pool: &PgPool, id: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("select trashed_at from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn age_trash(pool: &PgPool, id: Uuid, hours: i64) {
    sqlx::query(
        "update conversations set trashed_at=clock_timestamp()-$2::int*interval '1 hour' where id=$1",
    )
    .bind(id)
    .bind(hours as i32)
    .execute(pool)
    .await
    .unwrap();
}

fn tool_context(pool: &PgPool) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: CancellationToken::new(),
    }
}

fn caller(root: Uuid, machine: Uuid, owner: Uuid) -> Caller {
    Caller {
        owner_id: owner,
        conversation_id: root,
        execution_id: Uuid::now_v7(),
        machine_id: machine,
        call_id: Uuid::now_v7(),
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trash_hides_the_session_from_lists_search_counts_usage_and_tools(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = f.conversation().await;
    let title = format!("корзина-{}", Uuid::now_v7());
    sqlx::query("update conversations set title=$2 where id=$1")
        .bind(f.conversation)
        .bind(&title)
        .execute(&f.pool)
        .await
        .unwrap();

    assert_eq!(trash(&f, f.conversation, "hide-trash").await.status, 204);

    let all = f
        .client
        .send(Method::GET, "/api/v1/conversations", None, None)
        .await;
    let all: Value = all.json();
    assert!(
        !all["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == json!(f.conversation)),
        "trashed session is still in the list"
    );
    assert_eq!(all["counts"]["trashed"], json!(1));
    assert_eq!(all["counts"]["all"], json!(1));

    let found = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations?filter=all&q={title}"),
            None,
            None,
        )
        .await;
    let found: Value = found.json();
    assert_eq!(
        found["items"].as_array().unwrap().len(),
        0,
        "search leaks it"
    );

    let detail = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}", f.conversation),
            None,
            None,
        )
        .await;
    assert_eq!(detail.status, 200, "the bucket page needs the session");
    let detail: Value = detail.json();
    assert_eq!(detail["trashed"], json!(true));
    assert!(detail["trashed_at"].is_string());

    let usage = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}/usage", f.conversation),
            None,
            None,
        )
        .await;
    assert_eq!(usage.status, 404);
    assert_eq!(code(&usage), "conversation_deleted");
    assert!(
        session_usage::for_conversation(&f.pool, f.owner, f.conversation)
            .await
            .unwrap()
            .is_none()
    );

    let context = tool_context(&f.pool);
    let from = caller(other, f.machine, f.owner);
    let listed = read::list(&context, &from, json!({"include_archived":true}))
        .await
        .unwrap();
    assert!(
        !listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == json!(f.conversation)),
        "sessions.list leaks the trashed session"
    );
    let error = read::inspect(&context, &from, json!({"session_id": f.conversation}))
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound);
    let error = session_tools::send::send(
        &context,
        &from,
        json!({"session_id": f.conversation, "text": "не дойдёт"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound);
    let silent = caller(f.conversation, f.machine, f.owner);
    let error = read::list(&context, &silent, json!({})).await.unwrap_err();
    assert_eq!(
        error.code,
        Code::SessionNotFound,
        "a trashed caller is gone too"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trash_keeps_delete_preconditions_and_leaves_rows(pool: PgPool) {
    let f = Fixture::new(pool).await;

    let live = f.conversation().await;
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state)
         values($1,$2,1,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(live)
    .execute(&f.pool)
    .await
    .unwrap();
    let response = trash(&f, live, "live").await;
    assert_eq!(response.status, 409);
    assert_eq!(code(&response), "conversation_live");
    assert!(trashed_at(&f.pool, live).await.is_none());

    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let worktree = f.conversation().await;
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(worktree)
        .bind(json!({"state":"active","path":"/tmp/wt","branch":"b"}))
        .execute(&f.pool)
        .await
        .unwrap();
    let response = trash(&f, worktree, "worktree").await;
    assert_eq!(response.status, 409);
    assert_eq!(code(&response), "worktree_not_removed");
    assert!(trashed_at(&f.pool, worktree).await.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from conversations where id=$1")
            .bind(worktree)
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        1,
        "a refused trash must not remove rows"
    );

    let unknown = Uuid::now_v7();
    let response = trash(&f, unknown, "unknown").await;
    assert_eq!(response.status, 404);
    assert_eq!(code(&response), "not_found");

    // Успешная корзина не трогает ни одной строки сессии.
    let rows: i64 = sqlx::query_scalar(
        "select (select count(*) from inputs where conversation_id=$1)
              + (select count(*) from blocks where conversation_id=$1)
              + (select count(*) from executions where conversation_id=$1)
              + (select count(*) from queue_state where conversation_id=$1)",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(rows, 1, "only queue_state exists in a fresh seed");
    assert_eq!(trash(&f, f.conversation, "ok").await.status, 204);
    assert!(trashed_at(&f.pool, f.conversation).await.is_some());
    let after: i64 = sqlx::query_scalar(
        "select (select count(*) from inputs where conversation_id=$1)
              + (select count(*) from blocks where conversation_id=$1)
              + (select count(*) from executions where conversation_id=$1)
              + (select count(*) from queue_state where conversation_id=$1)",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(after, rows, "trash must not delete session rows");
}

async fn notified_jobs(pool: &PgPool, target: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from scheduled_jobs
         where kind='session_task' and payload->>'action'='event'
           and payload->>'target_id'=$1 and payload->>'state'='deleted'",
    )
    .bind(target.to_string())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn agents_are_told_once_at_trash_and_not_at_restore_or_purge(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let subscriber = f.conversation().await;
    let target = f.conversation().await;
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id,chain_depth)
         values($1,$2,$3,$4,0)",
    )
    .bind(Uuid::now_v7())
    .bind(f.owner)
    .bind(subscriber)
    .bind(target)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,deadline_at,condition,call_id)
         values($1,$2,$3,'agent','condition',now()+interval '1 day',
                jsonb_build_object('kind','session_state','session_ids',jsonb_build_array($4::text),
                                   'states',jsonb_build_array('idle','deleted'),'all',false,'armed','{}'::jsonb),
                $5)",
    )
    .bind(Uuid::now_v7())
    .bind(f.owner)
    .bind(subscriber)
    .bind(target)
    .bind(Uuid::now_v7())
    .execute(&f.pool)
    .await
    .unwrap();

    assert_eq!(trash(&f, target, "notify").await.status, 204);
    assert_eq!(notified_jobs(&f.pool, target).await, 1, "one deleted event");
    let (closed, reason): (bool, Option<String>) = sqlx::query_as(
        "select closed_at is not null,closed_reason from session_subscriptions where target_id=$1",
    )
    .bind(target)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(closed);
    assert_eq!(reason.as_deref(), Some("deleted"));
    let (state, fired): (String, Option<String>) =
        sqlx::query_as("select state,fired_reason from alarms where conversation_id=$1")
            .bind(subscriber)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "fired");
    assert!(fired.is_some(), "the alarm is closed with a reason");

    let restored = restore(&f, target, "notify-restore").await;
    assert_eq!(restored.status, 200, "{:?}", restored.body);
    assert_eq!(notified_jobs(&f.pool, target).await, 1, "restore is silent");
    let still_closed: bool = sqlx::query_scalar(
        "select closed_at is not null from session_subscriptions where target_id=$1",
    )
    .bind(target)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(still_closed, "restore must not resurrect subscriptions");

    assert_eq!(trash(&f, target, "notify-again").await.status, 204);
    assert_eq!(purge(&f, target, "notify-purge").await.status, 204);
    assert_eq!(notified_jobs(&f.pool, target).await, 1, "purge is silent");
    let owner_deleted: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='conversation_deleted' and conversation_id=$1",
    )
    .bind(target)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(owner_deleted, 1, "the list gets one deletion event");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn restore_is_idempotent_and_returns_the_session_as_it_was(pool: PgPool) {
    let f = Fixture::new(pool).await;
    sqlx::query("update conversations set archived_at=clock_timestamp() where id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(trash(&f, f.conversation, "restore-trash").await.status, 204);
    assert_eq!(
        trash(&f, f.conversation, "restore-trash-again")
            .await
            .status,
        204
    );

    let first = restore(&f, f.conversation, "restore-1").await;
    assert_eq!(first.status, 200, "{:?}", first.body);
    assert_eq!(first.json::<Value>()["trashed"], json!(false));
    assert!(trashed_at(&f.pool, f.conversation).await.is_none());
    let archived: bool =
        sqlx::query_scalar("select archived_at is not null from conversations where id=$1")
            .bind(f.conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(archived, "restore keeps the archive flag");

    assert_eq!(
        restore(&f, f.conversation, "restore-1").await.status,
        200,
        "same key replays"
    );
    assert_eq!(
        restore(&f, f.conversation, "restore-2").await.status,
        200,
        "restore is a no-op on a live session"
    );
    let listed = f
        .client
        .send(
            Method::GET,
            "/api/v1/conversations?filter=archived",
            None,
            None,
        )
        .await;
    let listed: Value = listed.json();
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == json!(f.conversation))
    );

    let unknown = restore(&f, Uuid::now_v7(), "restore-unknown").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(code(&unknown), "not_found");
}

async fn seed_run(
    pool: &PgPool,
    machine: Uuid,
    conversation: Uuid,
    command: Uuid,
    input: Uuid,
    owner: Uuid,
) {
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,outcome)
         values($1,$2,1,'stopped','user_stop')",
    )
    .bind(execution)
    .bind(conversation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,1,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(machine)
    .bind(conversation)
    .bind(execution)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at)
         values($1,$2,$3,1,'seed','[]','accepted',$4,now())",
    )
    .bind(input)
    .bind(conversation)
    .bind(owner)
    .bind(command)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id) values($1,$2,$3)")
        .bind(command)
        .bind(execution)
        .bind(conversation)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens)
         values($1,'opus','main',10,20,0,0)",
    )
    .bind(command)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purge_after_retention_uses_the_hard_delete_and_folds_usage(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let parent = f.conversation().await;
    let child = f.conversation().await;
    sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
        .bind(child)
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    let command = Uuid::now_v7();
    seed_run(&f.pool, f.machine, child, command, Uuid::now_v7(), f.owner).await;
    let file = f
        .ready_in(child, "trash.txt", "text/plain", &content(64, 7))
        .await;
    let blob = f.blob_path(file);
    assert!(blob.exists());

    assert_eq!(trash(&f, child, "purge-trash").await.status, 204);
    age_trash(&f.pool, child, 25).await;
    let (dispatcher, _) = Dispatcher::channel();
    let scheduler =
        Scheduler::new(f.pool.clone(), dispatcher, Default::default()).with_blobs(f.store.clone());
    let deleted = scheduler.purge_trashed(chrono::Utc::now()).await.unwrap();
    assert_eq!(deleted, 1);
    assert!(
        !sqlx::query_scalar::<_, bool>("select exists(select 1 from conversations where id=$1)")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap()
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from conversation_tombstones where conversation_id=$1)"
        )
        .bind(child)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    );
    assert!(!blob.exists(), "blob removed at purge");
    let rolled: i64 = sqlx::query_scalar(
        "select turns from conversation_usage_rollup where parent_id=$1 and source_id=$2",
    )
    .bind(parent)
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(rolled, 1, "the child's turn folded into the parent");
    let usage = session_usage::for_conversation(&f.pool, f.owner, parent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(usage.delegates.turns, 1);
    assert_eq!(usage.delegates.models[0].input_tokens, 10);

    // Восстановленная сессия сроком не затрагивается.
    let restored = f.conversation().await;
    assert_eq!(trash(&f, restored, "purge-restore-trash").await.status, 204);
    age_trash(&f.pool, restored, 30).await;
    assert_eq!(restore(&f, restored, "purge-restore").await.status, 200);
    assert_eq!(
        scheduler.purge_trashed(chrono::Utc::now()).await.unwrap(),
        0
    );
    assert!(
        sqlx::query_scalar::<_, bool>("select exists(select 1 from conversations where id=$1)")
            .bind(restored)
            .fetch_one(&f.pool)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purge_endpoint_requires_trash_and_is_idempotent(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let active = f.conversation().await;
    let response = purge(&f, active, "not-trashed").await;
    assert_eq!(response.status, 409);
    assert_eq!(code(&response), "invalid_state");

    assert_eq!(trash(&f, active, "endpoint-trash").await.status, 204);
    let file = f
        .ready_in(active, "gone.txt", "text/plain", &content(32, 3))
        .await;
    let blob = f.blob_path(file);
    assert!(blob.exists());
    assert_eq!(purge(&f, active, "purge-1").await.status, 204);
    assert_eq!(purge(&f, active, "purge-1").await.status, 204, "replay");
    assert!(!blob.exists());
    let again = purge(&f, active, "purge-2").await;
    assert_eq!(again.status, 404);
    assert_eq!(code(&again), "conversation_deleted");
    let unknown = purge(&f, Uuid::now_v7(), "purge-unknown").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(code(&unknown), "not_found");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_to_a_trashed_session_is_rejected(pool: PgPool) {
    let f = Fixture::new(pool).await;
    assert_eq!(trash(&f, f.conversation, "input-trash").await.status, 204);
    let response = f
        .submit_to(f.conversation, "after-trash", "привет", &[])
        .await;
    assert_eq!(response.status, 404);
    assert_eq!(code(&response), "conversation_deleted");
    let inputs: i64 = sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(f.conversation)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(inputs, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_restore_and_purge_have_exactly_one_winner(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for round in 0..6 {
        let id = f.conversation().await;
        assert_eq!(
            trash(&f, id, &format!("race-trash-{round}")).await.status,
            204
        );
        age_trash(&f.pool, id, 25).await;
        let restore_key = format!("race-restore-{round}");
        let purge_key = format!("race-purge-{round}");
        let (restored, purged) =
            tokio::join!(restore(&f, id, &restore_key), purge(&f, id, &purge_key));
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
                .bind(id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let tombstone: bool = sqlx::query_scalar(
            "select exists(select 1 from conversation_tombstones where conversation_id=$1)",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        match (restored.status.as_u16(), purged.status.as_u16()) {
            (200, 409) => {
                assert!(exists, "restore won but the row is gone");
                assert!(!tombstone);
                assert!(trashed_at(&f.pool, id).await.is_none());
            }
            (404, 204) => {
                assert!(!exists, "purge won but the row is still there");
                assert!(tombstone);
            }
            other => panic!("round {round}: unexpected outcome {other:?}"),
        }
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accepted_delegate_retention_stays_a_hard_delete(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let child = f.conversation().await;
    sqlx::query(
        "update conversations
         set parent_conversation_id=$2,spawn_depth=1,archived_at=clock_timestamp(),
             accepted_at=clock_timestamp()-interval '4 days'
         where id=$1",
    )
    .bind(child)
    .bind(f.conversation)
    .execute(&f.pool)
    .await
    .unwrap();
    let (dispatcher, _) = Dispatcher::channel();
    let scheduler = Scheduler::new(f.pool.clone(), dispatcher, Default::default());
    let deleted = scheduler
        .purge_accepted_retention(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert!(
        !sqlx::query_scalar::<_, bool>("select exists(select 1 from conversations where id=$1)")
            .bind(child)
            .fetch_one(&f.pool)
            .await
            .unwrap()
    );
    let trashed: i64 =
        sqlx::query_scalar("select count(*) from conversations where trashed_at is not null")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(trashed, 0, "accepted retention never uses the trash");
}

/// Проверка возврата после окончательной уборки: надгробие отвечает как у удалённой.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purged_session_answers_conversation_deleted(pool: PgPool) {
    let f = Fixture::new(pool).await;
    assert_eq!(trash(&f, f.conversation, "tomb-trash").await.status, 204);
    assert_eq!(purge(&f, f.conversation, "tomb-purge").await.status, 204);
    let response = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}", f.conversation),
            None,
            None,
        )
        .await;
    assert_eq!(response.status, 404);
    assert_eq!(code(&response), "conversation_deleted");
    assert!(
        deletion::tombstoned(&f.pool, f.owner, f.conversation)
            .await
            .unwrap()
    );
}
