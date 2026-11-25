//! Расход токенов за сессию и её делегатов: суммы по ходам и моделям, свёртывание
//! удалённой делегации на родителя и каскад при удалении самого родителя.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit::{self, TestClient},
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
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
            machine,
            client,
            conversation,
            _dir: dir,
        }
    }

    async fn run(&self, conversation: Uuid, model: &str, tokens: (i64, i64, i64, i64)) {
        self.run_costed(conversation, model, tokens, None).await;
    }

    /// Ход с ценой в микродолларах; `None` — цена хода неизвестна.
    async fn run_costed(
        &self,
        conversation: Uuid,
        model: &str,
        tokens: (i64, i64, i64, i64),
        cost: Option<i64>,
    ) -> Uuid {
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
            "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cost_micro_usd,cost_source)
             values($1,$2,'main',$3,$4,$5,$6,$7,case when $7::bigint is null then null else 'cli' end)",
        )
        .bind(command)
        .bind(model)
        .bind(tokens.0)
        .bind(tokens.1)
        .bind(tokens.2)
        .bind(tokens.3)
        .bind(cost)
        .execute(&self.pool)
        .await
        .unwrap();
        command
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

#[tokio::test]
async fn totals_cover_turns_models_and_cache() {
    let pool = canterlot_hub::testkit::db().await;
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
