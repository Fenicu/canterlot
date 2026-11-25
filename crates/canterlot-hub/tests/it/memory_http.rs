//! Общая память: API владельца `/api/v1/memory` и поля памяти в карточке сессии.

use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    testkit::{self, TestClient, TestResponse},
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

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.client.send(method, path, None, body).await
    }

    async fn create(&self, domain: &str, subject: &str, body: &str) -> Value {
        let response = self
            .call(
                Method::POST,
                "/api/v1/memory",
                Some(json!({
                    "domain": domain,
                    "level": "domain",
                    "kind": "gotcha",
                    "subject": subject,
                    "body": body,
                })),
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
}

fn code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn subjects(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["subject"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn owner_list_requires_domain_and_never_mixes() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let work = fx
        .create("work", "Сборка воркера", "cargo build -p worker")
        .await;
    fx.create("personal", "Рецепт борща", "свёкла, капуста")
        .await;

    for path in [
        "/api/v1/memory",
        "/api/v1/memory?domain=",
        "/api/v1/memory?domain=other",
    ] {
        let response = fx.call(Method::GET, path, None).await;
        assert_eq!(response.status.as_u16(), 400, "{path}");
        assert_eq!(code(&response), "invalid_request", "{path}");
        assert_eq!(
            response.json::<Value>()["error"]["details"]["field"],
            "domain",
            "{path}"
        );
    }

    let work_page = fx
        .call(Method::GET, "/api/v1/memory?domain=work", None)
        .await
        .json::<Value>();
    assert_eq!(subjects(&work_page), ["Сборка воркера"]);
    let personal_page = fx
        .call(Method::GET, "/api/v1/memory?domain=personal", None)
        .await
        .json::<Value>();
    assert_eq!(subjects(&personal_page), ["Рецепт борща"]);

    let id = work["id"].as_str().unwrap();
    let own = fx
        .call(
            Method::GET,
            &format!("/api/v1/memory/{id}?domain=work"),
            None,
        )
        .await;
    assert_eq!(own.status.as_u16(), 200);
    assert_eq!(own.json::<Value>()["body"], "cargo build -p worker");
    let foreign = fx
        .call(
            Method::GET,
            &format!("/api/v1/memory/{id}?domain=personal"),
            None,
        )
        .await;
    assert_eq!(
        foreign.status.as_u16(),
        404,
        "чужой домен неотличим от пропавшей записи"
    );
    assert_eq!(code(&foreign), "not_found");
    let no_domain = fx
        .call(Method::GET, &format!("/api/v1/memory/{id}"), None)
        .await;
    assert_eq!(no_domain.status.as_u16(), 400);

    // Архив живёт за фильтром и тоже не смешивает домены.
    let archived = fx
        .call(
            Method::POST,
            &format!("/api/v1/memory/{id}/archive"),
            Some(json!({"domain": "work", "expected_version": 1})),
        )
        .await;
    assert_eq!(archived.status.as_u16(), 200);
    let active = fx
        .call(Method::GET, "/api/v1/memory?domain=work", None)
        .await
        .json::<Value>();
    assert!(subjects(&active).is_empty());
    let in_archive = fx
        .call(
            Method::GET,
            "/api/v1/memory?domain=work&status=archived",
            None,
        )
        .await
        .json::<Value>();
    assert_eq!(subjects(&in_archive), ["Сборка воркера"]);
    let other_archive = fx
        .call(
            Method::GET,
            "/api/v1/memory?domain=personal&status=archived",
            None,
        )
        .await
        .json::<Value>();
    assert!(subjects(&other_archive).is_empty());
    let restored = fx
        .call(
            Method::POST,
            &format!("/api/v1/memory/{id}/restore"),
            Some(json!({"domain": "work"})),
        )
        .await;
    assert_eq!(restored.status.as_u16(), 200);
    assert_eq!(restored.json::<Value>()["status"], "active");
    let revisions = fx
        .call(
            Method::GET,
            &format!("/api/v1/memory/{id}/revisions?domain=work"),
            None,
        )
        .await;
    assert_eq!(revisions.status.as_u16(), 200);
    assert_eq!(revisions.json::<Value>().as_array().unwrap().len(), 2);

    // Поиск по тексту и страницы по курсору.
    let found = fx
        .call(Method::GET, "/api/v1/memory?domain=work&q=cargo", None)
        .await
        .json::<Value>();
    assert_eq!(subjects(&found), ["Сборка воркера"]);
    for n in 0..3 {
        fx.create("work", &format!("Запись {n}"), "текст").await;
    }
    let first = fx
        .call(Method::GET, "/api/v1/memory?domain=work&limit=2", None)
        .await
        .json::<Value>();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = fx
        .call(
            Method::GET,
            &format!("/api/v1/memory?domain=work&limit=2&cursor={cursor}"),
            None,
        )
        .await
        .json::<Value>();
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert!(second["next_cursor"].is_null());
    let mut all = subjects(&first);
    all.extend(subjects(&second));
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "страницы не повторяют записи");
    let _ = fx.owner;
}

#[tokio::test]
async fn owner_put_wrong_version_409() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let created = fx.create("work", "Порт стенда", "5432").await;
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["version"], 1);

    let stale = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"domain": "work", "expected_version": 7, "body": "5433"})),
        )
        .await;
    assert_eq!(stale.status.as_u16(), 409);
    assert_eq!(code(&stale), "version_conflict");
    assert_eq!(
        stale.json::<Value>()["error"]["details"]["current_version"],
        1
    );

    let missing = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"domain": "work", "body": "5433"})),
        )
        .await;
    assert_eq!(missing.status.as_u16(), 400, "expected_version обязателен");

    let saved = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"domain": "work", "expected_version": 1, "body": "5433"})),
        )
        .await;
    assert_eq!(saved.status.as_u16(), 200);
    let saved = saved.json::<Value>();
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["body"], "5433");
    assert_eq!(saved["subject"], "Порт стенда");
}

#[tokio::test]
async fn owner_secret_warning_needs_force() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let secret = "ключ sk-abcdefghijklmnop1234 для вызова";
    let payload = |force: Option<bool>| {
        let mut body = json!({
            "domain": "work",
            "level": "domain",
            "kind": "reference",
            "subject": "Доступ к API",
            "body": secret,
        });
        if let Some(force) = force {
            body["force_secret_warning"] = json!(force);
        }
        body
    };
    for force in [None, Some(false)] {
        let refused = fx
            .call(Method::POST, "/api/v1/memory", Some(payload(force)))
            .await;
        assert_eq!(refused.status.as_u16(), 422);
        assert_eq!(code(&refused), "looks_like_secret");
        let text = String::from_utf8_lossy(&refused.body).into_owned();
        assert!(
            !text.contains("sk-abcdefghijklmnop1234"),
            "значение не эхом: {text}"
        );
        assert!(
            refused.json::<Value>()["error"]["details"]["class"].is_string(),
            "класс находки назван: {text}"
        );
    }
    let page = fx
        .call(Method::GET, "/api/v1/memory?domain=work", None)
        .await
        .json::<Value>();
    assert!(subjects(&page).is_empty(), "отказ ничего не сохранил");

    let forced = fx
        .call(Method::POST, "/api/v1/memory", Some(payload(Some(true))))
        .await;
    assert_eq!(forced.status.as_u16(), 201);

    // Правка проходит ту же проверку.
    let id = forced.json::<Value>()["id"].as_str().unwrap().to_owned();
    let edit = |force: bool| {
        json!({
            "domain": "work",
            "expected_version": 1,
            "body": "новый ghp_abcdefghijklmnopqrstuvwx",
            "force_secret_warning": force,
        })
    };
    let refused = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(edit(false)),
        )
        .await;
    assert_eq!(code(&refused), "looks_like_secret");
    let allowed = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(edit(true)),
        )
        .await;
    assert_eq!(allowed.status.as_u16(), 200);
}

#[tokio::test]
async fn hard_delete_404_after() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let created = fx.create("personal", "Старая заметка", "текст").await;
    let id = created["id"].as_str().unwrap();

    let wrong_domain = fx
        .call(
            Method::DELETE,
            &format!("/api/v1/memory/{id}?domain=work"),
            None,
        )
        .await;
    assert_eq!(wrong_domain.status.as_u16(), 404);
    let no_domain = fx
        .call(Method::DELETE, &format!("/api/v1/memory/{id}"), None)
        .await;
    assert_eq!(no_domain.status.as_u16(), 400);

    let deleted = fx
        .call(
            Method::DELETE,
            &format!("/api/v1/memory/{id}?domain=personal"),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    for (method, path) in [
        (Method::GET, format!("/api/v1/memory/{id}?domain=personal")),
        (
            Method::DELETE,
            format!("/api/v1/memory/{id}?domain=personal"),
        ),
        (
            Method::GET,
            format!("/api/v1/memory/{id}/revisions?domain=personal"),
        ),
    ] {
        let response = fx.call(method.clone(), &path, None).await;
        assert_eq!(response.status.as_u16(), 404, "{method} {path}");
        assert_eq!(code(&response), "not_found");
    }
    let archived = fx
        .call(
            Method::GET,
            "/api/v1/memory?domain=personal&status=archived",
            None,
        )
        .await
        .json::<Value>();
    assert!(subjects(&archived).is_empty());
}

#[tokio::test]
async fn conversation_info_carries_memory_domain_and_conflict() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool.clone()).await;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(fx.owner)
        .execute(&fx.pool)
        .await
        .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    let seed = testkit::Seed {
        owner_id: fx.owner,
        machine_id: machine,
        conversation_id: testkit::seed_conversation(&pool, fx.owner, machine).await,
    };
    let info = async |id: Uuid| {
        fx.call(Method::GET, &format!("/api/v1/conversations/{id}"), None)
            .await
            .json::<Value>()
    };

    // Посев даёт домен `work`; здесь нужен разговор без домена.
    sqlx::query("update conversations set memory_domain=null where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let bare = info(seed.conversation_id).await;
    assert!(bare["memory_domain"].is_null());
    assert!(bare["memory_project"].is_null());
    assert_eq!(bare["memory_conflict"], false);

    sqlx::query(
        "update conversations set memory_domain='work', memory_project='/h/work/alpha' where id=$1",
    )
    .bind(seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    let work = info(seed.conversation_id).await;
    assert_eq!(work["memory_domain"], "work");
    assert_eq!(work["memory_project"], "/h/work/alpha");
    assert_eq!(work["memory_conflict"], false);

    sqlx::query("update conversations set memory_domain=null, memory_project=null, memory_domain_conflict_at=now() where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let conflict = info(seed.conversation_id).await;
    assert!(conflict["memory_domain"].is_null());
    assert_eq!(conflict["memory_conflict"], true);
}

#[tokio::test]
async fn owner_put_can_move_an_entry_between_projects() {
    let pool = canterlot_hub::testkit::db().await;
    let fx = Fx::new(pool).await;
    let created = fx
        .call(
            Method::POST,
            "/api/v1/memory",
            Some(json!({
                "domain": "work", "level": "project", "project": "/h/work/alpha",
                "kind": "gotcha", "subject": "Правило", "body": "текст",
            })),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201);
    let id = created.json::<Value>()["id"].as_str().unwrap().to_owned();
    let moved = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({
                "domain": "work", "expected_version": 1,
                "level": "project", "project": "/h/work/beta",
            })),
        )
        .await;
    assert_eq!(moved.status.as_u16(), 200);
    let moved = moved.json::<Value>();
    assert_eq!(moved["project"], "/h/work/beta");
    assert_eq!(moved["project_label"], "beta");
    // Без явного проекта запись остаётся там, где была.
    let kept = fx
        .call(
            Method::PUT,
            &format!("/api/v1/memory/{id}"),
            Some(json!({"domain": "work", "expected_version": 2, "level": "project"})),
        )
        .await
        .json::<Value>();
    assert_eq!(kept["project"], "/h/work/beta");
}
