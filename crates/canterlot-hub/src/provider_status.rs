//! Публичный статус провайдеров: хаб сам читает страницы statuspage (Claude, OpenAI)
//! и сводит нужные компоненты к одному уровню для полосы лимитов и маршрута.
//! Статус только советует: ничего не запрещает и в запрос не кладёт данных владельца.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use tokio::sync::watch;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    config::{ProviderStatusConfig, StatusPage},
    events::{self, NewEvent, Scope},
};

pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Данные старше стольких периодов опроса больше не говорят ничего: уровень `unknown`.
const STALE_INTERVALS: u32 = 3;
const MAX_INCIDENTS: usize = 3;
const MAX_TEXT_CHARS: usize = 200;
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Уровень, важный для маршрута; порядок — от лучшего к худшему.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatusLevel {
    Ok,
    Degraded,
    Maintenance,
    PartialOutage,
    MajorOutage,
    Unknown,
}

impl ProviderStatusLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Maintenance => "maintenance",
            Self::PartialOutage => "partial_outage",
            Self::MajorOutage => "major_outage",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "ok" => Self::Ok,
            "degraded" => Self::Degraded,
            "maintenance" => Self::Maintenance,
            "partial_outage" => Self::PartialOutage,
            "major_outage" => Self::MajorOutage,
            _ => Self::Unknown,
        }
    }

    fn from_component(status: &str) -> Option<Self> {
        Some(match status {
            "operational" => Self::Ok,
            "degraded_performance" => Self::Degraded,
            "under_maintenance" => Self::Maintenance,
            "partial_outage" => Self::PartialOutage,
            "major_outage" => Self::MajorOutage,
            _ => return None,
        })
    }

    fn from_indicator(indicator: &str) -> Option<Self> {
        Some(match indicator {
            "none" => Self::Ok,
            "minor" => Self::Degraded,
            "major" => Self::PartialOutage,
            "critical" => Self::MajorOutage,
            "maintenance" => Self::Maintenance,
            _ => return None,
        })
    }

    /// Сбой, из-за которого маршрут предпочитает другого кандидата.
    pub fn is_outage(self) -> bool {
        matches!(self, Self::PartialOutage | Self::MajorOutage)
    }

    /// Уровень, о котором стоит сказать: не `ok` и не `unknown`.
    pub fn is_notable(self) -> bool {
        !matches!(self, Self::Ok | Self::Unknown)
    }

    pub fn words(self) -> &'static str {
        match self {
            Self::Ok => "operational",
            Self::Degraded => "degraded performance",
            Self::Maintenance => "maintenance",
            Self::PartialOutage => "partial outage",
            Self::MajorOutage => "major outage",
            Self::Unknown => "unknown",
        }
    }
}

/// Незакрытый инцидент страницы статуса.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProviderIncident {
    pub name: String,
    /// Короткая ссылка на инцидент, если страница её дала.
    pub url: Option<String>,
}

/// Разобранная страница статуса.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub level: ProviderStatusLevel,
    pub description: String,
    /// Перечисленные в конфиге компоненты не в рабочем состоянии.
    pub components: Vec<String>,
    pub incidents: Vec<ProviderIncident>,
    /// Адрес страницы для человека.
    pub url: String,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    page: Option<PageInfo>,
    status: PageStatus,
    #[serde(default)]
    components: Vec<Component>,
    #[serde(default)]
    incidents: Vec<Incident>,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct PageStatus {
    indicator: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct Component {
    name: String,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct Incident {
    name: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    shortlink: Option<String>,
}

fn clip(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

fn http_url(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let parsed = reqwest::Url::parse(value).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| value.to_owned())
}

/// Корень сайта страницы статуса, если страница не назвала свой адрес.
fn site_of(api_url: &str) -> String {
    reqwest::Url::parse(api_url)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(match url.port() {
                Some(port) => format!("{}://{host}:{port}", url.scheme()),
                None => format!("{}://{host}", url.scheme()),
            })
        })
        .unwrap_or_else(|| api_url.to_owned())
}

/// Сводит `summary.json` к уровню по перечисленным компонентам. Компонент, которого
/// нет на странице, не учитывается; если нет ни одного — берётся общий индикатор.
pub fn read_page(body: &[u8], page: &StatusPage) -> Result<Reading, &'static str> {
    let summary: Summary =
        serde_json::from_slice(body).map_err(|_| "provider_status_unreadable")?;
    let mut level: Option<ProviderStatusLevel> = None;
    let mut affected: Vec<String> = Vec::new();
    for component in &summary.components {
        if !page.components.iter().any(|name| name == &component.name) {
            continue;
        }
        let Some(found) = component
            .status
            .as_deref()
            .and_then(ProviderStatusLevel::from_component)
        else {
            continue;
        };
        level = Some(level.map_or(found, |known| known.max(found)));
        if found != ProviderStatusLevel::Ok && !affected.contains(&component.name) {
            affected.push(component.name.clone());
        }
    }
    let level = match level {
        Some(level) => level,
        None => ProviderStatusLevel::from_indicator(&summary.status.indicator)
            .ok_or("provider_status_unreadable")?,
    };
    let incidents = summary
        .incidents
        .iter()
        .filter(|incident| {
            !matches!(
                incident.status.as_deref(),
                Some("resolved" | "postmortem" | "completed")
            )
        })
        .take(MAX_INCIDENTS)
        .map(|incident| ProviderIncident {
            name: clip(&incident.name),
            url: http_url(incident.shortlink.as_deref()),
        })
        .collect();
    Ok(Reading {
        level,
        description: clip(summary.status.description.as_deref().unwrap_or_default()),
        components: affected,
        incidents,
        url: http_url(summary.page.as_ref().and_then(|page| page.url.as_deref()))
            .unwrap_or_else(|| site_of(&page.url)),
    })
}

/// Статус провайдера в хабе на момент чтения.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub provider: String,
    /// Действующий уровень: `unknown`, если данных нет или они устарели.
    pub level: ProviderStatusLevel,
    pub description: String,
    pub components: Vec<String>,
    pub incidents: Vec<ProviderIncident>,
    pub url: String,
    pub fetched_at: Option<DateTime<Utc>>,
    pub changed_at: DateTime<Utc>,
    /// Показаны последние известные данные, но они устарели.
    pub stale: bool,
}

impl Status {
    /// Строка для модели: `partial outage (Claude Code) — <инцидент>, since 16:02 UTC`.
    pub fn summary(&self) -> String {
        let mut text = self.level.words().to_owned();
        if !self.components.is_empty() {
            text.push_str(&format!(" ({})", self.components.join(", ")));
        }
        if let Some(incident) = self.incidents.first() {
            text.push_str(&format!(" — {}", incident.name));
        }
        let since = if self.changed_at.date_naive() == Utc::now().date_naive() {
            self.changed_at.format("%H:%M UTC")
        } else {
            self.changed_at.format("%b %-d %H:%M UTC")
        };
        text.push_str(&format!(", since {since}"));
        text
    }
}

/// Статус провайдера для клиентов.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ProviderStatusView {
    /// Уровень по компонентам, важным для харнесса; `unknown` — данных нет или они устарели.
    pub level: ProviderStatusLevel,
    /// Общая строка страницы, например «Partial System Outage».
    pub description: String,
    /// Важные для харнесса компоненты не в рабочем состоянии.
    pub components: Vec<String>,
    /// До трёх незакрытых инцидентов.
    pub incidents: Vec<ProviderIncident>,
    /// Страница статуса для человека.
    pub url: String,
    /// Последнее удачное чтение страницы.
    pub fetched_at: Option<String>,
    /// Когда уровень сменился в последний раз.
    pub changed_at: String,
    /// Последние известные данные старше трёх периодов опроса.
    pub stale: bool,
}

fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl From<&Status> for ProviderStatusView {
    fn from(status: &Status) -> Self {
        Self {
            level: status.level,
            description: status.description.clone(),
            components: status.components.clone(),
            incidents: status.incidents.clone(),
            url: status.url.clone(),
            fetched_at: status.fetched_at.map(wire),
            changed_at: wire(status.changed_at),
            stale: status.stale,
        }
    }
}

fn status_of(row: &sqlx::postgres::PgRow, now: DateTime<Utc>) -> Result<Status, sqlx::Error> {
    let level = ProviderStatusLevel::parse(row.try_get::<&str, _>("level")?);
    let fetched_at: Option<DateTime<Utc>> = row.try_get("fetched_at")?;
    let expires_at: Option<DateTime<Utc>> = row.try_get("expires_at")?;
    let fresh = fetched_at.is_some() && expires_at.is_some_and(|at| at > now);
    let list = |name: &str| -> Result<Value, sqlx::Error> { row.try_get(name) };
    Ok(Status {
        provider: row.try_get("provider")?,
        level: if fresh {
            level
        } else {
            ProviderStatusLevel::Unknown
        },
        description: row.try_get("description")?,
        components: serde_json::from_value(list("components")?).unwrap_or_default(),
        incidents: serde_json::from_value(list("incidents")?).unwrap_or_default(),
        url: row.try_get("url")?,
        fetched_at,
        changed_at: row.try_get("changed_at")?,
        stale: fetched_at.is_some() && !fresh,
    })
}

/// Статус всех провайдеров, которые хаб когда-либо опрашивал.
pub async fn load_all(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
) -> Result<HashMap<String, Status>, sqlx::Error> {
    let rows = sqlx::query(
        "select provider,level,description,components,incidents,url,fetched_at,expires_at,
                changed_at
           from provider_status order by provider",
    )
    .fetch_all(conn)
    .await?;
    rows.iter()
        .map(|row| status_of(row, now).map(|status| (status.provider.clone(), status)))
        .collect()
}

pub async fn load(
    conn: &mut PgConnection,
    provider: &str,
    now: DateTime<Utc>,
) -> Result<Option<Status>, sqlx::Error> {
    let row = sqlx::query(
        "select provider,level,description,components,incidents,url,fetched_at,expires_at,
                changed_at
           from provider_status where provider=$1",
    )
    .bind(provider)
    .fetch_optional(conn)
    .await?;
    row.map(|row| status_of(&row, now)).transpose()
}

/// Записывает итог опроса страницы. Неудача хранит прежние данные: уровень станет
/// `unknown`, когда они устареют. Смена действующего уровня — событие владельцу.
pub async fn record(
    pool: &PgPool,
    page: &StatusPage,
    reading: Result<Reading, &'static str>,
    interval: Duration,
    now: DateTime<Utc>,
) -> Result<ProviderStatusLevel, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query(
        "insert into provider_status(provider,level,url,checked_at,failed,changed_at)
         values($1,'unknown',$2,$3,true,$3) on conflict do nothing",
    )
    .bind(page.provider)
    .bind(site_of(&page.url))
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let announced: String = sqlx::query_scalar(
        "select announced_level from provider_status where provider=$1 for update",
    )
    .bind(page.provider)
    .fetch_one(&mut *tx)
    .await?;
    match reading {
        Ok(reading) => {
            let lifetime = chrono::TimeDelta::from_std(interval * STALE_INTERVALS)
                .unwrap_or(chrono::TimeDelta::minutes(9));
            sqlx::query(
                "update provider_status set level=$2,description=$3,components=$4,incidents=$5,
                   url=$6,fetched_at=$7,expires_at=$8,checked_at=$7,failed=false
                 where provider=$1",
            )
            .bind(page.provider)
            .bind(reading.level.as_str())
            .bind(&reading.description)
            .bind(json!(reading.components))
            .bind(json!(reading.incidents))
            .bind(&reading.url)
            .bind(now)
            .bind(now + lifetime)
            .execute(&mut *tx)
            .await?;
        }
        Err(_) => {
            sqlx::query("update provider_status set checked_at=$2,failed=true where provider=$1")
                .bind(page.provider)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
    }
    let mut status = load(&mut tx, page.provider, now)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    if status.level.as_str() != announced {
        sqlx::query(
            "update provider_status set announced_level=$2,changed_at=$3 where provider=$1",
        )
        .bind(page.provider)
        .bind(status.level.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        status.changed_at = now;
        emit_changed(&mut tx, &status).await?;
    }
    tx.commit().await?;
    Ok(status.level)
}

async fn emit_changed(conn: &mut PgConnection, status: &Status) -> Result<(), sqlx::Error> {
    let owners: Vec<Uuid> = sqlx::query_scalar("select id from owners order by id")
        .fetch_all(&mut *conn)
        .await?;
    let view = serde_json::to_value(ProviderStatusView::from(status))
        .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    for owner_id in owners {
        events::append(
            &mut *conn,
            NewEvent {
                scope: Scope::Conversations { owner_id },
                kind: "provider_status_changed",
                conversation_id: None,
                execution_id: None,
                payload: json!({"provider": status.provider, "status": view}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await?;
    }
    Ok(())
}

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .user_agent("canterlot-hub")
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
}

async fn fetch(client: &reqwest::Client, page: &StatusPage) -> Result<Reading, &'static str> {
    let mut response = client
        .get(&page.url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "provider_status_unreachable")?;
    if !response.status().is_success() {
        return Err("provider_status_http_error");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "provider_status_unreachable")?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err("provider_status_too_large");
        }
        body.extend_from_slice(&chunk);
    }
    read_page(&body, page)
}

/// Один проход по всем страницам.
pub async fn poll_once(
    pool: &PgPool,
    client: &reqwest::Client,
    pages: &[StatusPage],
    interval: Duration,
) -> Result<(), sqlx::Error> {
    for page in pages {
        let reading = fetch(client, page).await;
        if let Err(reason) = reading {
            // Сбой чужой страницы — не ошибка хаба: INFO не тревожит проверку выкатки.
            tracing::info!(
                kind = "provider_status_fetch_failed",
                provider = page.provider,
                reason,
                "provider status page unavailable"
            );
        }
        record(pool, page, reading, interval, Utc::now()).await?;
    }
    Ok(())
}

/// Фоновый опрос под advisory lock: второй хаб не опрашивает.
pub async fn run(pool: PgPool, config: ProviderStatusConfig, shutdown: watch::Receiver<bool>) {
    let client = match client() {
        Ok(client) => client,
        Err(_) => {
            tracing::warn!(
                kind = "provider_status_client_error",
                "provider status client unavailable"
            );
            return;
        }
    };
    let pages = config.pages();
    let interval = config.interval.max(ProviderStatusConfig::MIN_INTERVAL);
    let supervisor = pool.clone();
    crate::singleton::supervise(&supervisor, "provider_status", shutdown, move || {
        let pool = pool.clone();
        let client = client.clone();
        let pages = pages.clone();
        async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = poll_once(&pool, &client, &pages, interval).await {
                    tracing::warn!(
                        kind = "provider_status_database",
                        error = %crate::dberr::describe(&error),
                        "provider status not stored"
                    );
                }
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = include_str!("../tests/fixtures/provider_status/claude_summary.json");
    const OPENAI: &str = include_str!("../tests/fixtures/provider_status/openai_summary.json");

    fn page(provider: &'static str) -> StatusPage {
        ProviderStatusConfig::default()
            .pages()
            .into_iter()
            .find(|page| page.provider == provider)
            .unwrap()
    }

    fn with_components(provider: &'static str, components: &[&str]) -> StatusPage {
        StatusPage {
            components: components.iter().map(|name| (*name).to_owned()).collect(),
            ..page(provider)
        }
    }

    #[test]
    fn claude_takes_the_worst_listed_component() {
        let reading = read_page(CLAUDE.as_bytes(), &page("claude")).unwrap();
        assert_eq!(reading.level, ProviderStatusLevel::PartialOutage);
        assert_eq!(
            reading.components,
            ["Claude API (api.anthropic.com)", "Claude Code"],
            "only listed components, in page order"
        );
        assert_eq!(reading.description, "Partial System Outage");
        assert_eq!(reading.url, "https://status.claude.com");
        assert_eq!(
            reading.incidents,
            [ProviderIncident {
                name: "Elevated errors on Claude Code".into(),
                url: Some("https://stspg.io/abc123".into()),
            }],
            "resolved incidents are dropped"
        );
    }

    #[test]
    fn codex_ignores_the_overall_indicator_and_unlisted_components() {
        let reading = read_page(OPENAI.as_bytes(), &page("codex")).unwrap();
        assert_eq!(
            reading.level,
            ProviderStatusLevel::Degraded,
            "image generation and the desktop app do not count"
        );
        assert_eq!(
            reading.components,
            ["Login"],
            "a repeated name is listed once"
        );
        assert_eq!(reading.incidents.len(), 2);
        assert_eq!(reading.incidents[1].url, None);
    }

    #[test]
    fn missing_components_fall_back_to_the_overall_indicator() {
        let reading = read_page(OPENAI.as_bytes(), &with_components("codex", &["Gone"])).unwrap();
        assert_eq!(reading.level, ProviderStatusLevel::Degraded, "minor");
        assert!(reading.components.is_empty());
        for (indicator, level) in [
            ("none", ProviderStatusLevel::Ok),
            ("minor", ProviderStatusLevel::Degraded),
            ("major", ProviderStatusLevel::PartialOutage),
            ("critical", ProviderStatusLevel::MajorOutage),
            ("maintenance", ProviderStatusLevel::Maintenance),
        ] {
            let body = json!({"status": {"indicator": indicator, "description": "x"}});
            let reading = read_page(body.to_string().as_bytes(), &page("claude")).unwrap();
            assert_eq!(reading.level, level, "{indicator}");
            assert_eq!(
                reading.url, "https://status.claude.com",
                "derived from the api url"
            );
        }
    }

    #[test]
    fn one_present_component_is_enough_and_unknown_statuses_are_ignored() {
        let body = json!({
            "status": {"indicator": "critical", "description": "Major"},
            "components": [
                {"name": "Claude Code", "status": "operational"},
                {"name": "Claude API (api.anthropic.com)", "status": "something_new"}
            ]
        });
        let reading = read_page(body.to_string().as_bytes(), &page("claude")).unwrap();
        assert_eq!(reading.level, ProviderStatusLevel::Ok);
    }

    #[test]
    fn unreadable_pages_are_errors() {
        assert!(read_page(b"<html>", &page("claude")).is_err());
        let body = json!({"status": {"indicator": "purple"}});
        assert!(read_page(body.to_string().as_bytes(), &page("claude")).is_err());
    }

    #[test]
    fn incidents_are_capped_and_clipped() {
        let long = "x".repeat(500);
        let incidents: Vec<Value> = (0..5)
            .map(|index| {
                json!({"name": format!("{index}{long}"), "status": "investigating",
                       "shortlink": "javascript:alert(1)"})
            })
            .collect();
        let body = json!({"status": {"indicator": "major"}, "incidents": incidents});
        let reading = read_page(body.to_string().as_bytes(), &page("claude")).unwrap();
        assert_eq!(reading.incidents.len(), 3);
        assert_eq!(
            reading.incidents[0].name.chars().count(),
            201,
            "200 and an ellipsis"
        );
        assert_eq!(reading.incidents[0].url, None, "only http links");
    }

    #[test]
    fn levels_order_and_classify() {
        use ProviderStatusLevel::*;
        assert!(Ok < Degraded && Degraded < Maintenance && Maintenance < PartialOutage);
        assert!(PartialOutage < MajorOutage);
        assert!(PartialOutage.is_outage() && MajorOutage.is_outage());
        assert!(!Degraded.is_outage() && !Maintenance.is_outage());
        assert!(!Ok.is_notable() && !Unknown.is_notable() && Degraded.is_notable());
        for level in [
            Ok,
            Degraded,
            Maintenance,
            PartialOutage,
            MajorOutage,
            Unknown,
        ] {
            assert_eq!(ProviderStatusLevel::parse(level.as_str()), level);
        }
    }

    #[test]
    fn summary_names_components_incident_and_time() {
        let changed_at = Utc::now()
            .date_naive()
            .and_hms_opt(16, 2, 0)
            .unwrap()
            .and_utc();
        let status = Status {
            provider: "claude".into(),
            level: ProviderStatusLevel::PartialOutage,
            description: "Partial System Outage".into(),
            components: vec!["Claude Code".into()],
            incidents: vec![ProviderIncident {
                name: "Elevated errors".into(),
                url: None,
            }],
            url: "https://status.claude.com".into(),
            fetched_at: Some(changed_at),
            changed_at,
            stale: false,
        };
        assert_eq!(
            status.summary(),
            "partial outage (Claude Code) — Elevated errors, since 16:02 UTC"
        );
    }
}
