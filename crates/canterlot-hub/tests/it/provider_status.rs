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
    config::{ProviderStatusConfig, SessionToolsConfig, StatusPage},
    provider_status::{self, ProviderStatusLevel},
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit,
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::task7::Fixture;

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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn poller_stores_the_level_and_announces_only_changes(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_page_never_read_stays_unknown_without_an_event(pool: PgPool) {
    testkit::seed(&pool).await;
    let fake = Fake::default();
    let url = serve(fake.clone()).await;
    let client = provider_status::client().unwrap();
    provider_status::poll_once(&pool, &client, &[claude_page(&url)], INTERVAL)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let status = provider_status::load(&mut conn, "claude", Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.level, ProviderStatusLevel::Unknown);
    assert!(!status.stale, "nothing to be stale");
    assert_eq!(status.fetched_at, None);
    assert!(changes(&pool).await.is_empty());

    // Неразборчивое тело тоже считается неудачей опроса.
    fake.set(Some("<html>maintenance</html>"));
    provider_status::poll_once(&pool, &client, &[claude_page(&url)], INTERVAL)
        .await
        .unwrap();
    assert!(changes(&pool).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn status_survives_a_restart_and_reaches_the_limits_api(pool: PgPool) {
    let f = Fixture::new(pool).await;
    testkit::offer_provider(&f.pool, f.seed.machine_id, "codex").await;
    let now = Utc::now();
    for provider in ["claude", "codex"] {
        sqlx::query(
            "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,weekly_observed_at)
             values($1,100,$2,$3) on conflict(provider) do update set weekly_used_permille=100,
               weekly_resets_at=$2,weekly_observed_at=$3",
        )
        .bind(provider)
        .bind(now + TimeDelta::days(3))
        .bind(now)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let reading = provider_status::read_page(CLAUDE.as_bytes(), &claude_page("http://x/")).unwrap();
    provider_status::record(
        &f.pool,
        &claude_page("http://x/"),
        Ok(reading),
        INTERVAL,
        now,
    )
    .await
    .unwrap();

    let view = f.get("/api/v1/limits").await;
    let claude = view
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "claude")
        .unwrap();
    let status = &claude["status"];
    assert_eq!(status["level"], "partial_outage", "{view}");
    assert_eq!(status["description"], "Partial System Outage");
    assert_eq!(status["url"], "https://status.claude.com");
    assert_eq!(status["stale"], false);
    assert_eq!(status["incidents"][0]["url"], "https://stspg.io/abc123");
    assert!(status["fetched_at"].is_string());
    assert!(status["changed_at"].is_string());
    let codex = view
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "codex")
        .unwrap();
    assert!(codex["status"].is_null(), "never polled: {view}");

    let snapshot = f.get("/api/v1/snapshot/conversations").await;
    let listed = snapshot["data"]["limits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "claude")
        .unwrap();
    assert_eq!(listed["status"]["level"], "partial_outage");
}

fn context(pool: &PgPool) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: std::time::Duration::from_secs(1800),
        routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn canterlot_limits_names_an_outage_and_stays_quiet_when_ok(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    testkit::offer_provider(&pool, seed.machine_id, "claude").await;
    let now = Utc::now();
    sqlx::query(
        "insert into provider_limits(provider,weekly_used_permille,weekly_resets_at,weekly_observed_at)
         values('claude',100,$1,$2)",
    )
    .bind(now + TimeDelta::days(3))
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let page = claude_page("http://x/");
    let report = |pool: PgPool| async move {
        session_tools::call(&context(&pool), &caller, "limits.report", json!({}))
            .await
            .unwrap()
    };

    let reading = provider_status::read_page(CLAUDE.as_bytes(), &page).unwrap();
    provider_status::record(&pool, &page, Ok(reading), INTERVAL, now)
        .await
        .unwrap();
    let answer = report(pool.clone()).await;
    let text = answer["text"].as_str().unwrap();
    let since = now.format("%H:%M UTC").to_string();
    let line = format!(
        "Claude status: partial outage (Claude API (api.anthropic.com), Claude Code) — Elevated errors on Claude Code, since {since}; https://status.claude.com"
    );
    assert!(text.contains(&line), "{text}");
    assert_eq!(answer["providers"][0]["status"]["level"], "partial_outage");

    let reading = provider_status::read_page(operational().as_bytes(), &page).unwrap();
    provider_status::record(&pool, &page, Ok(reading), INTERVAL, now)
        .await
        .unwrap();
    let answer = report(pool.clone()).await;
    let text = answer["text"].as_str().unwrap();
    assert!(!text.contains("status:"), "{text}");
    assert_eq!(answer["providers"][0]["status"]["level"], "ok");
}
