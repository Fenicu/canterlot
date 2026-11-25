//! Тесты задачи 2 плана 2c: пресеты инструкций в хабе.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    presets::{self, Actor, NewPreset, PresetError, PresetPatch},
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
    machine: Uuid,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            owner,
            machine,
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

    async fn create(&self, key: &str, title: &str, text: &str) -> Value {
        let response = self
            .request(
                Method::POST,
                "/api/v1/presets",
                Some(key),
                Some(json!({"title": title, "text": text})),
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

#[tokio::test]
async fn list_starts_with_builtins_then_custom_by_title() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let first: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    // Посев init уже создал «Исполнитель (английский)».
    assert_eq!(first[0]["id"], "clean");
    assert_eq!(first[0]["kind"], "clean");
    assert_eq!(first[0]["title"], "Без инструкций");
    assert!(first[0].get("text").is_none());
    assert!(first[0]["revision"].is_null());
    assert_eq!(first[1]["id"], "owner");
    assert_eq!(first[1]["kind"], "owner");
    assert_eq!(first[1]["title"], "Как у владельца");
    f.create("list_beta", "Бета", "текст беты").await;
    f.create("list_alpha", "альфа", "текст альфы").await;
    let list: Value = f
        .request(Method::GET, "/api/v1/presets", None, None)
        .await
        .json();
    let titles: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Без инструкций",
            "Как у владельца",
            "альфа",
            "Бета",
            "Исполнитель (английский)"
        ]
    );
    for preset in list.as_array().unwrap() {
        assert!(preset.get("text").is_none(), "в списке нет текста");
    }
}

#[tokio::test]
async fn update_requires_current_base_revision_and_bumps_revision() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let created = f.create("bump", "Версии", "первый").await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["revision"], 1);
    let stale = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_stale"),
            Some(json!({"title": "Другой", "base_revision": 999})),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(Fx::error_code(&stale), "preset_revision_conflict");
    assert_eq!(
        stale.json::<Value>()["error"]["details"]["current_revision"],
        1
    );
    let updated: Value = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_ok"),
            Some(json!({"title": "Версии 2", "text": "второй", "base_revision": 1})),
        )
        .await
        .json();
    assert_eq!(updated["revision"], 2);
    assert_eq!(updated["text"], "второй");
    let again = f
        .request(
            Method::PATCH,
            &format!("/api/v1/presets/{id}"),
            Some("bump_again"),
            Some(json!({"text": "третий", "base_revision": 1})),
        )
        .await;
    assert_eq!(again.status.as_u16(), 409);
    assert_eq!(
        again.json::<Value>()["error"]["details"]["current_revision"],
        2
    );
}

/// Пресет читают сессии обоих доменов, поэтому текст от агента проходит тот же детектор
/// секретов, что и запись памяти. Владелец пишет свой текст без проверки.
#[tokio::test]
async fn agent_cannot_put_a_secret_into_a_preset() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fx::new(pool).await;
    let conversation = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let agent = Actor::Agent {
        conversation_id: conversation,
    };
    let new = |title: &str, description: Option<&str>, text: &str| NewPreset {
        title: title.into(),
        description: description.map(str::to_owned),
        text: text.into(),
        purpose: canterlot_hub::presets::PresetPurpose::Instructions,
    };
    let secret = "Staging DB: postgres://app:Tr0ub4dor&3xyz@db.internal:5432/app";
    for (n, body) in [
        new("Стенд", None, secret),
        new("Стенд", Some("password: Hunter2Hunter2"), "текст"),
        new("token: abcd1234efgh", None, "текст"),
    ]
    .into_iter()
    .enumerate()
    {
        let error = presets::create(
            &f.pool,
            f.owner,
            &format!("agent-secret-{n}"),
            body,
            agent,
            Uuid::now_v7(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                PresetError::Invalid {
                    field: "looks_like_secret"
                }
            ),
            "{n}: {error:?}"
        );
    }
    let created = presets::create(
        &f.pool,
        f.owner,
        "agent-clean",
        new("Стенд", None, "секреты просим через secret_request"),
        agent,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let id: Uuid = created.summary.id.parse().unwrap();
    let patch = || PresetPatch {
        title: None,
        description: None,
        text: Some(secret.into()),
        base_revision: 1,
    };
    let error = presets::update(
        &f.pool,
        f.owner,
        id,
        "agent-secret-update",
        patch(),
        agent,
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            PresetError::Invalid {
                field: "looks_like_secret"
            }
        ),
        "{error:?}"
    );
    // Владелец отвечает за свой текст сам.
    presets::update(
        &f.pool,
        f.owner,
        id,
        "owner-update",
        patch(),
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}
