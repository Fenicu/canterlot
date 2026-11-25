//! Статус провайдеров: опрос страницы statuspage на локальном сервере, хранение,
//! событие только при смене уровня, устаревание, поле `status` в API и строка
//! `canterlot_limits`.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use canterlot_hub::{
    config::{ProviderStatusConfig, StatusPage},
    provider_status::{self, ProviderStatusLevel},
    testkit,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const CLAUDE: &str = include_str!("../fixtures/provider_status/claude_summary.json");
const INTERVAL: std::time::Duration = std::time::Duration::from_secs(180);

#[derive(Clone, Default)]
struct Fake {
    body: Arc<Mutex<Option<String>>>,
    headers: Arc<Mutex<Vec<HeaderMap>>>,
}

impl Fake {
    fn set(&self, body: Option<&str>) {
        *self.body.lock().unwrap() = body.map(str::to_owned);
    }
}

/// Локальная страница статуса: тело задаёт тест, `None` — ответ 503.
async fn serve(fake: Fake) -> String {
    let app = Router::new().route(
        "/api/v2/summary.json",
        get(move |headers: HeaderMap| {
            let fake = fake.clone();
            async move {
                fake.headers.lock().unwrap().push(headers);
                match fake.body.lock().unwrap().clone() {
                    Some(body) => (StatusCode::OK, body),
                    None => (StatusCode::SERVICE_UNAVAILABLE, String::new()),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/api/v2/summary.json")
}

fn claude_page(url: &str) -> StatusPage {
    let mut page = ProviderStatusConfig::default()
        .pages()
        .into_iter()
        .find(|page| page.provider == "claude")
        .unwrap();
    page.url = url.to_owned();
    page
}

fn operational() -> String {
    json!({
        "page": {"url": "https://status.claude.com"},
        "status": {"indicator": "none", "description": "All Systems Operational"},
        "components": [
            {"name": "Claude Code", "status": "operational"},
            {"name": "Claude API (api.anthropic.com)", "status": "operational"}
        ],
        "incidents": []
    })
    .to_string()
}

async fn changes(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from events where kind='provider_status_changed' order by committed_at,id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

type EventRow = (
    Uuid,
    String,
    i64,
    String,
    Value,
    Uuid,
    chrono::DateTime<Utc>,
);

async fn event_rows(pool: &PgPool) -> Vec<Value> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "select id,scope,scope_seq,kind,payload,correlation_id,committed_at from events
              where kind='provider_status_changed' order by committed_at,id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(id, scope, seq, kind, payload, correlation, at)| {
            let at = at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            json!({
                "v": 1, "id": id, "scope": scope.rsplit_once(':').map_or(scope.clone(), |(_, s)| s.to_owned()),
                "scope_seq": seq.to_string(), "scope_seq_from": seq.to_string(),
                "committed_at": at, "committed_at_from": at,
                "conversation_id": null, "execution_id": null,
                "correlation_id": correlation, "kind": kind, "payload": payload,
            })
        })
        .collect()
}

fn validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/provider_status_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

#[tokio::test]
async fn poller_stores_the_level_and_announces_only_changes() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let fake = Fake::default();
    fake.set(Some(CLAUDE));
    let url = serve(fake.clone()).await;
    let page = claude_page(&url);
    let client = provider_status::client().unwrap();
    let mut conn = pool.acquire().await.unwrap();

    provider_status::poll_once(&pool, &client, std::slice::from_ref(&page), INTERVAL)
        .await
        .unwrap();
    let status = provider_status::load(&mut conn, "claude", Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.level, ProviderStatusLevel::PartialOutage);
    assert_eq!(status.url, "https://status.claude.com");
    assert_eq!(status.incidents[0].name, "Elevated errors on Claude Code");
    assert!(!status.stale);
    let seen = changes(&pool).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["provider"], "claude");
    assert_eq!(seen[0]["status"]["level"], "partial_outage");
    assert_eq!(
        seen[0]["status"]["components"],
        json!(["Claude API (api.anthropic.com)", "Claude Code"])
    );
    let owner_scope: String =
        sqlx::query_scalar("select scope from events where kind='provider_status_changed' limit 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        owner_scope,
        format!("owner:{}:conversations", seed.owner_id)
    );

    // Тот же уровень — без события.
    provider_status::poll_once(&pool, &client, std::slice::from_ref(&page), INTERVAL)
        .await
        .unwrap();
    assert_eq!(changes(&pool).await.len(), 1);

    fake.set(Some(&operational()));
    provider_status::poll_once(&pool, &client, std::slice::from_ref(&page), INTERVAL)
        .await
        .unwrap();
    let seen = changes(&pool).await;
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1]["status"]["level"], "ok");
    assert_eq!(seen[1]["status"]["incidents"], json!([]));

    // Один сбой опроса не меняет уровень: данные ещё свежие.
    fake.set(None);
    provider_status::poll_once(&pool, &client, std::slice::from_ref(&page), INTERVAL)
        .await
        .unwrap();
    assert_eq!(changes(&pool).await.len(), 2);
    let failed: bool =
        sqlx::query_scalar("select failed from provider_status where provider='claude'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(failed);

    // Данные старше трёх периодов: `unknown`, подробности остаются и помечены устаревшими.
    sqlx::query("update provider_status set expires_at=$1 where provider='claude'")
        .bind(Utc::now() - TimeDelta::seconds(1))
        .execute(&pool)
        .await
        .unwrap();
    provider_status::poll_once(&pool, &client, std::slice::from_ref(&page), INTERVAL)
        .await
        .unwrap();
    let seen = changes(&pool).await;
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2]["status"]["level"], "unknown");
    assert_eq!(seen[2]["status"]["stale"], true);
    assert_eq!(seen[2]["status"]["description"], "All Systems Operational");
    assert!(seen[2]["status"]["fetched_at"].is_string());

    let validator = validator();
    for event in event_rows(&pool).await {
        assert!(
            validator.is_valid(&event),
            "{event}: {:?}",
            validator
                .iter_errors(&event)
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        );
    }

    let headers = fake.headers.lock().unwrap();
    assert_eq!(headers.len(), 5);
    for request in headers.iter() {
        assert_eq!(request["user-agent"], "canterlot-hub");
        assert!(request.get("cookie").is_none());
        assert!(request.get("authorization").is_none());
    }
}
