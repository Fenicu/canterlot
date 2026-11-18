//! Заготовки сообщений: CRUD через настоящий роутер, ревизии, области и события.

use axum::http::Method;
use canterlot_hub::{
    events,
    http::{self, AppState},
    testkit,
    testkit::TestClient,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            owner,
            _dir: dir,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> testkit::TestResponse {
        let response = self.client.send(method, path, key, body).await;
        assert_eq!(response.headers["cache-control"], "no-store");
        response
    }

    async fn create(&self, key: &str, body: Value) -> Value {
        let response = self
            .request(
                Method::POST,
                "/api/v1/message-snippets",
                Some(key),
                Some(body),
            )
            .await;
        assert_eq!(
            response.status.as_u16(),
            201,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    fn error_code(response: &testkit::TestResponse) -> Value {
        response.json::<Value>()["error"]["code"].clone()
    }
}

fn validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/message_snippet_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

async fn snippet_events(fx: &Fx) -> Vec<Value> {
    let scope = format!("owner:{}:conversations", fx.owner);
    let rows: Vec<events::StoredEvent> = sqlx::query_as(
        "select * from events where scope=$1 and kind='message_snippet_changed' order by scope_seq",
    )
    .bind(&scope)
    .fetch_all(&mut *fx.pool.acquire().await.unwrap())
    .await
    .unwrap();
    events::project_for_client(rows)
        .into_iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn crud_filters_scopes_and_rejects_stale_revision(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let global = fx
        .create(
            "snip-global",
            json!({"scope":"global","title":"Общая","body":"Привет"}),
        )
        .await;
    let project = fx
        .create(
            "snip-project",
            json!({"scope":"project","project_key":"/tmp/primer","title":"Проектная","body":"Текст"}),
        )
        .await;
    assert_eq!(global["scope"], "global");
    assert_eq!(global["project_key"], "");
    assert_eq!(global["revision"], 1);
    assert_eq!(project["project_key"], "/tmp/primer");

    let listed: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=global",
            None,
            None,
        )
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], global["id"]);
    let listed: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=project&project_key=%2Ftmp%2Fprimer",
            None,
            None,
        )
        .await
        .json();
    assert_eq!(listed[0]["id"], project["id"]);
    let other: Value = fx
        .request(
            Method::GET,
            "/api/v1/message-snippets?scope=project&project_key=%2Ftmp%2Fdrugoe",
            None,
            None,
        )
        .await
        .json();
    assert!(other.as_array().unwrap().is_empty());

    let id = global["id"].as_str().unwrap();
    let updated: Value = fx
        .request(
            Method::PATCH,
            &format!("/api/v1/message-snippets/{id}"),
            Some("snip-update"),
            Some(json!({"body":"Новый текст","base_revision":1})),
        )
        .await
        .json();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["body"], "Новый текст");

    let stale = fx
        .request(
            Method::PATCH,
            &format!("/api/v1/message-snippets/{id}"),
            Some("snip-stale"),
            Some(json!({"body":"Устаревший","base_revision":1})),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&stale), "message_snippet_revision_conflict");
    assert_eq!(
        stale.json::<Value>()["error"]["details"]["current_revision"],
        2
    );

    let reorder = fx
        .request(
            Method::PUT,
            "/api/v1/message-snippets/order",
            Some("snip-order"),
            Some(json!({"scope":"global","ids":[global["id"], project["id"]]})),
        )
        .await;
    // Проектная заготовка не входит в глобальную область: Invalid/NotFound.
    assert!(!reorder.status.is_success());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reorder_delete_and_events(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let first = fx
        .create(
            "r-one",
            json!({"scope":"global","title":"Первая","body":"а"}),
        )
        .await;
    let second = fx
        .create(
            "r-two",
            json!({"scope":"global","title":"Вторая","body":"б"}),
        )
        .await;
    let response = fx
        .request(
            Method::PUT,
            "/api/v1/message-snippets/order",
            Some("r-order"),
            Some(json!({"scope":"global","ids":[second["id"], first["id"]]})),
        )
        .await;
    assert_eq!(
        response.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let ordered: Value = response.json();
    assert_eq!(ordered[0]["id"], second["id"]);
    assert_eq!(ordered[0]["position"], 0);

    let deleted = fx
        .request(
            Method::DELETE,
            &format!("/api/v1/message-snippets/{}", first["id"].as_str().unwrap()),
            Some("r-delete"),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    let listed: Value = fx
        .request(Method::GET, "/api/v1/message-snippets", None, None)
        .await
        .json();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let events = snippet_events(&fx).await;
    assert!(events.len() >= 4);
    let check = validator();
    for event in &events {
        assert!(check.is_valid(event), "{event}");
    }
    let last = events.last().unwrap();
    assert_eq!(last["payload"]["deleted"], true);
    assert_eq!(last["payload"]["snippet"], Value::Null);
    assert_eq!(
        events::wire_scope(&events::Scope::Conversations { owner_id: fx.owner }),
        "conversations"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_limits_and_duplicates(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let long_title = "ж".repeat(81);
    let response = fx
        .request(
            Method::POST,
            "/api/v1/message-snippets",
            Some("lim-title"),
            Some(json!({"scope":"global","title":long_title,"body":"текст"})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 422);
    assert_eq!(Fx::error_code(&response), "message_snippet_invalid");
    assert_eq!(
        response.json::<Value>()["error"]["details"]["field"],
        "title"
    );

    let response = fx
        .request(
            Method::POST,
            "/api/v1/message-snippets",
            Some("lim-body"),
            Some(json!({"scope":"global","title":"Пустая","body":""})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 422);
    assert_eq!(
        response.json::<Value>()["error"]["details"]["field"],
        "body"
    );

    let response = fx
        .request(
            Method::POST,
            "/api/v1/message-snippets",
            Some("lim-scope"),
            Some(json!({"scope":"project","project_key":"","title":"Без проекта","body":"текст"})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 422);
    assert_eq!(
        response.json::<Value>()["error"]["details"]["field"],
        "scope"
    );

    fx.create(
        "lim-first",
        json!({"scope":"global","title":"Заготовка","body":"текст"}),
    )
    .await;
    let response = fx
        .request(
            Method::POST,
            "/api/v1/message-snippets",
            Some("lim-dup"),
            Some(json!({"scope":"global","title":"заготовка","body":"другой"})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&response), "message_snippet_title_taken");
}
