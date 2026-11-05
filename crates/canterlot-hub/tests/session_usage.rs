//! Расход токенов за сессию и её делегатов: суммы по ходам и моделям, свёртывание
//! удалённой делегации на родителя и каскад при удалении самого родителя.

use axum::http::Method;
use canterlot_hub::{
    deletion,
    http::{self, AppState},
    session_usage,
    testkit::{self, TestClient},
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    owner: Uuid,
    machine: Uuid,
    client: TestClient,
    conversation: Uuid,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        testkit::seed_catalog(&pool, machine).await;
        let conversation = testkit::seed_conversation(&pool, owner, machine).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            owner,
            machine,
            client,
            conversation,
            _dir: dir,
        }
    }

    async fn child(&self, parent: Uuid) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.machine).await;
        sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
            .bind(id)
            .bind(parent)
            .execute(&self.pool)
            .await
            .unwrap();
        id
    }

    async fn run(&self, conversation: Uuid, model: &str, tokens: (i64, i64, i64, i64)) {
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state,outcome)
             values($1,$2,1,'stopped','user_stop')
             on conflict (conversation_id,generation) do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(conversation)
        .execute(&self.pool)
        .await
        .unwrap();
        let execution: Uuid = sqlx::query_scalar(
            "select id from executions where conversation_id=$1 order by generation desc limit 1",
        )
        .bind(conversation)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let command = Uuid::now_v7();
        sqlx::query(
            "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
             values($1,$2,$3,$4,1,'deliver_input','{}','completed')",
        )
        .bind(command)
        .bind(self.machine)
        .bind(conversation)
        .bind(execution)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at)
             values($1,$2,$3,clock_timestamp(),clock_timestamp())",
        )
        .bind(command)
        .bind(execution)
        .bind(conversation)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens)
             values($1,$2,'main',$3,$4,$5,$6)",
        )
        .bind(command)
        .bind(model)
        .bind(tokens.0)
        .bind(tokens.1)
        .bind(tokens.2)
        .bind(tokens.3)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn usage(&self, id: Uuid) -> Value {
        let response = self
            .client
            .send(
                Method::GET,
                &format!("/api/v1/conversations/{id}/usage"),
                None,
                None,
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }
}

fn model<'a>(usage: &'a Value, slot: &str, name: &str) -> &'a Value {
    usage[slot]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == name)
        .unwrap_or_else(|| panic!("{slot} has no model {name}: {usage}"))
}

#[sqlx::test(migrations = "./migrations")]
async fn totals_cover_turns_models_and_cache(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.run(f.conversation, "opus", (100, 10, 5, 2)).await;
    f.run(f.conversation, "haiku", (50, 5, 0, 0)).await;
    f.run(f.conversation, "opus", (200, 20, 7, 3)).await;

    let usage = f.usage(f.conversation).await;
    let session = &usage["session"];
    assert_eq!(session["turns"], 3);
    assert_eq!(session["input_tokens"], 350);
    assert_eq!(session["output_tokens"], 35);
    assert_eq!(session["cache_read_tokens"], 12);
    assert_eq!(session["cache_write_tokens"], 5);
    assert_eq!(session["models"].as_array().unwrap().len(), 2, "{usage}");
    assert_eq!(model(&usage, "session", "opus")["input_tokens"], 300);
    assert_eq!(model(&usage, "session", "opus")["cache_read_tokens"], 12);
    assert_eq!(model(&usage, "session", "opus")["cache_write_tokens"], 5);
    assert_eq!(model(&usage, "session", "haiku")["input_tokens"], 50);
    assert_eq!(model(&usage, "session", "haiku")["output_tokens"], 5);
    assert_eq!(usage["delegates"]["turns"], 0);
    assert_eq!(usage["delegates"]["input_tokens"], 0);
    assert!(usage["delegates"]["models"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn delegates_include_the_whole_live_subtree(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let middle = f.child(f.conversation).await;
    let leaf = f.child(middle).await;
    f.run(f.conversation, "opus", (10, 1, 0, 0)).await;
    f.run(middle, "sonnet", (20, 2, 1, 0)).await;
    f.run(leaf, "haiku", (30, 3, 0, 1)).await;

    let root = f.usage(f.conversation).await;
    assert_eq!(root["session"]["turns"], 1);
    assert_eq!(root["session"]["input_tokens"], 10);
    assert_eq!(root["delegates"]["turns"], 2);
    assert_eq!(root["delegates"]["input_tokens"], 50);
    assert_eq!(root["delegates"]["output_tokens"], 5);
    assert_eq!(root["delegates"]["cache_read_tokens"], 1);
    assert_eq!(root["delegates"]["cache_write_tokens"], 1);
    assert_eq!(root["delegates"]["models"].as_array().unwrap().len(), 2);
    assert_eq!(model(&root, "delegates", "sonnet")["input_tokens"], 20);
    assert_eq!(model(&root, "delegates", "haiku")["input_tokens"], 30);

    let nested = f.usage(middle).await;
    assert_eq!(nested["session"]["input_tokens"], 20);
    assert_eq!(nested["delegates"]["turns"], 1);
    assert_eq!(nested["delegates"]["input_tokens"], 30);
}

#[sqlx::test(migrations = "./migrations")]
async fn retention_keeps_the_parent_delegates_total(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let child = f.child(f.conversation).await;
    f.run(child, "sonnet", (100, 10, 5, 2)).await;
    let before = f.usage(f.conversation).await;
    assert_eq!(before["delegates"]["turns"], 1);

    deletion::delete_retained(&f.pool, None, f.owner, child, Uuid::now_v7())
        .await
        .unwrap();

    let after = f.usage(f.conversation).await;
    assert_eq!(after["delegates"]["turns"], 1);
    assert_eq!(after["delegates"]["input_tokens"], 100);
    assert_eq!(after["delegates"]["output_tokens"], 10);
    assert_eq!(after["delegates"]["cache_read_tokens"], 5);
    assert_eq!(after["delegates"]["cache_write_tokens"], 2);
    assert_eq!(model(&after, "delegates", "sonnet")["input_tokens"], 100);
    assert_eq!(after["session"]["turns"], 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn manual_delete_keeps_the_parent_delegates_total(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let child = f.child(f.conversation).await;
    f.run(child, "haiku", (40, 4, 0, 0)).await;

    let response = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{child}"),
            Some("manual-fold"),
            None,
        )
        .await;
    assert_eq!(
        response.status.as_u16(),
        204,
        "{}",
        String::from_utf8_lossy(&response.body)
    );

    let after = f.usage(f.conversation).await;
    assert_eq!(after["delegates"]["turns"], 1);
    assert_eq!(after["delegates"]["input_tokens"], 40);
    assert_eq!(model(&after, "delegates", "haiku")["output_tokens"], 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn folding_the_same_delegate_twice_counts_once(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let child = f.child(f.conversation).await;
    f.run(child, "sonnet", (70, 7, 2, 1)).await;

    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    session_usage::fold_in_tx(&mut tx, child).await.unwrap();
    session_usage::fold_in_tx(&mut tx, child).await.unwrap();
    tx.commit().await.unwrap();

    let folded: (i64, Value) = sqlx::query_as(
        "select turns, models from conversation_usage_rollup
          where parent_id=$1 and source_id=$2",
    )
    .bind(f.conversation)
    .bind(child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(folded.0, 1);
    assert_eq!(folded.1.as_array().unwrap().len(), 1);
    assert_eq!(folded.1[0]["input_tokens"], 70);

    // Уборка сворачивает ещё раз, но строка уже есть: итог не удваивается.
    deletion::delete_retained(&f.pool, None, f.owner, child, Uuid::now_v7())
        .await
        .unwrap();
    let after = f.usage(f.conversation).await;
    assert_eq!(after["delegates"]["turns"], 1);
    assert_eq!(after["delegates"]["input_tokens"], 70);
}

#[sqlx::test(migrations = "./migrations")]
async fn folded_grandchild_stays_in_the_whole_subtree(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let middle = f.child(f.conversation).await;
    let leaf = f.child(middle).await;
    f.run(middle, "sonnet", (20, 2, 0, 0)).await;
    f.run(leaf, "haiku", (30, 3, 0, 0)).await;

    deletion::delete_retained(&f.pool, None, f.owner, leaf, Uuid::now_v7())
        .await
        .unwrap();

    let root = f.usage(f.conversation).await;
    assert_eq!(root["delegates"]["turns"], 2);
    assert_eq!(root["delegates"]["input_tokens"], 50);
    assert_eq!(model(&root, "delegates", "haiku")["input_tokens"], 30);
    let nested = f.usage(middle).await;
    assert_eq!(nested["delegates"]["turns"], 1);
    assert_eq!(nested["delegates"]["input_tokens"], 30);

    // Свёрнутое уходит выше вместе с самой серединой: лист не теряется.
    deletion::delete_retained(&f.pool, None, f.owner, middle, Uuid::now_v7())
        .await
        .unwrap();
    let root = f.usage(f.conversation).await;
    assert_eq!(root["delegates"]["turns"], 2);
    assert_eq!(root["delegates"]["input_tokens"], 50);
    assert_eq!(model(&root, "delegates", "haiku")["input_tokens"], 30);
    assert_eq!(model(&root, "delegates", "sonnet")["input_tokens"], 20);
}

#[sqlx::test(migrations = "./migrations")]
async fn deleting_the_parent_removes_its_rollup(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let child = f.child(f.conversation).await;
    f.run(child, "sonnet", (90, 9, 0, 0)).await;
    deletion::delete_retained(&f.pool, None, f.owner, child, Uuid::now_v7())
        .await
        .unwrap();

    let folded: i64 =
        sqlx::query_scalar("select count(*) from conversation_usage_rollup where parent_id=$1")
            .bind(f.conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(folded, 1);

    deletion::delete_retained(&f.pool, None, f.owner, f.conversation, Uuid::now_v7())
        .await
        .unwrap();

    let left: i64 = sqlx::query_scalar(
        "select count(*) from conversation_usage_rollup where parent_id=$1 or source_id=$1",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}
