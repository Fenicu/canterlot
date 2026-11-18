use crate::support::task7::Fixture;
use axum::http::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Row {
    machine: Uuid,
    kind: &'static str,
    key: &'static str,
    payload: Value,
    age_seconds: i64,
}

async fn insert(pool: &PgPool, owner: Uuid, row: Row) {
    sqlx::query("insert into machine_updates(id,owner_id,machine_id,kind,harness,dedup_key,payload,created_at) values($1,$2,$3,$4,$5,$6,$7,clock_timestamp()-$8*interval '1 second')")
        .bind(Uuid::now_v7())
        .bind(owner)
        .bind(row.machine)
        .bind(row.kind)
        .bind("codex")
        .bind(row.key)
        .bind(row.payload)
        .bind(row.age_seconds)
        .execute(pool)
        .await
        .unwrap();
}

async fn feed(f: &Fixture) -> Value {
    let response = f
        .client
        .send(Method::GET, "/api/v1/updates", None, None)
        .await;
    assert_eq!(response.status.as_u16(), 200, "{:?}", response.body);
    response.json::<Value>()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn feed_is_newest_first_with_cursor_and_optional_filters(pool: PgPool) -> TestResult {
    let f = Fixture::new(pool.clone()).await;
    let other = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'other')")
        .bind(other)
        .bind(f.seed.owner_id)
        .execute(&pool)
        .await?;
    let cmd = json!("npm i -g @openai/codex@latest");
    for (kind, key, age, machine, payload) in [
        (
            "version_available",
            "v1",
            100,
            f.seed.machine_id,
            json!({"latest_version":"1.1.0","update_command":cmd}),
        ),
        (
            "below_minimum",
            "v2",
            90,
            f.seed.machine_id,
            json!({"installed_version":"0.9.0","update_command":cmd}),
        ),
        (
            "cli_updated",
            "v3",
            80,
            f.seed.machine_id,
            json!({"from":"1.0.0","to":"1.1.0","update_command":cmd}),
        ),
        (
            "models_added",
            "v4",
            70,
            f.seed.machine_id,
            json!({"models":["a"]}),
        ),
        (
            "models_removed",
            "v5",
            60,
            f.seed.machine_id,
            json!({"models":["b"]}),
        ),
        (
            "version_available",
            "v6",
            50,
            other,
            json!({"latest_version":"2.0.0","update_command":cmd}),
        ),
    ] {
        insert(
            &pool,
            f.seed.owner_id,
            Row {
                machine,
                kind,
                key,
                payload,
                age_seconds: age,
            },
        )
        .await;
    }

    let first: Value = f
        .client
        .send(Method::GET, "/api/v1/updates?limit=2", None, None)
        .await
        .json();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["items"][0]["kind"], "version_available");
    assert_eq!(first["items"][1]["kind"], "models_removed");
    assert_eq!(first["items"][0]["machine"], "other");
    assert_eq!(first["items"][1]["machine"], "test");
    assert_eq!(first["items"][0]["harness"], "codex");
    assert_eq!(
        first["items"][0]["update_command"],
        json!("npm i -g @openai/codex@latest")
    );
    assert_eq!(first["items"][0]["payload"]["latest_version"], "2.0.0");
    assert!(first["items"][0]["created_at"].is_string());
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();

    let second: Value = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/updates?limit=2&cursor={cursor}"),
            None,
            None,
        )
        .await
        .json();
    assert_eq!(second["items"][0]["kind"], "models_added");
    assert_eq!(second["items"][1]["kind"], "cli_updated");
    assert!(second["items"][0]["update_command"].is_null());

    let by_kind: Value = f
        .client
        .send(
            Method::GET,
            "/api/v1/updates?kind=version_available",
            None,
            None,
        )
        .await
        .json();
    assert_eq!(by_kind["items"].as_array().unwrap().len(), 2);

    let by_machine: Value = f
        .client
        .send(Method::GET, "/api/v1/updates?machine=other", None, None)
        .await
        .json();
    assert_eq!(by_machine["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_machine["items"][0]["machine"], "other");

    let all = feed(&f).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 6);
    assert!(all["next_cursor"].is_null());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn feed_rejects_invalid_queries_and_anonymous_readers(pool: PgPool) -> TestResult {
    let f = Fixture::new(pool.clone()).await;
    for query in [
        "limit=0",
        "limit=101",
        "kind=unknown",
        "cursor=not-a-cursor",
        "unexpected=1",
    ] {
        let response = f
            .client
            .send(Method::GET, &format!("/api/v1/updates?{query}"), None, None)
            .await;
        assert_eq!(response.status.as_u16(), 400, "{query}");
    }
    let anonymous = canterlot_hub::testkit::TestClient::in_process(canterlot_hub::http::router(
        f.state.clone(),
    ))
    .with_header("Origin", canterlot_hub::testkit::TEST_ORIGIN)
    .with_header("X-Canterlot-Client", "1");
    let refused = anonymous
        .send(Method::GET, "/api/v1/updates", None, None)
        .await;
    assert_eq!(refused.status.as_u16(), 401);
    Ok(())
}
